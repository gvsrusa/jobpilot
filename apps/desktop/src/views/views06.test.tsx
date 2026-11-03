import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { Application, ApplyItem, ApplyRunView, ItemCheck, Material, Preflight } from "../lib/ipc";
import { useUiStore } from "../lib/store";
import { ApplyRun } from "./ApplyRun";
import { backend, count, renderApp } from "../test-utils";

type Args = Record<string, unknown>;

const FIXED =
  "Runs on your own accounts, in a visible browser. Once started, it submits with no check per application. It stops for sign-in, CAPTCHA, codes, identity checks, and legal declarations. A site may restrict an account that uses automation.";

const application = (over: Partial<Application>): Application => ({
  id: "a1",
  company: "Acme",
  role: "Engineer",
  job_text_id: null,
  resume_draft_id: "d1",
  letter_draft_id: "d2",
  link: "https://boards.greenhouse.io/acme/jobs/1",
  country: "US",
  notes: "",
  status: "saved",
  applied_on: null,
  created_at: "2026-11-01T10:00:00Z",
  updated_at: "2026-11-01T10:00:00Z",
  ...over,
});

const check = (over: Partial<ItemCheck> = {}): ItemCheck => ({
  application_id: "a1",
  company: "Acme",
  role: "Engineer",
  host: "boards.greenhouse.io",
  country: "US",
  resume: "ready",
  letter: "missing",
  answers: 4,
  stale_left_out: 0,
  sealed: 2,
  authorization: "stated",
  blocks: [],
  ...over,
});

const preflight = (over: Partial<Preflight> = {}): Preflight => ({
  items: [check()],
  runtime: "ready",
  claude: "/usr/local/bin/claude",
  consent_missing: [],
  sealed_total: 2,
  sensitive_consent: true,
  ...over,
});

const item = (over: Partial<ApplyItem> = {}): ApplyItem => ({
  id: "i1",
  run_id: "run1",
  application_id: "a1",
  position: 0,
  host: "boards.greenhouse.io",
  state: "active",
  submit_clicks: 0,
  auth_released: false,
  resume_path: null,
  letter_path: null,
  policy_note: null,
  confirmation_text: null,
  confirmation_url: null,
  status_written: false,
  reason: null,
  released_answers: [],
  ...over,
});

const run = (over: Partial<ApplyRunView> = {}): ApplyRunView => ({
  id: "run1",
  mode: "claude_code",
  state: "running",
  items: [item()],
  active_pause: null,
  created_at: "2026-11-02T10:00:00.000Z",
  ended_at: null,
  ...over,
});

const APPS = [application({}), application({ id: "a2", company: "Globex", status: "applied" })];

async function openPreflight(overrides: Record<string, (args: Args) => unknown>) {
  const api = backend({ app_list: () => APPS, resume_list: () => [], ...overrides });
  const { user } = renderApp();
  await screen.findByRole("heading", { name: "Status" });
  await user.click(screen.getByRole("button", { name: "Applications" }));
  await user.click(await screen.findByRole("checkbox", { name: "Select Acme" }));
  expect(screen.queryByRole("checkbox", { name: "Select Globex" })).not.toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Apply with Claude Code..." }));
  return { api, user };
}

/** The run screen on its own, with a run already started. */
function showRun(view: ApplyRunView, overrides: Record<string, (args: Args) => unknown> = {}) {
  const api = backend({ apply_view: () => view, ...overrides });
  useUiStore.setState({ applyRun: view.id });
  const rendered = renderApp(<ApplyRun applicationIds={[]} onClose={vi.fn()} />);
  return { api, ...rendered };
}

