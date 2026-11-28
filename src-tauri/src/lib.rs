use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    ffi::OsStr,
    fs,
    io::{BufRead, BufReader, Read},
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
use tauri_plugin_notification::NotificationExt;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

mod acp;
mod github_issue;
mod jira;
mod project_features;

use acp::{
    AcpProvider, permission_outcome, send_acp_message, send_acp_session_message,
    start_acp_session_blocking,
};

use project_features::{
    TerminalManager, close_project_terminal, create_agent_task_proposals, create_project_task,
    create_project_terminal, delete_project_task, finish_project_task_session,
    get_project_dependencies, get_project_diff, get_project_pipelines, get_project_ref_diff,
    increment_project_task_step, link_project_task_session, list_project_git_refs,
    list_project_skills, list_project_tasks, list_project_terminals, prepare_task_worktree,
    resize_project_terminal, set_project_task_archived, set_project_task_loop_enabled,
    set_project_task_status, take_next_loop_task, update_project_task, write_project_terminal,
};

#[derive(Clone)]
pub(crate) struct Database(pub(crate) Arc<Mutex<Connection>>);
pub(crate) struct SessionProcesses(pub(crate) Arc<Mutex<HashMap<String, SessionControl>>>);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PermissionOption {
    pub(crate) option_id: String,
    pub(crate) name: String,
    pub(crate) kind: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PendingPermission {
    pub(crate) request_id: String,
    pub(crate) title: String,
    pub(crate) options: Vec<PermissionOption>,
}

#[derive(Clone)]
pub(crate) struct SessionControl {
    pub(crate) provider: String,
    pub(crate) input: Sender<String>,
    pub(crate) pending: Arc<Mutex<HashMap<u64, Sender<Value>>>>,
    pub(crate) permission_requests: Arc<Mutex<HashMap<String, (Value, PendingPermission)>>>,
    pub(crate) next_request_id: Arc<AtomicU64>,
    pub(crate) stop_requested: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) replaying_history: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) child: Arc<Mutex<Child>>,
    pub(crate) assistant_message_id: Arc<Mutex<Option<String>>>,
}

pub(crate) const GITHUB_CREDENTIAL_SERVICE: &str = "com.agentcontrolplane.desktop.github";
pub(crate) const GITHUB_CREDENTIAL_USER: &str = "github-token";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    id: String,
    name: String,
    path: String,
    branch: String,
    current_commit: String,
    git_state: String,
    looping: bool,
    agent_task_creation: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentSession {
    id: String,
    project_id: String,
    task_id: Option<String>,
    worktree_path: Option<String>,
    external_session_id: Option<String>,
    project: String,
    task: String,
    branch: String,
    provider: String,
    status: String,
    step: String,
    elapsed: String,
    tokens: String,
    context_tokens: Option<i64>,
    context_window_size: Option<i64>,
    permission_mode: String,
    managed: bool,
    last_event: String,
    archived: bool,
    status_reason: String,
    updated_at: String,
    task_done: bool,
    task_archived: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptMessage {
    role: String,
    content: String,
    timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    event_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<String>,
    #[serde(skip)]
    sequence: i64,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TranscriptActivity {
    pub(crate) event_type: String,
    pub(crate) label: String,
    pub(crate) status: String,
    pub(crate) details: Value,
}

#[derive(Debug, Default, PartialEq)]
struct ClaudeObservation {
    session_id: Option<String>,
    text: Option<String>,
    step: Option<String>,
    result: Option<String>,
    activities: Vec<TranscriptActivity>,
    completed: bool,
    failed: bool,
    token_count: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
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
    #[serde(default)]
    antigravity: ToolSettingsInput,
    github_username: String,
    github_auth_method: String,
    github_token: String,
    jira_email: String,
    jira_token: String,
    default_provider: String,
    transcript_font_size: u8,
    ui_font_scale: u8,
    notifications_enabled: bool,
    attention_notifications_enabled: bool,
    task_completion_notifications_enabled: bool,
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
    antigravity: ToolSettings,
    github_username: String,
    github_auth_method: String,
    github_token_configured: bool,
    jira_email: String,
    jira_token_configured: bool,
    default_provider: String,
    transcript_font_size: u8,
    ui_font_scale: u8,
    notifications_enabled: bool,
    attention_notifications_enabled: bool,
    task_completion_notifications_enabled: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskResultNotificationEvent {
    session_id: String,
    project_id: String,
    task_id: String,
    success: bool,
}

fn task_result_notification_event(
    connection: &Connection,
    session_id: &str,
    success: bool,
) -> Result<Option<TaskResultNotificationEvent>, String> {
    connection
        .query_row(
            "SELECT p.id, t.id FROM project_tasks t
             JOIN projects p ON p.id = t.project_id
             WHERE t.session_id = ?1 AND t.status = 'in_progress'",
            [session_id],
            |row| {
                Ok(TaskResultNotificationEvent {
                    session_id: session_id.to_owned(),
                    project_id: row.get(0)?,
                    task_id: row.get(1)?,
                    success,
                })
            },
        )
        .optional()
        .map_err(|error| format!("Unable to read task notification details: {error}"))
}

fn emit_task_result_notification(
    app: &tauri::AppHandle,
    event: TaskResultNotificationEvent,
) -> Result<(), String> {
    app.emit("agent-task-result", event)
        .map_err(|error| format!("Unable to publish task result notification: {error}"))
}

#[derive(Debug)]
struct GitSnapshot {
    root: PathBuf,
    branch: String,
    current_commit: String,
    git_state: String,
}

fn git_command(path: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    hide_console_window(&mut cmd);
    let output = cmd
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

fn project_for_directory(path: &Path) -> Result<Project, String> {
    if path.as_os_str().is_empty() {
        return Err("Choose a local folder.".to_owned());
    }

    let canonical_path = fs::canonicalize(path)
        .map_err(|error| format!("The project path is not accessible: {error}"))?;
    if !canonical_path.is_dir() {
        return Err("The selected path is not a folder.".to_owned());
    }

    let has_git_metadata = canonical_path
        .ancestors()
        .any(|ancestor| ancestor.join(".git").exists());
    let (project_path, branch, current_commit, git_state) = if has_git_metadata {
        let snapshot = inspect_repository(&canonical_path)?;
        (
            snapshot.root,
            snapshot.branch,
            snapshot.current_commit,
            snapshot.git_state,
        )
    } else {
        (
            canonical_path,
            String::new(),
            String::new(),
            "not-git".to_owned(),
        )
    };
    let path = project_path
        .to_str()
        .ok_or_else(|| "The project path is not valid UTF-8.".to_owned())?
        .to_owned();

    Ok(Project {
        id: path.clone(),
        name: project_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Unnamed project")
            .to_owned(),
        path,
        branch,
        current_commit,
        git_state,
        looping: false,
        agent_task_creation: false,
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
        looping: row.get::<_, i64>(6)? != 0,
        agent_task_creation: row.get::<_, i64>(7)? != 0,
    })
}

fn agent_session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentSession> {
    let token_count: i64 = row.get(8)?;
    let token_usage_available: bool = row.get::<_, i64>(9)? != 0 || token_count != 0;
    let context_window_size: Option<i64> = row.get(10)?;
    Ok(AgentSession {
        id: row.get(0)?,
        project_id: row.get(20)?,
        task_id: row.get(21)?,
        worktree_path: row.get(22)?,
        project: row.get(1)?,
        task: row.get(2)?,
        branch: row.get(3)?,
        provider: row.get(4)?,
        status: row.get(5)?,
        step: row.get(6)?,
        elapsed: row.get(7)?,
        tokens: if token_usage_available {
            format_token_count(token_count)
        } else {
            "Not reported".to_owned()
        },
        context_tokens: token_usage_available.then_some(token_count),
        context_window_size,
        permission_mode: row.get(11)?,
        managed: row.get::<_, String>(12)? == "managed",
        last_event: row.get(13)?,
        archived: row.get::<_, i64>(14)? != 0,
        status_reason: row.get(15)?,
        updated_at: row.get(16)?,
        external_session_id: row.get(17)?,
        task_done: row.get::<_, i64>(18)? != 0,
        task_archived: row.get::<_, i64>(19)? != 0,
    })
}

fn format_token_count(token_count: i64) -> String {
    if token_count < 1000 {
        token_count.to_string()
    } else {
        format!("{:.1}k", token_count as f64 / 1000.0)
    }
}

fn validate_session_status(status: &str) -> Result<(), String> {
    match status {
        "starting" | "running" | "waiting" | "idle" | "paused" | "completed" | "failed"
        | "orphaned" | "unknown" => Ok(()),
        _ => Err(format!("Unsupported agent session status: {status}")),
    }
}

fn copilot_executable_from(
    override_path: Option<&OsStr>,
    search_path: Option<&OsStr>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(executable) = override_path {
        return Some(copilot_override_path(executable));
    }

    if let Some(path) = search_path.and_then(|path| {
        std::env::split_paths(path).find_map(|directory| copilot_in_directory(&directory))
    }) {
        return Some(path);
    }

    if let Some(home) = home {
        for candidate in [
            home.join(".local/bin"),
            home.join(".npm-global/bin"),
            home.join(".volta/bin"),
            home.join(".bun/bin"),
            home.join(".local/share/pnpm"),
            home.join("Library/pnpm"),
            home.join(".yarn/bin"),
            home.join(".linuxbrew/bin"),
        ] {
            if let Some(candidate) = copilot_in_directory(&candidate) {
                return Some(candidate);
            }
        }

        let nvm_nodes = home.join(".nvm/versions/node");
        if let Ok(entries) = fs::read_dir(nvm_nodes) {
            let mut candidates = entries
                .filter_map(Result::ok)
                .filter_map(|entry| copilot_in_directory(&entry.path().join("bin")))
                .collect::<Vec<_>>();
            candidates.sort();
            if let Some(candidate) = candidates.pop() {
                return Some(candidate);
            }
        }
    }

    #[cfg(windows)]
    if let Some(app_data) = std::env::var_os("APPDATA") {
        if let Some(candidate) = copilot_in_directory(&PathBuf::from(app_data).join("npm")) {
            return Some(candidate);
        }
    }

    for candidate in [
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ] {
        if let Some(candidate) = copilot_in_directory(&candidate) {
            return Some(candidate);
        }
    }

    None
}

fn copilot_in_directory(directory: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    let candidates = [
        directory.join("copilot.exe"),
        directory.join("copilot.cmd"),
        directory.join("copilot.bat"),
    ];
    #[cfg(not(windows))]
    let candidates = [directory.join("copilot")];
    candidates.into_iter().find(|candidate| candidate.is_file())
}

fn copilot_override_path(executable: &OsStr) -> PathBuf {
    let executable = PathBuf::from(executable);
    #[cfg(windows)]
    {
        let extension = executable
            .extension()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_lowercase);
        if !matches!(extension.as_deref(), Some("exe" | "cmd" | "bat")) {
            for extension in ["exe", "cmd", "bat"] {
                let candidate = executable.with_extension(extension);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }
    executable
}

fn copilot_executable() -> Result<PathBuf, String> {
    let override_path = std::env::var_os("COPILOT_CLI_PATH");
    let search_path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    copilot_executable_from(
        override_path.as_deref(),
        search_path.as_deref(),
        home.as_deref(),
    )
    .ok_or_else(|| {
        "GitHub Copilot CLI was not found. Install it or set COPILOT_CLI_PATH to its executable. See https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/install-copilot-cli"
            .to_owned()
    })
}

// CLI shims use `#!/usr/bin/env node`; GUI apps on macOS have a minimal PATH,
// so add the executable's directory and common node locations.
fn with_node_path(command: &mut Command, executable: &std::path::Path) {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(parent) = executable.parent() {
        dirs.push(parent.to_path_buf());
        if let Ok(real) = fs::canonicalize(executable) {
            if let Some(parent) = real.parent() {
                dirs.push(parent.to_path_buf());
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".volta/bin"));
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".bun/bin"));
        if let Ok(entries) = fs::read_dir(home.join(".nvm/versions/node")) {
            let mut nodes = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path().join("bin"))
                .collect::<Vec<_>>();
            nodes.sort();
            dirs.extend(nodes.pop());
        }
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    if let Some(existing) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&existing));
    }
    if let Ok(joined) = std::env::join_paths(dirs) {
        command.env("PATH", joined);
    }
}

fn hide_console_window(command: &mut Command) {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
}

pub(crate) fn configure_agent_process(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
}

pub(crate) fn terminate_agent_process(child: &mut Child) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let process_group = -(child.id() as libc::pid_t);
        let result = unsafe { libc::kill(process_group, libc::SIGKILL) };
        if result == 0 {
            return Ok(());
        }

        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
        return match child.try_wait()? {
            Some(_) => Ok(()),
            None => child.kill(),
        };
    }

    #[cfg(windows)]
    {
        let mut command = Command::new("taskkill");
        hide_console_window(&mut command);
        let process_id = child.id().to_string();
        let status = command
            .args(["/PID", process_id.as_str(), "/T", "/F"])
            .status()?;
        if status.success() {
            return Ok(());
        }

        return match child.try_wait()? {
            Some(_) => Ok(()),
            None => child.kill(),
        };
    }
}

fn command_for_executable(executable: &Path) -> Command {
    #[cfg(windows)]
    if matches!(
        executable
            .extension()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("cmd" | "bat")
    ) {
        let mut command = Command::new("cmd.exe");
        hide_console_window(&mut command);
        command.arg("/d").arg("/c").arg(executable);
        return command;
    }

    let mut command = Command::new(executable);
    hide_console_window(&mut command);
    command
}

pub(crate) fn copilot_command() -> Result<(Command, PathBuf), String> {
    let executable = copilot_executable()?;
    let mut command = command_for_executable(&executable);
    with_node_path(&mut command, &executable);
    Ok((command, executable))
}

fn antigravity_in_directory(directory: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    let candidates = [
        directory.join("agy.exe"),
        directory.join("agy.cmd"),
        directory.join("agy.bat"),
    ];
    #[cfg(not(windows))]
    let candidates = [directory.join("agy")];
    candidates.into_iter().find(|candidate| candidate.is_file())
}

fn antigravity_executable_from(
    override_path: Option<&OsStr>,
    search_path: Option<&OsStr>,
    home: Option<&Path>,
    local_app_data: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(executable) = override_path {
        return Some(copilot_override_path(executable));
    }

    if let Some(path) = search_path.and_then(|path| {
        std::env::split_paths(path).find_map(|directory| antigravity_in_directory(&directory))
    }) {
        return Some(path);
    }

    if let Some(local_app_data) = local_app_data {
        if let Some(candidate) = antigravity_in_directory(&local_app_data.join("agy").join("bin")) {
            return Some(candidate);
        }
    }

    if let Some(home) = home {
        if let Some(candidate) = antigravity_in_directory(&home.join(".local/bin")) {
            return Some(candidate);
        }
    }

    for candidate in [
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ] {
        if let Some(candidate) = antigravity_in_directory(&candidate) {
            return Some(candidate);
        }
    }

    None
}

fn antigravity_executable() -> Result<PathBuf, String> {
    let override_path = std::env::var_os("ANTIGRAVITY_CLI_PATH");
    let search_path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let local_app_data = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    antigravity_executable_from(
        override_path.as_deref(),
        search_path.as_deref(),
        home.as_deref(),
        local_app_data.as_deref(),
    )
    .ok_or_else(|| {
        "Antigravity CLI (agy) was not found. Install it or set ANTIGRAVITY_CLI_PATH to its executable. See https://antigravity.google/docs/getting-started"
            .to_owned()
    })
}

pub(crate) fn antigravity_command() -> Result<(Command, PathBuf), String> {
    let executable = antigravity_executable()?;
    let command = command_for_executable(&executable);
    Ok((command, executable))
}

