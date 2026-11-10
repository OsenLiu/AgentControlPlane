# Agent Control Plane

Local-first desktop supervision for AI CLI agents across projects.

The current foundation is a Tauri 2 desktop shell with a React/TypeScript
Command Center dashboard. The Rust core will own process supervision,
filesystem/Git operations, persistence, provider adapters, and typed
frontend commands/events.

Agent Control Plane stores its session list and chat transcripts in its own
SQLite database. Copilot CLI interaction uses its supported Agent Client
Protocol; the app does not open Copilot CLI or VS Code private session stores.

## Development

```bash
npm install
npm run dev
```

To run the desktop shell:

```bash
npm run tauri dev
```

The desktop build requires the Rust toolchain and the platform prerequisites
listed in the [Tauri guide](https://tauri.app/start/prerequisites/). On Debian
or Ubuntu, install the WebKit, GTK, DBus, OpenSSL, X11, AppIndicator, and
librsvg development packages before running `npm run tauri build`.

## Validation

```bash
npm run build
npm run tauri build
```

The web build is currently independent of native system libraries; the Tauri
build validates the complete desktop bundle.
