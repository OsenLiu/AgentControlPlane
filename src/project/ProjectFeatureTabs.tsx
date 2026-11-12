import { FormEvent, ReactNode, useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { Terminal as XTerm } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";

type ProjectReference = {
  id: string;
  name: string;
};

type ProjectTask = {
  id: string;
  projectId: string;
  title: string;
  description: string;
  mode: "acceptEdits" | "auto";
  model: string;
  stepBudget: number;
  stepsUsed: number;
  autoApprove: boolean;
  status: "backlog" | "in_progress" | "done";
  loopEnabled: boolean;
  origin: "user" | "agent";
  sessionId: string | null;
  createdAt: string;
  updatedAt: string;
  archived: boolean;
};

const COPILOT_MODEL_GROUPS = [
  {
    provider: "OpenAI",
    models: [
      { id: "gpt-5.3-codex", label: "GPT-5.3-Codex" },
      { id: "gpt-5.4-nano", label: "GPT-5.4 nano" },
      { id: "gpt-5.6-luna", label: "GPT-5.6 Luna" },
      { id: "gpt-5.6-sol", label: "GPT-5.6 Sol" },
      { id: "gpt-5.6-terra", label: "GPT-5.6 Terra" },
      { id: "gpt-6-astra", label: "GPT-6 Astra" },
      { id: "gpt-6-luna", label: "GPT-6 Luna" },
      { id: "gpt-6-sol", label: "GPT-6 Sol" },
      { id: "gpt-6.1-sol", label: "GPT-6.1 Sol" },
      { id: "gpt-6.1-terra", label: "GPT-6.1 Terra" },
    ],
  },
  {
    provider: "Anthropic",
    models: [
      { id: "claude-fable-5", label: "Claude Fable 5" },
      { id: "claude-fable-5.1", label: "Claude Fable 5.1" },
      { id: "claude-fable-5.2", label: "Claude Fable 5.2" },
      { id: "claude-haiku-4.5", label: "Claude Haiku 4.5" },
      { id: "claude-haiku-5.5", label: "Claude Haiku 5.5" },
      { id: "claude-opus-4.8", label: "Claude Opus 4.8" },
      { id: "claude-opus-4.8-fast", label: "Claude Opus 4.8 (fast mode, preview)" },
      { id: "claude-opus-5", label: "Claude Opus 5" },
      { id: "claude-opus-5.5", label: "Claude Opus 5.5" },
      { id: "claude-sonnet-4.6", label: "Claude Sonnet 4.6" },
      { id: "claude-sonnet-5", label: "Claude Sonnet 5" },
      { id: "claude-sonnet-5.5", label: "Claude Sonnet 5.5" },
    ],
  },
  {
    provider: "Google",
    models: [
      { id: "gemini-3.8-flash", label: "Gemini 3.8 Flash" },
      { id: "gemini-4-argon", label: "Gemini 4 Argon" },
      { id: "gemini-4-carbon", label: "Gemini 4 Carbon" },
    ],
  },
  {
    provider: "Microsoft",
    models: [{ id: "mai-code-1.1-flash", label: "MAI-Code-1.1-Flash" }],
  },
  {
    provider: "Moonshot AI",
    models: [{ id: "kimi-k3", label: "Kimi K3" }],
  },
  {
    provider: "xAI",
    models: [
      { id: "grok-4.5", label: "Grok 4.5" },
      { id: "grok-4.6", label: "Grok 4.6" },
      { id: "grok-4.7", label: "Grok 4.7" },
    ],
  },
] as const;

const COPILOT_MODEL_IDS = new Set<string>(
  COPILOT_MODEL_GROUPS.flatMap(({ models }) => models.map(({ id }) => id)),
);

type ProjectDiff = {
  branch: string;
  changedFiles: string[];
  diff: string;
  truncated: boolean;
};

type DiffFile = {
  path: string;
  content: string;
};

type ProjectDependency = {
  name: string;
  version: string;
  kind: string;
  manifest: string;
};

type ProjectDependencies = {
  manifests: string[];
  dependencies: ProjectDependency[];
  errors: string[];
};

type ProjectSkill = {
  name: string;
  description: string;
  folderName: string;
  source: "global" | "project";
  content: string;
  warning: string | null;
  shadowed: boolean;
};

type ProjectSkills = {
  global: ProjectSkill[];
  project: ProjectSkill[];
  errors: string[];
};

type PipelineRun = {
  id: string;
  name: string;
  status: string;
  conclusion: string | null;
  branch: string;
  url: string;
  createdAt: string;
  updatedAt: string;
};

type ProjectPipelines = {
  repository: string | null;
  configured: boolean;
  error: string | null;
  runs: PipelineRun[];
};

type TerminalSnapshot = {
  id: string;
  projectId: string;
  title: string;
  createdAt: string;
  alive: boolean;
  exitCode: number | null;
  buffer: string;
  bufferEndOffset: number;
};

type TerminalOutputEvent = {
  terminalId: string;
  projectId: string;
  chunk: string;
  endOffset: number;
};

type TerminalExitEvent = {
  terminalId: string;
  projectId: string;
  exitCode: number | null;
};

function messageFromError(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

function FeatureFrame({
  eyebrow,
  title,
  description,
  action,
  children,
}: {
  eyebrow: string;
  title: string;
  description: string;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="project-feature" aria-label={title}>
      <header className="project-feature-header">
        <div>
          <p className="project-feature-eyebrow">{eyebrow}</p>
          <h2>{title}</h2>
          <p>{description}</p>
        </div>
        {action}
      </header>
      {children}
    </section>
  );
}

function Feedback({ error }: { error: string }) {
  return error ? <p className="project-feature-error" role="alert">{error}</p> : null;
}

export function ProjectTasksTab({
  project,
  onOpenSession,
}: {
  project: ProjectReference;
  onOpenSession: (sessionId: string) => void;
}) {
  const [tasks, setTasks] = useState<ProjectTask[]>([]);
  const [selectedTaskId, setSelectedTaskId] = useState("");
  const [filter, setFilter] = useState("");
  const [statusFilter, setStatusFilter] = useState<ProjectTask["status"] | "all">("all");
  const [archivedView, setArchivedView] = useState(false);
  const [taskDialog, setTaskDialog] = useState<ProjectTask | "new" | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [confirmDeleteId, setConfirmDeleteId] = useState("");
  const [startingTaskId, setStartingTaskId] = useState("");

  const loadTasks = useCallback(async () => {
    setError("");
    try {
      const loadedTasks = await invoke<ProjectTask[]>("list_project_tasks", { projectId: project.id });
      setTasks(loadedTasks);
      setSelectedTaskId((current) =>
        current && loadedTasks.some((task) => task.id === current && task.archived === archivedView)
          ? current
          : loadedTasks.find((task) => task.archived === archivedView)?.id ?? "",
      );
    } catch (loadError) {
      setError(messageFromError(loadError));
    } finally {
      setLoading(false);
    }
  }, [archivedView, project.id]);

  const currentTasks = tasks.filter((task) => task.archived === archivedView);
  const archivedTaskCount = tasks.filter((task) => task.archived).length;
  const visibleTasks = currentTasks.filter((task) => {
    const searchable = `${task.title} ${task.description}`.toLowerCase();
    return searchable.includes(filter.trim().toLowerCase()) &&
      (statusFilter === "all" || task.status === statusFilter);
  });
  const selectedTask = tasks.find((task) => task.id === selectedTaskId && task.archived === archivedView);

  useEffect(() => {
    void loadTasks();
  }, [loadTasks]);

  useEffect(() => {
    let disposed = false;
    let unsubscribe: (() => void) | undefined;
    void listen<string>("agent-session-updated", () => {
      if (!disposed) void loadTasks();
    }).then((unlisten) => {
      if (disposed) unlisten();
      else unsubscribe = unlisten;
    }).catch((listenError) => {
      if (!disposed) setError(messageFromError(listenError));
    });
    return () => {
      disposed = true;
      unsubscribe?.();
    };
  }, [loadTasks]);

  async function saveTask(input: ProjectTaskInput) {
    if (!taskDialog) return;
    setSaving(true);
    setError("");
    try {
      const saved = taskDialog === "new"
        ? await invoke<ProjectTask>("create_project_task", { projectId: project.id, ...input })
        : await invoke<ProjectTask>("update_project_task", {
            projectId: project.id,
            taskId: taskDialog.id,
            ...input,
          });
      setSelectedTaskId(saved.id);
      setTaskDialog(null);
      await loadTasks();
    } catch (saveError) {
      setError(messageFromError(saveError));
    } finally {
      setSaving(false);
    }
  }

  async function setTaskStatus(task: ProjectTask, status: ProjectTask["status"]) {
    setError("");
    try {
      await invoke("set_project_task_status", {
        projectId: project.id,
        taskId: task.id,
        status,
      });
      if (status === "done") {
        await invoke("continue_project_loop", { projectId: project.id });
      }
      await loadTasks();
    } catch (updateError) {
      setError(messageFromError(updateError));
    }
  }

  async function setTaskLoopEnabled(task: ProjectTask, enabled: boolean) {
    setError("");
    try {
      await invoke("set_project_task_loop_enabled", {
        projectId: project.id,
        taskId: task.id,
        enabled,
      });
      await loadTasks();
    } catch (updateError) {
      setError(messageFromError(updateError));
    }
  }

  async function setTaskArchived(task: ProjectTask, archived: boolean) {
    setError("");
    try {
      await invoke("set_project_task_archived", {
        projectId: project.id,
        taskId: task.id,
        archived,
      });
      await loadTasks();
    } catch (archiveError) {
      setError(messageFromError(archiveError));
    }
  }

  async function startTask(task: ProjectTask) {
    setStartingTaskId(task.id);
    setError("");
    try {
      await invoke<{ id: string }>("start_copilot_session", {
        projectId: project.id,
        task: `${task.title}\n\n${task.description}`.trim(),
        taskId: task.id,
      });
      await loadTasks();
    } catch (startError) {
      setError(messageFromError(startError));
    } finally {
      setStartingTaskId("");
    }
  }

  async function deleteTask(task: ProjectTask) {
    setError("");
    try {
      await invoke("delete_project_task", { projectId: project.id, taskId: task.id });
      setConfirmDeleteId("");
      await loadTasks();
    } catch (deleteError) {
      setError(messageFromError(deleteError));
    }
  }

  return (
    <section className="project-feature project-tasks-feature" aria-label="Project tasks">
      <Feedback error={error} />
      <div className="project-task-workspace">
        <aside className="project-task-sidebar" aria-label="Task list">
          <header className="project-task-sidebar-header">
            <div>
              <h2>{archivedView ? "Archived tasks" : "Tasks"}</h2>
              <span>{currentTasks.length} {currentTasks.length === 1 ? "task" : "tasks"}</span>
            </div>
            <div className="project-task-sidebar-actions">
              <button
                className="project-feature-button"
                type="button"
                onClick={() => setArchivedView((showArchived) => !showArchived)}
              >
                {archivedView ? "Active tasks" : `Archived tasks (${archivedTaskCount})`}
              </button>
              {!archivedView && (
                <button
                  className="project-feature-primary project-task-new-button"
                  type="button"
                  onClick={() => setTaskDialog("new")}
                >
                  + New task
                </button>
              )}
            </div>
          </header>

          <div className="project-task-list-controls">
            <input
              type="search"
              value={filter}
              onChange={(event) => setFilter(event.target.value)}
              placeholder="Filter tasks…"
              aria-label="Filter tasks"
            />
            <select
              aria-label="Filter by status"
              value={statusFilter}
              onChange={(event) => setStatusFilter(event.target.value as ProjectTask["status"] | "all")}
            >
              <option value="all">All statuses</option>
              <option value="backlog">Backlog</option>
              <option value="in_progress">In progress</option>
              <option value="done">Done</option>
            </select>
          </div>

          {loading && currentTasks.length === 0 && tasks.length === 0 ? (
            <p className="project-feature-state" role="status">Loading project tasks…</p>
          ) : currentTasks.length === 0 ? (
            <div className="project-task-list-empty">
              <h3>{archivedView ? "No archived tasks" : tasks.length === 0 ? "No tasks yet" : "No active tasks"}</h3>
              <p>
                {archivedView
                  ? "Tasks you archive will appear here, where you can restore them."
                  : tasks.length === 0
                    ? "Add the first task to this project backlog."
                    : "All tasks are archived. Switch to archived tasks to restore one."}
              </p>
              {!archivedView && tasks.length === 0 && (
                <button className="project-feature-quiet" type="button" onClick={() => setTaskDialog("new")}>
                  Create a task
                </button>
              )}
            </div>
          ) : (
            <div className="project-task-list">
              {visibleTasks.map((task) => (
                <button
                  className={`project-task-row ${selectedTaskId === task.id ? "active" : ""} ${task.status}`}
                  key={task.id}
                  type="button"
                  aria-pressed={selectedTaskId === task.id}
                  onClick={() => setSelectedTaskId(task.id)}
                >
                  <span className="project-task-row-title">{task.title}</span>
                  <span className="project-task-row-meta">
                    <span className={`project-task-status ${task.status}`}>
                      {task.status.replace("_", " ")}
                    </span>
                    {task.archived && <span className="project-task-row-archived">Archived</span>}
                    {task.loopEnabled && task.status === "backlog" && <span className="project-task-row-loop">In loop</span>}
                  </span>
                </button>
              ))}
              {visibleTasks.length === 0 && (
                <p className="project-task-list-empty" role="status">No tasks match these filters.</p>
              )}
            </div>
          )}
        </aside>

        {selectedTask ? (
            <ProjectTaskDetail
              task={selectedTask}
              confirmDelete={confirmDeleteId === selectedTask.id}
              starting={startingTaskId === selectedTask.id}
              onEdit={() => setTaskDialog(selectedTask)}
              onSetArchived={(archived) => void setTaskArchived(selectedTask, archived)}
              onOpenSession={onOpenSession}
              onStart={() => void startTask(selectedTask)}
              onSetStatus={(status) => void setTaskStatus(selectedTask, status)}
              onSetLoop={(enabled) => void setTaskLoopEnabled(selectedTask, enabled)}
              onDelete={() => void deleteTask(selectedTask)}
              onConfirmDelete={() => setConfirmDeleteId(selectedTask.id)}
              onCancelDelete={() => setConfirmDeleteId("")}
            />
        ) : (
          <div className="project-task-detail project-task-detail-empty">
            <span className="project-task-detail-mark" aria-hidden="true">✓</span>
            <h2>{tasks.length === 0 ? "Your backlog starts here" : archivedView ? "No archived task selected" : "No active task selected"}</h2>
            <p>{tasks.length === 0 ? "Create a task to give your agent a clear goal, or add context for later." : "Choose a task from the list to see its details."}</p>
          </div>
        )}
      </div>
      {taskDialog && (
        <ProjectTaskDialog
          initial={taskDialog === "new" ? null : taskDialog}
          error={error}
          saving={saving}
          onClose={() => {
            setTaskDialog(null);
            setError("");
          }}
          onSave={(input) => void saveTask(input)}
        />
      )}
    </section>
  );
}

type ProjectTaskInput = {
  title: string;
  description: string;
  mode: ProjectTask["mode"];
  model: string;
  stepBudget: number;
  autoApprove: boolean;
};

function TaskSettingHelp({ label, text }: { label: string; text: string }) {
  return (
    <button
      className="project-info-button task-setting-help"
      type="button"
      aria-label={`${label} help: ${text}`}
      aria-description={text}
      title={text}
    >
      !
    </button>
  );
}

function ProjectTaskDialog({
  initial,
  error,
  saving,
  onClose,
  onSave,
}: {
  initial: ProjectTask | null;
  error: string;
  saving: boolean;
  onClose: () => void;
  onSave: (input: ProjectTaskInput) => void;
}) {
  const dialogRef = useRef<HTMLElement>(null);
  const [title, setTitle] = useState(initial?.title ?? "");
  const [description, setDescription] = useState(initial?.description ?? "");
  const [mode, setMode] = useState<ProjectTask["mode"]>(initial?.mode ?? "acceptEdits");
  const initialModel = initial?.model ?? "auto";
  const [modelChoice, setModelChoice] = useState(
    initialModel === "auto" || COPILOT_MODEL_IDS.has(initialModel) ? initialModel : "custom",
  );
  const [customModel, setCustomModel] = useState(
    initialModel !== "auto" && !COPILOT_MODEL_IDS.has(initialModel) ? initialModel : "",
  );
  const [stepBudget, setStepBudget] = useState(initial?.stepBudget ?? 30);
  const [autoApprove, setAutoApprove] = useState(initial?.autoApprove ?? false);

  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape" && !saving) onClose();
    }
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose, saving]);

  const model = modelChoice === "custom" ? customModel.trim() : modelChoice;
  const valid = title.trim().length > 0 &&
    description.trim().length > 0 &&
    model.length > 0 &&
    model.length <= 120 &&
    Number.isSafeInteger(stepBudget) &&
    stepBudget >= 1;

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (valid) onSave({ title, description, mode, model, stepBudget, autoApprove });
  }

  return (
    <div
      className="task-dialog-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget && !saving) onClose();
      }}
    >
      <section
        ref={dialogRef}
        className="task-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="task-dialog-title"
        onKeyDown={(event) => {
          if (event.key !== "Tab") return;
          const focusable = dialogRef.current?.querySelectorAll<HTMLElement>(
            'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
          );
          if (!focusable?.length) return;
          const first = focusable[0];
          const last = focusable[focusable.length - 1];
          if (event.shiftKey && document.activeElement === first) {
            event.preventDefault();
            last.focus();
          } else if (!event.shiftKey && document.activeElement === last) {
            event.preventDefault();
            first.focus();
          }
        }}
      >
        <header className="task-dialog-header">
          <h2 id="task-dialog-title">{initial ? "Edit task" : "New task"}</h2>
          <button className="project-feature-quiet" type="button" disabled={saving} onClick={onClose}>Close</button>
        </header>
        <form onSubmit={submit}>
          <label className="task-dialog-field">
            <span>Task title</span>
            <input
              autoFocus
              required
              maxLength={200}
              value={title}
              onChange={(event) => setTitle(event.target.value)}
              placeholder="Task title"
            />
          </label>
          <label className="task-dialog-field">
            <span>Brief</span>
            <textarea
              required
              maxLength={20_000}
              rows={4}
              value={description}
              onChange={(event) => setDescription(event.target.value)}
              placeholder="What should the agent build? Be specific about the goal."
            />
          </label>
          <div className="task-dialog-setting">
            <label htmlFor="project-task-mode">Mode</label>
            <TaskSettingHelp
              label="Mode"
              text="Accept edits automatically approves file-edit requests while asking before other tools run. Auto approves all tool permissions and should be used only for trusted projects."
            />
            <select id="project-task-mode" value={mode} onChange={(event) => setMode(event.target.value as ProjectTask["mode"])}>
              <option value="acceptEdits">Accept edits</option>
              <option value="auto">Auto</option>
            </select>
          </div>
          <div className="task-dialog-setting">
            <label htmlFor="project-task-model">Model</label>
            <TaskSettingHelp
              label="Model"
              text="The model choices follow GitHub Copilot's supported-model catalog. Availability depends on your Copilot plan and the model support in your Copilot CLI/ACP version. Auto uses the CLI default; enter a custom model ID if needed."
            />
            <select id="project-task-model" value={modelChoice} onChange={(event) => setModelChoice(event.target.value)}>
              <option value="auto">Auto (default)</option>
              {COPILOT_MODEL_GROUPS.map(({ provider, models }) => (
                <optgroup key={provider} label={provider}>
                  {models.map(({ id, label }) => <option key={id} value={id}>{label}</option>)}
                </optgroup>
              ))}
              <option value="custom">Custom model ID…</option>
            </select>
            {modelChoice === "custom" && (
              <input
                className="task-dialog-custom-model"
                required
                maxLength={120}
                value={customModel}
                onChange={(event) => setCustomModel(event.target.value)}
                placeholder="Copilot model ID"
                aria-label="Custom Copilot model ID"
              />
            )}
          </div>
          <div className="task-dialog-setting">
            <label htmlFor="project-task-step-budget">Step budget</label>
            <TaskSettingHelp
              label="Step budget"
              text="Maximum number of agent replies in this task session. When the limit is reached, the session pauses and you can raise the budget to continue."
            />
            <input
              id="project-task-step-budget"
              type="number"
              min={1}
              step={1}
              required
              value={stepBudget}
              onChange={(event) => setStepBudget(Number(event.target.value))}
            />
          </div>
          <div className="task-dialog-setting task-dialog-auto-approve">
            <label htmlFor="project-task-auto-approve">Auto-approve</label>
            <TaskSettingHelp
              label="Auto-approve"
              text="Automatically mark this task done when Copilot reports completion and the required quality gate passes. This does not auto-approve tool permission requests."
            />
            <input
              id="project-task-auto-approve"
              type="checkbox"
              checked={autoApprove}
              onChange={(event) => setAutoApprove(event.target.checked)}
            />
          </div>
          {error && <p className="project-feature-error" role="alert">{error}</p>}
          <footer className="task-dialog-actions">
            <button className="project-feature-button" type="button" disabled={saving} onClick={onClose}>Cancel</button>
            <button className="project-feature-primary" type="submit" disabled={saving || !valid}>
              {saving ? "Saving…" : initial ? "Save task" : "Create task"}
            </button>
          </footer>
        </form>
      </section>
    </div>
  );
}

