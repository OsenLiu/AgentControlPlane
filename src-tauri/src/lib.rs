use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Sender},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{Emitter, Manager, State};

struct Database(Arc<Mutex<Connection>>);
struct SessionProcesses(Arc<Mutex<HashMap<String, SessionControl>>>);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PermissionOption {
    option_id: String,
    name: String,
    kind: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingPermission {
    request_id: String,
    title: String,
    options: Vec<PermissionOption>,
}

#[derive(Clone)]
struct SessionControl {
    input: Sender<String>,
    pending: Arc<Mutex<HashMap<u64, Sender<Value>>>>,
    permission_requests: Arc<Mutex<HashMap<String, (Value, PendingPermission)>>>,
    next_request_id: Arc<AtomicU64>,
    stop_requested: Arc<std::sync::atomic::AtomicBool>,
    child: Arc<Mutex<Child>>,
    assistant_message_id: Arc<Mutex<Option<String>>>,
}

const GITHUB_CREDENTIAL_SERVICE: &str = "com.agentcontrolplane.desktop.github";
const GITHUB_CREDENTIAL_USER: &str = "github-token";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    id: String,
    name: String,
    path: String,
    branch: String,
    current_commit: String,
    git_state: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentSession {
    id: String,
    project: String,
    task: String,
    branch: String,
    provider: String,
    status: String,
    step: String,
    elapsed: String,
    tokens: String,
    permission_mode: String,
    managed: bool,
    last_event: String,
    archived: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptMessage {
    role: String,
    content: String,
    timestamp: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct CopilotObservation {
    status: Option<String>,
    step: Option<String>,
    last_event: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolSettingsInput {
    enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsInput {
    copilot: ToolSettingsInput,
    gemini: ToolSettingsInput,
    claude_code: ToolSettingsInput,
    github_username: String,
    github_token: String,
    default_provider: String,
    transcript_font_size: u8,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolSettings {
    enabled: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    copilot: ToolSettings,
    gemini: ToolSettings,
    claude_code: ToolSettings,
    github_username: String,
    github_token_configured: bool,
    default_provider: String,
    transcript_font_size: u8,
}

#[derive(Debug)]
struct GitSnapshot {
    root: PathBuf,
    branch: String,
    current_commit: String,
    git_state: String,
}

fn git_command(path: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|error| format!("Unable to start Git: {error}"))?;

    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if detail.is_empty() {
            "Git rejected the project path.".to_owned()
        } else {
            detail
        });
    }

    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|_| "Git returned invalid UTF-8 output.".to_owned())
}

fn inspect_repository(path: &Path) -> Result<GitSnapshot, String> {
    let root = PathBuf::from(git_command(path, &["rev-parse", "--show-toplevel"])?);
    let branch = git_command(&root, &["branch", "--show-current"])?;
    let current_commit = git_command(&root, &["rev-parse", "HEAD"])?;
    let changes = git_command(&root, &["status", "--porcelain"])?;

    Ok(GitSnapshot {
        root,
        branch: if branch.is_empty() {
            "(detached HEAD)".to_owned()
        } else {
            branch
        },
        current_commit,
        git_state: if changes.is_empty() {
            "clean".to_owned()
        } else {
            "dirty".to_owned()
        },
    })
}

fn project_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get(0)?,
        name: row.get(1)?,
        path: row.get(2)?,
        branch: row.get(3)?,
        current_commit: row.get(4)?,
        git_state: row.get(5)?,
    })
}

fn agent_session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentSession> {
    let token_count: i64 = row.get(8)?;
    Ok(AgentSession {
        id: row.get(0)?,
        project: row.get(1)?,
        task: row.get(2)?,
        branch: row.get(3)?,
        provider: row.get(4)?,
        status: row.get(5)?,
        step: row.get(6)?,
        elapsed: row.get(7)?,
        tokens: format!("{:.1}k", token_count as f64 / 1000.0),
        permission_mode: row.get(9)?,
        managed: row.get::<_, String>(10)? == "managed",
        last_event: row.get(11)?,
        archived: row.get::<_, i64>(12)? != 0,
    })
}

fn validate_session_status(status: &str) -> Result<(), String> {
    match status {
        "starting" | "running" | "waiting" | "idle" | "paused" | "completed" | "failed"
        | "orphaned" | "unknown" => Ok(()),
        _ => Err(format!("Unsupported agent session status: {status}")),
    }
}

fn copilot_executable() -> PathBuf {
    if let Some(executable) = std::env::var_os("COPILOT_CLI_PATH") {
        return PathBuf::from(executable);
    }

    if let Some(path) = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join("copilot"))
            .find(|candidate| candidate.is_file())
    }) {
        return path;
    }

    if let Some(home) = std::env::var_os("HOME") {
        let nvm_nodes = PathBuf::from(home).join(".nvm/versions/node");
        if let Ok(entries) = fs::read_dir(nvm_nodes) {
            let mut candidates = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path().join("bin/copilot"))
                .filter(|candidate| candidate.is_file())
                .collect::<Vec<_>>();
            candidates.sort();
            if let Some(candidate) = candidates.pop() {
                return candidate;
            }
        }
    }

    PathBuf::from("copilot")
}

fn copilot_command() -> Command {
    Command::new(copilot_executable())
}

fn copilot_session_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("copilot-{timestamp:x}")
}

fn copilot_content_text(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str().filter(|text| !text.is_empty()) {
        return Some(text.to_owned());
    }
    if let Some(text) = content
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        return Some(text.to_owned());
    }
    content.as_array().and_then(|parts| {
        let text = parts
            .iter()
            .filter_map(copilot_content_text)
            .collect::<String>();
        (!text.is_empty()).then_some(text)
    })
}

