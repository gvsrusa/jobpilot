import { useQuery } from "@tanstack/react-query";
import { PausePanel } from "@/components/PausePanel";
import { ipc, type ApplyRunView } from "@/lib/ipc";

/** One query for the app: the live Copilot run, polled whatever view is open. */
export const COPILOT_LIVE = ["apply", "copilot", "live"] as const;

/** The newest run, when it is a Copilot run that has not ended. */
export function liveCopilot(run: ApplyRunView | null | undefined): ApplyRunView | null {
  return run && run.mode === "copilot" && run.state !== "ended" ? run : null;
}

export function useLiveCopilot() {
  return useQuery({ queryKey: COPILOT_LIVE, queryFn: () => ipc.applyLatest(), refetchInterval: 1000 });
}

/**
 * A live Copilot run's stop, at the top of the app, whatever view is open: closing the review
 * screen neither hides nor blocks it. The page's words show as plain text, and only the
 * panel's buttons end it.
 */
export function CopilotStop() {
  const run = liveCopilot(useLiveCopilot().data);
  if (!run?.active_pause) return null;
  return (
    <div className="mb-4">
      <PausePanel runId={run.id} pause={run.active_pause} />
    </div>
  );
}
