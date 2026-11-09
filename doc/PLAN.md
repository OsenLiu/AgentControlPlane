# Agent Control Plane — Implementation Plan

## 1. Product goal

Build a cross-platform desktop application for macOS, Linux, and Windows that manages Copilot, Gemini, Claude Code CLI agents across local projects. The application should make delegation, supervision, recovery, review, repository inspection, pipeline monitoring, and project analytics visible from one local dashboard.

The app must be local-first: project source code, transcripts, task state, and credentials remain on the user's machine unless a configured provider is contacted.

## 2. UI design — Command Center

The selected dashboard direction is **Command Center** (`dashboard-concept-a-command-center.svg`). The dashboard is the first screen shown when the app opens and is optimized for quickly answering: *What is running? What needs me? Is delivery healthy?*

### Overall layout

- **Desktop shell:** dark navy application chrome with a fixed left navigation rail and a fluid content canvas.
- **Navigation rail:** product mark, Dashboard, Projects, Agent sessions, Attention inbox, Pipeline, and Analytics. The bottom of the rail shows supervisor connectivity and project/session counts.
- **Header:** greeting/context line, current date, and a prominent **Add project** action.
- **Dashboard canvas:** four summary cards, a projects table, an attention inbox, a live activity stream, and a pipeline trend card.
- **Density:** information-rich but scannable; use clear card boundaries, consistent alignment, and short status labels rather than large decorative illustrations.

### Visual system

- **Theme:** dark-first, with `#0B1020` / `#111A2E` background surfaces and `#151F35` cards.
- **Primary accent:** violet-to-blue gradient for selected navigation, primary actions, and links.
- **Status colors:** green for healthy/running, amber for waiting/review, red for failed/blocked, and muted blue-gray for inactive states.
- **Typography:** system sans-serif; bold numeric values for KPIs; uppercase letter-spaced labels for metadata; monospace for branches, paths, commit hashes, and token counts.
- **Interaction states:** every live item must expose hover, focus, selected, loading, stale, and error states. Do not rely on color alone; pair status colors with icons and text.
- **Accessibility:** maintain keyboard navigation, visible focus rings, adequate contrast, reduced-motion support, and semantic labels for status indicators.

### Dashboard components

1. **Summary cards**
   - Active agents and running/paused counts.
   - Attention item count.
   - Ready backlog count across projects.
   - Pipeline health percentage with trend direction.
   - Clicking a card applies the corresponding dashboard filter.
2. **Projects table**
   - Columns: project, branch/path, active agents, task progress, pipeline status, and Git dirty/clean state.
   - Show a compact secondary line for branch and local path.
   - Use a status dot plus text for pipeline and Git states.
   - Clicking a project opens its project workspace; clicking a task or agent opens the relevant detail view.
   - Support sorting and filtering without losing live updates.
3. **Attention inbox card**
   - Show the highest-priority unresolved items directly on the dashboard.
   - Include review requests, failed pipelines, agent questions, permission requests, and exhausted retries.
   - Use severity indicators and relative timestamps.
   - **Open inbox** navigates to the cross-project queue; each row opens an in-place response/review action.
4. **Live activity stream**
   - Stream structured events such as implementation steps, test progress, waiting questions, retries, and accepted runs.
   - Include project, task/session name, current step, and elapsed time.
   - Pause/resume live updates while the user is inspecting older activity.
   - Provide a link to the transcript, diff, or pipeline log associated with each event.
5. **Pipeline trend card**
   - Plot recent pass/fail results for the selected rolling window.
   - Show passing and failed totals below the graph.
   - Clicking the card opens normalized pipeline runs with provider deep links and logs.

### Navigation and responsive behavior

- On wide desktop windows, preserve the two-column lower layout: activity stream plus pipeline trend.
- At narrower widths, stack cards in this order: summary cards, attention inbox, projects, live activity, pipeline trend.
- Keep the navigation rail available on desktop; collapse it to an icon rail when the content canvas requires additional width.
- The dashboard must remain usable at 1280px wide without horizontal scrolling.
- Opening a detail view should preserve the current project/filter context and provide a clear return path to the dashboard.

### Live-state behavior

- Show a subtle connection indicator when the event stream is delayed or disconnected.
- Keep the last known data visible while showing stale timestamps; never replace useful data with blank loading states during refresh.
- Animate only state transitions and progress updates, with a reduced-motion alternative.
- If the app restarts while agents are running, show an explicit reconciliation state until each process is confirmed running, completed, or orphaned.