fn claude_executable() -> PathBuf {
    if let Some(executable) = std::env::var_os("CLAUDE_CODE_CLI_PATH") {
        return PathBuf::from(executable);
    }

    if let Some(path) = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join("claude"))
            .find(|candidate| candidate.is_file())
    }) {
        return path;
    }

    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        for candidate in [
            home.join(".local/bin/claude"),
            home.join(".claude/local/claude"),
            home.join(".npm-global/bin/claude"),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }

        let nvm_nodes = home.join(".nvm/versions/node");
        if let Ok(entries) = fs::read_dir(nvm_nodes) {
            let mut candidates = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path().join("bin/claude"))
                .filter(|candidate| candidate.is_file())
                .collect::<Vec<_>>();
            candidates.sort();
            if let Some(candidate) = candidates.pop() {
                return candidate;
            }
        }
    }

    PathBuf::from("claude")
}

pub(crate) fn claude_command() -> Command {
    let executable = claude_executable();
    let mut command = Command::new(&executable);
    hide_console_window(&mut command);
    with_node_path(&mut command, &executable);
    command
}

fn copilot_session_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("copilot-{timestamp:x}")
}

const MAX_ACTIVITY_DETAILS_BYTES: usize = 24 * 1024;
const MAX_TRANSCRIPT_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
const TRANSCRIPT_TRUNCATION_MARKER: &str = "\n[Transcript output truncated at 2 MiB]";

fn redact_activity_text(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let markers = [
        "bearer ",
        "token=",
        "api_key=",
        "apikey=",
        "password=",
        "secret=",
        "authorization=",
    ];
    let mut ranges = Vec::new();
    for marker in markers {
        let mut cursor = 0;
        while cursor + marker.len() <= lower.len() {
            let Some(offset) = lower[cursor..]
                .as_bytes()
                .windows(marker.len())
                .position(|window| window == marker.as_bytes())
            else {
                break;
            };
            let start = cursor + offset + marker.len();
            let end = text.as_bytes()[start..]
                .iter()
                .position(|byte| {
                    byte.is_ascii_whitespace() || matches!(*byte, b'"' | b'\'' | b'&' | b';' | b',')
                })
                .map(|offset| start + offset)
                .unwrap_or(text.len());
            if end > start {
                ranges.push((start, end));
            }
            cursor = end.max(start).saturating_add(1);
        }
    }
    ranges.sort_unstable();
    ranges.dedup();
    let mut redacted = text.to_owned();
    for (start, end) in ranges.into_iter().rev() {
        redacted.replace_range(start..end, "[REDACTED]");
    }
    redacted
}

fn redact_activity_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let normalized = key.to_ascii_lowercase();
                    let sensitive = [
                        "token",
                        "secret",
                        "password",
                        "authorization",
                        "credential",
                        "api_key",
                        "apikey",
                    ]
                    .iter()
                    .any(|marker| normalized.contains(marker));
                    (
                        key.clone(),
                        if sensitive {
                            Value::String("[REDACTED]".to_owned())
                        } else {
                            redact_activity_value(value)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_activity_value).collect()),
        Value::String(text) => Value::String(redact_activity_text(text)),
        _ => value.clone(),
    }
}

fn bounded_activity_details(value: &Value) -> String {
    let sanitized = redact_activity_value(value);
    let encoded = serde_json::to_string_pretty(&sanitized)
        .unwrap_or_else(|_| "\"Unable to format provider details\"".to_owned());
    if encoded.len() <= MAX_ACTIVITY_DETAILS_BYTES {
        return encoded;
    }

    const SUFFIX: &str = "…[truncated]";
    let mut end = MAX_ACTIVITY_DETAILS_BYTES - SUFFIX.len();
    while !encoded.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &encoded[..end], SUFFIX)
}

fn claude_observation_from_line(line: &str) -> Result<ClaudeObservation, String> {
    let message: Value = serde_json::from_str(line)
        .map_err(|error| format!("Claude Code returned invalid stream JSON: {error}"))?;
    let event_type = message["type"].as_str().unwrap_or_default();
    let session_id = message["session_id"].as_str().map(str::to_owned);

    match event_type {
        "system" => Ok(ClaudeObservation {
            session_id,
            ..ClaudeObservation::default()
        }),
        "stream_event" => {
            let event = &message["event"];
            let text = (event["type"].as_str() == Some("content_block_delta")
                && event["delta"]["type"].as_str() == Some("text_delta"))
            .then(|| event["delta"]["text"].as_str().map(str::to_owned))
            .flatten();
            Ok(ClaudeObservation {
                session_id,
                text,
                ..ClaudeObservation::default()
            })
        }
        "assistant" => {
            let content = message["message"]["content"].as_array();
            let activities = content
                .into_iter()
                .flatten()
                .filter(|block| block["type"].as_str() == Some("tool_use"))
                .map(|block| TranscriptActivity {
                    event_type: "tool_call".to_owned(),
                    label: block["name"]
                        .as_str()
                        .unwrap_or("Claude Code tool call")
                        .to_owned(),
                    status: "started".to_owned(),
                    details: block.clone(),
                })
                .collect::<Vec<_>>();
            let step = activities
                .first()
                .map(|activity| format!("Claude Code is using {}", activity.label));
            Ok(ClaudeObservation {
                session_id,
                step,
                activities,
                ..ClaudeObservation::default()
            })
        }
        "user" => {
            let activities = message["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|block| block["type"].as_str() == Some("tool_result"))
                .map(|block| {
                    let failed = block["is_error"].as_bool().unwrap_or(false);
                    TranscriptActivity {
                        event_type: if failed {
                            "tool_error".to_owned()
                        } else {
                            "tool_output".to_owned()
                        },
                        label: block["tool_use_id"]
                            .as_str()
                            .map(|id| format!("Tool result {id}"))
                            .unwrap_or_else(|| "Claude Code tool result".to_owned()),
                        status: if failed {
                            "failed".to_owned()
                        } else {
                            "completed".to_owned()
                        },
                        details: block.clone(),
                    }
                })
                .collect();
            Ok(ClaudeObservation {
                session_id,
                activities,
                ..ClaudeObservation::default()
            })
        }
        "control_request" | "control_response" => {
            let is_request = event_type == "control_request";
            let details = message.get("request").or_else(|| message.get("response"));
            let details = details.cloned().unwrap_or_else(|| message.clone());
            let request_text = details.to_string().to_ascii_lowercase();
            let permission_event = request_text.contains("permission")
                || request_text.contains("approval")
                || request_text.contains("allow");
            let activity = permission_event.then(|| TranscriptActivity {
                event_type: if is_request {
                    "permission_request".to_owned()
                } else {
                    "permission_decision".to_owned()
                },
                label: if is_request {
                    "Claude Code requested permission".to_owned()
                } else {
                    "Claude Code permission decision".to_owned()
                },
                status: if is_request {
                    "requested".to_owned()
                } else {
                    "decided".to_owned()
                },
                details,
            });
            Ok(ClaudeObservation {
                session_id,
                activities: activity.into_iter().collect(),
                ..ClaudeObservation::default()
            })
        }
        "result" => {
            let usage = &message["usage"];
            let token_count = [
                usage["input_tokens"].as_u64(),
                usage["output_tokens"].as_u64(),
                usage["cache_read_input_tokens"].as_u64(),
                usage["cache_creation_input_tokens"].as_u64(),
            ]
            .into_iter()
            .flatten()
            .fold(0_u64, u64::saturating_add);
            let result = message["result"].as_str().map(str::to_owned);
            let activities = (message["subtype"].as_str() != Some("success"))
                .then(|| TranscriptActivity {
                    event_type: "error".to_owned(),
                    label: result
                        .clone()
                        .unwrap_or_else(|| "Claude Code reported a failed request".to_owned()),
                    status: "failed".to_owned(),
                    details: message.clone(),
                })
                .into_iter()
                .collect();
            Ok(ClaudeObservation {
                session_id,
                result,
                activities,
                completed: true,
                failed: message["subtype"].as_str() != Some("success"),
                token_count: i64::try_from(token_count).ok(),
                ..ClaudeObservation::default()
            })
        }
        _ => Ok(ClaudeObservation {
            session_id,
            ..ClaudeObservation::default()
        }),
    }
}

fn persist_session_activity(
    database: &Arc<Mutex<Connection>>,
    session_id: &str,
    activity: &TranscriptActivity,
) -> Result<(), String> {
    let connection = database
        .lock()
        .map_err(|_| "Agent session database is unavailable.".to_owned())?;
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
        .map_err(|error| format!("Unable to sequence transcript activity: {error}"))?;
    let payload = serde_json::json!({
        "label": redact_activity_text(&activity.label),
        "status": activity.status,
        "details": bounded_activity_details(&activity.details),
    });
    let payload = serde_json::to_string(&payload)
        .map_err(|error| format!("Unable to encode transcript activity: {error}"))?;
    connection
        .execute(
            "INSERT INTO session_events
             (id, session_id, sequence, event_type, payload)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                copilot_session_id(),
                session_id,
                sequence,
                activity.event_type,
                payload
            ],
        )
        .map_err(|error| format!("Unable to persist transcript activity: {error}"))?;
    connection
        .execute(
            "UPDATE agent_sessions
             SET last_seen_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1",
            [session_id],
        )
        .map_err(|error| format!("Unable to update agent session activity time: {error}"))?;
    Ok(())
}

pub(crate) fn capture_session_activity(
    database: &Arc<Mutex<Connection>>,
    app: &tauri::AppHandle,
    session_id: &str,
    activity: &TranscriptActivity,
) {
    match persist_session_activity(database, session_id, activity) {
        Ok(()) => {
            let _ = app.emit("agent-session-updated", session_id);
        }
        Err(error) => {
            eprintln!("{error}");
            let _ = app.emit(
                "agent-session-activity-error",
                serde_json::json!({ "sessionId": session_id, "error": error }),
            );
        }
    }
}

pub(crate) fn validate_session_message_status(
    status: &str,
    is_initial_prompt: bool,
    has_pending_permission: bool,
) -> Result<(), String> {
    if has_pending_permission {
        return Err(
            "Respond to the agent's pending permission request before sending a message."
                .to_owned(),
        );
    }
    if matches!(status, "running" | "starting") && !is_initial_prompt {
        return Err("Wait for the agent to finish its current response.".to_owned());
    }
    Ok(())
}

pub(crate) fn insert_session_message(
    connection: &Connection,
    session_id: &str,
    role: &str,
    content: &str,
) -> Result<String, String> {
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

pub(crate) fn append_assistant_chunk(
    database: &Arc<Mutex<Connection>>,
    session_id: &str,
    message_id: &str,
    content: &str,
) -> Result<(), String> {
    let connection = database
        .lock()
        .map_err(|_| "Agent session database is unavailable.".to_owned())?;
    let existing = connection
        .query_row(
            "SELECT content FROM session_messages
             WHERE id = ?1 AND session_id = ?2 AND role = 'assistant'",
            params![message_id, session_id],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| format!("Unable to read assistant transcript: {error}"))?;
    let content_limit = MAX_TRANSCRIPT_MESSAGE_BYTES - TRANSCRIPT_TRUNCATION_MARKER.len();
    if existing.len() >= content_limit {
        let mut keep_end = content_limit.saturating_sub(TRANSCRIPT_TRUNCATION_MARKER.len());
        while !existing.is_char_boundary(keep_end) {
            keep_end -= 1;
        }
        let replacement = format!("{}{}", &existing[..keep_end], TRANSCRIPT_TRUNCATION_MARKER);
        connection
            .execute(
                "UPDATE session_messages SET content = ?2
                 WHERE id = ?1 AND session_id = ?3 AND role = 'assistant'",
                params![message_id, replacement, session_id],
            )
            .map_err(|error| format!("Unable to bound assistant transcript: {error}"))?;
        return Ok(());
    }
    let remaining = content_limit - existing.len();
    let mut end = content.len().min(remaining);
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    let mut bounded_chunk = content[..end].to_owned();
    if end < content.len() {
        bounded_chunk.push_str(TRANSCRIPT_TRUNCATION_MARKER);
    }
    connection
        .execute(
            "UPDATE session_messages SET content = content || ?2
             WHERE id = ?1 AND session_id = ?3 AND role = 'assistant'",
            params![message_id, bounded_chunk, session_id],
        )
        .map_err(|error| format!("Unable to save agent response: {error}"))?;
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

pub(crate) fn update_session_token_count(
    database: &Arc<Mutex<Connection>>,
    session_id: &str,
    token_count: i64,
    context_window_size: Option<i64>,
) -> Result<(), String> {
    let connection = database
        .lock()
        .map_err(|_| "Agent session database is unavailable.".to_owned())?;
    connection
        .execute(
            "UPDATE agent_sessions
             SET token_count = ?2, token_usage_available = 1, context_window_size = ?3,
                 last_seen_at = CURRENT_TIMESTAMP,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1",
            params![session_id, token_count, context_window_size],
        )
        .map_err(|error| format!("Unable to update agent session token usage: {error}"))?;
    Ok(())
}

pub(crate) fn update_session_status(
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

fn read_notification_setting(connection: &Connection, key: &str) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            [key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("Unable to read notification setting {key}: {error}"))
        .map(|value| {
            value
                .and_then(|value| value.parse::<bool>().ok())
                .unwrap_or(true)
        })
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

fn jira_token_configured() -> bool {
    keyring::Entry::new(jira::JIRA_CREDENTIAL_SERVICE, jira::JIRA_CREDENTIAL_USER)
        .and_then(|entry| entry.get_password())
        .is_ok()
}

#[tauri::command]
fn get_current_username() -> Option<String> {
    ["USER", "USERNAME"].iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .map(|username| username.trim().to_owned())
            .filter(|username| !username.is_empty())
    })
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
    let github_auth_method = connection
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'github_auth_method'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| "token".to_owned());
    let jira_email = connection
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'jira_email'",
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
    let ui_font_scale = connection
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'ui_font_scale'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|value| value.parse::<u8>().ok())
        .filter(|scale| [100, 110, 120, 130].contains(scale))
        .unwrap_or(100);
    Ok(Settings {
        copilot: read("copilot"),
        gemini: read("gemini"),
        claude_code: read("claude_code"),
        antigravity: read("antigravity"),
        github_username,
        github_auth_method,
        github_token_configured: github_token_configured(),
        jira_email,
        jira_token_configured: jira_token_configured(),
        default_provider,
        transcript_font_size,
        ui_font_scale,
        notifications_enabled: read_notification_setting(&connection, "notifications_enabled")?,
        attention_notifications_enabled: read_notification_setting(
            &connection,
            "attention_notifications_enabled",
        )?,
        task_completion_notifications_enabled: read_notification_setting(
            &connection,
            "task_completion_notifications_enabled",
        )?,
    })
}

