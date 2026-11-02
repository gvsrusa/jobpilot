import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ErrorAlert } from "@/components/ErrorAlert";
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
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Textarea } from "@/components/ui/textarea";
import { JobDetailView, noScoreText } from "@/views/JobDetail";
import {
  ipc,
  toAppError,
  type Job,
  type JobFilter,
  type PayPeriod,
  type SourceResult,
  type SourceStatus,
  type WorkMode,
} from "@/lib/ipc";

// Every posting field here comes from outside. It renders as text: no anchors, no images,
// no HTML. A link is shown as its characters and opened by Rust, from the stored job.

// ponytail: the list shows the first 200 jobs, like the CLI; there is no paging yet.
const LIMIT = 200;
const EMPTY_FILTER: JobFilter = { country: null, work_mode: null, min_pay: null, query: null, include_duplicates: false };
const PERIODS: PayPeriod[] = ["hour", "day", "week", "month", "year"];

const selectClass =
  "h-8 rounded-lg border border-input bg-transparent px-2.5 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50";

const sourceLabel = (statuses: SourceStatus[], connector: string) =>
  connector === "import" ? "Imported" : (statuses.find((s) => s.name === connector)?.label ?? connector);

/** What a source row says about a stop. `code` is shown beside it. */
function problem(code: string, nextAt: string | null, message?: string): string {
  if (code.startsWith("policy_")) return `Refused by policy: ${code.slice(7).replace(/_/g, " ")}`;
  if (code === "rate_limited") {
    // The time comes from the stored hold, so a long Retry-After shows as long.
    const next = nextAt ? new Date(nextAt) : null;
    return next && !Number.isNaN(next.getTime())
      ? `Rate limited. Next fetch at ${next.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })}.`
      : "Rate limited. Try again later.";
  }
  const known: Record<string, string> = {
    source_blocked: "The source refused or showed a challenge page. JobPilot stops there.",
    source_not_found: "The source says that board or feed does not exist. Check the name.",
    bad_feed: "The source answered in a shape JobPilot does not read.",
    network: "The source could not be reached.",
  };
  return known[code] ?? message ?? `Stopped: ${code}`;
}

