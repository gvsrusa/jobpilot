import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type {
  Answer,
  AppliedAnswer,
  AuthorizationAnswer,
  Application,
  ApplicationEvent,
  ApplicationInput,
  ApplicationStatus,
  Finding,
  LogRow,
  KnowledgeItem,
  MemoryClass,
  MemoryHit,
  Question,
  QuestionRow,
  Report,
  RunDetail,
  RunState,
  RunSummary,
} from "../lib/ipc";
import { backend, count, renderApp } from "../test-utils";

type Args = Record<string, unknown>;

/** A small in-memory store behind the tracking commands. */
function tracking() {
  const apps: Application[] = [];
  const events = new Map<string, ApplicationEvent[]>();
  let n = 0;
  const log = (id: string, kind: ApplicationEvent["kind"], over: Partial<ApplicationEvent> = {}) => {
    const list = events.get(id) ?? [];
    list.push({
      id: ++n,
      at: "2026-11-01T10:00:00Z",
      actor: "gui",
      kind,
      from_status: null,
      to_status: null,
      note: "",
      ...over,
    });
    events.set(id, list);
  };
  const find = (id: unknown) => apps.find((a) => a.id === id)!;
  return backend({
    app_list: ({ status }: Args) => apps.filter((a) => !status || a.status === status),
    app_get: ({ id }: Args) => find(id),
    app_add: ({ input }: Args) => {
      const app: Application = {
        ...(input as ApplicationInput),
        id: `a${apps.length + 1}`,
        status: "saved",
        applied_on: null,
        created_at: "2026-11-01T10:00:00Z",
        updated_at: "2026-11-01T10:00:00Z",
      };
      apps.unshift(app);
      log(app.id, "created", { to_status: "saved" });
      return app;
    },
    app_update: ({ id, input }: Args) => {
      Object.assign(find(id), input);
      log(String(id), "edited");
      return find(id);
    },
    app_set_status: ({ id, status, on, note }: Args) => {
      const app = find(id);
      if (app.status === status) {
        log(app.id, "note", { note: String(note) });
        return app;
      }
      log(app.id, "status", { from_status: app.status, to_status: status as ApplicationStatus, note: String(note) });
      app.status = status as ApplicationStatus;
      if (status === "applied" && !app.applied_on) app.applied_on = (on as string) ?? "2026-11-01";
      return app;
    },
    app_note: ({ id, note }: Args) => {
      log(String(id), "note", { note: String(note) });
    },
    app_history: ({ id }: Args) => events.get(String(id)) ?? [],
    app_remove: ({ id }: Args) => {
      apps.splice(apps.indexOf(find(id)), 1);
    },
  });
}

async function open(user: ReturnType<typeof renderApp>["user"]) {
  await user.click(await screen.findByRole("button", { name: "Applications" }));
  await screen.findByRole("heading", { level: 1, name: "Applications" });
}

const env = (globalThis as unknown as { process: { env: Record<string, string | undefined> } }).process.env;

const report = (over: Partial<Report> = {}): Report => ({
  id: "r1",
  created_at: "2026-11-01T10:00:00Z",
  volume: { total: 0, by_status: {}, applied_last_30_days: 0 },
  funnel: { applied: 0, responded: 0, interviews: 0, offers: 0, response_rate: null, offer_rate: null },
  timing: { median_days_to_response: null, responses_counted: 0 },
  ...over,
});

describe("applications summary", () => {
  it("summary_shows_not_enough_data_with_no_applications", async () => {
    backend({ analytics_latest: () => report() });
    const { user } = renderApp();
    await open(user);
    const summary = await screen.findByRole("region", { name: "Summary" });
    expect(await within(summary).findByText("Total applications")).toBeInTheDocument();
    expect(within(summary).getAllByText("Not enough data yet")).toHaveLength(3);
    expect(within(summary).getByText("Applied in the last 30 days").nextSibling).toHaveTextContent("0");
  });

  it("no_report_yet_asks_for_a_refresh", async () => {
    backend({ analytics_latest: () => null });
    const { user } = renderApp();
    await open(user);
    const summary = await screen.findByRole("region", { name: "Summary" });
    expect(await within(summary).findByText("No summary yet. Press Refresh to build one.")).toBeInTheDocument();
  });

  it("refresh_runs_a_report", async () => {
    let built = false;
    const api = backend({
      analytics_latest: () => (built ? report({ id: "r2" }) : null),
      analytics_run: () => {
        built = true;
        return report({
          id: "r2",
          volume: { total: 4, by_status: { applied: 3, rejected: 1 }, applied_last_30_days: 4 },
          funnel: { applied: 4, responded: 1, interviews: 0, offers: 0, response_rate: 0.25, offer_rate: 0 },
          timing: { median_days_to_response: 3.5, responses_counted: 1 },
        });
      },
    });
    const { user } = renderApp();
    await open(user);
    const summary = await screen.findByRole("region", { name: "Summary" });
    await user.click(await within(summary).findByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(count(api, "analytics_run")).toBe(1));
    expect(await within(summary).findByText("25%")).toBeInTheDocument();
    expect(within(summary).getByText("0%")).toBeInTheDocument();
    expect(within(summary).getByText("3.5 days (from 1 response)")).toBeInTheDocument();
    expect(within(summary).getByText("Total applications").nextSibling).toHaveTextContent("4");
    expect(within(summary).getByText("Applied: 3")).toBeInTheDocument();
    expect(within(summary).getByText("Rejected: 1")).toBeInTheDocument();
  });

  it("a_rate_never_rounds_to_an_end_it_has_not_reached", async () => {
    backend({
      analytics_latest: () =>
        report({
          funnel: { applied: 1000, responded: 2, interviews: 0, offers: 999, response_rate: 0.002, offer_rate: 0.999 },
        }),
    });
    const { user } = renderApp();
    await open(user);
    const summary = await screen.findByRole("region", { name: "Summary" });
    expect(await within(summary).findByText("<1%")).toBeInTheDocument();
    expect(within(summary).getByText(">99%")).toBeInTheDocument();
  });

  it("the_median_names_its_sample_and_says_1_day", async () => {
    backend({
      analytics_latest: () => report({ timing: { median_days_to_response: 1, responses_counted: 1 } }),
    });
    const { user } = renderApp();
    await open(user);
    const summary = await screen.findByRole("region", { name: "Summary" });
    expect(await within(summary).findByText("1 day (from 1 response)")).toBeInTheDocument();
  });

  it("the_median_pluralises_days_and_responses", async () => {
    backend({
      analytics_latest: () => report({ timing: { median_days_to_response: 3.5, responses_counted: 2 } }),
    });
    const { user } = renderApp();
    await open(user);
    const summary = await screen.findByRole("region", { name: "Summary" });
    expect(await within(summary).findByText("3.5 days (from 2 responses)")).toBeInTheDocument();
  });

  it("the_report_time_shows_beside_its_date", async () => {
    backend({ analytics_latest: () => report({ created_at: "2026-11-01T10:45:00Z" }) });
    const { user } = renderApp();
    await open(user);
    const summary = await screen.findByRole("region", { name: "Summary" });
    const expected = new Date("2026-11-01T10:45:00Z").toLocaleTimeString(undefined, {
      hour: "numeric",
      minute: "2-digit",
    });
    expect(await within(summary).findByText(new RegExp(`As of .*${expected}`))).toBeInTheDocument();
  });

  it("a_status_change_asks_for_the_summary_again", async () => {
    const api = tracking();
    const { user } = renderApp();
    await open(user);
    await user.click(screen.getByRole("button", { name: "Add application" }));
    await user.type(screen.getByLabelText("Company"), "Acme");
    await user.type(screen.getByLabelText("Role"), "Engineer");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await user.click(await screen.findByRole("button", { name: "Acme" }));
    await screen.findByRole("region", { name: "Summary" });
    await waitFor(() => expect(count(api, "analytics_latest")).toBeGreaterThan(0));
    const before = count(api, "analytics_latest");
    await user.selectOptions(await screen.findByLabelText("New status"), "applied");
    await user.click(screen.getByRole("button", { name: "Set status" }));
    await waitFor(() => expect(count(api, "app_set_status")).toBe(1));
    await waitFor(() => expect(count(api, "analytics_latest")).toBeGreaterThan(before));
  });

  it("a_refused_refresh_shows_its_code", async () => {
    backend({
      analytics_latest: () => null,
      analytics_run: () => {
        throw { code: "paused", message: "emergency pause is on" };
      },
    });
    const { user } = renderApp();
    await open(user);
    const summary = await screen.findByRole("region", { name: "Summary" });
    await user.click(await within(summary).findByRole("button", { name: "Refresh" }));
    expect(await within(summary).findByRole("alert")).toHaveTextContent("paused");
  });
});

