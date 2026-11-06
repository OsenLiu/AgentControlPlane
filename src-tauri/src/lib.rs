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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolSettingsInput {
    enabled: bool,
    executable: String,
    profile: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsInput {
    copilot: ToolSettingsInput,
    gemini: ToolSettingsInput,
    claude_code: ToolSettingsInput,
    github_username: String,
    github_token: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolSettings {
    enabled: bool,
    executable: String,
    profile: String,
    configured: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    copilot: ToolSettings,
    gemini: ToolSettings,
    claude_code: ToolSettings,
    github_username: String,
    github_token_configured: bool,
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

fn tool_settings_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ToolSettings> {
    let executable: String = row.get(1)?;
    Ok(ToolSettings {
        enabled: row.get(0)?,
        configured: !executable.trim().is_empty(),
        executable,
        profile: row.get(2)?,
    })
}

fn default_tool_settings() -> ToolSettings {
    ToolSettings {
        enabled: false,
        executable: String::new(),
        profile: String::new(),
        configured: false,
    }
}

fn read_tool_settings(
    connection: &Connection,
    provider: &str,
) -> Result<ToolSettings, String> {
    connection
        .query_row(
            "SELECT enabled, executable, profile FROM tool_settings WHERE provider = ?1",
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

fn validate_tool(provider: &str, settings: &ToolSettingsInput) -> Result<(), String> {
    if !settings.enabled {
        return Ok(());
    }
    if settings.executable.trim().is_empty() {
        return Err(format!("Set an executable for {provider} or disable it."));
    }

    let output = Command::new(settings.executable.trim())
        .arg("--version")
        .output()
        .map_err(|error| format!("Unable to start {provider}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }

    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(if detail.is_empty() {
        format!("{provider} did not accept the --version check.")
    } else {
        format!("{provider} check failed: {detail}")
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

    Ok(Settings {
        copilot: read("copilot"),
        gemini: read("gemini"),
        claude_code: read("claude_code"),
        github_username,
        github_token_configured: github_token_configured(),
    })
}

#[tauri::command]
fn save_settings(input: SettingsInput, database: State<'_, Database>) -> Result<Settings, String> {
    validate_tool("GitHub Copilot", &input.copilot)?;
    validate_tool("Gemini CLI", &input.gemini)?;
    validate_tool("Claude Code", &input.claude_code)?;

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
                "INSERT INTO tool_settings (provider, enabled, executable, profile)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(provider) DO UPDATE SET
                   enabled = excluded.enabled,
                   executable = excluded.executable,
                   profile = excluded.profile",
                params![provider, settings.enabled, settings.executable.trim(), settings.profile.trim()],
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

fn open_database(app: &tauri::AppHandle) -> Result<Database, Box<dyn std::error::Error>> {
    let data_dir = app.path().app_data_dir()?;
    fs::create_dir_all(&data_dir)?;
    let database_path = data_dir.join("agent-control-plane.sqlite3");
    let connection = Connection::open(database_path)?;
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
           enabled INTEGER NOT NULL DEFAULT 0,
           executable TEXT NOT NULL DEFAULT '',
           profile TEXT NOT NULL DEFAULT ''
         );
         CREATE TABLE IF NOT EXISTS app_settings (
           key TEXT PRIMARY KEY NOT NULL,
           value TEXT NOT NULL DEFAULT ''
         );",
    )?;
    Ok(Database(Mutex::new(connection)))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            app.manage(open_database(app.handle())?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_projects,
            register_project,
            load_settings,
            save_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running Agent Control Plane");
}