## 3. Recommended architecture

### Desktop shell

- **Tauri 2** for macOS, Linux, and Windows packaging, native notifications, secure OS integration, and a small install footprint.
- **React + TypeScript** for the UI.
- **Rust application core** for process supervision, filesystem/git operations, provider polling, persistence access, and cross-platform behavior.
- A typed command/event boundary between the React frontend and Rust core. No direct unrestricted shell execution from the webview.

### Local services

1. **Project registry** — tracks projects, repository roots, branch state, settings, and provider configuration.
2. **Task orchestrator** — owns backlog selection, delegation, run loops, budgets, recovery, review gates, and looping mode.
3. **Session manager** — starts and discovers Claude CLI sessions, captures stdout/stderr/transcripts, detects waiting states, and sends replies or permission-mode changes.
4. **Repository service** — reads git status, branches, commits, refs, diffs, and file contents.
5. **Pipeline service** — runs provider adapters with polling, ETags, backoff, normalized statuses, logs, and notification events.
6. **Analytics service** — executes pluggable widgets and stores encrypted provider credentials through the OS credential vault.
7. **Event bus** — emits typed updates so dashboards and inboxes update without polling the UI.

### Persistence

Use SQLite in the platform application-data directory with migrations and WAL mode. Store large or sensitive content separately where appropriate:

- SQLite: projects, tasks, runs, sessions, events, provider state, widget definitions, review decisions.
- Local files: transcript chunks and cached pipeline logs, indexed by database metadata.
- OS credential vault: GitHub/Vercel/API bearer tokens and other secrets; store only opaque credential references in SQLite.

## 4. Core domain model

The initial schema should include:

- `projects`: repository path, display name, category, default branch, looping-mode settings.
- `tasks`: project, title, purpose, acceptance criteria, priority, state, auto-approve flag, retry policy, budgets.
- `task_runs`: task, Claude session, attempt number, lifecycle state, started/completed timestamps, step/token usage, failure reason.
- `run_steps`: run, step name, status, timestamps, output summary, token usage.
- `agent_sessions`: project, external Claude session identifier, process metadata, permission mode, status, transcript location.
- `inbox_items`: project, source type, related entity, question/request payload, status, response, timestamps.
- `reviews`: run, decision (`accepted`, `changes_requested`), feedback, reviewer, timestamp.
- `pipeline_providers`: project, provider type, configuration reference, enabled state.
- `pipeline_runs`: provider, external run/deployment ID, status, URL, logs cache, ETag, timestamps.
- `analytics_widgets`: project, widget type, configuration, layout, refresh settings, credential reference.
- `events`: append-only audit and notification stream with structured payloads.

Use explicit state machines rather than free-form strings. Every terminal state should be recorded with a reason and timestamp.

## 5. Major user flows

### Project setup

1. Add or discover a local git repository.
2. Validate the path and Claude CLI availability.
3. Configure project category, pipeline providers, looping mode, budgets, and analytics widgets.
4. Show branch, dirty state, current commit, provider connectivity, and active sessions.

### Delegation and supervised run loop

1. User creates a backlog task with purpose and acceptance criteria.
2. Orchestrator selects a ready task and launches Claude with a structured task prompt.
3. Session manager streams transcript and status events.
4. Orchestrator enforces step and token budgets.
5. On recoverable failure, apply bounded backoff and retry policy; after exhaustion, create an inbox item.
6. On completion, run repository/test checks and capture the resulting diff.
7. If `autoApprove` is enabled and checks are clean, accept automatically.
8. Otherwise create a review inbox item with summary, checks, diff, and transcript links.
9. User accepts or sends feedback; accepted tasks are completed, while requested changes create the next bounded attempt.

### Looping mode

- Per-project toggle.
- Automatically delegates the next eligible backlog task after an accepted completion.
- Automatically approves delegated-run permission requests and clean auto-approved completions.
- Never auto-answers agent questions or exhausted-failure events; those remain in the attention inbox.
- Enforce project-level concurrency and budget limits to prevent runaway delegation.

### Attention inbox

One cross-project queue containing:

- Agent questions.
- Permission requests.
- Exhausted retries and failures.
- Completed runs awaiting review.
- Provider failures that require action.

Each item must be answerable in place, show enough context to act, support filtering/sorting, and link back to the project, task, session, transcript, diff, or external pipeline run.