describe("apply run", () => {
  it("preflight lists blocks and sensitive counts and shows the fixed copy", async () => {
    const blocked = check({ application_id: "a1", blocks: ["link_not_https", "already_in_a_run"] });
    const { api } = await openPreflight({ apply_preflight: () => preflight({ items: [blocked] }) });

    const table = await screen.findByRole("table", { name: "Preflight" });
    const row = within(table).getAllByRole("row")[1];
    expect(row).toHaveTextContent("Acme");
    expect(row).toHaveTextContent("boards.greenhouse.io");
    expect(within(row).getByText("2")).toBeInTheDocument();
    expect(row).toHaveTextContent("The posting link is not https.");
    expect(row).toHaveTextContent("It is already in a run that has not ended.");
    expect(screen.getByText(FIXED)).toBeInTheDocument();
    const asked = api.calls.find((c) => c.cmd === "apply_preflight");
    expect(asked?.args).toEqual({ applicationIds: ["a1"] });
  });

  it("an application submitted before shows why it is blocked, not its code", async () => {
    const blocked = check({ application_id: "a1", blocks: ["already_submitted"] });
    await openPreflight({ apply_preflight: () => preflight({ items: [blocked] }) });

    const table = await screen.findByRole("table", { name: "Preflight" });
    const row = within(table).getAllByRole("row")[1];
    expect(row).toHaveTextContent(
      "This application was already submitted, or may have been. Record its outcome first.",
    );
    expect(row).not.toHaveTextContent("Refused: already_submitted");
    expect(screen.getByRole("button", { name: "Start run" })).toBeDisabled();
  });

  it("Start is disabled while a block stands", async () => {
    let blocks = ["not_saved"];
    const { api, user } = await openPreflight({
      apply_preflight: () => preflight({ items: [check({ blocks })] }),
      apply_start: () => "run9",
      apply_view: () => run({ id: "run9", state: "ready", items: [item({ state: "queued" })] }),
    });
    const start = await screen.findByRole("button", { name: "Start run" });
    expect(start).toBeDisabled();

    blocks = [];
    await user.click(screen.getByRole("button", { name: "Check again" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Start run" })).toBeEnabled());
    await user.click(screen.getByRole("button", { name: "Start run" }));

    await waitFor(() => expect(useUiStore.getState().applyRun).toBe("run9"));
    expect(api.calls.find((c) => c.cmd === "apply_start")?.args).toEqual({ applicationIds: ["a1"] });
  });

  it("setup needed offers Set up browser with the reason", async () => {
    let state: Preflight["runtime"] = "not_installed";
    const { api, user } = await openPreflight({
      apply_preflight: () => preflight({ runtime: state }),
      browser_setup: () => {
        state = "ready";
        return { state, found: null, playwright_mcp: "0.0.84" };
      },
    });
    expect(await screen.findByText("The browser runtime is not installed.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Start run" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Set up browser" }));
    await waitFor(() => expect(count(api, "browser_setup")).toBe(1));
    expect(await screen.findByRole("button", { name: "Start run" })).toBeEnabled();
  });

  it("a pause shows the page's words as text and no anchor", async () => {
    const excerpt = '<a href="https://evil.example/">Click here to continue</a>';
    showRun(
      run({
        active_pause: {
          id: "p1",
          item_id: "i1",
          kind: "login",
          origin: "page",
          excerpt,
          created_at: "2026-11-02T10:00:00.000Z",
        },
      }),
    );
    expect(await screen.findByText(excerpt)).toBeInTheDocument();
    expect(screen.queryAllByRole("link")).toHaveLength(0);
    expect(
      screen.getByText("Page text is shown as plain text and cannot end a pause. Only you can, with the buttons below."),
    ).toBeInTheDocument();
    const pauses = screen.getByRole("list", { name: "Mandatory pauses" });
    expect(within(pauses).getAllByRole("listitem")).toHaveLength(6);
    const active = within(pauses).getByText(/ACTIVE/).closest("li")!;
    expect(active).toHaveTextContent("Other step that needs a person");
    expect(active).toHaveTextContent("Sign in, in the browser window.");
    expect(active).toHaveAttribute("aria-current", "step");
  });

  it("the pause buttons send only a person's answer", async () => {
    const pause = {
      id: "p1",
      item_id: "i1",
      kind: "captcha" as const,
      origin: "page",
      excerpt: "Prove you are human",
      created_at: "2026-11-02T10:00:00.000Z",
    };
    const { api, user } = showRun(run({ active_pause: pause }), { apply_resolve_pause: () => undefined });
    await user.click(await screen.findByRole("button", { name: "I did this in the browser: continue" }));
    await waitFor(() => expect(count(api, "apply_resolve_pause")).toBe(1));
    expect(api.calls.find((c) => c.cmd === "apply_resolve_pause")?.args).toEqual({
      pauseId: "p1",
      resolution: "continue",
    });
    await user.click(screen.getByRole("button", { name: "Take over by hand" }));
    await waitFor(() => expect(count(api, "apply_resolve_pause")).toBe(2));
    expect(api.calls.filter((c) => c.cmd === "apply_resolve_pause")[1].args.resolution).toBe("take_over");

    await user.click(screen.getByRole("button", { name: "Cancel run" }));
    const dialog = await screen.findByRole("dialog");
    expect(count(api, "apply_cancel")).toBe(0);
    await user.click(within(dialog).getByRole("button", { name: "Cancel run" }));
    await waitFor(() => expect(count(api, "apply_cancel")).toBe(1));
  });

  it("an uncertain item reads Check the site. JobPilot will not try again.", async () => {
    showRun(
      run({
        state: "ended",
        ended_at: "2026-11-02T11:00:00.000Z",
        items: [item({ state: "uncertain", reason: "session_ended", submit_clicks: 1 })],
      }),
    );
    expect(await screen.findByText("Check the site. JobPilot will not try again.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Record status" })).not.toBeInTheDocument();
  });

  it("a submitted item with no status shows Record status", async () => {
    const said = "<b>Thanks</b> for applying";
    const { api, user } = showRun(
      run({
        state: "ended",
        ended_at: "2026-11-02T11:00:00.000Z",
        items: [
          item({ state: "submitted", confirmation_text: said, status_written: false }),
          item({ id: "i2", position: 1, state: "submitted", status_written: true }),
        ],
      }),
      { apply_settle: () => 1 },
    );
    expect(await screen.findByText(said)).toBeInTheDocument();
    expect(screen.getByText("The page said")).toBeInTheDocument();
    const buttons = screen.getAllByRole("button", { name: "Record status" });
    expect(buttons).toHaveLength(1);
    await user.click(buttons[0]);
    await waitFor(() => expect(count(api, "apply_settle")).toBe(1));
    expect(api.calls.find((c) => c.cmd === "apply_settle")?.args).toEqual({ id: "run1" });
  });

  it("running polls the run and shows a row per item", async () => {
    const { api } = showRun(
      run({ items: [item({ state: "submitted" }), item({ id: "i2", position: 1, host: "jobs.example.com", state: "queued" })] }),
    );
    const table = await screen.findByRole("table", { name: "Applications in this run" });
    expect(within(table).getAllByRole("row")).toHaveLength(3);
    expect(table).toHaveTextContent("jobs.example.com");
    await waitFor(() => expect(count(api, "apply_view")).toBeGreaterThan(1), { timeout: 3000 });
  });

  it("dismissing the release dialog stays on the preflight with no alert", async () => {
    await openPreflight({
      apply_preflight: () => preflight(),
      apply_start: () => Promise.reject({ code: "cancelled", message: "cancelled" }),
    });
    const start = await screen.findByRole("button", { name: "Start run" });
    start.click();
    await waitFor(() => expect(start).toBeEnabled());
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(useUiStore.getState().applyRun).toBeNull();
    expect(screen.getByRole("table", { name: "Preflight" })).toBeInTheDocument();
  });

  it("preflight names a missing consent for sensitive answers", async () => {
    await openPreflight({ apply_preflight: () => preflight({ sensitive_consent: false }) });
    expect(
      await screen.findByText(
        "Your Claude Code consent does not cover sensitive answers, so they stay held back and each such field stops for you. Review it on the AI screen.",
      ),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start run" })).toBeEnabled();
  });

  it("a blocked document reads in words", async () => {
    await openPreflight({
      apply_preflight: () => preflight({ items: [check({ resume: { blocked: "claims_blocked" } })] }),
    });
    expect(await screen.findByText("Blocked: a claim to fix first")).toBeInTheDocument();
  });

  it("a run screen that cannot load still has a way back", async () => {
    const { user } = showRun(run(), {
      apply_view: () => Promise.reject({ code: "not_found", message: "not found" }),
    });
    expect(await screen.findByRole("alert")).toHaveTextContent("code: not_found");
    await user.click(screen.getByRole("button", { name: "Back" }));
    expect(useUiStore.getState().applyRun).toBeNull();
  });

  it("the last run with a status left to record is offered after a restart", async () => {
    const last = run({
      id: "run7",
      state: "ended",
      ended_at: "2026-11-02T11:00:00.000Z",
      items: [item({ run_id: "run7", state: "submitted", status_written: false })],
    });
    backend({ app_list: () => APPS, resume_list: () => [], apply_latest: () => last, apply_view: () => last });
    const { user } = renderApp();
    await screen.findByRole("heading", { name: "Status" });
    await user.click(screen.getByRole("button", { name: "Applications" }));
    await user.click(await screen.findByRole("button", { name: "Show last run" }));
    expect(useUiStore.getState().applyRun).toBe("run7");
    expect(await screen.findByRole("button", { name: "Record status" })).toBeInTheDocument();
  });

  it("the last run opens view-only: never hosted, never cancelled", async () => {
    const last = run({
      id: "run7",
      state: "ended",
      ended_at: "2026-11-02T11:00:00.000Z",
      items: [item({ run_id: "run7", state: "submitted", status_written: false })],
    });
    const api = backend({
      app_list: () => APPS,
      resume_list: () => [],
      apply_latest: () => last,
      apply_view: () => last,
      // As the real backend: an ended run is not ready.
      pty_open_apply: () => Promise.reject({ code: "refused", message: "refused: apply_run_not_ready" }),
    });
    const { user } = renderApp();
    await screen.findByRole("heading", { name: "Status" });
    await user.click(screen.getByRole("button", { name: "Applications" }));
    await user.click(await screen.findByRole("button", { name: "Show last run" }));
    expect(await screen.findByRole("button", { name: "Record status" })).toBeInTheDocument();
    await new Promise((resolve) => setTimeout(resolve, 300));
    expect(count(api, "pty_open_apply")).toBe(0);
    expect(count(api, "apply_cancel")).toBe(0);
    expect(useUiStore.getState().applyRun).toBe("run7");
    expect(screen.getByRole("button", { name: "Record status" })).toBeInTheDocument();
  });
});

const BLOCKED = "The site's terms forbid this. JobPilot will not do it, even if you ask.";

const material = (over: Partial<Material> = {}): Material => ({
  modes: [
    {
      mode: "manual",
      available: true,
      note: "You fill and submit in your own browser. JobPilot opens the link, gives you files and answers, and records what happened.",
    },
    { mode: "copilot", available: false, note: BLOCKED },
    { mode: "autonomous", available: false, note: BLOCKED },
    {
      mode: "claude_code",
      available: true,
      note: "Fills and submits with Claude Code in a visible browser. Add it to a run from Applications.",
    },
  ],
  resume: "/data/exports/apply/a1/resume/resume.pdf",
  letter: null,
  answers: [],
  authorization: { country: "US", authorized: "not_stated", needs_sponsorship: "not_stated", note: "", stated: false },
  ...over,
});

async function openReview(app: Application, overrides: Record<string, (args: Args) => unknown>) {
  const api = backend({ app_list: () => [app], app_get: () => app, resume_list: () => [], ...overrides });
  const { user } = renderApp();
  await screen.findByRole("heading", { name: "Status" });
  await user.click(screen.getByRole("button", { name: "Applications" }));
  await user.click(await screen.findByRole("button", { name: app.company }));
  await user.click(await screen.findByRole("button", { name: "Review and apply..." }));
  await screen.findByRole("heading", { name: `Review application: ${app.role}, ${app.company}` });
  return { api, user };
}

describe("application review", () => {
  it("Autonomous shows its reason and has no enabled control today but Revoke", async () => {
    const NONE =
      "No site or connector qualifies today. Needs a site rule of allowed, verified permission, and your explicit authorization.";
    const modes = material().modes.map((m) => (m.mode === "autonomous" ? { ...m, note: NONE } : m));
    const { api, user } = await openReview(application({ link: "https://jobs.lever.co/acme/1" }), {
      apply_material: () => material({ modes }),
      autonomous_grants: () => ["lever"],
    });
    const group = await screen.findByRole("radiogroup", { name: "How do you want to apply?" });
    const autonomous = within(group).getByRole("radio", { name: "Autonomous" });
    expect(autonomous).toHaveAttribute("aria-disabled", "true");
    expect(autonomous).toHaveAccessibleDescription(`Not available. ${NONE}`);
    // What the owner authorized is shown, and the only control takes it away.
    expect(await screen.findByText("Connectors you authorized for Autonomous: lever.")).toBeInTheDocument();
    await user.click(autonomous);
    expect(autonomous).not.toBeChecked();
    for (const button of screen.queryAllByRole("button")) {
      if (button.textContent === "Revoke lever") continue;
      expect(button.textContent ?? "").not.toMatch(/autonomous|authori[sz]e|submit/i);
    }
    expect(count(api, "autonomous_grant")).toBe(0);
    expect(count(api, "autonomous_start")).toBe(0);
    await user.click(screen.getByRole("button", { name: "Revoke lever" }));
    await waitFor(() => expect(count(api, "autonomous_revoke")).toBe(1));
    expect(api.calls.find((c) => c.cmd === "autonomous_revoke")?.args).toEqual({ connector: "lever" });
  });

  it("disabled modes read their reason and cannot be selected", async () => {
    const { user } = await openReview(application({}), { apply_material: () => material() });
    const group = await screen.findByRole("radiogroup", { name: "How do you want to apply?" });
    const manual = within(group).getByRole("radio", { name: "Manual" });
    const copilot = within(group).getByRole("radio", { name: "Copilot" });
    expect(manual).toBeChecked();
    expect(copilot).toHaveAttribute("aria-disabled", "true");
    expect(copilot).toHaveAccessibleDescription(`Not available. ${BLOCKED}`);
    expect(within(group).getByRole("radio", { name: "Autonomous" })).toHaveAccessibleDescription(
      `Not available. ${BLOCKED}`,
    );
    await user.click(copilot);
    expect(copilot).not.toBeChecked();
    expect(manual).toBeChecked();
  });

  it("a sensitive answer shows Confirm and copy, never a plain copy", async () => {
    const answers: Material["answers"] = [
      { answer_id: "n1", question: "Notice period", class: "normal", stale: false, needs_confirmation: false },
      { answer_id: "s1", question: "Salary expectation", class: "sensitive", stale: false, needs_confirmation: true },
      // Confirmed already: still asked again, never a plain copy.
      { answer_id: "s2", question: "Current salary", class: "sensitive", stale: false, needs_confirmation: false },
    ];
    const { api, user } = await openReview(application({}), {
      apply_material: () => material({ answers }),
      memory_confirm_use: () => undefined,
      apply_copy: () => ({ stale: false }),
    });
    const list = await screen.findByRole("list", { name: "Answers you approved for this form" });
    const row = (question: string) => within(list).getByText(question).closest("li")!;
    expect(within(row("Notice period")).getByRole("button", { name: "Copy: Notice period" })).toBeInTheDocument();
    for (const question of ["Salary expectation", "Current salary"]) {
      expect(within(row(question)).queryByRole("button", { name: `Copy: ${question}` })).not.toBeInTheDocument();
      expect(within(row(question)).getByRole("button", { name: `Confirm and copy: ${question}` })).toBeInTheDocument();
    }
    expect(row("Salary expectation")).toHaveTextContent("SENSITIVE: confirm");

    await user.click(within(row("Salary expectation")).getByRole("button", { name: "Confirm and copy: Salary expectation" }));
    await waitFor(() => expect(count(api, "apply_copy")).toBe(1));
    const order = api.calls.map((c) => c.cmd).filter((cmd) => cmd === "memory_confirm_use" || cmd === "apply_copy");
    expect(order).toEqual(["memory_confirm_use", "apply_copy"]);
    expect(api.calls.find((c) => c.cmd === "apply_copy")?.args).toEqual({ applicationId: "a1", answerId: "s1" });
    expect(await screen.findByText("Copied to the clipboard.")).toBeInTheDocument();
    expect(count(api, "memory_use")).toBe(0);

    await user.click(within(row("Notice period")).getByRole("button", { name: "Copy: Notice period" }));
    await waitFor(() => expect(count(api, "apply_copy")).toBe(2));
    expect(count(api, "memory_confirm_use")).toBe(1);
  });

  it("a copy reloads no material and updates only the stale flag", async () => {
    const answers: Material["answers"] = [
      { answer_id: "n1", question: "Notice period", class: "normal", stale: false, needs_confirmation: false },
    ];
    const { api, user } = await openReview(application({}), {
      apply_material: () => material({ answers }),
      apply_copy: () => ({ stale: true }),
    });
    const list = await screen.findByRole("list", { name: "Answers you approved for this form" });
    expect(list).toHaveTextContent("approved, fresh");
    await user.click(within(list).getByRole("button", { name: "Copy: Notice period" }));
    await waitFor(() => expect(list).toHaveTextContent("approved, not reviewed for over 180 days"));
    await new Promise((resolve) => setTimeout(resolve, 100));
    expect(count(api, "apply_material")).toBe(1);
  });

  it("work authorization not stated reads so and offers Enter it yourself", async () => {
    const { user } = await openReview(application({}), { apply_material: () => material() });
    expect(await screen.findByText("Work authorization (US): not stated by you")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Enter it yourself" }));
    expect(useUiStore.getState().view).toBe("profile");
  });

  it("Open the form is disabled with the reason when the link is not https", async () => {
    await openReview(application({ link: "http://boards.greenhouse.io/acme/jobs/1" }), {
      apply_material: () => material(),
    });
    const open = await screen.findByRole("button", { name: "Open the form in my browser" });
    expect(open).toBeDisabled();
    expect(open).toHaveAccessibleDescription("The posting link is not https, so JobPilot will not open it.");
  });

  it("Open the form sends only the application id, and Record outcome tracks the status", async () => {
    const { api, user } = await openReview(application({}), {
      apply_material: () => material(),
      apply_open: () => undefined,
      apply_record: () => application({ status: "applied", applied_on: "2026-11-02" }),
    });
    await user.click(await screen.findByRole("button", { name: "Open the form in my browser" }));
    await waitFor(() => expect(count(api, "apply_open")).toBe(1));
    expect(api.calls.find((c) => c.cmd === "apply_open")?.args).toEqual({ applicationId: "a1" });

    await user.click(screen.getByRole("button", { name: "Record outcome..." }));
    await user.selectOptions(screen.getByLabelText("Outcome"), "applied");
    await user.type(screen.getByLabelText("Applied on (optional)"), "2026-11-02");
    await user.click(screen.getByRole("button", { name: "Save outcome" }));
    await waitFor(() => expect(count(api, "apply_record")).toBe(1));
    expect(api.calls.find((c) => c.cmd === "apply_record")?.args).toEqual({
      applicationId: "a1",
      outcome: "applied",
      on: "2026-11-02",
      note: "",
    });
  });
});
