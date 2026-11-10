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
    if (command === "get_pending_session_permission") return null;
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
    expect(invokeMock).toHaveBeenCalledWith("list_agent_sessions", { archived: false });
  });

  it("renders a read-only session returned by the local backend", async () => {
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
      lastEvent: "Locally persisted read-only session",
    };
    invokeMock.mockImplementation(async (command) => {
      if (command === "list_projects") return [];
      if (command === "list_agent_sessions") return [session];
      if (command === "get_pending_session_permission") return null;
      if (command === "load_settings") return settings;
      throw new Error(`Unexpected command: ${command}`);
    });

    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Agent sessions" }));

    expect((await screen.findAllByText("Observed Copilot CLI session")).length).toBe(2);
    expect(screen.getByText("Read only")).toBeTruthy();
    expect(screen.getByText("Locally persisted read-only session")).toBeTruthy();
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
      if (command === "get_pending_session_permission") return null;
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

  it("resumes a Copilot session from its existing session id", async () => {
    const session = {
      id: "session-to-resume",
      project: "demo",
      task: "Resume this session",
      branch: "main",
      provider: "copilot",
      status: "completed",
      step: "Finished",
      elapsed: "2026-11-09 11:20:00",
      tokens: "1.0k",
      permissionMode: "unknown",
      managed: true,
      lastEvent: "Available to resume in Copilot CLI",
    };
    invokeMock.mockImplementation(async (command) => {
      if (command === "list_projects") return [];
      if (command === "load_settings") return settings;
      if (command === "list_agent_sessions") return [session];
      if (command === "get_pending_session_permission") return null;
      if (command === "get_session_transcript") return [];
      if (command === "start_copilot_session") return session;
      throw new Error(`Unexpected command: ${command}`);
    });

    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Agent sessions" }));
    expect(await screen.findByRole("button", { name: "Resume session" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Resume session" }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("start_copilot_session", {
      projectId: null,
      task: "",
      resumeSessionId: "session-to-resume",
    }));
  });

  it("sends a chat message to a managed Copilot session", async () => {
    const session = {
      id: "managed-session",
      project: "demo",
      task: "Discuss the implementation",
      branch: "main",
      provider: "copilot",
      status: "idle",
      step: "Ready for your message",
      elapsed: "2026-11-09 11:20:00",
      tokens: "0.0k",
      permissionMode: "plan",
      managed: true,
      lastEvent: "",
      archived: false,
    };
    const transcript = [
      { role: "user", content: "What changed?", timestamp: "2026-11-09 11:21:00" },
      { role: "assistant", content: "The session store is local.", timestamp: "2026-11-09 11:21:01" },
    ];
    invokeMock.mockImplementation(async (command) => {
      if (command === "list_projects") return [];
      if (command === "load_settings") return settings;
      if (command === "list_agent_sessions") return [session];
      if (command === "get_pending_session_permission") return null;
      if (command === "get_session_transcript") return transcript;
      if (command === "send_session_message") return undefined;
      throw new Error(`Unexpected command: ${command}`);
    });

    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Agent sessions" }));
    const input = await screen.findByRole("textbox", { name: "Message Copilot" });
    fireEvent.change(input, { target: { value: "What changed?" } });
    fireEvent.click(screen.getByRole("button", { name: /Send/ }));

    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("send_session_message", {
      sessionId: "managed-session",
      message: "What changed?",
    }));
    expect(await screen.findByText("The session store is local.")).toBeTruthy();
  });

  it("stops a managed Copilot session", async () => {
    const session = {
      id: "managed-session",
      project: "demo",
      task: "Stop this session",
      branch: "main",
      provider: "copilot",
      status: "running",
      step: "Working",
      elapsed: "2026-11-09 11:20:00",
      tokens: "0.0k",
      permissionMode: "plan",
      managed: true,
      lastEvent: "Working",
      archived: false,
    };
    invokeMock.mockImplementation(async (command) => {
      if (command === "list_projects") return [];
      if (command === "load_settings") return settings;
      if (command === "list_agent_sessions") return [session];
      if (command === "get_pending_session_permission") return null;
      if (command === "get_session_transcript") return [];
      if (command === "stop_agent_session") return undefined;
      throw new Error(`Unexpected command: ${command}`);
    });

    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Agent sessions" }));
    expect(await screen.findByRole("button", { name: "Stop session" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Stop session" }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("stop_agent_session", {
        sessionId: "managed-session",
      });
    });
  });
});
