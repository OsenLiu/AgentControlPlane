# Agent Control Plane

Shared language for the local application that supervises Claude work across repositories.

## Core work

**Project**:
An authorized local repository checkout managed by the application. A Project is the boundary for tasks, sessions, budgets, and configuration.

**Task**:
A durable user intent describing work to be completed for a Project, including its purpose, acceptance criteria, priority, and execution policy.

**Task Run**:
One bounded execution attempt for a Task. Retries and requested changes create new Task Runs so the attempt history remains distinct.

**Agent Session**:
A Claude CLI process and conversation associated with a Project. A session may be discovered independently and may outlive the Task Run it was associated with.

**Review**:
A user or automated decision about the result of a Task Run, including acceptance or a request for changes.

## Supervision

**Inbox Item**:
One actionable request or unresolved condition requiring attention, such as a question, permission request, failure, review, or provider problem.

**Looping Mode**:
A Project setting that automatically delegates the next eligible Task after an accepted completion, within configured safety limits.

**Permission Mode**:
The configured level of authority granted to an Agent Session for actions that require supervisor approval.

**Working Tree**:
The current filesystem state of a Project checkout, including tracked, staged, unstaged, and untracked changes.

**Reconciliation**:
The process of comparing persisted application state with the actual state of processes, repositories, and external services after observation was interrupted.

**Delivery Gate**:
A configured condition that must be satisfied before a Task Run may be automatically accepted, such as a local check or required pipeline result.

**Credential Reference**:
An opaque identifier for a secret stored in the operating system's credential vault; it is not the secret itself.

**Observed Session**:
An existing Claude CLI session discovered by the application but not started or controlled by it.

**Audit Event**:
A durable record of a significant state transition, recovery decision, or user action relevant to accountability.

**Event Bus**:
The local channel through which committed domain changes are delivered to application consumers.

**Provider Adapter**:
The boundary component that translates one external service's API, authentication, polling, and failure behavior into the application's normalized model.

**Git Baseline**:
The recorded repository state against which a Task Run's changes and checks are evaluated.

**Redaction Boundary**:
The point before content enters logs, events, notifications, reports, or caches where credentials and common secret patterns are removed.

**Recovery Action**:
A user-visible or automatic step that moves an unavailable, failed, or interrupted operation toward a known outcome.

## External services

**Pipeline Run**:
One execution or deployment reported by an external pipeline provider, represented through the application's provider-independent status model.

**Analytics Widget**:
A project-scoped, declarative metric view that retrieves and presents external data without executing arbitrary code.
