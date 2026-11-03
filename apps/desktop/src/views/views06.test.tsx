import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { Application, ApplyItem, ApplyRunView, ItemCheck, Preflight } from "../lib/ipc";
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
});
