import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => vi.fn()),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
}));

const invokeMock = vi.mocked(invoke);

const settings = {
  copilot: { enabled: false },
  gemini: { enabled: false },
  claudeCode: { enabled: true },
  githubUsername: "",
  githubTokenConfigured: false,
  defaultProvider: "claudeCode" as const,
};

beforeEach(() => {
  invokeMock.mockImplementation(async (command) => {
    if (command === "list_projects") return [];
    if (command === "list_agent_sessions") return [];
    if (command === "load_settings") return settings;
    throw new Error(`Unexpected command: ${command}`);
  });
});

describe("App", () => {
  it("loads the empty dashboard state", async () => {
    render(<App />);

    expect(await screen.findByText("No projects connected yet. Add a local Git repository to get started.")).toBeTruthy();
    expect(invokeMock).toHaveBeenCalledWith("list_projects");
    expect(invokeMock).toHaveBeenCalledWith("load_settings");
  });

  it("registers a local project from the add-project dialog", async () => {
    const project = {
      id: "/tmp/demo",
      name: "demo",
      path: "/tmp/demo",
      branch: "main",
      currentCommit: "abc123",
      gitState: "clean" as const,
    };
    invokeMock.mockImplementation(async (command) => {
      if (command === "list_projects") return [];
      if (command === "load_settings") return settings;
      if (command === "register_project") return project;
      throw new Error(`Unexpected command: ${command}`);
    });

    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: /add project/i }));
    fireEvent.change(screen.getByLabelText("Repository path"), {
      target: { value: "/tmp/demo" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Validate repository" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("register_project", { path: "/tmp/demo" });
    });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("loads the empty agent sessions state from the backend", async () => {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Agent sessions" }));

    expect(await screen.findByText("No agent sessions yet")).toBeTruthy();
    expect(invokeMock).toHaveBeenCalledWith("list_agent_sessions");
  });

  it("renders a discovered Copilot session returned by the backend", async () => {
    const session = {
      id: "observed-copilot-abc",
      project: "demo",
      task: "Observed Copilot CLI session",
      branch: "main",
      provider: "copilot",
      status: "running",
      step: "Running outside Agent Control Plane",
      elapsed: "2026-11-09 11:20:00",
      tokens: "0.0k",
      permissionMode: "unknown",
      managed: false,
      lastEvent: "Discovered running Copilot process",
    };
    invokeMock.mockImplementation(async (command) => {
      if (command === "list_projects") return [];
      if (command === "list_agent_sessions") return [session];
      if (command === "load_settings") return settings;
      throw new Error(`Unexpected command: ${command}`);
    });

    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Agent sessions" }));

    expect((await screen.findAllByText("Observed Copilot CLI session")).length).toBe(2);
    expect(screen.getByText("Read only")).toBeTruthy();
    expect(screen.getByText("Discovered running Copilot process")).toBeTruthy();
  });

  it("archives a session and shows it in the archived session list", async () => {
    const session = {
      id: "session-to-archive",
      project: "demo",
      task: "Archive this session",
      branch: "main",
      provider: "copilot",
      status: "completed",
      step: "Finished",
      elapsed: "2026-11-09 11:20:00",
      tokens: "1.0k",
      permissionMode: "unknown",
      managed: true,
      lastEvent: "Completed",
    };
    let archived = false;
    invokeMock.mockImplementation(async (command, args) => {
      const input = (args ?? {}) as { archived?: boolean };
      if (command === "list_projects") return [];
      if (command === "load_settings") return settings;
      if (command === "list_agent_sessions") {
        return input.archived ? (archived ? [session] : []) : (archived ? [] : [session]);
      }
      if (command === "get_session_transcript") return [];
      if (command === "set_session_archived") {
        archived = Boolean(input.archived);
        return undefined;
      }
      throw new Error(`Unexpected command: ${command}`);
    });

    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Agent sessions" }));
    expect((await screen.findAllByText("Archive this session")).length).toBe(2);

    fireEvent.click(screen.getByRole("button", { name: "Archive session" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("set_session_archived", {
      sessionId: "session-to-archive",
      archived: true,
    }));

    await screen.findByText("No agent sessions yet");
    fireEvent.click(screen.getByRole("button", { name: "Archived sessions" }));
    expect((await screen.findAllByText("Archive this session")).length).toBe(2);
    expect(invokeMock).toHaveBeenCalledWith("list_agent_sessions", { archived: true });

    fireEvent.click(screen.getByRole("button", { name: "Unarchive session" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("set_session_archived", {
      sessionId: "session-to-archive",
      archived: false,
    }));
  });
});
