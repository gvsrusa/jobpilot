import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type {
  AiStatus,
  Compare,
  Draft,
  JobText,
  KeyState,
  KnowledgeItem,
  Profile,
  ProfileBasic,
  Status,
} from "../lib/ipc";
import { backend, count, READY, renderApp } from "../test-utils";

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
  claude_code: { available: false, path: null, consent: null },
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
  locked: false,
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
      await screen.findByText("Sign in by running `claude` once in the terminal panel"),
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
        aiWith({ claude_code: { available: true, path: "/usr/local/bin/claude", consent: null } }),
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

  it("ai_claude_not_found_offers_the_native_picker", async () => {
    const b = backend({ ai_status: () => AI_OFF, ai_set_claude_path: () => undefined });
    const { user } = renderApp();
    await go(user, "AI", /AI/);
    expect(await screen.findByText(/Claude Code not found/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Choose claude..." }));
    await waitFor(() => expect(count(b, "ai_set_claude_path")).toBe(1));
  });

  it("ai_order_moves_a_provider_and_saves", async () => {
    const b = backend({ ai_status: () => AI_OFF, ai_set_order: () => undefined });
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
      ...withStatus({ key_state: LOCKED, recovery_key_exported: false }),
      key_state: () => LOCKED,
    });
    const { user } = renderApp();
    await go(user, "Data", "Data and privacy");
    expect(await screen.findByText(/key id: k9/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Retry" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Import recovery key file..." })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Discard locked fields..." })).toBeInTheDocument();
    expect(screen.getByText("No recovery key exported")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Export recovery key..." })).toBeInTheDocument();
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