### Agent sessions

- Discover app-managed and existing Claude CLI sessions associated with a project.
- Display live transcript, process status, current step, permission mode, and resource usage.
- Allow replies only when the session is in a known waiting state.
- Allow permission mode changes (`plan`, `accept edits`, `auto`) through the supervisor.
- Clearly distinguish sessions managed by the app from sessions observed read-only.

### Diff viewer

- Working-tree view: staged, unstaged, and untracked files.
- Ref-to-ref view for arbitrary local refs/commits.
- Group by directory with file search/filter.
- Unified and side-by-side modes.
- Syntax highlighting with safe handling for binary, large, and generated files.
- Link changed files to the originating task run and review.

### Pipeline monitoring

Define a provider interface such as:

```text
Provider
  discover(project)
  listRuns(project, cursor, conditionalRequest)
  getRun(project, externalId)
  getLogs(project, externalId)
  getDeepLink(run)
```

Ship GitHub Actions and Vercel adapters first. Normalize provider-specific data into `pipeline_runs`; the UI and data model must not branch on provider type. Each adapter owns authentication, ETag handling where supported, polling cadence, rate-limit behavior, and exponential backoff.

Record a rolling failure rate over a configurable window (for example, the last 20 completed runs or 7 days). Send native desktop notifications for failures with click-through to the in-app run and provider URL.

### Analytics

Implement a widget registry so new metrics require an adapter/widget registration rather than schema or dashboard rewrites. Initial widgets:

- GitHub releases with per-asset download counts.
- Repository traffic: views and clones.
- Repository statistics.
- Generic JSON metric endpoint with configurable URL, method, headers, JSON path, dimensions, and time series mapping.

Refresh widgets independently, cache successful results, show stale/error states, and never expose bearer tokens to the frontend logs or transcript.

## 6. Security and reliability requirements

- Treat all Claude output, repository files, pipeline logs, and external JSON as untrusted content.
- Restrict subprocess execution to approved binaries and project working directories.
- Use argument arrays, not shell interpolation, for Claude, git, and test commands.
- Require explicit project authorization before managing a repository or session.
- Store tokens only in Keychain (macOS), Secret Service/libsecret (Linux), or Credential Manager (Windows), with an encrypted-file fallback only if explicitly enabled.
- Redact credentials and common secret patterns from logs, transcripts, notifications, and crash reports.
- Apply filesystem size/time limits to transcripts, diffs, logs, and generic metrics.
- Persist every state transition and recovery decision for auditability.
- Handle app restart by reconciling persisted runs with live processes and marking orphaned runs for attention.
- Make retry, token, step, concurrency, polling, and log-retention limits configurable with safe defaults.

## 7. Delivery phases

### Phase 0 — Foundation

- Create Tauri/React/Rust workspace and CI for all three operating systems.
- Add SQLite migrations, typed commands/events, structured logging, and error reporting.
- Implement project registration and git status.
- Verify Claude CLI discovery and version compatibility.

**Exit criteria:** A user can add a local repository, see branch/dirty state, and restart the app without losing project data.

### Phase 1 — Tasks and supervised runs

- Implement backlog CRUD and task state machine.
- Launch and supervise Claude sessions.
- Stream transcripts and structured status.
- Add step/token budgets, bounded retries, failure inbox items, and review decisions.
- Add task-level auto-approve.

**Exit criteria:** A task can move from backlog to delegated run to accepted or changes-requested, with persisted evidence and restart recovery.

### Phase 2 — Dashboard, inbox, looping, and sessions

- Build cross-project dashboard and attention inbox.
- Add project looping mode with safety limits.
- Add live session discovery, replies, and permission-mode controls.
- Add native notifications and deep links.

**Exit criteria:** A user can manage multiple projects from one queue and run a project backlog hands-free while questions and failures still interrupt them.

### Phase 3 — Diff and pipeline monitoring

- Implement diff viewer and run/review integration.
- Add provider interface, GitHub Actions adapter, and Vercel adapter.
- Add conditional requests, per-provider backoff, logs, rolling failure rate, and failure notifications.

**Exit criteria:** Provider-specific additions do not require changes to the task, project, or dashboard data model.

### Phase 4 — Analytics and extensibility

- Add widget registry and customizable layouts.
- Implement GitHub release, traffic, and repository-stat widgets.
- Implement generic JSON metric widget.
- Add credential-vault integration, refresh scheduling, caching, and stale/error UX.