#[tauri::command]
fn save_settings(input: SettingsInput, database: State<'_, Database>) -> Result<Settings, String> {
    if !matches!(input.github_auth_method.as_str(), "token" | "cli") {
        return Err("Choose a supported GitHub connection method.".to_owned());
    }
    if !matches!(
        input.default_provider.as_str(),
        "copilot" | "gemini" | "claudeCode" | "antigravity"
    ) {
        return Err("Choose a supported default AI agent.".to_owned());
    }
    if !(12..=24).contains(&input.transcript_font_size) {
        return Err("Transcript text size must be between 12px and 24px.".to_owned());
    }
    if ![100, 110, 120, 130].contains(&input.ui_font_scale) {
        return Err("Application text size must be 100%, 110%, 120%, or 130%.".to_owned());
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
    let jira_token = input.jira_token.trim().to_owned();
    if !jira_token.is_empty() {
        keyring::Entry::new(jira::JIRA_CREDENTIAL_SERVICE, jira::JIRA_CREDENTIAL_USER)
            .map_err(|error| format!("Unable to access the OS credential vault: {error}"))?
            .set_password(&jira_token)
            .map_err(|error| {
                format!("Unable to save the Jira API token in the OS credential vault: {error}")
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
        ("antigravity", &input.antigravity),
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
            "INSERT INTO app_settings (key, value) VALUES ('github_auth_method', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [&input.github_auth_method],
        )
        .map_err(|error| format!("Unable to save GitHub settings: {error}"))?;
    transaction
        .execute(
            "INSERT INTO app_settings (key, value) VALUES ('jira_email', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [&input.jira_email.trim()],
        )
        .map_err(|error| format!("Unable to save Jira settings: {error}"))?;
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
        .execute(
            "INSERT INTO app_settings (key, value) VALUES ('ui_font_scale', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [input.ui_font_scale.to_string()],
        )
        .map_err(|error| format!("Unable to save application text size: {error}"))?;
    for (key, enabled) in [
        ("notifications_enabled", input.notifications_enabled),
        (
            "attention_notifications_enabled",
            input.attention_notifications_enabled,
        ),
        (
            "task_completion_notifications_enabled",
            input.task_completion_notifications_enabled,
        ),
    ] {
        transaction
            .execute(
                "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, enabled.to_string()],
            )
            .map_err(|error| format!("Unable to save {key}: {error}"))?;
    }
    transaction
        .commit()
        .map_err(|error| format!("Unable to commit settings: {error}"))?;

    drop(connection);
    load_settings(database)
}

#[tauri::command]
fn send_system_notification(
    category: String,
    session_id: String,
    request_id: Option<String>,
    project_id: Option<String>,
    task_id: Option<String>,
    success: Option<bool>,
    database: State<'_, Database>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let (title, body) = match category.as_str() {
        "attention" => (
            "Agent needs your attention",
            "Open Agent Control Plane to review a pending request.",
        ),
        "taskResult" => match success {
            Some(true) => (
                "Task completed",
                "Open Agent Control Plane to view the outcome.",
            ),
            Some(false) => (
                "Task failed",
                "Open Agent Control Plane to view the outcome.",
            ),
            None => return Err("A task result notification needs an outcome.".to_owned()),
        },
        _ => return Err("Choose a supported notification category.".to_owned()),
    };
    if session_id.is_empty() {
        return Err("A session is required for a notification.".to_owned());
    }
    let connection = database
        .0
        .lock()
        .map_err(|_| "Notification settings database is unavailable.".to_owned())?;
    let master_enabled = read_notification_setting(&connection, "notifications_enabled")?;
    let category_enabled = match category.as_str() {
        "attention" => read_notification_setting(&connection, "attention_notifications_enabled")?,
        "taskResult" => {
            read_notification_setting(&connection, "task_completion_notifications_enabled")?
        }
        _ => false,
    };
    if !master_enabled || !category_enabled {
        return Ok(());
    }
    drop(connection);

    let mut notification = app.notification().builder().title(title).body(body);
    #[cfg(target_os = "linux")]
    {
        notification = notification.sound("bell");
    }
    notification = notification
        .extra("category", &category)
        .extra("sessionId", &session_id);
    if let Some(request_id) = request_id {
        notification = notification.extra("requestId", request_id);
    }
    if let Some(project_id) = project_id {
        notification = notification.extra("projectId", project_id);
    }
    if let Some(task_id) = task_id {
        notification = notification.extra("taskId", task_id);
    }
    if let Some(success) = success {
        notification = notification.extra("success", success);
    }
    notification
        .show()
        .map_err(|error| format!("Unable to show system notification: {error}"))
}

#[tauri::command]
fn list_projects(database: State<'_, Database>) -> Result<Vec<Project>, String> {
    let connection = database
        .0
        .lock()
        .map_err(|_| "Project database is unavailable.".to_owned())?;
    let mut statement = connection
        .prepare(
            "SELECT id, name, path, branch, current_commit, git_state, looping, agent_task_creation
             FROM projects WHERE connected = 1 ORDER BY name COLLATE NOCASE",
        )
        .map_err(|error| format!("Unable to read projects: {error}"))?;

    statement
        .query_map([], project_from_row)
        .map_err(|error| format!("Unable to read projects: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Unable to decode project data: {error}"))
}

fn disconnect_project(connection: &Connection, project_id: &str) -> Result<(), String> {
    let active: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_sessions WHERE project_id = ?1
         AND origin = 'managed' AND status IN ('starting', 'running', 'waiting', 'idle'))",
            [project_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Unable to check project sessions: {error}"))?;
    if active {
        return Err("Stop the project's active agent sessions before removing it.".to_owned());
    }
    let updated = connection.execute(
        "UPDATE projects SET connected = 0, looping = 0, agent_task_creation = 0, updated_at = CURRENT_TIMESTAMP
         WHERE id = ?1 AND connected = 1", [project_id],
    ).map_err(|error| format!("Unable to remove project: {error}"))?;
    if updated == 0 {
        return Err("Project was not found.".to_owned());
    }
    Ok(())
}

#[tauri::command]
fn remove_project(
    project_id: String,
    database: State<'_, Database>,
    terminals: State<'_, TerminalManager>,
) -> Result<(), String> {
    if terminals.has_live_project_terminal(&project_id)? {
        return Err("Close the project's open terminals before removing it.".to_owned());
    }
    let connection = database
        .0
        .lock()
        .map_err(|_| "Project database is unavailable.".to_owned())?;
    disconnect_project(&connection, &project_id)
}

#[tauri::command]
async fn set_project_automation(
    project_id: String,
    looping: bool,
    agent_task_creation: bool,
    database: State<'_, Database>,
    processes: State<'_, SessionProcesses>,
    app: tauri::AppHandle,
) -> Result<Project, String> {
    let database = Database(database.0.clone());
    let processes = SessionProcesses(processes.0.clone());
    run_agent_operation(move || {
        {
            let connection = database
                .0
                .lock()
                .map_err(|_| "Project database is unavailable.".to_owned())?;
            let changed = connection
                .execute(
                    "UPDATE projects SET looping = ?1, agent_task_creation = ?2,
                     updated_at = CURRENT_TIMESTAMP WHERE id = ?3",
                    params![looping, agent_task_creation, project_id],
                )
                .map_err(|error| format!("Unable to save project automation settings: {error}"))?;
            if changed == 0 {
                return Err("The selected project is no longer registered.".to_owned());
            }
        }
        if looping {
            maybe_start_next_loop_task(
                project_id.clone(),
                database.clone(),
                processes,
                app.clone(),
            )?;
        }
        let connection = database
            .0
            .lock()
            .map_err(|_| "Project database is unavailable.".to_owned())?;
        connection
            .query_row(
                "SELECT id, name, path, branch, current_commit, git_state, looping, agent_task_creation
                 FROM projects WHERE id = ?1",
                [&project_id],
                project_from_row,
            )
            .map_err(|error| format!("Unable to read updated project settings: {error}"))
    })
    .await
}

#[tauri::command]
async fn continue_project_loop(
    project_id: String,
    database: State<'_, Database>,
    processes: State<'_, SessionProcesses>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let database = Database(database.0.clone());
    let processes = SessionProcesses(processes.0.clone());
    run_agent_operation(move || {
        maybe_start_next_loop_task(project_id, database, processes, app)?;
        Ok(())
    })
    .await
}

fn vscode_executables() -> Vec<PathBuf> {
    #[cfg(not(windows))]
    #[allow(unused_mut)]
    let mut executables = vec![PathBuf::from("code"), PathBuf::from("code-insiders")];
    #[cfg(windows)]
    let mut executables = {
        let mut candidates = Vec::new();
        if let Some(path) = std::env::var_os("PATH") {
            for directory in std::env::split_paths(&path) {
                let candidate = directory.join("code.cmd");
                if candidate.is_file() {
                    candidates.push(candidate);
                }
            }
        }
        candidates.push(PathBuf::from("code.exe"));
        candidates
    };
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            executables.push(
                PathBuf::from(home)
                    .join("Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"),
            );
        }
        executables.push(PathBuf::from(
            "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code",
        ));
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            executables.push(
                PathBuf::from(local_app_data).join("Programs/Microsoft VS Code/bin/code.cmd"),
            );
        }
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            executables.push(PathBuf::from(program_files).join("Microsoft VS Code/bin/code.cmd"));
        }
        if let Some(program_files_x86) = std::env::var_os("ProgramFiles(x86)") {
            executables
                .push(PathBuf::from(program_files_x86).join("Microsoft VS Code/bin/code.cmd"));
        }
    }
    executables
}

