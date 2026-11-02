import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type {
  AiStatus,
  Compare,
  ExternalStatus,
  Draft,
  JobText,
  KeyState,
  KnowledgeItem,
  Profile,
  ProfileBasic,
  Status,
} from "../lib/ipc";
import { explainReason } from "../lib/reasons";
import { backend, count, READY, renderApp, sharing } from "../test-utils";

const NOTHING: ProfileBasic = {
  name: "",
  email: "",
  countries: [],
  regions: [],
  cities: [],
  currencies: [],
  time_zones: [],
  languages: [],
  work_modes: [],
  employment_types: [],
};
const UNLOCKED: KeyState = { state: "unlocked", key_id: "k1" };

const profile = (over: Partial<Profile> = {}, basic: Partial<ProfileBasic> = {}): Profile => ({
  basic: { ...NOTHING, ...basic },
  authorization: [],
  locked: false,
  key_state: UNLOCKED,
  ...over,
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

const aiWith = (over: Partial<AiStatus>): AiStatus => ({ ...AI_OFF, ...over });

const JOB: JobText = { id: "j1", title: "Backend Engineer, Acme", normalized: "Build <b>billing</b>.", findings: [] };

const draft = (over: Partial<Draft>): Draft => ({
  id: "d1",
  kind: "resume",
  lineage_id: "L1",
  version: 1,
  origin: "generated",
  title: "Backend Engineer, Acme",
  job_text_id: "j1",
  provider: "ollama",
  doc: { name: "Ada", contact: [], summary: [], sections: [] },
  created_at: "2026-11-01T10:00:00Z",
  ...over,
});

const item = (id: string, body: string): KnowledgeItem => ({
  id,
  kind: "resume_line",
  body,
  source: "user",
  source_ref: null,
  approved: true,
  marked_sensitive: false,
  sealed: false,
});

const COMPARE: Compare = {
  original: ["Led 4 engineers on billing."],
  generated: [
    { id: "c1", position: 0, text: "Led a team of 4 on billing.", evidence_ids: ["e1"], status: "ok" },
    { id: "c2", position: 1, text: "Expert in Kubernetes.", evidence_ids: [], status: "no_evidence" },
    { id: "c3", position: 2, text: "Worked at Visa Inc.", evidence_ids: [], status: "authorization_statement" },
  ],
  evidence: [item("e1", "Led 4 engineers on billing")],
  blocked: 2,
};

const withStatus = (over: Partial<Status>) => ({ status: () => ({ ...READY, ...over }) });

async function go(user: ReturnType<typeof renderApp>["user"], nav: string, heading: string | RegExp) {
  await user.click(await screen.findByRole("button", { name: nav }));
  await screen.findByRole("heading", { level: 1, name: heading });
}

const resumeBackend = (over: Record<string, (a: Record<string, unknown>) => unknown> = {}) =>
  backend({
    job_list: () => [JOB],
    ai_status: () => AI_OFF,
    resume_list: () => [draft({})],
    resume_compare: () => COMPARE,
    kb_list: () => [item("e1", "Led 4 engineers on billing")],
    ...over,
  });

describe("profile", () => {
  it("profile_renders_chips_and_never_prefills_authorization", async () => {
    backend({ profile_get: () => profile({}, { countries: ["PT"] }) });
    const { user } = renderApp();
    await go(user, "Profile", "Profile");
    expect(await screen.findByRole("button", { name: "Remove PT" })).toBeInTheDocument();
    const row = screen.getByRole("row", { name: /PT/ });
    for (const select of within(row).getAllByRole("combobox")) {
      expect(select).toHaveValue("not_stated");
      expect(select).toHaveDisplayValue("Not stated");
    }
    expect(screen.getByText(/never works it out from your citizenship, location, or resume/)).toBeInTheDocument();
    expect(document.body.textContent).not.toMatch(/suggest/i);
  });

  it("profile_empty_state_has_no_country_row", async () => {
    backend({ profile_get: () => profile() });
    const { user } = renderApp();
    await go(user, "Profile", "Profile");
    expect(await screen.findByText("No country added yet.")).toBeInTheDocument();
  });

  it("profile_locked_fields_are_read_only", async () => {
    backend({
      profile_get: () =>
        profile({ locked: true, key_state: { state: "key_missing", key_id: "k9" } }, { countries: ["PT"] }),
    });
    const { user } = renderApp();
    await go(user, "Profile", "Profile");
    expect((await screen.findAllByText("Locked, see Recovery")).length).toBeGreaterThan(0);
    for (const select of within(screen.getByRole("row", { name: /PT/ })).getAllByRole("combobox")) {
      expect(select).toBeDisabled();
    }
  });

  it("profile_save_writes_basic_then_only_touched_authorization", async () => {
    const b = backend({
      profile_get: () => profile({}, { countries: ["PT", "CA"] }),
      profile_set_basic: ({ basic }) => basic,
      profile_set_authorization: () => undefined,
    });
    const { user } = renderApp();
    await go(user, "Profile", "Profile");
    const row = await screen.findByRole("row", { name: /PT/ });
    await user.selectOptions(within(row).getByLabelText("PT: what you state"), "yes");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(count(b, "profile_set_authorization")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "profile_set_authorization")?.args).toEqual({
      row: { country: "PT", authorized: "yes", needs_sponsorship: "not_stated", note: "" },
    });
    expect(count(b, "profile_set_basic")).toBe(1);
  });

  it("add_a_country_button_opens_the_countries_input", async () => {
    backend({ profile_get: () => profile() });
    const { user } = renderApp();
    await go(user, "Profile", "Profile");
    await user.click(await screen.findByRole("button", { name: "Add a country" }));
    expect(screen.getByRole("textbox", { name: "Add country" })).toHaveFocus();
  });

  it("chip_field_keyboard_adds_and_backspace_removes", async () => {
    backend({ profile_get: () => profile({}, { languages: ["English"] }) });
    const { user } = renderApp();
    await go(user, "Profile", "Profile");
    await user.click(await screen.findByRole("button", { name: "Add language" }));
    await user.keyboard("Portuguese{Enter}");
    expect(screen.getByRole("button", { name: "Remove Portuguese" })).toBeInTheDocument();
    await user.keyboard("{Backspace}");
    expect(screen.queryByRole("button", { name: "Remove Portuguese" })).toBeNull();
    expect(screen.getByRole("button", { name: "Remove English" })).toBeInTheDocument();
  });
});

describe("resumes", () => {
  it("resumes_compare_flags_claims_and_blocks_export", async () => {
    resumeBackend();
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    expect(await screen.findByText("Expert in Kubernetes.")).toBeInTheDocument();
    expect(screen.getAllByText("[E1]").length).toBeGreaterThan(0);
    expect(screen.getByText("OK")).toBeInTheDocument();
    expect(screen.getByText("NO EVIDENCE: remove or fix")).toBeInTheDocument();
    expect(screen.getByText("AUTHORIZATION STATEMENT: remove or fix")).toBeInTheDocument();
    expect(screen.getByText("2 claim(s) have no evidence. Export is blocked until it is fixed.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Export DOCX" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Export PDF" })).toBeDisabled();
    // why a Visa Inc. style line is held back
    expect(screen.getAllByText(/Visa Inc\./)).toHaveLength(2);
    expect(screen.getByText(/word for word/)).toBeInTheDocument();
  });

  it("resumes_show_the_posting_as_text_beside_the_draft", async () => {
    resumeBackend();
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    const posting = await screen.findByText(/Build <b>billing<\/b>\./);
    expect(posting).toBeInTheDocument();
    expect(document.querySelector("b")).toBeNull();
  });

  it("export_is_enabled_when_nothing_is_blocked_and_runs", async () => {
    const b = resumeBackend({
      resume_compare: () => ({ ...COMPARE, generated: [COMPARE.generated[0]], blocked: 0 }),
      resume_export: () => "/home/me/jobpilot/exports/a-v1.pdf",
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    const pdf = await screen.findByRole("button", { name: "Export PDF" });
    await waitFor(() => expect(pdf).toBeEnabled());
    await user.click(pdf);
    expect(await screen.findByText("/home/me/jobpilot/exports/a-v1.pdf")).toBeInTheDocument();
    expect(b.calls.find((c) => c.cmd === "resume_export")?.args).toEqual({
      draftId: "d1",
      template: "classic",
      format: "pdf",
    });
  });

  it("drafts_are_grouped_by_lineage_with_a_version_select", async () => {
    resumeBackend({
      resume_list: () => [
        draft({ id: "d3", version: 3, origin: "edited" }),
        draft({ id: "d2", version: 2 }),
        draft({ id: "d1", version: 1 }),
      ],
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    const version = await screen.findByLabelText("Version");
    expect(within(version).getAllByRole("option")).toHaveLength(3);
    expect(version).toHaveDisplayValue("v3 (current), edited");
    expect(screen.queryByLabelText("Draft")).toBeNull();
  });

  it("imported_resume_without_a_job_is_shown", async () => {
    let imported = false;
    const up = draft({ id: "u1", lineage_id: "U", origin: "uploaded", job_text_id: null, title: "My CV" });
    resumeBackend({
      resume_list: () => (imported ? [up] : []),
      resume_import: () => {
        imported = true;
        return { draft_id: "u1", lineage_id: "U", items: 3, warnings: [] };
      },
      resume_compare: () => ({ ...COMPARE, generated: [], blocked: 0 }),
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    await user.click(await screen.findByRole("button", { name: "Import resume..." }));
    expect(await screen.findByLabelText("Version")).toBeInTheDocument();
    expect(screen.queryByText("No generated resume for this job yet.")).toBeNull();
  });

  it("resumes_empty_state_offers_editor_without_backend", async () => {
    resumeBackend({ resume_list: () => [] });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    expect(await screen.findByText("No generated resume for this job yet.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open editor" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Generate" })).toBeNull();
  });

  it("resumes_empty_state_offers_generate_with_a_backend", async () => {
    resumeBackend({
      resume_list: () => [],
      ai_status: () =>
        aiWith({ ollama: { state: "running", url: "http://127.0.0.1:11434", version: "1", models: [] } }),
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    expect(await screen.findByRole("button", { name: "Generate" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Open editor" })).toBeNull();
  });

  it("manual_required_opens_the_editor_with_the_reason", async () => {
    resumeBackend({
      resume_list: () => [],
      ai_status: () =>
        aiWith({ ollama: { state: "running", url: "http://127.0.0.1:11434", version: "1", models: [] } }),
      resume_generate: () => {
        throw { code: "manual_required", message: "this needs manual work: no_evidence" };
      },
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    await user.click(await screen.findByRole("button", { name: "Generate" }));
    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    expect(screen.getByText(/approve some knowledge items first/i)).toBeInTheDocument();
  });

  const OLLAMA_UP = aiWith({ ollama: { state: "running", url: "http://127.0.0.1:11434", version: "1", models: [] } });

  it("no_evidence_points_at_the_knowledge_section", async () => {
    resumeBackend({
      resume_list: () => [],
      ai_status: () => OLLAMA_UP,
      resume_generate: () => {
        throw { code: "manual_required", message: "this needs manual work: no_evidence" };
      },
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    await user.click(await screen.findByRole("button", { name: "Generate" }));
    expect(await screen.findByText(/Knowledge section of this screen/)).toBeInTheDocument();
    expect(screen.queryByText(/kb approve/)).toBeNull();
  });

  it.each([
    ["consent_needed", /does not cover this task/],
    ["consent_suspended", /so it is paused and nothing is sent/],
    ["provider_unavailable", /cannot be reached/],
    ["no_model_set", /No local model is chosen/],
    ["model_not_installed", /not installed in Ollama/],
    ["rate_limited", /busy and asked to wait/],
    ["auth_failed", /refused the key/],
    ["quota_exhausted", /usage or credit has run out/],
  ])("manual_required_%s_is_not_called_switched_off", async (reason, text) => {
    resumeBackend({
      resume_list: () => [],
      ai_status: () => OLLAMA_UP,
      resume_generate: () => {
        throw { code: "manual_required", message: `this needs manual work: ${reason}` };
      },
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    await user.click(await screen.findByRole("button", { name: "Generate" }));
    expect(await screen.findByText(text)).toBeInTheDocument();
    expect(screen.queryByText(/switched on/)).toBeNull();
  });

  it.each(["consent_needed", "consent_suspended"])("%s_points_to_the_ai_screen_and_no_terminal", async (reason) => {
    resumeBackend({
      resume_list: () => [],
      ai_status: () => OLLAMA_UP,
      resume_generate: () => {
        throw { code: "manual_required", message: `this needs manual work: ${reason}` };
      },
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    await user.click(await screen.findByRole("button", { name: "Generate" }));
    expect(await screen.findByText(/AI screen/)).toBeInTheDocument();
    expect(document.body.textContent).not.toMatch(/jobpilot ai|claude_code/);
  });

  it("consent_needed_says_what_to_press", async () => {
    resumeBackend({
      resume_list: () => [],
      ai_status: () => OLLAMA_UP,
      resume_generate: () => {
        throw { code: "manual_required", message: "this needs manual work: consent_needed" };
      },
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    await user.click(await screen.findByRole("button", { name: "Generate" }));
    expect(await screen.findByText(/choose Review, then Allow what drafting needs/)).toBeInTheDocument();
  });

  it("failure_reasons_are_explained", async () => {
    resumeBackend({
      resume_list: () => [],
      ai_status: () =>
        aiWith({ ollama: { state: "running", url: "http://127.0.0.1:11434", version: "1", models: [] } }),
      resume_generate: () => {
        throw { code: "failed", message: "failed: not_logged_in" };
      },
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    await user.click(await screen.findByRole("button", { name: "Generate" }));
    expect(
      await screen.findByText(
        "This CLI is not signed in. Run it once in your own terminal to sign in, then try again.",
      ),
    ).toBeInTheDocument();
  });

  it("draft_editor_saves_a_new_version", async () => {
    const b = resumeBackend({
      resume_list: () => [],
      draft_save: () => draft({ id: "n1", lineage_id: "N", origin: "edited" }),
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    await user.click(await screen.findByRole("button", { name: "Open editor" }));
    await user.type(await screen.findByLabelText("Name"), "Ada Lovelace");
    await user.type(screen.getByLabelText("Contact (one per line)"), "ada@example.com");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(count(b, "draft_save")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "draft_save")?.args).toEqual({
      kind: "resume",
      lineageId: null,
      doc: { name: "Ada Lovelace", contact: ["ada@example.com"], summary: [], sections: [] },
    });
  });
});

describe("knowledge", () => {
  const CV = draft({ id: "u1", lineage_id: "U", origin: "uploaded", job_text_id: null, title: "My CV" });
  const parsed = (id: string, body: string, over: Partial<KnowledgeItem> = {}): KnowledgeItem => ({
    ...item(id, body),
    kind: "experience",
    source: "parsed",
    source_ref: `draft:u1#line:${id}`,
    approved: false,
    ...over,
  });
  const CLAUDE_READY = aiWith({
    claude_code: { available: true, path: "/opt/claude", consent: ["job_text", "profile_basic", "resume_text"], suspended: false },
    enabled: { ollama: true, claude_code: true },
    order: ["ollama", "claude_code"],
  });

  /** Items that answer to the commands, as the core would. */
  function store(start: KnowledgeItem[]) {
    let items = start;
    return {
      kb_list: () => items,
      kb_approve: ({ id, approved }: Record<string, unknown>) => {
        items = items.map((k) => (k.id === id ? { ...k, approved: approved as boolean } : k));
      },
      kb_mark_sensitive: ({ id, marked }: Record<string, unknown>) => {
        items = items.map((k) => (k.id === id ? { ...k, marked_sensitive: marked as boolean } : k));
      },
      kb_remove: ({ id }: Record<string, unknown>) => {
        items = items.filter((k) => k.id !== id);
      },
      kb_add: ({ kind, body }: Record<string, unknown>) => {
        const added = { ...item("n1", String(body)), kind: String(kind) };
        items = [added, ...items];
        return added;
      },
      approved: () => items.filter((k) => k.approved).map((k) => k.id),
    };
  }

  it("import_then_approve_then_generate_reaches_the_model", async () => {
    let imported = false;
    const kb = store([]);
    const lines = [parsed("p1", "Led 4 engineers"), parsed("p2", "Shipped billing")];
    let items = kb;
    const generated = draft({ id: "g1", lineage_id: "G" });
    const b = resumeBackend({
      ai_status: () => CLAUDE_READY,
      resume_list: () => (imported ? [CV] : []),
      resume_import: () => {
        imported = true;
        items = store(lines);
        return { draft_id: "u1", lineage_id: "U", items: 2, warnings: [] };
      },
      resume_compare: () => ({ ...COMPARE, generated: [], blocked: 0 }),
      kb_list: () => items.kb_list(),
      kb_approve: (args) => items.kb_approve(args),
      // The core's own rule: nothing approved, nothing is sent.
      resume_generate: () => {
        if (items.approved().length === 0) {
          throw { code: "manual_required", message: "this needs manual work: no_evidence" };
        }
        return { draft: generated, blocked: 0, provider: "claude_code" };
      },
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    await user.click(await screen.findByRole("button", { name: "Import resume..." }));
    expect(await screen.findByText("Led 4 engineers")).toBeInTheDocument();
    expect(screen.getByText(/From My CV/)).toBeInTheDocument();
    expect(screen.getByText(/2 lines imported/)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Approve all from this resume" }));
    await waitFor(() => expect(count(b, "kb_approve")).toBe(2));
    expect(b.calls.filter((c) => c.cmd === "kb_approve").map((c) => c.args)).toEqual([
      { id: "p1", approved: true },
      { id: "p2", approved: true },
    ]);

    await user.click(await screen.findByRole("button", { name: "Generate" }));
    await waitFor(() => expect(count(b, "resume_generate")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "resume_generate")?.args).toEqual({ jobTextId: "j1" });
    expect(screen.queryByText(/Nothing was generated/)).toBeNull();
  });

  it("export_choice_reaches_resume_generate", async () => {
    const generated = draft({ id: "g1", lineage_id: "G" });
    const b = resumeBackend({
      resume_list: () => [],
      ai_status: () => CLAUDE_READY,
      resume_compare: () => ({ ...COMPARE, generated: [], blocked: 0 }),
      resume_generate: () => ({
        draft: generated,
        blocked: 0,
        provider: "claude_code",
        exported: "/data/exports/backend-engineer-v1.pdf",
        export_skipped: null,
      }),
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    await user.selectOptions(await screen.findByLabelText("After drafting, export as"), "pdf");
    await user.click(await screen.findByRole("button", { name: "Generate" }));
    await waitFor(() => expect(count(b, "resume_generate")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "resume_generate")?.args).toEqual({
      jobTextId: "j1",
      export: { format: "pdf", template: "classic" },
    });
    expect(await screen.findByText("/data/exports/backend-engineer-v1.pdf")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Copy path" })).toBeInTheDocument();
  });

  it("a_refused_export_after_drafting_is_said_aloud", async () => {
    const generated = draft({ id: "g1", lineage_id: "G" });
    resumeBackend({
      resume_list: () => [],
      ai_status: () => CLAUDE_READY,
      resume_compare: () => ({ ...COMPARE, generated: [], blocked: 0 }),
      resume_generate: () => ({
        draft: generated,
        blocked: 0,
        provider: "claude_code",
        exported: null,
        export_skipped: "claims_blocked",
      }),
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    await user.selectOptions(await screen.findByLabelText("After drafting, export as"), "pdf");
    await user.click(await screen.findByRole("button", { name: "Generate" }));
    expect(await screen.findByText(/was not exported/)).toBeInTheDocument();
  });

  it("approve_all_skips_what_a_draft_could_not_cite_anyway", async () => {
    const kb = store([
      parsed("p1", "Open line"),
      parsed("p2", "Already fine", { approved: true }),
      parsed("p3", "Marked", { marked_sensitive: true }),
      parsed("p4", "", { sealed: true, marked_sensitive: true }),
    ]);
    const b = resumeBackend({ ...kb, resume_list: () => [CV] });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    await user.click(await screen.findByRole("button", { name: "Approve all from this resume" }));
    await waitFor(() => expect(count(b, "kb_approve")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "kb_approve")?.args).toEqual({ id: "p1", approved: true });
  });

  it("nothing_is_approved_until_the_user_presses", async () => {
    const b = resumeBackend({ ...store([parsed("p1", "Open line")]), resume_list: () => [CV] });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    await screen.findByText("Open line");
    expect(screen.getByText("Not approved")).toBeInTheDocument();
    expect(count(b, "kb_approve")).toBe(0);
  });

  it("each_item_can_be_approved_marked_and_removed", async () => {
    const b = resumeBackend({ ...store([parsed("p1", "Open line")]), resume_list: () => [CV] });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    const row = (await screen.findByText("Open line")).closest("li")!;
    await user.click(within(row).getByRole("button", { name: "Approve" }));
    await waitFor(() => expect(within(row).getByRole("button", { name: "Unapprove" })).toBeInTheDocument());

    await user.click(within(row).getByRole("button", { name: "Mark sensitive" }));
    await waitFor(() => expect(count(b, "kb_mark_sensitive")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "kb_mark_sensitive")?.args).toEqual({ id: "p1", marked: true });

    await user.click(await within(row).findByRole("button", { name: "Remove" }));
    expect(count(b, "kb_remove")).toBe(0);
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: "Remove" }));
    await waitFor(() => expect(count(b, "kb_remove")).toBe(1));
    await waitFor(() => expect(screen.queryByText("Open line")).toBeNull());
  });

  it("add_stores_a_typed_item_shown_as_the_users_own", async () => {
    const b = resumeBackend({ ...store([]), resume_list: () => [] });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    const add = await screen.findByRole("button", { name: "Add" });
    expect(add).toBeDisabled();
    await user.selectOptions(screen.getByLabelText("Kind of item"), "skill");
    await user.type(screen.getByLabelText("New item"), "Rust");
    await user.click(add);
    await waitFor(() => expect(count(b, "kb_add")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "kb_add")?.args).toEqual({ kind: "skill", body: "Rust" });
    expect(await screen.findByText("Added by you")).toBeInTheDocument();
  });

  it("a_sealed_item_shows_no_text_and_cannot_be_marked", async () => {
    resumeBackend({
      kb_list: () => [parsed("p1", "", { sealed: true, marked_sensitive: true })],
      resume_list: () => [CV],
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    const row = (await screen.findByText("Sealed")).closest("li")!;
    expect(within(row).getByRole("button", { name: "Unmark sensitive" })).toBeDisabled();
    expect(within(row).getByRole("button", { name: "Approve" })).toBeDisabled();
  });

  it("kb_show_reveals_a_sealed_item_on_request", async () => {
    const b = resumeBackend({
      kb_list: () => [parsed("p1", "", { sealed: true, marked_sensitive: true })],
      kb_reveal: () => "Treated for a rare condition",
      resume_list: () => [CV],
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    const row = (await screen.findByText("Sealed")).closest("li")!;
    // Listing never opens the body: nothing asked Rust to reveal until the press.
    expect(count(b, "kb_reveal")).toBe(0);
    expect(screen.queryByText("Treated for a rare condition")).toBeNull();

    await user.click(within(row).getByRole("button", { name: "Show" }));

    expect(await screen.findByText("Treated for a rare condition")).toBeInTheDocument();
    expect(count(b, "kb_reveal")).toBe(1);
    expect(b.calls.find((c) => c.cmd === "kb_reveal")?.args).toEqual({ id: "p1" });
    await user.click(within(row).getByRole("button", { name: "Hide" }));
    expect(screen.queryByText("Treated for a rare condition")).toBeNull();
    expect(within(row).getByRole("button", { name: "Show" })).toBeInTheDocument();
  });

  it("kb_show_with_no_key_shows_the_error_and_points_to_recovery", async () => {
    resumeBackend({
      kb_list: () => [parsed("p1", "", { sealed: true, marked_sensitive: true })],
      kb_reveal: () => {
        throw { code: "key_missing", message: "the key for this data is not on this computer" };
      },
      resume_list: () => [CV],
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    await user.click(await screen.findByRole("button", { name: "Show" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("key_missing");
    expect(screen.getByText(/see Recovery/)).toBeInTheDocument();
    expect(screen.getByText("Sealed")).toBeInTheDocument();
  });

  it("item_text_is_shown_as_text", async () => {
    resumeBackend({ kb_list: () => [parsed("p1", "<b>bold</b> line")], resume_list: () => [CV] });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    expect(await screen.findByText("<b>bold</b> line")).toBeInTheDocument();
    expect(document.querySelector("b")).toBeNull();
  });
});

describe("ai", () => {
  it("ai_not_running_state_hides_models_and_keeps_no_ai_text", async () => {
    backend({ ai_status: () => AI_OFF });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    expect(await screen.findByText(/No AI is needed\. JobPilot works with none\./)).toBeInTheDocument();
    expect(
      screen.getByText(
        "Ollama is not running on this computer. Start it, then check again. JobPilot does not install or start it.",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText("Installed models")).toBeNull();
    expect(screen.queryByText(/Available to download/)).toBeNull();
    expect(screen.getByRole("button", { name: "Check again" })).toBeInTheDocument();
  });

  const RUNNING = aiWith({
    ollama: {
      state: "running",
      url: "http://127.0.0.1:11434",
      version: "0.9",
      models: [
        { name: "llama3.2:3b", size: 2_000_000_000, local: true, family: "llama", parameter_size: "3B", fits: "fits" },
        { name: "big:70b", size: 40_000_000_000, local: true, family: "llama", parameter_size: "70B", fits: "too_large" },
        { name: "cloud:cloud", size: 0, local: false, family: "x", parameter_size: "", fits: "fits" },
      ],
    },
  });

  it("ai_lists_local_models_with_fit_wording", async () => {
    backend({ ai_status: () => RUNNING });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    expect(await screen.findByText("Installed models")).toBeInTheDocument();
    const installed = within(screen.getByRole("region", { name: "Installed models" }));
    expect(installed.getByText("Fits this computer")).toBeInTheDocument();
    expect(installed.getByText("Too large for this computer")).toBeInTheDocument();
    expect(screen.queryByText("cloud:cloud")).toBeNull();
  });

  it("ai_download_needs_dialog_confirmation", async () => {
    const b = backend({ ai_status: () => RUNNING, ai_pull: () => undefined });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    await screen.findByText("Installed models");
    const rows = screen.getAllByRole("button", { name: /^Download/ });
    await user.click(rows[0]);
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText(/GB|MB/)).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(count(b, "ai_pull")).toBe(0);
    await user.click(screen.getAllByRole("button", { name: /^Download/ })[0]);
    await user.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Download" }));
    await waitFor(() => expect(count(b, "ai_pull")).toBe(1));
  });

  it("ai_claude_row_shows_path_and_consent_terms", async () => {
    const b = backend({
      ai_status: () =>
        aiWith({ claude_code: { available: true, path: "/usr/local/bin/claude", consent: null, suspended: false } }),
      ai_consent_give: () => undefined,
      ai_enable: () => undefined,
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    expect(await screen.findByText("Claude Code available /usr/local/bin/claude")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Review consent..." }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText(/Consumer Terms allow training unless the user opts out\./)).toBeInTheDocument();
    const allow = within(dialog).getByRole("button", { name: "Allow selected" });
    expect(allow).toBeDisabled();
    await user.click(within(dialog).getByLabelText("Job text"));
    await user.click(allow);
    await waitFor(() => expect(count(b, "ai_consent_give")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "ai_consent_give")?.args).toEqual({
      provider: "claude_code",
      categories: ["job_text"],
    });
    await waitFor(() => expect(count(b, "ai_enable")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "ai_enable")?.args).toEqual({ provider: "claude_code", enabled: true });
  });

  const claudeUp = (consent: AiStatus["claude_code"]["consent"]) =>
    aiWith({ claude_code: { available: true, path: "/opt/claude", consent, suspended: false } });
  const consentBackend = (consent: AiStatus["claude_code"]["consent"]) =>
    backend({
      ai_status: () => claudeUp(consent),
      ai_sharing: () => sharing({ given: consent }),
      ai_consent_give: () => undefined,
      ai_enable: () => undefined,
    });

  it("consent_names_what_drafting_needs_and_ticks_nothing_until_pressed", async () => {
    const b = consentBackend(null);
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    await user.click(await screen.findByRole("button", { name: "Review consent..." }));
    const dialog = within(await screen.findByRole("dialog"));
    expect(dialog.getByText(/need Job text, Basic profile, and Resume text/)).toBeInTheDocument();
    for (const box of dialog.getAllByRole("checkbox")) expect(box).not.toBeChecked();
    expect(count(b, "ai_consent_give")).toBe(0);

    await user.click(dialog.getByRole("button", { name: "Allow what drafting needs" }));

    await waitFor(() => expect(count(b, "ai_consent_give")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "ai_consent_give")?.args).toEqual({
      provider: "claude_code",
      categories: ["job_text", "profile_basic", "resume_text"],
    });
    await waitFor(() => expect(count(b, "ai_enable")).toBe(1));
  });

  it("a_refused_native_confirm_gives_nothing_and_does_not_enable", async () => {
    const b = backend({
      ai_status: () => claudeUp(null),
      ai_consent_give: () => {
        throw { code: "cancelled", message: "cancelled" };
      },
      ai_enable: () => undefined,
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    await user.click(await screen.findByRole("button", { name: "Review consent..." }));
    await user.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Allow what drafting needs" }));
    await waitFor(() => expect(count(b, "ai_consent_give")).toBe(1));
    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    expect(count(b, "ai_enable")).toBe(0);
  });

  it("partial_consent_says_what_is_missing_and_can_be_widened", async () => {
    const b = consentBackend(["job_text"]);
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    expect(await screen.findByText(/Drafting also needs Basic profile, Resume text/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Review consent..." }));
    const dialog = within(await screen.findByRole("dialog"));
    expect(dialog.getByLabelText("Job text")).toBeChecked();
    expect(dialog.getByLabelText("Resume text")).not.toBeChecked();
    await user.click(dialog.getByRole("button", { name: "Allow what drafting needs" }));
    await waitFor(() => expect(count(b, "ai_consent_give")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "ai_consent_give")?.args).toEqual({
      provider: "claude_code",
      categories: ["job_text", "profile_basic", "resume_text"],
    });
  });

  const external = (over: Partial<ExternalStatus>): ExternalStatus => ({
    name: "openai",
    label: "OpenAI",
    kind: "api",
    available: false,
    reason: "not_built",
    model: null,
    key_stored: false,
    path: null,
    consent: null,
    suspended: false,
    runs_job_text: true,
    newer_than_checked: false,
    ...over,
  });
  const SUSPENDED_LINE =
    "Suspended: this consent was given before the terms on file now. Review it once to turn it back on.";
  const CODEX = sharing({
    name: "codex_cli",
    label: "Codex CLI",
    key: "codex_cli/cli_login",
    destination: "OpenAI, through the codex program on this computer",
    data_use: "The terms of whatever sign-in the installed CLI uses.",
    tasks: sharing().tasks.map((t) => ({ ...t, runs: false, skipped_because: "no_tools" })),
  });
  const GOOGLE = sharing({
    name: "google",
    label: "Google Gemini",
    key: "google/paid_key",
    destination: "Google's Gemini API, at generativelanguage.googleapis.com",
    data_use: "Not used to improve products; logged for a limited period for abuse detection.",
    attestation: "This key belongs to a Google project with billing on. A free Gemini key is refused for personal data.",
  });
  const reviewOf = async (user: ReturnType<typeof renderApp>["user"], label: string) => {
    await go(user, "AI", /AI/);
    await user.click(await screen.findByRole("button", { name: `Review ${label}...` }));
    return within(await screen.findByRole("dialog"));
  };

  it("consent_dialog_shows_tasks_destination_and_terms", async () => {
    backend({
      ai_status: () => aiWith({ external: [external({ name: "google", label: "Google Gemini" })] }),
      ai_sharing: () => GOOGLE,
    });
    const { user } = renderApp();
    const dialog = await reviewOf(user, "Google Gemini");
    expect(dialog.getByRole("heading", { name: "Share data with Google Gemini (your own paid key)" })).toBeInTheDocument();
    expect(dialog.getByRole("heading", { name: "What will be sent" })).toBeInTheDocument();
    expect(dialog.getByText("Resume: Job text, Basic profile, Resume text")).toBeInTheDocument();
    expect(dialog.getByText(GOOGLE.destination)).toBeInTheDocument();
    expect(dialog.getByRole("heading", { name: "How Google Gemini uses it (terms read 2026-11-01)" })).toBeInTheDocument();
    expect(dialog.getByText(GOOGLE.data_use)).toBeInTheDocument();
    expect(dialog.getByText(GOOGLE.attestation!)).toBeInTheDocument();
    expect(dialog.getByText("Paste a posting to see this.")).toBeInTheDocument();
    expect(dialog.queryByRole("button", { name: "Show exactly what will be sent" })).toBeNull();
    for (const box of dialog.getAllByRole("checkbox")) expect(box).not.toBeChecked();
  });

  it("google_consent_shows_the_paid_key_line", async () => {
    backend({
      ai_status: () => aiWith({ external: [external({ name: "google", label: "Google Gemini" }), external({})] }),
      ai_sharing: () => GOOGLE,
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    const row = within(await screen.findByRole("listitem", { name: "Google Gemini" }));
    expect(row.getByText("Free keys are not supported.")).toBeInTheDocument();
    const other = within(screen.getByRole("listitem", { name: "OpenAI" }));
    expect(other.queryByText("Free keys are not supported.")).toBeNull();
    await user.click(row.getByRole("button", { name: "Review Google Gemini..." }));
    const dialog = within(await screen.findByRole("dialog"));
    expect(dialog.getByRole("heading", { name: "Share data with Google Gemini (your own paid key)" })).toBeInTheDocument();
    expect(dialog.getByText(GOOGLE.attestation!)).toBeInTheDocument();
  });

  it("consent_dialog_says_why_a_task_is_skipped", async () => {
    backend({
      ai_status: () => aiWith({ external: [external({ name: "codex_cli", label: "Codex CLI", runs_job_text: false })] }),
      ai_sharing: () => CODEX,
    });
    const { user } = renderApp();
    const dialog = await reviewOf(user, "Codex CLI");
    expect(dialog.getByText("Resume: Skipped: this provider cannot prove it has no tools")).toBeInTheDocument();
    expect(dialog.queryByRole("button", { name: "Allow what drafting needs" })).toBeNull();
    expect(dialog.queryByText(/Job text goes to/)).toBeNull();
  });

  it("a_missing_cli_shows_unavailable_and_a_chooser", async () => {
    const b = backend({
      ai_status: () =>
        aiWith({
          external: [external({ name: "gemini_cli", label: "Gemini CLI", kind: "cli", reason: "not_installed" })],
        }),
      ai_set_cli_path: () => undefined,
      ai_clear_cli_path: () => undefined,
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    const row = within(await screen.findByRole("listitem", { name: "Gemini CLI" }));
    expect(row.getByText("Unavailable: not installed")).toBeInTheDocument();
    expect(row.queryByRole("button", { name: /Use automatic/ })).toBeNull();
    await user.click(row.getByRole("button", { name: "Choose gemini..." }));
    await waitFor(() => expect(count(b, "ai_set_cli_path")).toBe(1));
    // Only the provider name crosses: the picker is Rust's.
    expect(b.calls.find((c) => c.cmd === "ai_set_cli_path")?.args).toEqual({ provider: "gemini_cli" });
  });

  it("a_found_cli_shows_its_path_and_goes_back_to_automatic", async () => {
    const b = backend({
      ai_status: () =>
        aiWith({
          external: [
            external({ name: "codex_cli", label: "Codex CLI", kind: "cli", available: true, reason: null, path: "/opt/codex" }),
          ],
        }),
      ai_clear_cli_path: () => undefined,
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    const row = within(await screen.findByRole("listitem", { name: "Codex CLI" }));
    expect(row.getByText("/opt/codex")).toBeInTheDocument();
    expect(row.queryByRole("button", { name: "Choose codex..." })).toBeNull();
    await user.click(row.getByRole("button", { name: "Use automatic" }));
    await waitFor(() => expect(count(b, "ai_clear_cli_path")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "ai_clear_cli_path")?.args).toEqual({ provider: "codex_cli" });
  });

  const keyRow = async (user: ReturnType<typeof renderApp>["user"]) => {
    await go(user, "AI", /AI/);
    return within(await screen.findByRole("listitem", { name: "OpenAI" }));
  };

  it("paste_key_sends_only_the_provider_name", async () => {
    const b = backend({
      ai_status: () => aiWith({ external: [external({})] }),
      ai_key_from_clipboard: () => null,
    });
    const { user } = renderApp();
    const row = await keyRow(user);
    expect(row.getByText("No key")).toBeInTheDocument();
    expect(row.getByText("Copy your key first. The key stays out of this window.")).toBeInTheDocument();
    expect(row.queryByRole("button", { name: /Remove key/ })).toBeNull();
    await user.click(row.getByRole("button", { name: /Paste key from clipboard\.\.\./ }));
    await waitFor(() => expect(count(b, "ai_key_from_clipboard")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "ai_key_from_clipboard")?.args).toEqual({ provider: "openai" });
  });

  it("remove_key_sends_only_the_provider_name", async () => {
    const b = backend({
      ai_status: () => aiWith({ external: [external({ key_stored: true })] }),
      ai_key_remove: () => null,
    });
    const { user } = renderApp();
    const row = await keyRow(user);
    expect(row.getByText("Key stored")).toBeInTheDocument();
    await user.click(row.getByRole("button", { name: /Remove key/ }));
    await waitFor(() => expect(count(b, "ai_key_remove")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "ai_key_remove")?.args).toEqual({ provider: "openai" });
  });

  it("a_refused_paste_shows_its_code", async () => {
    backend({
      ai_status: () => aiWith({ external: [external({})] }),
      ai_key_from_clipboard: () => {
        throw { code: "validation", message: "invalid input: the clipboard does not hold a key" };
      },
    });
    const { user } = renderApp();
    const row = await keyRow(user);
    await user.click(row.getByRole("button", { name: /Paste key from clipboard\.\.\./ }));
    const alert = await row.findByRole("alert");
    expect(alert).toHaveTextContent("the clipboard does not hold a key");
    expect(alert).toHaveTextContent("code: validation");
  });

  it("an_endpoint_row_takes_a_key_too", async () => {
    const b = backend({
      ai_status: () =>
        aiWith({ external: [external({ name: "endpoint", label: "Compatible endpoint", kind: "endpoint" })] }),
      ai_key_from_clipboard: () => null,
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    const row = within(await screen.findByRole("listitem", { name: "Compatible endpoint" }));
    await user.click(row.getByRole("button", { name: /Paste key from clipboard\.\.\./ }));
    await waitFor(() => expect(count(b, "ai_key_from_clipboard")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "ai_key_from_clipboard")?.args).toEqual({ provider: "endpoint" });
  });

  it("a_key_stored_with_a_clipboard_failure_shows_both", async () => {
    let stored = false;
    backend({
      ai_status: () => aiWith({ external: [external({ key_stored: stored })] }),
      ai_key_from_clipboard: () => {
        stored = true;
        throw { code: "failed", message: "the key is stored, but the clipboard could not be cleared. Clear it yourself" };
      },
    });
    const { user } = renderApp();
    const row = await keyRow(user);
    await user.click(row.getByRole("button", { name: /Paste key from clipboard\.\.\./ }));
    expect(await screen.findByText("Key stored")).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("Clear it yourself");
  });

  it("gemini_row_shows_it_runs_job_text_with_no_tools", async () => {
    backend({
      ai_status: () =>
        aiWith({
          external: [
            external({
              name: "gemini_cli",
              label: "Gemini CLI",
              kind: "cli",
              available: true,
              reason: null,
              path: "/opt/bin/gemini",
            }),
            external({ name: "codex_cli", label: "Codex CLI", kind: "cli", runs_job_text: false }),
          ],
        }),
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    const gemini = within(await screen.findByRole("listitem", { name: "Gemini CLI" }));
    expect(gemini.getByText("/opt/bin/gemini")).toBeInTheDocument();
    expect(gemini.getByText("Runs job text with every tool denied.")).toBeInTheDocument();
    const codex = within(screen.getByRole("listitem", { name: "Codex CLI" }));
    expect(codex.queryByText("Runs job text with every tool denied.")).toBeNull();
  });

  it("a_cli_past_the_checked_version_says_so_and_no_hard_stop", async () => {
    backend({
      ai_status: () =>
        aiWith({
          external: [
            external({
              name: "gemini_cli",
              label: "Gemini CLI",
              kind: "cli",
              available: true,
              reason: null,
              newer_than_checked: true,
            }),
          ],
        }),
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    const row = within(await screen.findByRole("listitem", { name: "Gemini CLI" }));
    expect(row.getByText("Runs job text with every tool denied.")).toBeInTheDocument();
    expect(row.getByText("This version is newer than the one JobPilot checked.")).toBeInTheDocument();
  });

  it("the_gemini_row_says_the_cli_keeps_a_transcript", async () => {
    backend({
      ai_status: () =>
        aiWith({
          external: [
            external({ name: "gemini_cli", label: "Gemini CLI", kind: "cli", reason: null, available: true }),
            external({ name: "codex_cli", label: "Codex CLI", kind: "cli", runs_job_text: false }),
          ],
        }),
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    const text = /keeps its own transcript of each prompt under ~\/\.gemini\/tmp, 30 days by default/;
    expect(within(await screen.findByRole("listitem", { name: "Gemini CLI" })).getByText(text)).toBeInTheDocument();
    expect(within(screen.getByRole("listitem", { name: "Codex CLI" })).queryByText(text)).toBeNull();
  });

  it("an_off_cli_past_the_quick_look_can_be_turned_on_and_comes_up", async () => {
    let on = false;
    const row = () =>
      external({
        name: "gemini_cli",
        label: "Gemini CLI",
        kind: "cli",
        consent: ["job_text"],
        available: on,
        reason: on ? null : "not_searched",
        path: on ? "/home/me/.nvm/bin/gemini" : null,
      });
    const b = backend({
      ai_status: () => aiWith({ external: [row()], enabled: { ollama: true, claude_code: false, gemini_cli: on } }),
      ai_enable: () => {
        on = true;
        return null;
      },
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    const shown = within(await screen.findByRole("listitem", { name: "Gemini CLI" }));
    expect(shown.getByText("Unavailable: turn on to look for it")).toBeInTheDocument();
    expect(shown.queryByText(/not installed/)).toBeNull();
    const use = shown.getByLabelText("Use Gemini CLI");
    expect(use).toBeEnabled();
    await user.click(use);
    expect(b.calls.find((c) => c.cmd === "ai_enable")?.args).toEqual({ provider: "gemini_cli", enabled: true });
    expect(await screen.findByText("/home/me/.nvm/bin/gemini")).toBeInTheDocument();
    expect(within(screen.getByRole("listitem", { name: "Gemini CLI" })).queryByText(/Unavailable/)).toBeNull();
  });

  it("codex_row_says_it_runs_no_task_today", async () => {
    backend({
      ai_status: () =>
        aiWith({
          external: [
            external({ name: "codex_cli", label: "Codex CLI", kind: "cli", available: true, reason: null, runs_job_text: false }),
          ],
        }),
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    const row = within(await screen.findByRole("listitem", { name: "Codex CLI" }));
    expect(row.getByText("No task can use this yet: it cannot prove it has no tools.")).toBeInTheDocument();
    expect(row.queryByText("Runs job text with every tool denied.")).toBeNull();
  });

  it("an_off_codex_only_the_login_shell_finds_says_why_and_can_still_be_turned_on", async () => {
    let on = false;
    const row = () =>
      external({
        name: "codex_cli",
        label: "Codex CLI",
        kind: "cli",
        consent: ["profile_basic"],
        available: on,
        reason: on ? null : "not_searched",
        path: on ? "/home/me/.nvm/bin/codex" : null,
        runs_job_text: false,
      });
    const b = backend({
      ai_status: () => aiWith({ external: [row()], enabled: { ollama: true, claude_code: false, codex_cli: on } }),
      ai_enable: () => {
        on = true;
        return null;
      },
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    const shown = within(await screen.findByRole("listitem", { name: "Codex CLI" }));
    expect(shown.getByText("Unavailable: turn on to look for it")).toBeInTheDocument();
    expect(shown.getByText("No task can use this yet: it cannot prove it has no tools.")).toBeInTheDocument();
    const use = shown.getByLabelText("Use Codex CLI");
    expect(use).toBeEnabled();
    await user.click(use);
    expect(b.calls.find((c) => c.cmd === "ai_enable")?.args).toEqual({ provider: "codex_cli", enabled: true });
    expect(await screen.findByText("/home/me/.nvm/bin/codex")).toBeInTheDocument();
  });

  it("the_account_refused_reason_is_a_hint_naming_antigravity", async () => {
    resumeBackend({
      resume_list: () => [],
      ai_status: () =>
        aiWith({ ollama: { state: "running", url: "http://127.0.0.1:11434", version: "1", models: [] } }),
      resume_generate: () => {
        throw { code: "failed", message: "failed: cli_account_refused" };
      },
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    await user.click(await screen.findByRole("button", { name: "Generate" }));
    expect(await screen.findByText(/may have moved to Antigravity CLI/)).toBeInTheDocument();
  });

  it("a_cli_too_old_says_why", async () => {
    backend({
      ai_status: () =>
        aiWith({
          external: [external({ name: "gemini_cli", label: "Gemini CLI", kind: "cli", reason: "cli_too_old" })],
        }),
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    const row = within(await screen.findByRole("listitem", { name: "Gemini CLI" }));
    expect(row.getByText("Unavailable: older than the version JobPilot checked")).toBeInTheDocument();
  });

  it("a_cli_row_has_no_key_controls", async () => {
    backend({
      ai_status: () => aiWith({ external: [external({ name: "gemini_cli", label: "Gemini CLI", kind: "cli" })] }),
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    const row = within(await screen.findByRole("listitem", { name: "Gemini CLI" }));
    expect(row.queryByRole("button", { name: /Paste key/ })).toBeNull();
    expect(row.queryByText("No key")).toBeNull();
  });

  it("a_key_field_does_not_exist", async () => {
    backend({ ai_status: () => aiWith({ external: [external({ key_stored: true })] }) });
    const { user } = renderApp();
    await keyRow(user);
    expect(document.querySelector("input[type=password]")).toBeNull();
  });

  it("an_api_row_sets_a_model", async () => {
    const b = backend({
      ai_status: () => aiWith({ external: [external({ reason: "no_api_model", model: "gpt-old" })] }),
      ai_set_api_model: () => null,
    });
    const { user } = renderApp();
    const row = await keyRow(user);
    expect(row.getByText("Unavailable: no model chosen")).toBeInTheDocument();
    const field = row.getByLabelText("Model id");
    expect(field).toHaveValue("gpt-old");
    await user.clear(field);
    await user.type(field, "gpt-new");
    await user.click(row.getByRole("button", { name: "OpenAI: Save model id" }));
    await waitFor(() => expect(count(b, "ai_set_api_model")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "ai_set_api_model")?.args).toEqual({ provider: "openai", model: "gpt-new" });
  });

  it("a_refused_model_shows_its_code", async () => {
    backend({
      ai_status: () => aiWith({ external: [external({ name: "openrouter", label: "OpenRouter" })] }),
      ai_set_api_model: () => {
        throw { code: "refused", message: "refused: origin_refused" };
      },
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    const row = within(await screen.findByRole("listitem", { name: "OpenRouter" }));
    await user.type(row.getByLabelText("Model id"), "meta-llama/x");
    await user.click(row.getByRole("button", { name: "OpenRouter: Save model id" }));
    const alert = await row.findByRole("alert");
    expect(alert).toHaveTextContent("code: refused");
    expect(alert).toHaveTextContent("not eligible through OpenRouter");
  });

  it("a_cli_row_has_no_model_field", async () => {
    backend({
      ai_status: () => aiWith({ external: [external({ name: "gemini_cli", label: "Gemini CLI", kind: "cli" })] }),
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    const row = within(await screen.findByRole("listitem", { name: "Gemini CLI" }));
    expect(row.queryByLabelText("Model id")).toBeNull();
    expect(row.queryByRole("button", { name: "Add endpoint..." })).toBeNull();
  });

  const endpointRow = async (user: ReturnType<typeof renderApp>["user"]) => {
    await go(user, "AI", /AI/);
    return within(await screen.findByRole("listitem", { name: "Compatible endpoint" }));
  };
  const ENDPOINT = { name: "endpoint", label: "Compatible endpoint", kind: "endpoint" } as const;

  it("the_endpoint_row_explains_loopback_only", async () => {
    const b = backend({
      ai_status: () => aiWith({ external: [external({ ...ENDPOINT, reason: "no_endpoint" })] }),
      ai_set_endpoint: () => null,
    });
    const { user } = renderApp();
    const row = await endpointRow(user);
    expect(row.getByText("Unavailable: no address set")).toBeInTheDocument();
    expect(row.getByText(/Loopback only: a server you run on this computer/)).toBeInTheDocument();
    expect(row.getByText(/It still needs your consent/)).toBeInTheDocument();
    expect(row.queryByLabelText("Endpoint address (loopback only)")).toBeNull();
    await user.click(row.getByRole("button", { name: "Add endpoint..." }));
    await user.type(row.getByLabelText("Endpoint address (loopback only)"), "http://127.0.0.1:1234/v1");
    await user.click(row.getByRole("button", { name: "Use this address..." }));
    await waitFor(() => expect(count(b, "ai_set_endpoint")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "ai_set_endpoint")?.args).toEqual({ url: "http://127.0.0.1:1234/v1" });
  });

  it("a_refused_endpoint_shows_its_code_and_keeps_the_address", async () => {
    backend({
      ai_status: () => aiWith({ external: [external({ ...ENDPOINT, reason: "no_endpoint" })] }),
      ai_set_endpoint: () => {
        throw { code: "refused", message: "refused: not_loopback" };
      },
    });
    const { user } = renderApp();
    const row = await endpointRow(user);
    await user.click(row.getByRole("button", { name: "Add endpoint..." }));
    const field = row.getByLabelText("Endpoint address (loopback only)");
    await user.type(field, "https://api.example.com/v1");
    await user.click(row.getByRole("button", { name: "Use this address..." }));
    expect(await row.findByRole("alert")).toHaveTextContent("code: refused");
    expect(field).toHaveValue("https://api.example.com/v1");
  });

  it("a_set_endpoint_shows_its_address_and_can_be_cleared", async () => {
    const b = backend({
      ai_status: () => aiWith({ external: [external({ ...ENDPOINT, reason: null, available: true, path: "http://127.0.0.1:1234/v1" })] }),
      ai_set_endpoint: () => null,
    });
    const { user } = renderApp();
    const row = await endpointRow(user);
    expect(row.getByText("http://127.0.0.1:1234/v1")).toBeInTheDocument();
    expect(row.getByRole("button", { name: "Change endpoint..." })).toBeInTheDocument();
    await user.click(row.getByRole("button", { name: "Clear endpoint..." }));
    await waitFor(() => expect(count(b, "ai_set_endpoint")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "ai_set_endpoint")?.args).toEqual({ url: "" });
  });

  it("a_suspended_endpoint_says_its_address_changed", async () => {
    backend({
      ai_status: () =>
        aiWith({ external: [external({ ...ENDPOINT, reason: null, available: true, suspended: true, path: "http://127.0.0.1:1/v1" })] }),
    });
    const { user } = renderApp();
    const row = await endpointRow(user);
    expect(row.getByRole("alert")).toHaveTextContent(/endpoint address changed/);
    expect(row.getByRole("alert")).not.toHaveTextContent("Suspended: this consent was given before the terms");
  });

  it("the_consent_dialog_names_the_endpoint_address", async () => {
    backend({
      ai_status: () => aiWith({ external: [external({ ...ENDPOINT })] }),
      ai_sharing: () => sharing({ name: "endpoint", label: "Compatible endpoint", key: "endpoint/loopback", path: "http://127.0.0.1:1/v1" }),
    });
    const { user } = renderApp();
    const dialog = await reviewOf(user, "Compatible endpoint");
    expect(dialog.getByText("Address: http://127.0.0.1:1/v1")).toBeInTheDocument();
    expect(dialog.queryByText(/^Program:/)).toBeNull();
  });

  it("the_api_failure_reasons_read_in_plain_words", () => {
    const said = (word: string) => explainReason({ code: "manual_required", message: `this needs manual work: ${word}` });
    expect(said("no_key")).toMatch(/No key is stored/);
    expect(said("no_api_model")).toMatch(/No model is chosen/);
    expect(said("no_endpoint")).toMatch(/No endpoint address/);
    expect(said("origin_refused")).toMatch(/not eligible through OpenRouter/);
    expect(said("model_has_tools")).toMatch(/searches by itself/);
    expect(said("model_has_tools")).toMatch(/:online/);
    expect(said("model_has_tools")).toMatch(/computer-use/);
    expect(said("model_not_found")).toMatch(/does not know that model id/);
    expect(said("model_not_found")).toMatch(/Model id/);
    expect(said("model_refused")).toMatch(/declined this task/);
    expect(said("endpoint_changed")).toMatch(/endpoint address changed/);
  });

  it("consent_focus_starts_on_cancel", async () => {
    consentBackend(null);
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    await user.click(await screen.findByRole("button", { name: "Review consent..." }));
    const dialog = within(await screen.findByRole("dialog"));
    expect(dialog.getByRole("button", { name: "Cancel" })).toHaveFocus();
  });

  it("show_exactly_what_will_be_sent_is_inert_text", async () => {
    const b = backend({
      ai_status: () => aiWith({ external: [external({})] }),
      ai_sharing: () => sharing({ name: "openai", label: "OpenAI" }),
      job_list: () => [JOB],
      ai_preview: () => ({
        task: "draft_resume",
        system: "ZXQ-SYSTEM",
        user: '<script>window.pwned = true</script><a href="https://x">link</a>',
      }),
    });
    const { user } = renderApp();
    const dialog = await reviewOf(user, "OpenAI");
    await user.click(await dialog.findByRole("button", { name: "Show exactly what will be sent" }));
    const shown = await dialog.findByText(/ZXQ-SYSTEM/);
    expect(shown.tagName).toBe("PRE");
    expect(shown.textContent).toContain("<script>window.pwned = true</script>");
    expect(shown.querySelector("script, a")).toBeNull();
    expect(b.calls.find((c) => c.cmd === "ai_preview")?.args).toEqual({ jobTextId: "j1", provider: "openai" });
    expect(count(b, "ai_consent_give")).toBe(0);
  });

  it("a_suspended_row_says_so_and_offers_review", async () => {
    const b = backend({
      ai_status: () => aiWith({ external: [external({ suspended: true })] }),
      ai_sharing: () => sharing({ name: "openai", label: "OpenAI", suspended: true, given: ["job_text"] }),
      ai_consent_revoke: () => undefined,
      ai_enable: () => undefined,
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    const row = within(await screen.findByRole("listitem", { name: "OpenAI" }));
    expect(row.getByRole("alert")).toHaveTextContent(SUSPENDED_LINE);
    expect(row.getByText("Unavailable: not built yet")).toBeInTheDocument();
    await user.click(row.getByRole("button", { name: "Revoke consent for OpenAI" }));
    await waitFor(() => expect(count(b, "ai_consent_revoke")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "ai_consent_revoke")?.args).toEqual({ provider: "openai" });
    await user.click(row.getByRole("button", { name: "Review OpenAI..." }));
    expect(within(await screen.findByRole("dialog")).getByLabelText("Job text")).toBeChecked();
  });

  it("a_provider_row_shows_what_was_allowed_and_can_be_used", async () => {
    const b = backend({
      ai_status: () => aiWith({ external: [external({ available: true, reason: null, consent: ["job_text"] })] }),
      ai_enable: () => undefined,
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    expect(await screen.findByText("Consent given for: Job text.")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Register date 2026-11-01" })).toBeInTheDocument();
    await user.click(screen.getByLabelText("Use OpenAI"));
    await waitFor(() => expect(count(b, "ai_enable")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "ai_enable")?.args).toEqual({ provider: "openai", enabled: true });
  });

  it("claude_suspended_on_upgrade_says_why_and_review_starts_ticked", async () => {
    const b = backend({
      ai_status: () =>
        aiWith({
          claude_code: { available: true, path: "/opt/claude", consent: null, suspended: true },
          enabled: { ollama: true, claude_code: true },
        }),
      ai_sharing: () => sharing({ suspended: true, given: ["job_text", "profile_basic", "resume_text"] }),
      ai_consent_give: () => undefined,
      ai_enable: () => undefined,
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    expect(await screen.findByRole("alert")).toHaveTextContent(SUSPENDED_LINE);
    expect(screen.queryByText("Off. Claude Code is used only after you consent below.")).toBeNull();
    expect(screen.getAllByRole("button", { name: "Review consent..." })).toHaveLength(1);
    expect(screen.getAllByRole("button", { name: "Revoke consent" })).toHaveLength(1);
    expect(screen.queryByLabelText("Use Claude Code")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Review consent..." }));
    const dialog = within(await screen.findByRole("dialog"));
    for (const name of ["Job text", "Basic profile", "Resume text"]) expect(dialog.getByLabelText(name)).toBeChecked();
    await user.click(dialog.getByRole("button", { name: "Allow selected" }));
    await waitFor(() => expect(count(b, "ai_consent_give")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "ai_consent_give")?.args).toEqual({
      provider: "claude_code",
      categories: ["job_text", "profile_basic", "resume_text"],
    });
  });

  it("a_suspended_claude_can_be_revoked_and_that_turns_it_off", async () => {
    const b = backend({
      ai_status: () =>
        aiWith({
          claude_code: { available: true, path: "/opt/claude", consent: null, suspended: true },
          enabled: { ollama: true, claude_code: true },
        }),
      ai_consent_revoke: () => undefined,
      ai_enable: () => undefined,
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    await user.click(await screen.findByRole("button", { name: "Revoke consent" }));
    await waitFor(() => expect(count(b, "ai_enable")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "ai_consent_revoke")?.args).toEqual({ provider: "claude_code" });
    expect(b.calls.find((c) => c.cmd === "ai_enable")?.args).toEqual({ provider: "claude_code", enabled: false });
  });

  it("every_kind_of_row_offers_review_and_revoke_while_suspended", async () => {
    const rows = [
      external({ suspended: true }),
      external({ name: "endpoint", label: "Compatible endpoint", kind: "endpoint", suspended: true }),
      external({ name: "gemini_cli", label: "Gemini CLI", kind: "cli", suspended: true }),
    ];
    const b = backend({
      ai_status: () => aiWith({ external: rows }),
      ai_consent_revoke: () => undefined,
      ai_enable: () => undefined,
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    for (const { label } of rows) {
      const row = within(await screen.findByRole("listitem", { name: label }));
      expect(row.getByRole("button", { name: `Review ${label}...` })).toBeInTheDocument();
      expect(row.getByRole("button", { name: `Revoke consent for ${label}` })).toBeInTheDocument();
      expect(row.queryByLabelText(`Use ${label}`)).toBeNull();
    }
    await user.click(screen.getByRole("button", { name: "Revoke consent for Gemini CLI" }));
    await waitFor(() => expect(count(b, "ai_enable")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "ai_consent_revoke")?.args).toEqual({ provider: "gemini_cli" });
    expect(b.calls.find((c) => c.cmd === "ai_enable")?.args).toEqual({ provider: "gemini_cli", enabled: false });
  });

  it("use_stays_disabled_on_a_consented_row_that_cannot_run", async () => {
    backend({ ai_status: () => aiWith({ external: [external({ consent: ["job_text"] })] }) });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    expect(await screen.findByLabelText("Use OpenAI")).toBeDisabled();
  });

  it("resumes_view_says_why_generate_is_gone_while_suspended", async () => {
    for (const ai of [
      aiWith({
        claude_code: { available: true, path: "/opt/claude", consent: null, suspended: true },
        enabled: { ollama: false, claude_code: true },
      }),
      aiWith({ external: [external({ suspended: true })], enabled: { ollama: false, claude_code: false, openai: true } }),
    ]) {
      resumeBackend({ resume_list: () => [], ai_status: () => ai });
      const { user, unmount } = renderApp();
      await go(user, "Resumes", "Resumes");
      expect(await screen.findByRole("alert")).toHaveTextContent(`${SUSPENDED_LINE} Open the AI screen to review it.`);
      expect(screen.queryByRole("button", { name: "Generate" })).toBeNull();
      unmount();
    }
  });

  it("a_suspended_provider_that_is_off_says_nothing_on_resumes", async () => {
    resumeBackend({
      resume_list: () => [],
      ai_status: () => aiWith({ external: [external({ suspended: true })] }),
    });
    const { user } = renderApp();
    await go(user, "Resumes", "Resumes");
    await screen.findByText("No generated resume for this job yet.");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("the_not_available_list_has_no_controls", async () => {
    backend({ ai_status: () => AI_OFF });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    const section = within(await screen.findByRole("region", { name: "Not available" }));
    expect(section.getByText("Vertex AI: not supported.")).toBeInTheDocument();
    expect(section.getByText("Ollama cloud models: not supported.")).toBeInTheDocument();
    expect(section.getByText(/Antigravity CLI/)).toBeInTheDocument();
    expect(section.getByText(/free key: no personal data allowed/)).toBeInTheDocument();
    expect(section.getByText(/Only localhost works/)).toBeInTheDocument();
    expect(section.queryByText(/Sign in with ChatGPT/)).toBeNull();
    for (const role of ["button", "checkbox", "textbox", "link"]) expect(section.queryByRole(role)).toBeNull();
  });

  it("resumes_view_counts_a_hosted_provider_as_a_backend", async () => {
    const ready = external({ available: true, reason: null, consent: ["job_text", "profile_basic", "resume_text"] });
    const on = { ollama: false, claude_code: false, openai: true };
    resumeBackend({ resume_list: () => [], ai_status: () => aiWith({ external: [ready], enabled: on }) });
    const first = renderApp();
    await go(first.user, "Resumes", "Resumes");
    expect(await screen.findByRole("button", { name: "Generate" })).toBeInTheDocument();
    first.unmount();

    for (const row of [{ ...ready, runs_job_text: false }, { ...ready, consent: null }, { ...ready, available: false }]) {
      resumeBackend({ resume_list: () => [], ai_status: () => aiWith({ external: [row], enabled: on }) });
      const { user, unmount } = renderApp();
      await go(user, "Resumes", "Resumes");
      await screen.findByText("No generated resume for this job yet.");
      expect(screen.queryByRole("button", { name: "Generate" })).toBeNull();
      unmount();
    }
  });

  it("full_consent_asks_for_nothing_more", async () => {
    consentBackend(["job_text", "profile_basic", "resume_text"]);
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    await screen.findByRole("button", { name: "Revoke consent" });
    expect(screen.queryByText(/Drafting also needs/)).toBeNull();
    expect(screen.queryByRole("button", { name: "Review consent..." })).toBeNull();
  });

  it("ai_claude_not_found_offers_the_native_picker", async () => {
    const b = backend({ ai_status: () => AI_OFF, ai_set_claude_path: () => undefined });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    expect(await screen.findByText(/Claude Code not found/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Choose claude..." }));
    await waitFor(() => expect(count(b, "ai_set_claude_path")).toBe(1));
  });

  it("ai_use_automatic_clears_the_claude_path", async () => {
    const b = backend({
      ai_status: () =>
        aiWith({ claude_code: { available: true, path: "/opt/claude", consent: null, suspended: false } }),
      ai_clear_claude_path: () => undefined,
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    await user.click(await screen.findByRole("button", { name: "Use automatic" }));
    await waitFor(() => expect(count(b, "ai_clear_claude_path")).toBe(1));
  });

  it("suggested_models_use_core_fit_thresholds_and_say_estimated", async () => {
    // 6 GB: qwen2.5:7b needs 4.7*1.2=5.64 GB, over 75% (4.5) but within 6, so may be slow.
    backend({
      ai_status: () =>
        aiWith({
          ollama: { state: "running", url: "http://127.0.0.1:11434", version: "1", models: [] },
          hardware: { ...AI_OFF.hardware, total_memory: 6_000_000_000 },
        }),
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    const list = within(await screen.findByRole("region", { name: /Suggested models \(sizes approximate/ }));
    const row = list.getByText("qwen2.5:7b").closest("li")!;
    expect(within(row).getByText("Estimated: May be slow here")).toBeInTheDocument();
    expect(within(list.getByText("llama3.2:3b").closest("li")!).getByText("Estimated: Fits this computer")).toBeInTheDocument();
  });

  it("download_error_claims_network_only_for_network_codes", async () => {
    backend({
      ai_status: () =>
        aiWith({ ollama: { state: "running", url: "http://127.0.0.1:11434", version: "1", models: [] } }),
      ai_pull: () => {
        throw { code: "refused", message: "refused: not_local" };
      },
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    await user.click(await screen.findByRole("button", { name: "Download llama3.2:3b" }));
    await user.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Download" }));
    expect(await screen.findByText("refused: not_local")).toBeInTheDocument();
    expect(screen.queryByText(/needs a network/)).toBeNull();
  });

  it("ai_order_shows_only_what_the_router_will_try", async () => {
    backend({ ai_status: () => AI_OFF });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    const order = within(await screen.findByRole("region", { name: "Order" }));
    expect(order.getByText("ollama")).toBeInTheDocument();
    expect(order.queryByText("claude_code")).toBeNull();
  });

  it("ai_order_moves_a_provider_and_saves", async () => {
    const both = aiWith({ order: ["ollama", "claude_code"], enabled: { ollama: true, claude_code: true } });
    const b = backend({ ai_status: () => both, ai_set_order: () => undefined });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    await user.click(await screen.findByRole("button", { name: "Move claude_code up" }));
    await waitFor(() => expect(count(b, "ai_set_order")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "ai_set_order")?.args).toEqual({ order: ["claude_code", "ollama"] });
  });
});

describe("data", () => {
  const LOCKED = { state: "key_missing", key_id: "k9" } as const;

  it("data_shows_locked_panel_and_recovery_banner", async () => {
    backend({
      ...withStatus({ key_state: UNLOCKED, recovery_key_exported: false }),
      key_state: () => UNLOCKED,
    });
    const { user } = renderApp();
    await go(user, "Data", "Data and privacy");
    expect(await screen.findByText("No recovery key exported")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Export recovery key..." })).toBeInTheDocument();
  });

  it("fresh_install_shows_no_recovery_banner", async () => {
    backend({
      ...withStatus({ key_state: null, recovery_key_exported: false }),
      key_state: () => ({ state: "key_unavailable", key_id: "", reason: "no_key_yet" }),
    });
    const { user } = renderApp();
    await go(user, "Data", "Data and privacy");
    await screen.findByRole("heading", { name: "Backups" });
    expect(screen.queryByText("No recovery key exported")).toBeNull();
    expect(screen.queryByRole("button", { name: "Export recovery key..." })).toBeNull();
  });

  it("locked_panel_lists_import_and_discard", async () => {
    backend({ key_state: () => LOCKED });
    const { user } = renderApp();
    await go(user, "Data", "Data and privacy");
    expect(await screen.findByText(/key id: k9/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Retry" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Import recovery key file..." })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Discard locked fields..." })).toBeInTheDocument();
  });

  it("unavailable_key_offers_retry_but_no_import_or_discard", async () => {
    backend({
      key_state: () => ({ state: "key_unavailable", key_id: "k1", reason: "keychain locked" }),
    });
    const { user } = renderApp();
    await go(user, "Data", "Data and privacy");
    expect(await screen.findByRole("button", { name: "Retry" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Import recovery key file..." })).toBeNull();
    expect(screen.queryByRole("button", { name: "Discard locked fields..." })).toBeNull();
  });

  it("no_key_yet_and_not_loaded_show_no_locked_panel", async () => {
    backend({ key_state: () => ({ state: "key_unavailable", key_id: "", reason: "no_key_yet" }) });
    const { user } = renderApp();
    await go(user, "Data", "Data and privacy");
    await screen.findByRole("heading", { name: "Backups" });
    await waitFor(() => expect(screen.queryByText("Locked fields")).toBeNull());
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
  });

  it("swap_interrupted_alert_can_be_dismissed", async () => {
    const b = backend({ ...withStatus({ swap_interrupted: true }), swap_warning_clear: () => undefined });
    const { user } = renderApp();
    await go(user, "Data", "Data and privacy");
    expect(await screen.findByText("The last restore was interrupted. Check your data, then dismiss.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Dismiss" }));
    await waitFor(() => expect(count(b, "swap_warning_clear")).toBe(1));
  });

  it("restored_key_state_is_shown_when_not_unlocked", async () => {
    backend({ ...withStatus({ restored_key_state: { state: "key_missing", key_id: "kb" } }) });
    const { user } = renderApp();
    await go(user, "Data", "Data and privacy");
    expect(await screen.findByText(/The restored backup's key \(kb\) is not on this computer/)).toBeInTheDocument();
  });

  it("restore_dialog_says_consent_is_revoked", async () => {
    const info = { path: "/home/me/jobpilot/backups/manual-1", created_at: "2026-11-01T10:00:00Z", schema_version: 5 };
    backend({ backup_list: () => [info] });
    const { user } = renderApp();
    await go(user, "Data", "Data and privacy");
    await user.click((await screen.findAllByRole("button", { name: /^Restore/ }))[0]);
    expect(await screen.findByText(/consent you gave to Claude Code is revoked/i)).toBeInTheDocument();
  });

  it("ai_view_after_restore_shows_claude_off_and_asks_for_consent", async () => {
    const info = { path: "/home/me/jobpilot/backups/manual-1", created_at: "2026-11-01T10:00:00Z", schema_version: 5 };
    let restored = false;
    const on = aiWith({
      claude_code: { available: true, path: "/opt/claude", consent: ["job_text", "profile_basic", "resume_text"], suspended: false },
      enabled: { ollama: true, claude_code: true },
      order: ["ollama", "claude_code"],
    });
    // As core leaves it: consent revoked, and Claude Code switched off with it.
    const off = aiWith({
      claude_code: { available: true, path: "/opt/claude", consent: null, suspended: false },
      enabled: { ollama: true, claude_code: false },
      order: ["ollama", "claude_code"],
    });
    backend({
      backup_list: () => [info],
      backup_restore: () => {
        restored = true;
        return READY;
      },
      ai_status: () => (restored ? off : on),
    });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    expect(await screen.findByLabelText("Use Claude Code")).toBeChecked();

    await go(user, "Data", "Data and privacy");
    await user.click((await screen.findAllByRole("button", { name: /^Restore/ }))[0]);
    await user.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Replace my data" }));
    await waitFor(() => expect(restored).toBe(true));

    await go(user, "AI", /AI/);
    expect(await screen.findByText("Off. Claude Code is used only after you consent below.")).toBeInTheDocument();
    expect(screen.queryByLabelText("Use Claude Code")).toBeNull();
    expect(screen.getByRole("button", { name: "Review consent..." })).toBeEnabled();
    expect(screen.getByText("Claude Code is off")).toBeInTheDocument();
  });

  it("data_export_warns_before_writing_sensitive_fields", async () => {
    const b = backend({});
    const { user } = renderApp();
    await go(user, "Data", "Data and privacy");
    await user.click(await screen.findByRole("button", { name: "Export all data..." }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getAllByText(/plain text/).length).toBeGreaterThan(0);
    expect(within(dialog).getByText(/work authorization/i)).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(count(b, "data_export")).toBe(0);
    await user.click(screen.getByRole("button", { name: "Export all data..." }));
    await user.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Write plain text export" }));
    await waitFor(() => expect(count(b, "data_export")).toBe(1));
  });

  it("delete_dialog_explains_shared_key_ids", async () => {
    backend({});
    const { user } = renderApp();
    await go(user, "Data", "Data and privacy");
    await user.click(await screen.findByRole("button", { name: "Delete all data..." }));
    expect(await screen.findByText(/shares this folder's key id/)).toBeInTheDocument();
  });
});

describe("status", () => {
  it("status_shows_key_state_row", async () => {
    backend({ ...withStatus({ key_state: { state: "key_mismatch", key_id: "k2" } }) });
    const { user } = renderApp();
    expect(await screen.findByText("Encryption key")).toBeInTheDocument();
    expect(screen.getByText(/does not match this data/i)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Open Data" }));
    expect(await screen.findByRole("heading", { level: 1, name: "Data and privacy" })).toBeInTheDocument();
  });

  it("status_not_loaded_is_not_called_locked", async () => {
    backend({});
    renderApp();
    expect(await screen.findByText("Encryption key")).toBeInTheDocument();
    expect(screen.getByText("Not checked yet")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Open Data" })).toBeNull();
  });
});