fn copilot_observation_from_line(line: &str) -> Result<Option<CopilotObservation>, String> {
    let message: Value = serde_json::from_str(line)
        .map_err(|error| format!("Copilot returned invalid ACP JSON: {error}"))?;

    if message.get("error").is_some() {
        return Ok(Some(CopilotObservation {
            status: Some("failed".to_owned()),
            step: Some("Copilot returned an ACP error".to_owned()),
            last_event: message["error"]["message"].as_str().map(str::to_owned),
        }));
    }

    if let Some(result) = message.get("result") {
        if result.get("stopReason").is_some() {
            return Ok(Some(CopilotObservation {
                status: Some("completed".to_owned()),
                step: Some("Prompt completed".to_owned()),
                last_event: result
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .map(|reason| format!("Prompt stopped: {reason}")),
            }));
        }
        return Ok(None);
    }

    if message.get("method").and_then(Value::as_str) != Some("session/update") {
        if message.get("method").and_then(Value::as_str) == Some("session/request_permission") {
            return Ok(Some(CopilotObservation {
                status: Some("waiting".to_owned()),
                step: Some("Waiting for permission".to_owned()),
                last_event: Some("Copilot requested tool permission".to_owned()),
            }));
        }
        return Ok(None);
    }

    let update = &message["params"]["update"];
    let update_type = update["sessionUpdate"].as_str().unwrap_or_default();
    let observation = match update_type {
        "agent_message_chunk" => {
            copilot_content_text(&update["content"]).map(|text| CopilotObservation {
                status: Some("running".to_owned()),
                step: Some("Receiving Copilot response".to_owned()),
                last_event: Some(text),
            })
        }
        "tool_call" | "tool_call_update" => {
            let title = update["title"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| copilot_content_text(&update["content"]))
                .unwrap_or_else(|| "Copilot is using a tool".to_owned());
            Some(CopilotObservation {
                status: Some("running".to_owned()),
                step: Some(title.clone()),
                last_event: Some(title),
            })
        }
        "user_message_chunk" => Some(CopilotObservation {
            status: Some("running".to_owned()),
            step: Some("Processing prompt".to_owned()),
            last_event: copilot_content_text(&update["content"]),
        }),
        _ => None,
    };

    Ok(observation)
}

fn send_acp_message(input: &Sender<String>, message: Value) -> Result<(), String> {
    let encoded = serde_json::to_string(&message)
        .map_err(|error| format!("Unable to encode Copilot protocol message: {error}"))?;
    input
        .send(encoded)
        .map_err(|_| "Copilot session input is no longer available.".to_owned())
}

fn permission_outcome(
    permission: &PendingPermission,
    option_id: Option<&str>,
) -> Result<Value, String> {
    if let Some(option_id) = option_id {
        if !permission
            .options
            .iter()
            .any(|option| option.option_id == option_id)
        {
            return Err("Choose one of the permission options provided by Copilot.".to_owned());
        }
        Ok(serde_json::json!({ "outcome": "selected", "optionId": option_id }))
    } else {
        Ok(serde_json::json!({ "outcome": "cancelled" }))
    }
}

fn acp_request(control: &SessionControl, method: &str, params: Value) -> Result<Value, String> {
    let id = control.next_request_id.fetch_add(1, Ordering::Relaxed);
    let (response_tx, response_rx) = mpsc::channel();
    control
        .pending
        .lock()
        .map_err(|_| "Copilot protocol state is unavailable.".to_owned())?
        .insert(id, response_tx);
    if let Err(error) = send_acp_message(
        &control.input,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }),
    ) {
        if let Ok(mut pending) = control.pending.lock() {
            pending.remove(&id);
        }
        return Err(error);
    }
    let response = match response_rx.recv_timeout(Duration::from_secs(600)) {
        Ok(response) => response,
        Err(_) => {
            if let Ok(mut pending) = control.pending.lock() {
                pending.remove(&id);
            }
            return Err(format!("Copilot did not respond to {method} in time."));
        }
    };
    if let Some(error) = response.get("error") {
        return Err(error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Copilot rejected the request.")
            .to_owned());
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| format!("Copilot returned an invalid response to {method}."))
}

fn insert_session_message(
    connection: &Connection,
    session_id: &str,
    role: &str,
    content: &str,
) -> Result<String, String> {
    let sequence: i64 = connection
        .query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1
             FROM session_messages WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Unable to sequence transcript message: {error}"))?;
    let message_id = copilot_session_id();
    connection
        .execute(
            "INSERT INTO session_messages (id, session_id, sequence, role, content)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![message_id, session_id, sequence, role, content],
        )
        .map_err(|error| format!("Unable to save transcript message: {error}"))?;
    Ok(message_id)
}

fn append_assistant_chunk(
    database: &Arc<Mutex<Connection>>,
    session_id: &str,
    message_id: &str,
    content: &str,
) -> Result<(), String> {
    let connection = database
        .lock()
        .map_err(|_| "Agent session database is unavailable.".to_owned())?;
    connection
        .execute(
            "UPDATE session_messages SET content = content || ?2
             WHERE id = ?1 AND session_id = ?3 AND role = 'assistant'",
            params![message_id, content, session_id],
        )
        .map_err(|error| format!("Unable to save Copilot response: {error}"))?;
    connection
        .execute(
            "UPDATE agent_sessions
             SET last_event = ?2, last_seen_at = CURRENT_TIMESTAMP,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1",
            params![session_id, content],
        )
        .map_err(|error| format!("Unable to update agent session: {error}"))?;
    Ok(())
}

fn update_session_status(
    database: &Arc<Mutex<Connection>>,
    session_id: &str,
    status: &str,
    step: &str,
    reason: &str,
) -> Result<(), String> {
    let connection = database
        .lock()
        .map_err(|_| "Agent session database is unavailable.".to_owned())?;
    connection
        .execute(
            "UPDATE agent_sessions
             SET status = ?2, current_step = ?3, status_reason = ?4,
                 last_seen_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP,
                 completed_at = CASE WHEN ?2 IN ('completed', 'failed') THEN CURRENT_TIMESTAMP ELSE completed_at END
             WHERE id = ?1",
            params![session_id, status, step, reason],
        )
        .map_err(|error| format!("Unable to update agent session: {error}"))?;
    Ok(())
}