function ProjectTaskDetail({
  task,
  confirmDelete,
  starting,
  onEdit,
  onSetArchived,
  onOpenSession,
  onStart,
  onSetStatus,
  onSetLoop,
  onDelete,
  onConfirmDelete,
  onCancelDelete,
}: {
  task: ProjectTask;
  confirmDelete: boolean;
  starting: boolean;
  onEdit: () => void;
  onSetArchived: (archived: boolean) => void;
  onOpenSession: (sessionId: string) => void;
  onStart: () => void;
  onSetStatus: (status: ProjectTask["status"]) => void;
  onSetLoop: (enabled: boolean) => void;
  onDelete: () => void;
  onConfirmDelete: () => void;
  onCancelDelete: () => void;
}) {
  const linkedSessionId = task.sessionId;

  return (
    <article className="project-task-detail" aria-labelledby="selected-task-title">
      <header className="project-task-detail-header">
        <div className="project-task-detail-heading">
          <h2 id="selected-task-title">{task.title}</h2>
          <div className="project-task-detail-badges">
            <span className={`project-task-status ${task.status}`}>{task.status.replace("_", " ")}</span>
            {task.archived && <span className="project-task-archived-badge">Archived</span>}
            {task.origin === "agent" && <span className="project-task-origin">Agent proposal</span>}
            {task.status === "backlog" && (
              <span className={`project-task-loop-badge ${task.loopEnabled ? "enabled" : ""}`}>
                {task.loopEnabled ? "In project loop" : "Loop off"}
              </span>
            )}
          </div>
        </div>
        <div className="project-task-detail-header-actions">
          <button className="project-feature-button" type="button" onClick={onEdit}>Edit</button>
          <button
            className="project-feature-button"
            type="button"
            onClick={() => onSetArchived(!task.archived)}
          >
            {task.archived ? "Unarchive" : "Archive"}
          </button>
          {confirmDelete ? (
            <div className="project-task-confirm">
              <span>Delete this task?</span>
              <button className="project-feature-danger" type="button" onClick={onDelete}>Delete</button>
              <button className="project-feature-button" type="button" onClick={onCancelDelete}>Cancel</button>
            </div>
          ) : (
            <button className="project-feature-quiet" type="button" onClick={onConfirmDelete}>Delete</button>
          )}
        </div>
      </header>

      <section className="project-task-description-section">
        <h3>Description</h3>
        <p>{task.description || "No description added."}</p>
      </section>

      {linkedSessionId && (
        <button
          className="project-task-session-section"
          type="button"
          aria-label={`Open linked session ${linkedSessionId}`}
          onClick={() => onOpenSession(linkedSessionId)}
        >
          <span className="project-task-session-icon" aria-hidden="true">↗</span>
          <div>
            <strong>Linked session</strong>
            <code>{linkedSessionId}</code>
          </div>
          <span className="project-task-session-open">Open session</span>
        </button>
      )}

      <section className="project-task-settings-summary" aria-label="Task settings">
        <span>{task.mode === "acceptEdits" ? "Accept edits" : "Auto"} mode</span>
        <span>{task.model === "auto" ? "Auto model" : task.model}</span>
        <span>{task.stepsUsed}/{task.stepBudget} steps</span>
        {task.autoApprove && <span>Auto-approve</span>}
      </section>

      <section className="project-task-actions-panel" aria-label="Task actions">
        <div className="project-task-actions-copy">
          <h3>{task.archived
            ? "Task archived"
            : task.status === "done"
              ? "Task complete"
              : task.status === "in_progress"
                ? "Work in progress"
                : "Ready to delegate"}</h3>
          <p>
            {task.archived
              ? "This task is archived. Unarchive it to return it to the active task list."
              : task.status === "done"
                ? "This task is complete. Reopen it if more work is needed."
                : task.status === "in_progress"
                  ? "A session has been started for this task."
                  : "Start a Copilot session now, or keep this task in your project backlog."}
          </p>
        </div>
        {!task.archived && (
          <div className="project-task-action-controls">
            {task.status === "backlog" && (
              <label className="project-task-loop-toggle">
                <input
                  type="checkbox"
                  checked={task.loopEnabled}
                  aria-label={`Include ${task.title} in project loop`}
                  onChange={(event) => onSetLoop(event.target.checked)}
                />
                <span>Include in project loop</span>
              </label>
            )}
            <div className="project-task-action-buttons">
              {task.status === "backlog" && (
                <button className="project-feature-primary" type="button" disabled={starting} onClick={onStart}>
                  {starting ? "Starting…" : "Start session"}
                </button>
              )}
              {task.status !== "done" ? (
                <button className="project-feature-button" type="button" onClick={() => onSetStatus("done")}>
                  Mark done
                </button>
              ) : (
                <button className="project-feature-primary" type="button" onClick={() => onSetStatus("backlog")}>
                  Reopen task
                </button>
              )}
            </div>
          </div>
        )}
      </section>
    </article>
  );
}

