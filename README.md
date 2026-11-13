# Agent Control Plane

Local-first desktop supervision for AI CLI agents across projects.

The current foundation is a Tauri 2 desktop shell with a React/TypeScript
Command Center dashboard. The Rust core will own process supervision,
filesystem/Git operations, persistence, provider adapters, and typed
frontend commands/events.

Agent Control Plane stores its session list and chat transcripts in its own
SQLite database. Copilot CLI interaction uses its supported Agent Client
Protocol. Claude Code runs through its local headless CLI with structured
stream output; the app uses Claude's CLI session IDs to resume conversations.
Enable the desired providers in Settings and choose an enabled CLI when
starting a session. The app does not open either CLI's private session stores.

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

## Releases

Pushing a Git tag matching `v*` builds the desktop app for Linux, macOS, and
Windows and publishes the installers as a GitHub Release. Use a semantic
version tag such as `v1.2.3`; the tag version is applied to the app and its
package metadata before building.
