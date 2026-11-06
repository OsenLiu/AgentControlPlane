import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
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

  it("sends a reply for a waiting managed session", async () => {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Agent sessions" }));
    fireEvent.click(screen.getByRole("button", { name: /add empty states to component gallery/i }));

    const reply = screen.getByRole("textbox", { name: "Reply to agent" });
    fireEvent.change(reply, { target: { value: "Use skeleton rows." } });
    fireEvent.click(screen.getByRole("button", { name: /send reply/i }));

    expect(await screen.findByText("You replied: Use skeleton rows.")).toBeTruthy();
    expect(screen.getByText("Processing your reply")).toBeTruthy();
  });
});