fn launch_vscode(workspace: &Path, executables: Vec<PathBuf>) -> Result<(), String> {
    let workspace = workspace
        .canonicalize()
        .map_err(|error| format!("The workspace folder is unavailable: {error}"))?;
    if !workspace.is_dir() {
        return Err("The workspace path is not a directory.".to_owned());
    }
    for executable in executables {
        // cmd.exe itself can start even when the requested VS Code script is missing.
        #[cfg(windows)]
        if executable
            .extension()
            .is_some_and(|extension| extension == "cmd")
            && !executable.is_file()
        {
            continue;
        }
        let mut cmd = command_for_executable(&executable);
        match cmd
            .arg("--new-window")
            .arg(&workspace)
            .stdin(Stdio::null())
            .output()
        {
            Ok(output) if output.status.success() => return Ok(()),
            Ok(output) => {
                let detail = String::from_utf8_lossy(&output.stderr);
                return Err(format!(
                    "VS Code failed to launch ({}). {}",
                    output.status,
                    detail.trim()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!("VS Code failed to launch: {error}"));
            }
        }
    }
    Err(
        "VS Code executable was not found. Install its `code` command on PATH and try again."
            .to_owned(),
    )
}

#[tauri::command]
async fn open_project_in_vscode(
    project_id: String,
    database: State<'_, Database>,
) -> Result<(), String> {
    let database = Database(database.0.clone());
    run_agent_operation(move || {
        let path = {
            let connection = database
                .0
                .lock()
                .map_err(|_| "Project database is unavailable.".to_owned())?;
            connection
                .query_row(
                    "SELECT path FROM projects WHERE id = ?1",
                    [&project_id],
                    |row| row.get::<_, String>(0),
                )
                .map_err(|error| format!("Unable to find project folder: {error}"))?
        };
        launch_vscode(Path::new(&path), vscode_executables())
    })
    .await
}

fn task_worktree_path(
    connection: &Connection,
    project_id: &str,
    task_id: &str,
) -> Result<String, String> {
    let path: Option<String> = connection
        .query_row(
            "SELECT s.worktree_path FROM project_tasks t
             LEFT JOIN agent_sessions s ON s.id = t.session_id AND s.project_id = t.project_id
             WHERE t.id = ?1 AND t.project_id = ?2",
            params![task_id, project_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Unable to find the task worktree: {error}"))?;
    path.filter(|path| !path.trim().is_empty())
        .ok_or_else(|| "This task does not have a prepared worktree.".to_owned())
}

#[tauri::command]
async fn open_task_in_vscode(
    project_id: String,
    task_id: String,
    database: State<'_, Database>,
) -> Result<(), String> {
    let database = Database(database.0.clone());
    run_agent_operation(move || {
        let path = {
            let connection = database
                .0
                .lock()
                .map_err(|_| "Project database is unavailable.".to_owned())?;
            task_worktree_path(&connection, &project_id, &task_id)?
        };
        launch_vscode(Path::new(&path), vscode_executables())
    })
    .await
}

pub(crate) fn project_session_auto_approve_permissions(
    database: &Arc<Mutex<Connection>>,
    session_id: &str,
    tool_kind: &str,
) -> Result<bool, String> {
    let connection = database
        .lock()
        .map_err(|_| "Project database is unavailable.".to_owned())?;
    connection
        .query_row(
            "SELECT s.permission_mode,
                (s.loop_owned = 1 AND p.looping = 1 AND COALESCE(t.loop_enabled, 0) = 1)
         FROM agent_sessions s
         JOIN projects p ON p.id = s.project_id
         LEFT JOIN project_tasks t ON t.session_id = s.id AND t.status = 'in_progress'
         WHERE s.id = ?1",
            [session_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
        )
        .map(|(mode, loop_approval)| {
            loop_approval || mode == "auto" || (mode == "acceptEdits" && tool_kind == "edit")
        })
        .map_err(|error| format!("Unable to read session task settings: {error}"))
}

fn maybe_start_next_loop_task(
    project_id: String,
    database: Database,
    processes: SessionProcesses,
    app: tauri::AppHandle,
) -> Result<bool, String> {
    let task = {
        let connection = database
            .0
            .lock()
            .map_err(|_| "Project database is unavailable.".to_owned())?;
        take_next_loop_task(&connection, &project_id)?
    };
    let Some(task) = task else {
        return Ok(false);
    };
    let task_prompt = format!("{}\n\n{}", task.title, task.description)
        .trim()
        .to_owned();
    let task_id = task.id.clone();
    let start_result = match task.provider.as_str() {
        "copilot" => start_acp_session_blocking(
            AcpProvider::Copilot,
            Some(project_id.clone()),
            task_prompt,
            None,
            Some(task_id.clone()),
            true,
            database.clone(),
            processes,
            app.clone(),
        )
        .map(|_| ()),
        "antigravity" => ensure_agent_provider_enabled(&database, "antigravity").and_then(|_| {
            start_acp_session_blocking(
                AcpProvider::Antigravity,
                Some(project_id.clone()),
                task_prompt,
                None,
                Some(task_id.clone()),
                true,
                database.clone(),
                processes,
                app.clone(),
            )
            .map(|_| ())
        }),
        "claudeCode" => ensure_agent_provider_enabled(&database, "claudeCode").and_then(|_| {
            start_claude_code_session_blocking(
                Some(project_id.clone()),
                task_prompt,
                None,
                Some(task_id.clone()),
                true,
                database.clone(),
                processes,
                app.clone(),
            )
            .map(|_| ())
        }),
        provider => Err(format!(
            "Unsupported agent provider on project task: {provider}"
        )),
    };
    if let Err(error) = start_result {
        let reset_result = database
            .0
            .lock()
            .map_err(|_| "Project database is unavailable.".to_owned())
            .and_then(|connection| {
                connection
                    .execute(
                        "UPDATE project_tasks SET status = 'backlog', session_id = NULL,
                         updated_at = CURRENT_TIMESTAMP WHERE id = ?1 AND project_id = ?2
                           AND status = 'in_progress'",
                        params![task_id, project_id],
                    )
                    .map(|_| ())
                    .map_err(|reset_error| format!("Unable to reset the loop task: {reset_error}"))
            });
        return match reset_result {
            Ok(()) => Err(error),
            Err(reset_error) => Err(format!("{error}; additionally, {reset_error}")),
        };
    }
    Ok(true)
}

struct AptStatusReport {
    state: String,
    note: String,
    gate_passed: Option<bool>,
}

fn parse_apt_status_report(text: &str) -> Option<AptStatusReport> {
    let sections = text.split("```").collect::<Vec<_>>();
    for section in sections
        .iter()
        .enumerate()
        .filter(|(index, _)| index % 2 == 1)
        .map(|(_, section)| *section)
        .rev()
    {
        let section = section.trim();
        let Some(separator) = section.find(char::is_whitespace) else {
            continue;
        };
        let language = &section[..separator];
        let json = section[separator..].trim();
        if !language.trim().eq_ignore_ascii_case("apt-status") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(json) else {
            continue;
        };
        let Some(state) = value
            .get("state")
            .and_then(Value::as_str)
            .filter(|state| matches!(*state, "working" | "question" | "blocked" | "complete"))
        else {
            continue;
        };
        return Some(AptStatusReport {
            state: state.to_owned(),
            note: value
                .get("note")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned(),
            gate_passed: value.get("gatePassed").and_then(Value::as_bool),
        });
    }
    None
}

pub(crate) fn handle_project_task_turn(
    database: &Arc<Mutex<Connection>>,
    session_id: &str,
    processes: &SessionProcesses,
    app: &tauri::AppHandle,
) {
    let result = (|| -> Result<
        (
            Option<(String, bool)>,
            Option<(String, String, String)>,
            Option<TaskResultNotificationEvent>,
        ),
        String,
    > {
            let connection = database
                .lock()
                .map_err(|_| "Project database is unavailable.".to_owned())?;
            let (project_id, looping, agent_task_creation, task) = connection
                .query_row(
                    "SELECT s.project_id, p.looping, p.agent_task_creation,
                        t.loop_enabled, t.step_budget, t.steps_used, t.auto_approve
                 FROM agent_sessions s
                 JOIN projects p ON p.id = s.project_id
                 LEFT JOIN project_tasks t ON t.session_id = s.id
                 WHERE s.id = ?1",
                    [session_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)? != 0,
                            row.get::<_, i64>(2)? != 0,
                            match row.get::<_, Option<i64>>(3)? {
                                Some(enabled) => Some((
                                    enabled != 0,
                                    row.get::<_, i64>(4)?,
                                    row.get::<_, i64>(5)?,
                                    row.get::<_, i64>(6)? != 0,
                                )),
                                None => None,
                            },
                        ))
                    },
                )
                .map_err(|error| format!("Unable to read task session settings: {error}"))?;
            let assistant_text = connection
                .query_row(
                    "SELECT content FROM session_messages
                 WHERE session_id = ?1 AND role = 'assistant'
                 ORDER BY sequence DESC LIMIT 1",
                    [session_id],
                    |row| row.get::<_, String>(0),
                )
                .map_err(|error| format!("Unable to read the completed agent response: {error}"))?;
            let report = parse_apt_status_report(&assistant_text);
            if agent_task_creation {
                create_agent_task_proposals(&connection, session_id)?;
            }

            let mut schedule_loop = false;
            let task_requires_review =
                task.is_some_and(|(task_loop_enabled, _, _, auto_approve)| {
                    !auto_approve && !(looping && task_loop_enabled)
                });
            let task_result_event = match report.as_ref() {
                Some(report) if report.state == "blocked" => {
                    task_result_notification_event(&connection, session_id, false)?
                }
                Some(report)
                    if report.state == "complete" && report.gate_passed == Some(true) =>
                {
                    task_result_notification_event(&connection, session_id, true)?
                }
                _ => None,
            };
            if let (Some((task_loop_enabled, _, _, _)), Some(report)) = (task, report.as_ref()) {
                match report.state.as_str() {
                    "complete" if report.gate_passed == Some(true) => {
                        finish_project_task_session(&connection, session_id, true)?;
                        schedule_loop = looping && task_loop_enabled;
                    }
                    "blocked" => {
                        finish_project_task_session(&connection, session_id, false)?;
                    }
                    _ => {}
                }
            }

            let status_update = match report.as_ref() {
                Some(report) => {
                    let update = match report.state.as_str() {
                        "question" => Some((
                            "waiting",
                            "Agent needs your direction",
                            if report.note.is_empty() {
                                "The agent needs a decision to continue."
                            } else {
                                report.note.as_str()
                            },
                        )),
                        "blocked" => Some((
                            "failed",
                            "Agent is blocked",
                            if report.note.is_empty() {
                                "The agent could not complete the task."
                            } else {
                                report.note.as_str()
                            },
                        )),
                        "complete" if report.gate_passed != Some(true) => Some((
                            "waiting",
                            "Agent completion needs review",
                            if report.note.is_empty() {
                                "The agent did not confirm that its quality checks passed."
                            } else {
                                report.note.as_str()
                            },
                        )),
                        "complete" if report.gate_passed == Some(true) => {
                            Some(if task_requires_review {
                                (
                                    "completed",
                                    "Task ready for review",
                                    "The agent completed the task and passed its checks. Review the result, then mark the task done.",
                                )
                            } else {
                                (
                                    "completed",
                                    "Task complete",
                                    "The agent completed the task and passed its checks.",
                                )
                            })
                        }
                        _ if task.is_some_and(|(_, budget, used, _)| used >= budget) => Some((
                            "waiting",
                            "Task step budget reached",
                            "Increase the task's step budget to continue this session.",
                        )),
                        "working" if !report.note.is_empty() => {
                            Some(("idle", report.note.as_str(), ""))
                        }
                        _ => None,
                    };
                    update.map(|(status, reason, detail)| {
                        (status.to_owned(), reason.to_owned(), detail.to_owned())
                    })
                }
                None if task.is_some_and(|(_, budget, used, _)| used >= budget) => Some((
                    "waiting".to_owned(),
                    "Task step budget reached".to_owned(),
                    "Increase the task's step budget to continue this session.".to_owned(),
                )),
                None => None,
            };
            Ok((
                schedule_loop.then_some((project_id, looping)),
                status_update,
                task_result_event,
            ))
        })();

    match result {
        Ok((schedule, status_update, task_result_event)) => {
            let mut status_updated = true;
            if let Some((status, reason, detail)) = status_update {
                if let Err(error) =
                    update_session_status(database, session_id, &status, &reason, &detail)
                {
                    status_updated = false;
                    let _ = app.emit("project-loop-error", error);
                }
            }
            if status_updated
                && let Some(event) = task_result_event
                && let Err(error) = emit_task_result_notification(app, event)
            {
                let _ = app.emit("project-loop-error", error);
            }
            if let Some((project_id, true)) = schedule {
                if let Err(error) = maybe_start_next_loop_task(
                    project_id,
                    Database(database.clone()),
                    SessionProcesses(processes.0.clone()),
                    app.clone(),
                ) {
                    let _ = app.emit("project-loop-error", error);
                }
            }
        }
        Err(error) => {
            let _ = app.emit("project-loop-error", error);
        }
    }
}

pub(crate) fn fail_project_task(database: &Arc<Mutex<Connection>>, session_id: &str, app: &tauri::AppHandle) {
    let result = (|| -> Result<Option<TaskResultNotificationEvent>, String> {
        let connection = database
            .lock()
            .map_err(|_| "Project database is unavailable.".to_owned())?;
        let event = task_result_notification_event(&connection, session_id, false)?;
        finish_project_task_session(&connection, session_id, false)?;
        Ok(event)
    })();
    match result {
        Ok(Some(event)) => {
            if let Err(error) = emit_task_result_notification(app, event) {
                let _ = app.emit("project-loop-error", error);
            }
        }
        Ok(None) => {}
        Err(error) => {
            let _ = app.emit("project-loop-error", error);
        }
    }
}

pub(crate) fn list_agent_sessions_from_connection(
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
                    s.current_step, s.started_at, s.token_count, s.token_usage_available,
                   s.context_window_size, s.permission_mode, s.origin, s.last_event, s.archived,
                   s.status_reason, s.updated_at, s.external_session_id,
                   EXISTS(
                     SELECT 1 FROM project_tasks t
                     WHERE t.session_id = s.id AND t.status = 'done'
                   ),
                   EXISTS(
                     SELECT 1 FROM project_tasks t
                     WHERE t.session_id = s.id AND t.archived = 1
                   ),
                   s.project_id,
                   (SELECT t.id FROM project_tasks t
                    WHERE t.session_id = s.id
                    ORDER BY t.updated_at DESC, t.id
                    LIMIT 1),
                   s.worktree_path
             FROM agent_sessions s
             JOIN projects p ON p.id = s.project_id
             WHERE p.connected = 1 AND (?1 IS NULL OR s.project_id = ?1)
               AND (?2 IS NULL OR s.status = ?2)
               AND s.archived = COALESCE(?3, 0)
             ORDER BY s.updated_at DESC, s.started_at DESC, s.id DESC",
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

fn load_session_transcript_from_connection(
    connection: &Connection,
    session_id: &str,
    full: bool,
) -> Result<Vec<TranscriptMessage>, String> {
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_sessions WHERE id = ?1)",
            [session_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Unable to verify agent session: {error}"))?;
    if !exists {
        return Err("Agent session was not found.".to_owned());
    }
    let mut statement = connection
        .prepare(
            "SELECT role, content, created_at, sequence
             FROM session_messages
             WHERE session_id = ?1
               AND (role = 'user' OR content <> '')",
        )
        .map_err(|error| format!("Unable to query local session transcript: {error}"))?;
    let mut entries = statement
        .query_map([session_id], |row| {
            Ok(TranscriptMessage {
                role: row.get(0)?,
                content: row.get(1)?,
                timestamp: row.get(2)?,
                event_type: None,
                status: None,
                details: None,
                sequence: row.get(3)?,
            })
        })
        .map_err(|error| format!("Unable to read local session transcript: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Unable to decode local session transcript: {error}"))?;
    if full {
        let mut statement = connection
            .prepare(
                "SELECT event_type, payload, observed_at, sequence
                 FROM session_events WHERE session_id = ?1",
            )
            .map_err(|error| format!("Unable to query local session activity: {error}"))?;
        let activity_rows = statement
            .query_map([session_id], |row| {
                let event_type = row.get::<_, String>(0)?;
                let payload = row.get::<_, String>(1)?;
                let timestamp = row.get::<_, String>(2)?;
                let sequence = row.get::<_, i64>(3)?;
                Ok((event_type, payload, timestamp, sequence))
            })
            .map_err(|error| format!("Unable to read local session activity: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Unable to decode local session activity: {error}"))?;
        for (event_type, payload, timestamp, sequence) in activity_rows {
            let payload: Value = serde_json::from_str(&payload).map_err(|error| {
                format!("Unable to decode local session activity payload: {error}")
            })?;
            let Some(label) = payload.get("label").and_then(Value::as_str) else {
                continue;
            };
            entries.push(TranscriptMessage {
                role: "activity".to_owned(),
                content: label.to_owned(),
                timestamp,
                event_type: Some(event_type),
                status: payload
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                details: payload
                    .get("details")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                sequence,
            });
        }
    }
    entries.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then(left.sequence.cmp(&right.sequence))
    });
    if !full && entries.len() > 100 {
        entries.drain(..entries.len() - 100);
    }
    Ok(entries)
}

#[tauri::command]
fn get_session_transcript(
    session_id: String,
    full: Option<bool>,
    database: State<'_, Database>,
) -> Result<Vec<TranscriptMessage>, String> {
    let connection = database
        .0
        .lock()
        .map_err(|_| "Agent session database is unavailable.".to_owned())?;
    load_session_transcript_from_connection(&connection, &session_id, full.unwrap_or(false))
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
        let provider: String = database
            .0
            .lock()
            .map_err(|_| "Agent session database is unavailable.".to_owned())?
            .query_row(
                "SELECT provider FROM agent_sessions WHERE id = ?1 AND origin = 'managed'",
                [&session_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Unable to find managed agent session: {error}"))?;
        if provider == "claudeCode" {
            return send_claude_message(&database, &session_id, &message, &app, &processes);
        }
        let acp_provider = AcpProvider::from_id(&provider)
            .ok_or_else(|| format!("Unsupported agent provider: {provider}"))?;
        let control = processes
            .0
            .lock()
            .map_err(|_| "Agent session process list is unavailable.".to_owned())?
            .get(&session_id)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "This session is not connected to a managed {} process.",
                    acp_provider.short_name()
                )
            })?;
        send_acp_session_message(
            acp_provider,
            &database.0,
            &session_id,
            &control,
            &message,
            &app,
            &processes,
            false,
        )
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
        .map_err(|_| "Agent permission state is unavailable.".to_owned())?
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
        .ok_or_else(|| "This session is not connected to a managed agent process.".to_owned())?;
    let acp_provider = AcpProvider::from_id(&control.provider).unwrap_or(AcpProvider::Copilot);
    let short_name = acp_provider.short_name();
    let (protocol_id, permission) = control
        .permission_requests
        .lock()
        .map_err(|_| format!("{short_name} permission state is unavailable."))?
        .get(&request_id)
        .cloned()
        .ok_or_else(|| format!("This {short_name} permission request is no longer active."))?;
    let outcome = permission_outcome(acp_provider, &permission, option_id.as_deref())?;
    send_acp_message(
        acp_provider,
        &control.input,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": protocol_id,
            "result": { "outcome": outcome }
        }),
    )?;
    capture_session_activity(
        &database.0,
        &app,
        &session_id,
        &TranscriptActivity {
            event_type: "permission_decision".to_owned(),
            label: if option_id.is_some() {
                format!("Permission allowed: {}", permission.title)
            } else {
                format!("Permission denied: {}", permission.title)
            },
            status: if option_id.is_some() {
                "allowed".to_owned()
            } else {
                "denied".to_owned()
            },
            details: serde_json::json!({
                "optionId": option_id,
                "outcome": outcome
            }),
        },
    );
    control
        .permission_requests
        .lock()
        .map_err(|_| format!("{short_name} permission state is unavailable."))?
        .remove(&request_id);
    update_session_status(
        &database.0,
        &session_id,
        "running",
        &format!("{short_name} is continuing"),
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
    if let Err(error) = control
        .child
        .lock()
        .map_err(|_| "Agent process state is unavailable.".to_owned())
        .and_then(|mut child| {
            terminate_agent_process(&mut child)
                .map_err(|error| format!("Unable to stop agent session: {error}"))
        })
    {
        control.stop_requested.store(false, Ordering::Relaxed);
        return Err(error);
    }
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

#[tauri::command]
fn rename_agent_session(
    session_id: String,
    name: String,
    database: State<'_, Database>,
) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Session name cannot be empty.".to_owned());
    }
    if name.len() > 500 {
        return Err("Session name must be 500 bytes or fewer.".to_owned());
    }

    let connection = database
        .0
        .lock()
        .map_err(|_| "Agent session database is unavailable.".to_owned())?;
    let changed = connection
        .execute(
            "UPDATE agent_sessions
             SET task = ?2, updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1",
            params![session_id, name],
        )
        .map_err(|error| format!("Unable to rename agent session: {error}"))?;
    if changed == 0 {
        return Err("Agent session was not found.".to_owned());
    }
    Ok(())
}

#[tauri::command]
async fn start_copilot_session(
    project_id: Option<String>,
    task: String,
    resume_session_id: Option<String>,
    task_id: Option<String>,
    database: State<'_, Database>,
    processes: State<'_, SessionProcesses>,
    app: tauri::AppHandle,
) -> Result<AgentSession, String> {
    let database = Database(database.0.clone());
    let processes = SessionProcesses(processes.0.clone());
    run_agent_operation(move || {
        start_acp_session_blocking(
            AcpProvider::Copilot,
            project_id,
            task,
            resume_session_id,
            task_id,
            false,
            database,
            processes,
            app,
        )
    })
    .await
}

#[tauri::command]
async fn start_agent_session(
    provider: String,
    project_id: Option<String>,
    task: String,
    resume_session_id: Option<String>,
    task_id: Option<String>,
    database: State<'_, Database>,
    processes: State<'_, SessionProcesses>,
    app: tauri::AppHandle,
) -> Result<AgentSession, String> {
    let database = Database(database.0.clone());
    let processes = SessionProcesses(processes.0.clone());
    run_agent_operation(move || {
        if resume_session_id.is_none() {
            ensure_agent_provider_enabled(&database, &provider)?;
        }

        match provider.as_str() {
            "copilot" => start_acp_session_blocking(
                AcpProvider::Copilot,
                project_id,
                task,
                resume_session_id,
                task_id,
                false,
                database,
                processes,
                app,
            ),
            "antigravity" => start_acp_session_blocking(
                AcpProvider::Antigravity,
                project_id,
                task,
                resume_session_id,
                task_id,
                false,
                database,
                processes,
                app,
            ),
            "claudeCode" => start_claude_code_session_blocking(
                project_id,
                task,
                resume_session_id,
                task_id,
                false,
                database,
                processes,
                app,
            ),
            _ => Err(format!("Unsupported agent provider: {provider}")),
        }
    })
    .await
}

