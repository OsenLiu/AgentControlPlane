use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};
#[cfg(test)]
use serde_json::Value;
use tauri::{Emitter, Manager, State};

struct Database(Arc<Mutex<Connection>>);

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

struct RunningProcess {
    pid: u32,
    command_line: String,
}

#[derive(Debug, Deserialize)]
struct CopilotOpenSessionState {
    working: bool,
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

fn copilot_start_args(task: &str, resume_session_id: Option<&str>) -> Vec<String> {
    let mut args = vec!["--interactive".to_owned(), task.to_owned()];
    if let Some(session_id) = resume_session_id {
        args.push("--resume".to_owned());
        args.push(session_id.to_owned());
    }
    args
}

fn spawn_copilot_terminal(
    project_path: &str,
    args: &[String],
) -> Result<std::process::Child, String> {
    let executable = copilot_executable();
    let terminals = [
        ("gnome-terminal", vec!["--wait".to_owned(), format!("--working-directory={project_path}"), "--".to_owned()]),
        ("konsole", vec!["--hold".to_owned(), "--workdir".to_owned(), project_path.to_owned(), "-e".to_owned()]),
        ("xfce4-terminal", vec!["--working-directory".to_owned(), project_path.to_owned(), "--".to_owned()]),
        ("xterm", vec!["-e".to_owned()]),
    ];

    let mut failures = Vec::new();
    for (terminal, terminal_args) in terminals {
        let mut command = Command::new(terminal);
        command.args(terminal_args).arg(&executable).args(args);
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(error) => failures.push(format!("{terminal}: {error}")),
        }
    }

    Err(format!(
        "Unable to open a terminal for Copilot. Tried: {}",
        failures.join("; ")
    ))
}

fn copilot_session_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("copilot-{timestamp:x}")
}

#[cfg(test)]
fn copilot_content_text(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str().filter(|text| !text.is_empty()) {
        return Some(text.to_owned());
    }
    if let Some(text) = content.get("text").and_then(Value::as_str).filter(|text| !text.is_empty()) {
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

#[cfg(test)]
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

fn tool_settings_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ToolSettings> {
    Ok(ToolSettings {
        enabled: row.get(0)?,
    })
}

fn default_tool_settings() -> ToolSettings {
    ToolSettings {
        enabled: false,
    }
}

fn read_tool_settings(
    connection: &Connection,
    provider: &str,
) -> Result<ToolSettings, String> {
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
            .map_err(|error| format!("Unable to save the GitHub token in the OS credential vault: {error}"))?;
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

fn copilot_external_session_id(arguments: &[String], pid: u32) -> String {
    for (index, argument) in arguments.iter().enumerate() {
        if let Some(value) = argument
            .strip_prefix("--resume=")
            .or_else(|| argument.strip_prefix("--session-id="))
        {
            return value.to_owned();
        }
        if matches!(argument.as_str(), "--resume" | "--session-id") {
            if let Some(value) = arguments.get(index + 1) {
                return value.to_owned();
            }
        }
    }
    format!("pid-{pid}")
}

fn running_processes() -> Result<Vec<RunningProcess>, String> {
    let output = Command::new("ps")
        .args(["-eo", "pid=,args="])
        .output()
        .map_err(|error| format!("Unable to inspect local processes: {error}"))?;
    if !output.status.success() {
        return Err("The operating system rejected process discovery.".to_owned());
    }

    let mut processes = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some((pid_text, command_line)) = line.trim_start().split_once(char::is_whitespace)
        else {
            continue;
        };
        let Ok(pid) = pid_text.parse::<u32>() else {
            continue;
        };
        let command_line = command_line.trim().to_owned();
        if !command_line.to_lowercase().contains("copilot") {
            continue;
        }

        processes.push(RunningProcess {
            pid,
            command_line,
        });
    }
    Ok(processes)
}

fn copilot_session_store_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("COPILOT_HOME") {
        return Some(PathBuf::from(path).join("session-store.db"));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".copilot/session-store.db"))
}

