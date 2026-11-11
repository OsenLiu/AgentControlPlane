import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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

vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: vi.fn(),
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
    if (command === "list_project_tasks") return [];
    if (command === "get_project_diff") return { branch: "main", changedFiles: [], diff: "", truncated: false };
    if (command === "list_project_terminals") return [];
    if (command === "get_project_pipelines") return {
      repository: null,
      configured: false,
      error: "GitHub Actions are available for projects with a GitHub origin remote.",
      runs: [],
    };
    if (command === "get_project_dependencies") return { manifests: [], dependencies: [], errors: [] };
    if (command === "list_project_skills") return { global: [], project: [], errors: [] };
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

  it("opens the project directory and filters repositories by search and Git state", async () => {
    invokeMock.mockImplementation(async (command) => {
      if (command === "list_projects") return [
        {
          id: "/work/agent-control-plane",
          name: "agent-control-plane",
          path: "/work/agent-control-plane",
          branch: "main",
          currentCommit: "a1b2c3d4",
          gitState: "clean",
        },
        {
          id: "/work/website",
          name: "website",
          path: "/work/website",
          branch: "feature/refresh",
          currentCommit: "e5f6a7b8",
          gitState: "dirty",
        },
      ];
      if (command === "list_agent_sessions") return [];
      if (command === "get_pending_session_permission") return null;
      if (command === "load_settings") return settings;
      if (command === "list_project_tasks") return [];
      if (command === "get_project_diff") return { branch: "feature/refresh", changedFiles: [], diff: "", truncated: false };
      if (command === "list_project_terminals") return [];
      if (command === "get_project_pipelines") return {
        repository: null,
        configured: false,
        error: "GitHub Actions are available for projects with a GitHub origin remote.",
        runs: [],
      };
      if (command === "get_project_dependencies") return { manifests: [], dependencies: [], errors: [] };
      if (command === "list_project_skills") return { global: [], project: [], errors: [] };
      throw new Error(`Unexpected command: ${command}`);
    });

    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /View all projects/ }));

    expect(await screen.findByRole("heading", { name: "Your projects" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: /Repositories 2/ })).toBeTruthy();
    expect(screen.getByText("/work/agent-control-plane")).toBeTruthy();
    expect(screen.getByText("a1b2c3d")).toBeTruthy();
    expect(screen.getByText("Uncommitted changes")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /Changes/ }));
    expect(screen.queryByRole("heading", { name: "agent-control-plane" })).toBeNull();
    expect(screen.getByRole("heading", { name: "website" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /All projects/ }));
    fireEvent.change(screen.getByRole("searchbox", { name: "Search projects" }), {
      target: { value: "feature/refresh" },
    });
    expect(screen.getByRole("heading", { name: "website" })).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "agent-control-plane" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Open workspace" }));
    expect(await screen.findByRole("heading", { name: "website" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "website", current: "page" })).toBeTruthy();
    expect(await screen.findByRole("heading", { name: "No tasks yet" })).toBeTruthy();
    expect(invokeMock).toHaveBeenCalledWith("list_project_tasks", { projectId: "/work/website" });
    for (const section of ["Tasks", "Diffs", "Sessions", "Terminals", "Pipelines", "Dependencies", "Skills"]) {
      expect(screen.getByRole("button", { name: section })).toBeTruthy();
    }

    fireEvent.click(screen.getByRole("button", { name: "Sessions" }));
    expect(await screen.findByRole("heading", { name: "No agent sessions yet" })).toBeTruthy();
    expect(invokeMock).toHaveBeenCalledWith("list_agent_sessions", {
      archived: false,
      projectId: "/work/website",
    });

    fireEvent.click(screen.getByRole("button", { name: "Pipelines" }));
    expect(await screen.findByRole("heading", { name: "No GitHub repository connected" })).toBeTruthy();
    expect(invokeMock).toHaveBeenCalledWith("get_project_pipelines", { projectId: "/work/website" });

    fireEvent.click(screen.getByRole("button", { name: "Diffs" }));
    expect(await screen.findByRole("heading", { name: "Working tree is clean" })).toBeTruthy();
    expect(invokeMock).toHaveBeenCalledWith("get_project_diff", { projectId: "/work/website" });

    fireEvent.click(screen.getByRole("button", { name: "Terminals" }));
    expect(await screen.findByRole("heading", { name: "No terminal sessions" })).toBeTruthy();
    expect(invokeMock).toHaveBeenCalledWith("list_project_terminals", { projectId: "/work/website" });

    fireEvent.click(screen.getByRole("button", { name: "Dependencies" }));
    expect(await screen.findByRole("heading", { name: "No supported manifests found" })).toBeTruthy();
    expect(invokeMock).toHaveBeenCalledWith("get_project_dependencies", { projectId: "/work/website" });

    fireEvent.click(screen.getByRole("button", { name: "Skills" }));
    expect(await screen.findByRole("heading", { name: "No skills discovered" })).toBeTruthy();
    expect(invokeMock).toHaveBeenCalledWith("list_project_skills", { projectId: "/work/website" });
  });

  it("creates a project task and links it to a new Copilot session", async () => {
    const project = {
      id: "/work/website",
      name: "website",
      path: "/work/website",
      branch: "main",
      currentCommit: "a1b2c3d4",
      gitState: "clean" as const,
    };
    let task: {
      id: string;
      projectId: string;
      title: string;
      description: string;
      status: "backlog" | "in_progress" | "done";
      sessionId: string | null;
      createdAt: string;
      updatedAt: string;
    } | null = null;

    invokeMock.mockImplementation(async (command, args) => {
      const payload = args as Record<string, string>;
      if (command === "list_projects") return [project];
      if (command === "list_agent_sessions") return [];
      if (command === "get_pending_session_permission") return null;
      if (command === "load_settings") return settings;
      if (command === "list_project_tasks") return task ? [task] : [];
      if (command === "create_project_task") {
        task = {
          id: "task-1",
          projectId: payload.projectId,
          title: payload.title,
          description: payload.description,
          status: "backlog",
          sessionId: null,
          createdAt: "2026-01-01",
          updatedAt: "2026-01-01",
        };
        return task;
      }
      if (command === "start_copilot_session") return { id: "session-1" };
      if (command === "link_project_task_session") {
        if (task) task = { ...task, status: "in_progress", sessionId: payload.sessionId };
        return undefined;
      }
      throw new Error(`Unexpected command: ${command}`);
    });

    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /View all projects/ }));
    fireEvent.click(await screen.findByRole("button", { name: "Open workspace" }));

    fireEvent.change(await screen.findByLabelText("Task title"), { target: { value: "Ship onboarding flow" } });
    fireEvent.change(screen.getByLabelText("Brief"), { target: { value: "Add the welcome screens." } });
    fireEvent.click(screen.getByRole("button", { name: "Add task" }));

    expect(await screen.findByRole("heading", { name: "Ship onboarding flow" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Start session" }));
    expect(await screen.findByText("Linked session · session-")).toBeTruthy();
    expect(invokeMock).toHaveBeenCalledWith("start_copilot_session", {
      projectId: "/work/website",
      task: "Ship onboarding flow\n\nAdd the welcome screens.",
    });
    expect(invokeMock).toHaveBeenCalledWith("link_project_task_session", {
      projectId: "/work/website",
      taskId: "task-1",
      sessionId: "session-1",
    });
  });

  it("loads an empty Attention inbox from the backend", async () => {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: /^Attention inbox/ }));

    expect(await screen.findByText("You’re all caught up")).toBeTruthy();
    expect(invokeMock).toHaveBeenCalledWith("list_agent_sessions", { archived: false });
  });

  it("shows pending permissions and failed sessions but excludes completed and orphaned sessions", async () => {
    const sessions = [
      {
        id: "waiting-session",
        project: "demo",
        task: "Run the checks",
        branch: "feat/checks",
        provider: "copilot",
        status: "waiting",
        step: "Waiting for permission",
        elapsed: "2026-11-09 11:20:00",
        tokens: "0.0k",
        permissionMode: "Plan",
        managed: true,
        lastEvent: "Run tests",
        archived: false,
        statusReason: "",
        updatedAt: "2026-11-10 10:00:00",
      },
      {
        id: "failed-session",
        project: "demo",
        task: "Build the app",
        branch: "fix/build",
        provider: "claudeCode",
        status: "failed",
        step: "Build failed",
        elapsed: "2026-11-09 11:20:00",
        tokens: "1.0k",
        permissionMode: "Plan",
        managed: true,
        lastEvent: "Compiler exited with status 1",
        archived: false,
        statusReason: "Build command failed",
        updatedAt: "2026-11-10 09:00:00",
      },
      {
        id: "completed-session",
        project: "demo",
        task: "Update the docs",
        branch: "docs/setup",
        provider: "copilot",
        status: "completed",
        step: "Finished",
        elapsed: "2026-11-09 11:20:00",
        tokens: "1.2k",
        permissionMode: "Plan",
        managed: true,
        lastEvent: "Prompt stopped: end_turn",
        archived: false,
        statusReason: "",
        updatedAt: "2026-11-10 08:00:00",
      },
      {
        id: "orphaned-session",
        project: "demo",
        task: "Resume the work",
        branch: "feat/recovery",
        provider: "copilot",
        status: "orphaned",
        step: "Reconnect required",
        elapsed: "2026-11-09 11:20:00",
        tokens: "0.5k",
        permissionMode: "Plan",
        managed: true,
        lastEvent: "",
        archived: false,
        statusReason: "Agent Control Plane restarted; reconnect the session to continue.",
        updatedAt: "2026-11-10 07:00:00",
      },
    ];
    let permissionPending = true;
    invokeMock.mockImplementation(async (command, args) => {
      if (command === "list_projects") return [];
      if (command === "load_settings") return settings;
      if (command === "get_pending_session_permission") {
        return permissionPending ? {
          requestId: "permission-1",
          title: "Run npm test",
          options: [{ optionId: "allow-once", name: "Allow once", kind: "allow_once" }],
        } : null;
      }
      if (command === "list_agent_sessions") {
        if (!permissionPending) sessions[0].status = "running";
        return sessions;
      }
      if (command === "respond_session_permission") {
        expect(args).toEqual({ sessionId: "waiting-session", requestId: "permission-1", optionId: "allow-once" });
        permissionPending = false;
        sessions[0].status = "running";
        return undefined;
      }
      if (command === "get_session_transcript") return [];
      throw new Error(`Unexpected command: ${command}`);
    });

    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: /^Attention inbox/ }));

    expect(await screen.findAllByText("Run npm test")).toHaveLength(2);
    expect(screen.getByText("Session failed")).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Items to review 2" })).toBeTruthy();
    expect(screen.queryByText("Review completed session")).toBeNull();
    expect(screen.queryByText("Session needs reconnect")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Allow once" }));

    await waitFor(() => {
      expect(screen.queryByText("Run npm test")).toBeNull();
      expect(screen.getByRole("heading", { name: "Items to review 1" })).toBeTruthy();
    });

    fireEvent.click(screen.getByRole("button", { name: /Session failed/ }));
    fireEvent.click(screen.getByRole("button", { name: "Open session" }));
    expect(await screen.findByRole("heading", { name: "Agent sessions" })).toBeTruthy();
    expect(await screen.findAllByText("Build the app")).toHaveLength(2);
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
    fireEvent.click(within(screen.getByRole("main")).getByRole("button", { name: /add project/i }));
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
      externalSessionId: "sess_actual_42",
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
    expect(screen.getByText("sess_actual_42")).toBeTruthy();
    expect(screen.queryByText("sess_8f31c2")).toBeNull();
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
    let resumed = false;
    invokeMock.mockImplementation(async (command) => {
      if (command === "list_projects") return [];
      if (command === "load_settings") return settings;
      if (command === "list_agent_sessions") return [{
        ...session,
        status: resumed ? "idle" : "completed",
        step: resumed ? "Ready for your message" : "Finished",
      }];
      if (command === "get_pending_session_permission") return null;
      if (command === "get_session_transcript") return [];
      if (command === "start_copilot_session") {
        resumed = true;
        return { ...session, status: "idle" };
      }
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
    expect(await screen.findByRole("button", { name: "Stop session" })).toBeEnabled();
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

  it("allows replying to a managed session waiting for user input", async () => {
    const session = {
      id: "waiting-session",
      project: "demo",
      task: "Choose an implementation",
      branch: "main",
      provider: "copilot",
      status: "waiting",
      step: "Waiting for your input",
      elapsed: "2026-11-09 11:20:00",
      tokens: "0.0k",
      permissionMode: "plan",
      managed: true,
      lastEvent: "Which approach should I use?",
      archived: false,
    };
    invokeMock.mockImplementation(async (command) => {
      if (command === "list_projects") return [];
      if (command === "load_settings") return settings;
      if (command === "list_agent_sessions") return [session];
      if (command === "get_pending_session_permission") return null;
      if (command === "get_session_transcript") return [];
      if (command === "send_session_message") return undefined;
      throw new Error(`Unexpected command: ${command}`);
    });

    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Agent sessions" }));
    const input = await screen.findByRole("textbox", { name: "Message Copilot" });

    expect(input).toBeEnabled();
    fireEvent.change(input, { target: { value: "Use the simpler approach." } });
    fireEvent.click(screen.getByRole("button", { name: /Send/ }));

    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("send_session_message", {
      sessionId: "waiting-session",
      message: "Use the simpler approach.",
    }));
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
