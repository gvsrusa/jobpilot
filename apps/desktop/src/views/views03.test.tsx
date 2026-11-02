import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type {
  Answer,
  Application,
  ApplicationEvent,
  ApplicationInput,
  ApplicationStatus,
  Finding,
  KnowledgeItem,
  MemoryClass,
  Question,
  QuestionRow,
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

describe("applications", () => {
  it("ctrl_7_opens_applications", async () => {
    backend();
    const { user } = renderApp();
    await screen.findByRole("heading", { name: "Status" });
    await user.keyboard("{Control>}7{/Control}");
    expect(await screen.findByRole("heading", { level: 1, name: "Applications" })).toBeInTheDocument();
    expect(await screen.findByText("No applications yet. Add the first one.")).toBeInTheDocument();
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
    expect(row).toHaveTextContent("Source: knowledge");
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