fn transcript_messages_from_store(
    store: &Connection,
    external_session_id: &str,
) -> Result<Vec<TranscriptMessage>, String> {
    let mut statement = store
        .prepare(
            "SELECT turn_index, user_message, assistant_response, timestamp
             FROM turns NOT INDEXED
             WHERE session_id = ?1
             ORDER BY turn_index DESC
             LIMIT 20",
        )
        .map_err(|error| format!("Unable to query Copilot transcript: {error}"))?;
    let mut messages = Vec::new();
    let rows = statement
        .query_map([external_session_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|error| format!("Unable to read Copilot transcript: {error}"))?;

    for row in rows {
        let (_, user_message, assistant_response, timestamp) =
            row.map_err(|error| format!("Unable to decode Copilot transcript: {error}"))?;
        if let Some(content) = user_message.filter(|value| !value.trim().is_empty()) {
            messages.push(TranscriptMessage {
                role: "user".to_owned(),
                content,
                timestamp: timestamp.clone(),
            });
        }
        if let Some(content) = assistant_response.filter(|value| !value.trim().is_empty()) {
            messages.push(TranscriptMessage {
                role: "assistant".to_owned(),
                content,
                timestamp,
            });
        }
        if messages.len() >= 20 {
            break;
        }
    }
    messages.reverse();
    Ok(messages)
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
    let (provider, external_session_id): (String, Option<String>) = connection
        .query_row(
            "SELECT provider, external_session_id FROM agent_sessions WHERE id = ?1",
            [&session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| format!("Unable to find agent session: {error}"))?;

    if provider == "copilot" {
        if let (Some(external_session_id), Some(store_path)) =
            (external_session_id, copilot_session_store_path())
        {
            if store_path.exists() {
                let store = Connection::open_with_flags(
                    store_path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY,
                )
                .map_err(|error| format!("Unable to read Copilot session store: {error}"))?;
                let messages = transcript_messages_from_store(&store, &external_session_id)?;
                if !messages.is_empty() {
                    return Ok(messages);
                }
            }
        }
    }

    let mut statement = connection
        .prepare(
            "SELECT payload, observed_at
             FROM session_events
             WHERE session_id = ?1
             ORDER BY sequence DESC
             LIMIT 20",
        )
        .map_err(|error| format!("Unable to query session transcript: {error}"))?;
    let rows = statement
        .query_map([session_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| format!("Unable to read session transcript: {error}"))?;
    let mut messages = Vec::new();
    for row in rows {
        let (payload, timestamp) =
            row.map_err(|error| format!("Unable to decode session transcript: {error}"))?;
        let observation: CopilotObservation = serde_json::from_str(&payload)
            .map_err(|error| format!("Unable to decode transcript event: {error}"))?;
        if let Some(content) = observation.last_event {
            messages.push(TranscriptMessage {
                role: "assistant".to_owned(),
                content,
                timestamp,
            });
        }
    }
    messages.reverse();
    Ok(messages)
}

fn copilot_open_sessions_state_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("COPILOT_HOME") {
        return Some(PathBuf::from(path).join("open-sessions-state.json"));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".copilot/open-sessions-state.json"))
}

fn copilot_working_session_ids(
) -> Result<Option<std::collections::HashSet<String>>, String> {
    let Some(path) = copilot_open_sessions_state_path() else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(&path)
        .map_err(|error| format!("Unable to read Copilot open-session state: {error}"))?;
    Ok(Some(copilot_working_session_ids_from_json(&contents)?))
}

fn copilot_working_session_ids_from_json(
    contents: &str,
) -> Result<std::collections::HashSet<String>, String> {
    let sessions: HashMap<String, CopilotOpenSessionState> =
        serde_json::from_str(&contents)
            .map_err(|error| format!("Unable to decode Copilot open-session state: {error}"))?;
    Ok(sessions
        .into_iter()
        .filter_map(|(id, state)| state.working.then_some(id))
        .collect())
}

fn active_copilot_session_ids() -> Result<std::collections::HashSet<String>, String> {
    let mut ids = std::collections::HashSet::new();
    for process in running_processes()? {
        if process.command_line.contains("--acp")
            || process.command_line.contains("sessions --")
        {
            continue;
        }
        let arguments = process
            .command_line
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let external_id = copilot_external_session_id(&arguments, process.pid);
        if !external_id.starts_with("pid-") {
            ids.insert(external_id);
        }
    }
    Ok(ids)
}

fn import_copilot_sessions(connection: &Connection) -> Result<(), String> {
    let Some(store_path) = copilot_session_store_path() else {
        return Ok(());
    };
    if !store_path.exists() {
        return Ok(());
    }
    let store = Connection::open_with_flags(&store_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("Unable to read Copilot session store: {error}"))?;
    let working_ids = copilot_working_session_ids()?;
    let fallback_active_ids = if working_ids.is_none() {
        Some(active_copilot_session_ids()?)
    } else {
        None
    };
    let projects = connection
        .prepare("SELECT id, path, branch FROM projects")
        .map_err(|error| format!("Unable to read projects for session discovery: {error}"))?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| format!("Unable to read projects for session discovery: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Unable to decode projects for session discovery: {error}"))?;

    for (project_id, project_path, project_branch) in projects {
        let mut sessions = store
            .prepare(
                "SELECT id, COALESCE(summary, ''), COALESCE(branch, ?2),
                        COALESCE(created_at, ''), COALESCE(updated_at, ''),
                        COALESCE((
                          SELECT SUM(COALESCE(input_tokens, 0) + COALESCE(output_tokens, 0))
                          FROM assistant_usage_events usage
                          WHERE usage.session_id = sessions.id
                        ), 0)
                 FROM sessions
                 WHERE cwd = ?1
                 ORDER BY updated_at DESC",
            )
            .map_err(|error| format!("Unable to query Copilot sessions: {error}"))?;
        let rows = sessions
            .query_map(params![project_path.as_str(), project_branch.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(|error| format!("Unable to decode Copilot sessions: {error}"))?;

        for row in rows {
            let (external_id, summary, branch, created_at, updated_at, token_count) =
                match row {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!("Unable to decode one Copilot session; skipping remaining discovered sessions: {error}");
                        break;
                    }
                };
            let managed: bool = connection
                .query_row(
                    "SELECT EXISTS(
                       SELECT 1 FROM agent_sessions
                       WHERE provider = 'copilot' AND external_session_id = ?1
                         AND origin = 'managed'
                     )",
                    [&external_id],
                    |value| value.get(0),
                )
                .map_err(|error| format!("Unable to inspect Copilot session ownership: {error}"))?;
            if managed {
                continue;
            }
            let active = working_ids
                .as_ref()
                .map(|ids| ids.contains(&external_id))
                .or_else(|| fallback_active_ids.as_ref().map(|ids| ids.contains(&external_id)))
                .unwrap_or(false);
            let last_event = if active {
                "Copilot process is using this session"
            } else {
                "Available to resume in Copilot CLI"
            };
            let session_id = format!("copilot-{external_id}");
            connection
                .execute(
                    "INSERT INTO agent_sessions
                     (id, project_id, provider, external_session_id, task, branch,
                      status, status_reason, current_step, token_count,
                      permission_mode, origin, last_event, started_at, last_seen_at)
                     VALUES (?1, ?2, 'copilot', ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                             'unknown', 'observed', ?10, ?11, ?12)
                     ON CONFLICT(provider, external_session_id) DO UPDATE SET
                       project_id = excluded.project_id,
                       task = excluded.task,
                       branch = excluded.branch,
                       status = excluded.status,
                       status_reason = excluded.status_reason,
                       current_step = excluded.current_step,
                       token_count = excluded.token_count,
                       last_event = excluded.last_event,
                       started_at = excluded.started_at,
                       last_seen_at = excluded.last_seen_at,
                       updated_at = CURRENT_TIMESTAMP
                     WHERE agent_sessions.origin = 'observed'",
                    params![
                        session_id,
                        project_id,
                        external_id,
                        if summary.is_empty() {
                            "(untitled Copilot session)"
                        } else {
                            &summary
                        },
                        if branch.is_empty() {
                            project_branch.as_str()
                        } else {
                            branch.as_str()
                        },
                        if active { "running" } else { "idle" },
                        if active {
                            "Active Copilot process"
                        } else {
                            "Saved Copilot session"
                        },
                        if active { "In use" } else { "Not currently in use" },
                        token_count,
                        last_event,
                        created_at,
                        updated_at,
                    ],
                )
                .map_err(|error| format!("Unable to persist Copilot session: {error}"))?;
        }
    }
    Ok(())
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
    import_copilot_sessions(&connection)?;
    list_agent_sessions_from_connection(&connection, project_id.as_deref(), status.as_deref(), archived)
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

#[tauri::command]
fn start_copilot_session(
    project_id: Option<String>,
    task: String,
    resume_session_id: Option<String>,
    database: State<'_, Database>,
    app: tauri::AppHandle,
) -> Result<AgentSession, String> {
    let mut task = task.trim().to_owned();
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

    let args = copilot_start_args(&task, resume_external_id.as_deref());
    let mut child = spawn_copilot_terminal(&project_path, &args)?;
    let pid = child.id();

    {
        let connection = database
            .0
            .lock()
            .map_err(|_| "Agent session database is unavailable.".to_owned())?;
        if resume_external_id.is_some() {
            connection
                .execute(
                    "UPDATE agent_sessions
                     SET project_id = ?2, task = ?3, branch = ?4, status = 'starting',
                         status_reason = '', current_step = 'Starting Copilot session',
                         origin = 'managed', pid = ?5, archived = 0,
                         updated_at = CURRENT_TIMESTAMP
                     WHERE id = ?1",
                    params![session_id, project_id, task, branch, pid],
                )
        } else {
            connection
                .execute(
                    "INSERT INTO agent_sessions
                     (id, project_id, provider, task, branch, status, status_reason,
                      current_step, permission_mode, origin, pid, last_event)
                     VALUES (?1, ?2, 'copilot', ?3, ?4, 'starting', '',
                             'Starting Copilot session', 'plan', 'managed', ?5, '')",
                    params![session_id, project_id, task, branch, pid],
                )
        }
            .map_err(|error| format!("Unable to persist Copilot session: {error}"))?;
    }

    let database_ref = database.0.clone();
    let completed_session_id = session_id.clone();
    thread::spawn(move || {
        let success = child.wait().map(|status| status.success()).unwrap_or(false);
        let observation = CopilotObservation {
            status: Some(if success { "completed" } else { "failed" }.to_owned()),
            step: Some(if success { "Prompt completed" } else { "Copilot process exited" }.to_owned()),
            last_event: Some(if success {
                "Copilot terminal session completed"
            } else {
                "Copilot terminal session exited unexpectedly"
            }.to_owned()),
        };
        let _ = persist_copilot_observation(&database_ref, &completed_session_id, &observation);
        let _ = app.emit("agent-session-updated", &completed_session_id);
    });

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
         );",
    )?;
    connection
        .execute(
            "ALTER TABLE agent_sessions ADD COLUMN archived INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .or_else(|error| {
            if error.to_string().contains("duplicate column name: archived") {
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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_projects,
            list_agent_sessions,
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

        assert_eq!(observation.last_event.as_deref(), Some("Checking the repository"));
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
    fn malformed_copilot_output_is_an_error() {
        assert!(copilot_observation_from_line("not-json").is_err());
    }

    #[test]
    fn copilot_session_id_prefers_resume_id_over_process_id() {
        let arguments = vec![
            "copilot".to_owned(),
            "--yolo".to_owned(),
            "--resume=abc-123".to_owned(),
        ];

        assert_eq!(
            copilot_external_session_id(&arguments, 42),
            "abc-123"
        );
        assert_eq!(
            copilot_external_session_id(&["copilot".to_owned()], 42),
            "pid-42"
        );
        assert_eq!(
            copilot_external_session_id(
                &["copilot".to_owned(), "--resume".to_owned(), "def-456".to_owned()],
                42
            ),
            "def-456"
        );
    }

    #[test]
    fn copilot_open_session_state_only_marks_working_sessions() {
        let working = copilot_working_session_ids_from_json(
            r#"{
                "working-session": {"schemaVersion": 1, "working": true},
                "idle-session": {"schemaVersion": 1, "working": false}
            }"#,
        )
        .expect("valid open-session state");

        assert!(working.contains("working-session"));
        assert!(!working.contains("idle-session"));
    }

}
