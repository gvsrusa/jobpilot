import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { AiStatus, AuthNote, Job, JobDetail, SourceRow, SourceStatus } from "../lib/ipc";
import { backend, count, renderApp } from "../test-utils";

type Args = Record<string, unknown>;

const R1 = "Vendor is silent on third-party readers. Read under ruling R1.";

const status = (over: Partial<SourceStatus>): SourceStatus => ({
  name: "lever",
  label: "Lever",
  read: "allowed",
  can_fetch: true,
  operations: ["list_board"],
  attribution: null,
  date_read: "2026-11-01",
  target: "board",
  min_interval_secs: 2,
  per_day: 100,
  note: null,
  ...over,
});

const SOURCES: SourceStatus[] = [
  status({}),
  status({
    name: "remoteok",
    label: "RemoteOK",
    target: "feed",
    operations: ["list_feed"],
    attribution: "Remote OK",
  }),
  status({
    name: "greenhouse",
    label: "Greenhouse",
    read: "silent_read",
    note: R1,
  }),
  status({ name: "ashby", label: "Ashby", read: "silent_read", note: R1 }),
  status({ name: "workday", label: "Workday", read: "unknown", can_fetch: false, operations: ["import_pasted"], target: "none" }),
  status({ name: "indeed", label: "Indeed", read: "blocked", can_fetch: false, operations: ["import_pasted"], target: "none" }),
  status({ name: "linkedin", label: "LinkedIn", read: "user_assisted", can_fetch: false, operations: ["import_pasted"], target: "none" }),
];

const job = (over: Partial<Job> = {}): Job => ({
  id: "j1",
  job_text_id: "t1",
  connector: "lever",
  title: "Backend Engineer",
  company: "Acme",
  company_id: "c1",
  location: "Remote, PT",
  country: "PT",
  work_mode: "remote",
  employment_type: null,
  url: "https://example.com/1",
  attribution: null,
  posted_on: null,
  pay: null,
  seniority: null,
  skills: [],
  fields_by: "rules",
  duplicate_of: null,
  duplicates: 0,
  findings: 0,
  first_seen_at: "2026-11-01T10:00:00Z",
  last_seen_at: "2026-11-01T10:00:00Z",
  score: 82,
  stale: false,
  not_stated: [],
  ...over,
});

const detailOf = (j: Job, over: Partial<JobDetail> = {}): JobDetail => ({
  job: j,
  text: "Build services in Rust.",
  findings: [],
  explanation: {
    rows: [{ criterion: "skills", weight: 40, points: 34, note: "6 of 7" }],
    score: j.score,
    compared_weight: 40,
    matched_skills: [],
    missing_skills: ["Kubernetes"],
    by: "rules",
  },
  semantic: null,
  group: [],
  authorization: { kind: "not_stated", country: "PT" },
  mentions_authorization: false,
  notes: [],
  ...over,
});

function jobsBackend(jobs: Job[], overrides: Record<string, (a: Args) => unknown> = {}, sources: SourceRow[] = []) {
  return backend({
    jobs_list: () => jobs,
    job_sources_status: () => SOURCES,
    job_sources_list: () => sources,
    job_detail: ({ id }: Args) => detailOf(jobs.find((j) => j.id === id)!),
    ...overrides,
  });
}

async function openJobs() {
  const rendered = renderApp();
  await screen.findByRole("heading", { name: "Status" });
  await rendered.user.keyboard("{Control>}0{/Control}");
  await screen.findByRole("heading", { name: "Jobs" });
  return rendered;
}