function parseDiffFiles(diff: string): DiffFile[] {
  const sections = diff.split(/(?=^diff --git )/m).filter((section) => section.trim());
  if (sections.length === 0 && diff.trim()) return [{ path: "Working tree diff", content: diff }];
  return sections.map((content) => {
    const header = content.match(/^diff --git a\/(.+?) b\/(.+)$/m);
    return { path: header?.[2] ?? "Changed file", content };
  });
}

export function ProjectDiffsTab({ project }: { project: ProjectReference }) {
  const [snapshot, setSnapshot] = useState<ProjectDiff | null>(null);
  const [selectedPath, setSelectedPath] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");

  const loadDiff = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      const result = await invoke<ProjectDiff>("get_project_diff", { projectId: project.id });
      setSnapshot(result);
      const files = parseDiffFiles(result.diff);
      setSelectedPath((current) => files.some((file) => file.path === current) ? current : files[0]?.path ?? "");
    } catch (loadError) {
      setError(messageFromError(loadError));
    } finally {
      setLoading(false);
    }
  }, [project.id]);

  useEffect(() => {
    void loadDiff();
  }, [loadDiff]);

  const files = parseDiffFiles(snapshot?.diff ?? "");
  const selectedFile = files.find((file) => file.path === selectedPath);

  return (
    <FeatureFrame
      eyebrow="WORKING TREE"
      title="Diffs"
      description={snapshot?.branch ? `Review uncommitted changes on ${snapshot.branch}.` : "Review uncommitted changes in this repository."}
      action={<button className="project-feature-button" type="button" onClick={() => void loadDiff()} disabled={loading}>Refresh diff</button>}
    >
      <Feedback error={error} />
      {loading && !snapshot ? (
        <p className="project-feature-state" role="status">Reading Git changes…</p>
      ) : !snapshot || (snapshot.changedFiles.length === 0 && !snapshot.diff) ? (
        <div className="project-feature-empty">
          <span className="project-feature-empty-mark" aria-hidden="true">✓</span>
          <h3>Working tree is clean</h3>
          <p>Uncommitted changes will appear here for review.</p>
        </div>
      ) : (
        <>
          {snapshot.truncated && <p className="project-feature-warning" role="status">The diff is large; only the first 2 MB are shown.</p>}
          <div className="project-diff-layout">
            <aside className="project-diff-files" aria-label="Changed files">
              <div className="project-diff-files-heading">
                <strong>Changed files</strong><span>{snapshot.changedFiles.length}</span>
              </div>
              {files.length ? files.map((file) => (
                <button
                  type="button"
                  className={selectedPath === file.path ? "active" : ""}
                  key={file.path}
                  onClick={() => setSelectedPath(file.path)}
                  title={file.path}
                >
                  <span aria-hidden="true">▤</span>{file.path}
                </button>
              )) : snapshot.changedFiles.map((file) => (
                <div className="project-diff-unavailable-file" key={file}>{file}</div>
              ))}
            </aside>
            <pre className="project-diff-content" aria-label="Unified diff">
              {selectedFile?.content ?? "Select a changed file to inspect its diff."}
            </pre>
          </div>
        </>
      )}
    </FeatureFrame>
  );
}

