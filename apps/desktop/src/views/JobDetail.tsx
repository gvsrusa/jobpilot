import { useEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ErrorAlert } from "@/components/ErrorAlert";
import { PageHeading } from "@/components/PageHeading";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { backendEnabled } from "@/lib/backend";
import { explainReason } from "@/lib/reasons";
import { ipc, type AuthNote, type Job, type JobDetail, type PayRange, type SourceStatus, type Stated } from "@/lib/ipc";
import { useUiStore } from "@/lib/store";

// Every posting field here comes from outside. It renders as text: no anchors, no images, no
// HTML. A link is a button that names a job: Rust reads the stored link and opens it.

const sourceLabel = (statuses: SourceStatus[], connector: string) =>
  connector === "import" ? "Imported" : (statuses.find((s) => s.name === connector)?.label ?? connector);

export const noScoreText = (job: Job) => (job.scored ? "No score: nothing in your profile to compare" : "Not scored yet");

function payText(pay: PayRange | null): string {
  if (!pay || (pay.min === null && pay.max === null)) return "not stated";
  const range = pay.min !== null && pay.max !== null ? `${pay.min}-${pay.max}` : String(pay.min ?? pay.max);
  const period = pay.period ? `per ${pay.period}` : "period not stated";
  return `${range} ${pay.currency ?? "(currency not stated)"}, ${period}`;
}

const stated = (value: Stated) => (value === "not_stated" ? "not stated" : value);

/** Exactly one line. It never says whether a person may work there: nothing here knows. */
function authText(note: AuthNote): string {
  switch (note.kind) {
    case "no_country":
      return "No country on this posting. Not checked.";
    case "not_stated":
      return `${note.country}: not stated by you. Not checked.`;
    case "stated":
      return `You stated: authorized ${stated(note.authorized)}, needs sponsorship ${stated(note.needs_sponsorship)} for ${note.country}. Not checked against this posting.`;
    case "locked":
      return "Locked, see Recovery.";
  }
}

