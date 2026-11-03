import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ErrorAlert } from "@/components/ErrorAlert";
import { PausePanel } from "@/components/PausePanel";
import { Button } from "@/components/ui/button";
import { ipc } from "@/lib/ipc";

const ENDED: Record<string, string> = {
  submitted: "You said you submitted it. It is recorded as applied, with the page you ended on.",
  manual_required: "Left to you. Record the outcome when you have applied.",
  skipped: "Stopped before the review. Nothing was submitted.",
};

/**
 * Copilot for one application. The webview sends only the application's id: Rust reads the
 * link, decides, opens a visible browser, fills what your profile and approved answers cover,
 * uploads the prepared files, and stops. You review and submit in the browser; JobPilot never
 * clicks submit. Each stop shows here, and only its buttons end it.
 */
export function CopilotRun({ applicationId }: { applicationId: string }) {
  const client = useQueryClient();
  const start = useMutation({
    mutationFn: () => ipc.copilotStart(applicationId),
    onSettled: () => client.invalidateQueries({ queryKey: ["applications"] }),
  });
  const live = useQuery({
    queryKey: ["apply", "copilot", applicationId],
    queryFn: () => ipc.applyLatest(),
    enabled: start.isPending,
    refetchInterval: 1000,
  });
  const run = start.isPending && live.data?.mode === "copilot" && live.data.state !== "ended" ? live.data : null;
  const ended = start.data?.items[0]?.state;

  return (
    <div className="grid gap-2">
      <p className="text-sm">
        Copilot fills this form in a visible browser window and uploads your prepared files, then stops. You review
        it and click submit yourself.
      </p>
      <div>
        <Button type="button" disabled={start.isPending} onClick={() => start.mutate()}>
          {start.isPending ? "Copilot is running..." : "Start Copilot"}
        </Button>
      </div>
      {run?.active_pause && <PausePanel runId={run.id} pause={run.active_pause} />}
      {run && !run.active_pause && (
        <p role="status" className="text-sm">
          Filling the form in the browser...
        </p>
      )}
      {start.isSuccess && (
        <p role="status" className="text-sm">
          Copilot run ended. {(ended && ENDED[ended]) ?? "Nothing was submitted."}
        </p>
      )}
      <ErrorAlert error={start.error} />
    </div>
  );
}