function ProjectTerminalPane({
  projectId,
  terminal,
  onReady,
  onData,
  onResize,
  onError,
}: {
  projectId: string;
  terminal: TerminalSnapshot;
  onReady: (id: string, instance: XTerm | null, endOffset: number) => void;
  onData: (id: string, data: string) => void;
  onResize: (id: string, cols: number, rows: number) => void;
  onError: (error: string) => void;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const onReadyRef = useRef(onReady);
  const onDataRef = useRef(onData);
  const onResizeRef = useRef(onResize);
  const onErrorRef = useRef(onError);
  onReadyRef.current = onReady;
  onDataRef.current = onData;
  onResizeRef.current = onResize;
  onErrorRef.current = onError;

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    let disposed = false;
    let instance: XTerm | undefined;
    let dataSubscription: { dispose: () => void } | undefined;
    let resizeObserver: ResizeObserver | undefined;
    let resizeFrame = 0;

    void Promise.all([
      import("@xterm/xterm"),
      import("@xterm/addon-fit"),
    ]).then(([terminalModule, fitModule]) => {
      if (disposed) return;
      instance = new terminalModule.Terminal({
        cursorBlink: true,
        fontFamily: '"JetBrains Mono", "Cascadia Code", monospace',
        fontSize: 11,
        scrollback: 5000,
        screenReaderMode: true,
        theme: {
          background: "#080e18",
          foreground: "#c6d3e7",
          cursor: "#b7aaff",
          selectionBackground: "#514487",
          black: "#111827",
          red: "#e47b8a",
          green: "#68c99a",
          yellow: "#e0bd69",
          blue: "#8399ed",
          magenta: "#b39bff",
          cyan: "#75c9d3",
          white: "#dce4f3",
          brightBlack: "#65738e",
          brightRed: "#ff9ba8",
          brightGreen: "#90e5b7",
          brightYellow: "#f4d98d",
          brightBlue: "#a7b7ff",
          brightMagenta: "#d1baff",
          brightCyan: "#9ce3e7",
          brightWhite: "#ffffff",
        },
      });
      const fit = new fitModule.FitAddon();
      instance.loadAddon(fit);
      instance.open(container);
      fit.fit();
      instance.write(terminal.buffer);
      instance.focus();
      onReadyRef.current(terminal.id, instance, terminal.bufferEndOffset);
      dataSubscription = instance.onData((data) => onDataRef.current(terminal.id, data));
      resizeObserver = new ResizeObserver(() => {
        cancelAnimationFrame(resizeFrame);
        resizeFrame = requestAnimationFrame(() => {
          fit.fit();
          if (instance) onResizeRef.current(terminal.id, instance.cols, instance.rows);
        });
      });
      resizeObserver.observe(container);
    }).catch((error: unknown) => {
      if (!disposed) onErrorRef.current(messageFromError(error));
    });

    return () => {
      disposed = true;
      cancelAnimationFrame(resizeFrame);
      resizeObserver?.disconnect();
      dataSubscription?.dispose();
      if (instance) {
        onReadyRef.current(terminal.id, null, terminal.bufferEndOffset);
        instance.dispose();
      }
    };
  }, [terminal.id]);

  return <div ref={containerRef} className="project-terminal-output" role="application" aria-label={`${terminal.title} terminal for ${projectId}`} />;
}