/** The host of a stored link, for the label of the button that opens it. */
function hostOf(url: string | null): string | null {
  if (!url || !/^https:\/\//i.test(url)) return null;
  try {
    return new URL(url).host;
  } catch {
    return null;
  }
}

function Explanation({ detail }: { detail: JobDetail }) {
  const { explanation: e, job } = detail;
  if (!e) return <p>Could not read this posting&apos;s fields, or it has not been scored yet.</p>;
  return (
    <>
      <Table aria-label="Score explanation" tabIndex={0}>
        <TableHeader>
          <TableRow>
            <TableHead>Criterion</TableHead>
            <TableHead>Weight</TableHead>
            <TableHead>Points</TableHead>
            <TableHead>Note</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {e.rows.map((r) => (
            <TableRow key={r.criterion}>
              <TableCell>{r.criterion}</TableCell>
              <TableCell>{r.weight}</TableCell>
              <TableCell>{r.points ?? "not compared"}</TableCell>
              <TableCell className="whitespace-normal">{r.note}</TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {e.compared_weight < 100 && <p>{`Compared weight: ${e.compared_weight} of 100`}</p>}
      {e.missing_skills.length > 0 ? (
        <p>{`Missing skills: ${e.missing_skills.join(", ")}`}</p>
      ) : (
        <p>
          {e.rows.some((r) => r.criterion === "skills" && r.points !== null)
            ? "Missing skills: None found"
            : "Add your profile to see this."}
        </p>
      )}
      <p>{`Pay: ${payText(job.pay)}`}</p>
    </>
  );
}

function Evaluation({ detail }: { detail: JobDetail }) {
  const s = detail.semantic;
  if (!s) return null;
  return (
    <section role="region" aria-label="AI evaluation" className="grid gap-1 rounded-lg border p-3">
      <h3 className="font-semibold">AI evaluation</h3>
      <p>{`fit ${s.fit} of 100. The model's own number: it is not part of the score.`}</p>
      <p>{s.summary}</p>
      {s.strengths.length > 0 && (
        <ul aria-label="Strengths">
          {s.strengths.map((x, i) => (
            <li key={i}>{`Strength: ${x.text} (evidence: ${x.evidence_ids.join(", ")})`}</li>
          ))}
        </ul>
      )}
      {s.gaps.length > 0 && (
        <ul aria-label="Gaps">
          {s.gaps.map((g, i) => (
            <li key={i}>{`Gap: ${g}`}</li>
          ))}
        </ul>
      )}
      <p>{`Provider: ${s.provider}`}</p>
      <p>{`Date: ${s.at.slice(0, 10)}`}</p>
      {s.dropped > 0 && <p>{`dropped: ${s.dropped}`}</p>}
    </section>
  );
}

export function JobDetailView({ id, onBack }: { id: string; onBack(): void }) {
  const client = useQueryClient();
  const detail = useQuery({ queryKey: ["job", id], queryFn: () => ipc.jobDetail(id) });
  const statuses = useQuery({ queryKey: ["job-sources-status"], queryFn: ipc.jobSourcesStatus });
  const ai = useQuery({ queryKey: ["ai"], queryFn: ipc.aiStatus });
  const [removing, setRemoving] = useState(false);
  const heading = useRef<HTMLDivElement>(null);
  const d: JobDetail | undefined = detail.data;
  const job: Job | undefined = d?.job;
  // Enter on a row lands here: the heading is where a reader starts.
  useEffect(() => {
    if (job) heading.current?.focus();
  }, [job?.id]);
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.altKey && e.key === "ArrowLeft") {
        e.preventDefault();
        onBack();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onBack]);
  const refresh = async () => {
    await client.invalidateQueries({ queryKey: ["job", id] });
    await client.invalidateQueries({ queryKey: ["jobs"] });
  };
  const evaluate = useMutation({ mutationFn: () => ipc.jobsAnalyze([id], true), onSettled: refresh });
  const track = useMutation({
    mutationFn: () => ipc.jobToApp(id),
    onSuccess: () => client.invalidateQueries({ queryKey: ["applications"] }),
  });
  // Rust names the count of outside calls in its own dialog before it researches.
  const research = useMutation({ mutationFn: () => ipc.jobResearch(job!.company_id!), onSettled: refresh });
  const [note, setNote] = useState("");
  const addNote = useMutation({
    mutationFn: () => ipc.companyNoteAdd(job!.company_id!, note.trim()),
    onSuccess: async () => {
      setNote("");
      await refresh();
    },
  });
  const open = useMutation({ mutationFn: () => ipc.jobOpen(id) });
  const split = useMutation({ mutationFn: () => ipc.jobSplit(id), onSettled: refresh });
  const remove = useMutation({
    mutationFn: () => ipc.jobRemove(id),
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: ["jobs"] });
      onBack();
    },
  });
  const host = hostOf(job?.url ?? null);
  const stopped = evaluate.data?.reason ?? null;
  return (
    <div className="grid gap-4">
      <div>
        <Button type="button" variant="ghost" onClick={onBack}>
          Back to Jobs
        </Button>
      </div>
      <ErrorAlert error={detail.error} />
      {d && job && (
        <>
          <div className="flex flex-wrap items-baseline justify-between gap-4">
            <div ref={heading} tabIndex={-1} className="outline-none">
              <PageHeading>
                {job.title}
                {job.company ? `, ${job.company}` : ""}
              </PageHeading>
            </div>
            <div className="flex items-baseline gap-3">
              <span>{`Score ${job.score === null ? "--" : `${job.score} / 100`}`}</span>
              {job.score === null && <span className="text-sm text-muted-foreground">{noScoreText(job)}</span>}
              {job.stale && <span className="text-sm text-muted-foreground">Out of date</span>}
              {stopped && (
                <span role="alert" className="text-sm text-destructive">
                  AI evaluation stopped. The rules score stands.{" "}
                  {stopped === "no_evidence" &&
                    "Add a knowledge item and approve it in the Knowledge section of the Resumes screen, then try again. "}
                  <span className="font-mono text-xs">{`code: ${stopped}`}</span>
                </span>
              )}
            </div>
          </div>
          <p className="text-sm text-muted-foreground">
            <span>{`Source: ${job.attribution ?? sourceLabel(statuses.data ?? [], job.connector)} (`}</span>
            {host ? (
              <Button type="button" variant="link" size="sm" className="h-auto p-0" onClick={() => open.mutate()}>
                {`link, ${host}`}
              </Button>
            ) : (
              <span>no link</span>
            )}
            <span>)</span> <span>Untrusted text. Shown as plain text.</span>
          </p>
          <div className="grid gap-4 md:grid-cols-2">
            <section aria-labelledby="why-heading" className="grid content-start gap-2 text-sm">
              <h2 id="why-heading" className="font-semibold">
                Why this score
              </h2>
              <Explanation detail={d} />
              <div className="grid gap-1">
                <p className="font-medium">Work authorization</p>
                <p>{authText(d.authorization)}</p>
                {d.mentions_authorization && <p>This posting mentions work authorization. Read it yourself.</p>}
              </div>
              <p>{`Scored by: ${d.explanation?.by ?? "rules"}.`}</p>
              {backendEnabled(ai.data) && (
                <div>
                  <Button type="button" variant="outline" disabled={evaluate.isPending} onClick={() => evaluate.mutate()}>
                    {evaluate.isPending ? "Evaluating..." : "Add AI evaluation"}
                  </Button>
                </div>
              )}
              <ErrorAlert error={evaluate.error} />
              <Evaluation detail={d} />
              {job.company_id && backendEnabled(ai.data) && (
                <div>
                  <Button type="button" variant="outline" disabled={research.isPending} onClick={() => research.mutate()}>
                    {research.isPending ? "Researching..." : "Research company"}
                  </Button>
                </div>
              )}
              <ErrorAlert error={research.error} />
              {research.data && (
                <p role="status">
                  {`${research.data.sources} source(s) read, ${research.data.facts} fact(s) kept, ${research.data.dropped} dropped.`}
                  {research.data.reason && ` Stopped: ${explainReason(research.data.reason) ?? research.data.reason}.`}
                </p>
              )}
              {d.group.length > 0 && (
                <p>
                  Same job on {d.group.length} other {d.group.length === 1 ? "source" : "sources"}.{" "}
                  <Button type="button" variant="outline" size="sm" onClick={() => split.mutate()}>
                    Not a duplicate
                  </Button>
                </p>
              )}
              {d.notes.map((n) => (
                <p key={n.id}>
                  {`Company note (${n.origin}${n.source_ref ? `, from ${n.source_ref}` : ""}): ${n.text}`}
                </p>
              ))}
              {job.company_id && (
                <form
                  className="grid gap-1.5"
                  onSubmit={(e) => {
                    e.preventDefault();
                    addNote.mutate();
                  }}
                >
                  <Label htmlFor="company-note">Company note</Label>
                  <div className="flex gap-2">
                    <Input id="company-note" value={note} onChange={(e) => setNote(e.target.value)} />
                    <Button type="submit" variant="outline" disabled={addNote.isPending || note.trim() === ""}>
                      Save note
                    </Button>
                  </div>
                  <ErrorAlert error={addNote.error} />
                </form>
              )}
            </section>
            <section aria-labelledby="text-heading" className="grid content-start gap-2 text-sm">
              <h2 id="text-heading" className="font-semibold">
                Posting text
              </h2>
              <div
                role="region"
                aria-label="Posting text"
                tabIndex={0}
                className="max-h-96 overflow-auto rounded-lg border p-3 whitespace-pre-wrap"
              >
                {d.text}
              </div>
              {d.findings.length > 0 && (
                <ul aria-label="Findings">
                  {d.findings.map((f, i) => (
                    <li key={i}>{`${f.kind}: ${f.excerpt}`}</li>
                  ))}
                </ul>
              )}
            </section>
          </div>
          <div className="flex flex-wrap gap-2">
            <Button type="button" disabled={track.isPending || track.isSuccess} onClick={() => track.mutate()}>
              Add to tracker
            </Button>
            <Button type="button" variant="outline" disabled={!host} onClick={() => open.mutate()}>
              Open posting in my browser
            </Button>
            <Button type="button" variant="outline" onClick={() => useUiStore.getState().openResumes(job.job_text_id)}>
              Prepare...
            </Button>
            <Button type="button" variant="outline" onClick={() => setRemoving(true)}>
              Remove job
            </Button>
          </div>
          {track.isSuccess && <p role="status">Added to the tracker.</p>}
          <ErrorAlert error={track.error ?? open.error ?? split.error} />
          {removing && (
            <ConfirmDialog
              title="Remove this job?"
              confirmLabel="Remove"
              busyLabel="Removing..."
              pending={remove.isPending}
              error={remove.error}
              onConfirm={() => remove.mutate()}
              onCancel={() => setRemoving(false)}
            >
              <p>
                The posting text is deleted with the job, unless a draft cites it. If this job is in your tracker, the
                application stays but loses its posting text.
              </p>
            </ConfirmDialog>
          )}
        </>
      )}
    </div>
  );
}
