import { useId, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CopyPath } from "@/components/CopyPath";
import { ErrorAlert } from "@/components/ErrorAlert";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { useUiStore } from "@/lib/store";
import { CopilotRun } from "@/views/CopilotRun";
import {
  ipc,
  STATUSES,
  toAppError,
  type Application,
  type ApplicationStatus,
  type MaterialRow,
  type ModeChoice,
} from "@/lib/ipc";

const MODE_NAMES: Record<ModeChoice["mode"], string> = {
  manual: "Manual",
  copilot: "Copilot",
  autonomous: "Autonomous",
  claude_code: "Claude Code apply",
};
const STATED = { yes: "Yes", no: "No", not_stated: "Not stated by you" } as const;
const NOT_HTTPS = "The posting link is not https, so JobPilot will not open it.";
const CANCELLED = "confirmation_required";
/** The user closed the confirmation dialog: their answer, not an error. */
class Declined extends Error {}

const label = (status: string) => status.charAt(0).toUpperCase() + status.slice(1);
const selectClass =
  "h-8 rounded-lg border border-input bg-transparent px-2.5 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50";

function hostOf(link: string): string {
  try {
    return new URL(link.trim()).host;
  } catch {
    return "";
  }
}

function answerState(row: MaterialRow): string {
  if (row.class !== "normal") {
    const name = row.class.replace("_", " ").toUpperCase();
    return row.needs_confirmation ? `${name}: confirm` : `${name}: confirmed for this application`;
  }
  return row.stale ? "approved, not reviewed for over 180 days" : "approved, fresh";
}

/**
 * Manual mode for one application, before anything happens on the site (wireframe 11). Each
 * mode shows Rust's fixed note, and a mode that is not available can be focused to read it but
 * not chosen. Answers go to the clipboard from Rust, so no answer's text comes to this screen. A
 * sealed answer is confirmed in a native dialog each time: there is no plain copy for it.
 */
