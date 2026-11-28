use crate::{
    AgentSession, Database, PendingPermission, PermissionOption, SessionControl, SessionProcesses,
    SlashCommand, TranscriptActivity, antigravity_command, append_assistant_chunk,
    capture_session_activity, configure_agent_process, copilot_command, fail_project_task,
    handle_project_task_turn, insert_session_message, list_agent_sessions_from_connection,
    project_features::{increment_project_task_step, prepare_task_worktree, project_task_step_usage},
    project_session_auto_approve_permissions, reset_task_after_start_failure,
    terminate_agent_process, update_session_slash_commands, update_session_status,
    update_session_token_count, validate_session_message_status,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Sender},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::Emitter;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AcpProvider {
    Copilot,
    Antigravity,
}

impl AcpProvider {
    pub(crate) fn from_id(provider: &str) -> Option<Self> {
        match provider {
            "copilot" => Some(Self::Copilot),
            "antigravity" => Some(Self::Antigravity),
            _ => None,
        }
    }

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Copilot => "copilot",
            Self::Antigravity => "antigravity",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Copilot => "GitHub Copilot CLI",
            Self::Antigravity => "Antigravity CLI",
        }
    }

    pub(crate) fn short_name(self) -> &'static str {
        match self {
            Self::Copilot => "Copilot",
            Self::Antigravity => "Antigravity",
        }
    }

    pub(crate) fn new_session_id(self) -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{}-{timestamp:x}", self.id())
    }

    pub(crate) fn command(self) -> Result<(Command, PathBuf), String> {
        match self {
            Self::Copilot => copilot_command(),
            Self::Antigravity => antigravity_command(),
        }
    }

    pub(crate) fn acp_args(self, model: &str) -> Vec<String> {
        match self {
            Self::Copilot => vec!["--acp".to_owned(), "--model".to_owned(), model.to_owned()],
            Self::Antigravity => vec!["acp".to_owned()],
        }
    }

    pub(crate) fn check_cli(self) -> Result<(), String> {
        let (mut command, executable) = self.command()?;
        let version = command.arg("--version").output().map_err(|error| {
            format!(
                "Unable to start {} at '{}': {error}",
                self.label(),
                executable.display()
            )
        })?;
        if !version.status.success() {
            let detail = String::from_utf8_lossy(&version.stderr).trim().to_owned();
            return Err(if detail.is_empty() {
                format!("{} rejected the version check.", self.label())
            } else {
                format!("{} rejected the version check: {detail}", self.label())
            });
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct AcpObservation {
    pub(crate) status: Option<String>,
    pub(crate) step: Option<String>,
    pub(crate) last_event: Option<String>,
}

pub(crate) fn acp_content_text(content: &Value) -> Option<String> {
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
            .filter_map(acp_content_text)
            .collect::<String>();
        (!text.is_empty()).then_some(text)
    })
}