export function ProjectTerminalsTab({ project }: { project: ProjectReference }) {
  const [terminals, setTerminals] = useState<TerminalSnapshot[]>([]);
  const [selectedId, setSelectedId] = useState("");
  const [loading, setLoading] = useState(true);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState("");
  const [pendingDeleteId, setPendingDeleteId] = useState("");
  const pendingOutput = useRef(new Map<string, TerminalOutputEvent[]>());
  const terminalInstances = useRef(new Map<string, { instance: XTerm; endOffset: number }>());
  const inputQueues = useRef(new Map<string, Promise<void>>());

  const loadTerminals = useCallback(async () => {
    try {
      const snapshots = await invoke<TerminalSnapshot[]>("list_project_terminals", { projectId: project.id });
      setTerminals((current) => snapshots.map((snapshot) => {
        const previous = current.find((terminal) => terminal.id === snapshot.id);
        if (previous && previous.bufferEndOffset > snapshot.bufferEndOffset) return previous;
        const queued = pendingOutput.current.get(snapshot.id) ?? [];
        const newer = queued.filter((event) => event.endOffset > snapshot.bufferEndOffset);
        pendingOutput.current.delete(snapshot.id);
        return newer.reduce(
          (terminal, event) => ({
            ...terminal,
            buffer: `${terminal.buffer}${event.chunk}`.slice(-200_000),
            bufferEndOffset: event.endOffset,
          }),
          snapshot,
        );
      }));
      setSelectedId((current) => current && snapshots.some((terminal) => terminal.id === current)
        ? current
        : snapshots[0]?.id ?? "");
    } catch (loadError) {
      setError(messageFromError(loadError));
    } finally {
      setLoading(false);
    }
  }, [project.id]);

  useEffect(() => {
    let disposed = false;
    const unlisteners: Array<() => void> = [];
    const subscribe = async () => {
      try {
        const unlistenOutput = await listen<TerminalOutputEvent>("project-terminal-output", (event) => {
          if (event.payload.projectId !== project.id) return;
          const mounted = terminalInstances.current.get(event.payload.terminalId);
          if (mounted && event.payload.endOffset > mounted.endOffset) {
            mounted.instance.write(event.payload.chunk);
            mounted.endOffset = event.payload.endOffset;
          } else if (!mounted) {
            const queued = pendingOutput.current.get(event.payload.terminalId) ?? [];
            queued.push(event.payload);
            pendingOutput.current.set(event.payload.terminalId, queued.slice(-100));
          }
          setTerminals((current) => {
            const terminal = current.find((item) => item.id === event.payload.terminalId);
            if (!terminal) return current;
            if (event.payload.endOffset <= terminal.bufferEndOffset) return current;
            return current.map((item) => item.id === terminal.id ? {
              ...item,
              buffer: `${item.buffer}${event.payload.chunk}`.slice(-200_000),
              bufferEndOffset: event.payload.endOffset,
            } : item);
          });
        });
        if (disposed) unlistenOutput();
        else unlisteners.push(unlistenOutput);

        const unlistenExit = await listen<TerminalExitEvent>("project-terminal-exit", (event) => {
          if (event.payload.projectId !== project.id) return;
          setTerminals((current) => current.map((terminal) => terminal.id === event.payload.terminalId
            ? { ...terminal, alive: false, exitCode: event.payload.exitCode }
            : terminal));
        });
        if (disposed) unlistenExit();
        else unlisteners.push(unlistenExit);
      } catch (listenError) {
        if (!disposed) setError(messageFromError(listenError));
      }
    };
    void subscribe();
    void loadTerminals();
    return () => {
      disposed = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [loadTerminals, project.id]);

  const selectedTerminal = terminals.find((terminal) => terminal.id === selectedId);

  function terminalReady(id: string, instance: XTerm | null, endOffset: number) {
    if (!instance) {
      terminalInstances.current.delete(id);
      return;
    }
    terminalInstances.current.set(id, { instance, endOffset });
    const queued = pendingOutput.current.get(id) ?? [];
    for (const event of queued) {
      const mounted = terminalInstances.current.get(id);
      if (mounted && event.endOffset > mounted.endOffset) {
        mounted.instance.write(event.chunk);
        mounted.endOffset = event.endOffset;
      }
    }
    pendingOutput.current.delete(id);
  }

  function sendTerminalData(terminalId: string, data: string) {
    const previous = inputQueues.current.get(terminalId) ?? Promise.resolve();
    const next = previous
      .then(async () => {
        await invoke("write_project_terminal", { projectId: project.id, terminalId, data });
        setError("");
      })
      .catch((writeError: unknown) => setError(messageFromError(writeError)));
    inputQueues.current.set(terminalId, next);
    return next;
  }

  function resizeTerminal(terminalId: string, cols: number, rows: number) {
    void invoke("resize_project_terminal", {
      projectId: project.id,
      terminalId,
      cols,
      rows,
    }).catch((resizeError) => setError(messageFromError(resizeError)));
  }

  async function createTerminal() {
    setCreating(true);
    setError("");
    try {
      const terminal = await invoke<TerminalSnapshot>("create_project_terminal", {
        projectId: project.id,
        cols: 100,
        rows: 26,
      });
      const queued = pendingOutput.current.get(terminal.id) ?? [];
      pendingOutput.current.delete(terminal.id);
      const withQueuedOutput = queued
        .filter((event) => event.endOffset > terminal.bufferEndOffset)
        .reduce((current, event) => ({
          ...current,
          buffer: `${current.buffer}${event.chunk}`.slice(-200_000),
          bufferEndOffset: event.endOffset,
        }), terminal);
      setTerminals((current) => [...current.filter((item) => item.id !== terminal.id), withQueuedOutput]);
      setSelectedId(terminal.id);
    } catch (createError) {
      setError(messageFromError(createError));
    } finally {
      setCreating(false);
    }
  }

  async function closeTerminal(terminal: TerminalSnapshot) {
    setError("");
    try {
      await invoke("close_project_terminal", { projectId: project.id, terminalId: terminal.id });
      setTerminals((current) => current.filter((item) => item.id !== terminal.id));
      setSelectedId((current) => current === terminal.id ? "" : current);
      setPendingDeleteId("");
    } catch (closeError) {
      setError(messageFromError(closeError));
    }
  }

  return (
    <FeatureFrame
      eyebrow="LOCAL SHELL"
      title="Terminals"
      description="Interactive shells start in this project's repository folder and remain available while you switch sections."
      action={<button className="project-feature-primary" type="button" onClick={() => void createTerminal()} disabled={creating}>{creating ? "Starting…" : "New terminal"}</button>}
    >
      <Feedback error={error} />
      <div className="project-terminal-tabs" aria-label="Project terminals">
        {terminals.map((terminal) => (
          <button
            key={terminal.id}
            type="button"
            aria-pressed={selectedId === terminal.id}
            className={selectedId === terminal.id ? "active" : ""}
            onClick={() => setSelectedId(terminal.id)}
          >
            <span className={terminal.alive ? "terminal-live-dot" : "terminal-exited-dot"} aria-hidden="true" />
            {terminal.title}
          </button>
        ))}
      </div>
      {selectedTerminal ? (
        <div className="project-terminal-panel">
          <div className="project-terminal-toolbar">
            <span><code>{project.name}</code> · {selectedTerminal.alive ? "Shell running" : `Exited (${selectedTerminal.exitCode ?? "unknown"})`}</span>
            {pendingDeleteId === selectedTerminal.id ? (
              <span className="project-task-confirm">
                <button className="project-feature-danger" type="button" onClick={() => void closeTerminal(selectedTerminal)}>Terminate</button>
                <button className="project-feature-button" type="button" onClick={() => setPendingDeleteId("")}>Cancel</button>
              </span>
            ) : (
              <button className="project-feature-quiet" type="button" onClick={() => setPendingDeleteId(selectedTerminal.id)}>Close terminal</button>
            )}
          </div>
          <ProjectTerminalPane
            key={selectedTerminal.id}
            projectId={project.id}
            terminal={selectedTerminal}
            onReady={terminalReady}
            onData={(id, data) => void sendTerminalData(id, data)}
            onResize={resizeTerminal}
            onError={setError}
          />
        </div>
      ) : (
        <div className="project-feature-empty">
          <span className="project-feature-empty-mark" aria-hidden="true">›_</span>
          <h3>{loading ? "Loading terminals…" : "No terminal sessions"}</h3>
          <p>Start a shell in the project directory for ad hoc commands.</p>
          <button className="project-feature-primary" type="button" onClick={() => void createTerminal()} disabled={creating}>{creating ? "Starting…" : "Start a terminal"}</button>
        </div>
      )}
    </FeatureFrame>
  );
}

function pipelineState(run: PipelineRun) {
  if (run.status === "completed") return run.conclusion || "completed";
  return run.status.replace("_", " ");
}

export function ProjectPipelinesTab({ project }: { project: ProjectReference }) {
  const [snapshot, setSnapshot] = useState<ProjectPipelines | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");

  const loadPipelines = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      setSnapshot(await invoke<ProjectPipelines>("get_project_pipelines", { projectId: project.id }));
    } catch (loadError) {
      setError(messageFromError(loadError));
    } finally {
      setLoading(false);
    }
  }, [project.id]);

  useEffect(() => {
    void loadPipelines();
  }, [loadPipelines]);

  async function openRun(url: string) {
    try {
      await openUrl(url);
    } catch (openError) {
      setError(messageFromError(openError));
    }
  }

  return (
    <FeatureFrame
      eyebrow="CONTINUOUS INTEGRATION"
      title="Pipelines"
      description={snapshot?.repository ? `Recent GitHub Actions runs for ${snapshot.repository}.` : "View recent GitHub Actions runs for this repository."}
      action={<button className="project-feature-button" type="button" onClick={() => void loadPipelines()} disabled={loading}>Refresh runs</button>}
    >
      <Feedback error={error} />
      {loading && !snapshot ? (
        <p className="project-feature-state" role="status">Loading workflow runs…</p>
      ) : snapshot?.error ? (
        <div className="project-feature-empty">
          <span className="project-feature-empty-mark" aria-hidden="true">↗</span>
          <h3>{snapshot.repository ? "GitHub Actions needs setup" : "No GitHub repository connected"}</h3>
          <p>{snapshot.error}</p>
        </div>
      ) : snapshot?.runs.length ? (
        <div className="project-pipeline-list">
          {snapshot.runs.map((run) => (
            <article className="project-pipeline-row" key={run.id}>
              <span className={`project-pipeline-state ${pipelineState(run).replace(/[^a-z0-9]+/gi, "-").toLowerCase()}`} aria-label={pipelineState(run)} />
              <div className="project-pipeline-main">
                <strong>{run.name}</strong>
                <span><code>{run.branch || "unknown branch"}</code> · Run #{run.id}</span>
              </div>
              <span className="project-pipeline-status">{pipelineState(run)}</span>
              <time dateTime={run.createdAt}>{new Date(run.createdAt).toLocaleString()}</time>
              <button className="project-feature-quiet" type="button" onClick={() => void openRun(run.url)}>Open run ↗</button>
            </article>
          ))}
        </div>
      ) : (
        <div className="project-feature-empty">
          <span className="project-feature-empty-mark" aria-hidden="true">✓</span>
          <h3>No workflow runs found</h3>
          <p>{snapshot?.repository ? "GitHub Actions has not reported any runs for this repository." : "Connect this project to GitHub to load workflow activity."}</p>
        </div>
      )}
    </FeatureFrame>
  );
}

