use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};
use tauri::{Manager, State};

struct Database(Mutex<Connection>);

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
    })
}

fn validate_session_status(status: &str) -> Result<(), String> {
    match status {
        "starting" | "running" | "waiting" | "paused" | "completed" | "failed"
        | "orphaned" | "unknown" => Ok(()),
        _ => Err(format!("Unsupported agent session status: {status}")),
    }
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

    Ok(Settings {
        copilot: read("copilot"),
        gemini: read("gemini"),
        claude_code: read("claude_code"),
        github_username,
        github_token_configured: github_token_configured(),
        default_provider,
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
) -> Result<Vec<AgentSession>, String> {
    if let Some(value) = status {
        validate_session_status(value)?;
    }

    let mut statement = connection
        .prepare(
            "SELECT s.id, p.name, s.task, s.branch, s.provider, s.status,
                    s.current_step, s.started_at, s.token_count,
                    s.permission_mode, s.origin, s.last_event
             FROM agent_sessions s
             JOIN projects p ON p.id = s.project_id
             WHERE (?1 IS NULL OR s.project_id = ?1)
               AND (?2 IS NULL OR s.status = ?2)
             ORDER BY
               CASE s.status
                 WHEN 'waiting' THEN 0
                 WHEN 'running' THEN 1
                 WHEN 'starting' THEN 2
                 WHEN 'paused' THEN 3
                 ELSE 4
               END,
               s.updated_at DESC",
        )
        .map_err(|error| format!("Unable to read agent sessions: {error}"))?;

    statement
        .query_map(params![project_id, status], agent_session_from_row)
        .map_err(|error| format!("Unable to read agent sessions: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Unable to decode agent session data: {error}"))
}

#[tauri::command]
fn list_agent_sessions(
    project_id: Option<String>,
    status: Option<String>,
    database: State<'_, Database>,
) -> Result<Vec<AgentSession>, String> {
    let connection = database
        .0
        .lock()
        .map_err(|_| "Agent session database is unavailable.".to_owned())?;
    list_agent_sessions_from_connection(&connection, project_id.as_deref(), status.as_deref())
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
    )
}

fn open_database(app: &tauri::AppHandle) -> Result<Database, Box<dyn std::error::Error>> {
    let data_dir = app.path().app_data_dir()?;
    fs::create_dir_all(&data_dir)?;
    let database_path = data_dir.join("agent-control-plane.sqlite3");
    let connection = Connection::open(database_path)?;
    initialize_schema(&connection)?;
    Ok(Database(Mutex::new(connection)))
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

        let sessions = list_agent_sessions_from_connection(&connection, None, None)
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

        let sessions = list_agent_sessions_from_connection(&connection, None, None)
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
        assert!(validate_session_status("not-a-session-state").is_err());
    }
}
