# Local desktop core and typed command boundary

The application will use Tauri 2 with a React/TypeScript frontend, a Rust application core, and SQLite for local persistence. Process supervision, filesystem and Git access, provider integration, and credential references remain in Rust; the frontend communicates only through typed commands and events, never unrestricted shell execution. This preserves the local-first security boundary while supporting cross-platform desktop behavior and a small install footprint.

## Consequences

- The Rust core owns process lifecycle, persistence transactions, reconciliation, and provider adapters.
- Frontend features require explicit command/event types rather than direct host access.
- Cross-platform process, filesystem, credential-vault, and notification behavior must be adapted in Rust.