fn send_copilot_message(
    database: &Arc<Mutex<Connection>>,
    session_id: &str,
    control: &SessionControl,
    message: &str,
    app: &tauri::AppHandle,
    is_initial_prompt: bool,
) -> Result<(), String> {
    let message = message.trim();
    if message.is_empty() {
        return Err("Enter a message before sending it to Copilot.".to_owned());
    }
    if message.len() > 16_000 {
        return Err("Messages must be 16,000 bytes or fewer.".to_owned());
    }
    let assistant_message_id = {
        let connection = database
            .lock()
            .map_err(|_| "Agent session database is unavailable.".to_owned())?;
        let status: String = connection
            .query_row(
                "SELECT status FROM agent_sessions WHERE id = ?1 AND origin = 'managed'",
                [session_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Unable to find managed agent session: {error}"))?;
        if status == "waiting"
            || (matches!(status.as_str(), "running" | "starting") && !is_initial_prompt)
        {
            return Err("Wait for Copilot to finish its current response.".to_owned());
        }
        insert_session_message(&connection, session_id, "user", message)?;
        let assistant_message_id =
            insert_session_message(&connection, session_id, "assistant", "")?;
        connection
            .execute(
                "UPDATE agent_sessions
                 SET status = 'running', status_reason = '', current_step = 'Sending message',
                     last_event = ?2, updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1",
                params![session_id, message],
            )
            .map_err(|error| format!("Unable to update agent session: {error}"))?;
        assistant_message_id
    };
    *control
        .assistant_message_id
        .lock()
        .map_err(|_| "Copilot transcript state is unavailable.".to_owned())? =
        Some(assistant_message_id);
    app.emit("agent-session-updated", session_id)
        .map_err(|error| format!("Unable to publish agent session update: {error}"))?;

    let external_session_id: String = database
        .lock()
        .map_err(|_| "Agent session database is unavailable.".to_owned())?
        .query_row(
            "SELECT external_session_id FROM agent_sessions WHERE id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Unable to read Copilot protocol session ID: {error}"))?;
    let result = acp_request(
        control,
        "session/prompt",
        serde_json::json!({
            "sessionId": external_session_id,
            "prompt": [{ "type": "text", "text": message }]
        }),
    );
    *control
        .assistant_message_id
        .lock()
        .map_err(|_| "Copilot transcript state is unavailable.".to_owned())? = None;
    match result {
        Ok(_) => {
            update_session_status(database, session_id, "idle", "Ready for your message", "")?;
            app.emit("agent-session-updated", session_id)
                .map_err(|error| format!("Unable to publish agent session update: {error}"))?;
            Ok(())
        }
        Err(error) => {
            update_session_status(
                database,
                session_id,
                "failed",
                "Copilot request failed",
                &error,
            )?;
            let _ = app.emit("agent-session-updated", session_id);
            Err(error)
        }
    }
}

async fn run_agent_operation<F, T>(operation: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(|error| format!("Agent operation failed in the background: {error}"))?
}

fn tool_settings_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ToolSettings> {
    Ok(ToolSettings {
        enabled: row.get(0)?,
    })
}

fn default_tool_settings() -> ToolSettings {
    ToolSettings { enabled: false }
}

fn read_tool_settings(connection: &Connection, provider: &str) -> Result<ToolSettings, String> {
    connection
        .query_row(
            "SELECT enabled FROM tool_settings WHERE provider = ?1",
            [provider],
            tool_settings_from_row,
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => "not configured".to_owned(),
            other => format!("Unable to read {provider} settings: {other}"),
        })
}

fn github_token_configured() -> bool {
    keyring::Entry::new(GITHUB_CREDENTIAL_SERVICE, GITHUB_CREDENTIAL_USER)
        .and_then(|entry| entry.get_password())
        .is_ok()
}

#[tauri::command]
fn load_settings(database: State<'_, Database>) -> Result<Settings, String> {
    let connection = database
        .0
        .lock()
        .map_err(|_| "Settings database is unavailable.".to_owned())?;
    let read = |provider: &str| {
        read_tool_settings(&connection, provider).unwrap_or_else(|_| default_tool_settings())
    };
    let github_username = connection
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'github_username'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_default();
    let default_provider = connection
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'default_provider'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| "claudeCode".to_owned());
    let transcript_font_size = connection
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'transcript_font_size'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|value| value.parse::<u8>().ok())
        .filter(|size| (12..=24).contains(size))
        .unwrap_or(16);

    Ok(Settings {
        copilot: read("copilot"),
        gemini: read("gemini"),
        claude_code: read("claude_code"),
        github_username,
        github_token_configured: github_token_configured(),
        default_provider,
        transcript_font_size,
    })
}

