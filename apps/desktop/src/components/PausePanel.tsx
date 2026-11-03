import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ErrorAlert } from "@/components/ErrorAlert";
import { Button } from "@/components/ui/button";
import { ipc, type ApplyPause, type PauseKind } from "@/lib/ipc";
import { cn } from "@/lib/utils";

/** The six mandatory pauses, always all listed, in wireframe 11's order. */
const PAUSES: { row: string; title: string; kinds: PauseKind[]; note: string }[] = [
  {
    row: "legal",
    title: "Legal declaration (FR-9.16)",
    kinds: ["legal"],
    note: "Read it and answer it yourself in the browser. JobPilot never answers this, including any question about AI use or who wrote the material.",
  },
  {
    row: "sensitive",
    title: "Unverified sensitive information (FR-9.17)",
    kinds: ["sensitive"],
    note: "Confirm or enter the value yourself in the browser.",
  },
  { row: "identity", title: "Identity verification (FR-9.18)", kinds: ["identity"], note: "Do it yourself in the browser, or cancel the run." },
  { row: "captcha", title: "CAPTCHA (FR-9.19)", kinds: ["captcha"], note: "Solve it yourself in the browser, or cancel the run." },
  { row: "mfa", title: "Multi-factor prompt (FR-9.20)", kinds: ["mfa"], note: "Enter the code yourself in the browser. JobPilot never sees it." },
  {
    row: "other",
    title: "Other step that needs a person (FR-9.21)",
    kinds: ["login", "other"],
    note: "Unmapped field, changed page, redirect off the site, unexpected dialog, unclear submission result.",
  },
];

/** Cancel run, behind a confirmation. Esc closes the confirmation and cancels nothing. */
export function CancelRun({ runId }: { runId: string }) {
  const client = useQueryClient();
  const [asking, setAsking] = useState(false);
  const cancel = useMutation({
    mutationFn: () => ipc.applyCancel(runId),
    onSuccess: () => {
      setAsking(false);
      return client.invalidateQueries({ queryKey: ["apply", "run", runId] });
    },
  });
  return (
    <>
      <Button type="button" variant="destructive" onClick={() => setAsking(true)}>
        Cancel run
      </Button>
      {asking && (
        <ConfirmDialog
          title="Cancel this run?"
          confirmLabel="Cancel run"
          busyLabel="Cancelling..."
          pending={cancel.isPending}
          error={cancel.error}
          onConfirm={() => cancel.mutate()}
          onCancel={() => setAsking(false)}
        >
          <p>
            The session and its browser close. An application already submitted stays submitted. One with a submit
            clicked and no result reads uncertain.
          </p>
        </ConfirmDialog>
      )}
    </>
  );
}

/**
 * A mandatory pause of an apply run. The page's words show as plain text: React renders them
 * inert, and nothing here reads them to choose anything. Only the buttons below end a pause,
 * and only a person presses them. Esc does nothing here.
 */
export function PausePanel({ runId, pause }: { runId: string; pause: ApplyPause }) {
  const client = useQueryClient();
  const resolve = useMutation({
    mutationFn: (resolution: "continue" | "take_over") => ipc.applyResolvePause(pause.id, resolution),
    onSuccess: () => client.invalidateQueries({ queryKey: ["apply", "run", runId] }),
  });
  const active = PAUSES.find((p) => p.kinds.includes(pause.kind)) ?? PAUSES[PAUSES.length - 1];
  return (
    <section aria-labelledby="pause-heading" className="grid gap-3 rounded-lg border border-destructive/50 p-4">
      <h3 id="pause-heading" className="font-semibold">
        Run paused: waiting for you
      </h3>
      <p className="text-sm">
        The run waits 30 minutes, then stops this application and hands it over to you.
      </p>
      <h4 className="text-sm font-semibold">Mandatory pauses</h4>
      <ul aria-label="Mandatory pauses" className="grid gap-2 text-sm">
        {PAUSES.map((p) => {
          const on = p === active;
          return (
            <li
              key={p.row}
              aria-current={on ? "step" : undefined}
              className={cn("grid gap-1 rounded-md px-2 py-1", on && "bg-muted font-medium")}
            >
              <span>
                {on ? "[>] " : "[ ] "}
                {p.title}
                {on && " ACTIVE"}
              </span>
              {on && pause.kind === "login" && <span>Sign in, in the browser window.</span>}
              {on && (
                <blockquote className="whitespace-pre-wrap border-l-2 pl-2 font-normal">{pause.excerpt}</blockquote>
              )}
              {on && <span className="font-normal text-muted-foreground">{p.note}</span>}
            </li>
          );
        })}
      </ul>
      <p className="text-sm">
        Page text is shown as plain text and cannot end a pause. Only you can, with the buttons below.
      </p>
      <ErrorAlert error={resolve.error} />
      <div className="flex flex-wrap gap-2">
        <Button type="button" disabled={resolve.isPending} onClick={() => resolve.mutate("continue")}>
          I did this in the browser: continue
        </Button>
        <Button type="button" variant="outline" disabled={resolve.isPending} onClick={() => resolve.mutate("take_over")}>
          Take over by hand
        </Button>
        <CancelRun runId={runId} />
      </div>
    </section>
  );
}