describe("Jobs screen", () => {
  it("Ctrl+0 opens Jobs, and Ctrl+1 still opens Status", async () => {
    jobsBackend([]);
    const { user } = await openJobs();
    expect(screen.getByRole("button", { name: "Jobs" })).toHaveAttribute("aria-current", "page");
    await user.keyboard("{Control>}1{/Control}");
    await screen.findByRole("heading", { name: "Status" });
  });

  it("shows a src's verdict and no Fetch button when it cannot fetch", async () => {
    jobsBackend([]);
    await openJobs();
    const sources = await screen.findByRole("region", { name: "Sources" });
    for (const [name, read, note] of [
      ["Workday", "read: unknown", "No connector. Import instead."],
      ["Indeed", "read: blocked", "No connector. Import instead."],
      ["LinkedIn", "read: unknown", "User-assisted only."],
    ]) {
      const row = within(sources).getByRole("listitem", { name });
      expect(within(row).getByText(read)).toBeInTheDocument();
      expect(within(row).getByText(note)).toBeInTheDocument();
      expect(within(row).queryByRole("button")).toBeNull();
    }
    const lever = within(sources).getByRole("listitem", { name: "Lever" });
    expect(within(lever).getByText("read: allowed")).toBeInTheDocument();
    expect(within(lever).getByRole("button", { name: "Fetch Lever" })).toBeInTheDocument();
    expect(within(lever).getByLabelText("Board for Lever")).toBeInTheDocument();
    const feed = within(sources).getByRole("listitem", { name: "RemoteOK" });
    expect(within(feed).queryByLabelText(/Board for/)).toBeNull();
    expect(within(feed).getByRole("button", { name: "Fetch RemoteOK" })).toBeInTheDocument();
  });

  it("shows the R1 note and a Fetch button on Greenhouse and Ashby", async () => {
    jobsBackend([]);
    await openJobs();
    const sources = await screen.findByRole("region", { name: "Sources" });
    for (const name of ["Greenhouse", "Ashby"]) {
      const row = within(sources).getByRole("listitem", { name });
      expect(within(row).getByText("read: owner decision")).toBeInTheDocument();
      expect(within(row).getByText(R1)).toBeInTheDocument();
      expect(within(row).getByRole("button", { name: `Fetch ${name}` })).toBeInTheDocument();
    }
  });

  it("shows Remote OK on a card from that src, with no image", async () => {
    jobsBackend([job({ id: "r1", connector: "remoteok", attribution: "Remote OK", title: "Platform Dev" })]);
    const { container } = await openJobs();
    const row = await screen.findByRole("row", { name: /Platform Dev/ });
    expect(within(row).getByText(/Remote OK/)).toBeInTheDocument();
    expect(container.querySelector("img, picture, svg image")).toBeNull();
    expect(container.querySelector("main a")).toBeNull();
  });

  it("shows -- and the reason when a job has no score", async () => {
    jobsBackend([job({ id: "n1", score: null, title: "Data Eng", company: "Gamma" })]);
    await openJobs();
    const row = await screen.findByRole("row", { name: /Data Eng/ });
    expect(within(row).getByText("--")).toBeInTheDocument();
    expect(within(row).getByText("no score: nothing in your profile to compare")).toBeInTheDocument();
  });

  it("renders an HTML posting title as text", async () => {
    const title = '<img src=x onerror="alert(1)"><b>Boss</b>';
    jobsBackend([job({ title })]);
    const { container } = await openJobs();
    expect(await screen.findByText(title, { exact: false })).toBeInTheDocument();
    expect(container.querySelector("img, b")).toBeNull();
  });

  it("shows the empty state with the Sources list still present", async () => {
    jobsBackend([]);
    await openJobs();
    expect(
      await screen.findByText("No jobs yet. Import a posting, or fetch from a source marked allowed."),
    ).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Sources" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Import a posting" })).toBeInTheDocument();
  });

  it("shows a refused fetch on its src row in role=alert", async () => {
    const row: SourceRow = { id: "s1", connector: "greenhouse", target: "acme", query: "", cursor: 0, created_at: "t" };
    const b = jobsBackend([], {
      jobs_fetch: () => ({
        sources: [{ source_id: "s1", connector: "greenhouse", target: "acme", added: 0, updated: 0, skipped: 0, reason: "policy_unknown" }],
        grouped: 0,
      }),
    }, [row]);
    const { user } = await openJobs();
    const greenhouse = await screen.findByRole("listitem", { name: "Greenhouse" });
    await user.click(within(greenhouse).getByRole("button", { name: "Fetch Greenhouse" }));
    const alert = await within(greenhouse).findByRole("alert");
    expect(alert).toHaveTextContent("Refused by policy: unknown");
    expect(count(b, "jobs_fetch")).toBe(1);
    expect(b.calls.find((c) => c.cmd === "jobs_fetch")!.args).toEqual({ srcIds: ["s1"] });
  });

  it("moves the table by arrows and opens detail with Enter", async () => {
    const jobs = [
      job({ id: "a", title: "Alpha Role" }),
      job({ id: "b", title: "Beta Role" }),
      job({ id: "c", title: "Gamma Role" }),
    ];
    jobsBackend(jobs);
    const { user } = await openJobs();
    const first = await screen.findByRole("row", { name: /Alpha Role/ });
    first.focus();
    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("row", { name: /Beta Role/ })).toHaveFocus();
    await user.keyboard("{End}");
    expect(screen.getByRole("row", { name: /Gamma Role/ })).toHaveFocus();
    await user.keyboard("{Home}");
    expect(screen.getByRole("row", { name: /Alpha Role/ })).toHaveFocus();
    await user.keyboard("{ArrowDown}{Enter}");
    await screen.findByRole("heading", { name: /Beta Role/ });
    expect(screen.getByText("Build services in Rust.")).toBeInTheDocument();
    expect(screen.getByText("Missing skills: Kubernetes")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: "Back to Jobs" })).toBeInTheDocument());
  });

  it("says a pay with no period has an unknown period", async () => {
    const j = job({ pay: { min: 70000, max: 80000, currency: "EUR", period: null } });
    jobsBackend([j]);
    const { user } = await openJobs();
    const row = await screen.findByRole("row", { name: /Backend Engineer/ });
    row.focus();
    await user.keyboard("{Enter}");
    expect(await screen.findByText(/70000-80000 EUR, period not stated/)).toBeInTheDocument();
  });
});