describe("applications", () => {
  it("ctrl_7_opens_applications", async () => {
    backend();
    const { user } = renderApp();
    await screen.findByRole("heading", { name: "Status" });
    await user.keyboard("{Control>}7{/Control}");
    expect(await screen.findByRole("heading", { level: 1, name: "Applications" })).toBeInTheDocument();
    expect(await screen.findByText("No applications yet. Add the first one.")).toBeInTheDocument();
  });

  it("an_application_tied_to_an_older_version_of_a_posting_still_names_it", async () => {
    const application: Application = {
      id: "a1",
      company: "Acme",
      role: "Engineer",
      status: "saved",
      job_text_id: "old",
      resume_draft_id: null,
      letter_draft_id: null,
      link: "",
      country: "",
      notes: "",
      applied_on: null,
      created_at: "2026-11-01T10:00:00Z",
      updated_at: "2026-11-01T10:00:00Z",
    };
    backend({
      app_list: () => [application],
      app_get: () => application,
      job_get: () => ({ id: "old", title: "Older posting", normalized: "x", findings: [] }),
    });
    const { user } = renderApp();
    await open(user);
    await user.click(await screen.findByRole("button", { name: "Acme" }));
    await waitFor(() => expect(screen.getByLabelText("Job posting")).toHaveDisplayValue("Older posting"));
  });

  it("applications_add_change_status_and_see_history", async () => {
    const api = tracking();
    const { user } = renderApp();
    await open(user);

    await user.click(screen.getByRole("button", { name: "Add application" }));
    await user.type(screen.getByLabelText("Company"), "Acme");
    await user.type(screen.getByLabelText("Role"), "Engineer");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(count(api, "app_add")).toBe(1));
    expect(api.calls.find((c) => c.cmd === "app_add")!.args.input).toMatchObject({
      company: "Acme",
      role: "Engineer",
      job_text_id: null,
    });

    const row = await screen.findByRole("button", { name: "Acme" });
    expect(row.closest("tr")).toHaveTextContent("Saved");
    await user.click(row);
    await user.selectOptions(await screen.findByLabelText("New status"), "applied");
    await user.type(screen.getByLabelText("Applied on (optional)"), "2026-10-02");
    await user.type(screen.getByLabelText("Status note (optional)"), "sent online");
    await user.click(screen.getByRole("button", { name: "Set status" }));
    await waitFor(() => expect(count(api, "app_set_status")).toBe(1));
    expect(api.calls.find((c) => c.cmd === "app_set_status")!.args).toMatchObject({
      id: "a1",
      status: "applied",
      on: "2026-10-02",
      note: "sent online",
    });

    await user.type(screen.getByLabelText("Add note"), "recruiter called");
    await user.click(screen.getByRole("button", { name: "Add note" }));
    const history = await screen.findByRole("list", { name: "History" });
    await waitFor(() => expect(within(history).getAllByRole("listitem")).toHaveLength(3));
    expect(history).toHaveTextContent("Saved to Applied");
    expect(history).toHaveTextContent("sent online");
    expect(history).toHaveTextContent("recruiter called");
    expect(row.closest("tr")).toHaveTextContent("Applied");
  });

  it("filters_by_status_and_removes_after_confirming", async () => {
    const api = tracking();
    const { user } = renderApp();
    await open(user);
    await user.click(screen.getByRole("button", { name: "Add application" }));
    await user.type(screen.getByLabelText("Company"), "Acme");
    await user.type(screen.getByLabelText("Role"), "Engineer");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await user.click(await screen.findByRole("button", { name: "Acme" }));

    await user.selectOptions(screen.getByLabelText("Filter by status"), "offer");
    expect(await screen.findByText("No applications with this status.")).toBeInTheDocument();
    await user.selectOptions(screen.getByLabelText("Filter by status"), "");
    await user.click(await screen.findByRole("button", { name: "Acme" }));

    await user.click(screen.getByRole("button", { name: "Remove..." }));
    expect(count(api, "app_remove")).toBe(0);
    await user.click(screen.getByRole("button", { name: "Remove application" }));
    await waitFor(() => expect(count(api, "app_remove")).toBe(1));
    expect(await screen.findByText("No applications yet. Add the first one.")).toBeInTheDocument();
  });

  it("shows_a_refusal_with_its_code", async () => {
    backend({
      app_add: () => {
        throw { code: "validation", message: "invalid input: a country is two letters" };
      },
    });
    const { user } = renderApp();
    await open(user);
    await user.click(screen.getByRole("button", { name: "Add application" }));
    await user.type(screen.getByLabelText("Company"), "Acme");
    await user.type(screen.getByLabelText("Role"), "Engineer");
    await user.click(screen.getByRole("button", { name: "Save" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("code: validation");
  });

  it("an_application_link_is_never_an_anchor", async () => {
    tracking();
    const { user, container } = renderApp();
    await open(user);
    await user.click(screen.getByRole("button", { name: "Add application" }));
    await user.type(screen.getByLabelText("Company"), "Acme");
    await user.type(screen.getByLabelText("Role"), "Engineer");
    await user.type(screen.getByLabelText(/^Link/), "https://example.com/job?x=1");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await user.click(await screen.findByRole("button", { name: "Acme" }));
    expect(screen.getByLabelText(/^Link/)).toHaveValue("https://example.com/job?x=1");
    expect(screen.getByText(/untrusted/i)).toBeInTheDocument();
    expect(container.querySelectorAll("a, [role='link']")).toHaveLength(0);
  });

  it("keeps_the_controls_when_a_status_change_leaves_the_filter", async () => {
    tracking();
    const { user } = renderApp();
    await open(user);
    await user.click(screen.getByRole("button", { name: "Add application" }));
    await user.type(screen.getByLabelText("Company"), "Acme");
    await user.type(screen.getByLabelText("Role"), "Engineer");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await user.selectOptions(screen.getByLabelText("Filter by status"), "saved");
    await user.click(await screen.findByRole("button", { name: "Acme" }));
    await user.selectOptions(await screen.findByLabelText("New status"), "applied");
    await user.click(screen.getByRole("button", { name: "Set status" }));
    await screen.findByText("No applications with this status.");
    expect(screen.getByRole("button", { name: "Set status" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Remove..." })).toBeInTheDocument();
  });

  it("a_same_status_needs_a_note_to_be_set", async () => {
    const api = tracking();
    const { user } = renderApp();
    await open(user);
    await user.click(screen.getByRole("button", { name: "Add application" }));
    await user.type(screen.getByLabelText("Company"), "Acme");
    await user.type(screen.getByLabelText("Role"), "Engineer");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await user.click(await screen.findByRole("button", { name: "Acme" }));
    const set = await screen.findByRole("button", { name: "Set status" });
    expect(screen.getByLabelText("New status")).toHaveValue("saved");
    expect(set).toBeDisabled();
    await user.type(screen.getByLabelText("Status note (optional)"), "still waiting");
    expect(set).toBeEnabled();
    await user.click(set);
    await waitFor(() => expect(count(api, "app_set_status")).toBe(1));
    expect(await screen.findByRole("list", { name: "History" })).toHaveTextContent("still waiting");
  });

  it("an_applied_date_is_not_a_day_early_west_of_utc", async () => {
    const zone = env.TZ;
    env.TZ = "America/Los_Angeles";
    try {
      const api = tracking();
      const { user } = renderApp();
      await open(user);
      await user.click(screen.getByRole("button", { name: "Add application" }));
      await user.type(screen.getByLabelText("Company"), "Acme");
      await user.type(screen.getByLabelText("Role"), "Engineer");
      await user.click(screen.getByRole("button", { name: "Save" }));
      await user.click(await screen.findByRole("button", { name: "Acme" }));
      await user.selectOptions(await screen.findByLabelText("New status"), "applied");
      await user.type(screen.getByLabelText("Applied on (optional)"), "2026-10-02");
      await user.click(screen.getByRole("button", { name: "Set status" }));
      await waitFor(() => expect(count(api, "app_set_status")).toBe(1));
      const cell = (await screen.findByRole("button", { name: "Acme" })).closest("tr")!;
      await waitFor(() => expect(cell).toHaveTextContent("Oct 2, 2026"));
      expect(cell).not.toHaveTextContent("Oct 1, 2026");
    } finally {
      if (zone === undefined) delete env.TZ;
      else env.TZ = zone;
    }
  });
});

const AT = "2026-11-01T10:00:00Z";

const questionOf = (id: string, text: string, over: Partial<Question> = {}): Question => ({
  id,
  text,
  class: "normal",
  class_source: "rules",
  country: null,
  source: "user",
  source_ref: null,
  findings: [],
  classified_at: null,
  created_at: AT,
  updated_at: AT,
  ...over,
});

const answerOf = (id: string, question_id: string, over: Partial<Answer> = {}): Answer => ({
  id,
  question_id,
  body: "",
  sealed: false,
  approved: false,
  version: 1,
  source: "user",
  source_ref: null,
  reviewed_at: AT,
  stale: false,
  created_at: AT,
  updated_at: AT,
  ...over,
});

/** A small in-memory store behind the memory commands. */
function bank(rows: QuestionRow[] = [], overrides: Record<string, (args: Args) => unknown> = {}) {
  let n = 0;
  const row = (id: unknown) => rows.find((r) => r.question.id === id)!;
  const answer = (id: unknown) => rows.flatMap((r) => r.answers).find((a) => a.id === id)!;
  return backend({
    memory_list: ({ class: wanted }: Args) => rows.filter((r) => !wanted || r.question.class === wanted),
    memory_add_question: ({ text, country }: Args) => {
      const question = questionOf(`q${++n}`, String(text), { country: (country as string | null) ?? null });
      rows.unshift({ question, answers: [] });
      return question;
    },
    memory_add_answer: ({ questionId, text, sourceRef }: Args) => {
      const added = answerOf(`a${++n}`, String(questionId), {
        body: String(text),
        source: sourceRef ? "knowledge" : "user",
        source_ref: (sourceRef as string | null) ?? null,
      });
      row(questionId).answers.push(added);
      return added;
    },
    memory_edit_answer: ({ answerId, text }: Args) => {
      const edited = answer(answerId);
      Object.assign(edited, { body: text, approved: false, version: edited.version + 1 });
      return edited;
    },
    memory_approve: ({ answerId, approved }: Args) => {
      answer(answerId).approved = Boolean(approved);
    },
    memory_mark_reviewed: ({ answerId }: Args) => {
      answer(answerId).stale = false;
    },
    memory_set_class: ({ questionId, class: to }: Args) => {
      row(questionId).question.class = to as MemoryClass;
      return row(questionId).question;
    },
    memory_remove_question: ({ id }: Args) => {
      rows.splice(rows.indexOf(row(id)), 1);
    },
    memory_remove_answer: ({ id }: Args) => {
      const owner = rows.find((r) => r.answers.some((a) => a.id === id))!;
      owner.answers.splice(owner.answers.indexOf(answer(id)), 1);
    },
    ...overrides,
  });
}

async function openMemory(user: ReturnType<typeof renderApp>["user"]) {
  await user.click(await screen.findByRole("button", { name: "Memory" }));
  await screen.findByRole("heading", { level: 1, name: "Career memory" });
}

const rowOf = async (text: string) => (await screen.findByText(text)).closest("li")!;

const ITEM: KnowledgeItem = {
  id: "k1",
  kind: "note",
  body: "Led the migration",
  source: "user",
  source_ref: null,
  approved: true,
  marked_sensitive: false,
  sealed: false,
};

describe("career memory", () => {
  it("ctrl_8_opens_career_memory", async () => {
    backend();
    const { user } = renderApp();
    await screen.findByRole("heading", { name: "Status" });
    await user.keyboard("{Control>}8{/Control}");
    expect(await screen.findByRole("heading", { level: 1, name: "Career memory" })).toBeInTheDocument();
    expect(await screen.findByText("No questions yet. Add one you are often asked.")).toBeInTheDocument();
  });

  it("memory_add_question_then_answer_then_approve", async () => {
    const api = bank([], { kb_list: () => [ITEM] });
    const { user } = renderApp();
    await openMemory(user);

    await user.type(screen.getByLabelText("Question"), "Why this job?");
    await user.type(screen.getByLabelText("Country (optional, two letters)"), "pt");
    await user.click(screen.getByRole("button", { name: "Add question" }));
    await waitFor(() => expect(count(api, "memory_add_question")).toBe(1));
    expect(api.calls.find((c) => c.cmd === "memory_add_question")!.args).toEqual({
      text: "Why this job?",
      country: "pt",
    });

    const row = await rowOf("Why this job?");
    await user.type(within(row).getByLabelText("Answer"), "Because");
    await user.selectOptions(within(row).getByLabelText("Source (optional)"), "k1");
    await user.click(within(row).getByRole("button", { name: "Add answer" }));
    await waitFor(() => expect(count(api, "memory_add_answer")).toBe(1));
    expect(api.calls.find((c) => c.cmd === "memory_add_answer")!.args).toEqual({
      questionId: "q1",
      text: "Because",
      sourceRef: "k1",
    });

    const added = await within(row).findByText("Because");
    expect(row).toHaveTextContent("Waiting");
    expect(row).toHaveTextContent("Source: knowledge, note: Led the migration");
    expect(added.closest("li")).toBeTruthy();
    await user.click(within(row).getByRole("button", { name: "Approve" }));
    await waitFor(() => expect(count(api, "memory_approve")).toBe(1));
    expect(api.calls.find((c) => c.cmd === "memory_approve")!.args).toEqual({ answerId: "a2", approved: true });
    expect(await within(row).findByRole("button", { name: "Unapprove" })).toBeInTheDocument();
    expect(row).toHaveTextContent("Approved");

    await user.click(within(row).getByRole("button", { name: "Reviewed" }));
    await waitFor(() => expect(count(api, "memory_mark_reviewed")).toBe(1));

    await user.click(within(row).getByRole("button", { name: "Edit" }));
    const editor = within(row).getByLabelText("New text");
    await user.clear(editor);
    await user.type(editor, "Since then");
    await user.click(within(row).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(count(api, "memory_edit_answer")).toBe(1));
    expect(await within(row).findByText("Since then")).toBeInTheDocument();
    expect(await within(row).findByRole("button", { name: "Approve" })).toBeInTheDocument();
  });

  it("a_sealed_answer_shows_only_on_request", async () => {
    const api = bank(
      [
        {
          question: questionOf("q1", "Your salary?", { class: "sensitive" }),
          answers: [answerOf("a1", "q1", { sealed: true, approved: true, stale: true })],
        },
      ],
      { memory_reveal: () => "Ninety thousand" },
    );
    const { user } = renderApp();
    await openMemory(user);
    const row = await rowOf("Your salary?");
    expect(within(row).getByText("Sealed")).toBeInTheDocument();
    expect(within(row).getByText("Stale")).toBeInTheDocument();
    expect(count(api, "memory_reveal")).toBe(0);
    expect(screen.queryByText("Ninety thousand")).toBeNull();

    await user.click(within(row).getByRole("button", { name: "Show" }));

    expect(await screen.findByText("Ninety thousand")).toBeInTheDocument();
    expect(api.calls.find((c) => c.cmd === "memory_reveal")!.args).toEqual({ answerId: "a1" });
    await user.click(within(row).getByRole("button", { name: "Hide" }));
    expect(screen.queryByText("Ninety thousand")).toBeNull();
    expect(count(api, "memory_reveal")).toBe(1);
  });

  it("a_refused_show_stays_sealed_and_shows_its_code", async () => {
    bank(
      [
        {
          question: questionOf("q1", "Your salary?", { class: "sensitive" }),
          answers: [answerOf("a1", "q1", { sealed: true })],
        },
      ],
      {
        memory_reveal: () => {
          throw { code: "confirmation_required", message: "confirmation required" };
        },
      },
    );
    const { user } = renderApp();
    await openMemory(user);
    await user.click(await screen.findByRole("button", { name: "Show" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("code: confirmation_required");
    expect(screen.getByText("Sealed")).toBeInTheDocument();
  });

  it("work_authorization_row_has_no_answer_form", async () => {
    bank([
      {
        question: questionOf("q1", "Are you authorized to work in Canada?", { class: "work_authorization" }),
        answers: [answerOf("a1", "q1", { sealed: true })],
      },
    ]);
    const { user } = renderApp();
    await openMemory(user);
    const row = await rowOf("Are you authorized to work in Canada?");
    expect(within(row).getByText("Answered from your profile, for one country at a time.")).toBeInTheDocument();
    expect(within(row).queryByLabelText("Answer")).toBeNull();
    expect(within(row).queryByRole("button", { name: "Add answer" })).toBeNull();
    // What a raise left behind can be read or removed, and never used.
    expect(within(row).queryByRole("button", { name: "Approve" })).toBeNull();
    expect(within(row).queryByRole("button", { name: "Edit" })).toBeNull();
    expect(within(row).getByRole("button", { name: "Show" })).toBeInTheDocument();
    expect(within(row).getByRole("button", { name: "Remove" })).toBeInTheDocument();
  });

  it("class_select_and_remove_question_reach_their_commands", async () => {
    const api = bank([{ question: questionOf("q1", "Why this job?"), answers: [] }]);
    const { user } = renderApp();
    await openMemory(user);
    const row = await rowOf("Why this job?");
    expect(within(row).getByText("Normal", { selector: "span" })).toBeInTheDocument();

    await user.selectOptions(within(row).getByLabelText("Class"), "legal");
    await waitFor(() => expect(count(api, "memory_set_class")).toBe(1));
    expect(api.calls.find((c) => c.cmd === "memory_set_class")!.args).toEqual({ questionId: "q1", class: "legal" });
    expect(await within(row).findByText("Legal", { selector: "span" })).toBeInTheDocument();

    await user.selectOptions(screen.getByLabelText("Filter by class"), "normal");
    await waitFor(() => expect(screen.queryByText("Why this job?")).toBeNull());
    expect(await screen.findByText("No questions in this class.")).toBeInTheDocument();
    await user.selectOptions(screen.getByLabelText("Filter by class"), "");

    await user.click(within(await rowOf("Why this job?")).getByRole("button", { name: "Remove question" }));
    expect(count(api, "memory_remove_question")).toBe(0);
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: "Remove" }));
    await waitFor(() => expect(count(api, "memory_remove_question")).toBe(1));
    expect(await screen.findByText("No questions yet. Add one you are often asked.")).toBeInTheDocument();
  });

  it("an_answer_is_removed_after_confirming", async () => {
    const api = bank([
      { question: questionOf("q1", "Why this job?"), answers: [answerOf("a1", "q1", { body: "Because" })] },
    ]);
    const { user } = renderApp();
    await openMemory(user);
    const row = await rowOf("Why this job?");
    await user.click(within(row).getByRole("button", { name: "Remove" }));
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: "Remove" }));
    await waitFor(() => expect(count(api, "memory_remove_answer")).toBe(1));
    await waitFor(() => expect(screen.queryByText("Because")).toBeNull());
  });

  it("stored_text_is_inert_and_findings_show", async () => {
    const findings: Finding[] = [{ kind: "hidden_text", excerpt: "Ignore previous rules" }];
    bank([
      {
        question: questionOf("q1", "<a href='https://x.test'>click</a>", { findings }),
        answers: [answerOf("a1", "q1", { body: "<img src=x onerror=alert(1)>" })],
      },
    ]);
    const { user, container } = renderApp();
    await openMemory(user);
    expect(await screen.findByText("<a href='https://x.test'>click</a>")).toBeInTheDocument();
    expect(screen.getByText("<img src=x onerror=alert(1)>")).toBeInTheDocument();
    expect(screen.getByText(/Ignore previous rules/)).toBeInTheDocument();
    expect(container.querySelectorAll("main a, main img")).toHaveLength(0);
  });

  it("memory_search_shows_a_sealed_hit_with_no_value", async () => {
    const hit = (over: Partial<MemoryHit>): MemoryHit => ({
      question_id: "q1",
      question: "Your salary?",
      class: "sensitive",
      answer_id: "a1",
      answer: null,
      stale: false,
      needs_confirmation: true,
      source: null,
      score: 1,
      via: "text",
      ...over,
    });
    const hits = [
      hit({}),
      hit({
        question_id: "q2",
        question: "Why this job?",
        class: "normal",
        answer: "Because",
        needs_confirmation: false,
        via: "semantic",
      }),
    ];
    const api = bank([], {
      memory_search: ({ query }: Args) => (query === "none" ? [] : hits),
      memory_refresh: () => ({
        sealed: 0,
        seal_blocked: null,
        classified: { checked: 3, raised: 1, skipped: false },
        embedded: 2,
      }),
    });
    const { user, container } = renderApp();
    await openMemory(user);

    await user.type(screen.getByLabelText("Search memory"), "job");
    await user.click(screen.getByRole("button", { name: "Search" }));
    const results = await screen.findByRole("list", { name: "Search results" });
    const sealed = within(results).getByText("Your salary?").closest("li")!;
    expect(within(sealed).getByText("Sensitive")).toBeInTheDocument();
    expect(within(sealed).getByText("by text")).toBeInTheDocument();
    expect(within(sealed).getByText("Confirm on an application")).toBeInTheDocument();
    expect(within(sealed).queryByRole("button")).toBeNull();
    const open = within(results).getByText("Why this job?").closest("li")!;
    expect(within(open).getByText("by meaning")).toBeInTheDocument();
    expect(within(open).getByText("Because")).toBeInTheDocument();
    expect(container.querySelectorAll("main a, main img")).toHaveLength(0);
    expect(api.calls.find((c) => c.cmd === "memory_search")?.args).toEqual({
      query: "job",
      limit: 10,
    });

    await user.click(screen.getByRole("button", { name: "Refresh index" }));
    expect(await screen.findByText("Checked 3, raised 1, indexed 2")).toBeInTheDocument();

    await user.clear(screen.getByLabelText("Search memory"));
    await user.type(screen.getByLabelText("Search memory"), "none");
    await user.click(screen.getByRole("button", { name: "Search" }));
    expect(await screen.findByText("Nothing matches.")).toBeInTheDocument();
  });

  it("refresh_says_when_no_ai_took_it", async () => {
    bank([], {
      memory_refresh: () => ({
        sealed: 0,
        seal_blocked: null,
        classified: { checked: 0, raised: 0, skipped: true },
        embedded: 0,
      }),
    });
    const { user } = renderApp();
    await openMemory(user);
    await user.click(screen.getByRole("button", { name: "Refresh index" }));
    expect(await screen.findByText("Checked 0, raised 0, indexed 0")).toBeInTheDocument();
    expect(screen.getByText("No AI took this; classes come from the rules.")).toBeInTheDocument();
  });

  it("refresh_says_when_a_key_left_answers_hidden", async () => {
    bank([], {
      memory_refresh: () => ({
        sealed: 0,
        seal_blocked: "key_unavailable",
        classified: { checked: 1, raised: 1, skipped: false },
        embedded: 0,
      }),
    });
    const { user } = renderApp();
    await openMemory(user);
    await user.click(screen.getByRole("button", { name: "Refresh index" }));
    expect(await screen.findByText(/stay hidden until they can be sealed \(key_unavailable\)/)).toBeInTheDocument();
  });

  it("refresh_shows_the_external_count_before_the_press", async () => {
    const api = bank([], {
      memory_refresh_plan: () => ({ pending: 3, external_calls: 3 }),
    });
    const { user } = renderApp();
    await openMemory(user);
    expect(await screen.findByText("At most 3 paid or subscription calls for 3 questions (20 questions a press)")).toBeInTheDocument();
    expect(api.calls.filter((c) => c.cmd === "memory_refresh")).toHaveLength(0);
    await user.click(screen.getByRole("button", { name: "Refresh index" }));
    await waitFor(() => expect(api.calls.filter((c) => c.cmd === "memory_refresh")).toHaveLength(1));
  });

  it("refresh_states_the_ceiling_when_two_providers_may_each_be_asked", async () => {
    bank([], { memory_refresh_plan: () => ({ pending: 3, external_calls: 6 }) });
    const { user } = renderApp();
    await openMemory(user);
    expect(await screen.findByText("At most 6 paid or subscription calls for 3 questions (20 questions a press)")).toBeInTheDocument();
    expect(screen.queryByText(/up to 20/i)).not.toBeInTheDocument();
  });

  it("refresh_names_one_call_and_no_call_in_plain_words", async () => {
    bank([], { memory_refresh_plan: () => ({ pending: 1, external_calls: 1 }) });
    const first = renderApp();
    await openMemory(first.user);
    expect(await screen.findByText("At most 1 paid or subscription call for 1 question (20 questions a press)")).toBeInTheDocument();
    first.unmount();

    bank([], { memory_refresh_plan: () => ({ pending: 2, external_calls: 0 }) });
    const { user } = renderApp();
    await openMemory(user);
    expect(await screen.findByText("No outside calls")).toBeInTheDocument();
  });

  it("a_cleared_body_offers_no_show_and_no_approve", async () => {
    bank([
      {
        question: questionOf("q1", "Your salary?", { class: "sensitive" }),
        answers: [answerOf("a1", "q1")],
      },
    ]);
    const { user } = renderApp();
    await openMemory(user);
    const row = await rowOf("Your salary?");
    expect(within(row).queryByRole("button", { name: "Show" })).toBeNull();
    expect(within(row).getByRole("button", { name: "Approve" })).toBeDisabled();
    expect(within(row).getByText(/cleared/)).toBeInTheDocument();
  });

  it("provenance_names_an_item_that_has_gone", async () => {
    bank([
      {
        question: questionOf("q1", "Why this job?"),
        answers: [answerOf("a1", "q1", { body: "Because", source: "knowledge", source_ref: "gone1" })],
      },
    ]);
    const { user } = renderApp();
    await openMemory(user);
    expect(await rowOf("Why this job?")).toHaveTextContent("Source: knowledge, gone1");
  });

  it("a_refused_lowering_says_to_reword_the_question", async () => {
    bank(
      [{ question: questionOf("q1", "Are you authorized to work?", { class: "work_authorization" }), answers: [] }],
      {
        memory_set_class: () => {
          throw { code: "refused", message: "refused: use_profile_by_country" };
        },
      },
    );
    const { user } = renderApp();
    await openMemory(user);
    const row = await rowOf("Are you authorized to work?");
    await user.selectOptions(within(row).getByLabelText("Class"), "normal");
    expect(await within(row).findByRole("alert")).toHaveTextContent("code: refused");
    expect(within(row).getByText(/in other words/)).toBeInTheDocument();
  });

  it("an_export_says_what_it_left_out", async () => {
    backend({ data_export: () => ({ path: "/home/me/jobpilot/exports/e1", withheld: 2 }) });
    const { user } = renderApp();
    await user.click(await screen.findByRole("button", { name: "Data" }));
    await user.click(await screen.findByRole("button", { name: "Export all data..." }));
    await user.click(await screen.findByRole("button", { name: "Write plain text export" }));
    expect(await screen.findByText(/\/home\/me\/jobpilot\/exports\/e1/)).toBeInTheDocument();
    expect(await screen.findByText(/2 sealed career answers were left out/)).toBeInTheDocument();
  });

  it("a_refusal_shows_its_code", async () => {
    bank([], {
      memory_add_question: () => {
        throw { code: "validation", message: "invalid input: a country is two letters" };
      },
    });
    const { user } = renderApp();
    await openMemory(user);
    await user.type(screen.getByLabelText("Question"), "Why?");
    await user.click(screen.getByRole("button", { name: "Add question" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("code: validation");
  });
});

describe("answers for an application", () => {
  const SECRET = "ZXQ-SENSITIVE-TEXT";
  const application: Application = {
    id: "app1",
    company: "Acme",
    role: "Engineer",
    job_text_id: null,
    resume_draft_id: null,
    letter_draft_id: null,
    link: "",
    country: "US",
    notes: "",
    status: "saved",
    applied_on: null,
    created_at: AT,
    updated_at: AT,
  };

  /** A bank of one normal, one sensitive, and one work authorization question. */
  function answers(overrides: Record<string, (args: Args) => unknown> = {}) {
    const applied: AppliedAnswer[] = [];
    const rows: QuestionRow[] = [
      {
        question: questionOf("q1", "Why this job?"),
        answers: [answerOf("a1", "q1", { approved: true })],
      },
      {
        question: questionOf("q2", "What is your salary?", { class: "sensitive" }),
        answers: [answerOf("a2", "q2", { approved: true, sealed: true }), answerOf("a3", "q2")],
      },
      {
        question: questionOf("q3", "Are you authorized to work here?", { class: "work_authorization", country: "CA" }),
        answers: [],
      },
      {
        question: questionOf("q4", "Do you need sponsorship?", { class: "work_authorization" }),
        answers: [],
      },
    ];
    const texts: Record<string, string> = { a1: "Because I like it", a2: SECRET };
    const api = backend({
      app_list: () => [application],
      app_get: () => application,
      memory_list: () => rows,
      memory_application_answers: () => applied,
      memory_confirm_use: ({ answerId }: Args) => {
        const found = rows.flatMap((r) => r.answers).find((a) => a.id === answerId)!;
        const question = rows.find((r) => r.answers.includes(found))!.question;
        applied.push({
          answer_id: found.id,
          question: question.text,
          class: question.class,
          confirmed_at: AT,
          used_at: null,
          current: true,
        });
      },
      memory_use: ({ answerId, applicationId }: Args) => ({
        application_id: applicationId,
        answer_id: answerId,
        class: answerId === "a1" ? "normal" : "sensitive",
        text: texts[String(answerId)],
        stale: false,
      }),
      memory_authorization: ({ country }: Args): AuthorizationAnswer => ({
        country: String(country),
        authorized: "not_stated",
        needs_sponsorship: "not_stated",
        note: "",
        stated: false,
      }),
      ...overrides,
    });
    return api;
  }

  async function select(user: ReturnType<typeof renderApp>["user"], option: RegExp) {
    await open(user);
    await user.click(await screen.findByRole("button", { name: "Acme" }));
    const picker = await screen.findByLabelText("Answer to use");
    const choice = within(picker).getByRole("option", { name: option });
    await user.selectOptions(picker, choice);
  }

  it("sensitive_answer_shows_confirm_and_copy_never_copy", async () => {
    const api = answers();
    const { user } = renderApp();
    const write = vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue();
    await select(user, /salary/);

    expect(screen.queryByRole("button", { name: "Copy" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Confirm and copy" }));

    expect(await screen.findByLabelText("Answer text")).toHaveValue(SECRET);
    expect(screen.getByLabelText("Answer text")).toHaveAttribute("readonly");
    await waitFor(() => expect(write).toHaveBeenCalledWith(SECRET));
    const order = api.calls.map((c) => c.cmd).filter((c) => c === "memory_confirm_use" || c === "memory_use");
    expect(order).toEqual(["memory_confirm_use", "memory_use"]);
    // The confirmation now matches, so the next press needs no dialog.
    expect(await screen.findByRole("button", { name: "Copy" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Confirm and copy" })).toBeNull();
  });

  it("a_normal_answer_copies_with_one_press_and_no_confirmation", async () => {
    const api = answers();
    const { user } = renderApp();
    vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue();
    await select(user, /Why this job/);

    await user.click(screen.getByRole("button", { name: "Copy" }));

    expect(await screen.findByLabelText("Answer text")).toHaveValue("Because I like it");
    expect(count(api, "memory_confirm_use")).toBe(0);
    expect(count(api, "memory_use")).toBe(1);
  });

  it("only_approved_answers_are_offered", async () => {
    answers();
    const { user } = renderApp();
    await select(user, /Why this job/);
    const picker = screen.getByLabelText("Answer to use");
    // The sensitive question has one approved and one unapproved answer.
    expect(within(picker).getAllByRole("option", { name: /salary/ })).toHaveLength(1);
  });

  it("a_cancelled_confirmation_shows_no_text", async () => {
    const api = answers({
      memory_confirm_use: () => {
        throw { code: "confirmation_required", message: "this needs your confirmation" };
      },
    });
    const { user } = renderApp();
    const write = vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue();
    await select(user, /salary/);

    await user.click(screen.getByRole("button", { name: "Confirm and copy" }));

    await waitFor(() => expect(count(api, "memory_confirm_use")).toBe(1));
    expect(count(api, "memory_use")).toBe(0);
    expect(screen.queryByLabelText("Answer text")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(write).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Confirm and copy" })).toBeInTheDocument();
  });

  it("a_clipboard_failure_leaves_the_text_in_the_field", async () => {
    answers();
    const { user } = renderApp();
    vi.spyOn(navigator.clipboard, "writeText").mockRejectedValue(new Error("denied"));
    await select(user, /Why this job/);

    await user.click(screen.getByRole("button", { name: "Copy" }));

    expect(await screen.findByLabelText("Answer text")).toHaveValue("Because I like it");
    expect(await screen.findByText(/could not copy/i)).toBeInTheDocument();
  });

  it("a_refusal_shows_its_code", async () => {
    answers({
      memory_use: () => {
        throw { code: "refused", message: "answer_not_approved" };
      },
    });
    const { user } = renderApp();
    await select(user, /Why this job/);
    await user.click(screen.getByRole("button", { name: "Copy" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("code: refused");
    expect(screen.queryByLabelText("Answer text")).toBeNull();
  });

  it("work_authorization_row_has_no_value_input", async () => {
    const api = answers();
    const { user } = renderApp();
    await select(user, /authorized to work/);

    // The question says CA; the application's own country (US) is not used.
    expect(screen.getByLabelText(/^Authorization country/)).toHaveValue("CA");
    expect(screen.queryByLabelText("Answer text")).toBeNull();
    expect(screen.queryByRole("button", { name: /copy/i })).toBeNull();
    // The only text box here is the country; no value can be typed for the answer.
    const boxes = within(screen.getByRole("region", { name: "Answers for this application" })).getAllByRole(
      "textbox",
    );
    expect(boxes).toHaveLength(1);

    await user.click(screen.getByRole("button", { name: "Show what I stated" }));
    expect(await screen.findByText("CA: Not stated by you")).toBeInTheDocument();
    expect(api.calls.find((c) => c.cmd === "memory_authorization")!.args).toEqual({ country: "CA" });
    expect(screen.getByRole("button", { name: "Open Profile" })).toBeInTheDocument();
  });

  it("a_question_with_no_country_asks_for_one", async () => {
    const api = answers();
    const { user } = renderApp();
    await select(user, /sponsorship/);

    expect(screen.getByLabelText(/^Authorization country/)).toHaveValue("");
    expect(screen.getByRole("button", { name: "Show what I stated" })).toBeDisabled();
    await user.type(screen.getByLabelText(/^Authorization country/), "pt");
    await user.click(screen.getByRole("button", { name: "Show what I stated" }));
    await waitFor(() => expect(count(api, "memory_authorization")).toBe(1));
    expect(api.calls.find((c) => c.cmd === "memory_authorization")!.args).toEqual({ country: "pt" });
  });

  it("a_refusal_from_the_use_step_shows_even_when_it_is_confirmation_required", async () => {
    answers({
      memory_use: () => {
        throw { code: "confirmation_required", message: "this needs your confirmation" };
      },
    });
    const { user } = renderApp();
    vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue();
    await select(user, /salary/);
    await user.click(screen.getByRole("button", { name: "Confirm and copy" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("code: confirmation_required");
  });

  it("a_stated_country_shows_its_values_as_text", async () => {
    answers({
      memory_authorization: ({ country }: Args) => ({
        country: String(country),
        authorized: "yes",
        needs_sponsorship: "no",
        note: "<b>citizen</b>",
        stated: true,
      }),
    });
    const { user, container } = renderApp();
    await select(user, /authorized to work/);
    await user.click(screen.getByRole("button", { name: "Show what I stated" }));
    expect(await screen.findByText(/Authorized: Yes/)).toBeInTheDocument();
    expect(screen.getByText(/Needs sponsorship: No/)).toBeInTheDocument();
    expect(screen.getByText("<b>citizen</b>")).toBeInTheDocument();
    expect(container.querySelectorAll("main a, main b")).toHaveLength(0);
  });
});

describe("agents", () => {
  const run = (over: Partial<RunSummary> = {}): RunSummary => ({
    id: "r1",
    agent: "tracking",
    state: "completed",
    step: 1,
    steps: 1,
    reason: null,
    actor: "gui",
    created_at: "2026-11-01T10:00:00Z",
    finished_at: "2026-11-01T10:00:01Z",
    ...over,
  });
  const row = (state: RunState, over: Partial<LogRow> = {}): LogRow => ({
    at: "2026-11-01T10:00:00Z",
    step: "update",
    state,
    duration_ms: 12,
    provider: null,
    data_categories: [],
    dropped: 0,
    outcome: "ok",
    ...over,
  });
  const agents = (runs: RunSummary[], log: LogRow[] = []) =>
    backend({
      agents_list: () => runs,
      agent_get: ({ id }: Args): RunDetail => ({ run: runs.find((r) => r.id === id)!, log }),
      agent_cancel: () => undefined,
      agent_resume: ({ id }: Args): RunDetail => ({ run: runs.find((r) => r.id === id)!, log }),
    });
  const openAgents = async (user: ReturnType<typeof renderApp>["user"]) => {
    await user.click(await screen.findByRole("button", { name: "Agents" }));
    await screen.findByRole("heading", { level: 1, name: "Agents" });
  };

  it("ctrl_9_opens_agents", async () => {
    backend({ agents_list: () => [] });
    const { user } = renderApp();
    await screen.findByRole("heading", { name: "Status" });
    await user.keyboard("{Control>}9{/Control}");
    expect(await screen.findByRole("heading", { level: 1, name: "Agents" })).toBeInTheDocument();
    expect(await screen.findByText("No agent has run yet.")).toBeInTheDocument();
  });

  it("agents_view_lists_runs_and_shows_a_log", async () => {
    agents(
      [run({ id: "r1", state: "failed", step: 2, steps: 3, reason: "timeout", agent: "cover_letter" })],
      [row("checking"), row("checkpointed", { provider: "ollama", data_categories: ["job_text", "resume_text"] })],
    );
    const { user } = renderApp();
    await openAgents(user);

    const cell = await screen.findByRole("button", { name: "Cover letter" });
    const line = cell.closest("tr")!;
    expect(line).toHaveTextContent("Failed");
    expect(line).toHaveTextContent("2 of 3");
    expect(line).toHaveTextContent("timeout");

    await user.click(cell);
    const log = await screen.findByRole("table", { name: "Run log" });
    expect(within(log).getByText("Checkpointed")).toBeInTheDocument();
    expect(within(log).getByText("ollama")).toBeInTheDocument();
    expect(within(log).getByText("job_text, resume_text")).toBeInTheDocument();
    expect(within(log).getAllByText("12 ms")).toHaveLength(2);
    expect(screen.queryByRole("button", { name: "Resume" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Cancel run" })).toBeNull();
  });

  it("resume_is_offered_only_on_a_halted_run", async () => {
    const api = agents([
      run({ id: "r1", agent: "tracking", state: "halted", reason: "interrupted", step: 0 }),
      run({ id: "r2", agent: "analytics", state: "running", step: 0 }),
    ]);
    const { user } = renderApp();
    await openAgents(user);

    await user.click(await screen.findByRole("button", { name: "Analytics" }));
    expect(await screen.findByRole("button", { name: "Cancel run" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Resume" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Cancel run" }));
    await waitFor(() => expect(count(api, "agent_cancel")).toBe(1));
    expect(api.calls.find((c) => c.cmd === "agent_cancel")!.args).toEqual({ id: "r2" });

    await user.click(screen.getByRole("button", { name: "Tracking" }));
    await user.click(await screen.findByRole("button", { name: "Resume" }));
    await waitFor(() => expect(count(api, "agent_resume")).toBe(1));
    expect(screen.getByRole("button", { name: "Cancel run" })).toBeInTheDocument();
  });

  it("a_refused_resume_shows_its_code", async () => {
    backend({
      agents_list: () => [run({ state: "halted", reason: "paused", step: 0 })],
      agent_get: (): RunDetail => ({ run: run({ state: "halted", step: 0 }), log: [] }),
      agent_resume: () => {
        throw { code: "paused", message: "emergency pause is on" };
      },
    });
    const { user } = renderApp();
    await openAgents(user);
    await user.click(await screen.findByRole("button", { name: "Tracking" }));
    await user.click(await screen.findByRole("button", { name: "Resume" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("emergency pause is on");
    expect(alert).toHaveTextContent("code: paused");
  });
});