fn provider_label(provider: &str) -> &'static str {
    match provider {
        "copilot" => "GitHub Copilot CLI",
        "claudeCode" => "Claude Code CLI",
        "antigravity" => "Antigravity CLI",
        _ => "Agent CLI",
    }
}

fn ensure_agent_provider_enabled(database: &Database, provider: &str) -> Result<(), String> {
    let settings_provider = match provider {
        "copilot" => "copilot",
        "claudeCode" => "claude_code",
        "antigravity" => "antigravity",
        _ => return Err(format!("Unsupported agent provider: {provider}")),
    };
    let enabled = {
        let connection = database
            .0
            .lock()
            .map_err(|_| "Agent session database is unavailable.".to_owned())?;
        read_tool_settings(&connection, settings_provider)
            .map(|settings| settings.enabled)
            .unwrap_or(false)
    };
    if enabled {
        Ok(())
    } else {
        Err(format!(
            "{} is disabled. Enable it in Settings before starting a session.",
            provider_label(provider)
        ))
    }
}

pub(crate) fn reset_task_after_start_failure(
    database: &Database,
    task_id: Option<&str>,
) -> Result<(), String> {
    let Some(task_id) = task_id else {
        return Ok(());
    };
    let connection = database
        .0
        .lock()
        .map_err(|_| "Project database is unavailable.".to_owned())?;
    connection
        .execute(
            "UPDATE project_tasks SET status = 'backlog', session_id = NULL,
             updated_at = CURRENT_TIMESTAMP WHERE id = ?1 AND status = 'in_progress'",
            [task_id],
        )
        .map_err(|error| {
            format!("Unable to reset the task after session startup failed: {error}")
        })?;
    Ok(())
}

fn claude_session_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("claude-{timestamp:x}")
}

fn check_claude_cli() -> Result<(), String> {
    let version = claude_command()
        .arg("--version")
        .output()
        .map_err(|error| format!("Unable to start Claude Code CLI: {error}"))?;
    if !version.status.success() {
        let detail = String::from_utf8_lossy(&version.stderr).trim().to_owned();
        return Err(if detail.is_empty() {
            "Claude Code CLI rejected the version check.".to_owned()
        } else {
            format!("Claude Code CLI rejected the version check: {detail}")
        });
    }
    Ok(())
}