#[tauri::command]
fn save_settings(input: SettingsInput, database: State<'_, Database>) -> Result<Settings, String> {
    if !matches!(
        input.default_provider.as_str(),
        "copilot" | "gemini" | "claudeCode"
    ) {
        return Err("Choose a supported default AI agent.".to_owned());
    }
    if !(12..=24).contains(&input.transcript_font_size) {
        return Err("Transcript text size must be between 12px and 24px.".to_owned());
    }

    let github_token = input.github_token.trim().to_owned();
    if !github_token.is_empty() {
        keyring::Entry::new(GITHUB_CREDENTIAL_SERVICE, GITHUB_CREDENTIAL_USER)
            .map_err(|error| format!("Unable to access the OS credential vault: {error}"))?
            .set_password(&github_token)
            .map_err(|error| {
                format!("Unable to save the GitHub token in the OS credential vault: {error}")
            })?;
    }

    let connection = database
        .0
        .lock()
        .map_err(|_| "Settings database is unavailable.".to_owned())?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| format!("Unable to start settings transaction: {error}"))?;

    for (provider, settings) in [
        ("copilot", &input.copilot),
        ("gemini", &input.gemini),
        ("claude_code", &input.claude_code),
    ] {
        transaction
            .execute(
                "INSERT INTO tool_settings (provider, enabled)
                     VALUES (?1, ?2)
                 ON CONFLICT(provider) DO UPDATE SET
                   enabled = excluded.enabled",
                params![provider, settings.enabled],
            )
            .map_err(|error| format!("Unable to save {provider} settings: {error}"))?;
    }
    transaction
        .execute(
            "INSERT INTO app_settings (key, value) VALUES ('github_username', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [&input.github_username.trim()],
        )
        .map_err(|error| format!("Unable to save GitHub settings: {error}"))?;
    transaction
        .execute(
            "INSERT INTO app_settings (key, value) VALUES ('default_provider', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [&input.default_provider],
        )
        .map_err(|error| format!("Unable to save default provider: {error}"))?;
    transaction
        .execute(
            "INSERT INTO app_settings (key, value) VALUES ('transcript_font_size', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [input.transcript_font_size.to_string()],
        )
        .map_err(|error| format!("Unable to save transcript text size: {error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("Unable to commit settings: {error}"))?;

    drop(connection);
    load_settings(database)
}

#[tauri::command]
fn list_projects(database: State<'_, Database>) -> Result<Vec<Project>, String> {
    let connection = database
        .0
        .lock()
        .map_err(|_| "Project database is unavailable.".to_owned())?;
    let mut statement = connection
        .prepare(
            "SELECT id, name, path, branch, current_commit, git_state
             FROM projects ORDER BY name COLLATE NOCASE",
        )
        .map_err(|error| format!("Unable to read projects: {error}"))?;

    statement
        .query_map([], project_from_row)
        .map_err(|error| format!("Unable to read projects: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Unable to decode project data: {error}"))
}

fn list_agent_sessions_from_connection(
    connection: &Connection,
    project_id: Option<&str>,
    status: Option<&str>,
    archived: Option<bool>,
) -> Result<Vec<AgentSession>, String> {
    if let Some(value) = status {
        validate_session_status(value)?;
    }

    let mut statement = connection
        .prepare(
            "SELECT s.id, p.name, s.task, s.branch, s.provider, s.status,
                    s.current_step, s.started_at, s.token_count,
                    s.permission_mode, s.origin, s.last_event, s.archived
             FROM agent_sessions s
             JOIN projects p ON p.id = s.project_id
             WHERE (?1 IS NULL OR s.project_id = ?1)
               AND (?2 IS NULL OR s.status = ?2)
               AND s.archived = COALESCE(?3, 0)
             ORDER BY COALESCE(s.last_seen_at, s.updated_at) DESC",
        )
        .map_err(|error| format!("Unable to read agent sessions: {error}"))?;

    statement
        .query_map(
            params![project_id, status, archived.map(|value| i64::from(value))],
            agent_session_from_row,
        )
        .map_err(|error| format!("Unable to read agent sessions: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Unable to decode agent session data: {error}"))
}

#[tauri::command]
fn get_session_transcript(
    session_id: String,
    database: State<'_, Database>,
) -> Result<Vec<TranscriptMessage>, String> {
    let connection = database
        .0
        .lock()
        .map_err(|_| "Agent session database is unavailable.".to_owned())?;
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_sessions WHERE id = ?1)",
            [&session_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Unable to verify agent session: {error}"))?;
    if !exists {
        return Err("Agent session was not found.".to_owned());
    }
    let mut statement = connection
        .prepare(
            "SELECT role, content, created_at
             FROM session_messages
             WHERE session_id = ?1
               AND (role = 'user' OR content <> '')
             ORDER BY sequence DESC
             LIMIT 100",
        )
        .map_err(|error| format!("Unable to query local session transcript: {error}"))?;
    let rows = statement
        .query_map([session_id], |row| {
            Ok(TranscriptMessage {
                role: row.get(0)?,
                content: row.get(1)?,
                timestamp: row.get(2)?,
            })
        })
        .map_err(|error| format!("Unable to read local session transcript: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Unable to decode local session transcript: {error}"))?;
    let mut messages = rows;
    messages.reverse();
    Ok(messages)
}

#[tauri::command]
fn list_agent_sessions(
    project_id: Option<String>,
    status: Option<String>,
    archived: Option<bool>,
    database: State<'_, Database>,
) -> Result<Vec<AgentSession>, String> {
    let connection = database
        .0
        .lock()
        .map_err(|_| "Agent session database is unavailable.".to_owned())?;
    list_agent_sessions_from_connection(
        &connection,
        project_id.as_deref(),
        status.as_deref(),
        archived,
    )
}

#[tauri::command]
async fn send_session_message(
    session_id: String,
    message: String,
    database: State<'_, Database>,
    processes: State<'_, SessionProcesses>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let database = Database(database.0.clone());
    let processes = SessionProcesses(processes.0.clone());
    run_agent_operation(move || {
        let control = processes
            .0
            .lock()
            .map_err(|_| "Agent session process list is unavailable.".to_owned())?
            .get(&session_id)
            .cloned()
            .ok_or_else(|| {
                "This session is not connected to a managed Copilot process.".to_owned()
            })?;
        send_copilot_message(&database.0, &session_id, &control, &message, &app, false)
    })
    .await
}

#[tauri::command]
fn get_pending_session_permission(
    session_id: String,
    processes: State<'_, SessionProcesses>,
) -> Result<Option<PendingPermission>, String> {
    let active = processes
        .0
        .lock()
        .map_err(|_| "Agent session process list is unavailable.".to_owned())?;
    let Some(control) = active.get(&session_id) else {
        return Ok(None);
    };
    Ok(control
        .permission_requests
        .lock()
        .map_err(|_| "Copilot permission state is unavailable.".to_owned())?
        .values()
        .next()
        .map(|(_, permission)| permission.clone()))
}

#[tauri::command]
fn respond_session_permission(
    session_id: String,
    request_id: String,
    option_id: Option<String>,
    database: State<'_, Database>,
    processes: State<'_, SessionProcesses>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let control = processes
        .0
        .lock()
        .map_err(|_| "Agent session process list is unavailable.".to_owned())?
        .get(&session_id)
        .cloned()
        .ok_or_else(|| "This session is not connected to Copilot.".to_owned())?;
    let (protocol_id, permission) = control
        .permission_requests
        .lock()
        .map_err(|_| "Copilot permission state is unavailable.".to_owned())?
        .get(&request_id)
        .cloned()
        .ok_or_else(|| "This Copilot permission request is no longer active.".to_owned())?;
    let outcome = permission_outcome(&permission, option_id.as_deref())?;
    send_acp_message(
        &control.input,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": protocol_id,
            "result": { "outcome": outcome }
        }),
    )?;
    control
        .permission_requests
        .lock()
        .map_err(|_| "Copilot permission state is unavailable.".to_owned())?
        .remove(&request_id);
    update_session_status(
        &database.0,
        &session_id,
        "running",
        "Copilot is continuing",
        "",
    )?;
    app.emit("agent-session-updated", &session_id)
        .map_err(|error| format!("Unable to publish agent session update: {error}"))?;
    Ok(())
}

