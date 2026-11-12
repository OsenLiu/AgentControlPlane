import { CSSProperties, FormEvent, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import {
  ProjectDependenciesTab,
  ProjectDiffsTab,
  ProjectPipelinesTab,
  ProjectSkillsTab,
  ProjectTasksTab,
  ProjectTerminalsTab,
} from "./project/ProjectFeatureTabs";
import "./App.css";

type Status = "healthy" | "running" | "waiting" | "failed";

type Project = {
  id: string;
  name: string;
  path: string;
  branch: string;
  currentCommit: string;
  gitState: "clean" | "dirty";
  looping: boolean;
  agentTaskCreation: boolean;
};

type ToolSettings = {
  enabled: boolean;
};

type Settings = {
  copilot: ToolSettings;
  gemini: ToolSettings;
  claudeCode: ToolSettings;
  githubUsername: string;
  githubAuthMethod: "token" | "cli";
  githubTokenConfigured: boolean;
  defaultProvider: "copilot" | "gemini" | "claudeCode";
  transcriptFontSize: number;
};

type SettingsForm = Settings & { githubToken: string };

type SessionStatus = "starting" | "running" | "waiting" | "idle" | "paused" | "completed" | "failed" | "orphaned" | "unknown";

type AgentSession = {
  id: string;
  externalSessionId: string | null;
  project: string;
  task: string;
  branch: string;
  provider: string;
  status: SessionStatus;
  step: string;
  elapsed: string;
  tokens: string;
  permissionMode: "Plan" | "Accept edits" | "Auto";
  managed: boolean;
  lastEvent: string;
  archived: boolean;
  statusReason: string;
  updatedAt: string;
};

type TranscriptMessage = {
  role: "user" | "assistant";
  content: string;
  timestamp: string;
};

type PermissionRequest = {
  requestId: string;
  title: string;
  options: { optionId: string; name: string; kind: string }[];
};

type AttentionKind = "permission" | "failure";

type AttentionItem = {
  id: string;
  sessionId: string;
  kind: AttentionKind;
  priority: "High" | "Medium" | "Low";
  title: string;
  project: string;
  branch: string;
  agent: string;
  updatedAt: string;
  summary: string;
  context: string;
  permissionRequest?: PermissionRequest;
};

const attentionFilters: Array<{ label: string; kind: AttentionKind | "all" }> = [
  { label: "All items", kind: "all" },
  { label: "Permissions", kind: "permission" },
  { label: "Failures", kind: "failure" },
];

async function loadAttentionItemsFromBackend(): Promise<AttentionItem[]> {
  const sessions = await invoke<AgentSession[]>("list_agent_sessions", { archived: false });
  const waitingSessions = sessions.filter((session) => session.status === "waiting");
  const permissionResults = await Promise.all(waitingSessions.map(async (session) => [
    session.id,
    await invoke<PermissionRequest | null>("get_pending_session_permission", { sessionId: session.id }),
  ] as const));
  const permissions = new Map(permissionResults.filter(([, permission]) => permission !== null));

  return sessions.flatMap((session): AttentionItem[] => {
    const permissionRequest = permissions.get(session.id) ?? undefined;
    if (permissionRequest) {
      return [{
        id: `${session.id}:permission:${permissionRequest.requestId}`,
        sessionId: session.id,
        kind: "permission" as const,
        priority: "High" as const,
        title: permissionRequest.title,
        project: session.project,
        branch: session.branch,
        agent: session.provider,
        updatedAt: session.updatedAt,
        summary: session.task,
        context: `${permissionRequest.options.length} permission option${permissionRequest.options.length === 1 ? "" : "s"} available.`,
        permissionRequest,
      }];
    }

    if (session.status === "waiting") {
      return [{
        id: `${session.id}:waiting`,
        sessionId: session.id,
        kind: "failure" as const,
        priority: "High" as const,
        title: "Session is waiting",
        project: session.project,
        branch: session.branch,
        agent: session.provider,
        updatedAt: session.updatedAt,
        summary: session.statusReason || session.lastEvent || session.step,
        context: "The session is waiting, but its active permission request is unavailable. Open the session to inspect it.",
      }];
    }

    if (session.status === "failed") {
      return [{
        id: `${session.id}:failed`,
        sessionId: session.id,
        kind: "failure" as const,
        priority: "High" as const,
        title: "Session failed",
        project: session.project,
        branch: session.branch,
        agent: session.provider,
        updatedAt: session.updatedAt,
        summary: session.statusReason || session.lastEvent || session.step,
        context: session.step,
      }];
    }

    return [];
  });
}

function providerLabel(provider: string) {
  switch (provider) {
    case "copilot": return "GitHub Copilot";
    case "claudeCode": return "Claude Code";
    case "gemini": return "Gemini CLI";
    default: return provider;
  }
}

function formatLocalTimestamp(value: string) {
  const normalized = value.includes("T") || value.endsWith("Z")
    ? value
    : `${value.replace(" ", "T")}Z`;
  const date = new Date(normalized);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
}

const emptyToolSettings = (): ToolSettings => ({
  enabled: false,
});

const emptySettings = (): Settings => ({
  copilot: emptyToolSettings(),
  gemini: emptyToolSettings(),
  claudeCode: emptyToolSettings(),
  githubUsername: "",
  githubAuthMethod: "token",
  githubTokenConfigured: false,
  defaultProvider: "claudeCode",
  transcriptFontSize: 16,
});

const navItems = [
  ["⌂", "Dashboard"],
  ["▣", "Projects"],
  ["◉", "Agent sessions"],
  ["!", "Attention inbox"],
  ["⚙", "Settings"],
];

const projectSections = [
  { id: "tasks", label: "Tasks" },
  { id: "diffs", label: "Diffs" },
  { id: "sessions", label: "Sessions" },
  { id: "terminals", label: "Terminals" },
  { id: "pipelines", label: "Pipelines" },
  { id: "dependencies", label: "Dependencies" },
  { id: "skills", label: "Skills" },
] as const;

type ProjectSection = (typeof projectSections)[number]["id"];

function StatusBadge({ status }: { status: Status }) {
  const labels: Record<Status, string> = {
    healthy: "Healthy",
    running: "Running",
    waiting: "Waiting",
    failed: "Failed",
  };
  return (
    <span className={`status status-${status}`}>
      <span className="status-dot" aria-hidden="true" />
      {labels[status]}
    </span>
  );
}

function SettingsView({
  settings,
  setSettings,
  onSave,
  saving,
}: {
  settings: SettingsForm;
  setSettings: (settings: SettingsForm) => void;
  onSave: (event: FormEvent<HTMLFormElement>) => void;
  saving: boolean;
}) {
  const updateTool = (key: "copilot" | "gemini" | "claudeCode", field: keyof ToolSettings, value: boolean) => {
    setSettings({ ...settings, [key]: { ...settings[key], [field]: value } });
  };
  const tools = [
    { key: "copilot" as const, label: "GitHub Copilot CLI", command: "copilot login", description: "Run copilot login in a terminal to start the browser-based GitHub sign-in flow." },
    { key: "gemini" as const, label: "Gemini CLI", command: "gemini", description: "Start Gemini and choose Sign in with Google. API key and Vertex AI modes are also supported." },
    { key: "claudeCode" as const, label: "Claude Code CLI", command: "claude", description: "Start Claude and follow the sign-in flow. Use /login inside Claude Code to switch accounts." },
  ];

  return (
    <form className="settings-page" onSubmit={onSave}>
      <header className="settings-header">
        <div><p className="eyebrow">APPLICATION SETTINGS</p><h1>Integrations</h1><p className="subtitle">Configure local agent CLIs and your GitHub connection.</p></div>
        <button className="primary-button" type="submit" disabled={saving}>{saving ? "Saving…" : "Save settings"}</button>
      </header>
      <section className="settings-section">
        <div className="section-header"><div><h2>Agent providers</h2><span className="section-caption">Executables run locally through the Rust supervisor.</span></div></div>
        <div className="provider-grid">
          {tools.map((tool) => {
            const value = settings[tool.key];
            return (
              <article className="provider-card" key={tool.key}>
                <div className="provider-title"><div className="provider-logo">{tool.label.charAt(0)}</div><div><h3>{tool.label}</h3><span>{tool.description}</span></div><label className="toggle"><input type="checkbox" checked={value.enabled} onChange={(event) => updateTool(tool.key, "enabled", event.currentTarget.checked)} /><span /></label></div>
                <div className="login-command"><code>{tool.command}</code><span>Use the CLI's own login and credential storage.</span></div>
                <span className={`setup-state ${value.enabled ? "configured" : ""}`}><span /> {value.enabled ? "Enabled for delegation" : "Disabled"}</span>
              </article>
            );
          })}
        </div>
      </section>
      <section className="settings-section">
        <div className="section-header"><div><h2>Default AI agent</h2><span className="section-caption">New delegated tasks use this provider unless a task overrides it.</span></div></div>
        <div className="default-agent-card">
          {tools.map((tool) => <label className="agent-option" key={tool.key}><input type="radio" name="defaultProvider" value={tool.key} checked={settings.defaultProvider === tool.key} onChange={() => setSettings({ ...settings, defaultProvider: tool.key })} /><span><strong>{tool.label}</strong><small>{tool.command}</small></span></label>)}
        </div>
      </section>
      <section className="settings-section">
        <div className="section-header"><div><h2>Transcript display</h2><span className="section-caption">Choose a comfortable size for live session output.</span></div></div>
        <div className="transcript-settings-card">
          <label className="transcript-size-control" htmlFor="transcript-font-size">
            <span><strong>Live transcript text size</strong><small>Applies to the transcript in Agent sessions.</small></span>
            <select
              id="transcript-font-size"
              className="settings-input transcript-size-select"
              value={settings.transcriptFontSize}
              onChange={(event) => setSettings({ ...settings, transcriptFontSize: Number(event.currentTarget.value) })}
            >
              {[12, 14, 16, 18, 20, 22, 24].map((size) => <option value={size} key={size}>{size}px</option>)}
            </select>
          </label>
        </div>
      </section>
      <section className="settings-section">
        <div className="section-header"><div><h2>GitHub connection</h2><span className="section-caption">Choose how Agent Control Plane authenticates with GitHub.</span></div></div>
        <div className="github-card">
          <div className="github-heading"><div className="github-logo" aria-hidden="true">GH</div><div><h3>GitHub account</h3><span>Used for Actions, releases, repository analytics, and deep links.</span></div><span className={`github-connection-state ${settings.githubAuthMethod === "cli" ? "cli-selected" : settings.githubTokenConfigured ? "configured" : ""}`}><span aria-hidden="true" />{settings.githubAuthMethod === "cli" ? "GitHub CLI selected" : settings.githubTokenConfigured ? "Token saved" : "Not connected"}</span></div>
          <fieldset className="github-auth-methods">
            <legend>Connection method</legend>
            <label className={`github-auth-option ${settings.githubAuthMethod === "token" ? "selected" : ""}`}>
              <input
                type="radio"
                name="githubAuthMethod"
                value="token"
                checked={settings.githubAuthMethod === "token"}
                onChange={() => setSettings({ ...settings, githubAuthMethod: "token" })}
              />
              <span className="github-auth-icon token" aria-hidden="true">PAT</span>
              <span className="github-auth-copy"><strong>Personal access token</strong><small>Paste a token and store it in your OS credential vault.</small></span>
            </label>
            <label className={`github-auth-option ${settings.githubAuthMethod === "cli" ? "selected" : ""}`}>
              <input
                type="radio"
                name="githubAuthMethod"
                value="cli"
                checked={settings.githubAuthMethod === "cli"}
                onChange={() => setSettings({ ...settings, githubAuthMethod: "cli" })}
              />
              <span className="github-auth-icon cli" aria-hidden="true">gh</span>
              <span className="github-auth-copy"><strong>GitHub CLI</strong><small>Reuse the credentials from your existing <code>gh</code> login.</small></span>
            </label>
          </fieldset>
          {settings.githubAuthMethod === "token" ? (
            <div className="github-fields">
              <div><label className="field-label" htmlFor="github-username">GITHUB USERNAME <span>(OPTIONAL)</span></label><input id="github-username" className="settings-input" value={settings.githubUsername} onChange={(event) => setSettings({ ...settings, githubUsername: event.currentTarget.value })} placeholder="octocat" /></div>
              <div><label className="field-label" htmlFor="github-token">PERSONAL ACCESS TOKEN</label><input id="github-token" className="settings-input" type="password" autoComplete="new-password" value={settings.githubToken} onChange={(event) => setSettings({ ...settings, githubToken: event.currentTarget.value })} placeholder={settings.githubTokenConfigured ? "Leave blank to keep saved token" : "ghp_…"} /><span className="github-field-help">Saved securely in your operating system credential vault.</span></div>
            </div>
          ) : (
            <div className="github-cli-details">
              <span className="github-cli-mark" aria-hidden="true">$</span>
              <div><strong>Use your existing GitHub CLI login</strong><p>Run this once in a terminal if you have not authenticated yet:</p><code>gh auth login</code><small>GitHub requests will use the CLI’s secure credential store. No token is copied into this app.</small></div>
            </div>
          )}
        </div>
      </section>
    </form>
  );
}

function SessionStatusBadge({ status }: { status: SessionStatus }) {
  const labels: Record<SessionStatus, string> = {
    starting: "Starting",
    running: "Running",
    waiting: "Waiting for you",
    idle: "Ready",
    paused: "Paused",
    completed: "Completed",
    failed: "Failed",
    orphaned: "Orphaned",
    unknown: "Unknown",
  };
  return (
    <span className={`session-status session-status-${status}`}>
      <span className="session-status-dot" aria-hidden="true" />
      {labels[status]}
    </span>
  );
}

function AgentSessionsView({
  transcriptFontSize,
  initialSessionId,
  projectId,
}: {
  transcriptFontSize: number;
  initialSessionId?: string;
  projectId?: string;
}) {
  const [sessions, setSessions] = useState<AgentSession[]>([]);
  const [selectedId, setSelectedId] = useState(initialSessionId ?? "");
  const [filter, setFilter] = useState<"all" | SessionStatus>("all");
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  const [projects, setProjects] = useState<Project[]>([]);
  const [selectedProjectId, setSelectedProjectId] = useState(projectId ?? "");
  const [task, setTask] = useState("Start a new Copilot session");
  const [starting, setStarting] = useState(false);
  const [resuming, setResuming] = useState(false);
  const [sendingMessage, setSendingMessage] = useState(false);
  const [stoppingSession, setStoppingSession] = useState(false);
  const [launcherOpen, setLauncherOpen] = useState(false);
  const [transcript, setTranscript] = useState<TranscriptMessage[]>([]);
  const [messageDraft, setMessageDraft] = useState("");
  const [permissionRequest, setPermissionRequest] = useState<PermissionRequest | null>(null);
  const [archivedView, setArchivedView] = useState(false);
  const [renamingSession, setRenamingSession] = useState(false);
  const [sessionNameDraft, setSessionNameDraft] = useState("");
  const [savingSessionName, setSavingSessionName] = useState(false);
  const selectedSession = sessions.find((session) => session.id === selectedId) ?? sessions[0];
  const visibleSessions = sessions.filter((session) => {
    const matchesFilter = filter === "all" || session.status === filter;
    const searchable = `${session.project} ${session.task} ${session.branch}`.toLowerCase();
    return matchesFilter && searchable.includes(query.toLowerCase());
  });

  useEffect(() => {
    void loadSessions(archivedView);
    void invoke<Project[]>("list_projects").then((loaded) => {
      setProjects(loaded);
      setSelectedProjectId(projectId ?? loaded[0]?.id ?? "");
    }).catch((reason) => {
      setLoadError(reason instanceof Error ? reason.message : String(reason));
    });
  }, [archivedView, projectId]);

  useEffect(() => {
    if (!selectedSession) {
      setTranscript([]);
      return;
    }
    void loadTranscript(selectedSession.id);
  }, [selectedSession?.id]);

  useEffect(() => {
    let unsubscribe: (() => void) | undefined;
    let unsubscribePermission: (() => void) | undefined;
    setPermissionRequest(null);
    void listen<string>("agent-session-updated", (event) => {
      void loadSessions();
      if (event.payload === selectedSession?.id) {
        void loadTranscript(event.payload);
      }
    }).then((unlisten) => {
      unsubscribe = unlisten;
    });
    void listen<{ sessionId: string; permission: PermissionRequest }>("agent-session-permission-requested", (event) => {
      if (event.payload.sessionId === selectedSession?.id) {
        setPermissionRequest(event.payload.permission);
      }
    }).then((unlisten) => {
      unsubscribePermission = unlisten;
    });
    if (selectedSession) {
      void invoke<PermissionRequest | null>("get_pending_session_permission", {
        sessionId: selectedSession.id,
      }).then(setPermissionRequest).catch((reason) => {
        setLoadError(reason instanceof Error ? reason.message : String(reason));
      });
    } else {
      setPermissionRequest(null);
    }
    return () => {
      unsubscribe?.();
      unsubscribePermission?.();
    };
  }, [selectedSession?.id]);

  async function loadSessions(showArchived = archivedView) {
    try {
      setLoadError("");
      const loaded = await invoke<AgentSession[]>("list_agent_sessions", {
        archived: showArchived,
        ...(projectId ? { projectId } : {}),
      });
      setSessions(loaded);
      setSelectedId((current) => {
        if (loaded.some((session) => session.id === current)) return current;
        if (initialSessionId && loaded.some((session) => session.id === initialSessionId)) return initialSessionId;
        return loaded[0]?.id ?? "";
      });
    } catch (reason) {
      setLoadError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setLoading(false);
    }
  }

  async function loadTranscript(sessionId: string) {
    try {
      const loaded = await invoke<TranscriptMessage[]>("get_session_transcript", { sessionId });
      setTranscript(loaded);
    } catch (reason) {
      setLoadError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  async function refreshSessions() {
    setRefreshing(true);
    await loadSessions(archivedView);
    setRefreshing(false);
  }

  async function updateArchiveState() {
    if (!selectedSession) return;
    try {
      setLoadError("");
      await invoke("set_session_archived", {
        sessionId: selectedSession.id,
        archived: !archivedView,
      });
      await loadSessions(archivedView);
    } catch (reason) {
      setLoadError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  function beginRenameSession() {
    if (!selectedSession) return;
    setSessionNameDraft(selectedSession.task);
    setRenamingSession(true);
  }

  async function renameSession(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!selectedSession || !sessionNameDraft.trim()) return;
    setSavingSessionName(true);
    setLoadError("");
    try {
      await invoke("rename_agent_session", {
        sessionId: selectedSession.id,
        name: sessionNameDraft.trim(),
      });
      setRenamingSession(false);
      await loadSessions(archivedView);
    } catch (reason) {
      setLoadError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setSavingSessionName(false);
    }
  }

  async function resumeSession() {
    if (!selectedSession) return;
    setResuming(true);
    setLoadError("");
    try {
      await invoke("start_copilot_session", {
        projectId: null,
        task: "",
        resumeSessionId: selectedSession.id,
      });
      setArchivedView(false);
      await loadSessions(false);
    } catch (reason) {
      setLoadError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setResuming(false);
    }
  }

  async function startSession(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!selectedProjectId || !task.trim()) return;
    setStarting(true);
    setLoadError("");
    try {
      await invoke("start_copilot_session", {
        projectId: selectedProjectId,
        task: task.trim(),
      });
      setTask("");
      await loadSessions();
    } catch (reason) {
      setLoadError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setStarting(false);
    }
  }

  async function sendMessage(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!selectedSession || !messageDraft.trim()) return;
    setSendingMessage(true);
    setLoadError("");
    try {
      await invoke("send_session_message", {
        sessionId: selectedSession.id,
        message: messageDraft,
      });
      setMessageDraft("");
      await loadTranscript(selectedSession.id);
      await loadSessions();
    } catch (reason) {
      setLoadError(reason instanceof Error ? reason.message : String(reason));
      await loadTranscript(selectedSession.id);
    } finally {
      setSendingMessage(false);
    }
  }

  async function stopSession() {
    if (!selectedSession) return;
    setStoppingSession(true);
    setLoadError("");
    try {
      await invoke("stop_agent_session", { sessionId: selectedSession.id });
      await loadSessions();
    } catch (reason) {
      setLoadError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setStoppingSession(false);
    }
  }

  async function respondToPermission(optionId: string | null) {
    if (!selectedSession || !permissionRequest) return;
    setLoadError("");
    try {
      await invoke("respond_session_permission", {
        sessionId: selectedSession.id,
        requestId: permissionRequest.requestId,
        optionId,
      });
      setPermissionRequest(null);
    } catch (reason) {
      setLoadError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  if (!selectedSession) {
    return <div className="sessions-page">
      <header className="sessions-header">
        <div><p className="eyebrow">SUPERVISOR / LIVE SESSIONS</p><h1>Agent sessions</h1><p className="subtitle">Inspect transcripts, answer questions, and keep delegated work moving.</p></div>
        <div className="session-header-actions"><span className="connection-pill"><span /> Supervisor online</span><button className="secondary-button" onClick={() => setArchivedView(!archivedView)}>{archivedView ? "Active sessions" : "Archived sessions"}</button><button className="secondary-button" onClick={() => void refreshSessions()} disabled={refreshing}><span className="refresh-glyph">↻</span> {refreshing ? "Refreshing…" : "Refresh"}</button></div>
      </header>
      <div className="empty-sessions-state">
        <h2>{loading ? "Loading agent sessions…" : "No agent sessions yet"}</h2>
        <p>{loadError || "Start a Copilot CLI session here to create a locally managed conversation."}</p>
        {!loading && projects.length > 0 && <form className="session-launch-card" onSubmit={startSession}>
          {projectId ? <div className="session-project-context"><span>PROJECT</span><strong>{projects.find((project) => project.id === projectId)?.name ?? "Selected project"}</strong></div> : <>
            <label className="field-label" htmlFor="session-project">PROJECT</label>
            <select id="session-project" className="settings-input" value={selectedProjectId} onChange={(event) => setSelectedProjectId(event.currentTarget.value)}>
              {projects.map((project) => <option value={project.id} key={project.id}>{project.name} · {project.branch}</option>)}
            </select>
          </>}
          <label className="field-label" htmlFor="session-task">TASK PROMPT</label>
          <textarea id="session-task" className="session-task-input" value={task} onChange={(event) => setTask(event.currentTarget.value)} placeholder="Describe what Copilot should work on" rows={3} />
          <button className="primary-button" type="submit" disabled={starting || !task.trim()}>{starting ? "Starting Copilot…" : "Start Copilot session"} <span>↗</span></button>
        </form>}
      </div>
    </div>;
  }

  return (
    <div className="sessions-page">
      <header className="sessions-header">
        <div>
          <p className="eyebrow">SUPERVISOR / LIVE SESSIONS</p>
          <h1>Agent sessions</h1>
          <p className="subtitle">Inspect transcripts, answer questions, and keep delegated work moving.</p>
        </div>
        <div className="session-header-actions">
          <span className="connection-pill"><span /> Supervisor online</span>
          <button className="primary-button" onClick={() => setLauncherOpen((open) => !open)}>Start Copilot session <span>↗</span></button>
          <button className="secondary-button" onClick={() => setArchivedView(!archivedView)}>{archivedView ? "Active sessions" : "Archived sessions"}</button>
          <button className="secondary-button" onClick={() => void refreshSessions()} disabled={refreshing}><span className="refresh-glyph">↻</span> {refreshing ? "Refreshing…" : "Refresh"}</button>
        </div>
      </header>
      {launcherOpen && <form className="session-launch-card" onSubmit={startSession}>
        {projectId ? <div className="session-project-context"><span>PROJECT</span><strong>{projects.find((project) => project.id === projectId)?.name ?? "Selected project"}</strong></div> : <>
          <label className="field-label" htmlFor="session-project">PROJECT</label>
          <select id="session-project" className="settings-input" value={selectedProjectId} onChange={(event) => setSelectedProjectId(event.currentTarget.value)} disabled={projects.length === 0}>
            {projects.length === 0 ? <option value="">No projects connected</option> : projects.map((project) => <option value={project.id} key={project.id}>{project.name} · {project.branch}</option>)}
          </select>
        </>}
        <label className="field-label" htmlFor="session-task">TASK PROMPT</label>
        <textarea id="session-task" className="session-task-input" value={task} onChange={(event) => setTask(event.currentTarget.value)} placeholder="Describe what Copilot should work on" rows={3} />
        <button className="primary-button" type="submit" disabled={starting || projects.length === 0 || !task.trim()}>{starting ? "Starting Copilot…" : "Start Copilot session"} <span>↗</span></button>
      </form>}
      {loadError && <p className="session-error" role="alert">{loadError}</p>}

      <section className="session-summary-grid" aria-label="Session summary">
        <article className="session-summary-card"><span className="summary-icon running-icon">◉</span><div><strong>{sessions.filter((session) => session.status === "running").length}</strong><span>Running now</span></div><small>Live output streaming</small></article>
        <article className="session-summary-card"><span className="summary-icon waiting-icon">?</span><div><strong>{sessions.filter((session) => session.status === "waiting").length}</strong><span>Needs your input</span></div><small>Reply to keep it moving</small></article>
        <article className="session-summary-card"><span className="summary-icon paused-icon">Ⅱ</span><div><strong>{sessions.filter((session) => session.status === "paused").length}</strong><span>Paused</span></div><small>Supervisor controlled</small></article>
        <article className="session-summary-card"><span className="summary-icon token-icon">⌁</span><div><strong>{sessions.reduce((total, session) => total + Number.parseFloat(session.tokens), 0).toFixed(1)}k</strong><span>Tokens this hour</span></div><small>Across {sessions.length} sessions</small></article>
      </section>

      <div className="session-toolbar">
        <div className="session-tabs" role="tablist" aria-label="Filter sessions">
          {(["all", "running", "waiting", "idle", "paused", "failed"] as const).map((value) => (
            <button key={value} className={filter === value ? "active" : ""} onClick={() => setFilter(value)} role="tab" aria-selected={filter === value}>
              {value === "all" ? "All sessions" : value.charAt(0).toUpperCase() + value.slice(1)}
              {value !== "all" && <span>{sessions.filter((session) => session.status === value).length}</span>}
            </button>
          ))}
        </div>
        <label className="session-search"><span aria-hidden="true">⌕</span><input value={query} onChange={(event) => setQuery(event.currentTarget.value)} placeholder="Search sessions" aria-label="Search sessions" /></label>
      </div>

      <div className="sessions-workspace">
        <section className="session-list-panel" aria-label="Agent session list">
        <div className="panel-heading"><div><h2>Connected sessions</h2><span>{visibleSessions.length} of {sessions.length} sessions</span></div><button className="icon-button" aria-label="Session list options">•••</button></div>
          <div className="session-list">
            {visibleSessions.map((session) => (
              <button className={`session-list-item ${selectedSession.id === session.id ? "selected" : ""}`} key={session.id} onClick={() => setSelectedId(session.id)}>
                <div className={`session-avatar avatar-${session.status}`}>{session.project.charAt(0)}</div>
                <div className="session-list-copy">
                  <div className="session-list-title"><strong>{session.task}</strong>{session.managed && <span className="managed-tag">MANAGED</span>}</div>
                  <span className="session-project">{session.project} <code>{session.branch}</code></span>
                  <div className="session-list-meta"><SessionStatusBadge status={session.status} /><time>{formatLocalTimestamp(session.elapsed)}</time></div>
                </div>
                <span className="session-chevron">›</span>
              </button>
            ))}
            {visibleSessions.length === 0 && <div className="empty-session-list">No sessions match this filter.</div>}
          </div>
          <div className="read-only-note"><span>◌</span><div><strong>Read-only sessions are visible too</strong><span>Only app-managed sessions can receive replies or permission changes.</span></div></div>
        </section>

        <section className="session-detail-panel" aria-label="Selected agent session">
          <div className="detail-header">
            <div className="detail-title"><div className={`session-avatar avatar-${selectedSession.status}`}>{selectedSession.project.charAt(0)}</div><div><div className="detail-name-row"><h2>{selectedSession.task}</h2><SessionStatusBadge status={selectedSession.status} /></div><p>{selectedSession.project} <span>·</span> <code>{selectedSession.branch}</code></p></div></div>
            <div className="detail-actions">
              <button className="secondary-button" onClick={beginRenameSession} disabled={savingSessionName}>Rename</button>
              <button className="secondary-button archive-button" onClick={() => void updateArchiveState()} disabled={resuming}>{archivedView ? "Unarchive session" : "Archive session"}</button>
              {selectedSession.managed && selectedSession.provider === "copilot" && ["completed", "failed", "orphaned"].includes(selectedSession.status) && <button className="primary-button archive-button" onClick={() => void resumeSession()} disabled={resuming}>{resuming ? "Resuming…" : "Resume session"}</button>}
            </div>
          </div>
          {renamingSession && <form className="session-rename-form" onSubmit={renameSession}>
            <label className="field-label" htmlFor="session-name">SESSION NAME</label>
            <input id="session-name" className="settings-input" value={sessionNameDraft} onChange={(event) => setSessionNameDraft(event.currentTarget.value)} autoFocus maxLength={500} />
            <button className="secondary-button" type="button" onClick={() => setRenamingSession(false)} disabled={savingSessionName}>Cancel</button>
            <button className="primary-button" type="submit" disabled={savingSessionName || !sessionNameDraft.trim()}>{savingSessionName ? "Saving…" : "Save name"}</button>
          </form>}
          <div className="detail-metrics"><div><span>Current step</span><strong>{selectedSession.step}</strong></div><div><span>Elapsed</span><strong>{formatLocalTimestamp(selectedSession.elapsed)}</strong></div><div><span>Token usage</span><strong>{selectedSession.tokens}</strong></div><div><span>Session</span><strong className="mono" title={selectedSession.externalSessionId ?? selectedSession.id}>{selectedSession.externalSessionId || selectedSession.id}</strong></div></div>

          <div className="transcript-heading"><div><h3>Live transcript</h3><span className="live-caption"><span /> Updates every few seconds</span></div><button className="text-button">Open full transcript <span>↗</span></button></div>
          <div className="transcript" style={{ "--transcript-font-size": `${transcriptFontSize}px` } as CSSProperties}>
            {transcript.length === 0 && <div className="transcript-line active-line"><time>{formatLocalTimestamp(selectedSession.elapsed)}</time><div><span className="transcript-label agent-label">{selectedSession.provider.toUpperCase()}</span><p>{selectedSession.lastEvent || "No transcript events have been recorded."}</p><span className="typing-indicator"><i /><i /><i /> {selectedSession.status === "running" ? "Working" : "Idle"}</span></div></div>}
            {transcript.map((message, index) => (
              <div className={`transcript-line ${index === transcript.length - 1 ? "active-line" : ""}`} key={`${message.timestamp}-${index}`}>
                <time>{formatLocalTimestamp(message.timestamp)}</time>
                <div>
                  <span className={`transcript-label ${message.role === "user" ? "user-label" : "agent-label"}`}>{message.role === "user" ? "YOU" : selectedSession.provider.toUpperCase()}</span>
                  <p>{message.content}</p>
                  {index === transcript.length - 1 && selectedSession.status === "running" && <span className="typing-indicator"><i /><i /><i /> Working</span>}
                </div>
              </div>
            ))}
          </div>

          <div className="control-panel">
            <div className="control-panel-heading"><div><h3>Chat with Copilot</h3><span>Messages and responses are saved in Agent Control Plane.</span></div><span className={`managed-state ${selectedSession.managed ? "managed" : ""}`}><span /> {selectedSession.managed ? "App managed" : "Read only"}</span></div>
            {permissionRequest && <div className="permission-request" role="group" aria-label="Copilot permission request">
              <strong>{permissionRequest.title}</strong>
              <span>Choose an option to let Copilot continue.</span>
              <div>{permissionRequest.options.map((option) => <button key={option.optionId} className="secondary-button" onClick={() => void respondToPermission(option.optionId)}>{option.name}</button>)}
                <button className="danger-button" onClick={() => void respondToPermission(null)}>Cancel request</button>
              </div>
            </div>}
            <form className="reply-row" onSubmit={sendMessage}>
              <input
                value={messageDraft}
                onChange={(event) => setMessageDraft(event.currentTarget.value)}
                disabled={!selectedSession.managed || sendingMessage || !!permissionRequest || selectedSession.status === "running" || selectedSession.status === "starting"}
                placeholder={selectedSession.managed ? selectedSession.status === "waiting" ? "Reply to Copilot…" : "Message Copilot…" : "This session is read only"}
                aria-label="Message Copilot"
              />
              <button
                className="primary-button"
                type="submit"
                disabled={!selectedSession.managed || sendingMessage || !!permissionRequest || !messageDraft.trim() || selectedSession.status === "running" || selectedSession.status === "starting"}
              >
                {sendingMessage ? "Sending…" : "Send"} <span>↗</span>
              </button>
            </form>
            <div className="control-actions">
              <button
                className="danger-button stop-session-button"
                type="button"
                onClick={() => void stopSession()}
                disabled={!selectedSession.managed || stoppingSession || ["completed", "orphaned"].includes(selectedSession.status)}
              >
                {stoppingSession ? "Stopping…" : "Stop session"}
              </button>
            </div>
          </div>
        </section>
      </div>
    </div>
  );
}

function AttentionInboxView({
  items,
  loading,
  error,
  onRefresh,
  onOpenSession,
}: {
  items: AttentionItem[];
  loading: boolean;
  error: string;
  onRefresh: () => Promise<void>;
  onOpenSession: (sessionId: string) => void;
}) {
  const [filter, setFilter] = useState<AttentionKind | "all">("all");
  const [query, setQuery] = useState("");
  const [selectedId, setSelectedId] = useState("");
  const [actionError, setActionError] = useState("");
  const [workingItemId, setWorkingItemId] = useState("");
  const visibleItems = items.filter((item) => {
    const matchesFilter = filter === "all" || item.kind === filter;
    const searchable = `${item.title} ${item.project} ${item.branch} ${item.agent} ${item.summary}`.toLowerCase();
    return matchesFilter && searchable.includes(query.toLowerCase());
  });
  const selectedItem = visibleItems.find((item) => item.id === selectedId) ?? visibleItems[0];
  const priorityCount = items.filter((item) => item.priority === "High").length;
  const decisionCount = items.filter((item) => item.kind === "permission").length;
  const projectCount = new Set(items.map((item) => item.project)).size;
  const selectedFilterLabel = attentionFilters.find((item) => item.kind === filter)?.label.toLowerCase();

  async function respondToPermission(item: AttentionItem, optionId: string | null) {
    if (!item.permissionRequest) return;
    setWorkingItemId(item.id);
    setActionError("");
    try {
      await invoke("respond_session_permission", {
        sessionId: item.sessionId,
        requestId: item.permissionRequest.requestId,
        optionId,
      });
      await onRefresh();
    } catch (reason) {
      setActionError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setWorkingItemId("");
    }
  }

  async function archiveItem(item: AttentionItem) {
    setWorkingItemId(item.id);
    setActionError("");
    try {
      await invoke("set_session_archived", { sessionId: item.sessionId, archived: true });
      await onRefresh();
    } catch (reason) {
      setActionError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setWorkingItemId("");
    }
  }

  return (
    <div className="attention-page">
      <header className="attention-header">
        <div>
          <p className="eyebrow">WORKSPACE / REVIEW QUEUE</p>
          <h1>Attention inbox</h1>
          <p className="subtitle">Review pending permissions and failed sessions that need your attention.</p>
        </div>
        <div className="attention-header-actions">
          <span className="attention-live-badge"><span /> Local session data</span>
          <button className="secondary-button" type="button" onClick={() => void onRefresh()} disabled={loading}>
            <span className="refresh-glyph">↻</span> {loading ? "Refreshing…" : "Refresh"}
          </button>
        </div>
      </header>

      <div className="attention-stats" aria-label="Attention inbox summary">
        <article className="attention-stat">
          <span className="attention-stat-icon purple">◈</span>
          <div><strong>{String(items.length).padStart(2, "0")}</strong><span>Open items</span></div>
        </article>
        <article className="attention-stat">
          <span className="attention-stat-icon red">!</span>
          <div><strong>{String(priorityCount).padStart(2, "0")}</strong><span>High priority</span></div>
        </article>
        <article className="attention-stat">
          <span className="attention-stat-icon amber">◷</span>
          <div><strong>{String(decisionCount).padStart(2, "0")}</strong><span>Permissions pending</span></div>
        </article>
        <article className="attention-stat">
          <span className="attention-stat-icon blue">▣</span>
          <div><strong>{String(projectCount).padStart(2, "0")}</strong><span>Projects involved</span></div>
        </article>
      </div>

      <section className="attention-queue" aria-label="Attention queue">
        <div className="attention-toolbar">
          <div>
            <p className="eyebrow">YOUR QUEUE</p>
            <h2>Items to review <span className="queue-count">{visibleItems.length}</span></h2>
          </div>
          <label className="attention-search">
            <span aria-hidden="true">⌕</span>
            <input
              aria-label="Search attention items"
              placeholder="Search items"
              value={query}
              onChange={(event) => setQuery(event.currentTarget.value)}
            />
            {query && <button type="button" aria-label="Clear search" onClick={() => setQuery("")}>×</button>}
          </label>
        </div>

        {error && <div className="attention-error" role="alert">{error}</div>}
        {actionError && <div className="attention-error" role="alert">{actionError}</div>}

        <div className="attention-filters" role="group" aria-label="Filter attention items">
          {attentionFilters.map(({ label, kind }) => (
            <button
              className={`attention-filter ${filter === kind ? "active" : ""}`}
              key={kind}
              type="button"
              aria-pressed={filter === kind}
              onClick={() => setFilter(kind)}
            >
              {label}
              {kind !== "all" && <span>{items.filter((item) => item.kind === kind).length}</span>}
              {kind === "all" && <span>{items.length}</span>}
            </button>
          ))}
          <span className="attention-sort">Sorted by last updated ↓</span>
        </div>

        <div className="attention-workspace">
          <div className="attention-list" role="region" aria-label="Attention items">
            {visibleItems.map((item) => (
              <button
                className={`attention-item ${selectedItem?.id === item.id ? "selected" : ""}`}
                key={item.id}
                type="button"
                aria-pressed={selectedItem?.id === item.id}
                onClick={() => setSelectedId(item.id)}
              >
                <span className={`attention-kind-icon kind-${item.kind}`} aria-hidden="true">
                  {item.kind === "permission" ? "⌘" : item.kind === "failure" ? "!" : "✓"}
                </span>
                <span className="attention-item-copy">
                  <span className="attention-item-label">
                    <span className={`attention-kind-label kind-label-${item.kind}`}>{item.kind}</span>
                    <span className={`priority-label priority-${item.priority.toLowerCase()}`}>{item.priority}</span>
                  </span>
                  <strong>{item.title}</strong>
                  <span className="attention-item-project">{item.project} <i>·</i> {providerLabel(item.agent)}</span>
                  <span className="attention-item-summary">{item.summary}</span>
                  <span className="attention-item-time">{formatLocalTimestamp(item.updatedAt)}<span>·</span><code>{item.branch}</code></span>
                </span>
                <span className="attention-item-chevron" aria-hidden="true">›</span>
              </button>
            ))}
            {loading && visibleItems.length === 0 && (
              <div className="attention-empty"><strong>Loading attention items…</strong></div>
            )}
            {!loading && visibleItems.length === 0 && (
              <div className="attention-empty">
                {items.length === 0 && !query && filter === "all" ? (
                  <>
                    <span>✓</span>
                    <strong>You’re all caught up</strong>
                    <p>Pending permissions and failed sessions will appear here.</p>
                  </>
                ) : (
                  <>
                    <span>⌕</span>
                    <strong>No {query ? "matching" : selectedFilterLabel} items</strong>
                    <p>Try another filter or search term.</p>
                    <button type="button" onClick={() => { setFilter("all"); setQuery(""); }}>Clear filters</button>
                  </>
                )}
              </div>
            )}
          </div>

          {selectedItem ? (
            <article className="attention-detail">
              <div className="attention-detail-top">
                <span className={`attention-kind-label kind-label-${selectedItem.kind}`}>{selectedItem.kind}</span>
                <span className="attention-priority-note">{selectedItem.priority} priority</span>
              </div>
              <h3>{selectedItem.title}</h3>
              <p className="attention-detail-summary">{selectedItem.summary}</p>
              <div className="attention-context">
                <span className={`attention-kind-icon kind-${selectedItem.kind}`} aria-hidden="true">
                  {selectedItem.kind === "permission" ? "⌘" : "!"}
                </span>
                <div>
                  <span>{selectedItem.kind === "permission" ? "Permission requested" : selectedItem.kind === "failure" ? "Session needs attention" : "Completed session"}</span>
                  <p>{selectedItem.context}</p>
                </div>
              </div>
              <dl className="attention-metadata">
                <div><dt>PROJECT</dt><dd>{selectedItem.project}</dd></div>
                <div><dt>BRANCH</dt><dd><code>{selectedItem.branch}</code></dd></div>
                <div><dt>AGENT</dt><dd>{providerLabel(selectedItem.agent)}</dd></div>
                <div><dt>LAST UPDATED</dt><dd>{formatLocalTimestamp(selectedItem.updatedAt)}</dd></div>
              </dl>
              {selectedItem.permissionRequest && (
                <div className="attention-permission-options">
                  <strong>Choose a permission response</strong>
                  {selectedItem.permissionRequest.options.map((option) => (
                    <button
                      className="secondary-button"
                      key={option.optionId}
                      type="button"
                      disabled={workingItemId === selectedItem.id}
                      onClick={() => void respondToPermission(selectedItem, option.optionId)}
                    >
                      {option.name}
                    </button>
                  ))}
                  <button
                    className="attention-deny-button"
                    type="button"
                    disabled={workingItemId === selectedItem.id}
                    onClick={() => void respondToPermission(selectedItem, null)}
                  >
                    {workingItemId === selectedItem.id ? "Responding…" : "Deny request"}
                  </button>
                </div>
              )}
              <div className="attention-actions">
                <button className="primary-button" type="button" onClick={() => onOpenSession(selectedItem.sessionId)}>
                  Open session
                </button>
                {selectedItem.kind !== "permission" && (
                  <button
                    className="secondary-button"
                    type="button"
                    disabled={workingItemId === selectedItem.id}
                    onClick={() => void archiveItem(selectedItem)}
                  >
                    {workingItemId === selectedItem.id ? "Archiving…" : "Archive session"}
                  </button>
                )}
              </div>
            </article>
          ) : (
            <div className="attention-detail-empty">
              {loading ? "Loading session context…" : "Select an item to see its session context."}
            </div>
          )}
        </div>
      </section>
    </div>
  );
}

function ProjectsView({
  projects,
  loading,
  onAddProject,
  onOpenProject,
  onRefresh,
}: {
  projects: Project[];
  loading: boolean;
  onAddProject: () => void;
  onOpenProject: (projectId: string) => void;
  onRefresh: () => void;
}) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<"all" | "changed" | "clean">("all");
  const changedCount = projects.filter((project) => project.gitState === "dirty").length;
  const cleanCount = projects.length - changedCount;
  const branchCount = new Set(projects.map((project) => project.branch).filter(Boolean)).size;
  const normalizedQuery = query.trim().toLowerCase();
  const visibleProjects = projects.filter((project) => {
    const matchesFilter = filter === "all" || project.gitState === (filter === "changed" ? "dirty" : "clean");
    const searchable = `${project.name} ${project.path} ${project.branch} ${project.currentCommit}`.toLowerCase();
    return matchesFilter && searchable.includes(normalizedQuery);
  });

  return (
    <div className="projects-page">
      <header className="projects-page-header">
        <div>
          <p className="eyebrow">WORKSPACE / REPOSITORIES</p>
          <h1>Your projects</h1>
          <p className="subtitle">Local Git checkouts connected to Agent Control Plane.</p>
        </div>
        <div className="projects-header-actions">
          <span className="projects-local-badge"><span aria-hidden="true" /> Stored on this device</span>
          <button className="secondary-button projects-refresh-button" type="button" onClick={onRefresh} disabled={loading}>
            <span aria-hidden="true">↻</span> {loading ? "Refreshing…" : "Refresh"}
          </button>
          <button className="primary-button" type="button" onClick={onAddProject}>
            <span aria-hidden="true">+</span> Add project
          </button>
        </div>
      </header>

      <section className="projects-overview" aria-label="Project repository summary">
        <article className="projects-overview-card">
          <span className="projects-overview-icon overview-total" aria-hidden="true">⌘</span>
          <div><strong>{projects.length}</strong><span>Connected repositories</span></div>
        </article>
        <article className="projects-overview-card">
          <span className="projects-overview-icon overview-clean" aria-hidden="true">✓</span>
          <div><strong>{cleanCount}</strong><span>Clean working trees</span></div>
        </article>
        <article className="projects-overview-card">
          <span className="projects-overview-icon overview-changed" aria-hidden="true">≋</span>
          <div><strong>{changedCount}</strong><span>With local changes</span></div>
        </article>
        <article className="projects-overview-card">
          <span className="projects-overview-icon overview-branches" aria-hidden="true">⑂</span>
          <div><strong>{branchCount}</strong><span>Active branches</span></div>
        </article>
      </section>

      <section className="projects-directory" aria-labelledby="projects-directory-title">
        <div className="projects-directory-heading">
          <div>
            <p className="eyebrow">LOCAL REGISTRY</p>
            <h2 id="projects-directory-title">Repositories <span>{projects.length}</span></h2>
          </div>
          <label className="projects-search">
            <span aria-hidden="true">⌕</span>
            <input
              type="search"
              aria-label="Search projects"
              placeholder="Search name, path, branch…"
              value={query}
              onChange={(event) => setQuery(event.currentTarget.value)}
            />
            {query && <button type="button" aria-label="Clear project search" onClick={() => setQuery("")}>×</button>}
          </label>
        </div>

        <div className="projects-filter-bar" role="group" aria-label="Filter projects by Git state">
          {([
            ["all", "All projects", projects.length],
            ["changed", "Changes", changedCount],
            ["clean", "Clean", cleanCount],
          ] as const).map(([value, label, count]) => (
            <button
              className={`projects-filter ${filter === value ? "active" : ""}`}
              key={value}
              type="button"
              aria-pressed={filter === value}
              onClick={() => setFilter(value)}
            >
              {label}<span>{count}</span>
            </button>
          ))}
          <span className="projects-filter-context">Sorted by name</span>
        </div>

        {loading && projects.length === 0 ? (
          <div className="projects-empty-state" role="status">Loading local repositories…</div>
        ) : projects.length === 0 ? (
          <div className="projects-empty-state projects-empty-onboarding">
            <span className="projects-empty-icon" aria-hidden="true">⌘</span>
            <h3>No projects connected yet</h3>
            <p>Add a local Git checkout to keep its branch and working-tree status in one place.</p>
            <button className="primary-button" type="button" onClick={onAddProject}><span aria-hidden="true">+</span> Add your first project</button>
          </div>
        ) : visibleProjects.length === 0 ? (
          <div className="projects-empty-state">
            <h3>No matching projects</h3>
            <p>Try a different search or Git state filter.</p>
            <button className="text-button" type="button" onClick={() => { setQuery(""); setFilter("all"); }}>Clear search and filters</button>
          </div>
        ) : (
          <div className="projects-card-grid">
            {visibleProjects.map((project) => (
              <article className="project-directory-card" key={project.id}>
                <div className="project-directory-card-top">
                  <span className="project-directory-avatar" aria-hidden="true">{project.name.charAt(0).toUpperCase()}</span>
                  <span className={`project-state-badge ${project.gitState}`}>
                    <span aria-hidden="true" />
                    {project.gitState === "clean" ? "Working tree clean" : "Uncommitted changes"}
                  </span>
                </div>
                <h3 title={project.name}>{project.name}</h3>
                <p className="project-directory-path" title={project.path}>{project.path}</p>
                <div className="project-directory-meta">
                  <div>
                    <span>BRANCH</span>
                    <code title={project.branch}>{project.branch || "No branch"}</code>
                  </div>
                  <div>
                    <span>LAST COMMIT</span>
                    <code title={project.currentCommit}>{project.currentCommit ? project.currentCommit.slice(0, 7) : "—"}</code>
                  </div>
                </div>
                <button className="project-open-button" type="button" onClick={() => onOpenProject(project.id)}>
                  Open workspace <span aria-hidden="true">→</span>
                </button>
              </article>
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

function ProjectWorkspaceView({
  project,
  transcriptFontSize,
  onProjectUpdated,
}: {
  project: Project;
  transcriptFontSize: number;
  onProjectUpdated: (project: Project) => void;
}) {
  const [activeSection, setActiveSection] = useState<ProjectSection>("tasks");
  const [sessionToOpen, setSessionToOpen] = useState("");
  const [savingAutomation, setSavingAutomation] = useState(false);
  const [openingEditor, setOpeningEditor] = useState(false);
  const [workspaceError, setWorkspaceError] = useState("");
  const [openAutomationHelp, setOpenAutomationHelp] = useState<"looping" | "agentTasks" | null>(null);

  async function updateAutomation(looping: boolean, agentTaskCreation: boolean) {
    setSavingAutomation(true);
    setWorkspaceError("");
    try {
      const updated = await invoke<Project>("set_project_automation", {
        projectId: project.id,
        looping,
        agentTaskCreation,
      });
      onProjectUpdated(updated);
    } catch (reason) {
      setWorkspaceError(reason instanceof Error ? reason.message : String(reason));
      try {
        const latest = (await invoke<Project[]>("list_projects")).find((item) => item.id === project.id);
        if (latest) onProjectUpdated(latest);
      } catch (refreshError) {
        setWorkspaceError(`${reason instanceof Error ? reason.message : String(reason)}; unable to refresh project settings: ${refreshError instanceof Error ? refreshError.message : String(refreshError)}`);
      }
    } finally {
      setSavingAutomation(false);
    }
  }

  async function openInVsCode() {
    setOpeningEditor(true);
    setWorkspaceError("");
    try {
      await invoke("open_project_in_vscode", { projectId: project.id });
    } catch (reason) {
      setWorkspaceError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setOpeningEditor(false);
    }
  }

  return (
    <div className="project-workspace-page">
      <header className="project-workspace-header">
        <div className="project-workspace-heading">
          <p className="eyebrow">PROJECT WORKSPACE</p>
          <h1>{project.name}</h1>
          <p className="project-workspace-path" title={project.path}>{project.path}</p>
        </div>
        <div className="project-workspace-repo-meta">
          <span className={`project-state-badge ${project.gitState}`}>
            <span aria-hidden="true" />
            {project.gitState === "clean" ? "Working tree clean" : "Uncommitted changes"}
          </span>
          <span className="project-workspace-branch"><span aria-hidden="true">⑂</span><code>{project.branch || "No branch"}</code></span>
        </div>
        <div className="project-workspace-actions">
          <div className="project-automation-control">
            <label className="project-automation-toggle">
              <input
                type="checkbox"
                aria-label="Enable project looping"
                checked={project.looping}
                disabled={savingAutomation}
                onChange={(event) => void updateAutomation(event.target.checked, project.agentTaskCreation)}
              />
              <span>Looping</span>
            </label>
            <button
              className="project-info-button"
              type="button"
              aria-label="Looping information"
              aria-expanded={openAutomationHelp === "looping"}
              aria-controls="project-loop-help"
              onClick={() => setOpenAutomationHelp(openAutomationHelp === "looping" ? null : "looping")}
            >i</button>
            <div className="project-automation-help" id="project-loop-help" role="note" hidden={openAutomationHelp !== "looping"}>
              Runs selected tasks one at a time until none remain. Loop-launched tasks may auto-approve one-time tools; failures still need you. There is no token cap yet.
            </div>
          </div>
          <div className="project-automation-control">
            <label className="project-automation-toggle">
              <input
                type="checkbox"
                aria-label="Allow agent task proposals"
                checked={project.agentTaskCreation}
                disabled={savingAutomation}
                onChange={(event) => void updateAutomation(project.looping, event.target.checked)}
              />
              <span>Agent tasks</span>
            </label>
            <button
              className="project-info-button"
              type="button"
              aria-label="Agent tasks information"
              aria-expanded={openAutomationHelp === "agentTasks"}
              aria-controls="project-agent-task-help"
              onClick={() => setOpenAutomationHelp(openAutomationHelp === "agentTasks" ? null : "agentTasks")}
            >i</button>
            <div className="project-automation-help" id="project-agent-task-help" role="note" hidden={openAutomationHelp !== "agentTasks"}>
              Save agent-proposed follow-up tasks to the backlog. They run only when looping is enabled and the task is selected.
            </div>
          </div>
          <button className="project-editor-button" type="button" disabled={openingEditor} onClick={() => void openInVsCode()}>
            {openingEditor ? "Opening…" : "VS Code"}
          </button>
        </div>
      </header>
      {workspaceError && <p className="project-feature-error" role="alert">{workspaceError}</p>}

      <nav className="project-section-tabs" aria-label={`${project.name} sections`}>
        {projectSections.map(({ id, label }) => (
          <button
            className={activeSection === id ? "active" : ""}
            key={id}
            type="button"
            aria-current={activeSection === id ? "page" : undefined}
            onClick={() => setActiveSection(id)}
          >
            {label}
          </button>
        ))}
      </nav>

      {activeSection === "sessions" ? (
        <div className="project-section-content">
          <AgentSessionsView
            transcriptFontSize={transcriptFontSize}
            projectId={project.id}
            initialSessionId={sessionToOpen}
          />
        </div>
      ) : (
        <div className="project-section-content">
          {activeSection === "tasks" && (
            <ProjectTasksTab
              project={project}
              onOpenSession={(sessionId) => {
                setSessionToOpen(sessionId);
                setActiveSection("sessions");
              }}
            />
          )}
          {activeSection === "diffs" && <ProjectDiffsTab project={project} />}
          {activeSection === "terminals" && <ProjectTerminalsTab project={project} />}
          {activeSection === "pipelines" && <ProjectPipelinesTab project={project} />}
          {activeSection === "dependencies" && <ProjectDependenciesTab project={project} />}
          {activeSection === "skills" && <ProjectSkillsTab project={project} />}
        </div>
      )}
    </div>
  );
}

function App() {
  const [activeNav, setActiveNav] = useState("Dashboard");
  const [sessionToOpen, setSessionToOpen] = useState("");
  const [showAddProject, setShowAddProject] = useState(false);
  const [projects, setProjects] = useState<Project[]>([]);
  const [selectedProjectId, setSelectedProjectId] = useState("");
  const [projectPath, setProjectPath] = useState("");
  const [loadingProjects, setLoadingProjects] = useState(true);
  const [savingProject, setSavingProject] = useState(false);
  const [pickingProjectFolder, setPickingProjectFolder] = useState(false);
  const [error, setError] = useState("");
  const [settings, setSettings] = useState<SettingsForm>({ ...emptySettings(), githubToken: "" });
  const [savingSettings, setSavingSettings] = useState(false);
  const [attentionItems, setAttentionItems] = useState<AttentionItem[]>([]);
  const [attentionLoading, setAttentionLoading] = useState(true);
  const [attentionError, setAttentionError] = useState("");

  async function loadProjects() {
    setLoadingProjects(true);
    try {
      setProjects(await invoke<Project[]>("list_projects"));
      setError("");
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setLoadingProjects(false);
    }
  }

  async function refreshAttentionItems() {
    setAttentionLoading(true);
    setAttentionError("");
    try {
      setAttentionItems(await loadAttentionItemsFromBackend());
    } catch (reason) {
      setAttentionError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setAttentionLoading(false);
    }
  }

  useEffect(() => {
    void loadProjects();
    void loadSettings();
  }, []);

  useEffect(() => {
    void refreshAttentionItems();
    let disposed = false;
    let unsubscribe: (() => void) | undefined;
    void listen<string>("agent-session-updated", () => {
      void refreshAttentionItems();
    }).then((unlisten) => {
      if (disposed) {
        unlisten();
      } else {
        unsubscribe = unlisten;
      }
    }).catch((reason) => {
      setAttentionError(reason instanceof Error ? reason.message : String(reason));
    });
    return () => {
      disposed = true;
      unsubscribe?.();
    };
  }, []);

  useEffect(() => {
    let disposed = false;
    let unsubscribe: (() => void) | undefined;
    void listen<string>("project-loop-error", (event) => {
      if (!disposed) setError(event.payload);
    }).then((unlisten) => {
      if (disposed) unlisten();
      else unsubscribe = unlisten;
    }).catch((reason) => {
      setError(reason instanceof Error ? reason.message : String(reason));
    });
    return () => {
      disposed = true;
      unsubscribe?.();
    };
  }, []);

  async function loadSettings() {
    try {
      const loaded = await invoke<Settings>("load_settings");
      setSettings({ ...emptySettings(), ...loaded, transcriptFontSize: loaded.transcriptFontSize ?? 16, githubToken: "" });
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  async function saveSettings(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setSavingSettings(true);
    setError("");
    try {
      const { githubToken, ...input } = settings;
      const saved = await invoke<Settings>("save_settings", {
        input: { ...input, githubToken: input.githubAuthMethod === "token" ? githubToken : "" },
      });
      setSettings({ ...emptySettings(), ...saved, transcriptFontSize: saved.transcriptFontSize ?? 16, githubToken: "" });
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setSavingSettings(false);
    }
  }

  async function addProject(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setSavingProject(true);
    setError("");
    try {
      const registeredProject = await invoke<Project>("register_project", { path: projectPath });
      setProjects((current) => current.some((project) => project.id === registeredProject.id)
        ? current.map((project) => project.id === registeredProject.id ? registeredProject : project)
        : [...current, registeredProject]);
      setSelectedProjectId(registeredProject.id);
      setActiveNav("Project workspace");
      setProjectPath("");
      setShowAddProject(false);
      await loadProjects();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setSavingProject(false);
    }

  }

  async function chooseProjectFolder() {
    setPickingProjectFolder(true);
    setError("");
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: "Choose a project folder",
      });
      if (typeof selected === "string") {
        setProjectPath(selected);
      }
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setPickingProjectFolder(false);
    }
  }

  function openAttentionSession(sessionId: string) {
    setSessionToOpen(sessionId);
    setActiveNav("Agent sessions");
  }

  function openProjectWorkspace(projectId: string) {
    setSelectedProjectId(projectId);
    setActiveNav("Project workspace");
  }

  function updateProject(updatedProject: Project) {
    setProjects((current) => current.map((project) => project.id === updatedProject.id ? updatedProject : project));
  }

  const selectedProject = projects.find((project) => project.id === selectedProjectId);
  const urgentAttentionCount = attentionItems.filter((item) => item.priority === "High").length;

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-mark">A</div>
          <div>
            <strong>Agent Control</strong>
            <span>PLANE</span>
          </div>
        </div>

        <nav aria-label="Primary navigation">
          <p className="nav-label">Workspace</p>
          {navItems.map(([icon, label]) => (
            <button
              className={`nav-item ${activeNav === label || (label === "Projects" && activeNav === "Project workspace") ? "selected" : ""}`}
              key={label}
              onClick={() => setActiveNav(label)}
            >
              <span className="nav-icon" aria-hidden="true">{icon}</span>
              <span>{label}</span>
              {label === "Attention inbox" && attentionItems.length > 0 && <span className="nav-count">{attentionItems.length}</span>}
            </button>
          ))}
        </nav>

        <nav className="sidebar-projects" aria-label="Projects">
          <div className="sidebar-projects-heading">
            <p className="nav-label">Projects <span>{projects.length}</span></p>
            <button type="button" aria-label="Add project" onClick={() => setShowAddProject(true)}>+</button>
          </div>
          <div className="sidebar-project-list">
            {loadingProjects && projects.length === 0 ? <p className="sidebar-project-empty">Loading projects…</p> : projects.length === 0 ? (
              <p className="sidebar-project-empty">No projects yet</p>
            ) : projects.map((project) => (
              <button
                className={`sidebar-project-item ${activeNav === "Project workspace" && selectedProjectId === project.id ? "selected" : ""}`}
                key={project.id}
                type="button"
                title={project.path}
                aria-current={activeNav === "Project workspace" && selectedProjectId === project.id ? "page" : undefined}
                onClick={() => openProjectWorkspace(project.id)}
              >
                <span className="sidebar-project-dot" aria-hidden="true" />
                <span>{project.name}</span>
              </button>
            ))}
          </div>
        </nav>

        <div className="sidebar-footer">
          <div className="connection">
            <span className="connection-dot" />
            <div>
              <strong>Supervisor online</strong>
              <span>Local runtime connected</span>
            </div>
          </div>
          <div className="footer-stats">
            <span><strong>{projects.length}</strong> projects connected</span>
          </div>
        </div>
      </aside>

      <main className="main-content">
        {activeNav === "Settings" ? <SettingsView settings={settings} setSettings={setSettings} onSave={saveSettings} saving={savingSettings} /> : activeNav === "Agent sessions" ? <AgentSessionsView transcriptFontSize={settings.transcriptFontSize} initialSessionId={sessionToOpen} /> : activeNav === "Attention inbox" ? <AttentionInboxView items={attentionItems} loading={attentionLoading} error={attentionError} onRefresh={refreshAttentionItems} onOpenSession={openAttentionSession} /> : activeNav === "Project workspace" && selectedProject ? <ProjectWorkspaceView key={selectedProject.id} project={selectedProject} transcriptFontSize={settings.transcriptFontSize} onProjectUpdated={updateProject} /> : activeNav === "Projects" || activeNav === "Project workspace" ? <ProjectsView projects={projects} loading={loadingProjects} onAddProject={() => setShowAddProject(true)} onOpenProject={openProjectWorkspace} onRefresh={() => void loadProjects()} /> : <>
        <header className="topbar">
          <div>
            <p className="eyebrow">Thursday, November 6, 2026</p>
            <h1>Good morning, Osen <span className="wave">✦</span></h1>
            <p className="subtitle">Here is what is happening across your projects.</p>
          </div>
          <button className="primary-button" onClick={() => setShowAddProject(true)}>
            <span aria-hidden="true">+</span> Add project
          </button>
        </header>

        <section className="summary-grid" aria-label="Workspace summary">
          <article className="summary-card">
            <div className="card-heading"><span>ACTIVE AGENTS</span><span className="card-icon purple">◉</span></div>
            <strong className="metric">{projects.length} <small>projects</small></strong>
            <div className="metric-meta">Connected local repositories</div>
            <div className="mini-bars"><i /><i /><i className="muted" /><i className="muted" /><i className="muted" /></div>
          </article>
          <article className="summary-card">
            <div className="card-heading"><span>NEEDS ATTENTION</span><span className="card-icon amber">!</span></div>
            <strong className="metric">{attentionItems.length}</strong>
            <div className="metric-meta">{urgentAttentionCount} high priority item{urgentAttentionCount === 1 ? "" : "s"}</div>
            <div className="severity-bar">{attentionItems.slice(0, 4).map((item) => <i key={item.id} />)}{attentionItems.length === 0 && <i className="muted" />}</div>
          </article>
          <article className="summary-card">
            <div className="card-heading"><span>READY BACKLOG</span><span className="card-icon blue">≡</span></div>
            <strong className="metric">—</strong>
            <div className="metric-meta">Tasks arrive in Phase 1</div>
            <div className="backlog-line"><span style={{ width: "72%" }} /></div>
          </article>
          <article className="summary-card">
            <div className="card-heading"><span>PIPELINE HEALTH</span><span className="card-icon green">↗</span></div>
            <strong className="metric">92<small>%</small></strong>
            <div className="metric-meta"><span className="trend-up">↑ 4.8%</span> over last 20 runs</div>
            <div className="sparkline" aria-label="Pipeline health trend"><i /><i /><i /><i /><i /><i /><i /><i /></div>
          </article>
        </section>

        <section className="section-block projects-section">
          <div className="section-header">
            <div><h2>Projects</h2><span className="section-caption">Live workspace overview</span></div>
            <button className="text-button" onClick={() => setActiveNav("Projects")}>View all projects <span>→</span></button>
          </div>
          <div className="table-card">
            <div className="table-row table-head"><span>PROJECT</span><span>ACTIVE AGENTS</span><span>PROGRESS</span><span>PIPELINE</span><span>GIT STATE</span><span /></div>
            {loadingProjects && <div className="empty-row">Loading local projects…</div>}
            {!loadingProjects && projects.length === 0 && <div className="empty-row">No projects connected yet. Add a local Git repository to get started.</div>}
            {!loadingProjects && projects.map((project) => (
              <div className="table-row" key={project.name}>
                <div className="project-name"><span className="project-avatar">{project.name.charAt(0)}</span><div><strong>{project.name}</strong><span><code>{project.branch}</code> · {project.path}</span></div></div>
                <span className="agent-count"><span className="idle-dot" /> No agents</span>
                <div className="progress-cell"><div className="progress-track"><span style={{ width: "0%" }} /></div><small>—</small></div>
                <StatusBadge status="waiting" />
                <span className={`git-state ${project.gitState}`}><span /> {project.gitState === "clean" ? "Clean" : "Changes"}</span>
                <button className="row-action" aria-label={`Open ${project.name}`} onClick={() => openProjectWorkspace(project.id)}>→</button>
              </div>
            ))}
          </div>
        </section>

        <div className="lower-grid">
          <section className="section-block">
            <div className="section-header"><div><h2>Attention inbox</h2><span className="section-caption">Items that need your decision</span></div><button className="text-button" onClick={() => setActiveNav("Attention inbox")}>Open inbox <span>→</span></button></div>
            <div className="inbox-card">
              {attentionLoading && attentionItems.length === 0 && <div className="empty-card">Loading attention items…</div>}
              {!attentionLoading && attentionError && attentionItems.length === 0 && <div className="empty-card">{attentionError}</div>}
              {!attentionLoading && !attentionError && attentionItems.length === 0 && <div className="empty-card">You’re all caught up. New permissions and failed sessions will appear here.</div>}
              {attentionItems.slice(0, 4).map((item) => (
                <button className="inbox-item" key={item.id} onClick={() => setActiveNav("Attention inbox")}>
                  <span className={`inbox-icon ${item.kind === "failure" ? "failure" : "high"}`}>{item.kind === "permission" ? "⌘" : "!"}</span>
                  <div><strong>{item.title}</strong><span>{item.project} · {providerLabel(item.agent)}</span></div>
                  <time>{formatLocalTimestamp(item.updatedAt)}</time>
                </button>
              ))}
            </div>
          </section>
          <section className="section-block activity-section">
            <div className="section-header"><div><h2>Live activity</h2><span className="section-caption live-caption"><span /> Streaming now</span></div><button className="text-button">View transcript <span>→</span></button></div>
            <div className="activity-card">
              <div className="empty-card">Live activity will appear when an agent session is connected.</div>
            </div>
          </section>
        </div>
        </>}
      </main>

      {showAddProject && (
        <div className="modal-backdrop" role="presentation" onClick={() => setShowAddProject(false)}>
          <form className="modal" role="dialog" aria-modal="true" aria-labelledby="add-project-title" onSubmit={addProject} onClick={(event) => event.stopPropagation()}>
            <div className="modal-title"><div><p className="eyebrow">PROJECT SETUP</p><h2 id="add-project-title">Add a local project</h2></div><button type="button" className="close-button" onClick={() => setShowAddProject(false)} aria-label="Close">×</button></div>
            <p className="modal-copy">Connect an existing local Git checkout. Remote cloning is not available in the first release.</p>
            <label className="field-label" htmlFor="project-path">Repository path</label>
            <div className="path-picker-row"><input id="project-path" className="path-input" value={projectPath} onChange={(event) => setProjectPath(event.currentTarget.value)} placeholder="/Users/you/Projects/my-repository" autoFocus /><button type="button" className="secondary-button browse-button" onClick={() => void chooseProjectFolder()} disabled={pickingProjectFolder}>{pickingProjectFolder ? "Opening…" : "Open folder"}</button></div>
            <div className="modal-actions"><button type="button" className="secondary-button" onClick={() => setShowAddProject(false)}>Cancel</button><button type="submit" className="primary-button" disabled={savingProject || !projectPath.trim()}>{savingProject ? "Validating…" : "Validate repository"}</button></div>
          </form>
        </div>
      )}
      {error && <div className="error-toast" role="alert"><strong>Action failed</strong><span>{error}</span><button onClick={() => setError("")} aria-label="Dismiss error">×</button></div>}
    </div>
  );
}

export default App;