const AI_OFF: AiStatus = {
  ollama: { state: "not_running", url: "http://127.0.0.1:11434" },
  claude_code: { available: false, path: null, consent: null, suspended: false },
  external: [],
  register_date: "2026-11-01",
  hardware: { total_memory: 16 * 2 ** 30, cpus: 8, gpu: "no GPU found", free_disk: 100 * 2 ** 30 },
  order: ["ollama"],
  enabled: { ollama: true, claude_code: false },
  drafting_model: null,
  embedding_model: null,
};

describe("Jobs screen scoring and copy", () => {
  it("scores with rules on load, so a full profile shows a number", async () => {
    let scored = false;
    const b = jobsBackend([], {
      jobs_analyze: () => {
        scored = true;
        return { scored: 1, parsed: 0, evaluated: 0, dropped: 0, reason: null };
      },
      jobs_list: () => [job({ id: "s1", title: "Scored Role", score: scored ? 77 : null })],
    });
    await openJobs();
    const row = await screen.findByRole("row", { name: /Scored Role/ });
    await waitFor(() => expect(within(row).getByText("77")).toBeInTheDocument());
    expect(within(row).queryByText("--")).toBeNull();
    expect(b.calls.find((c) => c.cmd === "jobs_analyze")!.args).toEqual({ ids: [], ai: false });
  });

  it("scores again after a fetch", async () => {
    const row: SourceRow = { id: "s1", connector: "lever", target: "acme", query: "", cursor: 0, created_at: "t" };
    const b = jobsBackend([], {
      jobs_fetch: () => ({
        sources: [{ source_id: "s1", connector: "lever", target: "acme", added: 1, updated: 0, skipped: 0, reason: null, next_at: null }],
        grouped: 0,
      }),
    }, [row]);
    const { user } = await openJobs();
    const lever = await screen.findByRole("listitem", { name: "Lever" });
    await waitFor(() => expect(count(b, "jobs_analyze")).toBe(1));
    await user.click(within(lever).getByRole("button", { name: "Fetch Lever" }));
    await waitFor(() => expect(count(b, "jobs_analyze")).toBe(2));
  });

  it("shows the true next fetch time from the stored hold", async () => {
    const row: SourceRow = { id: "s1", connector: "lever", target: "acme", query: "", cursor: 0, created_at: "t" };
    const next = "2026-11-02T16:30:00Z";
    jobsBackend([], {
      jobs_fetch: () => ({
        sources: [{ source_id: "s1", connector: "lever", target: "acme", added: 0, updated: 0, skipped: 0, reason: "rate_limited", next_at: next }],
        grouped: 0,
      }),
    }, [row]);
    const { user } = await openJobs();
    const lever = await screen.findByRole("listitem", { name: "Lever" });
    await user.click(within(lever).getByRole("button", { name: "Fetch Lever" }));
    const alert = await within(lever).findByRole("alert");
    const when = new Date(next).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
    expect(alert).toHaveTextContent(`Rate limited. Next fetch at ${when}.`);
  });

  it("says what Remove really does to the posting text", async () => {
    const j = job();
    jobsBackend([j], { ai_status: () => AI_OFF });
    const { user } = await openJobs();
    const r = await screen.findByRole("row", { name: /Backend Engineer/ });
    r.focus();
    await user.keyboard("{Enter}");
    await user.click(await screen.findByRole("button", { name: "Remove job" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("The posting text is deleted with the job, unless a draft cites it.");
    expect(dialog).not.toHaveTextContent("stays in your data");
  });
});

const AI_ON: AiStatus = {
  ...AI_OFF,
  ollama: { state: "running", url: "http://127.0.0.1:11434", version: "1", models: [] },
};

async function openDetail(j: Job, over: Partial<JobDetail> = {}, overrides: Record<string, (a: Args) => unknown> = {}) {
  const b = jobsBackend([j], {
    job_detail: () => detailOf(j, over),
    ai_status: () => AI_ON,
    ...overrides,
  });
  const rendered = await openJobs();
  const row = await screen.findByRole("row", { name: new RegExp(j.title) });
  row.focus();
  await rendered.user.keyboard("{Enter}");
  await screen.findByRole("button", { name: "Back to Jobs" });
  return { ...rendered, b };
}

const lastList = (b: ReturnType<typeof jobsBackend>) => b.calls.filter((c) => c.cmd === "jobs_list").slice(-1)[0];

describe("Job detail", () => {
  it("lists every criterion with weight and points", async () => {
    await openDetail(job({ url: "https://remoteok.com/x" }), {
      explanation: {
        rows: [
          { criterion: "skills", weight: 40, points: 34, note: "6 of 7" },
          { criterion: "seniority", weight: 20, points: 18, note: "senior vs senior" },
          { criterion: "location", weight: 20, points: 20, note: "remote" },
          { criterion: "pay", weight: 20, points: null, note: "posting names no currency" },
        ],
        score: 90,
        compared_weight: 80,
        matched_skills: [],
        missing_skills: [],
        by: "rules",
      },
    });
    const table = screen.getByRole("table", { name: "Score explanation" });
    for (const cells of [
      ["skills", "40", "34", "6 of 7"],
      ["seniority", "20", "18", "senior vs senior"],
      ["location", "20", "20", "remote"],
      ["pay", "20", "not compared", "posting names no currency"],
    ]) {
      const row = within(table).getByRole("row", { name: new RegExp(`^${cells[0]}`, "i") });
      expect(within(row).getAllByRole("cell").map((c) => c.textContent)).toEqual(cells);
    }
    expect(screen.getByText("Compared weight: 80 of 100")).toBeInTheDocument();
  });

  it("never shows eligible or ineligible, for each authz state", async () => {
    const states: [AuthNote, boolean, string][] = [
      [{ kind: "not_stated", country: "PT" }, false, "PT: not stated by you. Not checked."],
      [
        { kind: "stated", country: "PT", authorized: "yes", needs_sponsorship: "no" },
        false,
        "You stated: authorized yes, needs sponsorship no for PT. Not checked against this posting.",
      ],
      [{ kind: "locked" }, false, "Locked, see Recovery."],
      [{ kind: "no_country" }, false, "No country on this posting. Not checked."],
      [{ kind: "not_stated", country: "PT" }, true, "This posting mentions work authz. Read it yourself."],
    ];
    for (const [authorization, mentions_authorization, line] of states) {
      const { container, unmount } = await openDetail(job(), { authorization, mentions_authorization });
      expect(screen.getByText(line)).toBeInTheDocument();
      expect(container.textContent ?? "").not.toMatch(/eligible|ineligible|likely/i);
      const lines = screen.getAllByText(/Not checked|Locked, see Recovery/);
      expect(lines).toHaveLength(1);
      unmount();
    }
  });

  it("shows Locked, see Recovery, for a locked note", async () => {
    await openDetail(job(), { authorization: { kind: "locked" } });
    expect(screen.getByText("Locked, see Recovery.")).toBeInTheDocument();
    expect(screen.queryByText(/not stated by you/i)).toBeNull();
  });

  it("keeps the rules score visible when AI evaluation fails", async () => {
    const { user } = await openDetail(job({ score: 82 }), {}, {
      jobs_analyze: () => ({ scored: 1, parsed: 0, evaluated: 0, dropped: 0, reason: "consent_missing" }),
    });
    await user.click(screen.getByRole("button", { name: "Add AI evaluation" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("code: consent_missing");
    expect(screen.getByText("Score 82 / 100")).toBeInTheDocument();
    expect(screen.getByText("Scored by: rules.")).toBeInTheDocument();
  });

  it("hides Add AI evaluation with no eligible provider", async () => {
    await openDetail(job(), {}, { ai_status: () => AI_OFF });
    expect(screen.queryByRole("button", { name: "Add AI evaluation" })).toBeNull();
    expect(screen.getByText("Scored by: rules.")).toBeInTheDocument();
  });

  it("renders posting text inert, with no anchors or images", async () => {
    const text = 'Apply <a href="https://evil.example/x">here</a> <img src="https://evil.example/p.png"> http://evil.example/y';
    const { container } = await openDetail(job({ url: "https://example.com/1" }), {
      text,
      findings: [{ kind: "hidden_html", excerpt: '<a href="https://evil.example/z">z</a>' }],
    });
    expect(screen.getByText(text)).toBeInTheDocument();
    expect(screen.getByText("Untrusted text. Shown as plain text.")).toBeInTheDocument();
    expect(container.querySelector("a, img, picture, svg image")).toBeNull();
    expect(container.querySelector("[href], [src]")).toBeNull();
  });

  it("opens the source link through job_open with the host in its label", async () => {
    const { user, b } = await openDetail(job({ id: "r1", connector: "remoteok", attribution: "Remote OK", url: "https://remoteok.com/remote-jobs/1" }), {}, {
      job_open: () => null,
    });
    expect(screen.getByText(/^Source: Remote OK \(/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /link.*remoteok\.com/ }));
    expect(b.calls.find((c) => c.cmd === "job_open")!.args).toEqual({ id: "r1" });
  });

  it("shows the AI evaluation as its own panel with cited strengths and dropped", async () => {
    await openDetail(job(), {
      semantic: {
        fit: 77,
        summary: "Good match on Rust.",
        strengths: [{ text: "Built Rust services", evidence_ids: ["e1"] }],
        gaps: ["No Kubernetes"],
        dropped: 2,
        provider: "ollama",
        at: "2026-11-01T10:00:00Z",
      },
    });
    const panel = screen.getByRole("region", { name: "AI evaluation" });
    expect(within(panel).getByText(/fit 77/)).toBeInTheDocument();
    expect(within(panel).getByText("Good match on Rust.")).toBeInTheDocument();
    expect(within(panel).getByText(/Built Rust services/)).toHaveTextContent("e1");
    expect(within(panel).getByText(/No Kubernetes/)).toBeInTheDocument();
    expect(within(panel).getByText(/ollama/)).toBeInTheDocument();
    expect(within(panel).getByText(/2026-11-01/)).toBeInTheDocument();
    expect(within(panel).getByText("dropped: 2")).toBeInTheDocument();
    expect(screen.getByText("Scored by: rules.")).toBeInTheDocument();
  });

  it("says nothing of duplicates for a job with none", async () => {
    await openDetail(job());
    expect(screen.queryByText(/Same job on/)).toBeNull();
    expect(screen.queryByRole("button", { name: "Not a duplicate" })).toBeNull();
  });

  it("says 1 other source for a pair", async () => {
    await openDetail(job(), { group: [job({ id: "j2", connector: "wwr" })] });
    expect(screen.getByText(/Same job on 1 other source\./)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Not a duplicate" })).toBeInTheDocument();
  });

  it("lists findings as a labelled list", async () => {
    await openDetail(job(), { findings: [{ kind: "instruction", excerpt: "ignore previous" }] });
    const list = screen.getByRole("list", { name: "Findings" });
    expect(within(list).getByText(/instruction/)).toHaveTextContent("ignore previous");
  });

  it("Prepare opens Resumes on this job's text", async () => {
    const { user } = await openDetail(job({ job_text_id: "t9" }), {}, { resume_list: () => [], job_list: () => [{ id: "t9", title: "Backend Engineer, Acme", normalized: "x", findings: [] }] });
    await user.click(screen.getByRole("button", { name: "Prepare..." }));
    await screen.findByRole("heading", { name: "Resumes" });
    expect(await screen.findByLabelText("Job posting")).toHaveValue("t9");
  });

  it("Alt+Left returns to the same row", async () => {
    const jobs = [job({ id: "a", title: "Alpha Role" }), job({ id: "b", title: "Beta Role" })];
    jobsBackend(jobs, { ai_status: () => AI_ON });
    const { user } = await openJobs();
    const second = await screen.findByRole("row", { name: /Beta Role/ });
    second.focus();
    await user.keyboard("{Enter}");
    await screen.findByRole("button", { name: "Back to Jobs" });
    await user.keyboard("{Alt>}{ArrowLeft}{/Alt}");
    await waitFor(() => expect(screen.getByRole("row", { name: /Beta Role/ })).toHaveFocus());
  });

  it("moves focus to the detail heading on Enter", async () => {
    await openDetail(job({ title: "Focus Role" }));
    await waitFor(() => expect(screen.getByRole("heading", { name: /Focus Role/ }).parentElement).toHaveFocus());
  });

  it("keeps the filters through Back", async () => {
    const b = jobsBackend([job({ title: "Filtered Role" })], { ai_status: () => AI_ON });
    const { user } = await openJobs();
    await user.type(await screen.findByLabelText("Country"), "PT");
    await user.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() =>
      expect((lastList(b).args.filter as { country: string }).country).toBe("PT"),
    );
    const row = await screen.findByRole("row", { name: /Filtered Role/ });
    row.focus();
    await user.keyboard("{Enter}");
    await user.click(await screen.findByRole("button", { name: "Back to Jobs" }));
    expect(await screen.findByLabelText("Country")).toHaveValue("PT");
    await screen.findByRole("row", { name: /Filtered Role/ });
    expect((lastList(b).args.filter as { country: string }).country).toBe("PT");
  });
});