function ImportDialog({ onClose, onSaved }: { onClose(): void; onSaved(): void }) {
  const client = useQueryClient();
  const [title, setTitle] = useState("");
  const [company, setCompany] = useState("");
  const [link, setLink] = useState("");
  const [text, setText] = useState("");
  const save = useMutation({
    mutationFn: () => ipc.jobImport({ title, company, url: link.trim() || null, text }),
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: ["jobs"] });
      onSaved();
      onClose();
    },
  });
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent showCloseButton={false}>
        <DialogHeader>
          <DialogTitle>Import a posting</DialogTitle>
          <DialogDescription>
            The text is kept as data and the link is kept, never requested. JobPilot never follows instructions written
            in a posting.
          </DialogDescription>
        </DialogHeader>
        <form
          className="grid gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            save.mutate();
          }}
        >
          <div className="grid gap-1.5">
            <Label htmlFor="import-title">Title</Label>
            <Input id="import-title" value={title} onChange={(e) => setTitle(e.target.value)} />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="import-company">Company (optional)</Label>
            <Input id="import-company" value={company} onChange={(e) => setCompany(e.target.value)} />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="import-link">Link (optional)</Label>
            <Input id="import-link" value={link} onChange={(e) => setLink(e.target.value)} />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="import-text">Posting text</Label>
            <Textarea id="import-text" value={text} onChange={(e) => setText(e.target.value)} />
          </div>
          <ErrorAlert error={save.error} />
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" disabled={save.isPending || title.trim() === "" || text.trim() === ""}>
              {save.isPending ? "Saving..." : "Save posting"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

type FilterForm = { country: string; mode: string; amount: string; currency: string; period: PayPeriod };
const EMPTY_FORM: FilterForm = { country: "", mode: "", amount: "", currency: "", period: "year" };

/** The form lives in the parent, so Back from a job finds the filters as they were left. */
function Filters({
  form,
  onForm,
  onApply,
}: {
  form: FilterForm;
  onForm(form: FilterForm): void;
  onApply(filter: JobFilter): void;
}) {
  const { country, mode, amount, currency, period } = form;
  const setCountry = (country: string) => onForm({ ...form, country });
  const setMode = (mode: string) => onForm({ ...form, mode });
  const setAmount = (amount: string) => onForm({ ...form, amount });
  const setCurrency = (currency: string) => onForm({ ...form, currency });
  const setPeriod = (period: PayPeriod) => onForm({ ...form, period });
  const pay = Number(amount);
  const wantsPay = amount.trim() !== "";
  const invalid = wantsPay && (!Number.isInteger(pay) || pay <= 0 || currency.trim().length !== 3);
  return (
    <form
      aria-label="Filters"
      className="grid gap-1"
      onSubmit={(e) => {
        e.preventDefault();
        if (invalid) return;
        onApply({
          ...EMPTY_FILTER,
          country: country.trim() || null,
          work_mode: (mode || null) as WorkMode | null,
          min_pay: wantsPay ? { amount: pay, currency: currency.trim().toUpperCase(), period } : null,
        });
      }}
    >
      <div className="flex flex-wrap items-end gap-3">
        <div className="grid gap-1">
          <Label htmlFor="f-country">Country</Label>
          <Input id="f-country" className="w-24" value={country} onChange={(e) => setCountry(e.target.value)} />
        </div>
        <div className="grid gap-1">
          <Label htmlFor="f-mode">Mode</Label>
          <select id="f-mode" className={selectClass} value={mode} onChange={(e) => setMode(e.target.value)}>
            <option value="">Any</option>
            <option value="remote">Remote</option>
            <option value="hybrid">Hybrid</option>
            <option value="onsite">On site</option>
          </select>
        </div>
        <div className="grid gap-1">
          <Label htmlFor="f-pay">Min salary</Label>
          <Input id="f-pay" className="w-28" inputMode="numeric" value={amount} onChange={(e) => setAmount(e.target.value)} />
        </div>
        <div className="grid gap-1">
          <Label htmlFor="f-currency">Currency</Label>
          <Input id="f-currency" className="w-20" maxLength={3} value={currency} onChange={(e) => setCurrency(e.target.value)} />
        </div>
        <div className="grid gap-1">
          <Label htmlFor="f-period">Per</Label>
          <select id="f-period" className={selectClass} value={period} onChange={(e) => setPeriod(e.target.value as PayPeriod)}>
            {PERIODS.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
        </div>
        <Button type="submit" disabled={invalid}>
          Apply
        </Button>
      </div>
      <p className="text-sm text-muted-foreground">
        Filters are exact rules. A job that does not state a field stays listed and says so. Score uses your stated
        preferences.
        {invalid && " A minimum salary needs a whole amount and a 3-letter currency."}
      </p>
    </form>
  );
}

function JobTable({
  jobs,
  statuses,
  focusId,
  onOpen,
  onImport,
}: {
  jobs: Job[];
  statuses: SourceStatus[];
  focusId: string | null;
  onOpen(id: string): void;
  onImport(): void;
}) {
  const body = useRef<HTMLTableSectionElement>(null);
  const [active, setActive] = useState(() => Math.max(0, jobs.findIndex((j) => j.id === focusId)));
  const row = Math.min(active, jobs.length - 1);
  useEffect(() => {
    if (focusId) body.current?.querySelector<HTMLElement>(`[data-job="${CSS.escape(focusId)}"]`)?.focus();
  }, [focusId]);
  const move = (to: number) => {
    setActive(to);
    body.current?.querySelectorAll<HTMLElement>("tr")[to]?.focus();
  };
  const onKey = (e: KeyboardEvent<HTMLTableSectionElement>) => {
    const at = Number((e.target as HTMLElement).closest("tr")?.dataset.index ?? row);
    const keys: Record<string, number> = {
      ArrowDown: Math.min(at + 1, jobs.length - 1),
      ArrowUp: Math.max(at - 1, 0),
      Home: 0,
      End: jobs.length - 1,
    };
    if (e.key in keys) {
      e.preventDefault();
      move(keys[e.key]);
    } else if (e.key === "Enter" && jobs[at]) {
      e.preventDefault();
      onOpen(jobs[at].id);
    } else if (e.key === "i" && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      onImport();
    }
  };
  return (
    <Table role="grid" aria-label="Jobs">
      <TableHeader>
        <TableRow>
          <TableHead>Score</TableHead>
          <TableHead>Title / Company</TableHead>
          <TableHead>Where</TableHead>
          <TableHead>Source</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody ref={body} onKeyDown={onKey}>
        {jobs.map((job, i) => (
          <TableRow
            key={job.id}
            data-job={job.id}
            data-index={i}
            tabIndex={i === row ? 0 : -1}
            onFocus={() => setActive(i)}
            onDoubleClick={() => onOpen(job.id)}
            className="focus-visible:ring-3 focus-visible:ring-ring/50 outline-none"
          >
            <TableCell className="align-top">
              <div>{job.score ?? "--"}</div>
              {job.stale && <div className="text-xs text-muted-foreground">Out of date</div>}
            </TableCell>
            <TableCell className="whitespace-normal">
              <div>
                {job.title}
                {job.company ? `, ${job.company}` : ""}
              </div>
              {job.score === null && <div className="text-xs text-muted-foreground">{noScoreText(job)}</div>}
              {job.not_stated.length > 0 && (
                <div className="text-xs text-muted-foreground">not stated: {job.not_stated.join(", ")}</div>
              )}
            </TableCell>
            <TableCell className="align-top">{[job.location, job.country].filter(Boolean).join(", ")}</TableCell>
            <TableCell className="align-top">
              <div>{sourceLabel(statuses, job.connector)}</div>
              {job.attribution && <div className="text-xs text-muted-foreground">{job.attribution}</div>}
              {job.duplicates > 0 && (
                <div className="text-xs text-muted-foreground">
                  +{job.duplicates} other {job.duplicates === 1 ? "source" : "sources"}
                </div>
              )}
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

type Outcome = { result?: SourceResult; error?: unknown };

function SourceItem({ status, existing, onFetched }: { status: SourceStatus; existing: string; onFetched(): void }) {
  const client = useQueryClient();
  const [typed, setTyped] = useState<string | null>(null);
  const board = typed ?? existing;
  const [outcome, setOutcome] = useState<Outcome>({});
  const fetchNow = useMutation({
    mutationFn: async (): Promise<SourceResult> => {
      const target = status.target === "board" ? board.trim() : "";
      const rows = await ipc.jobSourcesList();
      const row = rows.find((r) => r.connector === status.name && r.target === target) ?? (await ipc.jobSrcAdd(status.name, target, ""));
      const report = await ipc.jobsFetch([row.id]);
      return report.sources[0];
    },
    onMutate: () => setOutcome({}),
    onSuccess: (result) => setOutcome({ result }),
    onError: (error) => setOutcome({ error }),
    onSettled: async () => {
      await client.invalidateQueries({ queryKey: ["jobs"] });
      await client.invalidateQueries({ queryKey: ["job-sources"] });
      onFetched();
    },
  });
  const read = { allowed: "allowed", silent_read: "ruling R1", blocked: "blocked", user_assisted: "unknown", unknown: "unknown" }[status.read];
  const closed = status.read === "user_assisted" ? "User-assisted only." : "No connector. Import instead.";
  const stop = outcome.result?.reason ?? null;
  const thrown = outcome.error ? toAppError(outcome.error) : null;
  const code = stop ?? thrown?.code ?? null;
  return (
    <li aria-label={status.label} className="grid gap-1 border-b py-2 text-sm">
      <div className="flex flex-wrap items-center gap-3">
        <span className="w-40 font-medium">{status.label}</span>
        <span>read: {read}</span>
        {status.can_fetch ? (
          <>
            {status.target === "board" && (
              <Input
                aria-label={`Board for ${status.label}`}
                className="w-48"
                placeholder="Board"
                value={board}
                onChange={(e) => setTyped(e.target.value)}
              />
            )}
            <Button
              type="button"
              variant="outline"
              aria-label={`Fetch ${status.label}`}
              disabled={fetchNow.isPending || (status.target === "board" && board.trim() === "")}
              onClick={() => fetchNow.mutate()}
            >
              {fetchNow.isPending ? "Fetching..." : "Fetch"}
            </Button>
            {status.attribution && <span className="text-muted-foreground">attribution shown</span>}
          </>
        ) : (
          <span className="text-muted-foreground">{closed}</span>
        )}
      </div>
      {status.note && <p className="text-muted-foreground">{status.note}</p>}
      {code && (
        <p role="alert" className="text-destructive">
          {problem(code, outcome.result?.next_at ?? null, thrown?.message)} <span className="font-mono text-xs">code: {code}</span>
        </p>
      )}
      {outcome.result && !stop && (
        <p role="status" className="text-muted-foreground">
          {outcome.result.added} added, {outcome.result.updated} updated, {outcome.result.skipped} skipped.
        </p>
      )}
    </li>
  );
}

function Sources({ statuses, onFetched }: { statuses: SourceStatus[]; onFetched(): void }) {
  const rows = useQuery({ queryKey: ["job-sources"], queryFn: ipc.jobSourcesList });
  return (
    <section aria-labelledby="sources-heading" className="grid gap-1">
      <h2 id="sources-heading" className="text-sm font-semibold">
        Sources
      </h2>
      <ErrorAlert error={rows.error} />
      <ul>
        {statuses.map((s) => (
          <SourceItem
            key={s.name}
            status={s}
            existing={rows.data?.find((r) => r.connector === s.name)?.target ?? ""}
            onFetched={onFetched}
          />
        ))}
      </ul>
    </section>
  );
}

function List({
  filter,
  onFilter,
  form,
  onForm,
  focusId,
  onOpen,
}: {
  filter: JobFilter;
  onFilter(filter: JobFilter): void;
  form: FilterForm;
  onForm(form: FilterForm): void;
  focusId: string | null;
  onOpen(id: string): void;
}) {
  const [importing, setImporting] = useState(false);
  const jobs = useQuery({ queryKey: ["jobs", filter], queryFn: () => ipc.jobsList(filter, LIMIT) });
  const statuses = useQuery({ queryKey: ["job-sources-status"], queryFn: ipc.jobSourcesStatus });
  // The rules score needs no model. It runs once on load, and only when a listed job has no
  // stored result or a stale one, so browsing writes no run. A fetch or an import runs it again.
  const client = useQueryClient();
  const score = useMutation({
    mutationFn: () => ipc.jobsAnalyze([], false),
    onSettled: () => client.invalidateQueries({ queryKey: ["jobs"] }),
  });
  const rescore = score.mutate;
  const toScore = jobs.isSuccess && jobs.data.some((job) => !job.scored || job.stale);
  const asked = useRef(false);
  useEffect(() => {
    if (toScore && !asked.current) {
      asked.current = true;
      rescore();
    }
  }, [toScore, rescore]);
  // A busy task slot or the pause only means this pass waits for the next load.
  const scoreError = score.error && !waits(score.error) ? score.error : null;
  return (
    <div className="grid gap-4">
      <div className="flex items-center justify-between">
        <PageHeading>Jobs</PageHeading>
        <Button type="button" onClick={() => setImporting(true)}>
          Import a posting
        </Button>
      </div>
      <Filters form={form} onForm={onForm} onApply={onFilter} />
      <ErrorAlert error={jobs.error ?? statuses.error ?? scoreError} />
      {jobs.isSuccess && jobs.data.length === 0 && (
        <p className="text-sm text-muted-foreground">
          No jobs yet. Import a posting, or fetch from a source marked allowed.
        </p>
      )}
      {jobs.isSuccess && jobs.data.length > 0 && (
        <>
          <p className="text-xs text-muted-foreground">Job text came from outside and is shown as plain text.</p>
          <JobTable
            jobs={jobs.data}
            statuses={statuses.data ?? []}
            focusId={focusId}
            onOpen={onOpen}
            onImport={() => setImporting(true)}
          />
          {jobs.data.length === LIMIT && (
            <p className="text-xs text-muted-foreground">
              The first {LIMIT} jobs are shown. Narrow with the country, mode, or pay filters.
            </p>
          )}
        </>
      )}
      {statuses.isSuccess && <Sources statuses={statuses.data} onFetched={() => rescore()} />}
      {importing && <ImportDialog onClose={() => setImporting(false)} onSaved={() => rescore()} />}
    </div>
  );
}

const waits = (error: unknown) => {
  const { code, message } = toAppError(error);
  return code === "paused" || message.includes("another_task_is_running");
};

export function JobsView() {
  const [openId, setOpenId] = useState<string | null>(null);
  // The row to focus when the list comes back, and the filters it had.
  const [lastId, setLastId] = useState<string | null>(null);
  const [filter, setFilter] = useState(EMPTY_FILTER);
  const [form, setForm] = useState(EMPTY_FORM);
  if (openId) {
    return (
      <JobDetailView
        id={openId}
        onBack={() => {
          setLastId(openId);
          setOpenId(null);
        }}
      />
    );
  }
  return <List filter={filter} onFilter={setFilter} form={form} onForm={setForm} focusId={lastId} onOpen={setOpenId} />;
}