#[tauri::command]
fn stop_agent_session(
    session_id: String,
    database: State<'_, Database>,
    processes: State<'_, SessionProcesses>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let connection = database
        .0
        .lock()
        .map_err(|_| "Agent session database is unavailable.".to_owned())?;
    let managed: bool = connection
        .query_row(
            "SELECT origin = 'managed' FROM agent_sessions WHERE id = ?1",
            [&session_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Unable to find agent session: {error}"))?;
    if !managed {
        return Err("Only Agent Control Plane-managed sessions can be stopped.".to_owned());
    }
    drop(connection);
    let control = processes
        .0
        .lock()
        .map_err(|_| "Agent session process list is unavailable.".to_owned())?
        .get(&session_id)
        .cloned()
        .ok_or_else(|| "This session is not running in Agent Control Plane.".to_owned())?;
    control.stop_requested.store(true, Ordering::Relaxed);
    control
        .child
        .lock()
        .map_err(|_| "Copilot process state is unavailable.".to_owned())?
        .kill()
        .map_err(|error| format!("Unable to stop Copilot session: {error}"))?;
    update_session_status(
        &database.0,
        &session_id,
        "completed",
        "Stopped by user.",
        "Stopped by user.",
    )?;
    app.emit("agent-session-updated", &session_id)
        .map_err(|error| format!("Unable to publish agent session update: {error}"))?;
    Ok(())
}

#[tauri::command]
fn set_session_archived(
    session_id: String,
    archived: bool,
    database: State<'_, Database>,
) -> Result<(), String> {
    let connection = database
        .0
        .lock()
        .map_err(|_| "Agent session database is unavailable.".to_owned())?;
    let changed = connection
        .execute(
            "UPDATE agent_sessions
             SET archived = ?2, updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1",
            params![session_id, i64::from(archived)],
        )
        .map_err(|error| format!("Unable to update agent session archive state: {error}"))?;
    if changed == 0 {
        return Err("Agent session was not found.".to_owned());
    }
    Ok(())
}

fn persist_copilot_observation(
    database: &Arc<Mutex<Connection>>,
    session_id: &str,
    observation: &CopilotObservation,
) -> Result<(), String> {
    let connection = database
        .lock()
        .map_err(|_| "Agent session database is unavailable.".to_owned())?;
    connection
        .execute(
            "UPDATE agent_sessions
             SET status = COALESCE(?2, status),
                 current_step = COALESCE(?3, current_step),
                 last_event = COALESCE(?4, last_event),
                 last_seen_at = CURRENT_TIMESTAMP,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1",
            params![
                session_id,
                observation.status,
                observation.step,
                observation.last_event
            ],
        )
        .map_err(|error| format!("Unable to update agent session: {error}"))?;

    let sequence: i64 = connection
        .query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM session_events WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Unable to sequence agent session event: {error}"))?;
    let payload = serde_json::to_string(observation)
        .map_err(|error| format!("Unable to encode agent session event: {error}"))?;
    connection
        .execute(
            "INSERT INTO session_events
             (id, session_id, sequence, event_type, payload)
             VALUES (?1, ?2, ?3, 'copilot_observation', ?4)",
            params![copilot_session_id(), session_id, sequence, payload],
        )
        .map_err(|error| format!("Unable to persist agent session event: {error}"))?;
    Ok(())
}

fn spawn_copilot_acp(
    project_path: &str,
    session_id: &str,
    database: Arc<Mutex<Connection>>,
    processes: Arc<Mutex<HashMap<String, SessionControl>>>,
    app: tauri::AppHandle,
) -> Result<SessionControl, String> {
    let mut child = Command::new(copilot_executable())
        .arg("--acp")
        .current_dir(project_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Unable to start Copilot ACP: {error}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Copilot did not provide an input stream.".to_owned())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Copilot did not provide an output stream.".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Copilot did not provide an error stream.".to_owned())?;
    let child = Arc::new(Mutex::new(child));
    let (input, input_rx) = mpsc::channel::<String>();
    let control = SessionControl {
        input,
        pending: Arc::new(Mutex::new(HashMap::new())),
        permission_requests: Arc::new(Mutex::new(HashMap::new())),
        next_request_id: Arc::new(AtomicU64::new(1)),
        stop_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        child: child.clone(),
        assistant_message_id: Arc::new(Mutex::new(None)),
    };

    thread::spawn(move || {
        let mut writer = std::io::BufWriter::new(stdin);
        for message in input_rx {
            if writeln!(writer, "{message}")
                .and_then(|_| writer.flush())
                .is_err()
            {
                break;
            }
        }
    });
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if line.is_err() {
                break;
            }
        }
    });

    let reader_control = control.clone();
    let reader_session_id = session_id.to_owned();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                let _ = update_session_status(
                    &database,
                    &reader_session_id,
                    "failed",
                    "Invalid Copilot protocol output",
                    "Copilot returned invalid ACP JSON.",
                );
                let _ = app.emit("agent-session-updated", &reader_session_id);
                continue;
            };

            if message.get("method").and_then(Value::as_str) == Some("session/request_permission") {
                let request_id = message["id"].to_string();
                let options = message["params"]["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|option| {
                        Some(PermissionOption {
                            option_id: option.get("optionId")?.as_str()?.to_owned(),
                            name: option.get("name")?.as_str()?.to_owned(),
                            kind: option.get("kind")?.as_str()?.to_owned(),
                        })
                    })
                    .collect::<Vec<_>>();
                let permission = PendingPermission {
                    request_id: request_id.clone(),
                    title: message["params"]["toolCall"]["title"]
                        .as_str()
                        .unwrap_or("Copilot requested tool permission")
                        .to_owned(),
                    options,
                };
                if let Ok(mut requests) = reader_control.permission_requests.lock() {
                    requests.insert(request_id, (message["id"].clone(), permission.clone()));
                }
                let observation = CopilotObservation {
                    status: Some("waiting".to_owned()),
                    step: Some("Waiting for permission".to_owned()),
                    last_event: Some(permission.title.clone()),
                };
                let _ = persist_copilot_observation(&database, &reader_session_id, &observation);
                if permission.options.is_empty() {
                    let _ = send_acp_message(
                        &reader_control.input,
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": message["id"],
                            "result": { "outcome": { "outcome": "cancelled" } }
                        }),
                    );
                    if let Ok(mut requests) = reader_control.permission_requests.lock() {
                        requests.remove(&permission.request_id);
                    }
                } else {
                    let _ = app.emit(
                        "agent-session-permission-requested",
                        serde_json::json!({
                            "sessionId": reader_session_id,
                            "permission": permission
                        }),
                    );
                }
                let _ = app.emit("agent-session-updated", &reader_session_id);
                continue;
            }

            if let Some(id) = message.get("id").and_then(Value::as_u64) {
                if let Ok(mut pending) = reader_control.pending.lock() {
                    if let Some(sender) = pending.remove(&id) {
                        let _ = sender.send(message);
                    }
                }
                continue;
            }

            match copilot_observation_from_line(&line) {
                Ok(Some(observation)) => {
                    if message["params"]["update"]["sessionUpdate"].as_str()
                        == Some("agent_message_chunk")
                    {
                        if let Some(content) =
                            copilot_content_text(&message["params"]["update"]["content"])
                        {
                            if let Ok(message_id) = reader_control.assistant_message_id.lock() {
                                if let Some(message_id) = message_id.as_deref() {
                                    let _ = append_assistant_chunk(
                                        &database,
                                        &reader_session_id,
                                        message_id,
                                        &content,
                                    );
                                }
                            }
                        }
                    }
                    let _ =
                        persist_copilot_observation(&database, &reader_session_id, &observation);
                    let _ = app.emit("agent-session-updated", &reader_session_id);
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = update_session_status(
                        &database,
                        &reader_session_id,
                        "failed",
                        "Copilot protocol error",
                        &error,
                    );
                    let _ = app.emit("agent-session-updated", &reader_session_id);
                }
            }
        }

        if let Ok(mut pending) = reader_control.pending.lock() {
            for (_, sender) in pending.drain() {
                let _ = sender.send(serde_json::json!({
                    "error": { "message": "Copilot ACP process exited before responding." }
                }));
            }
        }
        let success = child
            .lock()
            .map(|mut process| {
                process
                    .wait()
                    .map(|status| status.success())
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        let stopped = reader_control.stop_requested.load(Ordering::Relaxed);
        let status = if success || stopped {
            "completed"
        } else {
            "failed"
        };
        let reason = if stopped {
            "Stopped by user."
        } else if success {
            "Copilot ACP session ended."
        } else {
            "Copilot ACP process exited unexpectedly."
        };
        let _ = update_session_status(&database, &reader_session_id, status, reason, reason);
        if let Ok(mut active) = processes.lock() {
            active.remove(&reader_session_id);
        }
        let _ = app.emit("agent-session-updated", &reader_session_id);
    });

    Ok(control)
}

