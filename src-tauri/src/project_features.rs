use crate::{Database, GITHUB_CREDENTIAL_SERVICE, GITHUB_CREDENTIAL_USER};
use keyring::Entry;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    env, fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, State};

const MAX_TASK_TITLE: usize = 200;
const MAX_TASK_DESCRIPTION: usize = 20_000;
const MAX_TERMINALS_PER_PROJECT: usize = 8;
const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;
const MAX_UNTRACKED_FILE_BYTES: u64 = 512 * 1024;
const MAX_TERMINAL_BUFFER_BYTES: usize = 512 * 1024;
const MAX_TERMINAL_INPUT_BYTES: usize = 64 * 1024;
const MAX_SKILL_BYTES: u64 = 512 * 1024;
const MAX_MANIFEST_BYTES: u64 = 2 * 1024 * 1024;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectTask {
    id: String,
    project_id: String,
    title: String,
    description: String,
    status: String,
    loop_enabled: bool,
    origin: String,
    session_id: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectDiff {
    branch: String,
    changed_files: Vec<String>,
    diff: String,
    truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectDependency {
    name: String,
    version: String,
    kind: String,
    manifest: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectDependencies {
    manifests: Vec<String>,
    dependencies: Vec<ProjectDependency>,
    errors: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectSkill {
    name: String,
    description: String,
    folder_name: String,
    source: String,
    content: String,
    warning: Option<String>,
    shadowed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectSkills {
    global: Vec<ProjectSkill>,
    project: Vec<ProjectSkill>,
    errors: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PipelineRun {
    id: String,
    name: String,
    status: String,
    conclusion: Option<String>,
    branch: String,
    url: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectPipelines {
    repository: Option<String>,
    configured: bool,
    error: Option<String>,
    runs: Vec<PipelineRun>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TerminalSnapshot {
    id: String,
    project_id: String,
    title: String,
    created_at: String,
    alive: bool,
    exit_code: Option<u32>,
    buffer: String,
    buffer_end_offset: u64,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TerminalOutput {
    terminal_id: String,
    project_id: String,
    chunk: String,
    end_offset: u64,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TerminalExit {
    terminal_id: String,
    project_id: String,
    exit_code: Option<u32>,
}

struct LiveTerminal {
    id: String,
    project_id: String,
    title: String,
    created_at: String,
    buffer: Mutex<String>,
    buffer_end_offset: AtomicU64,
    alive: AtomicBool,
    exit_code: Mutex<Option<u32>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send>>,
    _master: Mutex<Box<dyn MasterPty + Send>>,
}

#[derive(Default)]
pub(crate) struct TerminalManager(Arc<Mutex<HashMap<String, Arc<LiveTerminal>>>>);

impl Drop for TerminalManager {
    fn drop(&mut self) {
        if let Ok(terminals) = self.0.lock() {
            for terminal in terminals.values() {
                if let Ok(mut killer) = terminal.killer.lock() {
                    let _ = killer.kill();
                }
            }
        }
    }
}

pub(crate) fn initialize_schema(connection: &Connection) -> rusqlite::Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS project_tasks (
           id TEXT PRIMARY KEY NOT NULL,
           project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
           title TEXT NOT NULL,
           description TEXT NOT NULL DEFAULT '',
           status TEXT NOT NULL DEFAULT 'backlog'
             CHECK(status IN ('backlog', 'in_progress', 'done')),
           loop_enabled INTEGER NOT NULL DEFAULT 1,
           origin TEXT NOT NULL DEFAULT 'user',
           session_id TEXT,
           created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
         );
         CREATE INDEX IF NOT EXISTS idx_project_tasks_project
           ON project_tasks(project_id, updated_at DESC);",
        )
        .and_then(|_| {
            connection
                .execute(
                    "ALTER TABLE project_tasks ADD COLUMN loop_enabled INTEGER NOT NULL DEFAULT 1",
                    [],
                )
                .or_else(|error| {
                    if error
                        .to_string()
                        .contains("duplicate column name: loop_enabled")
                    {
                        Ok(0)
                    } else {
                        Err(error)
                    }
                })
        })
        .and_then(|_| {
            connection
                .execute(
                    "ALTER TABLE project_tasks ADD COLUMN origin TEXT NOT NULL DEFAULT 'user'",
                    [],
                )
                .or_else(|error| {
                    if error.to_string().contains("duplicate column name: origin") {
                        Ok(0)
                    } else {
                        Err(error)
                    }
                })
        })
        .map(|_| ())
}

fn project_path(database: &Database, project_id: &str) -> Result<PathBuf, String> {
    let connection = database
        .0
        .lock()
        .map_err(|_| "Project database is unavailable.".to_owned())?;
    let stored_path = connection
        .query_row(
            "SELECT path FROM projects WHERE id = ?1",
            [project_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("Unable to read project path: {error}"))?
        .ok_or_else(|| "The selected project is no longer registered.".to_owned())?;
    drop(connection);

    let canonical_path = PathBuf::from(stored_path)
        .canonicalize()
        .map_err(|error| format!("The project folder is unavailable: {error}"))?;
    if !canonical_path.is_dir() {
        return Err("The registered project path is not a directory.".to_owned());
    }
    Ok(canonical_path)
}

fn ensure_project_exists(connection: &Connection, project_id: &str) -> Result<(), String> {
    let exists = connection
        .query_row("SELECT 1 FROM projects WHERE id = ?1", [project_id], |_| {
            Ok(())
        })
        .optional()
        .map_err(|error| format!("Unable to validate project: {error}"))?
        .is_some();
    if exists {
        Ok(())
    } else {
        Err("The selected project is no longer registered.".to_owned())
    }
}

fn task_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectTask> {
    Ok(ProjectTask {
        id: row.get(0)?,
        project_id: row.get(1)?,
        title: row.get(2)?,
        description: row.get(3)?,
        status: row.get(4)?,
        loop_enabled: row.get::<_, i64>(5)? != 0,
        origin: row.get(6)?,
        session_id: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn list_tasks(connection: &Connection, project_id: &str) -> Result<Vec<ProjectTask>, String> {
    ensure_project_exists(connection, project_id)?;
    let mut statement = connection
        .prepare(
            "SELECT id, project_id, title, description, status, loop_enabled, origin, session_id, created_at, updated_at
             FROM project_tasks
             WHERE project_id = ?1
             ORDER BY CASE status WHEN 'backlog' THEN 0 WHEN 'in_progress' THEN 1 ELSE 2 END,
                      updated_at DESC",
        )
        .map_err(|error| format!("Unable to read project tasks: {error}"))?;
    statement
        .query_map([project_id], task_from_row)
        .map_err(|error| format!("Unable to read project tasks: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Unable to decode project tasks: {error}"))
}

#[tauri::command]
pub(crate) fn list_project_tasks(
    project_id: String,
    database: State<'_, Database>,
) -> Result<Vec<ProjectTask>, String> {
    let connection = database
        .0
        .lock()
        .map_err(|_| "Project database is unavailable.".to_owned())?;
    list_tasks(&connection, &project_id)
}

#[tauri::command]
pub(crate) fn create_project_task(
    project_id: String,
    title: String,
    description: String,
    database: State<'_, Database>,
) -> Result<ProjectTask, String> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > MAX_TASK_TITLE {
        return Err(format!(
            "Task title must be between 1 and {MAX_TASK_TITLE} characters."
        ));
    }
    if description.chars().count() > MAX_TASK_DESCRIPTION {
        return Err(format!(
            "Task description must be no more than {MAX_TASK_DESCRIPTION} characters."
        ));
    }
    let id = new_id("task");
    let connection = database
        .0
        .lock()
        .map_err(|_| "Project database is unavailable.".to_owned())?;
    ensure_project_exists(&connection, &project_id)?;
    connection
        .execute(
            "INSERT INTO project_tasks (id, project_id, title, description)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, project_id, title, description.trim()],
        )
        .map_err(|error| format!("Unable to create project task: {error}"))?;
    connection
        .query_row(
            "SELECT id, project_id, title, description, status, loop_enabled, origin, session_id, created_at, updated_at
             FROM project_tasks WHERE id = ?1",
            [&id],
            task_from_row,
        )
        .map_err(|error| format!("Unable to read the new project task: {error}"))
}

#[tauri::command]
pub(crate) fn set_project_task_status(
    project_id: String,
    task_id: String,
    status: String,
    database: State<'_, Database>,
) -> Result<(), String> {
    if !matches!(status.as_str(), "backlog" | "in_progress" | "done") {
        return Err("Choose a supported task status.".to_owned());
    }
    let connection = database
        .0
        .lock()
        .map_err(|_| "Project database is unavailable.".to_owned())?;
    let changed = connection
        .execute(
            "UPDATE project_tasks
             SET status = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE id = ?2 AND project_id = ?3",
            params![status, task_id, project_id],
        )
        .map_err(|error| format!("Unable to update project task: {error}"))?;
    if changed == 1 {
        Ok(())
    } else {
        Err("The task was not found in this project.".to_owned())
    }
}

#[tauri::command]
pub(crate) fn set_project_task_loop_enabled(
    project_id: String,
    task_id: String,
    enabled: bool,
    database: State<'_, Database>,
) -> Result<(), String> {
    let connection = database
        .0
        .lock()
        .map_err(|_| "Project database is unavailable.".to_owned())?;
    let changed = connection
        .execute(
            "UPDATE project_tasks
             SET loop_enabled = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE id = ?2 AND project_id = ?3 AND status = 'backlog'",
            params![enabled, task_id, project_id],
        )
        .map_err(|error| format!("Unable to update task loop selection: {error}"))?;
    if changed == 1 {
        Ok(())
    } else {
        Err("Only a backlog task in this project can be selected for looping.".to_owned())
    }
}

pub(crate) struct LoopTask {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) description: String,
}

pub(crate) fn take_next_loop_task(
    connection: &Connection,
    project_id: &str,
) -> Result<Option<LoopTask>, String> {
    let looping = connection
        .query_row(
            "SELECT looping FROM projects WHERE id = ?1",
            [project_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| format!("Unable to read project looping setting: {error}"))?
        .unwrap_or(0);
    if looping == 0 {
        return Ok(None);
    }
    let active_task = connection
        .query_row(
            "SELECT 1 FROM project_tasks
             WHERE project_id = ?1 AND status = 'in_progress' LIMIT 1",
            [project_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| format!("Unable to check active project tasks: {error}"))?
        .is_some();
    if active_task {
        return Ok(None);
    }
    let active_session = connection
        .query_row(
            "SELECT 1 FROM agent_sessions
             WHERE project_id = ?1 AND status IN ('starting', 'running', 'waiting')
             LIMIT 1",
            [project_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| format!("Unable to check active project sessions: {error}"))?
        .is_some();
    if active_session {
        return Ok(None);
    }
    let task = connection
        .query_row(
            "SELECT id, title, description FROM project_tasks
             WHERE project_id = ?1 AND status = 'backlog' AND loop_enabled = 1
             ORDER BY created_at, id LIMIT 1",
            [project_id],
            |row| {
                Ok(LoopTask {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    description: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("Unable to select the next loop task: {error}"))?;
    if let Some(task) = task {
        connection
            .execute(
                "UPDATE project_tasks SET status = 'in_progress', updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1 AND project_id = ?2 AND status = 'backlog'",
                params![task.id, project_id],
            )
            .map_err(|error| format!("Unable to claim the next loop task: {error}"))?;
        Ok(Some(task))
    } else {
        Ok(None)
    }
}

pub(crate) fn finish_project_task_session(
    connection: &Connection,
    session_id: &str,
    success: bool,
) -> Result<bool, String> {
    let task = connection
        .query_row(
            "SELECT t.id, t.loop_enabled, p.looping, p.agent_task_creation
             FROM project_tasks t JOIN projects p ON p.id = t.project_id
             WHERE t.session_id = ?1 AND t.status = 'in_progress'",
            [session_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? != 0,
                    row.get::<_, i64>(2)? != 0,
                    row.get::<_, i64>(3)? != 0,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("Unable to find the task for the completed session: {error}"))?;
    let Some((task_id, task_loop_enabled, looping, agent_task_creation)) = task else {
        let agent_task_creation = connection
            .query_row(
                "SELECT p.agent_task_creation FROM agent_sessions s
                 JOIN projects p ON p.id = s.project_id WHERE s.id = ?1",
                [session_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| format!("Unable to read agent-task setting: {error}"))?
            .unwrap_or(0);
        return Ok(agent_task_creation != 0);
    };
    if !success {
        connection
            .execute(
                "UPDATE project_tasks SET status = 'backlog', session_id = NULL,
                          updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1",
                [&task_id],
            )
            .map_err(|error| format!("Unable to mark the failed task: {error}"))?;
    } else if looping && task_loop_enabled {
        connection
            .execute(
                "UPDATE project_tasks SET status = 'done', updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1",
                [&task_id],
            )
            .map_err(|error| format!("Unable to accept the completed task: {error}"))?;
    }
    Ok(agent_task_creation)
}

pub(crate) fn create_agent_task_proposals(
    connection: &Connection,
    session_id: &str,
) -> Result<(), String> {
    let project_id = connection
        .query_row(
            "SELECT p.id FROM projects p JOIN agent_sessions s ON s.project_id = p.id
             WHERE s.id = ?1 AND p.agent_task_creation = 1",
            [session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("Unable to check agent-task setting: {error}"))?;
    let Some(project_id) = project_id else {
        return Ok(());
    };
    let mut statement = connection
        .prepare(
            "SELECT content FROM session_messages
             WHERE session_id = ?1 AND role = 'assistant' ORDER BY sequence",
        )
        .map_err(|error| format!("Unable to read assistant task proposals: {error}"))?;
    let messages = statement
        .query_map([session_id], |row| row.get::<_, String>(0))
        .map_err(|error| format!("Unable to read assistant task proposals: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Unable to decode assistant task proposals: {error}"))?;
    for (title, description) in messages
        .iter()
        .flat_map(|message| parse_agent_task_blocks(message))
    {
        let duplicate = connection
            .query_row(
                "SELECT 1 FROM project_tasks
                 WHERE project_id = ?1 AND title = ?2 COLLATE NOCASE LIMIT 1",
                params![project_id, title],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| format!("Unable to check duplicate agent task: {error}"))?
            .is_some();
        if !duplicate {
            connection
                .execute(
                    "INSERT INTO project_tasks (id, project_id, title, description, origin)
                     VALUES (?1, ?2, ?3, ?4, 'agent')",
                    params![new_id("task"), project_id, title, description],
                )
                .map_err(|error| format!("Unable to create proposed project task: {error}"))?;
        }
    }
    Ok(())
}

fn parse_agent_task_blocks(text: &str) -> Vec<(String, String)> {
    let mut proposals = Vec::new();
    for section in text.split("```").skip(1).step_by(2) {
        let Some((language, json)) = section.split_once('\n') else {
            continue;
        };
        if !language.trim().eq_ignore_ascii_case("apt-task") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(json.trim()) else {
            continue;
        };
        let Some(title) = value
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.chars().count() <= MAX_TASK_TITLE)
        else {
            continue;
        };
        let Some(purpose) = value
            .get("purpose")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let mut description = purpose.to_owned();
        if let Some(criteria) = value.get("acceptanceCriteria").and_then(Value::as_array) {
            let criteria = criteria
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|criterion| !criterion.is_empty())
                .collect::<Vec<_>>();
            if !criteria.is_empty() {
                description.push_str("\n\nAcceptance criteria:\n");
                description.push_str(
                    &criteria
                        .iter()
                        .map(|criterion| format!("- {criterion}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
        }
        if description.chars().count() <= MAX_TASK_DESCRIPTION {
            proposals.push((title.to_owned(), description));
        }
    }
    proposals
}

#[tauri::command]
pub(crate) fn link_project_task_session(
    project_id: String,
    task_id: String,
    session_id: String,
    database: State<'_, Database>,
) -> Result<(), String> {
    let connection = database
        .0
        .lock()
        .map_err(|_| "Project database is unavailable.".to_owned())?;
    let session_belongs_to_project = connection
        .query_row(
            "SELECT 1 FROM agent_sessions WHERE id = ?1 AND project_id = ?2",
            params![session_id, project_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| format!("Unable to validate the agent session: {error}"))?
        .is_some();
    if !session_belongs_to_project {
        return Err("The agent session must belong to this project.".to_owned());
    }
    let changed = connection
        .execute(
            "UPDATE project_tasks
             SET session_id = ?1, status = 'in_progress', updated_at = CURRENT_TIMESTAMP
             WHERE id = ?2 AND project_id = ?3",
            params![session_id, task_id, project_id],
        )
        .map_err(|error| format!("Unable to associate the agent session with the task: {error}"))?;
    if changed == 1 {
        Ok(())
    } else {
        Err("The task was not found in this project.".to_owned())
    }
}

#[tauri::command]
pub(crate) fn delete_project_task(
    project_id: String,
    task_id: String,
    database: State<'_, Database>,
) -> Result<(), String> {
    let connection = database
        .0
        .lock()
        .map_err(|_| "Project database is unavailable.".to_owned())?;
    let changed = connection
        .execute(
            "DELETE FROM project_tasks WHERE id = ?1 AND project_id = ?2",
            params![task_id, project_id],
        )
        .map_err(|error| format!("Unable to delete project task: {error}"))?;
    if changed == 1 {
        Ok(())
    } else {
        Err("The task was not found in this project.".to_owned())
    }
}

fn new_id(prefix: &str) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{prefix}-{timestamp}-{}",
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn git_output(path: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|error| format!("Unable to start Git: {error}"))
}

fn git_text(path: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_output(path, args)?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if detail.is_empty() {
            "Git rejected the project operation.".to_owned()
        } else {
            detail
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn append_limited(target: &mut String, addition: &str, truncated: &mut bool) {
    if target.len() >= MAX_DIFF_BYTES {
        *truncated = true;
        return;
    }
    let remaining = MAX_DIFF_BYTES - target.len();
    if addition.len() <= remaining {
        target.push_str(addition);
    } else {
        let mut end = remaining;
        while !addition.is_char_boundary(end) {
            end -= 1;
        }
        target.push_str(&addition[..end]);
        *truncated = true;
    }
}

fn append_untracked_diffs(
    project_path: &Path,
    target: &mut String,
    truncated: &mut bool,
) -> Result<(), String> {
    let output = git_output(
        project_path,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    if !output.status.success() {
        return Err(format!(
            "Unable to list untracked project files: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    for raw_path in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let relative = match std::str::from_utf8(raw_path) {
            Ok(relative) => Path::new(relative),
            Err(_) => continue,
        };
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            continue;
        }
        let file_path = project_path.join(relative);
        let metadata = match fs::symlink_metadata(&file_path) {
            Ok(metadata) if metadata.file_type().is_file() => metadata,
            _ => continue,
        };
        if metadata.len() > MAX_UNTRACKED_FILE_BYTES {
            *truncated = true;
            continue;
        }
        let canonical_file = file_path
            .canonicalize()
            .map_err(|error| format!("Unable to resolve untracked file: {error}"))?;
        if !canonical_file.starts_with(project_path) {
            continue;
        }
        let bytes = fs::read(&canonical_file)
            .map_err(|error| format!("Unable to read untracked file: {error}"))?;
        let Ok(contents) = String::from_utf8(bytes) else {
            let display_path = relative
                .to_string_lossy()
                .replace('\n', "\\n")
                .replace('\r', "\\r");
            append_limited(
                target,
                &format!(
                    "diff --git a/{display_path} b/{display_path}\nBinary or non-UTF-8 file; content not shown.\n"
                ),
                truncated,
            );
            continue;
        };
        let display_path = relative
            .to_string_lossy()
            .replace('\n', "\\n")
            .replace('\r', "\\r");
        let lines: Vec<&str> = contents.lines().collect();
        let mut file_diff = format!(
            "diff --git a/{display_path} b/{display_path}\nnew file mode 100644\n--- /dev/null\n+++ b/{display_path}\n@@ -0,0 +1,{} @@\n",
            lines.len()
        );
        for line in lines {
            file_diff.push('+');
            file_diff.push_str(line);
            file_diff.push('\n');
        }
        append_limited(target, &file_diff, truncated);
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn get_project_diff(
    project_id: String,
    database: State<'_, Database>,
) -> Result<ProjectDiff, String> {
    let path = project_path(&database, &project_id)?;
    let branch = git_text(&path, &["branch", "--show-current"]).unwrap_or_default();
    let changed_files = git_text(&path, &["status", "--short"])?
        .lines()
        .map(str::to_owned)
        .collect();
    let has_head = git_output(&path, &["rev-parse", "--verify", "HEAD"])
        .is_ok_and(|output| output.status.success());
    let args: &[&str] = if has_head {
        &["diff", "--no-color", "--no-ext-diff", "HEAD", "--"]
    } else {
        &["diff", "--no-color", "--no-ext-diff", "--"]
    };
    let mut diff = git_text(&path, args)?;
    if !diff.is_empty() && !diff.ends_with('\n') {
        diff.push('\n');
    }
    let mut truncated = false;
    append_untracked_diffs(&path, &mut diff, &mut truncated)?;
    if diff.len() > MAX_DIFF_BYTES {
        let mut end = MAX_DIFF_BYTES;
        while !diff.is_char_boundary(end) {
            end -= 1;
        }
        diff.truncate(end);
        truncated = true;
    }
    Ok(ProjectDiff {
        branch,
        changed_files,
        diff,
        truncated,
    })
}

fn read_manifest(project_root: &Path, path: &Path) -> Result<Option<String>, String> {
    let canonical_path = match path.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Unable to inspect {}: {error}", path.display())),
    };
    if !canonical_path.starts_with(project_root) {
        return Err(format!(
            "{} resolves outside the project folder.",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    let metadata = fs::metadata(&canonical_path)
        .map_err(|error| format!("Unable to inspect {}: {error}", path.display()))?;
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(format!(
            "{} exceeds the 2 MB manifest limit.",
            path.display()
        ));
    }
    fs::read_to_string(&canonical_path)
        .map(Some)
        .map_err(|error| format!("Unable to read {}: {error}", path.display()))
}

fn read_json_dependencies(
    source: &str,
    manifest: &str,
    dependencies: &mut BTreeMap<(String, String), ProjectDependency>,
) -> Result<(), String> {
    let value: Value = serde_json::from_str(source)
        .map_err(|error| format!("Unable to parse {manifest}: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| format!("{manifest} must contain a JSON object."))?;
    for (field, kind) in [
        ("dependencies", "production"),
        ("devDependencies", "development"),
        ("peerDependencies", "peer"),
        ("optionalDependencies", "optional"),
    ] {
        let Some(items) = object.get(field).and_then(Value::as_object) else {
            continue;
        };
        for (name, version) in items {
            let version = version.as_str().unwrap_or("unspecified");
            dependencies
                .entry((name.clone(), kind.to_owned()))
                .or_insert_with(|| ProjectDependency {
                    name: name.clone(),
                    version: version.to_owned(),
                    kind: kind.to_owned(),
                    manifest: manifest.to_owned(),
                });
        }
    }
    Ok(())
}

fn read_cargo_dependencies(
    source: &str,
    dependencies: &mut BTreeMap<(String, String), ProjectDependency>,
) -> Result<(), String> {
    let value: toml::Value =
        toml::from_str(source).map_err(|error| format!("Unable to parse Cargo.toml: {error}"))?;
    for (field, kind) in [
        ("dependencies", "runtime"),
        ("dev-dependencies", "development"),
        ("build-dependencies", "build"),
    ] {
        let Some(items) = value.get(field).and_then(toml::Value::as_table) else {
            continue;
        };
        for (name, specification) in items {
            let version = match specification {
                toml::Value::String(value) => value.as_str(),
                toml::Value::Table(options) => options
                    .get("version")
                    .and_then(toml::Value::as_str)
                    .or_else(|| options.get("path").and_then(toml::Value::as_str))
                    .or_else(|| options.get("git").and_then(toml::Value::as_str))
                    .unwrap_or("workspace or source override"),
                _ => "unspecified",
            };
            dependencies
                .entry((name.clone(), kind.to_owned()))
                .or_insert_with(|| ProjectDependency {
                    name: name.clone(),
                    version: version.to_owned(),
                    kind: kind.to_owned(),
                    manifest: "Cargo.toml".to_owned(),
                });
        }
    }
    Ok(())
}

fn read_requirements(
    source: &str,
    dependencies: &mut BTreeMap<(String, String), ProjectDependency>,
) {
    for line in source.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() || line.starts_with('-') {
            continue;
        }
        let name_end = line
            .char_indices()
            .find_map(|(index, ch)| {
                (ch.is_whitespace() || matches!(ch, '<' | '>' | '=' | '!' | '~' | '[' | ';' | '@'))
                    .then_some(index)
            })
            .unwrap_or(line.len());
        let name = line[..name_end].trim();
        if name.is_empty() {
            continue;
        }
        let version = line[name_end..].trim();
        let kind = "python";
        dependencies
            .entry((name.to_owned(), kind.to_owned()))
            .or_insert_with(|| ProjectDependency {
                name: name.to_owned(),
                version: if version.is_empty() {
                    "unspecified".to_owned()
                } else {
                    version.to_owned()
                },
                kind: kind.to_owned(),
                manifest: "requirements.txt".to_owned(),
            });
    }
}

#[tauri::command]
pub(crate) fn get_project_dependencies(
    project_id: String,
    database: State<'_, Database>,
) -> Result<ProjectDependencies, String> {
    let path = project_path(&database, &project_id)?;
    let mut manifests = Vec::new();
    let mut dependencies = BTreeMap::new();
    let mut errors = Vec::new();

    for (filename, kind) in [("package.json", "npm"), ("Cargo.toml", "cargo")] {
        match read_manifest(&path, &path.join(filename)) {
            Ok(Some(source)) => {
                manifests.push(kind.to_owned());
                let result = if filename == "package.json" {
                    read_json_dependencies(&source, filename, &mut dependencies)
                } else {
                    read_cargo_dependencies(&source, &mut dependencies)
                };
                if let Err(error) = result {
                    errors.push(error);
                }
            }
            Ok(None) => {}
            Err(error) => errors.push(error),
        }
    }
    match read_manifest(&path, &path.join("requirements.txt")) {
        Ok(Some(source)) => {
            manifests.push("python".to_owned());
            read_requirements(&source, &mut dependencies);
        }
        Ok(None) => {}
        Err(error) => errors.push(error),
    }

    Ok(ProjectDependencies {
        manifests,
        dependencies: dependencies.into_values().collect(),
        errors,
    })
}

fn parse_skill_frontmatter(content: &str) -> (Option<String>, Option<String>) {
    let mut lines = content.lines();
    if lines.next() != Some("---") {
        return (None, None);
    }
    let mut name = None;
    let mut description = None;
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let Some((key, raw_value)) = line.split_once(':') else {
            continue;
        };
        let value = raw_value
            .trim()
            .trim_matches(|character| character == '"' || character == '\'')
            .to_owned();
        match key.trim() {
            "name" => name = Some(value),
            "description" => description = Some(value),
            _ => {}
        }
    }
    (name, description)
}

fn read_skills_dir(
    directory: &Path,
    source: &str,
    errors: &mut Vec<String>,
) -> Result<Vec<ProjectSkill>, String> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "Unable to read {} skills: {error}",
                directory.display()
            ));
        }
    };
    let mut skills = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("Unable to list skills: {error}"))?;
        let folder_name = entry.file_name().to_string_lossy().into_owned();
        let folder_path = entry.path();
        let folder_is_directory = match fs::metadata(&folder_path) {
            Ok(metadata) => metadata.is_dir(),
            Err(error) => {
                errors.push(format!(
                    "Unable to inspect {}: {error}",
                    folder_path.display()
                ));
                continue;
            }
        };
        if !folder_is_directory {
            continue;
        }
        let skill_path = folder_path.join("SKILL.md");
        let metadata = match fs::metadata(&skill_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                errors.push(format!(
                    "Unable to inspect {}: {error}",
                    skill_path.display()
                ));
                continue;
            }
        };
        if metadata.len() > MAX_SKILL_BYTES {
            errors.push(format!(
                "{} exceeds the 512 KB skill limit.",
                skill_path.display()
            ));
            continue;
        }
        let content = match fs::read_to_string(&skill_path) {
            Ok(content) => content,
            Err(error) => {
                errors.push(format!("Unable to read {}: {error}", skill_path.display()));
                continue;
            }
        };
        let (frontmatter_name, frontmatter_description) = parse_skill_frontmatter(&content);
        let has_name = frontmatter_name
            .as_ref()
            .is_some_and(|name| !name.trim().is_empty());
        let name = frontmatter_name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| folder_name.clone());
        let description = frontmatter_description
            .filter(|description| !description.trim().is_empty())
            .unwrap_or_default();
        let warning = match (!has_name, description.is_empty()) {
            (true, true) => Some("Frontmatter is missing name and description".to_owned()),
            (true, false) => Some("Frontmatter is missing name".to_owned()),
            (false, true) => Some("Frontmatter is missing description".to_owned()),
            (false, false) => None,
        };
        skills.push(ProjectSkill {
            name,
            description,
            folder_name,
            source: source.to_owned(),
            content,
            warning,
            shadowed: false,
        });
    }
    skills.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));
    Ok(skills)
}

#[tauri::command]
pub(crate) fn list_project_skills(
    project_id: String,
    database: State<'_, Database>,
) -> Result<ProjectSkills, String> {
    let path = project_path(&database, &project_id)?;
    let mut errors = Vec::new();
    let global_path = env::var_os("APT_CLAUDE_HOME")
        .map(PathBuf::from)
        .or_else(|| home_directory().map(|home| home.join(".claude")))
        .map(|claude_home| claude_home.join("skills"));
    let mut global = match global_path {
        Some(path) => read_skills_dir(&path, "global", &mut errors)?,
        None => {
            errors.push("Unable to locate the user home directory for global skills.".to_owned());
            Vec::new()
        }
    };
    let mut project =
        read_skills_dir(&path.join(".claude").join("skills"), "project", &mut errors)?;
    let global_names: std::collections::HashSet<String> =
        global.iter().map(|skill| skill.name.clone()).collect();
    for skill in &mut project {
        skill.shadowed = global_names.contains(&skill.name);
    }
    global.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));
    Ok(ProjectSkills {
        global,
        project,
        errors,
    })
}

fn home_directory() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[derive(Deserialize)]
struct GitHubRunsResponse {
    workflow_runs: Vec<GitHubWorkflowRun>,
}

#[derive(Deserialize)]
struct GitHubWorkflowRun {
    id: u64,
    name: Option<String>,
    status: String,
    conclusion: Option<String>,
    head_branch: Option<String>,
    html_url: String,
    created_at: String,
    updated_at: String,
}

fn github_repository(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let path = if let Some(path) = remote.strip_prefix("git@github.com:") {
        path
    } else if let Some(path) = remote.strip_prefix("ssh://git@github.com/") {
        path
    } else if let Some(path) = remote.strip_prefix("https://github.com/") {
        path
    } else if let Some(path) = remote.strip_prefix("http://github.com/") {
        path
    } else {
        return None;
    };
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let repository = parts.next()?;
    let safe_component = |component: &str| {
        !component.is_empty()
            && component != "."
            && component != ".."
            && component
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    if !safe_component(owner) || !safe_component(repository) || parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{repository}"))
}

#[tauri::command]
pub(crate) async fn get_project_pipelines(
    project_id: String,
    database: State<'_, Database>,
) -> Result<ProjectPipelines, String> {
    let path = project_path(&database, &project_id)?;
    let remote = match git_text(&path, &["remote", "get-url", "origin"]) {
        Ok(remote) => remote,
        Err(error) => {
            return Ok(ProjectPipelines {
                repository: None,
                configured: false,
                error: Some(format!("Unable to read the GitHub origin remote: {error}")),
                runs: Vec::new(),
            });
        }
    };
    let Some(repository) = github_repository(&remote) else {
        return Ok(ProjectPipelines {
            repository: None,
            configured: false,
            error: Some(
                "GitHub Actions are available for projects with a GitHub origin remote.".to_owned(),
            ),
            runs: Vec::new(),
        });
    };
    let token = match Entry::new(GITHUB_CREDENTIAL_SERVICE, GITHUB_CREDENTIAL_USER)
        .and_then(|entry| entry.get_password())
    {
        Ok(token) => token,
        Err(keyring::Error::NoEntry) => {
            return Ok(ProjectPipelines {
                repository: Some(repository),
                configured: false,
                error: Some("Add a GitHub token in Settings to view workflow runs.".to_owned()),
                runs: Vec::new(),
            });
        }
        Err(error) => return Err(format!("Unable to read the GitHub token: {error}")),
    };

    let url = format!("https://api.github.com/repos/{repository}/actions/runs?per_page=30");
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("AgentControlPlane")
        .build()
        .map_err(|error| format!("Unable to prepare GitHub request: {error}"))?
        .get(url)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| format!("Unable to load GitHub Actions runs: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "GitHub Actions request failed with HTTP {}.",
            response.status()
        ));
    }
    let response: GitHubRunsResponse = response
        .json()
        .await
        .map_err(|error| format!("Unable to decode GitHub Actions runs: {error}"))?;
    Ok(ProjectPipelines {
        repository: Some(repository),
        configured: true,
        error: None,
        runs: response
            .workflow_runs
            .into_iter()
            .map(|run| PipelineRun {
                id: run.id.to_string(),
                name: run.name.unwrap_or_else(|| "GitHub Actions".to_owned()),
                status: run.status,
                conclusion: run.conclusion,
                branch: run.head_branch.unwrap_or_default(),
                url: run.html_url,
                created_at: run.created_at,
                updated_at: run.updated_at,
            })
            .collect(),
    })
}

fn terminal_snapshot(terminal: &LiveTerminal) -> Result<TerminalSnapshot, String> {
    Ok(TerminalSnapshot {
        id: terminal.id.clone(),
        project_id: terminal.project_id.clone(),
        title: terminal.title.clone(),
        created_at: terminal.created_at.clone(),
        alive: terminal.alive.load(Ordering::Acquire),
        exit_code: *terminal
            .exit_code
            .lock()
            .map_err(|_| "Terminal exit status is unavailable.".to_owned())?,
        buffer: terminal
            .buffer
            .lock()
            .map_err(|_| "Terminal output is unavailable.".to_owned())?
            .clone(),
        buffer_end_offset: terminal.buffer_end_offset.load(Ordering::Acquire),
    })
}

#[tauri::command]
pub(crate) fn list_project_terminals(
    project_id: String,
    manager: State<'_, TerminalManager>,
) -> Result<Vec<TerminalSnapshot>, String> {
    let terminals = manager
        .0
        .lock()
        .map_err(|_| "Terminal list is unavailable.".to_owned())?;
    let mut snapshots = terminals
        .values()
        .filter(|terminal| terminal.project_id == project_id)
        .map(|terminal| terminal_snapshot(terminal))
        .collect::<Result<Vec<_>, _>>()?;
    snapshots.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    Ok(snapshots)
}

fn terminal_output_loop(
    mut reader: Box<dyn Read + Send>,
    terminal: Arc<LiveTerminal>,
    app: AppHandle,
) {
    let mut bytes = [0_u8; 8192];
    loop {
        match reader.read(&mut bytes) {
            Ok(0) => break,
            Ok(count) => {
                let chunk = String::from_utf8_lossy(&bytes[..count]).into_owned();
                if let Ok(mut buffer) = terminal.buffer.lock() {
                    buffer.push_str(&chunk);
                    if buffer.len() > MAX_TERMINAL_BUFFER_BYTES {
                        let excess = buffer.len() - MAX_TERMINAL_BUFFER_BYTES;
                        let boundary = (excess..=buffer.len())
                            .find(|index| buffer.is_char_boundary(*index))
                            .unwrap_or(buffer.len());
                        buffer.drain(..boundary);
                    }
                }
                let end_offset = terminal
                    .buffer_end_offset
                    .fetch_add(count as u64, Ordering::AcqRel)
                    + count as u64;
                let event = TerminalOutput {
                    terminal_id: terminal.id.clone(),
                    project_id: terminal.project_id.clone(),
                    chunk,
                    end_offset,
                };
                if let Err(error) = app.emit("project-terminal-output", event) {
                    eprintln!("Unable to deliver project terminal output: {error}");
                }
            }
            Err(error) => {
                eprintln!("Unable to read project terminal output: {error}");
                break;
            }
        }
    }
}

#[tauri::command]
pub(crate) fn create_project_terminal(
    project_id: String,
    cols: u16,
    rows: u16,
    database: State<'_, Database>,
    manager: State<'_, TerminalManager>,
    app: AppHandle,
) -> Result<TerminalSnapshot, String> {
    if !(20..=500).contains(&cols) || !(5..=200).contains(&rows) {
        return Err("Terminal dimensions are outside the supported range.".to_owned());
    }
    let path = project_path(&database, &project_id)?;
    let mut terminals = manager
        .0
        .lock()
        .map_err(|_| "Terminal list is unavailable.".to_owned())?;
    let sequence = terminals
        .values()
        .filter(|terminal| terminal.project_id == project_id)
        .count()
        + 1;
    if sequence > MAX_TERMINALS_PER_PROJECT {
        return Err(format!(
            "A project can have at most {MAX_TERMINALS_PER_PROJECT} open or recently exited terminals."
        ));
    }

    let shell = if cfg!(windows) {
        env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_owned())
    } else {
        env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned())
    };
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| format!("Unable to create a project terminal: {error}"))?;
    let mut command = CommandBuilder::new(shell);
    if !cfg!(windows) {
        command.arg("-i");
    }
    command.cwd(&path);
    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| format!("Unable to start the project shell: {error}"))?;
    let mut killer = child.clone_killer();
    let reader = match pair.master.try_clone_reader() {
        Ok(reader) => reader,
        Err(error) => {
            let _ = killer.kill();
            return Err(format!("Unable to read project terminal output: {error}"));
        }
    };
    let writer = match pair.master.take_writer() {
        Ok(writer) => writer,
        Err(error) => {
            let _ = killer.kill();
            return Err(format!("Unable to write to the project terminal: {error}"));
        }
    };
    let id = new_id("terminal");
    let terminal = Arc::new(LiveTerminal {
        id: id.clone(),
        project_id,
        title: format!("Terminal {sequence}"),
        created_at: chrono_like_now(),
        buffer: Mutex::new(String::new()),
        buffer_end_offset: AtomicU64::new(0),
        alive: AtomicBool::new(true),
        exit_code: Mutex::new(None),
        writer: Mutex::new(writer),
        killer: Mutex::new(killer),
        _master: Mutex::new(pair.master),
    });
    terminals.insert(id, terminal.clone());
    drop(terminals);

    let reader_terminal = terminal.clone();
    let reader_app = app.clone();
    thread::spawn(move || terminal_output_loop(reader, reader_terminal, reader_app));

    let exit_terminal = terminal.clone();
    thread::spawn(move || {
        let exit_code = match child.wait() {
            Ok(status) => Some(status.exit_code()),
            Err(error) => {
                eprintln!("Unable to observe project terminal exit: {error}");
                None
            }
        };
        if let Ok(mut stored_exit_code) = exit_terminal.exit_code.lock() {
            *stored_exit_code = exit_code;
        }
        exit_terminal.alive.store(false, Ordering::Release);
        let event = TerminalExit {
            terminal_id: exit_terminal.id.clone(),
            project_id: exit_terminal.project_id.clone(),
            exit_code,
        };
        if let Err(error) = app.emit("project-terminal-exit", event) {
            eprintln!("Unable to deliver project terminal exit: {error}");
        }
    });

    terminal_snapshot(&terminal)
}

fn chrono_like_now() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}Z", elapsed.as_secs(), elapsed.subsec_millis())
}

#[tauri::command]
pub(crate) fn write_project_terminal(
    project_id: String,
    terminal_id: String,
    data: String,
    manager: State<'_, TerminalManager>,
) -> Result<(), String> {
    if data.len() > MAX_TERMINAL_INPUT_BYTES {
        return Err("Terminal input exceeds the 64 KB limit.".to_owned());
    }
    let terminal = manager
        .0
        .lock()
        .map_err(|_| "Terminal list is unavailable.".to_owned())?
        .get(&terminal_id)
        .cloned()
        .ok_or_else(|| "The terminal is no longer available.".to_owned())?;
    if terminal.project_id != project_id {
        return Err("The terminal does not belong to this project.".to_owned());
    }
    if !terminal.alive.load(Ordering::Acquire) {
        return Err("The terminal shell has already exited.".to_owned());
    }
    terminal
        .writer
        .lock()
        .map_err(|_| "Terminal input is unavailable.".to_owned())?
        .write_all(data.as_bytes())
        .map_err(|error| format!("Unable to write to the project terminal: {error}"))
}

#[tauri::command]
pub(crate) fn resize_project_terminal(
    project_id: String,
    terminal_id: String,
    cols: u16,
    rows: u16,
    manager: State<'_, TerminalManager>,
) -> Result<(), String> {
    if !(20..=500).contains(&cols) || !(5..=200).contains(&rows) {
        return Err("Terminal dimensions are outside the supported range.".to_owned());
    }
    let terminal = manager
        .0
        .lock()
        .map_err(|_| "Terminal list is unavailable.".to_owned())?
        .get(&terminal_id)
        .cloned()
        .ok_or_else(|| "The terminal is no longer available.".to_owned())?;
    if terminal.project_id != project_id {
        return Err("The terminal does not belong to this project.".to_owned());
    }
    if !terminal.alive.load(Ordering::Acquire) {
        return Ok(());
    }
    terminal
        ._master
        .lock()
        .map_err(|_| "Terminal display is unavailable.".to_owned())?
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| format!("Unable to resize the project terminal: {error}"))
}

#[tauri::command]
pub(crate) fn close_project_terminal(
    project_id: String,
    terminal_id: String,
    manager: State<'_, TerminalManager>,
) -> Result<(), String> {
    let mut terminals = manager
        .0
        .lock()
        .map_err(|_| "Terminal list is unavailable.".to_owned())?;
    let terminal = terminals
        .get(&terminal_id)
        .cloned()
        .ok_or_else(|| "The terminal is no longer available.".to_owned())?;
    if terminal.project_id != project_id {
        return Err("The terminal does not belong to this project.".to_owned());
    }
    if terminal.alive.load(Ordering::Acquire) {
        terminal
            .killer
            .lock()
            .map_err(|_| "Terminal process control is unavailable.".to_owned())?
            .kill()
            .map_err(|error| format!("Unable to stop the project terminal: {error}"))?;
    }
    terminals.remove(&terminal_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_remotes_are_restricted_to_github_owner_and_repository() {
        assert_eq!(
            github_repository("git@github.com:octo/project.git"),
            Some("octo/project".to_owned())
        );
        assert_eq!(
            github_repository("https://github.com/octo/project"),
            Some("octo/project".to_owned())
        );
        assert_eq!(github_repository("https://example.com/octo/project"), None);
        assert_eq!(
            github_repository("https://github.com/octo/project/issues"),
            None
        );
        assert_eq!(
            github_repository("https://github.com/octo/project?token=other"),
            None
        );
        assert_eq!(github_repository("https://github.com/../project"), None);
    }

    #[test]
    fn dependency_manifests_are_parsed_by_scope() {
        let mut dependencies = BTreeMap::new();
        read_json_dependencies(
            r#"{"dependencies":{"react":"^19"},"devDependencies":{"vitest":"^3"}}"#,
            "package.json",
            &mut dependencies,
        )
        .expect("package manifest");
        read_requirements("requests>=2.0\n# ignored\n", &mut dependencies);
        assert_eq!(dependencies.len(), 3);
        assert!(dependencies.contains_key(&("react".to_owned(), "production".to_owned())));
        assert!(dependencies.contains_key(&("vitest".to_owned(), "development".to_owned())));
        assert!(dependencies.contains_key(&("requests".to_owned(), "python".to_owned())));
    }

    #[test]
    fn skill_frontmatter_is_tolerant_and_extracts_name_and_description() {
        let (name, description) = parse_skill_frontmatter(
            "---\nname: \"review\"\ndescription: Code review steps\n---\nBody",
        );
        assert_eq!(name.as_deref(), Some("review"));
        assert_eq!(description.as_deref(), Some("Code review steps"));
        assert_eq!(parse_skill_frontmatter("No frontmatter"), (None, None));
    }

    #[test]
    fn project_tasks_persist_status_and_session_links_in_sqlite() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        connection
            .execute_batch(
                "CREATE TABLE projects (id TEXT PRIMARY KEY, path TEXT NOT NULL);
                 CREATE TABLE agent_sessions (id TEXT PRIMARY KEY, project_id TEXT NOT NULL);",
            )
            .expect("core schema");
        initialize_schema(&connection).expect("feature schema");
        connection
            .execute(
                "INSERT INTO projects (id, path) VALUES ('project-1', '/tmp/project-1')",
                [],
            )
            .expect("project row");
        connection
            .execute(
                "INSERT INTO project_tasks (id, project_id, title, description)
                 VALUES ('task-1', 'project-1', 'Ship the feature', 'Add the final screen')",
                [],
            )
            .expect("task row");
        connection
            .execute(
                "INSERT INTO agent_sessions (id, project_id) VALUES ('session-1', 'project-1')",
                [],
            )
            .expect("session row");
        connection
            .execute(
                "UPDATE project_tasks
                 SET status = 'in_progress', session_id = 'session-1'
                 WHERE id = 'task-1' AND project_id = 'project-1'",
                [],
            )
            .expect("link task");

        let tasks = list_tasks(&connection, "project-1").expect("task listing");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, "in_progress");
        assert_eq!(tasks[0].session_id.as_deref(), Some("session-1"));
        assert!(tasks[0].loop_enabled);
        assert_eq!(tasks[0].origin, "user");
    }

    #[test]
    fn existing_tasks_migrate_with_loop_selection_enabled() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        connection
            .execute_batch(
                "CREATE TABLE projects (id TEXT PRIMARY KEY, path TEXT NOT NULL);
                 CREATE TABLE project_tasks (
                   id TEXT PRIMARY KEY NOT NULL,
                   project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                   title TEXT NOT NULL,
                   description TEXT NOT NULL DEFAULT '',
                   status TEXT NOT NULL DEFAULT 'backlog'
                     CHECK(status IN ('backlog', 'in_progress', 'done')),
                   session_id TEXT,
                   created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                   updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                 );
                 INSERT INTO projects (id, path) VALUES ('project-1', '/tmp/project-1');
                 INSERT INTO project_tasks (id, project_id, title)
                   VALUES ('task-1', 'project-1', 'Existing task');",
            )
            .expect("legacy feature schema");

        initialize_schema(&connection).expect("task migration");
        let tasks = list_tasks(&connection, "project-1").expect("migrated tasks");
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].loop_enabled);
        assert_eq!(tasks[0].origin, "user");
    }

    #[test]
    fn loop_selection_claims_only_eligible_backlog_tasks() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        connection
            .execute_batch(
                "CREATE TABLE projects (
                   id TEXT PRIMARY KEY, path TEXT NOT NULL,
                   looping INTEGER NOT NULL DEFAULT 0,
                   agent_task_creation INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE agent_sessions (
                  id TEXT PRIMARY KEY, project_id TEXT NOT NULL, status TEXT NOT NULL
                );",
            )
            .expect("core schema");
        initialize_schema(&connection).expect("feature schema");
        connection
            .execute(
                "INSERT INTO projects (id, path, looping) VALUES ('project-1', '/tmp/project-1', 1)",
                [],
            )
            .expect("project row");
        connection
            .execute_batch(
                "INSERT INTO project_tasks (id, project_id, title) VALUES
                   ('task-a', 'project-1', 'First'),
                   ('task-b', 'project-1', 'Second');
                 UPDATE project_tasks SET loop_enabled = 0 WHERE id = 'task-b';",
            )
            .expect("task rows");
        connection
            .execute(
                "INSERT INTO agent_sessions (id, project_id, status)
                 VALUES ('active-session', 'project-1', 'running')",
                [],
            )
            .expect("active session");
        assert!(
            take_next_loop_task(&connection, "project-1")
                .expect("active session prevents a new loop task")
                .is_none()
        );
        connection
            .execute(
                "UPDATE agent_sessions SET status = 'completed' WHERE id = 'active-session'",
                [],
            )
            .expect("complete active session");

        let first = take_next_loop_task(&connection, "project-1")
            .expect("select task")
            .expect("eligible task");
        assert_eq!(first.id, "task-a");
        assert!(
            take_next_loop_task(&connection, "project-1")
                .expect("active task prevents a second selection")
                .is_none()
        );
        connection
            .execute(
                "UPDATE project_tasks SET status = 'done' WHERE id = 'task-a'",
                [],
            )
            .expect("complete first task");
        assert!(
            take_next_loop_task(&connection, "project-1")
                .expect("excluded task")
                .is_none()
        );
    }

    #[test]
    fn task_completion_requires_project_and_task_loop_opt_in() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        connection
            .execute_batch(
                "CREATE TABLE projects (
                   id TEXT PRIMARY KEY, path TEXT NOT NULL,
                   looping INTEGER NOT NULL DEFAULT 0,
                   agent_task_creation INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE agent_sessions (id TEXT PRIMARY KEY, project_id TEXT NOT NULL);",
            )
            .expect("core schema");
        initialize_schema(&connection).expect("feature schema");
        connection
            .execute_batch(
                "INSERT INTO projects (id, path, looping, agent_task_creation)
                   VALUES ('project-1', '/tmp/project-1', 1, 1);
                 INSERT INTO agent_sessions (id, project_id) VALUES
                   ('session-1', 'project-1'), ('session-2', 'project-1');
                 INSERT INTO project_tasks (id, project_id, title, status, session_id)
                   VALUES ('task-1', 'project-1', 'Auto-accepted', 'in_progress', 'session-1');
                 INSERT INTO project_tasks
                   (id, project_id, title, status, loop_enabled, session_id)
                   VALUES ('task-2', 'project-1', 'Manual review', 'in_progress', 0, 'session-2');",
            )
            .expect("fixture rows");

        assert!(
            finish_project_task_session(&connection, "session-1", true)
                .expect("finish linked task")
        );
        let first_status: String = connection
            .query_row(
                "SELECT status FROM project_tasks WHERE id = 'task-1'",
                [],
                |row| row.get(0),
            )
            .expect("first status");
        assert_eq!(first_status, "done");

        assert!(
            finish_project_task_session(&connection, "session-2", true)
                .expect("finish excluded task")
        );
        let second_status: String = connection
            .query_row(
                "SELECT status FROM project_tasks WHERE id = 'task-2'",
                [],
                |row| row.get(0),
            )
            .expect("second status");
        assert_eq!(second_status, "in_progress");
        finish_project_task_session(&connection, "session-2", false)
            .expect("mark failed task for retry");
        let retry: (String, Option<String>) = connection
            .query_row(
                "SELECT status, session_id FROM project_tasks WHERE id = 'task-2'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("retry status");
        assert_eq!(retry, ("backlog".to_owned(), None));
    }

    #[test]
    fn agent_task_blocks_create_deduplicated_backlog_proposals() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        connection
            .execute_batch(
                "CREATE TABLE projects (
                   id TEXT PRIMARY KEY, path TEXT NOT NULL,
                   looping INTEGER NOT NULL DEFAULT 0,
                   agent_task_creation INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE agent_sessions (id TEXT PRIMARY KEY, project_id TEXT NOT NULL);
                 CREATE TABLE session_messages (
                   id TEXT PRIMARY KEY, session_id TEXT NOT NULL, sequence INTEGER NOT NULL,
                   role TEXT NOT NULL, content TEXT NOT NULL DEFAULT ''
                 );",
            )
            .expect("core schema");
        initialize_schema(&connection).expect("feature schema");
        connection
            .execute_batch(
                "INSERT INTO projects (id, path, agent_task_creation)
                   VALUES ('project-1', '/tmp/project-1', 1);
                 INSERT INTO agent_sessions (id, project_id) VALUES ('session-1', 'project-1');
                 INSERT INTO session_messages (id, session_id, sequence, role, content)
                   VALUES ('message-1', 'session-1', 1, 'assistant',
                     'Follow-up: ```apt-task
                     {\"title\":\"Add audit log\",\"purpose\":\"Keep a history\",\"acceptanceCriteria\":[\"Users can inspect events\"]}
                     ```');
                 INSERT INTO project_tasks (id, project_id, title)
                   VALUES ('existing', 'project-1', 'ADD AUDIT LOG');",
            )
            .expect("fixture rows");

        create_agent_task_proposals(&connection, "session-1").expect("create proposals");
        assert_eq!(
            list_tasks(&connection, "project-1")
                .expect("task list")
                .len(),
            1
        );
        connection
            .execute("DELETE FROM project_tasks WHERE id = 'existing'", [])
            .expect("remove duplicate");
        create_agent_task_proposals(&connection, "session-1").expect("create proposal");
        let tasks = list_tasks(&connection, "project-1").expect("task list");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].origin, "agent");
        assert!(tasks[0].description.contains("- Users can inspect events"));
    }
}
