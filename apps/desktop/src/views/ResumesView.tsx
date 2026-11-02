import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CompareTable } from "@/components/CompareTable";
import { CopyPath } from "@/components/CopyPath";
import { DraftEditor } from "@/components/DraftEditor";
import { ErrorAlert } from "@/components/ErrorAlert";
import { KnowledgeSection } from "@/components/KnowledgeSection";
import { PageHeading } from "@/components/PageHeading";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import {
  ipc,
  toAppError,
  type AiStatus,
  type Claim,
  type Draft,
  type DraftKind,
  type ExportFormat,
  type JobText,
  type Template,
} from "@/lib/ipc";
import { ENDPOINT_SUSPENDED, explainReason, SUSPENDED } from "@/lib/reasons";

/** A generation backend is one that can answer now: Ollama running, or an external provider that is on, available, consented, and able to carry a posting. */
function backendEnabled(ai: AiStatus | undefined): boolean {
  if (!ai) return false;
  const ollama = ai.enabled.ollama && ai.ollama.state === "running";
  const claude = ai.enabled.claude_code && ai.claude_code.available && ai.claude_code.consent !== null;
  const external = ai.external.some(
    (p) => ai.enabled[p.name] && p.available && p.consent !== null && p.runs_job_text,
  );
  return ollama || claude || external;
}

function endpointSuspended(ai: AiStatus | undefined): boolean {
  return ai?.external.some((p) => p.kind === "endpoint" && ai.enabled[p.name] && p.suspended) ?? false;
}

/** Nothing can draft, and a provider that is on is only waiting for a review. */
function suspendedOnly(ai: AiStatus | undefined): boolean {
  if (!ai || backendEnabled(ai)) return false;
  return (ai.enabled.claude_code && ai.claude_code.suspended) || ai.external.some((p) => ai.enabled[p.name] && p.suspended);
}

/** Versions grouped by lineage, oldest version first. The list arrives newest first. */
function lineages(drafts: Draft[]): Draft[][] {
  const groups = new Map<string, Draft[]>();
  for (const d of drafts) groups.set(d.lineage_id, [...(groups.get(d.lineage_id) ?? []), d]);
  return [...groups.values()].map((g) => g.sort((a, b) => a.version - b.version));
}

const tied = (group: Draft[], jobId: string | null) => group.some((d) => d.job_text_id === jobId);
const latest = (group: Draft[]) => group[group.length - 1];