#[tauri::command]
async fn start_copilot_session(
    project_id: Option<String>,
    task: String,
    resume_session_id: Option<String>,
    database: State<'_, Database>,
    processes: State<'_, SessionProcesses>,
    app: tauri::AppHandle,
) -> Result<AgentSession, String> {
    let database = Database(database.0.clone());
    let processes = SessionProcesses(processes.0.clone());
    run_agent_operation(move || {
        start_copilot_session_blocking(
            project_id,
            task,
            resume_session_id,
            database,
            processes,
            app,
        )
    })
    .await
}

fn start_copilot_session_blocking(
    project_id: Option<String>,
    task: String,
    resume_session_id: Option<String>,
    database: Database,
    processes: SessionProcesses,
    app: tauri::AppHandle,
) -> Result<AgentSession, String> {
    let mut task = task.trim().to_owned();
    if let Some(resume_session_id) = resume_session_id.as_deref() {
        if processes
            .0
            .lock()
            .map_err(|_| "Agent session process list is unavailable.".to_owned())?
            .contains_key(resume_session_id)
        {
            return Err("This Copilot session is already connected.".to_owned());
        }
    }
    let (project_id, project_path, branch, session_id, resume_external_id) = {
        let connection = database
            .0
            .lock()
            .map_err(|_| "Project database is unavailable.".to_owned())?;
        if let Some(session_id) = resume_session_id.as_deref() {
            let (project_id, external_id, stored_task): (String, String, String) = connection
                .query_row(
                    "SELECT project_id, external_session_id, task
                     FROM agent_sessions
                     WHERE id = ?1 AND provider = 'copilot'",
                    [session_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(|error| format!("Unable to find Copilot session to resume: {error}"))?;
            if task.is_empty() {
                task = stored_task;
            }
            let (path, branch) = connection
                .query_row(
                    "SELECT path, branch FROM projects WHERE id = ?1",
                    [&project_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .map_err(|error| format!("Unable to find project for Copilot session: {error}"))?;
            (
                project_id,
                path,
                branch,
                session_id.to_owned(),
                Some(external_id),
            )
        } else {
            let project_id = project_id
                .ok_or_else(|| "A project is required to start a Copilot session.".to_owned())?;
            if task.is_empty() {
                return Err("Describe the task before starting a Copilot session.".to_owned());
            }
            let (path, branch) = connection
                .query_row(
                    "SELECT path, branch FROM projects WHERE id = ?1",
                    [&project_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .map_err(|error| format!("Unable to find project for Copilot session: {error}"))?;
            (project_id, path, branch, copilot_session_id(), None)
        }
    };

    let version = copilot_command()
        .arg("--version")
        .output()
        .map_err(|error| format!("Unable to start Copilot CLI: {error}"))?;
    if !version.status.success() {
        let detail = String::from_utf8_lossy(&version.stderr).trim().to_owned();
        return Err(if detail.is_empty() {
            "Copilot CLI rejected the version check.".to_owned()
        } else {
            format!("Copilot CLI rejected the version check: {detail}")
        });
    }

    {
        let connection = database
            .0
            .lock()
            .map_err(|_| "Agent session database is unavailable.".to_owned())?;
        if resume_external_id.is_some() {
            connection.execute(
                "UPDATE agent_sessions
                     SET project_id = ?2, task = ?3, branch = ?4, status = 'starting',
                         status_reason = '', current_step = 'Starting Copilot session',
                         origin = 'managed', archived = 0,
                         updated_at = CURRENT_TIMESTAMP
                     WHERE id = ?1",
                params![session_id, project_id, task, branch],
            )
        } else {
            connection.execute(
                "INSERT INTO agent_sessions
                     (id, project_id, provider, task, branch, status, status_reason,
                      current_step, permission_mode, origin, last_event)
                     VALUES (?1, ?2, 'copilot', ?3, ?4, 'starting', '',
                             'Starting Copilot session', 'plan', 'managed', '')",
                params![session_id, project_id, task, branch],
            )
        }
        .map_err(|error| format!("Unable to persist Copilot session: {error}"))?;
    }

    let processes_ref = processes.0.clone();
    let control = match spawn_copilot_acp(
        &project_path,
        &session_id,
        database.0.clone(),
        processes_ref.clone(),
        app.clone(),
    ) {
        Ok(control) => control,
        Err(error) => {
            update_session_status(
                &database.0,
                &session_id,
                "failed",
                "Unable to start Copilot ACP",
                &error,
            )?;
            return Err(error);
        }
    };
    let initialization = acp_request(
        &control,
        "initialize",
        serde_json::json!({
            "protocolVersion": 1,
            "clientCapabilities": {},
            "clientInfo": {
                "name": "Agent Control Plane",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
    );
    if let Err(error) = initialization {
        let _ = control.child.lock().map(|mut process| process.kill());
        update_session_status(
            &database.0,
            &session_id,
            "failed",
            "Copilot initialization failed",
            &error,
        )?;
        return Err(error);
    }

    let session_result = if let Some(external_id) = resume_external_id.as_deref() {
        acp_request(
            &control,
            "session/load",
            serde_json::json!({
                "cwd": project_path,
                "sessionId": external_id,
                "mcpServers": []
            }),
        )
    } else {
        acp_request(
            &control,
            "session/new",
            serde_json::json!({
                "cwd": project_path,
                "mcpServers": []
            }),
        )
    };
    let session_result = match session_result {
        Ok(result) => result,
        Err(error) => {
            let _ = control.child.lock().map(|mut process| process.kill());
            update_session_status(
                &database.0,
                &session_id,
                "failed",
                "Unable to load Copilot session",
                &error,
            )?;
            return Err(error);
        }
    };
    let external_id = session_result
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or(resume_external_id);
    let Some(external_id) = external_id else {
        let error = "Copilot did not return a session identifier.".to_owned();
        let _ = control.child.lock().map(|mut process| process.kill());
        update_session_status(
            &database.0,
            &session_id,
            "failed",
            "Invalid Copilot session response",
            &error,
        )?;
        return Err(error);
    };
    {
        let connection = database
            .0
            .lock()
            .map_err(|_| "Agent session database is unavailable.".to_owned())?;
        connection
            .execute(
                "UPDATE agent_sessions
                 SET external_session_id = ?2, pid = ?3, status = ?4,
                     current_step = ?5, status_reason = '',
                     updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1",
                params![
                    session_id,
                    external_id,
                    control
                        .child
                        .lock()
                        .map_err(|_| "Copilot process state is unavailable.".to_owned())?
                        .id(),
                    if resume_session_id.is_none() {
                        "running"
                    } else {
                        "idle"
                    },
                    if resume_session_id.is_none() {
                        "Sending initial prompt"
                    } else {
                        "Ready for your message"
                    }
                ],
            )
            .map_err(|error| format!("Unable to save Copilot protocol session ID: {error}"))?;
    }
    processes_ref
        .lock()
        .map_err(|_| "Agent session process list is unavailable.".to_owned())?
        .insert(session_id.clone(), control.clone());

    if resume_session_id.is_none() {
        let database_ref = database.0.clone();
        let initial_control = control.clone();
        let initial_session_id = session_id.clone();
        let initial_task = task.clone();
        let initial_app = app.clone();
        thread::spawn(move || {
            if let Err(error) = send_copilot_message(
                &database_ref,
                &initial_session_id,
                &initial_control,
                &initial_task,
                &initial_app,
                true,
            ) {
                let _ = update_session_status(
                    &database_ref,
                    &initial_session_id,
                    "failed",
                    "Unable to send initial prompt",
                    &error,
                );
                let _ = initial_app.emit("agent-session-updated", &initial_session_id);
            }
        });
    }
    app.emit("agent-session-updated", &session_id)
        .map_err(|error| format!("Unable to publish agent session update: {error}"))?;

    let connection = database
        .0
        .lock()
        .map_err(|_| "Agent session database is unavailable.".to_owned())?;
    list_agent_sessions_from_connection(&connection, None, None, Some(false))?
        .into_iter()
        .find(|session| session.id == session_id)
        .ok_or_else(|| "Copilot session was started but could not be read back.".to_owned())
}

#[tauri::command]
fn register_project(path: String, database: State<'_, Database>) -> Result<Project, String> {
    let requested_path = PathBuf::from(path.trim());
    if requested_path.as_os_str().is_empty() {
        return Err("Choose a local repository path.".to_owned());
    }

    let canonical_path = fs::canonicalize(&requested_path)
        .map_err(|error| format!("The project path is not accessible: {error}"))?;
    let snapshot = inspect_repository(&canonical_path)?;
    let project_path = snapshot
        .root
        .to_str()
        .ok_or_else(|| "The repository path is not valid UTF-8.".to_owned())?
        .to_owned();
    let project = Project {
        id: project_path.clone(),
        name: snapshot
            .root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Unnamed project")
            .to_owned(),
        path: project_path.clone(),
        branch: snapshot.branch,
        current_commit: snapshot.current_commit,
        git_state: snapshot.git_state,
    };

    let connection = database
        .0
        .lock()
        .map_err(|_| "Project database is unavailable.".to_owned())?;
    connection
        .execute(
            "INSERT INTO projects (id, name, path, branch, current_commit, git_state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name,
               path = excluded.path,
               branch = excluded.branch,
               current_commit = excluded.current_commit,
               git_state = excluded.git_state,
               updated_at = CURRENT_TIMESTAMP",
            params![
                project.id,
                project.name,
                project.path,
                project.branch,
                project.current_commit,
                project.git_state
            ],
        )
        .map_err(|error| format!("Unable to save project: {error}"))?;

    Ok(project)
}

fn initialize_schema(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE IF NOT EXISTS projects (
           id TEXT PRIMARY KEY NOT NULL,
           name TEXT NOT NULL,
           path TEXT NOT NULL,
           branch TEXT NOT NULL,
           current_commit TEXT NOT NULL,
           git_state TEXT NOT NULL,
           created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
         );
         CREATE TABLE IF NOT EXISTS tool_settings (
           provider TEXT PRIMARY KEY NOT NULL,
           enabled INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS app_settings (
           key TEXT PRIMARY KEY NOT NULL,
           value TEXT NOT NULL DEFAULT ''
         );
         CREATE TABLE IF NOT EXISTS agent_sessions (
           id TEXT PRIMARY KEY NOT NULL,
           project_id TEXT NOT NULL,
           provider TEXT NOT NULL,
           external_session_id TEXT,
           task TEXT NOT NULL,
           branch TEXT NOT NULL DEFAULT '',
           status TEXT NOT NULL,
           status_reason TEXT NOT NULL DEFAULT '',
           current_step TEXT NOT NULL DEFAULT '',
           token_count INTEGER NOT NULL DEFAULT 0,
           permission_mode TEXT NOT NULL DEFAULT 'plan',
           origin TEXT NOT NULL DEFAULT 'observed',
           last_event TEXT NOT NULL DEFAULT '',
           pid INTEGER,
           process_start_time TEXT,
           transcript_path TEXT,
           started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           last_seen_at TEXT,
           completed_at TEXT,
           archived INTEGER NOT NULL DEFAULT 0,
           created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           UNIQUE(provider, external_session_id)
         );
         CREATE INDEX IF NOT EXISTS idx_agent_sessions_project
           ON agent_sessions(project_id);
         CREATE INDEX IF NOT EXISTS idx_agent_sessions_status
           ON agent_sessions(status);
         CREATE INDEX IF NOT EXISTS idx_agent_sessions_updated
           ON agent_sessions(updated_at DESC);
         CREATE TABLE IF NOT EXISTS session_events (
           id TEXT PRIMARY KEY NOT NULL,
           session_id TEXT NOT NULL,
           sequence INTEGER NOT NULL,
           event_type TEXT NOT NULL,
           payload TEXT NOT NULL,
           observed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           UNIQUE(session_id, sequence)
         );
         CREATE TABLE IF NOT EXISTS session_messages (
           id TEXT PRIMARY KEY NOT NULL,
           session_id TEXT NOT NULL,
           sequence INTEGER NOT NULL,
           role TEXT NOT NULL CHECK(role IN ('user', 'assistant')),
           content TEXT NOT NULL DEFAULT '',
           created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           UNIQUE(session_id, sequence)
         );",
    )?;
    connection.execute(
        "UPDATE agent_sessions
         SET status = 'orphaned',
             status_reason = 'Agent Control Plane restarted; reconnect the session to continue.',
             current_step = 'Reconnect required',
             updated_at = CURRENT_TIMESTAMP
         WHERE origin = 'managed'
           AND status IN ('starting', 'running', 'waiting', 'idle')",
        [],
    )?;
    connection
        .execute(
            "ALTER TABLE agent_sessions ADD COLUMN archived INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .or_else(|error| {
            if error
                .to_string()
                .contains("duplicate column name: archived")
            {
                Ok(0)
            } else {
                Err(error)
            }
        })?;
    Ok(())
}

fn open_database(app: &tauri::AppHandle) -> Result<Database, Box<dyn std::error::Error>> {
    let data_dir = app.path().app_data_dir()?;
    fs::create_dir_all(&data_dir)?;
    let database_path = data_dir.join("agent-control-plane.sqlite3");
    let connection = Connection::open(database_path)?;
    initialize_schema(&connection)?;
    Ok(Database(Arc::new(Mutex::new(connection))))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            app.manage(open_database(app.handle())?);
            app.manage(SessionProcesses(Arc::new(Mutex::new(HashMap::new()))));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_projects,
            list_agent_sessions,
            send_session_message,
            get_pending_session_permission,
            respond_session_permission,
            stop_agent_session,
            set_session_archived,
            get_session_transcript,
            start_copilot_session,
            register_project,
            load_settings,
            save_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running Agent Control Plane");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocking_agent_operations_run_off_the_calling_thread() {
        let calling_thread = thread::current().id();
        let operation_thread =
            tauri::async_runtime::block_on(run_agent_operation(|| Ok(thread::current().id())))
                .expect("background agent operation");

        assert_ne!(operation_thread, calling_thread);
    }

    #[test]
    fn new_database_has_an_empty_session_read_model() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        initialize_schema(&connection).expect("schema");

        let sessions = list_agent_sessions_from_connection(&connection, None, None, None)
            .expect("session query");

        assert!(sessions.is_empty());
    }

    #[test]
    fn session_read_model_returns_persisted_copilot_session() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        initialize_schema(&connection).expect("schema");
        connection
            .execute(
                "INSERT INTO projects (id, name, path, branch, current_commit, git_state)
                 VALUES ('project-1', 'Control Plane', '/tmp/control-plane', 'main', 'abc', 'clean')",
                [],
            )
            .expect("project");
        connection
            .execute(
                "INSERT INTO agent_sessions
                 (id, project_id, provider, task, branch, status, status_reason,
                  current_step, token_count, permission_mode, origin, last_event)
                 VALUES ('session-1', 'project-1', 'copilot', 'Implement sessions',
                         'feat/sessions', 'waiting', 'Agent asked a question',
                         'Waiting for your answer', 12800, 'plan', 'managed',
                         'Which provider should be used?')",
                [],
            )
            .expect("session");

        let sessions = list_agent_sessions_from_connection(&connection, None, None, None)
            .expect("session query");

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "session-1");
        assert_eq!(sessions[0].provider, "copilot");
        assert_eq!(sessions[0].status, "waiting");
        assert!(sessions[0].managed);
        assert_eq!(sessions[0].tokens, "12.8k");
    }

    #[test]
    fn transcript_messages_are_persisted_and_streamed_in_the_local_database() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        initialize_schema(&connection).expect("schema");
        let user_id = insert_session_message(&connection, "session-1", "user", "Check the tests")
            .expect("user message");
        let assistant_id =
            insert_session_message(&connection, "session-1", "assistant", "").expect("assistant");
        assert_ne!(user_id, assistant_id);

        let database = Arc::new(Mutex::new(connection));
        append_assistant_chunk(&database, "session-1", &assistant_id, "All tests ")
            .expect("first response chunk");
        append_assistant_chunk(&database, "session-1", &assistant_id, "passed.")
            .expect("second response chunk");

        let connection = database.lock().expect("database lock");
        let mut messages = connection
            .prepare(
                "SELECT role, content FROM session_messages
                 WHERE session_id = ?1 ORDER BY sequence",
            )
            .expect("query");
        let values = messages
            .query_map(["session-1"], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("message values");
        assert_eq!(
            values,
            vec![
                ("user".to_owned(), "Check the tests".to_owned()),
                ("assistant".to_owned(), "All tests passed.".to_owned()),
            ]
        );
    }

    #[test]
    fn session_status_validation_rejects_unknown_states() {
        assert!(validate_session_status("running").is_ok());
        assert!(validate_session_status("waiting").is_ok());
        assert!(validate_session_status("idle").is_ok());
        assert!(validate_session_status("not-a-session-state").is_err());
    }

    #[test]
    fn copilot_agent_message_is_mapped_to_running_observation() {
        let line = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Checking the repository"}}}}"#;

        let observation = copilot_observation_from_line(line)
            .expect("valid ACP message")
            .expect("agent message observation");

        assert_eq!(
            observation,
            CopilotObservation {
                status: Some("running".to_owned()),
                step: Some("Receiving Copilot response".to_owned()),
                last_event: Some("Checking the repository".to_owned()),
            }
        );
    }

    #[test]
    fn copilot_agent_message_array_content_is_mapped_to_running_observation() {
        let line = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":[{"type":"text","text":"Checking "},{"type":"text","text":"the repository"}]}}}"#;

        let observation = copilot_observation_from_line(line)
            .expect("valid ACP message")
            .expect("agent message observation");

        assert_eq!(
            observation.last_event.as_deref(),
            Some("Checking the repository")
        );
    }

    #[test]
    fn copilot_permission_request_is_waiting_observation() {
        let line = r#"{"jsonrpc":"2.0","id":9,"method":"session/request_permission","params":{"sessionId":"s1","toolCall":{"title":"Run tests"}}}"#;

        let observation = copilot_observation_from_line(line)
            .expect("valid ACP message")
            .expect("permission observation");

        assert_eq!(observation.status.as_deref(), Some("waiting"));
        assert_eq!(observation.step.as_deref(), Some("Waiting for permission"));
    }

    #[test]
    fn permission_response_only_uses_provider_supplied_options() {
        let permission = PendingPermission {
            request_id: "7".to_owned(),
            title: "Run tests".to_owned(),
            options: vec![PermissionOption {
                option_id: "allow_once".to_owned(),
                name: "Allow once".to_owned(),
                kind: "allow_once".to_owned(),
            }],
        };

        assert_eq!(
            permission_outcome(&permission, Some("allow_once")).expect("known choice"),
            serde_json::json!({ "outcome": "selected", "optionId": "allow_once" })
        );
        assert!(permission_outcome(&permission, Some("allow_all")).is_err());
        assert_eq!(
            permission_outcome(&permission, None).expect("cancel choice"),
            serde_json::json!({ "outcome": "cancelled" })
        );
    }

    #[test]
    fn malformed_copilot_output_is_an_error() {
        assert!(copilot_observation_from_line("not-json").is_err());
    }
}
