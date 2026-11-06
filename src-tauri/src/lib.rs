use rusqlite::{params, Connection};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};
use tauri::{Manager, State};

struct Database(Mutex<Connection>);

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
        .invoke_handler(tauri::generate_handler![list_projects, register_project])
        .run(tauri::generate_context!())
        .expect("error while running Agent Control Plane");
}
