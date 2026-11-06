import { FormEvent, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
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
  executable: string;
  profile: string;
  configured: boolean;
};

type Settings = {
  copilot: ToolSettings;
  gemini: ToolSettings;
  claudeCode: ToolSettings;
  githubUsername: string;
  githubTokenConfigured: boolean;
};

type SettingsForm = Settings & { githubToken: string };

const emptyToolSettings = (): ToolSettings => ({
  enabled: false,
  executable: "",
  profile: "",
  configured: false,
});

const emptySettings = (): Settings => ({
  copilot: emptyToolSettings(),
  gemini: emptyToolSettings(),
  claudeCode: emptyToolSettings(),
  githubUsername: "",
  githubTokenConfigured: false,
});

const navItems = [
  ["⌂", "Dashboard"],
  ["▣", "Projects"],
  ["◉", "Agent sessions"],
  ["!", "Attention inbox"],
  ["↗", "Pipeline"],
  ["◌", "Analytics"],
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
  const updateTool = (key: "copilot" | "gemini" | "claudeCode", field: keyof ToolSettings, value: string | boolean) => {
    setSettings({ ...settings, [key]: { ...settings[key], [field]: value } });
  };
  const tools = [
    { key: "copilot" as const, label: "GitHub Copilot", command: "copilot", description: "Use Copilot CLI for delegated agent runs." },
    { key: "gemini" as const, label: "Gemini CLI", command: "gemini", description: "Use Gemini CLI as an additional agent provider." },
    { key: "claudeCode" as const, label: "Claude Code", command: "claude", description: "Use Claude Code for supervised implementation sessions." },
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
                <label className="field-label" htmlFor={`${tool.key}-executable`}>EXECUTABLE</label>
                <input id={`${tool.key}-executable`} className="settings-input" value={value.executable} onChange={(event) => updateTool(tool.key, "executable", event.currentTarget.value)} placeholder={tool.command} />
                <label className="field-label" htmlFor={`${tool.key}-profile`}>PROFILE / CONFIGURATION (OPTIONAL)</label>
                <input id={`${tool.key}-profile`} className="settings-input" value={value.profile} onChange={(event) => updateTool(tool.key, "profile", event.currentTarget.value)} placeholder="Default profile" />
                <span className={`setup-state ${value.configured ? "configured" : ""}`}><span /> {value.configured ? "Executable configured" : "Not configured"}</span>
              </article>
            );
          })}
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

function App() {
  const [activeNav, setActiveNav] = useState("Dashboard");
  const [showAddProject, setShowAddProject] = useState(false);
  const [projects, setProjects] = useState<Project[]>([]);
  const [projectPath, setProjectPath] = useState("");
  const [loadingProjects, setLoadingProjects] = useState(true);
  const [savingProject, setSavingProject] = useState(false);
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
      setSettings({ ...loaded, githubToken: "" });
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
      setSettings({ ...saved, githubToken: "" });
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
        {activeNav === "Settings" ? <SettingsView settings={settings} setSettings={setSettings} onSave={saveSettings} saving={savingSettings} /> : <>
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
            <input id="project-path" className="path-input" value={projectPath} onChange={(event) => setProjectPath(event.currentTarget.value)} placeholder="/Users/you/Projects/my-repository" autoFocus />
            <div className="modal-actions"><button type="button" className="secondary-button" onClick={() => setShowAddProject(false)}>Cancel</button><button type="submit" className="primary-button" disabled={savingProject || !projectPath.trim()}>{savingProject ? "Validating…" : "Validate repository"}</button></div>
          </form>
        </div>
      )}
      {error && <div className="error-toast" role="alert"><strong>Action failed</strong><span>{error}</span><button onClick={() => setError("")} aria-label="Dismiss error">×</button></div>}
    </div>
  );
}

export default App;