pub(crate) fn acp_activity_from_message(
    provider: AcpProvider,
    message: &Value,
) -> Option<TranscriptActivity> {
    let short_name = provider.short_name();
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method == "session/request_permission" {
        let title = message["params"]["toolCall"]["title"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{short_name} requested permission"));
        return Some(TranscriptActivity {
            event_type: "permission_request".to_owned(),
            label: title,
            status: "requested".to_owned(),
            details: message["params"].clone(),
        });
    }
    if let Some(error) = message.get("error") {
        let label = error["message"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{short_name} returned an ACP error"));
        return Some(TranscriptActivity {
            event_type: "error".to_owned(),
            label,
            status: "failed".to_owned(),
            details: error.clone(),
        });
    }
    if method == "session/update" {
        let update = &message["params"]["update"];
        let update_type = update["sessionUpdate"].as_str()?;
        if matches!(
            update_type,
            "agent_message_chunk"
                | "user_message_chunk"
                | "agent_thought_chunk"
                | "usage_update"
                | "current_mode_update"
                | "config_option_update"
                | "available_commands_update"
        ) {
            return None;
        }
        let (event_type, label, fallback_status) = match update_type {
            "tool_call" => (
                "tool_call",
                update["title"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{short_name} tool call")),
                "started",
            ),
            "tool_call_update" => (
                "tool_update",
                update["title"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{short_name} tool update")),
                "updated",
            ),
            "plan" => (
                "plan",
                update["title"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| "Plan updated".to_owned()),
                "updated",
            ),
            _ => (
                "provider_update",
                update["title"].as_str().unwrap_or(update_type).to_owned(),
                "observed",
            ),
        };
        return Some(TranscriptActivity {
            event_type: event_type.to_owned(),
            label,
            status: update["status"]
                .as_str()
                .unwrap_or(fallback_status)
                .to_owned(),
            details: update.clone(),
        });
    }
    None
}

pub(crate) fn acp_observation_from_line(
    provider: AcpProvider,
    line: &str,
    permission_pending: bool,
) -> Result<Option<AcpObservation>, String> {
    let short_name = provider.short_name();
    let message: Value = serde_json::from_str(line)
        .map_err(|error| format!("{short_name} returned invalid ACP JSON: {error}"))?;

    if message.get("error").is_some() {
        return Ok(Some(AcpObservation {
            status: Some("failed".to_owned()),
            step: Some(format!("{short_name} returned an ACP error")),
            last_event: message["error"]["message"].as_str().map(str::to_owned),
        }));
    }

    if let Some(result) = message.get("result") {
        if result.get("stopReason").is_some() {
            return Ok(Some(AcpObservation {
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
            return Ok(Some(AcpObservation {
                status: Some("waiting".to_owned()),
                step: Some("Waiting for permission".to_owned()),
                last_event: Some(format!("{short_name} requested tool permission")),
            }));
        }
        return Ok(None);
    }

    let update = &message["params"]["update"];
    let update_type = update["sessionUpdate"].as_str().unwrap_or_default();
    let mut observation = match update_type {
        "agent_message_chunk" => acp_content_text(&update["content"]).map(|text| AcpObservation {
            status: Some("running".to_owned()),
            step: Some(format!("Receiving {short_name} response")),
            last_event: Some(text),
        }),
        "tool_call" | "tool_call_update" => {
            let title = update["title"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| acp_content_text(&update["content"]))
                .unwrap_or_else(|| format!("{short_name} is using a tool"));
            Some(AcpObservation {
                status: Some("running".to_owned()),
                step: Some(title.clone()),
                last_event: Some(title),
            })
        }
        "user_message_chunk" => Some(AcpObservation {
            status: Some("running".to_owned()),
            step: Some("Processing prompt".to_owned()),
            last_event: acp_content_text(&update["content"]),
        }),
        _ => None,
    };

    if permission_pending {
        if let Some(observation) = observation.as_mut() {
            observation.status = Some("waiting".to_owned());
            observation.step = Some("Waiting for permission".to_owned());
        }
    }

    Ok(observation)
}

pub(crate) fn acp_available_commands_from_message(message: &Value) -> Option<Vec<SlashCommand>> {
    if message.get("method").and_then(Value::as_str) != Some("session/update") {
        return None;
    }
    let update = &message["params"]["update"];
    if update.get("sessionUpdate").and_then(Value::as_str) != Some("available_commands_update") {
        return None;
    }
    let commands = update.get("availableCommands")?.as_array()?;
    let parsed = commands
        .iter()
        .filter_map(|command| {
            let name = command
                .get("name")
                .and_then(Value::as_str)?
                .trim()
                .trim_start_matches('/')
                .to_owned();
            if name.is_empty() {
                return None;
            }
            Some(SlashCommand {
                name,
                description: command
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned),
                input_hint: command
                    .get("input")
                    .and_then(|input| input.get("hint"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned),
            })
        })
        .collect::<Vec<_>>();
    Some(parsed)
}

pub(crate) fn acp_token_count_from_message(
    message: &Value,
    expected_session_id: &str,
) -> Result<Option<(i64, i64)>, String> {
    if message.get("method").and_then(Value::as_str) != Some("session/update") {
        return Ok(None);
    }
    let params = message
        .get("params")
        .ok_or_else(|| "ACP session update is missing params.".to_owned())?;
    let update = params
        .get("update")
        .ok_or_else(|| "ACP session update is missing update.".to_owned())?;
    if update.get("sessionUpdate").and_then(Value::as_str) != Some("usage_update") {
        return Ok(None);
    }
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| "ACP session update is missing sessionId.".to_owned())?;
    if session_id != expected_session_id {
        return Err("ACP session update belongs to a different session.".to_owned());
    }
    let used = update
        .get("used")
        .and_then(Value::as_u64)
        .ok_or_else(|| "ACP usage_update.used must be a nonnegative integer.".to_owned())?;
    let size = update
        .get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| "ACP usage_update.size must be a nonnegative integer.".to_owned())?;
    let used = i64::try_from(used)
        .map_err(|_| "ACP usage_update.used exceeds the supported range.".to_owned())?;
    let size = i64::try_from(size)
        .map_err(|_| "ACP usage_update.size exceeds the supported range.".to_owned())?;
    if used > size {
        return Err("ACP usage_update.used cannot exceed size.".to_owned());
    }
    Ok(Some((used, size)))
}

pub(crate) fn acp_usage_update_for_session(
    database: &Arc<Mutex<Connection>>,
    message: &Value,
    session_id: &str,
) -> Result<Option<(i64, i64)>, String> {
    if message.get("method").and_then(Value::as_str) != Some("session/update")
        || message
            .get("params")
            .and_then(|params| params.get("update"))
            .and_then(|update| update.get("sessionUpdate"))
            .and_then(Value::as_str)
            != Some("usage_update")
    {
        return Ok(None);
    }

    let external_session_id = database
        .lock()
        .map_err(|_| "Agent session database is unavailable.".to_owned())?
        .query_row(
            "SELECT external_session_id
             FROM agent_sessions
             WHERE id = ?1",
            [session_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(|error| format!("Unable to read ACP protocol session ID: {error}"))?
        .flatten()
        .ok_or_else(|| "ACP protocol session ID is not available yet.".to_owned())?;

    acp_token_count_from_message(message, &external_session_id)
}

pub(crate) fn send_acp_message(
    provider: AcpProvider,
    input: &Sender<String>,
    message: Value,
) -> Result<(), String> {
    let short_name = provider.short_name();
    let encoded = serde_json::to_string(&message)
        .map_err(|error| format!("Unable to encode {short_name} protocol message: {error}"))?;
    input
        .send(encoded)
        .map_err(|_| format!("{short_name} session input is no longer available."))
}

pub(crate) fn permission_outcome(
    provider: AcpProvider,
    permission: &PendingPermission,
    option_id: Option<&str>,
) -> Result<Value, String> {
    if let Some(option_id) = option_id {
        if !permission
            .options
            .iter()
            .any(|option| option.option_id == option_id)
        {
            return Err(format!(
                "Choose one of the permission options provided by {}.",
                provider.short_name()
            ));
        }
        Ok(serde_json::json!({ "outcome": "selected", "optionId": option_id }))
    } else {
        Ok(serde_json::json!({ "outcome": "cancelled" }))
    }
}

pub(crate) fn acp_request(
    provider: AcpProvider,
    control: &SessionControl,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let short_name = provider.short_name();
    let id = control.next_request_id.fetch_add(1, Ordering::Relaxed);
    let (response_tx, response_rx) = mpsc::channel();
    control
        .pending
        .lock()
        .map_err(|_| format!("{short_name} protocol state is unavailable."))?
        .insert(id, response_tx);
    if let Err(error) = send_acp_message(
        provider,
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
            return Err(format!("{short_name} did not respond to {method} in time."));
        }
    };
    if let Some(error) = response.get("error") {
        return Err(error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{short_name} rejected the request.")));
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| format!("{short_name} returned an invalid response to {method}."))
}

pub(crate) fn supports_session_resume(init_result: &Value) -> bool {
    match init_result
        .get("agentCapabilities")
        .and_then(|caps| caps.get("sessionCapabilities"))
        .and_then(|session_caps| session_caps.get("resume"))
    {
        Some(Value::Bool(enabled)) => *enabled,
        Some(Value::Object(_)) => true,
        _ => false,
    }
}

pub(crate) fn resolve_acp_model_rpc(
    session_result: &Value,
    external_session_id: &str,
    requested_model: &str,
) -> (&'static str, Value) {
    if let Some(config_id) = session_result
        .get("configOptions")
        .and_then(Value::as_array)
        .and_then(|options| {
            options.iter().find_map(|option| {
                let category = option.get("category").and_then(Value::as_str);
                let id = option.get("id").and_then(Value::as_str)?;
                (category == Some("model") || id == "model").then_some(id)
            })
        })
    {
        return (
            "session/set_config_option",
            serde_json::json!({
                "sessionId": external_session_id,
                "configId": config_id,
                "value": requested_model
            }),
        );
    }

    (
        "session/set_model",
        serde_json::json!({
            "sessionId": external_session_id,
            "modelId": requested_model
        }),
    )
}

pub(crate) fn resolve_acp_plan_mode_rpc(
    session_result: &Value,
    external_session_id: &str,
) -> Option<(&'static str, Value)> {
    let has_plan_mode = session_result
        .get("modes")
        .and_then(|modes| modes.get("availableModes"))
        .and_then(Value::as_array)
        .is_some_and(|modes| {
            modes
                .iter()
                .any(|mode| mode.get("id").and_then(Value::as_str) == Some("plan"))
        });
    if has_plan_mode {
        return Some((
            "session/set_mode",
            serde_json::json!({
                "sessionId": external_session_id,
                "modeId": "plan"
            }),
        ));
    }

    let config_id = session_result
        .get("configOptions")
        .and_then(Value::as_array)
        .and_then(|options| {
            options.iter().find_map(|option| {
                let category = option.get("category").and_then(Value::as_str);
                let id = option.get("id").and_then(Value::as_str)?;
                if category != Some("mode") && id != "mode" {
                    return None;
                }
                let has_plan = option
                    .get("options")
                    .and_then(Value::as_array)
                    .is_some_and(|values| {
                        values.iter().any(|entry| {
                            entry.get("value").and_then(Value::as_str) == Some("plan")
                                || entry.get("id").and_then(Value::as_str) == Some("plan")
                        })
                    });
                has_plan.then_some(id)
            })
        })?;

    Some((
        "session/set_config_option",
        serde_json::json!({
            "sessionId": external_session_id,
            "configId": config_id,
            "value": "plan"
        }),
    ))
}

fn persist_acp_observation(
    provider: AcpProvider,
    database: &Arc<Mutex<Connection>>,
    session_id: &str,
    observation: &AcpObservation,
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
            "SELECT COALESCE(MAX(sequence), 0) + 1
             FROM (
               SELECT sequence FROM session_messages WHERE session_id = ?1
               UNION ALL
               SELECT sequence FROM session_events WHERE session_id = ?1
             )",
            [session_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Unable to sequence agent session event: {error}"))?;
    let payload = serde_json::to_string(observation)
        .map_err(|error| format!("Unable to encode agent session event: {error}"))?;
    let event_type = match provider {
        AcpProvider::Copilot => "copilot_observation",
        AcpProvider::Antigravity => "antigravity_observation",
    };
    connection
        .execute(
            "INSERT INTO session_events
             (id, session_id, sequence, event_type, payload)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                provider.new_session_id(),
                session_id,
                sequence,
                event_type,
                payload
            ],
        )
        .map_err(|error| format!("Unable to persist agent session event: {error}"))?;
    Ok(())
}

fn spawn_acp_process(
    provider: AcpProvider,
    project_path: &str,
    session_id: &str,
    model: &str,
    database: Arc<Mutex<Connection>>,
    processes: Arc<Mutex<HashMap<String, SessionControl>>>,
    app: tauri::AppHandle,
) -> Result<SessionControl, String> {
    let short_name = provider.short_name();
    let (mut command, executable) = provider.command()?;
    configure_agent_process(&mut command);
    for arg in provider.acp_args(model) {
        command.arg(arg);
    }
    let mut child = command
        .current_dir(project_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!(
                "Unable to start {short_name} ACP at '{}': {error}",
                executable.display()
            )
        })?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("{short_name} did not provide an input stream."))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{short_name} did not provide an output stream."))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{short_name} did not provide an error stream."))?;
    let child = Arc::new(Mutex::new(child));
    let (input, input_rx) = mpsc::channel::<String>();
    let control = SessionControl {
        provider: provider.id().to_owned(),
        input,
        pending: Arc::new(Mutex::new(HashMap::new())),
        permission_requests: Arc::new(Mutex::new(HashMap::new())),
        next_request_id: Arc::new(AtomicU64::new(1)),
        stop_requested: Arc::new(AtomicBool::new(false)),
        replaying_history: Arc::new(AtomicBool::new(false)),
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
    let stderr_tail = Arc::new(Mutex::new(Vec::<String>::new()));
    let stderr_writer = stderr_tail.clone();
    let stderr_thread = thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else {
                break;
            };
            if let Ok(mut tail) = stderr_writer.lock() {
                tail.push(line);
                if tail.len() > 20 {
                    tail.remove(0);
                }
            }
        }
    });

    let reader_control = control.clone();
    let reader_session_id = session_id.to_owned();
    thread::spawn(move || {
        let mut reported_thinking = false;
        for line in BufReader::new(stdout).lines() {
            let line = match line {
                Ok(line) => line,
                Err(error) => {
                    let message = format!("Unable to read {short_name} ACP output: {error}");
                    capture_session_activity(
                        &database,
                        &app,
                        &reader_session_id,
                        &TranscriptActivity {
                            event_type: "error".to_owned(),
                            label: message.clone(),
                            status: "failed".to_owned(),
                            details: Value::Null,
                        },
                    );
                    let _ = update_session_status(
                        &database,
                        &reader_session_id,
                        "failed",
                        &format!("Unable to read {short_name} protocol output"),
                        &message,
                    );
                    break;
                }
            };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                capture_session_activity(
                    &database,
                    &app,
                    &reader_session_id,
                    &TranscriptActivity {
                        event_type: "error".to_owned(),
                        label: format!("{short_name} returned invalid ACP JSON"),
                        status: "failed".to_owned(),
                        details: Value::String(line.clone()),
                    },
                );
                let _ = update_session_status(
                    &database,
                    &reader_session_id,
                    "failed",
                    &format!("Invalid {short_name} protocol output"),
                    &format!("{short_name} returned invalid ACP JSON."),
                );
                let _ = app.emit("agent-session-updated", &reader_session_id);
                continue;
            };

            if let Some(id) = message.get("id").and_then(Value::as_u64) {
                if message.get("method").is_none() {
                    if let Ok(mut pending) = reader_control.pending.lock() {
                        if let Some(sender) = pending.remove(&id) {
                            let _ = sender.send(message);
                        }
                    }
                    continue;
                }
            }

            if reader_control.replaying_history.load(Ordering::Relaxed)
                && message.get("method").and_then(Value::as_str) == Some("session/update")
            {
                continue;
            }

            if let Some(activity) = acp_activity_from_message(provider, &message) {
                capture_session_activity(&database, &app, &reader_session_id, &activity);
            }

            if let Some(commands) = acp_available_commands_from_message(&message) {
                update_session_slash_commands(&app, &reader_session_id, commands);
            }

            match acp_usage_update_for_session(&database, &message, &reader_session_id) {
                Ok(Some((token_count, context_window_size))) => {
                    match update_session_token_count(
                        &database,
                        &reader_session_id,
                        token_count,
                        Some(context_window_size),
                    ) {
                        Ok(()) => {
                            let _ = app.emit("agent-session-updated", &reader_session_id);
                        }
                        Err(error) => {
                            eprintln!("{error}");
                            let _ = app.emit("agent-session-usage-error", error);
                        }
                    }
                }
                Err(error) => {
                    eprintln!("{error}");
                    let _ = app.emit("agent-session-usage-error", error);
                }
                Ok(None) => {}
            }

            if message.get("method").and_then(Value::as_str) == Some("session/request_permission") {
                reported_thinking = false;
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
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("{short_name} requested tool permission")),
                    options,
                };
                let tool_kind = message["params"]["toolCall"]["kind"]
                    .as_str()
                    .unwrap_or_default();
                let auto_option = match project_session_auto_approve_permissions(
                    &database,
                    &reader_session_id,
                    tool_kind,
                ) {
                    Ok(true) => permission
                        .options
                        .iter()
                        .find(|option| option.kind == "allow_once")
                        .map(|option| option.option_id.clone()),
                    Ok(false) => None,
                    Err(error) => {
                        let _ = app.emit("project-loop-error", error);
                        None
                    }
                };
                if let Some(option_id) = auto_option {
                    match permission_outcome(provider, &permission, Some(&option_id)) {
                        Ok(outcome) => match send_acp_message(
                            provider,
                            &reader_control.input,
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": message["id"],
                                "result": { "outcome": outcome }
                            }),
                        ) {
                            Ok(()) => {
                                capture_session_activity(
                                    &database,
                                    &app,
                                    &reader_session_id,
                                    &TranscriptActivity {
                                        event_type: "permission_decision".to_owned(),
                                        label: format!("Allowed once: {}", permission.title),
                                        status: "allowed".to_owned(),
                                        details: serde_json::json!({
                                            "optionId": option_id,
                                            "automatic": true
                                        }),
                                    },
                                );
                                let observation = AcpObservation {
                                    status: Some("running".to_owned()),
                                    step: Some("Looping task permission approved once".to_owned()),
                                    last_event: Some(format!("Allowed once: {}", permission.title)),
                                };
                                let _ = persist_acp_observation(
                                    provider,
                                    &database,
                                    &reader_session_id,
                                    &observation,
                                );
                                let _ = app.emit("agent-session-updated", &reader_session_id);
                                continue;
                            }
                            Err(error) => {
                                let _ = update_session_status(
                                    &database,
                                    &reader_session_id,
                                    "failed",
                                    "Unable to approve loop permission",
                                    &error,
                                );
                                let _ = app.emit("project-loop-error", error);
                                let _ = app.emit("agent-session-updated", &reader_session_id);
                                continue;
                            }
                        },
                        Err(error) => {
                            let _ = app.emit("project-loop-error", error);
                        }
                    }
                }
                if let Ok(mut requests) = reader_control.permission_requests.lock() {
                    requests.insert(request_id, (message["id"].clone(), permission.clone()));
                }
                let observation = AcpObservation {
                    status: Some("waiting".to_owned()),
                    step: Some("Waiting for permission".to_owned()),
                    last_event: Some(permission.title.clone()),
                };
                let _ = persist_acp_observation(provider, &database, &reader_session_id, &observation);
                if permission.options.is_empty() {
                    let _ = send_acp_message(
                        provider,
                        &reader_control.input,
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": message["id"],
                            "result": { "outcome": { "outcome": "cancelled" } }
                        }),
                    );
                    capture_session_activity(
                        &database,
                        &app,
                        &reader_session_id,
                        &TranscriptActivity {
                            event_type: "permission_decision".to_owned(),
                            label: format!("Cancelled: {}", permission.title),
                            status: "cancelled".to_owned(),
                            details: serde_json::json!({ "automatic": true }),
                        },
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

            let permission_pending = reader_control
                .permission_requests
                .lock()
                .ok()
                .is_some_and(|requests| !requests.is_empty());

            if message.get("method").and_then(Value::as_str) == Some("session/update")
                && message["params"]["update"]["sessionUpdate"].as_str()
                    == Some("agent_thought_chunk")
            {
                if !permission_pending && !reported_thinking {
                    reported_thinking = true;
                    let _ = update_session_status(
                        &database,
                        &reader_session_id,
                        "running",
                        &format!("{short_name} is thinking"),
                        "",
                    );
                    let _ = app.emit("agent-session-updated", &reader_session_id);
                }
                continue;
            }

            match acp_observation_from_line(provider, &line, permission_pending) {
                Ok(Some(observation)) => {
                    reported_thinking = false;
                    if message["params"]["update"]["sessionUpdate"].as_str()
                        == Some("agent_message_chunk")
                    {
                        if let Some(content) =
                            acp_content_text(&message["params"]["update"]["content"])
                        {
                            if let Ok(message_id) = reader_control.assistant_message_id.lock() {
                                if let Some(message_id) = message_id.as_deref() {
                                    if let Err(error) = append_assistant_chunk(
                                        &database,
                                        &reader_session_id,
                                        message_id,
                                        &content,
                                    ) {
                                        let _ = app.emit(
                                            "agent-session-activity-error",
                                            serde_json::json!({
                                                "sessionId": reader_session_id,
                                                "error": error
                                            }),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    let _ = persist_acp_observation(
                        provider,
                        &database,
                        &reader_session_id,
                        &observation,
                    );
                    let _ = app.emit("agent-session-updated", &reader_session_id);
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = update_session_status(
                        &database,
                        &reader_session_id,
                        "failed",
                        &format!("{short_name} protocol error"),
                        &error,
                    );
                    let _ = app.emit("agent-session-updated", &reader_session_id);
                }
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
        let _ = stderr_thread.join();
        let stderr_text = stderr_tail
            .lock()
            .map(|tail| tail.join("\n"))
            .unwrap_or_default();
        let exit_message = if stderr_text.trim().is_empty() {
            format!("{short_name} ACP process exited before responding.")
        } else {
            format!("{short_name} ACP process exited before responding: {stderr_text}")
        };
        if let Ok(mut pending) = reader_control.pending.lock() {
            for (_, sender) in pending.drain() {
                let _ = sender.send(serde_json::json!({
                    "error": { "message": exit_message }
                }));
            }
        }
        let stopped = reader_control.stop_requested.load(Ordering::Relaxed);
        let status = if success || stopped {
            "completed"
        } else {
            "failed"
        };
        let reason = if stopped {
            "Stopped by user.".to_owned()
        } else if success {
            format!("{short_name} ACP session ended.")
        } else {
            format!("{short_name} ACP process exited unexpectedly.")
        };
        let _ = update_session_status(&database, &reader_session_id, status, &reason, &reason);
        capture_session_activity(
            &database,
            &app,
            &reader_session_id,
            &TranscriptActivity {
                event_type: "process_exit".to_owned(),
                label: format!("{short_name} ACP process exited"),
                status: if stopped {
                    "stopped".to_owned()
                } else if success {
                    "completed".to_owned()
                } else {
                    "failed".to_owned()
                },
                details: serde_json::json!({
                    "success": success,
                    "stopped": stopped,
                    "stderr": stderr_text
                }),
            },
        );
        if let Ok(mut active) = processes.lock() {
            active.remove(&reader_session_id);
        }
        if !success || stopped {
            fail_project_task(&database, &reader_session_id, &app);
        }
        let _ = app.emit("agent-session-updated", &reader_session_id);
    });

    Ok(control)
}

pub(crate) fn send_acp_session_message(
    provider: AcpProvider,
    database: &Arc<Mutex<Connection>>,
    session_id: &str,
    control: &SessionControl,
    message: &str,
    app: &tauri::AppHandle,
    processes: &SessionProcesses,
    is_initial_prompt: bool,
) -> Result<(), String> {
    let short_name = provider.short_name();
    let message = message.trim();
    if message.is_empty() {
        return Err(format!("Enter a message before sending it to {short_name}."));
    }
    if message.len() > 16_000 {
        return Err("Messages must be 16,000 bytes or fewer.".to_owned());
    }
    let has_pending_permission = !control
        .permission_requests
        .lock()
        .map_err(|_| format!("{short_name} permission state is unavailable."))?
        .is_empty();
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
        validate_session_message_status(&status, is_initial_prompt, has_pending_permission)?;
        if let Some((steps_used, step_budget)) = project_task_step_usage(&connection, session_id)?
            && steps_used >= step_budget
        {
            return Err(format!(
                "This task has reached its step budget of {step_budget}. Edit the task to increase the budget before continuing."
            ));
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
        .map_err(|_| format!("{short_name} transcript state is unavailable."))? =
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
        .map_err(|error| format!("Unable to read {short_name} protocol session ID: {error}"))?;
    let result = acp_request(
        provider,
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
        .map_err(|_| format!("{short_name} transcript state is unavailable."))? = None;
    match result {
        Ok(_) => {
            let connection = database
                .lock()
                .map_err(|_| "Agent session database is unavailable.".to_owned())?;
            increment_project_task_step(&connection, session_id)?;
            drop(connection);
            update_session_status(database, session_id, "idle", "Ready for your message", "")?;
            handle_project_task_turn(database, session_id, processes, app);
            app.emit("agent-session-updated", session_id)
                .map_err(|error| format!("Unable to publish agent session update: {error}"))?;
            Ok(())
        }
        Err(error) => {
            update_session_status(
                database,
                session_id,
                "failed",
                &format!("{short_name} request failed"),
                &error,
            )?;
            fail_project_task(database, session_id, app);
            let _ = app.emit("agent-session-updated", session_id);
            Err(error)
        }
    }
}

pub(crate) fn start_acp_session_blocking(
    provider: AcpProvider,
    project_id: Option<String>,
    task: String,
    resume_session_id: Option<String>,
    task_id: Option<String>,
    loop_owned: bool,
    database: Database,
    processes: SessionProcesses,
    app: tauri::AppHandle,
) -> Result<AgentSession, String> {
    let provider_id = provider.id();
    let short_name = provider.short_name();
    let mut task = task.trim().to_owned();
    if let Some(resume_session_id) = resume_session_id.as_deref() {
        if processes
            .0
            .lock()
            .map_err(|_| "Agent session process list is unavailable.".to_owned())?
            .contains_key(resume_session_id)
        {
            return Err(format!("This {short_name} session is already connected."));
        }
    }
    let (
        project_id,
        mut project_path,
        mut branch,
        session_id,
        resume_external_id,
        allow_agent_tasks,
        mut worktree_path,
    ) = {
        let connection = database
            .0
            .lock()
            .map_err(|_| "Project database is unavailable.".to_owned())?;
        if let Some(session_id) = resume_session_id.as_deref() {
            let (project_id, external_id, stored_task, stored_branch, stored_worktree_path): (
                String,
                Option<String>,
                String,
                String,
                Option<String>,
            ) = connection
                .query_row(
                    "SELECT project_id, external_session_id, task, branch, worktree_path
                     FROM agent_sessions
                     WHERE id = ?1 AND provider = ?2",
                    params![session_id, provider_id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .map_err(|error| format!("Unable to find {short_name} session to resume: {error}"))?;
            let external_id = external_id.ok_or_else(|| {
                format!("{short_name} did not return a session ID, so this session cannot be resumed.")
            })?;
            if task.is_empty() {
                task = stored_task;
            }
            let (project_path, project_branch) = connection
                .query_row(
                    "SELECT path, branch FROM projects WHERE id = ?1",
                    [&project_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .map_err(|error| {
                    format!("Unable to find project for {short_name} session: {error}")
                })?;
            let path = stored_worktree_path.clone().unwrap_or(project_path);
            let branch = if stored_branch.is_empty() {
                project_branch
            } else {
                stored_branch
            };
            (
                project_id,
                path,
                branch,
                session_id.to_owned(),
                Some(external_id),
                false,
                stored_worktree_path,
            )
        } else {
            let project_id = project_id
                .ok_or_else(|| format!("A project is required to start a {short_name} session."))?;
            if task.is_empty() {
                return Err(format!(
                    "Describe the task before starting a {short_name} session."
                ));
            }
            let (path, branch, allow_agent_tasks) = connection
                .query_row(
                    "SELECT path, branch, agent_task_creation FROM projects WHERE id = ?1",
                    [&project_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)? != 0,
                        ))
                    },
                )
                .map_err(|error| {
                    format!("Unable to find project for {short_name} session: {error}")
                })?;
            (
                project_id,
                path,
                branch,
                provider.new_session_id(),
                None,
                allow_agent_tasks,
                None,
            )
        }
    };
    let (permission_mode, model, use_worktree, worktree_task_id) = {
        let connection = database
            .0
            .lock()
            .map_err(|_| "Project database is unavailable.".to_owned())?;
        if let Some(task_id) = task_id.as_deref() {
            let (task_provider, mode, model, use_worktree): (String, String, String, bool) =
                connection
                    .query_row(
                        "SELECT provider, mode, model, use_worktree FROM project_tasks WHERE id = ?1 AND project_id = ?2",
                        params![task_id, project_id],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get::<_, i64>(3)? != 0,
                            ))
                        },
                    )
                    .map_err(|error| format!("Unable to read project task settings: {error}"))?;
            if task_provider != provider_id {
                return Err("The task is assigned to a different agent.".to_owned());
            }
            (mode, model, use_worktree, Some(task_id.to_owned()))
        } else if let Some(resume_session_id) = resume_session_id.as_deref() {
            connection
                .query_row(
                    "SELECT COALESCE(t.mode, s.permission_mode), COALESCE(t.model, 'auto')
                     FROM agent_sessions s
                     LEFT JOIN project_tasks t ON t.session_id = s.id
                     WHERE s.id = ?1",
                    [resume_session_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .map(|(mode, model)| (mode, model, false, None))
                .map_err(|error| format!("Unable to read resumed session settings: {error}"))?
        } else {
            ("plan".to_owned(), "auto".to_owned(), false, None)
        }
    };

    if allow_agent_tasks && resume_session_id.is_none() {
        task.push_str(
            "\n\n# Agent-proposed backlog tasks\nThis project allows you to propose separate follow-up work. When useful, add a fenced `apt-task` JSON block with a short `title`, a `purpose`, and optional `acceptanceCriteria` string array. Example:\n```apt-task\n{\"title\":\"Add example\",\"purpose\":\"Describe the follow-up work\",\"acceptanceCriteria\":[\"A verifiable outcome\"]}\n```\nProposed tasks are added to the backlog and do not interrupt this session. Do not propose work that is part of the task you were asked to complete.",
        );
    }
    if task_id.is_some() && resume_session_id.is_none() {
        task.push_str(
            "\n\n# Task completion protocol\nEnd your response with one fenced apt-status JSON block. Use state `working` with a brief note when unfinished, `question` when you need a human decision, `blocked` when you cannot proceed, or `complete` when the task is done. A `complete` report must include `gatePassed: true` and a brief `gateSummary` only after all available relevant checks pass. Example: ```apt-status\n{\"state\":\"complete\",\"note\":\"Implemented and verified the task\",\"gatePassed\":true,\"gateSummary\":\"Tests passed\"}\n``` Do not report complete when checks fail or were not run.",
        );
    }

    provider.check_cli()?;

    if use_worktree {
        let task_id = worktree_task_id
            .as_deref()
            .ok_or_else(|| "Unable to find the task for its Git worktree.".to_owned())?;
        let (resolved_worktree_path, worktree_branch) =
            prepare_task_worktree(Path::new(&project_path), task_id)?;
        project_path = resolved_worktree_path.to_string_lossy().into_owned();
        branch = worktree_branch;
        worktree_path = Some(project_path.clone());
    }

    let starting_step = format!("Starting {short_name} session");
    {
        let mut connection = database
            .0
            .lock()
            .map_err(|_| "Agent session database is unavailable.".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Unable to start session transaction: {error}"))?;
        if resume_external_id.is_some() {
            transaction.execute(
                "UPDATE agent_sessions
                     SET project_id = ?2, task = ?3, branch = ?4,
                         worktree_path = COALESCE(worktree_path, ?6), status = 'starting',
                         status_reason = '', current_step = ?7,
                         permission_mode = ?5, origin = 'managed', archived = 0,
                         updated_at = CURRENT_TIMESTAMP
                     WHERE id = ?1",
                params![
                    session_id,
                    project_id,
                    task,
                    branch,
                    permission_mode,
                    worktree_path,
                    starting_step
                ],
            )
        } else {
            transaction.execute(
                "INSERT INTO agent_sessions
                     (id, project_id, provider, task, branch, worktree_path, status, status_reason,
                      current_step, permission_mode, origin, loop_owned, last_event)
                     VALUES (?1, ?2, ?8, ?3, ?4, ?5, 'starting', '',
                             ?9, ?6, 'managed', ?7, '')",
                params![
                    session_id,
                    project_id,
                    task,
                    branch,
                    worktree_path,
                    permission_mode,
                    loop_owned,
                    provider_id,
                    starting_step
                ],
            )
        }
        .map_err(|error| format!("Unable to persist {short_name} session: {error}"))?;
        if let Some(task_id) = task_id.as_deref() {
            let changed = transaction
                .execute(
                    "UPDATE project_tasks SET status = 'in_progress', session_id = ?1,
                     workspace_locked = 1,
                     updated_at = CURRENT_TIMESTAMP
                     WHERE id = ?2 AND project_id = ?3 AND provider = ?5 AND
                      ((status = 'backlog' AND ?4 = 0) OR
                       (status = 'in_progress' AND session_id IS NULL AND ?4 = 1))",
                    params![session_id, task_id, project_id, loop_owned, provider_id],
                )
                .map_err(|error| format!("Unable to link task to {short_name} session: {error}"))?;
            if changed != 1 {
                return Err("The task is no longer available to start.".to_owned());
            }
        }
        transaction
            .commit()
            .map_err(|error| format!("Unable to save {short_name} task session: {error}"))?;
    }

    let processes_ref = processes.0.clone();
    let control = match spawn_acp_process(
        provider,
        &project_path,
        &session_id,
        &model,
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
                &format!("Unable to start {short_name} ACP"),
                &error,
            )?;
            reset_task_after_start_failure(&database, task_id.as_deref())
                .map_err(|cleanup_error| format!("{error}; additionally, {cleanup_error}"))?;
            return Err(error);
        }
    };
    let initialization = match acp_request(
        provider,
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
    ) {
        Ok(result) => result,
        Err(error) => {
            let _ = control
                .child
                .lock()
                .map(|mut process| terminate_agent_process(&mut process));
            update_session_status(
                &database.0,
                &session_id,
                "failed",
                &format!("{short_name} initialization failed"),
                &error,
            )?;
            reset_task_after_start_failure(&database, task_id.as_deref())
                .map_err(|cleanup_error| format!("{error}; additionally, {cleanup_error}"))?;
            return Err(error);
        }
    };

    let session_result = if let Some(external_id) = resume_external_id.as_deref() {
        let resume_method = if provider == AcpProvider::Antigravity
            && supports_session_resume(&initialization)
        {
            "session/resume"
        } else {
            "session/load"
        };
        if resume_method == "session/load" {
            control.replaying_history.store(true, Ordering::Relaxed);
        }
        let result = acp_request(
            provider,
            &control,
            resume_method,
            serde_json::json!({
                "cwd": project_path,
                "sessionId": external_id,
                "mcpServers": []
            }),
        );
        control.replaying_history.store(false, Ordering::Relaxed);
        result
    } else {
        acp_request(
            provider,
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
            let _ = control
                .child
                .lock()
                .map(|mut process| terminate_agent_process(&mut process));
            update_session_status(
                &database.0,
                &session_id,
                "failed",
                &format!("Unable to load {short_name} session"),
                &error,
            )?;
            reset_task_after_start_failure(&database, task_id.as_deref())
                .map_err(|cleanup_error| format!("{error}; additionally, {cleanup_error}"))?;
            return Err(error);
        }
    };
    let external_id = session_result
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or(resume_external_id);
    let Some(external_id) = external_id else {
        let error = format!("{short_name} did not return a session identifier.");
        let _ = control
            .child
            .lock()
            .map(|mut process| terminate_agent_process(&mut process));
        update_session_status(
            &database.0,
            &session_id,
            "failed",
            &format!("Invalid {short_name} session response"),
            &error,
        )?;
        reset_task_after_start_failure(&database, task_id.as_deref())
            .map_err(|cleanup_error| format!("{error}; additionally, {cleanup_error}"))?;
        return Err(error);
    };

    if provider == AcpProvider::Antigravity {
        if model != "auto" {
            let (method, params) = resolve_acp_model_rpc(&session_result, &external_id, &model);
            if let Err(error) = acp_request(provider, &control, method, params) {
                let _ = control
                    .child
                    .lock()
                    .map(|mut process| terminate_agent_process(&mut process));
                update_session_status(
                    &database.0,
                    &session_id,
                    "failed",
                    "Unable to configure Antigravity model",
                    &error,
                )?;
                reset_task_after_start_failure(&database, task_id.as_deref())
                    .map_err(|cleanup_error| format!("{error}; additionally, {cleanup_error}"))?;
                return Err(error);
            }
        }
        if permission_mode == "plan" {
            if let Some((method, params)) = resolve_acp_plan_mode_rpc(&session_result, &external_id)
            {
                if let Err(error) = acp_request(provider, &control, method, params) {
                    let _ = control
                        .child
                        .lock()
                        .map(|mut process| terminate_agent_process(&mut process));
                    update_session_status(
                        &database.0,
                        &session_id,
                        "failed",
                        "Unable to configure Antigravity plan mode",
                        &error,
                    )?;
                    reset_task_after_start_failure(&database, task_id.as_deref())
                        .map_err(|cleanup_error| format!("{error}; additionally, {cleanup_error}"))?;
                    return Err(error);
                }
            }
        }
    }

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
                        .map_err(|_| format!("{short_name} process state is unavailable."))?
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
            .map_err(|error| format!("Unable to save {short_name} protocol session ID: {error}"))?;
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
        let initial_processes = SessionProcesses(processes_ref.clone());
        thread::spawn(move || {
            if let Err(error) = send_acp_session_message(
                provider,
                &database_ref,
                &initial_session_id,
                &initial_control,
                &initial_task,
                &initial_app,
                &initial_processes,
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
        .ok_or_else(|| format!("{short_name} session was started but could not be read back."))
}

struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const MAX_DRAFT_RESPONSE_BYTES: usize = 256 * 1024;

pub(crate) fn run_antigravity_draft_prompt(prompt: &str) -> Result<String, String> {
    let temp_dir = std::env::temp_dir().join(format!(
        "acp-antigravity-draft-{}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::create_dir_all(&temp_dir)
        .map_err(|error| format!("Unable to prepare isolated workspace for Antigravity: {error}"))?;
    let _guard = TempDirGuard(temp_dir.clone());

    let (mut command, executable) = antigravity_command()?;
    configure_agent_process(&mut command);
    let mut child = command
        .arg("acp")
        .arg("--sandbox")
        .current_dir(&temp_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!(
                "Unable to start Antigravity CLI at '{}': {error}",
                executable.display()
            )
        })?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Antigravity did not provide an input stream.".to_owned())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Antigravity did not provide an output stream.".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Antigravity did not provide an error stream.".to_owned())?;

    let (input_tx, input_rx) = mpsc::channel::<String>();
    let pending: Arc<Mutex<HashMap<u64, Sender<Value>>>> = Arc::new(Mutex::new(HashMap::new()));
    let collected = Arc::new(Mutex::new(String::new()));
    let next_id = AtomicU64::new(1);

    let writer_thread = thread::spawn(move || {
        let mut writer = std::io::BufWriter::new(stdin);
        for line in input_rx {
            if writeln!(writer, "{line}").and_then(|_| writer.flush()).is_err() {
                break;
            }
        }
    });
    let stderr_thread = thread::spawn(move || {
        let mut lines = Vec::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            lines.push(line);
            if lines.len() > 20 {
                lines.remove(0);
            }
        }
        lines.join("\n")
    });

    let reader_pending = pending.clone();
    let reader_collected = collected.clone();
    let reader_input = input_tx.clone();
    let reader_thread = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message.get("method").and_then(Value::as_str) == Some("session/request_permission") {
                let _ = send_acp_message(
                    AcpProvider::Antigravity,
                    &reader_input,
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": message["id"],
                        "result": { "outcome": { "outcome": "cancelled" } }
                    }),
                );
                continue;
            }
            if let Some(id) = message.get("id").and_then(Value::as_u64) {
                if message.get("method").is_none() {
                    if let Ok(mut map) = reader_pending.lock() {
                        if let Some(tx) = map.remove(&id) {
                            let _ = tx.send(message);
                        }
                    }
                    continue;
                }
            }
            if message.get("method").and_then(Value::as_str) == Some("session/update")
                && message["params"]["update"]["sessionUpdate"].as_str()
                    == Some("agent_message_chunk")
            {
                if let Some(text) = acp_content_text(&message["params"]["update"]["content"]) {
                    if let Ok(mut buffer) = reader_collected.lock() {
                        if buffer.len() < MAX_DRAFT_RESPONSE_BYTES {
                            let remaining = MAX_DRAFT_RESPONSE_BYTES - buffer.len();
                            let mut end = text.len().min(remaining);
                            while !text.is_char_boundary(end) {
                                end -= 1;
                            }
                            buffer.push_str(&text[..end]);
                        }
                    }
                }
            }
        }
        if let Ok(mut map) = reader_pending.lock() {
            for (_, tx) in map.drain() {
                let _ = tx.send(serde_json::json!({
                    "error": { "message": "Antigravity ACP process exited before completing the draft." }
                }));
            }
        }
    });

    let rpc = |method: &str, params: Value| -> Result<Value, String> {
        let id = next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        pending
            .lock()
            .map_err(|_| "Antigravity protocol state is unavailable.".to_owned())?
            .insert(id, tx);
        if let Err(error) = send_acp_message(
            AcpProvider::Antigravity,
            &input_tx,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params
            }),
        ) {
            if let Ok(mut map) = pending.lock() {
                map.remove(&id);
            }
            return Err(error);
        }
        let response = match rx.recv_timeout(Duration::from_secs(120)) {
            Ok(response) => response,
            Err(_) => {
                if let Ok(mut map) = pending.lock() {
                    map.remove(&id);
                }
                return Err(format!("Antigravity did not respond to {method} in time."));
            }
        };
        if let Some(error) = response.get("error") {
            return Err(error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Antigravity rejected the task-drafting request.")
                .to_owned());
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| format!("Antigravity returned an invalid response to {method}."))
    };

    let run_result = (|| -> Result<String, String> {
        let _ = rpc(
            "initialize",
            serde_json::json!({
                "protocolVersion": 1,
                "clientCapabilities": {},
                "clientInfo": {
                    "name": "Agent Control Plane",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )?;
        let session = rpc(
            "session/new",
            serde_json::json!({
                "cwd": temp_dir.to_string_lossy(),
                "mcpServers": []
            }),
        )?;
        let external_id = session
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| "Antigravity did not return a session identifier.".to_owned())?;
        if let Some((method, params)) = resolve_acp_plan_mode_rpc(&session, external_id) {
            let _ = rpc(method, params);
        }
        let _ = rpc(
            "session/prompt",
            serde_json::json!({
                "sessionId": external_id,
                "prompt": [{ "type": "text", "text": prompt }]
            }),
        )?;
        let text = collected
            .lock()
            .map_err(|_| "Antigravity draft output is unavailable.".to_owned())?
            .clone();
        Ok(text)
    })();

    drop(input_tx);
    let _ = terminate_agent_process(&mut child);
    let _ = child.wait();
    let _ = reader_thread.join();
    let _ = writer_thread.join();
    let stderr_output = stderr_thread.join().unwrap_or_default();

    run_result.map_err(|error| {
        if stderr_output.trim().is_empty() {
            error
        } else {
            format!("{error} ({})", stderr_output.trim())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antigravity_thought_and_metadata_updates_do_not_spam_activity_or_observations() {
        for update_type in [
            "agent_thought_chunk",
            "usage_update",
            "current_mode_update",
            "config_option_update",
            "available_commands_update",
        ] {
            let message = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "agy-1",
                    "update": {
                        "sessionUpdate": update_type,
                        "content": { "type": "text", "text": "Reasoning..." }
                    }
                }
            });
            assert!(
                acp_activity_from_message(AcpProvider::Antigravity, &message).is_none(),
                "unexpected activity for {update_type}"
            );
            assert_eq!(
                acp_observation_from_line(AcpProvider::Antigravity, &message.to_string(), false),
                Ok(None),
                "unexpected observation for {update_type}"
            );
        }
    }

    #[test]
    fn antigravity_agent_message_and_plan_updates_are_mapped() {
        let chunk = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"agy-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Running tests"}}}}"#;
        let observation = acp_observation_from_line(AcpProvider::Antigravity, chunk, false)
            .expect("valid message")
            .expect("observation");
        assert_eq!(
            observation,
            AcpObservation {
                status: Some("running".to_owned()),
                step: Some("Receiving Antigravity response".to_owned()),
                last_event: Some("Running tests".to_owned()),
            }
        );

        let plan = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "agy-1",
                "update": {
                    "sessionUpdate": "plan",
                    "title": "Implementation steps",
                    "entries": []
                }
            }
        });
        let activity = acp_activity_from_message(AcpProvider::Antigravity, &plan)
            .expect("plan activity");
        assert_eq!(activity.event_type, "plan");
        assert_eq!(activity.label, "Implementation steps");
        assert_eq!(activity.status, "updated");
    }

    #[test]
    fn resolves_model_rpc_for_config_options_and_set_model_fallback() {
        let with_config = serde_json::json!({
            "sessionId": "agy-1",
            "configOptions": [
                { "id": "model", "category": "model", "currentValue": "gemini-4-hydrogen" }
            ]
        });
        assert_eq!(
            resolve_acp_model_rpc(&with_config, "agy-1", "gemini-4-carbon"),
            (
                "session/set_config_option",
                serde_json::json!({
                    "sessionId": "agy-1",
                    "configId": "model",
                    "value": "gemini-4-carbon"
                })
            )
        );

        let without_config = serde_json::json!({ "sessionId": "agy-1" });
        assert_eq!(
            resolve_acp_model_rpc(&without_config, "agy-1", "gemini-4-neon"),
            (
                "session/set_model",
                serde_json::json!({
                    "sessionId": "agy-1",
                    "modelId": "gemini-4-neon"
                })
            )
        );
    }

    #[test]
    fn resolves_plan_mode_rpc_and_resume_capability() {
        let init_with_resume = serde_json::json!({
            "agentCapabilities": {
                "loadSession": true,
                "sessionCapabilities": { "resume": {} }
            }
        });
        assert!(supports_session_resume(&init_with_resume));
        assert!(!supports_session_resume(&serde_json::json!({
            "agentCapabilities": { "loadSession": true }
        })));

        let session_with_modes = serde_json::json!({
            "sessionId": "agy-1",
            "modes": {
                "availableModes": [
                    { "id": "default", "name": "Default" },
                    { "id": "plan", "name": "Plan" }
                ]
            }
        });
        assert_eq!(
            resolve_acp_plan_mode_rpc(&session_with_modes, "agy-1"),
            Some((
                "session/set_mode",
                serde_json::json!({
                    "sessionId": "agy-1",
                    "modeId": "plan"
                })
            ))
        );

        let session_with_config = serde_json::json!({
            "sessionId": "agy-1",
            "configOptions": [
                {
                    "id": "mode",
                    "category": "mode",
                    "options": [{ "value": "plan" }, { "value": "auto" }]
                }
            ]
        });
        assert_eq!(
            resolve_acp_plan_mode_rpc(&session_with_config, "agy-1"),
            Some((
                "session/set_config_option",
                serde_json::json!({
                    "sessionId": "agy-1",
                    "configId": "mode",
                    "value": "plan"
                })
            ))
        );
        assert_eq!(
            resolve_acp_plan_mode_rpc(&serde_json::json!({ "sessionId": "agy-1" }), "agy-1"),
            None
        );
    }

    #[test]
    fn acp_available_commands_update_parses_commands_and_hints() {
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "agy-1",
                "update": {
                    "sessionUpdate": "available_commands_update",
                    "availableCommands": [
                        {
                            "name": "/plan",
                            "description": "Create an implementation plan",
                            "input": { "hint": "topic or task" }
                        },
                        {
                            "name": "compact",
                            "description": "Summarize conversation"
                        },
                        {
                            "name": "   ",
                            "description": "Ignored empty command"
                        }
                    ]
                }
            }
        });
        assert_eq!(
            acp_available_commands_from_message(&message),
            Some(vec![
                SlashCommand {
                    name: "plan".to_owned(),
                    description: Some("Create an implementation plan".to_owned()),
                    input_hint: Some("topic or task".to_owned()),
                },
                SlashCommand {
                    name: "compact".to_owned(),
                    description: Some("Summarize conversation".to_owned()),
                    input_hint: None,
                },
            ])
        );

        let non_command = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "agy-1",
                "update": { "sessionUpdate": "usage_update" }
            }
        });
        assert_eq!(acp_available_commands_from_message(&non_command), None);
    }
}