fn start_claude_code_session_blocking(
    project_id: Option<String>,
    task: String,
    resume_session_id: Option<String>,
    task_id: Option<String>,
    loop_owned: bool,
    database: Database,
    processes: SessionProcesses,
    app: tauri::AppHandle,
) -> Result<AgentSession, String> {
    check_claude_cli()?;
    if let Some(session_id) = resume_session_id.as_deref() {
        if processes
            .0
            .lock()
            .map_err(|_| "Agent session process list is unavailable.".to_owned())?
            .contains_key(session_id)
        {
            return Err("This Claude Code session is already connected.".to_owned());
        }
        let (project_id, external_id, task): (String, Option<String>, String) = {
            let connection = database
                .0
                .lock()
                .map_err(|_| "Agent session database is unavailable.".to_owned())?;
            let (project_id, external_id, stored_task): (String, Option<String>, String) =
                connection
                    .query_row(
                        "SELECT project_id, external_session_id, task
                     FROM agent_sessions
                     WHERE id = ?1 AND provider = 'claudeCode'",
                        [session_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .map_err(|error| {
                        format!("Unable to find Claude Code session to resume: {error}")
                    })?;
            (
                project_id,
                external_id,
                if task.trim().is_empty() {
                    stored_task
                } else {
                    task
                },
            )
        };
        if external_id.is_none() {
            return Err(
                "Claude Code did not return a session ID, so this session cannot be resumed."
                    .to_owned(),
            );
        }
        {
            let connection = database
                .0
                .lock()
                .map_err(|_| "Agent session database is unavailable.".to_owned())?;
            connection
                .execute(
                    "UPDATE agent_sessions
                     SET project_id = ?2, task = ?3, status = 'idle',
                         status_reason = '', current_step = 'Ready for your message',
                         origin = 'managed', archived = 0, updated_at = CURRENT_TIMESTAMP
                     WHERE id = ?1 AND provider = 'claudeCode'",
                    params![session_id, project_id, task],
                )
                .map_err(|error| format!("Unable to resume Claude Code session: {error}"))?;
        }
        app.emit("agent-session-updated", session_id)
            .map_err(|error| format!("Unable to publish agent session update: {error}"))?;
        let connection = database
            .0
            .lock()
            .map_err(|_| "Agent session database is unavailable.".to_owned())?;
        return list_agent_sessions_from_connection(&connection, None, None, Some(false))?
            .into_iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| {
                "Claude Code session was resumed but could not be read back.".to_owned()
            });
    }

    let project_id = project_id
        .ok_or_else(|| "A project is required to start a Claude Code session.".to_owned())?;
    let mut task = task.trim().to_owned();
    if task.is_empty() {
        return Err("Describe the task before starting a Claude Code session.".to_owned());
    }
    if task.len() > 16_000 {
        return Err("Messages must be 16,000 bytes or fewer.".to_owned());
    }
    let (mut project_path, mut branch, allow_agent_tasks) = {
        let connection = database
            .0
            .lock()
            .map_err(|_| "Project database is unavailable.".to_owned())?;
        connection
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
            .map_err(|error| format!("Unable to find project for Claude Code session: {error}"))?
    };
    let (permission_mode, model, use_worktree) = if let Some(task_id) = task_id.as_deref() {
        let connection = database
            .0
            .lock()
            .map_err(|_| "Project database is unavailable.".to_owned())?;
        let (provider, mode, model, use_worktree): (String, String, String, bool) = connection
            .query_row(
                "SELECT provider, mode, model, use_worktree FROM project_tasks
                 WHERE id = ?1 AND project_id = ?2",
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
        if provider != "claudeCode" {
            return Err("The task is assigned to a different agent.".to_owned());
        }
        (mode, model, use_worktree)
    } else {
        ("plan".to_owned(), "auto".to_owned(), false)
    };
    let mut worktree_path = None;
    if allow_agent_tasks {
        task.push_str(
            "\n\n# Agent-proposed backlog tasks\nThis project allows you to propose separate follow-up work. When useful, add a fenced `apt-task` JSON block with a short `title`, a `purpose`, and optional `acceptanceCriteria` string array. Example:\n```apt-task\n{\"title\":\"Add example\",\"purpose\":\"Describe the follow-up work\",\"acceptanceCriteria\":[\"A verifiable outcome\"]}\n```\nProposed tasks are added to the backlog and do not interrupt this session. Do not propose work that is part of the task you were asked to complete.",
        );
    }
    if task_id.is_some() {
        task.push_str(
            "\n\n# Task completion protocol\nEnd your response with one fenced apt-status JSON block. Use state `working` with a brief note when unfinished, `question` when you need a human decision, `blocked` when you cannot proceed, or `complete` when the task is done. A `complete` report must include `gatePassed: true` and a brief `gateSummary` only after all available relevant checks pass. Example: ```apt-status\n{\"state\":\"complete\",\"note\":\"Implemented and verified the task\",\"gatePassed\":true,\"gateSummary\":\"Tests passed\"}\n``` Do not report complete when checks fail or were not run.",
        );
    }

    if use_worktree {
        let task_id = task_id
            .as_deref()
            .ok_or_else(|| "Unable to find the task for its Git worktree.".to_owned())?;
        let (resolved_worktree_path, worktree_branch) =
            prepare_task_worktree(Path::new(&project_path), task_id)?;
        project_path = resolved_worktree_path.to_string_lossy().into_owned();
        branch = worktree_branch;
        worktree_path = Some(project_path.clone());
    }

    let session_id = claude_session_id();
    {
        let mut connection = database
            .0
            .lock()
            .map_err(|_| "Agent session database is unavailable.".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Unable to start session transaction: {error}"))?;
        transaction
            .execute(
                "INSERT INTO agent_sessions
                 (id, project_id, provider, task, branch, worktree_path, status, status_reason,
                  current_step, permission_mode, origin, loop_owned, last_event)
                 VALUES (?1, ?2, 'claudeCode', ?3, ?4, ?5, 'starting', '',
                     'Starting Claude Code session', ?6, 'managed', ?7, '')",
                params![
                    session_id,
                    project_id,
                    task,
                    branch,
                    worktree_path,
                    permission_mode,
                    loop_owned
                ],
            )
            .map_err(|error| format!("Unable to persist Claude Code session: {error}"))?;
        if let Some(task_id) = task_id.as_deref() {
            let changed = transaction
                .execute(
                    "UPDATE project_tasks SET status = 'in_progress', session_id = ?1,
                     workspace_locked = 1,
                     updated_at = CURRENT_TIMESTAMP
                     WHERE id = ?2 AND project_id = ?3 AND provider = 'claudeCode' AND
                      ((status = 'backlog' AND ?4 = 0) OR
                       (status = 'in_progress' AND session_id IS NULL AND ?4 = 1))",
                    params![session_id, task_id, project_id, loop_owned],
                )
                .map_err(|error| format!("Unable to link task to Claude Code session: {error}"))?;
            if changed != 1 {
                return Err("The task is no longer available to start.".to_owned());
            }
        }
        transaction
            .commit()
            .map_err(|error| format!("Unable to save Claude Code task session: {error}"))?;
    }

    let processes_ref = processes.0.clone();
    let result = start_claude_turn(
        &database.0,
        &processes_ref,
        &app,
        &session_id,
        &project_path,
        None,
        &permission_mode,
        (model != "auto").then_some(model.as_str()),
        &task,
        true,
    );
    if let Err(error) = result {
        let _ = update_session_status(
            &database.0,
            &session_id,
            "failed",
            "Unable to start Claude Code",
            &error,
        );
        reset_task_after_start_failure(&database, task_id.as_deref())
            .map_err(|cleanup_error| format!("{error}; additionally, {cleanup_error}"))?;
        return Err(error);
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
        .ok_or_else(|| "Claude Code session was started but could not be read back.".to_owned())
}

fn start_claude_turn(
    database: &Arc<Mutex<Connection>>,
    processes: &Arc<Mutex<HashMap<String, SessionControl>>>,
    app: &tauri::AppHandle,
    session_id: &str,
    project_path: &str,
    external_session_id: Option<&str>,
    permission_mode: &str,
    model: Option<&str>,
    prompt: &str,
    is_initial_prompt: bool,
) -> Result<mpsc::Receiver<Result<(), String>>, String> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err("Enter a message before sending it to Claude Code.".to_owned());
    }
    if prompt.len() > 16_000 {
        return Err("Messages must be 16,000 bytes or fewer.".to_owned());
    }
    let cli_permission_mode = match permission_mode {
        "plan" => "plan",
        "acceptEdits" => "acceptEdits",
        "auto" => "bypassPermissions",
        other => return Err(format!("Unsupported Claude Code permission mode: {other}")),
    };
    if processes
        .lock()
        .map_err(|_| "Agent session process list is unavailable.".to_owned())?
        .contains_key(session_id)
    {
        return Err("This agent session already has an active process.".to_owned());
    }

    let assistant_message_id = {
        let connection = database
            .lock()
            .map_err(|_| "Agent session database is unavailable.".to_owned())?;
        let status: String = connection
            .query_row(
                "SELECT status FROM agent_sessions WHERE id = ?1 AND provider = 'claudeCode'",
                [session_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Unable to find Claude Code session: {error}"))?;
        validate_session_message_status(&status, is_initial_prompt, false)?;
        insert_session_message(&connection, session_id, "user", prompt)?;
        let assistant_message_id =
            insert_session_message(&connection, session_id, "assistant", "")?;
        connection
            .execute(
                "UPDATE agent_sessions
                 SET status = 'running', status_reason = '',
                     current_step = 'Sending message', last_event = ?2,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1",
                params![session_id, prompt],
            )
            .map_err(|error| format!("Unable to update Claude Code session: {error}"))?;
        assistant_message_id
    };

    let mut command = claude_command();
    configure_agent_process(&mut command);
    command
        .arg("--print")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--permission-mode")
        .arg(cli_permission_mode);
    if let Some(external_session_id) = external_session_id {
        command.arg("--resume").arg(external_session_id);
    }
    if let Some(model) = model.filter(|model| *model != "auto") {
        command.arg("--model").arg(model);
    }
    let mut child = match command
        .arg(prompt)
        .current_dir(project_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let message = format!("Unable to start Claude Code CLI: {error}");
            let _ = update_session_status(
                database,
                session_id,
                "failed",
                "Unable to start Claude Code",
                &message,
            );
            return Err(message);
        }
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Claude Code did not provide an output stream.".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Claude Code did not provide an error stream.".to_owned())?;
    let child = Arc::new(Mutex::new(child));
    let (input, _input_rx) = mpsc::channel::<String>();
    let control = SessionControl {
        provider: "claudeCode".to_owned(),
        input,
        pending: Arc::new(Mutex::new(HashMap::new())),
        permission_requests: Arc::new(Mutex::new(HashMap::new())),
        next_request_id: Arc::new(AtomicU64::new(1)),
        stop_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        replaying_history: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        child: child.clone(),
        assistant_message_id: Arc::new(Mutex::new(Some(assistant_message_id.clone()))),
    };
    let pid = child
        .lock()
        .map_err(|_| "Claude Code process state is unavailable.".to_owned())?
        .id();
    {
        let connection = database
            .lock()
            .map_err(|_| "Agent session database is unavailable.".to_owned())?;
        connection
            .execute(
                "UPDATE agent_sessions SET pid = ?2 WHERE id = ?1",
                params![session_id, pid],
            )
            .map_err(|error| format!("Unable to save Claude Code process ID: {error}"))?;
    }
    processes
        .lock()
        .map_err(|_| "Agent session process list is unavailable.".to_owned())?
        .insert(session_id.to_owned(), control.clone());

    let (done_sender, done_receiver) = mpsc::channel();
    let reader_database = database.clone();
    let reader_processes = processes.clone();
    let reader_app = app.clone();
    let reader_session_id = session_id.to_owned();
    thread::spawn(move || {
        let stderr_reader = thread::spawn(move || {
            let mut output = String::new();
            let read_result = BufReader::new(stderr).read_to_string(&mut output);
            (read_result, output)
        });
        let mut stream_error = None;
        let mut result_text = None;
        let mut external_id = None;
        let mut got_result = false;
        let mut failed_result = false;
        let mut saw_text = false;
        for line in BufReader::new(stdout).lines() {
            let line = match line {
                Ok(line) => line,
                Err(error) => {
                    stream_error = Some(format!("Unable to read Claude Code output: {error}"));
                    break;
                }
            };
            let observation = match claude_observation_from_line(&line) {
                Ok(observation) => observation,
                Err(error) => {
                    capture_session_activity(
                        &reader_database,
                        &reader_app,
                        &reader_session_id,
                        &TranscriptActivity {
                            event_type: "error".to_owned(),
                            label: error.clone(),
                            status: "failed".to_owned(),
                            details: serde_json::json!({ "line": line }),
                        },
                    );
                    stream_error = Some(error);
                    continue;
                }
            };
            for activity in &observation.activities {
                capture_session_activity(
                    &reader_database,
                    &reader_app,
                    &reader_session_id,
                    activity,
                );
            }
            if let Some(id) = observation.session_id {
                external_id = Some(id);
            }
            if let Some(step) = observation.step {
                let _ = update_session_status(
                    &reader_database,
                    &reader_session_id,
                    "running",
                    &step,
                    "",
                );
            }
            if let Some(text) = observation.text {
                if !text.is_empty() {
                    saw_text = true;
                    if let Err(error) = append_assistant_chunk(
                        &reader_database,
                        &reader_session_id,
                        &assistant_message_id,
                        &text,
                    ) {
                        let _ = reader_app.emit(
                            "agent-session-activity-error",
                            serde_json::json!({
                                "sessionId": reader_session_id,
                                "error": error
                            }),
                        );
                    }
                    let _ = reader_app.emit("agent-session-updated", &reader_session_id);
                }
            }
            if observation.completed {
                got_result = true;
                failed_result = observation.failed;
                result_text = observation.result;
                if let Some(token_count) = observation.token_count {
                    let _ = update_session_token_count(
                        &reader_database,
                        &reader_session_id,
                        token_count,
                        None,
                    );
                }
            }
        }

        let stderr = match stderr_reader.join() {
            Ok((Ok(_), output)) => output.trim().to_owned(),
            Ok((Err(error), _)) => {
                stream_error.get_or_insert_with(|| {
                    format!("Unable to read Claude Code error output: {error}")
                });
                String::new()
            }
            Err(_) => {
                stream_error.get_or_insert_with(|| {
                    "Claude Code error-output reader stopped unexpectedly.".to_owned()
                });
                String::new()
            }
        };
        if !stderr.is_empty() && (stream_error.is_some() || !got_result) {
            capture_session_activity(
                &reader_database,
                &reader_app,
                &reader_session_id,
                &TranscriptActivity {
                    event_type: "error".to_owned(),
                    label: "Claude Code reported an error".to_owned(),
                    status: "failed".to_owned(),
                    details: Value::String(stderr.clone()),
                },
            );
        }
        let exit_success = child
            .lock()
            .map_err(|_| "Claude Code process state is unavailable.".to_owned())
            .and_then(|mut process| {
                process
                    .wait()
                    .map(|status| status.success())
                    .map_err(|error| format!("Unable to wait for Claude Code: {error}"))
            });
        let stopped = control.stop_requested.load(Ordering::Relaxed);
        let mut failure = stream_error
            .or_else(|| exit_success.as_ref().err().cloned())
            .or_else(|| {
                if !exit_success.unwrap_or(false) {
                    Some(if stderr.is_empty() {
                        "Claude Code exited unsuccessfully.".to_owned()
                    } else {
                        stderr
                    })
                } else if !got_result {
                    Some("Claude Code exited without a result event.".to_owned())
                } else if failed_result {
                    Some(result_text.clone().unwrap_or_else(|| {
                        "Claude Code reported that the request failed.".to_owned()
                    }))
                } else if external_id.is_none() {
                    Some("Claude Code did not return a session ID.".to_owned())
                } else {
                    None
                }
            });
        if let Some(external_id) = external_id.as_deref() {
            let persist_result = reader_database
                .lock()
                .map_err(|_| "Agent session database is unavailable.".to_owned())
                .and_then(|connection| {
                    connection
                        .execute(
                            "UPDATE agent_sessions SET external_session_id = ?2 WHERE id = ?1",
                            params![reader_session_id, external_id],
                        )
                        .map(|_| ())
                        .map_err(|error| format!("Unable to save Claude Code session ID: {error}"))
                });
            if let Err(error) = persist_result {
                if let Some(existing) = failure.as_mut() {
                    existing.push_str(&format!("; additionally, {error}"));
                } else {
                    failure = Some(error);
                }
            }
        }
        if !saw_text {
            if let Some(text) = result_text.as_deref().filter(|text| !text.is_empty()) {
                if let Err(error) = append_assistant_chunk(
                    &reader_database,
                    &reader_session_id,
                    &assistant_message_id,
                    text,
                ) {
                    let _ = reader_app.emit(
                        "agent-session-activity-error",
                        serde_json::json!({
                            "sessionId": reader_session_id,
                            "error": error
                        }),
                    );
                }
            }
        }

        let (status, step, reason) = if stopped {
            ("completed", "Stopped by user.", "Stopped by user.")
        } else if let Some(error) = failure.as_deref() {
            ("failed", "Claude Code request failed", error)
        } else {
            ("idle", "Ready for your message", "")
        };
        if let Some(error) = failure.as_deref() {
            capture_session_activity(
                &reader_database,
                &reader_app,
                &reader_session_id,
                &TranscriptActivity {
                    event_type: "error".to_owned(),
                    label: "Claude Code session failed".to_owned(),
                    status: "failed".to_owned(),
                    details: Value::String(error.to_owned()),
                },
            );
        }
        capture_session_activity(
            &reader_database,
            &reader_app,
            &reader_session_id,
            &TranscriptActivity {
                event_type: "turn_update".to_owned(),
                label: "Claude Code turn ended".to_owned(),
                status: if stopped {
                    "stopped".to_owned()
                } else if failure.is_some() {
                    "failed".to_owned()
                } else {
                    "completed".to_owned()
                },
                details: serde_json::json!({
                    "resultReceived": got_result,
                    "textReceived": saw_text
                }),
            },
        );
        let _ = update_session_status(&reader_database, &reader_session_id, status, step, reason);
        if let Ok(mut active) = reader_processes.lock() {
            active.remove(&reader_session_id);
        }
        if failure.is_none() && !stopped {
            if let Ok(connection) = reader_database.lock() {
                let _ = increment_project_task_step(&connection, &reader_session_id);
            }
            handle_project_task_turn(
                &reader_database,
                &reader_session_id,
                &SessionProcesses(reader_processes),
                &reader_app,
            );
        } else {
            fail_project_task(&reader_database, &reader_session_id, &reader_app);
        }
        let _ = reader_app.emit("agent-session-updated", &reader_session_id);
        let _ = done_sender.send(if stopped {
            Ok(())
        } else if let Some(error) = failure {
            Err(error)
        } else {
            Ok(())
        });
    });
    Ok(done_receiver)
}

fn send_claude_message(
    database: &Database,
    session_id: &str,
    message: &str,
    app: &tauri::AppHandle,
    processes: &SessionProcesses,
) -> Result<(), String> {
    check_claude_cli()?;
    let (project_path, external_id, permission_mode, model) = {
        let connection = database
            .0
            .lock()
            .map_err(|_| "Agent session database is unavailable.".to_owned())?;
        connection
            .query_row(
                "SELECT COALESCE(s.worktree_path, p.path), s.external_session_id,
                    s.permission_mode,
                    COALESCE((SELECT t.model FROM project_tasks t WHERE t.session_id = s.id), 'auto')
                 FROM agent_sessions s
                 JOIN projects p ON p.id = s.project_id
                 WHERE s.id = ?1 AND s.provider = 'claudeCode' AND s.origin = 'managed'",
                [session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .map_err(|error| format!("Unable to read Claude Code session: {error}"))?
    };
    let external_id = external_id.ok_or_else(|| {
        "Claude Code did not return a session ID, so this session cannot receive messages."
            .to_owned()
    })?;
    let receiver = start_claude_turn(
        &database.0,
        &processes.0,
        app,
        session_id,
        &project_path,
        Some(&external_id),
        &permission_mode,
        (model != "auto").then_some(model.as_str()),
        message,
        false,
    )?;
    match receiver.recv_timeout(Duration::from_secs(600)) {
        Ok(result) => result,
        Err(_) => Err("Claude Code did not finish responding within 10 minutes.".to_owned()),
    }
}

#[tauri::command]
fn register_project(path: String, database: State<'_, Database>) -> Result<Project, String> {
    let project = project_for_directory(Path::new(path.trim()))?;

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
               connected = 1,
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

    connection
        .query_row(
            "SELECT id, name, path, branch, current_commit, git_state, looping, agent_task_creation
             FROM projects WHERE id = ?1",
            [&project.id],
            project_from_row,
        )
        .map_err(|error| format!("Unable to read the registered project: {error}"))
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
           looping INTEGER NOT NULL DEFAULT 0,
           agent_task_creation INTEGER NOT NULL DEFAULT 0,
           connected INTEGER NOT NULL DEFAULT 1,
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
           worktree_path TEXT,
           status TEXT NOT NULL,
           status_reason TEXT NOT NULL DEFAULT '',
           current_step TEXT NOT NULL DEFAULT '',
           token_count INTEGER NOT NULL DEFAULT 0,
           token_usage_available INTEGER NOT NULL DEFAULT 0,
           context_window_size INTEGER,
           permission_mode TEXT NOT NULL DEFAULT 'plan',
           origin TEXT NOT NULL DEFAULT 'observed',
           loop_owned INTEGER NOT NULL DEFAULT 0,
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
    connection
        .execute(
            "ALTER TABLE projects ADD COLUMN connected INTEGER NOT NULL DEFAULT 1",
            [],
        )
        .or_else(|error| {
            if error
                .to_string()
                .contains("duplicate column name: connected")
            {
                Ok(0)
            } else {
                Err(error)
            }
        })?;
    connection
        .execute(
            "ALTER TABLE projects ADD COLUMN looping INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .or_else(|error| {
            if error.to_string().contains("duplicate column name: looping") {
                Ok(0)
            } else {
                Err(error)
            }
        })?;
    connection
        .execute(
            "ALTER TABLE projects ADD COLUMN agent_task_creation INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .or_else(|error| {
            if error
                .to_string()
                .contains("duplicate column name: agent_task_creation")
            {
                Ok(0)
            } else {
                Err(error)
            }
        })?;
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
    connection
        .execute(
            "ALTER TABLE agent_sessions ADD COLUMN loop_owned INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .or_else(|error| {
            if error
                .to_string()
                .contains("duplicate column name: loop_owned")
            {
                Ok(0)
            } else {
                Err(error)
            }
        })?;
    connection
        .execute(
            "ALTER TABLE agent_sessions ADD COLUMN worktree_path TEXT",
            [],
        )
        .or_else(|error| {
            if error
                .to_string()
                .contains("duplicate column name: worktree_path")
            {
                Ok(0)
            } else {
                Err(error)
            }
        })?;
    connection
        .execute(
            "ALTER TABLE agent_sessions ADD COLUMN token_usage_available INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .or_else(|error| {
            if error
                .to_string()
                .contains("duplicate column name: token_usage_available")
            {
                Ok(0)
            } else {
                Err(error)
            }
        })?;
    connection
        .execute(
            "ALTER TABLE agent_sessions ADD COLUMN context_window_size INTEGER",
            [],
        )
        .or_else(|error| {
            if error
                .to_string()
                .contains("duplicate column name: context_window_size")
            {
                Ok(0)
            } else {
                Err(error)
            }
        })?;
    project_features::initialize_schema(connection)?;
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
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            app.manage(open_database(app.handle())?);
            app.manage(SessionProcesses(Arc::new(Mutex::new(HashMap::new()))));
            app.manage(TerminalManager::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_current_username,
            jira::import_jira_ticket,
            github_issue::configure_github_issue_host,
            github_issue::list_github_issue_hosts,
            github_issue::import_github_issue,
            list_projects,
            remove_project,
            set_project_automation,
            continue_project_loop,
            open_project_in_vscode,
            open_task_in_vscode,
            list_agent_sessions,
            send_session_message,
            get_pending_session_permission,
            respond_session_permission,
            stop_agent_session,
            set_session_archived,
            rename_agent_session,
            get_session_transcript,
            start_copilot_session,
            start_agent_session,
            register_project,
            list_project_tasks,
            create_project_task,
            update_project_task,
            set_project_task_status,
            set_project_task_archived,
            set_project_task_loop_enabled,
            link_project_task_session,
            delete_project_task,
            get_project_diff,
            list_project_git_refs,
            get_project_ref_diff,
            get_project_dependencies,
            list_project_skills,
            get_project_pipelines,
            list_project_terminals,
            create_project_terminal,
            write_project_terminal,
            resize_project_terminal,
            close_project_terminal,
            load_settings,
            save_settings,
            send_system_notification
        ])
        .run(tauri::generate_context!())
        .expect("error while running Agent Control Plane");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::{
        AcpObservation, acp_activity_from_message, acp_observation_from_line,
        acp_token_count_from_message, acp_usage_update_for_session,
    };

    #[test]
    fn vscode_launch_reports_missing_executable_and_unavailable_workspace() {
        let workspace = std::env::temp_dir();
        let missing = workspace.join(copilot_session_id()).join("missing-code");
        let error = launch_vscode(&workspace, vec![missing.clone()]).unwrap_err();
        assert!(error.contains("executable was not found"), "{error}");
        let error = launch_vscode(&missing, vec![]).unwrap_err();
        assert!(error.contains("workspace folder is unavailable"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn vscode_launch_passes_workspace_as_one_argument_and_checks_exit_status() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("vscode-{}", copilot_session_id()));
        let workspace = root.join("worktree with spaces");
        fs::create_dir_all(&workspace).expect("workspace");
        let executable = root.join("code");
        fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args\"\n",
        )
        .expect("fake VS Code CLI");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).expect("executable");
        launch_vscode(&workspace, vec![executable.clone()]).expect("successful launch");
        assert_eq!(
            fs::read_to_string(root.join("args")).expect("arguments"),
            format!(
                "--new-window\n{}\n",
                workspace.canonicalize().unwrap().display()
            )
        );

        fs::write(
            &executable,
            "#!/bin/sh\necho 'test launch failure' >&2\nexit 7\n",
        )
        .expect("failing VS Code CLI");
        let error = launch_vscode(&workspace, vec![executable.clone()]).unwrap_err();
        assert!(error.contains("VS Code failed to launch"), "{error}");
        assert!(error.contains("test launch failure"), "{error}");
        assert!(error.contains('7'), "{error}");

        fs::set_permissions(&executable, fs::Permissions::from_mode(0o644))
            .expect("not executable");
        let error = launch_vscode(&workspace, vec![executable.clone()]).unwrap_err();
        assert!(error.contains("VS Code failed to launch"), "{error}");
        let error = launch_vscode(&executable, vec![]).unwrap_err();
        assert!(error.contains("not a directory"), "{error}");
        fs::remove_dir_all(&root).expect("remove VS Code test directory");
    }

    #[test]
    fn copilot_executable_preserves_path_precedence_over_user_install() {
        let home = std::env::temp_dir().join(format!(
            "agent-control-plane-copilot-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        let executable_name = if cfg!(windows) {
            "copilot.cmd"
        } else {
            "copilot"
        };
        let executable = home.join(".local/bin").join(executable_name);
        let path_shim = home.join("app-path").join(executable_name);
        fs::create_dir_all(executable.parent().expect("copilot parent"))
            .expect("create user-local bin");
        fs::create_dir_all(path_shim.parent().expect("shim parent")).expect("create app PATH");
        fs::write(&executable, "").expect("create copilot fixture");
        fs::write(&path_shim, "").expect("create app PATH shim");

        let result = copilot_executable_from(
            None,
            Some(path_shim.parent().expect("shim parent").as_os_str()),
            Some(&home),
        );
        let fallback = copilot_executable_from(None, None, Some(&home));

        fs::remove_dir_all(&home).expect("remove copilot fixture");
        assert_eq!(result, Some(path_shim));
        assert_eq!(fallback, Some(executable));
    }

    #[test]
    fn non_git_directories_can_be_registered_as_projects() {
        let directory = std::env::temp_dir().join(format!(
            "agent-control-plane-folder-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("create project folder");

        let project = project_for_directory(&directory).expect("inspect non-Git folder");
        let canonical_directory = fs::canonicalize(&directory).expect("canonical project folder");

        assert_eq!(
            project.path,
            canonical_directory.to_string_lossy().to_string()
        );
        assert_eq!(
            project.name,
            directory.file_name().unwrap().to_string_lossy().to_string()
        );
        assert_eq!(project.git_state, "not-git");
        assert!(project.branch.is_empty());
        assert!(project.current_commit.is_empty());
        fs::remove_dir_all(&directory).expect("remove project folder");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stopping_agent_process_terminates_its_process_group() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 & echo $!; wait"]);
        command.stdout(Stdio::piped()).stderr(Stdio::null());
        configure_agent_process(&mut command);
        let mut child = command.spawn().expect("start agent process group");
        let mut descendant_pid = String::new();
        BufReader::new(child.stdout.take().expect("agent stdout"))
            .read_line(&mut descendant_pid)
            .expect("read descendant PID");
        let descendant_pid: u32 = descendant_pid.trim().parse().expect("parse descendant PID");

        terminate_agent_process(&mut child).expect("terminate agent process group");
        child.wait().expect("wait for agent process");

        for _ in 0..50 {
            let running = fs::read_to_string(format!("/proc/{descendant_pid}/stat"))
                .ok()
                .and_then(|stat| {
                    stat.rsplit_once(')')
                        .map(|(_, fields)| !fields.trim_start().starts_with('Z'))
                })
                .unwrap_or(false);
            if !running {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("agent process group left its descendant running");
    }

    #[cfg(windows)]
    #[test]
    fn copilot_cmd_shim_is_discovered_and_started_with_arguments() {
        let directory = std::env::temp_dir().join(format!(
            "agent-control-plane copilot-cmd-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("create CLI directory");
        let executable = directory.join("copilot.cmd");
        let extensionless_shim = directory.join("copilot");
        fs::write(&extensionless_shim, "not a Windows executable")
            .expect("create extensionless CLI shim");
        fs::write(&executable, "@echo off\r\necho %1\r\n").expect("create CLI shim");

        assert_eq!(copilot_in_directory(&directory), Some(executable.clone()));
        assert_eq!(
            copilot_executable_from(Some(extensionless_shim.as_os_str()), None, None),
            Some(executable.clone())
        );
        let output = command_for_executable(&executable)
            .arg("hello")
            .output()
            .expect("start CLI shim");

        fs::remove_dir_all(&directory).expect("remove CLI directory");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
    }

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
    fn notification_preferences_default_to_enabled_and_persist_independently() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        initialize_schema(&connection).expect("schema");

        assert!(read_notification_setting(&connection, "notifications_enabled").expect("master"));
        assert!(
            read_notification_setting(&connection, "attention_notifications_enabled")
                .expect("attention")
        );
        assert!(
            read_notification_setting(&connection, "task_completion_notifications_enabled")
                .expect("task completion")
        );

        connection
            .execute_batch(
                "INSERT INTO app_settings (key, value) VALUES
                   ('notifications_enabled', 'false'),
                   ('attention_notifications_enabled', 'true'),
                   ('task_completion_notifications_enabled', 'false')",
            )
            .expect("notification preferences");

        assert!(!read_notification_setting(&connection, "notifications_enabled").expect("master"));
        assert!(
            read_notification_setting(&connection, "attention_notifications_enabled")
                .expect("attention")
        );
        assert!(
            !read_notification_setting(&connection, "task_completion_notifications_enabled")
                .expect("task completion")
        );
    }

    #[test]
    fn task_result_notification_resolves_only_active_task_links() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        initialize_schema(&connection).expect("schema");
        connection
            .execute_batch(
                "INSERT INTO projects (id, name, path, branch, current_commit, git_state)
                 VALUES ('project-1', 'Project', '/tmp/project', 'main', 'abc', 'clean');
                 INSERT INTO project_tasks (id, project_id, title, status, session_id)
                 VALUES ('task-1', 'project-1', 'Sensitive task title', 'in_progress', 'session-1');",
            )
            .expect("project task link");

        let event = task_result_notification_event(&connection, "session-1", false)
            .expect("task notification query")
            .expect("active task result");
        assert_eq!(event.session_id, "session-1");
        assert_eq!(event.project_id, "project-1");
        assert_eq!(event.task_id, "task-1");
        assert!(!event.success);

        finish_project_task_session(&connection, "session-1", false).expect("finish failed task");
        assert!(
            task_result_notification_event(&connection, "session-1", false)
                .expect("task notification query")
                .is_none()
        );
    }

    #[test]
    fn disconnect_project_preserves_history_and_blocks_running_sessions() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        initialize_schema(&connection).expect("schema");
        connection.execute_batch(
            "INSERT INTO projects (id, name, path, branch, current_commit, git_state, looping, agent_task_creation)
             VALUES ('project-1', 'Project', '/tmp/project', 'main', 'abc', 'clean', 1, 1);
             INSERT INTO project_tasks (id, project_id, title) VALUES ('task-1', 'project-1', 'Task');
             INSERT INTO agent_sessions (id, project_id, provider, task, status, origin)
             VALUES ('session-1', 'project-1', 'copilot', 'Task', 'running', 'managed');"
        ).expect("project and session");

        assert!(
            disconnect_project(&connection, "project-1")
                .unwrap_err()
                .contains("active agent sessions")
        );
        connection
            .execute("UPDATE agent_sessions SET status = 'completed'", [])
            .expect("finish session");
        disconnect_project(&connection, "project-1").expect("disconnect project");
        let state: (i64, i64, i64, i64) = connection.query_row(
            "SELECT connected, looping, agent_task_creation, (SELECT count(*) FROM project_tasks) FROM projects WHERE id = 'project-1'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).expect("preserved project");
        assert_eq!(state, (0, 0, 0, 1));
        assert!(
            list_agent_sessions_from_connection(&connection, None, None, None)
                .expect("sessions")
                .is_empty()
        );
        connection
            .execute(
                "UPDATE projects SET connected = 1 WHERE id = 'project-1'",
                [],
            )
            .expect("reconnect project");
        assert_eq!(
            list_agent_sessions_from_connection(&connection, None, None, None)
                .expect("sessions")
                .len(),
            1
        );
    }

    #[test]
    fn existing_projects_receive_disabled_automation_defaults() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        connection
            .execute_batch(
                "CREATE TABLE projects (
                   id TEXT PRIMARY KEY NOT NULL,
                   name TEXT NOT NULL,
                   path TEXT NOT NULL,
                   branch TEXT NOT NULL,
                   current_commit TEXT NOT NULL,
                   git_state TEXT NOT NULL,
                   created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                   updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                 );
                 INSERT INTO projects
                   (id, name, path, branch, current_commit, git_state)
                   VALUES ('project-1', 'Project', '/tmp/project', 'main', 'abc', 'clean');",
            )
            .expect("legacy project schema");

        initialize_schema(&connection).expect("automation migration");
        let settings: (i64, i64) = connection
            .query_row(
                "SELECT looping, agent_task_creation FROM projects WHERE id = 'project-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("migrated project settings");
        assert_eq!(settings, (0, 0));
    }

    #[test]
    fn only_loop_owned_task_sessions_auto_approve_one_time_permissions() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        initialize_schema(&connection).expect("schema");
        connection
            .execute(
                "INSERT INTO projects (id, name, path, branch, current_commit, git_state, looping)
                 VALUES ('project-1', 'Project', '/tmp/project', 'main', 'abc', 'clean', 1)",
                [],
            )
            .expect("project");
        connection
            .execute(
                "INSERT INTO agent_sessions (id, project_id, provider, task, status, loop_owned)
                 VALUES ('session-1', 'project-1', 'copilot', 'Task', 'running', 0)",
                [],
            )
            .expect("session");
        connection
            .execute(
                "INSERT INTO project_tasks (id, project_id, title, status, session_id)
                 VALUES ('task-1', 'project-1', 'Task', 'in_progress', 'session-1')",
                [],
            )
            .expect("task");
        let database = Arc::new(Mutex::new(connection));

        assert!(
            !project_session_auto_approve_permissions(&database, "session-1", "execute")
                .expect("manual task permission check")
        );
        database
            .lock()
            .expect("database lock")
            .execute(
                "UPDATE agent_sessions SET loop_owned = 1 WHERE id = 'session-1'",
                [],
            )
            .expect("mark loop-owned session");
        assert!(
            project_session_auto_approve_permissions(&database, "session-1", "execute")
                .expect("loop-owned task permission check")
        );
        database
            .lock()
            .expect("database lock")
            .execute(
                "UPDATE agent_sessions SET loop_owned = 0, permission_mode = 'acceptEdits'
                 WHERE id = 'session-1'",
                [],
            )
            .expect("set accept edits mode");
        assert!(
            project_session_auto_approve_permissions(&database, "session-1", "edit")
                .expect("accept-edits permission check")
        );
        assert!(
            !project_session_auto_approve_permissions(&database, "session-1", "execute")
                .expect("accept-edits non-edit permission check")
        );
        database
            .lock()
            .expect("database lock")
            .execute(
                "UPDATE agent_sessions SET permission_mode = 'auto' WHERE id = 'session-1'",
                [],
            )
            .expect("set auto mode");
        assert!(
            project_session_auto_approve_permissions(&database, "session-1", "execute")
                .expect("auto permission check")
        );
    }

    #[test]
    fn startup_marks_interrupted_managed_sessions_as_orphaned() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        initialize_schema(&connection).expect("initial schema");
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
                 (id, project_id, provider, task, branch, status, origin)
                 VALUES ('interrupted-session', 'project-1', 'copilot', 'Run checks', 'main', 'waiting', 'managed')",
                [],
            )
            .expect("interrupted session");

        initialize_schema(&connection).expect("restart schema reconciliation");

        let session = list_agent_sessions_from_connection(&connection, None, None, None)
            .expect("session query")
            .into_iter()
            .next()
            .expect("persisted session");
        assert_eq!(session.status, "orphaned");
        assert_eq!(
            session.status_reason,
            "Agent Control Plane restarted; reconnect the session to continue."
        );
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
                 (id, project_id, provider, external_session_id, task, branch, status, status_reason,
                  current_step, token_count, permission_mode, origin, last_event)
                 VALUES ('session-1', 'project-1', 'copilot', 'sess_external_1', 'Implement sessions',
                         'feat/sessions', 'waiting', 'Agent asked a question',
                         'Waiting for your answer', 12800, 'plan', 'managed',
                         'Which provider should be used?')",
                [],
            )
            .expect("session");
        connection
            .execute(
                "INSERT INTO project_tasks (id, project_id, title, status, session_id)
                 VALUES ('task-1', 'project-1', 'Implement sessions', 'in_progress', 'session-1')",
                [],
            )
            .expect("linked task");

        let sessions = list_agent_sessions_from_connection(&connection, None, None, None)
            .expect("session query");

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "session-1");
        assert_eq!(sessions[0].project_id, "project-1");
        assert_eq!(sessions[0].task_id.as_deref(), Some("task-1"));
        assert!(sessions[0].worktree_path.is_none());
        assert!(task_worktree_path(&connection, "project-1", "task-1").is_err());
        connection
            .execute(
                "UPDATE agent_sessions SET worktree_path = '/tmp/task worktree' WHERE id = 'session-1'",
                [],
            )
            .expect("prepared worktree");
        assert_eq!(
            task_worktree_path(&connection, "project-1", "task-1").unwrap(),
            "/tmp/task worktree"
        );
        assert!(task_worktree_path(&connection, "other-project", "task-1").is_err());
        assert!(task_worktree_path(&connection, "project-1", "missing-task").is_err());
        let prepared = list_agent_sessions_from_connection(&connection, None, None, None)
            .expect("prepared worktree session query");
        assert_eq!(
            prepared[0].worktree_path.as_deref(),
            Some("/tmp/task worktree")
        );
        connection
            .execute(
                "UPDATE agent_sessions SET worktree_path = '   ' WHERE id = 'session-1'",
                [],
            )
            .expect("empty worktree");
        assert!(task_worktree_path(&connection, "project-1", "task-1").is_err());
        assert_eq!(
            sessions[0].external_session_id.as_deref(),
            Some("sess_external_1")
        );
        assert_eq!(sessions[0].provider, "copilot");
        assert_eq!(sessions[0].status, "waiting");
        assert_eq!(sessions[0].status_reason, "Agent asked a question");
        assert!(!sessions[0].updated_at.is_empty());
        assert!(sessions[0].managed);
        assert_eq!(sessions[0].tokens, "12.8k");
        assert!(!sessions[0].task_done);
        assert!(!sessions[0].task_archived);

        connection
            .execute(
                "UPDATE project_tasks SET status = 'done' WHERE id = 'task-1'",
                [],
            )
            .expect("complete linked task");
        let completed = list_agent_sessions_from_connection(&connection, None, None, None)
            .expect("completed task session query");
        assert!(completed[0].task_done);

        connection
            .execute(
                "UPDATE project_tasks SET status = 'in_progress', archived = 1 WHERE id = 'task-1'",
                [],
            )
            .expect("archive linked task");
        let archived = list_agent_sessions_from_connection(&connection, None, None, None)
            .expect("archived task session query");
        assert!(archived[0].task_archived);
    }

    #[test]
    fn archived_sessions_are_sorted_by_last_updated_descending() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        initialize_schema(&connection).expect("schema");
        connection
            .execute(
                "INSERT INTO projects (id, name, path, branch, current_commit, git_state)
                 VALUES ('project-1', 'Control Plane', '/tmp/control-plane', 'main', 'abc', 'clean')",
                [],
            )
            .expect("project");
        for (id, started_at, updated_at) in [
            (
                "started-newest",
                "2026-11-10 10:00:00",
                "2026-11-08 10:00:00",
            ),
            (
                "updated-newest",
                "2026-11-08 10:00:00",
                "2026-11-10 10:00:00",
            ),
            ("middle", "2026-11-09 10:00:00", "2026-11-09 10:00:00"),
        ] {
            connection
                .execute(
                    "INSERT INTO agent_sessions
                     (id, project_id, provider, task, branch, status, started_at, updated_at, archived)
                     VALUES (?1, 'project-1', 'copilot', ?1, 'main', 'completed', ?2, ?3, 1)",
                    params![id, started_at, updated_at],
                )
                .expect("session");
        }

        let sessions = list_agent_sessions_from_connection(&connection, None, None, Some(true))
            .expect("session query");

        assert_eq!(
            sessions
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec!["updated-newest", "middle", "started-newest"]
        );
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
    fn oversized_assistant_output_is_bounded_and_marked() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        initialize_schema(&connection).expect("schema");
        let assistant_id =
            insert_session_message(&connection, "session-1", "assistant", "").expect("assistant");
        let database = Arc::new(Mutex::new(connection));

        append_assistant_chunk(
            &database,
            "session-1",
            &assistant_id,
            &"x".repeat(MAX_TRANSCRIPT_MESSAGE_BYTES + 32),
        )
        .expect("append oversized response");

        let connection = database.lock().expect("database lock");
        let content = connection
            .query_row(
                "SELECT content FROM session_messages WHERE id = ?1",
                [&assistant_id],
                |row| row.get::<_, String>(0),
            )
            .expect("bounded response");
        assert!(content.len() <= MAX_TRANSCRIPT_MESSAGE_BYTES);
        assert!(content.ends_with(TRANSCRIPT_TRUNCATION_MARKER));
    }

    #[test]
    fn full_transcript_returns_all_messages_in_chronological_order() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        initialize_schema(&connection).expect("schema");
        connection
            .execute(
                "INSERT INTO agent_sessions (id, project_id, provider, task, status, origin)
                 VALUES ('session-full', 'proj-1', 'codex', 'Long session', 'completed', 'app')",
                [],
            )
            .expect("insert session");

        for index in 1..=105 {
            let role = if index % 2 == 1 { "user" } else { "assistant" };
            insert_session_message(
                &connection,
                "session-full",
                role,
                &format!("Message {index}"),
            )
            .expect("insert message");
        }

        let preview = load_session_transcript_from_connection(&connection, "session-full", false)
            .expect("preview transcript");
        assert_eq!(preview.len(), 100);
        assert_eq!(
            preview.first().map(|m| m.content.as_str()),
            Some("Message 6")
        );
        assert_eq!(
            preview.last().map(|m| m.content.as_str()),
            Some("Message 105")
        );

        let full = load_session_transcript_from_connection(&connection, "session-full", true)
            .expect("full transcript");
        assert_eq!(full.len(), 105);
        assert_eq!(full.first().map(|m| m.content.as_str()), Some("Message 1"));
        assert_eq!(full.last().map(|m| m.content.as_str()), Some("Message 105"));
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

        let observation = acp_observation_from_line(AcpProvider::Copilot, line, false)
            .expect("valid ACP message")
            .expect("agent message observation");

        assert_eq!(
            observation,
            AcpObservation {
                status: Some("running".to_owned()),
                step: Some("Receiving Copilot response".to_owned()),
                last_event: Some("Checking the repository".to_owned()),
            }
        );
    }

    #[test]
    fn copilot_agent_message_array_content_is_mapped_to_running_observation() {
        let line = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":[{"type":"text","text":"Checking "},{"type":"text","text":"the repository"}]}}}"#;

        let observation = acp_observation_from_line(AcpProvider::Copilot, line, false)
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

        let observation = acp_observation_from_line(AcpProvider::Copilot, line, false)
            .expect("valid ACP message")
            .expect("permission observation");

        assert_eq!(observation.status.as_deref(), Some("waiting"));
        assert_eq!(observation.step.as_deref(), Some("Waiting for permission"));
    }

    #[test]
    fn copilot_updates_preserve_pending_permission_attention() {
        for update_type in [
            "tool_call",
            "tool_call_update",
            "agent_message_chunk",
            "user_message_chunk",
        ] {
            let line = serde_json::json!({
                "method": "session/update",
                "params": { "update": {
                    "sessionUpdate": update_type,
                    "title": "Run tests",
                    "content": { "type": "text", "text": "Checking the repository" }
                } }
            })
            .to_string();
            let observation = acp_observation_from_line(AcpProvider::Copilot, &line, true)
                .expect("valid update")
                .expect("update observation");
            assert_eq!(
                observation.status.as_deref(),
                Some("waiting"),
                "{update_type}"
            );
            assert_eq!(observation.step.as_deref(), Some("Waiting for permission"));

            let resumed = acp_observation_from_line(AcpProvider::Copilot, &line, false)
                .expect("valid update")
                .expect("resumed observation");
            assert_eq!(resumed.status.as_deref(), Some("running"));
        }

        let error = acp_observation_from_line(
            AcpProvider::Copilot,
            r#"{"error":{"message":"Tool failed"}}"#,
            true,
        )
        .expect("valid error")
        .expect("error observation");
        assert_eq!(error.status.as_deref(), Some("failed"));
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
            permission_outcome(AcpProvider::Copilot, &permission, Some("allow_once"))
                .expect("known choice"),
            serde_json::json!({ "outcome": "selected", "optionId": "allow_once" })
        );
        assert!(permission_outcome(AcpProvider::Copilot, &permission, Some("allow_all")).is_err());
        assert_eq!(
            permission_outcome(AcpProvider::Antigravity, &permission, None).expect("cancel choice"),
            serde_json::json!({ "outcome": "cancelled" })
        );
    }

    #[test]
    fn task_status_parser_accepts_only_valid_status_blocks_and_uses_the_last_one() {
        let report = parse_apt_status_report(
            "Progress\n```apt-status\n{\"state\":\"working\",\"note\":\"Still checking\"}\n```\n\
             Done\n```apt-status\n{\"state\":\"complete\",\"note\":\"Ready\",\"gatePassed\":true}\n```",
        )
        .expect("valid completion report");
        assert_eq!(report.state, "complete");
        assert_eq!(report.note, "Ready");
        assert_eq!(report.gate_passed, Some(true));

        let inline_report = parse_apt_status_report(
            "```apt-status {\"state\":\"complete\",\"note\":\"Answered using the provided current time\",\"gatePassed\":true,\"gateSummary\":\"Current time provided in the conversation context\"} ```",
        )
        .expect("valid inline completion report");
        assert_eq!(inline_report.state, "complete");
        assert_eq!(inline_report.gate_passed, Some(true));

        assert!(
            parse_apt_status_report("```apt-status\n{\"state\":\"complete\"}\n```")
                .expect("valid state")
                .gate_passed
                .is_none()
        );
        assert!(parse_apt_status_report("```apt-status\nnot json\n```").is_none());
    }

    #[test]
    fn waiting_session_accepts_a_message_unless_permission_is_pending() {
        assert!(validate_session_message_status("waiting", false, false).is_ok());
        assert!(
            validate_session_message_status("waiting", false, true)
                .expect_err("pending permission must be answered first")
                .contains("pending permission request")
        );
        assert!(validate_session_message_status("running", false, false).is_err());
    }

    #[test]
    fn session_names_must_not_be_empty() {
        assert!("   ".trim().is_empty());
        assert!("A useful session name".len() <= 500);
    }

    #[test]
    fn malformed_copilot_output_is_an_error() {
        assert!(acp_observation_from_line(AcpProvider::Copilot, "not-json", false).is_err());
        assert!(acp_observation_from_line(AcpProvider::Antigravity, "not-json", false).is_err());
    }

    #[test]
    fn claude_text_stream_events_are_mapped_to_transcript_chunks() {
        let observation = claude_observation_from_line(
            r#"{"type":"stream_event","session_id":"claude-session","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hello"}}}"#,
        )
        .expect("valid Claude stream event");

        assert_eq!(observation.session_id.as_deref(), Some("claude-session"));
        assert_eq!(observation.text.as_deref(), Some("Hello"));
        assert!(!observation.completed);
    }

    #[test]
    fn claude_result_events_report_session_usage_and_failures() {
        let success = claude_observation_from_line(
            r#"{"type":"result","subtype":"success","session_id":"claude-session","result":"Done","usage":{"input_tokens":10,"output_tokens":5}}"#,
        )
        .expect("valid Claude result event");
        assert!(success.completed);
        assert!(!success.failed);
        assert_eq!(success.result.as_deref(), Some("Done"));
        assert_eq!(success.token_count, Some(15));

        let failure = claude_observation_from_line(
            r#"{"type":"result","subtype":"error_during_execution","session_id":"claude-session","result":"Tool failed"}"#,
        )
        .expect("valid Claude error result event");
        assert!(failure.completed);
        assert!(failure.failed);
        assert_eq!(failure.result.as_deref(), Some("Tool failed"));
    }

    #[test]
    fn copilot_activity_parser_captures_tool_updates_permissions_and_errors() {
        let tool = acp_activity_from_message(
            AcpProvider::Copilot,
            &serde_json::json!({
                "method": "session/update",
                "params": { "update": {
                    "sessionUpdate": "tool_call_update",
                    "title": "Run tests",
                    "status": "completed",
                    "input": { "command": "cargo test" }
                } }
            }),
        )
        .expect("tool update");
        assert_eq!(tool.event_type, "tool_update");
        assert_eq!(tool.status, "completed");
        assert_eq!(tool.label, "Run tests");

        let permission = acp_activity_from_message(
            AcpProvider::Copilot,
            &serde_json::json!({
                "id": 8,
                "method": "session/request_permission",
                "params": { "toolCall": { "title": "Write file" }, "options": [] }
            }),
        )
        .expect("permission request");
        assert_eq!(permission.event_type, "permission_request");
        assert_eq!(permission.status, "requested");

        let error = acp_activity_from_message(
            AcpProvider::Copilot,
            &serde_json::json!({
                "error": { "message": "Provider rejected the call" }
            }),
        )
        .expect("provider error");
        assert_eq!(error.event_type, "error");
        assert_eq!(error.status, "failed");
    }

    #[test]
    fn claude_structured_events_capture_tool_io_and_permission_decisions() {
        let tool = claude_observation_from_line(
            r#"{"type":"assistant","session_id":"claude-session","message":{"content":[{"type":"thinking","thinking":"not exposed as transcript activity"},{"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"cargo test"}}]}}"#,
        )
        .expect("Claude tool event");
        assert_eq!(tool.activities.len(), 1);
        assert_eq!(tool.activities[0].event_type, "tool_call");
        assert_eq!(tool.activities[0].label, "Bash");

        let output = claude_observation_from_line(
            r#"{"type":"user","session_id":"claude-session","message":{"content":[{"type":"tool_result","tool_use_id":"tool-1","content":"tests passed","is_error":false}]}}"#,
        )
        .expect("Claude tool output");
        assert_eq!(output.activities[0].event_type, "tool_output");
        assert_eq!(output.activities[0].status, "completed");

        let permission = claude_observation_from_line(
            r#"{"type":"control_request","session_id":"claude-session","request":{"subtype":"permission_request","tool_name":"Bash"}}"#,
        )
        .expect("Claude permission request");
        assert_eq!(permission.activities[0].event_type, "permission_request");

        let decision = claude_observation_from_line(
            r#"{"type":"control_response","session_id":"claude-session","response":{"subtype":"permission_response","behavior":"allow"}}"#,
        )
        .expect("Claude permission decision");
        assert_eq!(decision.activities[0].event_type, "permission_decision");
    }

    #[test]
    fn transcript_activity_is_redacted_bounded_and_persisted_in_chronological_order() {
        let path = std::env::temp_dir().join(format!(
            "agent-control-plane-transcript-{}-{}.sqlite",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let connection = Connection::open(&path).expect("database");
        initialize_schema(&connection).expect("schema");
        connection
            .execute(
                "INSERT INTO projects (id, name, path, branch, current_commit, git_state)
                 VALUES ('project-1', 'Project', '/tmp/project', 'main', 'abc', 'clean')",
                [],
            )
            .expect("project");
        connection
            .execute(
                "INSERT INTO agent_sessions (id, project_id, provider, task, status)
                 VALUES ('session-1', 'project-1', 'copilot', 'Task', 'completed')",
                [],
            )
            .expect("session");
        insert_session_message(&connection, "session-1", "user", "Run tests")
            .expect("user message");
        let database = Arc::new(Mutex::new(connection));
        persist_session_activity(
            &database,
            "session-1",
            &TranscriptActivity {
                event_type: "tool_call".to_owned(),
                label: "Run tests".to_owned(),
                status: "started".to_owned(),
                details: serde_json::json!({
                    "token": "private-token",
                    "command": "curl -H 'Authorization: Bearer private-bearer' " ,
                    "output": "x".repeat(MAX_ACTIVITY_DETAILS_BYTES + 100)
                }),
            },
        )
        .expect("persist activity");
        insert_session_message(
            &database.lock().expect("lock"),
            "session-1",
            "assistant",
            "Done",
        )
        .expect("assistant message");
        drop(database);

        let connection = Connection::open(&path).expect("reopen persisted database");
        let transcript = load_session_transcript_from_connection(&connection, "session-1", true)
            .expect("full transcript");
        let _ = fs::remove_file(path);

        assert_eq!(
            transcript
                .iter()
                .map(|entry| entry.role.as_str())
                .collect::<Vec<_>>(),
            vec!["user", "activity", "assistant"]
        );
        let details = transcript[1].details.as_deref().expect("activity details");
        assert!(details.len() <= MAX_ACTIVITY_DETAILS_BYTES);
        assert!(details.contains("[REDACTED]"));
        assert!(!details.contains("private-token"));
        assert!(!details.contains("private-bearer"));
        assert!(details.contains("[truncated]"));
    }

    #[test]
    fn copilot_usage_updates_parse_context_snapshots() {
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "copilot-session",
                "update": {
                    "sessionUpdate": "usage_update",
                    "used": 12800,
                    "size": 128000
                }
            }
        });

        assert_eq!(
            acp_token_count_from_message(&message, "copilot-session"),
            Ok(Some((12800, 128000)))
        );
    }

    #[test]
    fn copilot_usage_updates_validate_the_provider_session_id() {
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
                 (id, project_id, provider, external_session_id, task, status)
                 VALUES ('local-session-id', 'project-1', 'copilot', 'acp-session-id', 'Test usage', 'running')",
                [],
            )
            .expect("session");
        let database = Arc::new(Mutex::new(connection));
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "acp-session-id",
                "update": {
                    "sessionUpdate": "usage_update",
                    "used": 12800,
                    "size": 128000
                }
            }
        });

        let (token_count, context_window_size) =
            acp_usage_update_for_session(&database, &message, "local-session-id")
                .expect("usage update")
                .expect("recognized usage update");
        update_session_token_count(
            &database,
            "local-session-id",
            token_count,
            Some(context_window_size),
        )
        .expect("persist usage");
        let connection = database.lock().expect("database lock");
        let session = list_agent_sessions_from_connection(&connection, None, None, None)
            .expect("read session")
            .into_iter()
            .next()
            .expect("session");
        assert_eq!(session.tokens, "12.8k");
        assert_eq!(session.context_tokens, Some(12800));
        assert_eq!(session.context_window_size, Some(128000));
    }

    #[test]
    fn antigravity_usage_updates_and_executable_discovery_work() {
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
                 (id, project_id, provider, external_session_id, task, status)
                 VALUES ('agy-local-id', 'project-1', 'antigravity', 'agy-ext-id', 'Test usage', 'running')",
                [],
            )
            .expect("session");
        let database = Database(Arc::new(Mutex::new(connection)));
        assert!(ensure_agent_provider_enabled(&database, "antigravity").is_err());
        database
            .0
            .lock()
            .expect("database lock")
            .execute(
                "INSERT INTO tool_settings (provider, enabled) VALUES ('antigravity', 1)",
                [],
            )
            .expect("enable antigravity");
        assert!(ensure_agent_provider_enabled(&database, "antigravity").is_ok());

        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "agy-ext-id",
                "update": {
                    "sessionUpdate": "usage_update",
                    "used": 6400,
                    "size": 200000
                }
            }
        });
        assert_eq!(
            acp_usage_update_for_session(&database.0, &message, "agy-local-id"),
            Ok(Some((6400, 200000)))
        );

        let root = std::env::temp_dir().join(format!(
            "agent-control-plane-agy-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        let binary_name = if cfg!(windows) { "agy.exe" } else { "agy" };
        let path_bin = root.join("path-bin").join(binary_name);
        let local_app_data = root.join("local-app-data");
        let app_data_bin = local_app_data.join("agy").join("bin").join(binary_name);
        let home_bin = root.join("home").join(".local/bin").join(binary_name);
        for candidate in [&path_bin, &app_data_bin, &home_bin] {
            fs::create_dir_all(candidate.parent().expect("parent")).expect("create dir");
            fs::write(candidate, "").expect("create fixture");
        }

        assert_eq!(
            antigravity_executable_from(
                None,
                Some(path_bin.parent().expect("path dir").as_os_str()),
                Some(&root.join("home")),
                Some(&local_app_data),
            ),
            Some(path_bin.clone())
        );
        assert_eq!(
            antigravity_executable_from(
                None,
                None,
                Some(&root.join("home")),
                Some(&local_app_data),
            ),
            Some(app_data_bin.clone())
        );
        assert_eq!(
            antigravity_executable_from(None, None, Some(&root.join("home")), None),
            Some(home_bin.clone())
        );
        fs::remove_dir_all(&root).expect("remove fixture");
    }

    #[test]
    fn copilot_usage_rejects_malformed_updates() {
        let missing_used = serde_json::json!({
            "method": "session/update",
            "params": {
                "sessionId": "copilot-session",
                "update": {
                    "sessionUpdate": "usage_update",
                    "size": 128000
                }
            }
        });
        let fractional_used = serde_json::json!({
            "method": "session/update",
            "params": {
                "sessionId": "copilot-session",
                "update": {
                    "sessionUpdate": "usage_update",
                    "used": 12.5,
                    "size": 128000
                }
            }
        });

        assert!(acp_token_count_from_message(&missing_used, "copilot-session").is_err());
        assert!(acp_token_count_from_message(&fractional_used, "copilot-session").is_err());
        assert_eq!(
            acp_token_count_from_message(&missing_used, "other-session"),
            Err("ACP session update belongs to a different session.".to_owned())
        );
    }
}