export function ProjectDependenciesTab({ project }: { project: ProjectReference }) {
  const [snapshot, setSnapshot] = useState<ProjectDependencies | null>(null);
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");

  const loadDependencies = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      setSnapshot(await invoke<ProjectDependencies>("get_project_dependencies", { projectId: project.id }));
    } catch (loadError) {
      setError(messageFromError(loadError));
    } finally {
      setLoading(false);
    }
  }, [project.id]);

  useEffect(() => {
    void loadDependencies();
  }, [loadDependencies]);

  const dependencies = (snapshot?.dependencies ?? []).filter((dependency) =>
    `${dependency.name} ${dependency.version} ${dependency.kind}`.toLowerCase().includes(query.toLowerCase()),
  );

  return (
    <FeatureFrame
      eyebrow="MANIFESTS"
      title="Dependencies"
      description="Inspect direct declarations from package.json, Cargo.toml, and requirements.txt."
      action={<button className="project-feature-button" type="button" onClick={() => void loadDependencies()} disabled={loading}>Rescan manifests</button>}
    >
      <Feedback error={error} />
      {snapshot?.errors.map((manifestError) => <p className="project-feature-warning" key={manifestError}>{manifestError}</p>)}
      {loading && !snapshot ? (
        <p className="project-feature-state" role="status">Scanning dependency manifests…</p>
      ) : !snapshot?.manifests.length ? (
        <div className="project-feature-empty">
          <span className="project-feature-empty-mark" aria-hidden="true">◇</span>
          <h3>No supported manifests found</h3>
          <p>Supported root manifests: package.json, Cargo.toml, and requirements.txt.</p>
        </div>
      ) : (
        <>
          <div className="project-dependency-summary">
            <div><strong>{snapshot.dependencies.length}</strong><span>direct dependencies</span></div>
            <div><strong>{snapshot.manifests.join(", ")}</strong><span>detected manifests</span></div>
          </div>
          <label className="project-feature-search">
            <span>Filter dependencies</span>
            <input type="search" value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search package names or scope" />
          </label>
          {dependencies.length ? (
            <div className="project-dependency-list">
              {dependencies.map((dependency) => (
                <article key={`${dependency.manifest}:${dependency.kind}:${dependency.name}`}>
                  <div><strong>{dependency.name}</strong><span>{dependency.manifest}</span></div>
                  <code>{dependency.version}</code>
                  <span className={`project-dependency-kind ${dependency.kind}`}>{dependency.kind}</span>
                </article>
              ))}
            </div>
          ) : (
            <p className="project-feature-state">No dependencies match this filter.</p>
          )}
        </>
      )}
    </FeatureFrame>
  );
}

