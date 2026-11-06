import { useState } from "react";
import "./App.css";

type Status = "healthy" | "running" | "waiting" | "failed";

type Project = {
  name: string;
  path: string;
  branch: string;
  agents: number;
  progress: number;
  pipeline: Status;
  git: "clean" | "dirty";
};

const projects: Project[] = [
  {
    name: "Agent Control Plane",
    path: "~/Work/AgentControlPlane",
    branch: "main",
    agents: 2,
    progress: 68,
    pipeline: "healthy",
    git: "dirty",
  },
  {
    name: "ClickShare Desktop",
    path: "~/Work/ClickShare",
    branch: "feature/session-recovery",
    agents: 1,
    progress: 42,
    pipeline: "running",
    git: "clean",
  },
  {
    name: "Protocol Gateway",
    path: "~/Work/ProtocolGateway",
    branch: "develop",
    agents: 0,
    progress: 91,
    pipeline: "waiting",
    git: "clean",
  },
];

const navItems = [
  ["⌂", "Dashboard"],
  ["▣", "Projects"],
  ["◉", "Agent sessions"],
  ["!", "Attention inbox"],
  ["↗", "Pipeline"],
  ["◌", "Analytics"],
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

function App() {
  const [activeNav, setActiveNav] = useState("Dashboard");
  const [showAddProject, setShowAddProject] = useState(false);

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
            <strong className="metric">3 <small>/ 5</small></strong>
            <div className="metric-meta"><span className="trend-up">↑ 1</span> since yesterday</div>
            <div className="mini-bars"><i /><i /><i /><i className="muted" /><i className="muted" /></div>
          </article>
          <article className="summary-card">
            <div className="card-heading"><span>NEEDS ATTENTION</span><span className="card-icon amber">!</span></div>
            <strong className="metric">4</strong>
            <div className="metric-meta">2 high priority items</div>
            <div className="severity-bar"><i /><i /><i className="muted" /><i className="muted" /></div>
          </article>
          <article className="summary-card">
            <div className="card-heading"><span>READY BACKLOG</span><span className="card-icon blue">≡</span></div>
            <strong className="metric">12</strong>
            <div className="metric-meta"><span className="trend-up">↑ 3</span> tasks ready to run</div>
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
            {projects.map((project) => (
              <div className="table-row" key={project.name}>
                <div className="project-name"><span className="project-avatar">{project.name.charAt(0)}</span><div><strong>{project.name}</strong><span><code>{project.branch}</code> · {project.path}</span></div></div>
                <span className="agent-count"><span className={project.agents ? "pulse-dot" : "idle-dot"} /> {project.agents || "—"} {project.agents ? "active" : "idle"}</span>
                <div className="progress-cell"><div className="progress-track"><span style={{ width: `${project.progress}%` }} /></div><small>{project.progress}%</small></div>
                <StatusBadge status={project.pipeline} />
                <span className={`git-state ${project.git}`}><span /> {project.git === "clean" ? "Clean" : "3 changes"}</span>
                <button className="row-action" aria-label={`Open ${project.name}`}>→</button>
              </div>
            ))}
          </div>
        </section>

        <div className="lower-grid">
          <section className="section-block">
            <div className="section-header"><div><h2>Attention inbox</h2><span className="section-caption">Items that need your decision</span></div><button className="text-button">Open inbox <span>→</span></button></div>
            <div className="inbox-card">
              <div className="inbox-item"><span className="inbox-icon high">!</span><div><strong>Review requested</strong><span>Session recovery · Agent Control Plane</span></div><time>8m ago</time></div>
              <div className="inbox-item"><span className="inbox-icon question">?</span><div><strong>Agent has a question</strong><span>Which auth provider should be used?</span></div><time>24m ago</time></div>
              <div className="inbox-item"><span className="inbox-icon failure">×</span><div><strong>Pipeline failed</strong><span>ClickShare Desktop · GitHub Actions</span></div><time>1h ago</time></div>
              <div className="inbox-item"><span className="inbox-icon review">✓</span><div><strong>Permission request</strong><span>Run integration tests with network access</span></div><time>2h ago</time></div>
            </div>
          </section>
          <section className="section-block activity-section">
            <div className="section-header"><div><h2>Live activity</h2><span className="section-caption live-caption"><span /> Streaming now</span></div><button className="text-button">View transcript <span>→</span></button></div>
            <div className="activity-card">
              <div className="activity-item"><span className="activity-line purple-line" /><div><strong>Implementing SQLite migrations</strong><span>Agent Control Plane · Run #12</span></div><time>Running · 04:32</time></div>
              <div className="activity-item"><span className="activity-line blue-line" /><div><strong>Running unit tests</strong><span>ClickShare Desktop · Run #8</span></div><time>Running · 01:18</time></div>
              <div className="activity-item"><span className="activity-line green-line" /><div><strong>Task accepted</strong><span>Protocol Gateway · Run #21</span></div><time>12 min ago</time></div>
            </div>
          </section>
        </div>
      </main>

      {showAddProject && (
        <div className="modal-backdrop" role="presentation" onClick={() => setShowAddProject(false)}>
          <div className="modal" role="dialog" aria-modal="true" aria-labelledby="add-project-title" onClick={(event) => event.stopPropagation()}>
            <div className="modal-title"><div><p className="eyebrow">PROJECT SETUP</p><h2 id="add-project-title">Add a local project</h2></div><button className="close-button" onClick={() => setShowAddProject(false)} aria-label="Close">×</button></div>
            <p className="modal-copy">Connect an existing local Git checkout. Remote cloning is not available in the first release.</p>
            <label className="field-label" htmlFor="project-path">Repository path</label>
            <input id="project-path" className="path-input" placeholder="/Users/you/Projects/my-repository" autoFocus />
            <div className="modal-actions"><button className="secondary-button" onClick={() => setShowAddProject(false)}>Cancel</button><button className="primary-button" onClick={() => setShowAddProject(false)}>Validate repository</button></div>
          </div>
        </div>
      )}
    </div>
  );
}

export default App;
