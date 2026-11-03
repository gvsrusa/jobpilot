import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { Application, ApplyItem, ApplyRunView, Material } from "../lib/ipc";
import { backend, count, renderApp } from "../test-utils";

type Args = Record<string, unknown>;

const NO_CLAUSE = "No clause found for this site. You review and submit.";
const REVIEW = "Review the form in the browser and click submit yourself. Continue records it as submitted; Take over if you did not.";

const application: Application = {
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
};

const material: Material = {
  modes: [
    { mode: "manual", available: true, note: "You fill and submit in your own browser." },
    { mode: "copilot", available: true, note: NO_CLAUSE },
    { mode: "autonomous", available: false, note: "No site or connector qualifies today." },
    { mode: "claude_code", available: false, note: "Needs the browser runtime and Claude Code set up first." },
  ],
  resume: null,
  letter: null,
  answers: [],
  authorization: { country: "US", authorized: "not_stated", needs_sponsorship: "not_stated", note: "", stated: false },
};

const item = (state: string): ApplyItem => ({
  id: "i1",
  run_id: "run1",
  application_id: "a1",
  position: 0,
  host: "boards.greenhouse.io",
  state,
  submit_clicks: 0,
  auth_released: false,
  resume_path: null,
  letter_path: null,
  policy_note: "no clause found",
  confirmation_text: null,
  confirmation_url: null,
  status_written: false,
  reason: null,
  released_answers: [],
});

const run = (over: Partial<ApplyRunView>): ApplyRunView => ({
  id: "run1",
  mode: "copilot",
  state: "running",
  items: [item("filled")],
  active_pause: null,
  created_at: "2026-11-03T10:00:00.000Z",
  ended_at: null,
  ...over,
});

async function openReview(overrides: Record<string, (args: Args) => unknown>) {
  const api = backend({
    app_list: () => [application],
    app_get: () => application,
    resume_list: () => [],
    apply_material: () => material,
    ...overrides,
  });
  const { user } = renderApp();
  await screen.findByRole("heading", { name: "Status" });
  await user.click(screen.getByRole("button", { name: "Applications" }));
  await user.click(await screen.findByRole("button", { name: "Acme" }));
  await user.click(await screen.findByRole("button", { name: "Review and apply..." }));
  const group = await screen.findByRole("radiogroup", { name: "How do you want to apply?" });
  await user.click(within(group).getByRole("radio", { name: "Copilot" }));
  return { api, user };
}

describe("copilot", () => {
  it("starts with the application id only, shows its review stop, and ends on the person's word", async () => {
    let finish: (view: ApplyRunView) => void = () => {};
    const latest = run({
      active_pause: {
        id: "p1",
        item_id: "i1",
        kind: "other",
        origin: "guard",
        excerpt: `${REVIEW} Left for you: Phone.`,
        created_at: "2026-11-03T10:00:01.000Z",
      },
    });
    let started = false;
    const { api, user } = await openReview({
      copilot_start: () => {
        started = true;
        return new Promise<ApplyRunView>((resolve) => (finish = resolve));
      },
      apply_latest: () => (started ? latest : null),
      apply_resolve_pause: () => undefined,
    });
    expect(screen.queryByText(/not built yet/)).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Start Copilot" }));
    expect(api.calls.find((c) => c.cmd === "copilot_start")?.args).toEqual({ applicationId: "a1" });
    expect(await screen.findByRole("button", { name: "Copilot is running..." })).toBeDisabled();

    // The stop is the pause panel's: the page's words as text, and only its buttons end it.
    expect(
      await screen.findByRole("heading", { name: "Run paused: waiting for you" }, { timeout: 3000 }),
    ).toBeInTheDocument();
    expect(screen.getByText(`${REVIEW} Left for you: Phone.`)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "I did this in the browser: continue" }));
    await waitFor(() => expect(count(api, "apply_resolve_pause")).toBe(1));
    expect(api.calls.find((c) => c.cmd === "apply_resolve_pause")?.args).toEqual({
      pauseId: "p1",
      resolution: "continue",
    });

    const done = run({ state: "ended", items: [item("submitted")], ended_at: "2026-11-03T10:05:00.000Z" });
    started = false;
    finish(done);
    expect(await screen.findByText(/It is recorded as applied/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start Copilot" })).toBeEnabled();
  });

  it("a refusal shows its code as an alert", async () => {
    await openReview({
      copilot_start: () => {
        throw { code: "refused", message: "policy_blocked" };
      },
      apply_latest: () => null,
    });
    await screen.getByRole("button", { name: "Start Copilot" }).click();
    expect(await screen.findByRole("alert")).toHaveTextContent("policy_blocked");
  });

  it("its stop shows in the app whatever view is open, and leaving the review loses nothing", async () => {
    let latest: ApplyRunView | null = null;
    const live = run({
      active_pause: {
        id: "p1",
        item_id: "i1",
        kind: "other",
        origin: "guard",
        excerpt: REVIEW,
        created_at: "2026-11-03T10:00:01.000Z",
      },
    });
    const { api, user } = await openReview({
      copilot_start: () => {
        latest = live;
        return new Promise<ApplyRunView>(() => {});
      },
      apply_latest: () => latest,
      apply_resolve_pause: () => undefined,
    });
    await user.click(screen.getByRole("button", { name: "Start Copilot" }));
    expect(
      await screen.findByRole("heading", { name: "Run paused: waiting for you" }, { timeout: 3000 }),
    ).toBeInTheDocument();

    // Another view: the stop is still there to answer.
    await user.click(screen.getByRole("button", { name: "Status" }));
    expect(
      await screen.findByRole("heading", { name: "Run paused: waiting for you" }, { timeout: 3000 }),
    ).toBeInTheDocument();

    // Back on the review: Start stays refused while the run lives, and the stop is shown once.
    await user.click(screen.getByRole("button", { name: "Applications" }));
    await user.click(await screen.findByRole("button", { name: "Acme" }));
    await user.click(await screen.findByRole("button", { name: "Review and apply..." }));
    const group = await screen.findByRole("radiogroup", { name: "How do you want to apply?" });
    await user.click(within(group).getByRole("radio", { name: "Copilot" }));
    expect(await screen.findByRole("button", { name: "Copilot is running..." })).toBeDisabled();
    expect(screen.getAllByRole("heading", { name: "Run paused: waiting for you" })).toHaveLength(1);
    await user.click(screen.getByRole("button", { name: "I did this in the browser: continue" }));
    await waitFor(() => expect(count(api, "apply_resolve_pause")).toBe(1));

    latest = run({ state: "ended", items: [item("submitted")], ended_at: "2026-11-03T10:05:00.000Z" });
    await waitFor(
      () => expect(screen.queryByRole("heading", { name: "Run paused: waiting for you" })).not.toBeInTheDocument(),
      { timeout: 3000 },
    );
    expect(await screen.findByRole("button", { name: "Start Copilot" }, { timeout: 3000 })).toBeEnabled();
  });
});