export function ProjectSkillsTab({ project }: { project: ProjectReference }) {
  const [snapshot, setSnapshot] = useState<ProjectSkills | null>(null);
  const [selectedSkill, setSelectedSkill] = useState<ProjectSkill | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const skillDialogRef = useRef<HTMLElement>(null);

  const loadSkills = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      setSnapshot(await invoke<ProjectSkills>("list_project_skills", { projectId: project.id }));
    } catch (loadError) {
      setError(messageFromError(loadError));
    } finally {
      setLoading(false);
    }
  }, [project.id]);

  useEffect(() => {
    void loadSkills();
  }, [loadSkills]);

  useEffect(() => {
    const dialog = skillDialogRef.current;
    if (!selectedSkill || !dialog) return;
    const previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const focusable = () => [...dialog.querySelectorAll<HTMLElement>(
      'button:not([disabled]), a[href], input:not([disabled]), textarea:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])',
    )];
    focusable()[0]?.focus();
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        setSelectedSkill(null);
        return;
      }
      if (event.key !== "Tab") return;
      const controls = focusable();
      if (controls.length === 0) {
        event.preventDefault();
        dialog.focus();
        return;
      }
      const first = controls[0];
      const last = controls[controls.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    dialog.addEventListener("keydown", handleKeyDown);
    return () => {
      dialog.removeEventListener("keydown", handleKeyDown);
      previousFocus?.focus();
    };
  }, [selectedSkill]);

  const skills = [...(snapshot?.project ?? []), ...(snapshot?.global ?? [])];

  return (
    <FeatureFrame
      eyebrow="AGENT CUSTOMIZATION"
      title="Skills"
      description="Discover project-level and personal Claude Code skills that can guide work in this repository."
      action={<button className="project-feature-button" type="button" onClick={() => void loadSkills()} disabled={loading}>Rescan skills</button>}
    >
      <Feedback error={error} />
      {snapshot?.errors.map((skillError) => <p className="project-feature-warning" key={skillError}>{skillError}</p>)}
      {loading && !snapshot ? (
        <p className="project-feature-state" role="status">Discovering skills…</p>
      ) : skills.length === 0 ? (
        <div className="project-feature-empty">
          <span className="project-feature-empty-mark" aria-hidden="true">✦</span>
          <h3>No skills discovered</h3>
          <p>Project skills in .claude/skills and personal skills in ~/.claude/skills appear here.</p>
        </div>
      ) : (
        <div className="project-skills-groups">
          {[
            { key: "project", label: "Project skills", entries: snapshot?.project ?? [] },
            { key: "global", label: "Personal skills", entries: snapshot?.global ?? [] },
          ].map((group) => (
            group.entries.length > 0 && (
              <section key={group.key} aria-labelledby={`skills-${group.key}`}>
                <header><h3 id={`skills-${group.key}`}>{group.label}</h3><span>{group.entries.length}</span></header>
                <div className="project-skill-grid">
                  {group.entries.map((skill) => (
                    <button className="project-skill-card" type="button" key={`${skill.source}:${skill.folderName}`} onClick={() => setSelectedSkill(skill)}>
                      <span className="project-skill-icon" aria-hidden="true">✦</span>
                      <strong>{skill.name}</strong>
                      <span className="project-skill-description">{skill.description || "No description provided."}</span>
                      <span className="project-skill-meta">
                        <code>{skill.folderName}</code>
                        {skill.shadowed && <span>Shadowed by personal skill</span>}
                        {skill.warning && <span className="project-skill-warning">{skill.warning}</span>}
                      </span>
                    </button>
                  ))}
                </div>
              </section>
            )
          ))}
        </div>
      )}
      {selectedSkill && (
        <div className="project-skill-dialog-backdrop" onMouseDown={(event) => {
          if (event.target === event.currentTarget) setSelectedSkill(null);
        }}>
          <section ref={skillDialogRef} className="project-skill-dialog" role="dialog" aria-modal="true" aria-labelledby="selected-skill-title" tabIndex={-1}>
            <header>
              <div><span className="project-feature-eyebrow">{selectedSkill.source} skill</span><h3 id="selected-skill-title">{selectedSkill.name}</h3></div>
              <button className="project-feature-quiet" type="button" onClick={() => setSelectedSkill(null)} aria-label="Close skill details">Close</button>
            </header>
            {selectedSkill.description && <p>{selectedSkill.description}</p>}
            <pre>{selectedSkill.content}</pre>
          </section>
        </div>
      )}
    </FeatureFrame>
  );
}