export function ApplicationReview({ application, onClose }: { application: Application; onClose: () => void }) {
  const client = useQueryClient();
  const ids = useId();
  const key = ["apply", "material", application.id];
  const material = useQuery({ queryKey: key, queryFn: () => ipc.applyMaterial(application.id) });
  const [mode, setMode] = useState<ModeChoice["mode"]>("manual");
  const [copied, setCopied] = useState<{ id: string; stale: boolean } | null>(null);
  const [showStated, setShowStated] = useState(false);
  const [recording, setRecording] = useState(false);
  const [outcome, setOutcome] = useState<ApplicationStatus>("applied");
  const [on, setOn] = useState("");
  const [note, setNote] = useState("");
  const setView = useUiStore((s) => s.setView);

  const copy = useMutation({
    mutationFn: async (row: MaterialRow) => {
      if (row.class !== "normal") {
        try {
          await ipc.memoryConfirmUse(row.answer_id, application.id);
        } catch (error) {
          throw toAppError(error).code === CANCELLED ? new Declined() : error;
        }
      }
      return ipc.applyCopy(application.id, row.answer_id);
    },
    onMutate: () => setCopied(null),
    onSuccess: (done, row) => setCopied({ id: row.answer_id, stale: done.stale }),
    onSettled: () => client.invalidateQueries({ queryKey: key }),
  });
  const open = useMutation({ mutationFn: () => ipc.applyOpen(application.id) });
  const record = useMutation({
    mutationFn: () => ipc.applyRecord(application.id, outcome, on.trim() || null, note),
    onSuccess: () => {
      setRecording(false);
      return client.invalidateQueries({ queryKey: ["applications"] });
    },
  });

  // Rust checks the stored link again, and refuses more than this does.
  const linkOk = /^https:\/\//i.test(application.link.trim());
  const data = material.data;
  const authorization = data?.authorization;
  const copyError = copy.error instanceof Declined ? null : copy.error;

  return (
    <section className="grid gap-5" aria-labelledby={`${ids}-heading`}>
      <div className="flex items-center gap-3">
        <h2 id={`${ids}-heading`} className="text-base font-semibold">
          Review application: {application.role}, {application.company}
        </h2>
        <Button type="button" variant="outline" size="sm" onClick={onClose}>
          Back
        </Button>
      </div>
      <p className="text-sm">
        Site: <span className="select-text">{hostOf(application.link) || "no link"}</span>
      </p>
      <ErrorAlert error={material.error} />

      {data && (
        <>
          <fieldset className="grid gap-2">
            <legend id={`${ids}-modes`} className="text-sm font-semibold">
              How do you want to apply?
            </legend>
            <div role="radiogroup" aria-labelledby={`${ids}-modes`} className="grid gap-2">
              {data.modes.map((choice) => {
                const noteId = `${ids}-note-${choice.mode}`;
                return (
                  <div key={choice.mode} className="grid gap-0.5">
                    <label className="flex items-center gap-2 text-sm font-medium">
                      <input
                        type="radio"
                        name={`${ids}-mode`}
                        value={choice.mode}
                        checked={mode === choice.mode}
                        aria-disabled={!choice.available}
                        aria-describedby={noteId}
                        onClick={(e) => {
                          if (!choice.available) e.preventDefault();
                        }}
                        onChange={() => {
                          if (choice.available) setMode(choice.mode);
                        }}
                      />
                      {MODE_NAMES[choice.mode]}
                    </label>
                    <p id={noteId} className="pl-6 text-sm text-muted-foreground">
                      {choice.available ? choice.note : `Not available. ${choice.note}`}
                    </p>
                  </div>
                );
              })}
            </div>
            {mode === "copilot" && <CopilotRun applicationId={application.id} />}
            {mode === "claude_code" && (
              <p className="text-sm">Select this application on Applications, then press Apply with Claude Code...</p>
            )}
          </fieldset>

          <section className="grid gap-3" aria-labelledby={`${ids}-material`}>
            <h3 id={`${ids}-material`} className="text-sm font-semibold">
              Material for this application
            </h3>
            {data.resume === null && data.letter === null ? (
              <p className="flex items-center gap-3 text-sm">
                Nothing prepared.
                <Button type="button" variant="outline" size="sm" onClick={() => setView("resumes")}>
                  Prepare documents
                </Button>
              </p>
            ) : (
              <ul className="grid gap-1 text-sm" aria-label="Files">
                {(
                  [
                    ["Resume (PDF)", data.resume],
                    ["Cover letter (PDF)", data.letter],
                  ] as const
                ).map(([name, path]) => (
                  <li key={name} className="flex items-center gap-3">
                    {name}:
                    {path ? (
                      <>
                        <span className="select-text break-all">{path}</span>
                        <CopyPath path={path} />
                      </>
                    ) : (
                      <span>nothing prepared</span>
                    )}
                  </li>
                ))}
              </ul>
            )}

            {data.answers.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                No approved answers for this form. You can fill it by hand.
              </p>
            ) : (
              <ul className="grid gap-2 text-sm" aria-label="Answers you approved for this form">
                {data.answers.map((row) => {
                  const sealed = row.class !== "normal";
                  const action = sealed ? "Confirm and copy" : "Copy";
                  return (
                    <li key={row.answer_id} className="flex items-center gap-3">
                      <span className="select-text">{row.question}</span>
                      <span className="text-muted-foreground">{answerState(row)}</span>
                      <Button
                        type="button"
                        variant="outline"
                        size="sm"
                        aria-label={`${action}: ${row.question}`}
                        disabled={copy.isPending}
                        onClick={() => copy.mutate(row)}
                      >
                        {action}
                      </Button>
                      {copied?.id === row.answer_id && (
                        <span role="status">
                          Copied to the clipboard.
                          {copied.stale && " This answer has not been reviewed for over 180 days."}
                        </span>
                      )}
                    </li>
                  );
                })}
              </ul>
            )}
            <ErrorAlert error={copyError} />

            {authorization && (
              <div className="grid gap-1 text-sm">
                {authorization.stated ? (
                  <>
                    <p className="flex items-center gap-3">
                      Work authorization ({authorization.country}): stated by you
                      <Button type="button" variant="outline" size="sm" onClick={() => setShowStated(!showStated)}>
                        {showStated ? "Hide what I stated" : "Show what I stated"}
                      </Button>
                    </p>
                    {showStated && (
                      <ul aria-label="Stated values">
                        <li>Authorized: {STATED[authorization.authorized]}</li>
                        <li>Needs sponsorship: {STATED[authorization.needs_sponsorship]}</li>
                        {authorization.note && <li className="select-text">{authorization.note}</li>}
                      </ul>
                    )}
                  </>
                ) : (
                  <p className="flex items-center gap-3">
                    {authorization.country
                      ? `Work authorization (${authorization.country}): not stated by you`
                      : "Work authorization: not stated by you. This application has no country."}
                    <Button type="button" variant="outline" size="sm" onClick={() => setView("profile")}>
                      Enter it yourself
                    </Button>
                  </p>
                )}
              </div>
            )}
          </section>
        </>
      )}

      <div className="grid gap-2">
        <div className="flex gap-2">
          <Button
            type="button"
            disabled={!linkOk || open.isPending}
            aria-describedby={linkOk ? undefined : `${ids}-not-https`}
            onClick={() => open.mutate()}
          >
            Open the form in my browser
          </Button>
          <Button type="button" variant="outline" onClick={() => setRecording(true)}>
            Record outcome...
          </Button>
        </div>
        {!linkOk && (
          <p id={`${ids}-not-https`} className="text-sm">
            {NOT_HTTPS}
          </p>
        )}
        <ErrorAlert error={open.error} />
        {record.isSuccess && !recording && (
          <p role="status" className="text-sm">
            Recorded: {label(record.data.status)}.
          </p>
        )}
      </div>

      {recording && (
        <form
          className="grid gap-3"
          aria-label="Record outcome"
          onSubmit={(e) => {
            e.preventDefault();
            if (!record.isPending) record.mutate();
          }}
        >
          <div className="grid gap-1.5">
            <Label htmlFor={`${ids}-outcome`}>Outcome</Label>
            <select
              id={`${ids}-outcome`}
              className={selectClass}
              value={outcome}
              onChange={(e) => setOutcome(e.target.value as ApplicationStatus)}
            >
              {STATUSES.map((s) => (
                <option key={s} value={s}>
                  {label(s)}
                </option>
              ))}
            </select>
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor={`${ids}-on`}>Applied on (optional)</Label>
            <Input
              id={`${ids}-on`}
              placeholder="YYYY-MM-DD"
              className="max-w-40"
              value={on}
              onChange={(e) => setOn(e.target.value)}
            />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor={`${ids}-note`}>Note (optional)</Label>
            <Input id={`${ids}-note`} value={note} onChange={(e) => setNote(e.target.value)} />
          </div>
          <ErrorAlert error={record.error} />
          <div className="flex gap-2">
            <Button type="submit" disabled={record.isPending}>
              {record.isPending ? "Saving..." : "Save outcome"}
            </Button>
            <Button type="button" variant="outline" onClick={() => setRecording(false)}>
              Cancel
            </Button>
          </div>
        </form>
      )}
    </section>
  );
}
