import { useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export function OpenTaskInVSCodeButton({
  projectId,
  taskId,
  worktreePath,
}: {
  projectId: string;
  taskId: string;
  worktreePath: string | null | undefined;
}) {
  const pending = useRef(false);
  const [opening, setOpening] = useState(false);
  const [error, setError] = useState("");

  if (!worktreePath?.trim()) return null;

  async function openWorkspace() {
    if (pending.current) return;
    pending.current = true;
    setOpening(true);
    setError("");
    try {
      await invoke("open_task_in_vscode", { projectId, taskId });
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      pending.current = false;
      setOpening(false);
    }
  }

  return (
    <div>
      <button
        className="project-feature-button"
        type="button"
        disabled={opening}
        onClick={() => void openWorkspace()}
      >
        {opening ? "Opening in VS Code..." : "Open in VS Code"}
      </button>
      {error && <p className="project-feature-error" role="alert">{error}</p>}
    </div>
  );
}
