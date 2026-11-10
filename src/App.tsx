import { CSSProperties, FormEvent, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import "./App.css";

type Status = "healthy" | "running" | "waiting" | "failed";

type Project = {
  id: string;
  name: string;
  path: string;
  branch: string;
  currentCommit: string;
  gitState: "clean" | "dirty";
};

type ToolSettings = {
  enabled: boolean;
};

type Settings = {
  copilot: ToolSettings;
  gemini: ToolSettings;
  claudeCode: ToolSettings;
  githubUsername: string;
  githubTokenConfigured: boolean;
  defaultProvider: "copilot" | "gemini" | "claudeCode";
  transcriptFontSize: number;
};

type SettingsForm = Settings & { githubToken: string };

type SessionStatus = "starting" | "running" | "waiting" | "idle" | "paused" | "completed" | "failed" | "orphaned" | "unknown";

type AgentSession = {
  id: string;
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
        <div className="section-header"><div><h2>GitHub access</h2><span className="section-caption">Used for Actions, releases, repository analytics, and deep links.</span></div></div>
        <div className="github-card">
          <div className="github-heading"><div className="github-logo">GH</div><div><h3>GitHub account</h3><span>Tokens are stored only in the operating system credential vault.</span></div><span className={`setup-state ${settings.githubTokenConfigured ? "configured" : ""}`}><span /> {settings.githubTokenConfigured ? "Token saved" : "Token not set"}</span></div>
          <div className="github-fields"><div><label className="field-label" htmlFor="github-username">USERNAME</label><input id="github-username" className="settings-input" value={settings.githubUsername} onChange={(event) => setSettings({ ...settings, githubUsername: event.currentTarget.value })} placeholder="GitHub username" /></div><div><label className="field-label" htmlFor="github-token">PERSONAL ACCESS TOKEN</label><input id="github-token" className="settings-input" type="password" autoComplete="new-password" value={settings.githubToken} onChange={(event) => setSettings({ ...settings, githubToken: event.currentTarget.value })} placeholder={settings.githubTokenConfigured ? "Leave blank to keep saved token" : "ghp_…"} /></div></div>
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

function AgentSessionsView({ transcriptFontSize }: { transcriptFontSize: number }) {
  const [sessions, setSessions] = useState<AgentSession[]>([]);
  const [selectedId, setSelectedId] = useState("");
  const [filter, setFilter] = useState<"all" | SessionStatus>("all");
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  const [projects, setProjects] = useState<Project[]>([]);
  const [selectedProjectId, setSelectedProjectId] = useState("");
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
      setSelectedProjectId(loaded[0]?.id ?? "");
    }).catch((reason) => {
      setLoadError(reason instanceof Error ? reason.message : String(reason));
    });
  }, [archivedView]);

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
      });
      setSessions(loaded);
      setSelectedId((current) => loaded.some((session) => session.id === current) ? current : loaded[0]?.id ?? "");
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
          <label className="field-label" htmlFor="session-project">PROJECT</label>
          <select id="session-project" className="settings-input" value={selectedProjectId} onChange={(event) => setSelectedProjectId(event.currentTarget.value)}>
            {projects.map((project) => <option value={project.id} key={project.id}>{project.name} · {project.branch}</option>)}
          </select>
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
        <label className="field-label" htmlFor="session-project">PROJECT</label>
        <select id="session-project" className="settings-input" value={selectedProjectId} onChange={(event) => setSelectedProjectId(event.currentTarget.value)} disabled={projects.length === 0}>
          {projects.length === 0 ? <option value="">No projects connected</option> : projects.map((project) => <option value={project.id} key={project.id}>{project.name} · {project.branch}</option>)}
        </select>
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
              <button className="secondary-button archive-button" onClick={() => void updateArchiveState()} disabled={resuming}>{archivedView ? "Unarchive session" : "Archive session"}</button>
              {selectedSession.managed && selectedSession.provider === "copilot" && ["completed", "failed", "orphaned"].includes(selectedSession.status) && <button className="primary-button archive-button" onClick={() => void resumeSession()} disabled={resuming}>{resuming ? "Resuming…" : "Resume session"}</button>}
            </div>
          </div>
          <div className="detail-metrics"><div><span>Current step</span><strong>{selectedSession.step}</strong></div><div><span>Elapsed</span><strong>{formatLocalTimestamp(selectedSession.elapsed)}</strong></div><div><span>Token usage</span><strong>{selectedSession.tokens}</strong></div><div><span>Session</span><strong className="mono">sess_8f31c2</strong></div></div>

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
                disabled={!selectedSession.managed || sendingMessage || !!permissionRequest || selectedSession.status === "running" || selectedSession.status === "starting" || selectedSession.status === "waiting"}
                placeholder={selectedSession.managed ? "Message Copilot…" : "This session is read only"}
                aria-label="Message Copilot"
              />
              <button
                className="primary-button"
                type="submit"
                disabled={!selectedSession.managed || sendingMessage || !!permissionRequest || !messageDraft.trim() || selectedSession.status === "running" || selectedSession.status === "starting" || selectedSession.status === "waiting"}
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

function App() {
  const [activeNav, setActiveNav] = useState("Dashboard");
  const [showAddProject, setShowAddProject] = useState(false);
  const [projects, setProjects] = useState<Project[]>([]);
  const [projectPath, setProjectPath] = useState("");
  const [loadingProjects, setLoadingProjects] = useState(true);
  const [savingProject, setSavingProject] = useState(false);
  const [pickingProjectFolder, setPickingProjectFolder] = useState(false);
  const [error, setError] = useState("");
  const [settings, setSettings] = useState<SettingsForm>({ ...emptySettings(), githubToken: "" });
  const [savingSettings, setSavingSettings] = useState(false);

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

  useEffect(() => {
    void loadProjects();
    void loadSettings();
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
      const saved = await invoke<Settings>("save_settings", { input: { ...input, githubToken } });
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
      await invoke<Project>("register_project", { path: projectPath });
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
              className={`nav-item ${activeNav === label ? "selected" : ""}`}
              key={label}
              onClick={() => setActiveNav(label)}
            >
              <span className="nav-icon" aria-hidden="true">{icon}</span>
              <span>{label}</span>
              {label === "Attention inbox" && <span className="nav-count">4</span>}
            </button>
          ))}
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
            <span><strong>3</strong> projects</span>
            <span><strong>3</strong> active sessions</span>
          </div>
        </div>
      </aside>

      <main className="main-content">
        {activeNav === "Settings" ? <SettingsView settings={settings} setSettings={setSettings} onSave={saveSettings} saving={savingSettings} /> : activeNav === "Agent sessions" ? <AgentSessionsView transcriptFontSize={settings.transcriptFontSize} /> : <>
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
            <strong className="metric">4</strong>
            <div className="metric-meta">2 high priority items</div>
            <div className="severity-bar"><i /><i /><i className="muted" /><i className="muted" /></div>
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
            <button className="text-button">View all projects <span>→</span></button>
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
                <button className="row-action" aria-label={`Open ${project.name}`}>→</button>
              </div>
            ))}
          </div>
        </section>

        <div className="lower-grid">
          <section className="section-block">
            <div className="section-header"><div><h2>Attention inbox</h2><span className="section-caption">Items that need your decision</span></div><button className="text-button">Open inbox <span>→</span></button></div>
              <div className="inbox-card">
                <div className="empty-card">The attention inbox will become active when supervised runs are available.</div>
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