function PostingForm({ onSaved, onClose }: { onSaved(job: JobText): void; onClose(): void }) {
  const [title, setTitle] = useState("");
  const [raw, setRaw] = useState("");
  const paste = useMutation({ mutationFn: () => ipc.jobPaste(title, raw), onSuccess: onSaved });
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent showCloseButton={false}>
        <DialogHeader>
          <DialogTitle>Paste a job posting</DialogTitle>
          <DialogDescription>
            The posting is kept as data. JobPilot never follows instructions written in it.
          </DialogDescription>
        </DialogHeader>
        <form
          className="grid gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            paste.mutate();
          }}
        >
          <div className="grid gap-1.5">
            <Label htmlFor="posting-title">Posting title</Label>
            <Input id="posting-title" value={title} onChange={(e) => setTitle(e.target.value)} />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="posting-text">Posting text</Label>
            <Textarea id="posting-text" value={raw} onChange={(e) => setRaw(e.target.value)} />
          </div>
          <ErrorAlert error={paste.error} />
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" disabled={paste.isPending || raw.trim() === ""}>
              Save posting
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function ClaimEditor({
  claim,
  onClose,
  onSaved,
}: {
  claim: Claim;
  onClose(): void;
  onSaved(draft: Draft): void;
}) {
  const knowledge = useQuery({ queryKey: ["knowledge"], queryFn: ipc.kbList });
  const [text, setText] = useState(claim.text);
  const [cited, setCited] = useState(claim.evidence_ids);
  const save = useMutation({
    mutationFn: () => ipc.claimEdit(claim.id, { text, evidence_ids: cited }),
    onSuccess: onSaved,
  });
  // The core counts only approved, unmarked items as evidence, so only those are offered.
  const usable = (knowledge.data ?? []).filter((k) => k.approved && !k.marked_sensitive && !k.sealed);
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent showCloseButton={false}>
        <DialogHeader>
          <DialogTitle>Edit claim</DialogTitle>
          <DialogDescription>Saving makes a new version. The old one stays.</DialogDescription>
        </DialogHeader>
        <form
          className="grid gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            save.mutate();
          }}
        >
          <div className="grid gap-1.5">
            <Label htmlFor="claim-text">Claim text</Label>
            <Textarea id="claim-text" value={text} onChange={(e) => setText(e.target.value)} />
          </div>
          <fieldset className="grid gap-1">
            <legend className="text-sm">Evidence</legend>
            {usable.length === 0 && (
              <p className="text-sm text-muted-foreground">No approved knowledge items yet.</p>
            )}
            {usable.map((k) => (
              <label key={k.id} className="flex items-start gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={cited.includes(k.id)}
                  onChange={(e) =>
                    setCited(e.target.checked ? [...cited, k.id] : cited.filter((id) => id !== k.id))
                  }
                />
                <span>{k.body}</span>
              </label>
            ))}
          </fieldset>
          <ErrorAlert error={save.error ?? knowledge.error} />
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" disabled={save.isPending}>
              Save claim
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export function ResumesView() {
  const client = useQueryClient();
  const [kind, setKind] = useState<DraftKind>("resume");
  const [jobChoice, setJobChoice] = useState<string | null>(null);
  const [lineageChoice, setLineageChoice] = useState<string | null>(null);
  const [draftChoice, setDraftChoice] = useState<string | null>(null);
  const [originalId, setOriginalId] = useState<string | null>(null);
  const [selected, setSelected] = useState(0);
  const [template, setTemplate] = useState<Template>("classic");
  const [exportAs, setExportAs] = useState<"none" | ExportFormat>("none");
  const [editor, setEditor] = useState<{ note: string | null } | null>(null);
  const [claimEditing, setClaimEditing] = useState(false);
  const [pasting, setPasting] = useState(false);

  const jobs = useQuery({ queryKey: ["jobs"], queryFn: ipc.jobList });
  const ai = useQuery({ queryKey: ["ai"], queryFn: ipc.aiStatus });
  const drafts = useQuery({ queryKey: ["drafts", kind], queryFn: () => ipc.resumeList(kind) });

  const job = jobs.data?.find((j) => j.id === jobChoice) ?? jobs.data?.[0] ?? null;
  const groups = lineages(drafts.data ?? []);
  const forJob = groups.filter((g) => tied(g, job?.id ?? null));
  const group =
    groups.find((g) => g[0].lineage_id === lineageChoice) ?? (lineageChoice === null ? forJob[0] : undefined);
  const current = group ? (group.find((d) => d.id === draftChoice) ?? latest(group)) : null;
  const earlier = group && current ? group.filter((d) => d.version < current.version) : [];

  const compare = useQuery({
    queryKey: ["compare", current?.id, originalId],
    queryFn: () => ipc.resumeCompare(current!.id, originalId),
    enabled: current !== null,
  });
  const claims = compare.data?.generated ?? [];
  const claim = claims[Math.min(selected, claims.length - 1)];

  // Reset, not invalidate: a stale compare or draft list must never flash after an edit.
  const refresh = () => Promise.all([client.resetQueries({ queryKey: ["drafts"] }), client.resetQueries({ queryKey: ["compare"] })]);
  const show = (draft: Draft) => {
    setKind(draft.kind);
    setLineageChoice(draft.lineage_id);
    setDraftChoice(draft.id);
    setOriginalId(null);
    setSelected(0);
    void refresh();
  };

  const generate = useMutation({
    mutationFn: () => {
      const choice = exportAs === "none" ? undefined : { format: exportAs, template };
      return kind === "resume" ? ipc.resumeGenerate(job!.id, choice) : ipc.letterGenerate(job!.id, choice);
    },
    onSuccess: (generated) => show(generated.draft),
    onError: (error) => {
      if (toAppError(error).code === "manual_required") {
        const why = explainReason(error);
        setEditor({
          note: `Nothing was generated${why ? `: ${why}` : ""}. You can write it yourself here.`,
        });
      }
    },
  });
  const importResume = useMutation({
    mutationFn: ipc.resumeImport,
    // A resume imported with no job is still shown: its lineage is chosen by name.
    onSuccess: (imported) => {
      setKind("resume");
      setLineageChoice(imported.lineage_id);
      setDraftChoice(imported.draft_id);
      setOriginalId(null);
      setSelected(0);
      void refresh();
      void client.resetQueries({ queryKey: ["knowledge"] });
    },
  });
  const remove = useMutation({
    mutationFn: (id: string) => ipc.claimRemove(id),
    onSuccess: show,
  });
  const exported = useMutation({
    mutationFn: (format: ExportFormat) => ipc.resumeExport(current!.id, template, format),
  });
  const cancel = useMutation({ mutationFn: ipc.aiCancel });

  const blocked = compare.data?.blocked ?? 0;
  const cancelled = (e: unknown) => (e && toAppError(e).code === "cancelled" ? null : e);
  const generationError = generate.error && toAppError(generate.error).code !== "manual_required" ? generate.error : null;
  const hint = generationError ? explainReason(generationError) : null;

  return (
    <div className="grid max-w-6xl gap-4">
      <PageHeading>Resumes</PageHeading>
      <div className="flex flex-wrap items-end gap-3">
        <div className="flex gap-1" role="group" aria-label="Kind">
          {(["resume", "cover_letter"] as const).map((k) => (
            <Button
              key={k}
              size="sm"
              variant={kind === k ? "secondary" : "outline"}
              aria-pressed={kind === k}
              onClick={() => {
                setKind(k);
                setLineageChoice(null);
                setDraftChoice(null);
                setOriginalId(null);
              }}
            >
              {k === "resume" ? "Resumes" : "Cover letters"}
            </Button>
          ))}
        </div>
        <div className="grid gap-1">
          <Label htmlFor="job-select">Job posting</Label>
          <select
            id="job-select"
            className="h-8 rounded-lg border bg-background px-2 text-sm"
            value={job?.id ?? ""}
            onChange={(e) => {
              setJobChoice(e.target.value);
              setLineageChoice(null);
              setDraftChoice(null);
            }}
          >
            {(jobs.data ?? []).length === 0 && <option value="">No job posting yet</option>}
            {(jobs.data ?? []).map((j) => (
              <option key={j.id} value={j.id}>
                {j.title}
              </option>
            ))}
          </select>
        </div>
        <div className="grid gap-1">
          <Label htmlFor="export-after">After drafting, export as</Label>
          <select
            id="export-after"
            className="h-8 rounded-lg border bg-background px-2 text-sm"
            value={exportAs}
            onChange={(e) => setExportAs(e.target.value as "none" | ExportFormat)}
          >
            <option value="none">None</option>
            <option value="pdf">PDF</option>
            <option value="docx">DOCX</option>
          </select>
        </div>
        <Button variant="outline" size="sm" onClick={() => setPasting(true)}>
          Paste a job posting...
        </Button>
        {kind === "resume" && (
          <Button variant="outline" size="sm" disabled={importResume.isPending} onClick={() => importResume.mutate()}>
            {importResume.isPending ? "Importing..." : "Import resume..."}
          </Button>
        )}
      </div>
      <ErrorAlert
        error={cancelled(importResume.error) ?? jobs.error ?? drafts.error ?? ai.error ?? compare.error ?? remove.error}
      />
      {importResume.data && (
        <p role="status" className="text-sm">
          {importResume.data.items} lines imported. Approve the true ones under Knowledge below:
          a draft can only use approved lines.
        </p>
      )}
      {importResume.data && importResume.data.warnings.length > 0 && (
        <ul className="list-disc pl-5 text-sm">
          {importResume.data.warnings.map((w) => (
            <li key={w}>{w}</li>
          ))}
        </ul>
      )}

      {generate.isPending && (
        <div role="status" className="flex items-center gap-3 text-sm">
          <span>Generating. This reports no progress and can take a few minutes.</span>
          <Button size="sm" variant="outline" onClick={() => cancel.mutate()}>
            Cancel generation
          </Button>
        </div>
      )}
      {generate.data?.exported && (
        <p role="status" className="flex flex-wrap items-center gap-2 text-sm">
          <span>
            Exported to <code className="break-all select-text">{generate.data.exported}</code>
          </span>
          <CopyPath path={generate.data.exported} />
        </p>
      )}
      {generate.data && generate.data.export_skipped && !generate.data.exported && (
        <p role="status" className="text-sm font-medium text-destructive">
          The draft is saved but was not exported: a line, the name, a contact, or a heading is
          one an export refuses. Fix it in the editor, then export.
        </p>
      )}
      {generationError && (
        <>
          <ErrorAlert error={cancelled(generationError)} />
          {hint && <p className="text-sm">{hint}</p>}
        </>
      )}

      {suspendedOnly(ai.data) && (
        <p role="alert" className="text-sm">
          {endpointSuspended(ai.data) ? ENDPOINT_SUSPENDED : SUSPENDED} Open the AI screen to review it.
        </p>
      )}

      {drafts.isSuccess && !ai.isPending && !current && (
        <section className="grid gap-3 rounded-lg border p-4">
          <p className="text-sm">
            {kind === "resume" ? "No generated resume for this job yet." : "No cover letter for this job yet."}
          </p>
          <div className="flex gap-2">
            {backendEnabled(ai.data) && job ? (
              <>
                <Button disabled={generate.isPending} onClick={() => generate.mutate()}>
                  Generate
                </Button>
                <Button variant="outline" onClick={() => setEditor({ note: null })}>
                  Write by hand
                </Button>
              </>
            ) : (
              <Button onClick={() => setEditor({ note: null })}>Open editor</Button>
            )}
          </div>
        </section>
      )}

      {group && current && (
        <section className="grid gap-3" aria-label="Compare">
          <div className="flex flex-wrap items-end gap-3">
            {groups.length > 1 && (
              <div className="grid gap-1">
                <Label htmlFor="draft-select">Draft</Label>
                <select
                  id="draft-select"
                  className="h-8 rounded-lg border bg-background px-2 text-sm"
                  value={group[0].lineage_id}
                  onChange={(e) => {
                    setLineageChoice(e.target.value);
                    setDraftChoice(null);
                    setOriginalId(null);
                  }}
                >
                  {groups.map((g) => (
                    <option key={g[0].lineage_id} value={g[0].lineage_id}>
                      {latest(g).title}
                      {tied(g, job?.id ?? null) ? "" : g[0].job_text_id ? " (other job)" : " (no job)"}
                    </option>
                  ))}
                </select>
              </div>
            )}
            <div className="grid gap-1">
              <Label htmlFor="version-select">Version</Label>
              <select
                id="version-select"
                className="h-8 rounded-lg border bg-background px-2 text-sm"
                value={current.id}
                onChange={(e) => {
                  setDraftChoice(e.target.value);
                  setOriginalId(null);
                  setSelected(0);
                }}
              >
                {[...group].reverse().map((d) => (
                  <option key={d.id} value={d.id}>
                    {`v${d.version}${d.id === latest(group).id ? " (current)" : ""}, ${d.origin}`}
                  </option>
                ))}
              </select>
            </div>
            {kind === "resume" && (
              <div className="grid gap-1">
                <Label htmlFor="original-select">Compare against</Label>
                <select
                  id="original-select"
                  className="h-8 rounded-lg border bg-background px-2 text-sm"
                  value={originalId ?? ""}
                  onChange={(e) => setOriginalId(e.target.value || null)}
                >
                  <option value="">Newest uploaded resume</option>
                  {earlier.map((d) => (
                    <option key={d.id} value={d.id}>
                      {`v${d.version}, ${d.origin}`}
                    </option>
                  ))}
                </select>
              </div>
            )}
            <Button variant="outline" size="sm" onClick={() => setEditor({ note: null })}>
              Open editor
            </Button>
            {backendEnabled(ai.data) && job && current.job_text_id !== job.id && (
              <Button size="sm" disabled={generate.isPending} onClick={() => generate.mutate()}>
                Generate
              </Button>
            )}
          </div>

          {compare.isPending && <p className="text-sm text-muted-foreground">Checking the evidence...</p>}
          {compare.data && (
            <>
              <CompareTable
                compare={compare.data}
                kind={kind}
                posting={jobs.data?.find((j) => j.id === current.job_text_id) ?? null}
                selected={Math.min(selected, claims.length - 1)}
                onSelect={setSelected}
                onEdit={() => claim && setClaimEditing(true)}
              />
              {blocked > 0 && (
                <p role="status" className="text-sm font-medium text-destructive">
                  {blocked} claim(s) have no evidence. Export is blocked until it is fixed.
                </p>
              )}
              {blocked > claims.filter((c) => c.status !== "ok").length && (
                <p className="text-sm text-destructive">
                  The name, contact, or a heading states something JobPilot cannot put in a document
                  for you. Open the editor and change it.
                </p>
              )}
              <ErrorAlert error={exported.error} />
              {exported.data && (
                <p className="text-sm">
                  Exported to <code className="break-all select-text">{exported.data}</code>
                </p>
              )}
              <div className="flex flex-wrap items-center gap-2">
                <Button variant="outline" disabled={!claim} onClick={() => setClaimEditing(true)}>
                  Edit claim
                </Button>
                <Button variant="outline" disabled={!claim || remove.isPending} onClick={() => claim && remove.mutate(claim.id)}>
                  Remove claim
                </Button>
                <select
                  aria-label="Template"
                  className="h-8 rounded-lg border bg-background px-2 text-sm"
                  value={template}
                  onChange={(e) => setTemplate(e.target.value as Template)}
                >
                  <option value="classic">Classic</option>
                  <option value="compact">Compact</option>
                </select>
                {(["docx", "pdf"] as const).map((format) => (
                  <Button
                    key={format}
                    disabled={blocked > 0 || exported.isPending}
                    onClick={() => exported.mutate(format)}
                  >
                    {`Export ${format.toUpperCase()}`}
                  </Button>
                ))}
              </div>
            </>
          )}
        </section>
      )}

      <KnowledgeSection />

      {editor && (
        <DraftEditor
          kind={kind}
          lineageId={current && editor.note === null ? current.lineage_id : null}
          start={current && editor.note === null ? current.doc : null}
          note={editor.note}
          onClose={() => setEditor(null)}
          onSaved={(draft) => {
            setEditor(null);
            show(draft);
          }}
        />
      )}
      {claimEditing && claim && (
        <ClaimEditor
          claim={claim}
          onClose={() => setClaimEditing(false)}
          onSaved={(draft) => {
            setClaimEditing(false);
            show(draft);
          }}
        />
      )}
      {pasting && (
        <PostingForm
          onClose={() => setPasting(false)}
          onSaved={(posted) => {
            setPasting(false);
            setJobChoice(posted.id);
            setLineageChoice(null);
            void client.resetQueries({ queryKey: ["jobs"] });
          }}
        />
      )}
    </div>
  );
}