**Exit criteria:** A user can add a third-party JSON metric endpoint without application code changes, and credentials never appear in frontend-visible state.

### Phase 5 — Hardening and release

- Cross-platform packaging, signing, auto-update strategy, and migration testing.
- Crash recovery and orphan-process reconciliation.
- Accessibility, keyboard navigation, large-repository performance, offline behavior, and security testing.
- End-to-end test matrix on macOS, Linux, and Windows.

**Exit criteria:** Release candidates pass the acceptance suite on all supported platforms and can safely recover from app, process, network, and provider failures.

## 8. Testing strategy

- **Unit:** state machines, retry/backoff, budget accounting, rolling failure-rate calculation, provider normalization, JSON metric extraction, redaction.
- **Integration:** Claude process lifecycle, SQLite migrations, git status/diff, credential-vault adapters, provider HTTP conditional requests.
- **End-to-end:** create project → delegate task → interact with session → review diff → accept/reject → loop to next task.
- **Failure tests:** CLI missing, process crash, malformed output, permission denial, token exhaustion, network timeout, rate limit, stale ETag, corrupted cache, app restart during a run.
- **Cross-platform:** path handling, process signals, notifications, vault integration, packaging, and shell/terminal behavior.

## 9. Initial acceptance checklist

- [ ] Projects show branch, dirty state, active tasks, sessions, pipeline status, and rolling failure rate.
- [ ] Every delegated task has purpose, acceptance criteria, budgets, retry policy, and an auditable lifecycle.
- [ ] Agent questions, permission requests, failures, and reviews appear in one actionable inbox.
- [ ] Looping mode delegates the next task but never silently answers questions or exhausted failures.
- [ ] Session transcripts update live and permission modes are visible and controllable.
- [ ] Diffs support working tree, ref-to-ref, grouped, syntax-highlighted, unified, and side-by-side views.
- [ ] GitHub Actions and Vercel use the same normalized pipeline UI and support logs/deep links.
- [ ] Analytics widgets are pluggable; built-in widgets and generic JSON metrics work per project.
- [ ] Secrets use the OS vault and are absent from UI events, logs, and persisted application data.
- [ ] App restart, process failure, network failure, and provider rate limits produce recoverable, visible states.
- [ ] macOS, Linux, and Windows builds pass the release test matrix.

## 10. Confirmed decisions before implementation

The following decisions were confirmed during design review:

1. Require a documented minimum Claude CLI version and reject unsupported versions. App-managed sessions may use a separate Claude profile.
2. Limit the first release to existing local repository checkouts. Remote cloning, collaboration, multi-user accounts, remote control, remote execution, and custom code plugins are out of scope.
3. Define a Project as one authorized checkout. Separate Git worktrees are separate Projects until a workspace-group concept is introduced.
4. Default looping mode to one active run per Project, finite token/step budgets, bounded retries, and a daily project budget. Keep looping mode and automatic acceptance disabled by default.
5. Support global provider credentials with optional per-Project overrides. Persist only opaque credential references in the platform vault.
6. Defer exact OS minimums, signing, package formats, and auto-update policy to release engineering, while requiring macOS, Linux, and Windows acceptance testing.
7. Keep Task, Task Run, Agent Session, Inbox Item, Review, Pipeline Run, and Analytics Widget as distinct domain concepts with explicit state machines.
8. Treat accepted work as an auditable decision supported by a Git baseline, diff, checks, and review evidence. Requested changes create a new Task Run.
9. Allow automation to handle only explicitly authorized permission requests. Agent questions, ambiguous requests, and exhausted failures always require the user.
10. Use normalized provider-independent pipeline data, adapter-owned integration behavior, declarative analytics widgets, and constrained user-configured JSON endpoints.
11. Use Tauri 2, React/TypeScript, Rust, and SQLite with typed command/event boundaries. The frontend cannot execute unrestricted shell commands.
12. Preserve local evidence and show explicit, retryable failures for unavailable CLIs, repositories, providers, networks, or credential vaults.
13. Guarantee ordered, at-least-once event delivery per entity, idempotent consumers, transactional persistence before event publication, and restart reconciliation.
14. Enforce append-only domain history, configurable retention, opt-in diagnostics, unified redaction, bounded caches, explicit destructive-action confirmation, and application-wide as well as Project-level concurrency limits.
15. Implement in the documented phase order and require each phase's exit criteria before advancing.

