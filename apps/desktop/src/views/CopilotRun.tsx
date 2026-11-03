import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ErrorAlert } from "@/components/ErrorAlert";
import { COPILOT_LIVE, liveCopilot, useLiveCopilot } from "@/components/CopilotStop";
import { Button } from "@/components/ui/button";
import { ipc } from "@/lib/ipc";

const ENDED: Record<string, string> = {
  submitted: "You said you submitted it. It is recorded as applied, with the page you ended on.",
  manual_required: "Left to you. Record the outcome when you have applied.",
  skipped: "Stopped before the review. Nothing was submitted.",
  uncertain: "Stopped after you pressed Continue, before it was recorded. Check the site. JobPilot will not try again.",
};

/**
 * Copilot for one application. The webview sends only the application's id: Rust reads the
 * link, decides, opens a visible browser, fills what your profile and approved answers cover,
 * uploads the prepared files, and stops. You review and submit in the browser; JobPilot never
 * clicks submit. Each stop shows at the top of the app, and only its buttons end it.
 */
export function CopilotRun({ applicationId }: { applicationId: string }) {
  const client = useQueryClient();
  const live = useLiveCopilot();
  const start = useMutation({
    mutationFn: () => ipc.copilotStart(applicationId),
    onMutate: () => client.invalidateQueries({ queryKey: COPILOT_LIVE }),
    onSettled: () =>
      Promise.all([
        client.invalidateQueries({ queryKey: ["applications"] }),
        client.invalidateQueries({ queryKey: COPILOT_LIVE }),
      ]),
  });
  // Read from the run itself, so a review screen opened again knows a run is still live.
  const running = liveCopilot(live.data);
  const ours = running?.items.some((item) => item.application_id === applicationId) ?? false;
  const busy = start.isPending || running !== null;
  const ended = start.data?.items[0]?.state;
  // A run that ends on an error (a pause or a cancel after Continue) returns no view: its end
  // is read from the run itself, so "Check the site" still shows beside the error. Only a run
  // this start created counts: a start refused before any run shows no earlier run's end.
  const lastRun = live.data && live.data.mode === "copilot" && live.data.state === "ended" ? live.data : null;
  const thisStart = lastRun !== null && start.submittedAt > 0 && Date.parse(lastRun.created_at) >= start.submittedAt;
  const endedOnError =
    start.isError && thisStart ? lastRun?.items.find((item) => item.application_id === applicationId)?.state : undefined;

  return (
    <div className="grid gap-2">
      <p className="text-sm">
        Copilot fills this form in a visible browser window and uploads your prepared files, then stops. You review
        it and click submit yourself.
      </p>
      <div>
        <Button type="button" disabled={busy} onClick={() => start.mutate()}>
          {busy ? "Copilot is running..." : "Start Copilot"}
        </Button>
      </div>
      {running && (
        <p role="status" className="text-sm">
          {running.active_pause
            ? "Copilot is waiting for you: answer its stop at the top of the window."
            : ours
              ? "Filling the form in the browser..."
              : "A Copilot run for another application is still open."}
        </p>
      )}
      {start.isSuccess && !running && (
        <p role="status" className="text-sm">
          Copilot run ended. {(ended && ENDED[ended]) ?? "Nothing was submitted."}
        </p>
      )}
      {endedOnError && ENDED[endedOnError] && (
        <p role="status" className="text-sm">
          Copilot run ended. {ENDED[endedOnError]}
        </p>
      )}
      <ErrorAlert error={start.error} />
    </div>
  );
}
