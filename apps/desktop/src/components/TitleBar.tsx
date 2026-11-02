import { ErrorAlert } from "@/components/ErrorAlert";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { toAppError } from "@/lib/ipc";
import type { Pause } from "@/lib/pause";

/**
 * First in the page on every screen, so the pause is the first tab stop. A declined clear
 * dialog is the user's answer, not an error. With no data open (`unavailable`) nothing is
 * running, and the alert says so.
 */
export function TitleBar({ version, pause }: { version?: string; pause: Pause }) {
  const paused = pause.status.data?.paused === true;
  const error = pause.set.error ?? pause.clear.error;
  const code = error ? toAppError(error).code : null;
  return (
    <header className="border-b">
      <div className="flex items-center justify-between gap-4 px-4 py-2">
        <span className="font-semibold">JobPilot AI</span>
        <span className="flex items-center gap-3">
          {paused ? (
            <>
              <span role="status" className="text-sm font-medium text-destructive">
                Paused. Nothing automated runs.
              </span>
              <Button size="sm" variant="outline" onClick={() => pause.clear.mutate()} disabled={pause.clear.isPending}>
                Clear pause
              </Button>
            </>
          ) : (
            <Button
              size="sm"
              variant="destructive"
              aria-keyshortcuts="Control+Shift+H Meta+Shift+H"
              title="Stop everything automated (Ctrl+Shift+H, or Cmd+Shift+H on a Mac)"
              onClick={() => pause.set.mutate()}
              disabled={pause.set.isPending}
            >
              Emergency pause
            </Button>
          )}
          {version && <span className="text-sm text-muted-foreground">v{version}</span>}
        </span>
      </div>
      {code === "unavailable" ? (
        <Alert variant="destructive" className="mx-4 mb-2 w-auto">
          <AlertDescription className="flex justify-between gap-4">
            <span>No data is open, so nothing is running.</span>
            <span className="shrink-0 font-mono text-xs">code: unavailable</span>
          </AlertDescription>
        </Alert>
      ) : (
        code !== "confirmation_required" && (
          <div className="px-4 pb-2 empty:hidden">
            <ErrorAlert error={error} />
          </div>
        )
      )}
    </header>
  );
}
