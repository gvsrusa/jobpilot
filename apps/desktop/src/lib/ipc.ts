// ponytail: hand-kept in sync with commands.rs; add tauri-specta when the command count makes drift a real problem.
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { open } from "@tauri-apps/plugin-dialog";

/** What every failed command rejects with (jobpilot-core `Error`). */
export type AppError = { code: string; message: string };

/** `state` is "recovery" when no database is open. An `Ok` answer can still be recovery. */
export type Status = {
  state: "ready" | "recovery";
  schema_version: number | null;
  app_version: string;
  data_root: string;
  error: AppError | null;
  /** Null with no database. Never asks the keychain, so `key_not_loaded` means not asked yet. */
  key_state: KeyState | null;
  swap_interrupted: boolean;
  /** False only while the active key has no recovery file, so also true with no key yet. */
  recovery_key_exported: boolean;
  /** What the last restore found about its backup's key. */
  restored_key_state: KeyState | null;
};

/** `key_not_loaded` says nothing about the key: it offers neither import nor discard. */
export type KeyState =
  | { state: "key_not_loaded" }
  | { state: "unlocked"; key_id: string }
  | { state: "key_unavailable"; key_id: string; reason: string }
  | { state: "key_missing"; key_id: string }
  | { state: "key_mismatch"; key_id: string };

export type RecoveryReceipt = { key_id: string; path: string };

export type WorkMode = "remote" | "hybrid" | "onsite";
export type ProfileBasic = {
  name: string;
  email: string;
  countries: string[];
  regions: string[];
  cities: string[];
  currencies: string[];
  time_zones: string[];
  languages: string[];
  work_modes: WorkMode[];
  employment_types: { country: string; kind: string }[];
};

export type WorkAuthorization = {
  country: string;
  authorized: Stated;
  needs_sponsorship: Stated;
  note: string;
};

export type Profile = {
  basic: ProfileBasic;
  authorization: WorkAuthorization[];
  locked: boolean;
  key_state: KeyState;
};

export type KnowledgeItem = {
  id: string;
  kind: string;
  /** Empty while `sealed`: `kbReveal` fetches it. Render it as text only. */
  body: string;
  source: string;
  source_ref: string | null;
  approved: boolean;
  marked_sensitive: boolean;
  /** The body is ciphertext. A list never opens it. */
  sealed: boolean;
};

export type Hit = { item_id: string; score: number; body: string };

export type Finding = { kind: string; excerpt: string };

/** `normalized` is untrusted text from a posting. Render it as text only. */
export type JobText = { id: string; title: string; normalized: string; findings: Finding[] };

export type Imported = {
  draft_id: string;
  lineage_id: string;
  items: number;
  warnings: string[];
};

export type Entry = { title: string; subtitle: string; dates: string; bullets: string[] };
export type Section = { heading: string; entries: Entry[] };
export type ResumeDoc = {
  name: string;
  contact: string[];
  summary: string[];
  sections: Section[];
};

export const STATUSES = [
  "saved",
  "applied",
  "screening",
  "interviewing",
  "offer",
  "accepted",
  "rejected",
  "withdrawn",
] as const;
export type ApplicationStatus = (typeof STATUSES)[number];

/** What the form sends. Optional ids are null when nothing is linked. */
export type ApplicationInput = {
  company: string;
  role: string;
  job_text_id: string | null;
  resume_draft_id: string | null;
  letter_draft_id: string | null;
  link: string;
  country: string;
  notes: string;
};

/** Core's `memory::Class`, lowest first. */
export const CLASSES = ["normal", "sensitive", "legal", "work_authorization"] as const;
export type MemoryClass = (typeof CLASSES)[number];

/** Render `text` as text only. It may come from outside the app. */
export type Question = {
  id: string;
  text: string;
  class: MemoryClass;
  class_source: string;
  country: string | null;
  source: string;
  source_ref: string | null;
  findings: Finding[];
  classified_at: string | null;
  created_at: string;
  updated_at: string;
};

export type Answer = {
  id: string;
  question_id: string;
  /** Empty under a sealed class: `memoryReveal` fetches it. Render it as text only. */
  body: string;
  /** There is text to show on request. False on a body cleared with its key. */
  sealed: boolean;
  approved: boolean;
  version: number;
  source: string;
  source_ref: string | null;
  reviewed_at: string;
  stale: boolean;
  created_at: string;
  updated_at: string;
};

export type QuestionRow = { question: Question; answers: Answer[] };

/** At most 20 questions go to a model per press, and an outside provider may be asked about each. */
export type RefreshPlan = { pending: number; external_calls: number };

/** `skipped`: no AI took the classifying, so classes are the rules'. */
export type MemoryRefreshed = {
  sealed: number;
  /** The error code that left answers hidden; they seal at the next refresh with a key. */
  seal_blocked: string | null;
  classified: { checked: number; raised: number; skipped: boolean };
  embedded: number;
};

/** One search hit. `answer` is set only for an approved answer of the normal class; a sealed
 * class has none and needs a confirmation on an application. Render text only. */
export type MemoryHit = {
  question_id: string;
  question: string;
  class: MemoryClass;
  answer_id: string | null;
  answer: string | null;
  stale: boolean;
  needs_confirmation: boolean;
  source: string | null;
  score: number;
  via: "semantic" | "text";
};

/** What `memoryUse` returns. The only place an answer's text reaches the screen for an
 * application. Render it as text only. */
export type UsedAnswer = {
  application_id: string;
  answer_id: string;
  class: MemoryClass;
  text: string;
  stale: boolean;
};

/** An answer an application has used or confirmed. It never holds the answer's text. */
export type AppliedAnswer = {
  answer_id: string;
  question: string;
  class: MemoryClass;
  confirmed_at: string | null;
  used_at: string | null;
  /** The confirmation matches the answer's version and the answer is not stale. */
  current: boolean;
};

export type Stated = "not_stated" | "yes" | "no";

/** Work authorization for one exact country, from the profile. `stated` is false when the
 * profile has no row for it. */
export type AuthorizationAnswer = {
  country: string;
  authorized: Stated;
  needs_sponsorship: Stated;
  note: string;
  stated: boolean;
};

export type Application = ApplicationInput & {
  id: string;
  status: ApplicationStatus;
  applied_on: string | null;
  created_at: string;
  updated_at: string;
};

export type ApplicationEvent = {
  id: number;
  at: string;
  actor: string;
  kind: "created" | "status" | "note" | "edited";
  from_status: ApplicationStatus | null;
  to_status: ApplicationStatus | null;
  note: string;
};

/** The emergency pause. `set_by` is `gui` or `cli`. */
export type PauseState = { paused: boolean; set_at: string | null; set_by: string | null };

export type Agent = "resume" | "cover_letter" | "career_memory" | "tracking" | "analytics";

export type RunState =
  | "queued"
  | "checking"
  | "running"
  | "checkpointed"
  | "retrying"
  | "waiting_for_human"
  | "halted"
  | "completed"
  | "manual_required"
  | "refused"
  | "failed"
  | "cancelled";

export type RunSummary = {
  id: string;
  agent: Agent;
  state: RunState;
  step: number;
  steps: number;
  reason: string | null;
  actor: string;
  created_at: string;
  finished_at: string | null;
};

export type LogRow = {
  at: string;
  step: string;
  state: RunState;
  duration_ms: number;
  provider: string | null;
  data_categories: DataCategory[];
  outcome: string;
  dropped: number;
};

export type RunDetail = { run: RunSummary; log: LogRow[] };

/** Counts and rates only. A rate with no applications behind it is null. */
export type Report = {
  id: string;
  created_at: string;
  volume: { total: number; by_status: Record<string, number>; applied_last_30_days: number };
  funnel: {
    applied: number;
    responded: number;
    interviews: number;
    offers: number;
    response_rate: number | null;
    offer_rate: number | null;
  };
  timing: { median_days_to_response: number | null; responses_counted: number };
};

export type DraftKind = "resume" | "cover_letter";
export type Template = "classic" | "compact";
export type ExportFormat = "docx" | "pdf";

export type Draft = {
  id: string;
  kind: DraftKind;
  lineage_id: string;
  version: number;
  origin: string;
  title: string;
  job_text_id: string | null;
  /** The current text of this draft's job, or `job_text_id` when no job holds it. */
  current_text_id: string | null;
  provider: string | null;
  doc: ResumeDoc;
  created_at: string;
};

export type Claim = {
  id: string;
  position: number;
  text: string;
  evidence_ids: string[];
  status: "ok" | "no_evidence" | "authorization_statement" | "removed";
};

export type ClaimInput = { text: string; evidence_ids: string[] };

export type Compare = {
  original: string[];
  generated: Claim[];
  evidence: KnowledgeItem[];
  blocked: number;
};

/** `blocked` claims are not `ok`, and each blocks export. */
export type Generated = {
  draft: Draft;
  blocked: number;
  provider: ProviderId;
  /** Where the file went, when an export was asked for and no claim was blocked. */
  exported: string | null;
  /** Set when an export was asked for and refused: a line, the name, a contact, or a heading. */
  export_skipped: string | null;
};

/** What to render right after drafting. Rust writes it to the exports folder only. */
export type ExportChoice = { format: ExportFormat; template: Template };

export type Fit = "fits" | "may_slow" | "too_large";

export type Model = {
  name: string;
  size: number;
  local: boolean;
  family: string;
  parameter_size: string;
  fits: Fit;
};

export type OllamaStatus =
  | { state: "running"; url: string; version: string; models: Model[] }
  | { state: "not_running"; url: string }
  | { state: "refused"; reason: string };

export type DataCategory =
  | "job_text"
  | "company_text"
  | "profile_basic"
  | "resume_text"
  | "career_memory"
  | "sensitive";

export type Provider =
  | "ollama"
  | "claude_code"
  | "openai"
  | "anthropic"
  | "google"
  | "openrouter"
  | "endpoint"
  | "gemini_cli"
  | "codex_cli";

/** Core's `ProviderId` as JSON. */
export type ProviderId = Provider | "no_ai";

export type ExternalStatus = {
  name: Provider;
  label: string;
  kind: "api" | "cli" | "endpoint";
  available: boolean;
  /** A reason code. Plain words are the screen's. */
  reason: string | null;
  model: string | null;
  key_stored: boolean;
  /** The program a CLI row runs, or the address an endpoint row sends to. */
  path: string | null;
  consent: DataCategory[] | null;
  suspended: boolean;
  runs_job_text: boolean;
  newer_than_checked: boolean;
};

export type TaskSharing = {
  task:
    | "parse_job"
    | "draft_resume"
    | "draft_cover_letter"
    | "classify_answer"
    | "semantic_match"
    | "summarize_company";
  categories: DataCategory[];
  runs: boolean;
  skipped_because: string | null;
};

export type Sharing = {
  name: Provider;
  label: string;
  key: string;
  destination: string;
  path: string | null;
  tasks: TaskSharing[];
  data_use: string;
  terms_date: string;
  attestation: string | null;
  suspended: boolean;
  given: DataCategory[] | null;
};

/** Holds the posting and evidence. Render it as text only. */
export type PromptPreview = { task: "draft_resume" | "draft_cover_letter"; system: string; user: string };

export type AiStatus = {
  ollama: OllamaStatus;
  claude_code: { available: boolean; path: string | null; consent: DataCategory[] | null; suspended: boolean };
  external: ExternalStatus[];
  register_date: string;
  hardware: { total_memory: number; cpus: number; gpu: string; free_disk: number };
  order: string[];
  enabled: { ollama: boolean; claude_code: boolean } & Partial<Record<Provider, boolean>>;
  drafting_model: string | null;
  embedding_model: string | null;
};

export type Document = {
  id: string;
  title: string;
  body: string;
  created_at: string;
  updated_at: string;
};

/** `snippet` is raw body text. Render it as text only. */
export type SearchHit = { id: string; title: string; snippet: string };

export type BackupInfo = {
  path: string;
  created_at: string;
  schema_version: number | null;
};

/** `withheld` counts sealed answers left out, because no key could open them. */
export type Exported = { path: string; withheld: number };

export type AuditEntry = {
  id: number;
  at: string;
  actor: string;
  action: string;
  target: string | null;
  detail: unknown;
};

export type PayPeriod = "hour" | "day" | "week" | "month" | "year";
/** `period` is null when the posting names none. It is never guessed. */
export type PayRange = { min: number | null; max: number | null; currency: string | null; period: PayPeriod | null };
export type PayWish = { amount: number; currency: string; period: PayPeriod };

/** Text fields come from outside postings. Render them as text only. */
export type Job = {
  id: string;
  job_text_id: string;
  /** The source: a connector name, or `import`. */
  connector: string;
  title: string;
  company: string | null;
  company_id: string | null;
  location: string;
  country: string | null;
  work_mode: string | null;
  employment_type: string | null;
  url: string | null;
  /** Shown on every card from a source that asks for it. */
  attribution: string | null;
  posted_on: string | null;
  pay: PayRange | null;
  seniority: string | null;
  skills: string[];
  fields_by: string;
  duplicate_of: string | null;
  duplicates: number;
  findings: number;
  first_seen_at: string;
  last_seen_at: string;
  score: number | null;
  /** A result is stored: with a null `score`, it had nothing to compare. */
  scored: boolean;
  stale: boolean;
  not_stated: string[];
};

export type JobFilter = {
  country: string | null;
  work_mode: WorkMode | null;
  min_pay: PayWish | null;
  query: string | null;
  include_duplicates: boolean;
};

export type PastedJob = { title: string; company: string; url: string | null; text: string };

export type ExplanationRow = {
  criterion: "skills" | "seniority" | "location" | "pay";
  weight: number;
  points: number | null;
  note: string;
};
export type Explanation = {
  rows: ExplanationRow[];
  score: number | null;
  compared_weight: number;
  matched_skills: string[];
  missing_skills: string[];
  by: string;
};
/** Each strength cites evidence that exists; the rest were cut and counted in `dropped`. */
export type Strength = { text: string; evidence_ids: string[] };
export type Semantic = {
  fit: number;
  summary: string;
  strengths: Strength[];
  gaps: string[];
  dropped: number;
  provider: string;
  at: string;
};
export type AuthNote =
  | { kind: "no_country" }
  | { kind: "not_stated"; country: string }
  | { kind: "stated"; country: string; authorized: Stated; needs_sponsorship: Stated }
  | { kind: "locked" };
export type CompanyNote = {
  id: string;
  text: string;
  origin: string;
  source_ref: string | null;
  provider: string | null;
  created_at: string;
};
export type Company = { id: string; name: string; jobs: number; notes: number };
export type JobDetail = {
  job: Job;
  text: string;
  findings: Finding[];
  explanation: Explanation | null;
  semantic: Semantic | null;
  group: Job[];
  authorization: AuthNote;
  mentions_authorization: boolean;
  notes: CompanyNote[];
};

export type SourceStatus = {
  name: string;
  label: string;
  read: "allowed" | "silent_read" | "user_assisted" | "blocked" | "unknown";
  can_fetch: boolean;
  operations: string[];
  attribution: string | null;
  date_read: string | null;
  target: "board" | "feed" | "none";
  min_interval_secs: number | null;
  per_day: number | null;
  /** The R1 note, on Greenhouse and Ashby. */
  note: string | null;
};
export type SourceRow = {
  id: string;
  connector: string;
  target: string;
  query: string;
  cursor: number;
  created_at: string;
};
export type SourceResult = {
  source_id: string;
  connector: string;
  target: string;
  added: number;
  updated: number;
  skipped: number;
  reason: string | null;
  /** When a rate-limited source may be read again, from the stored hold. */
  next_at: string | null;
};
export type DiscoveryReport = { sources: SourceResult[]; grouped: number };
export type MatchReport = { scored: number; parsed: number; evaluated: number; dropped: number; reason: string | null };
export type AiPlan = { jobs: number; external_calls: number };
export type CompanyResearch = {
  company_id: string;
  sources: number;
  facts: number;
  dropped: number;
  reason: string | null;
};
export type MatchPrefs = { min_pay: PayWish | null; seniority: string | null };

/** `browser status`: the pinned Playwright runtime and the Node it runs on. */
export type BrowserStatus = {
  state: "no_node" | "node_too_old" | "not_installed" | "wrong_version" | "ready";
  found: string | null;
  playwright_mcp: string;
};
export type DocState = "ready" | "missing" | { blocked: string };
export type AuthorizationState = "stated" | "not_stated" | "no_country";
/** One application as the preflight checked it. Counts only: no answer text. */
export type ItemCheck = {
  application_id: string;
  company: string;
  role: string;
  host: string;
  country: string;
  resume: DocState;
  letter: DocState;
  answers: number;
  stale_left_out: number;
  sealed: number;
  authorization: AuthorizationState;
  blocks: string[];
};
export type Preflight = {
  items: ItemCheck[];
  runtime: BrowserStatus["state"];
  claude: string | null;
  consent_missing: DataCategory[];
  sealed_total: number;
};
export type ApplyItem = {
  id: string;
  run_id: string;
  application_id: string | null;
  position: number;
  host: string;
  state: string;
  submit_clicks: number;
  auth_released: boolean;
  resume_path: string | null;
  letter_path: string | null;
  policy_note: string | null;
  /** The page's own words, untrusted: shown as plain text only. */
  confirmation_text: string | null;
  confirmation_url: string | null;
  status_written: boolean;
  reason: string | null;
  released_answers: string[];
};
export type PauseKind = "legal" | "sensitive" | "identity" | "captcha" | "mfa" | "login" | "other";
export type ApplyPause = {
  id: string;
  item_id: string;
  kind: PauseKind;
  origin: string;
  /** Page text, untrusted: shown as plain text only. */
  excerpt: string;
  created_at: string;
};
export type ApplyRunView = {
  id: string;
  mode: string;
  state: "ready" | "running" | "ended";
  items: ApplyItem[];
  active_pause: ApplyPause | null;
  created_at: string;
  ended_at: string | null;
};

export const ipc = {
  status: () => invoke<Status>("status"),
  retryOpen: () => invoke<Status>("retry_open"),
  documentsList: () => invoke<Document[]>("documents_list"),
  documentsCreate: (title: string, body: string) =>
    invoke<Document>("documents_create", { title, body }),
  documentsUpdate: (id: string, title: string, body: string) =>
    invoke<Document>("documents_update", { id, title, body }),
  documentsDelete: (id: string) => invoke<void>("documents_delete", { id }),
  documentsSearch: (query: string) => invoke<SearchHit[]>("documents_search", { query }),
  settingsGet: (key: string) => invoke<unknown>("settings_get", { key }),
  settingsSet: (key: string, value: unknown) => invoke<void>("settings_set", { key, value }),
  backupCreate: () => invoke<BackupInfo>("backup_create"),
  backupList: () => invoke<BackupInfo[]>("backup_list"),
  backupRestore: (source: string) => invoke<Status>("backup_restore", { source }),
  dataExport: () => invoke<Exported>("data_export"),
  dataDelete: (confirm: string, includeBackups: boolean) =>
    invoke<Status>("data_delete", { confirm, includeBackups }),
  secretSet: (name: string, value: string) => invoke<void>("secret_set", { name, value }),
  secretDelete: (name: string) => invoke<void>("secret_delete", { name }),
  secretNames: () => invoke<string[]>("secret_names"),
  auditRecent: (limit: number) => invoke<AuditEntry[]>("audit_recent", { limit }),
  swapWarningClear: () => invoke<void>("swap_warning_clear"),

  profileGet: () => invoke<Profile>("profile_get"),
  profileSetBasic: (basic: ProfileBasic) => invoke<ProfileBasic>("profile_set_basic", { basic }),
  profileSetAuthorization: (row: WorkAuthorization) =>
    invoke<void>("profile_set_authorization", { row }),
  profileRemoveAuthorization: (country: string) =>
    invoke<void>("profile_remove_authorization", { country }),

  kbList: () => invoke<KnowledgeItem[]>("kb_list"),
  kbAdd: (kind: string, body: string) => invoke<KnowledgeItem>("kb_add", { kind, body }),
  kbApprove: (id: string, approved: boolean) => invoke<void>("kb_approve", { id, approved }),
  kbMarkSensitive: (id: string, marked: boolean) =>
    invoke<void>("kb_mark_sensitive", { id, marked }),
  kbRemove: (id: string) => invoke<void>("kb_remove", { id }),
  /** Rust asks the user in a native dialog each time; script cannot answer it. */
  kbReveal: (id: string) => invoke<string>("kb_reveal", { id }),
  kbSearch: (query: string, limit: number) => invoke<Hit[]>("kb_search", { query, limit }),

  jobPaste: (title: string, raw: string) => invoke<JobText>("job_paste", { title, raw }),
  jobList: () => invoke<JobText[]>("job_list"),
  jobGet: (id: string) => invoke<JobText>("job_get", { id }),

  jobsList: (filter: JobFilter, limit: number) => invoke<Job[]>("jobs_list", { filter, limit }),
  jobDetail: (id: string) => invoke<JobDetail>("job_detail", { id }),
  jobImport: (input: PastedJob) => invoke<Job>("job_import", { input }),
  jobRemove: (id: string) => invoke<void>("job_remove", { id }),
  jobSplit: (id: string) => invoke<void>("job_split", { id }),
  /** Rust opens the stored link when it is https. The webview never names a URL. */
  jobOpen: (id: string) => invoke<void>("job_open", { id }),
  jobToApp: (id: string) => invoke<Application>("job_to_app", { id }),
  jobSourcesStatus: () => invoke<SourceStatus[]>("job_sources_status"),
  jobSourcesList: () => invoke<SourceRow[]>("job_sources_list"),
  jobSrcAdd: (connector: string, target: string, query: string) =>
    invoke<SourceRow>("job_src_add", { connector, target, query }),
  jobSrcRemove: (id: string) => invoke<void>("job_src_remove", { id }),
  jobsFetch: (srcIds: string[]) => invoke<DiscoveryReport>("jobs_fetch", { srcIds }),
  jobsAnalyzePlan: (ids: string[]) => invoke<AiPlan>("jobs_analyze_plan", { ids }),
  /** With `ai`, Rust names the count in its own dialog first. */
  jobsAnalyze: (ids: string[], ai: boolean) => invoke<MatchReport>("jobs_analyze", { ids, ai }),
  jobResearchPlan: (companyId: string) => invoke<AiPlan>("job_research_plan", { companyId }),
  jobResearch: (companyId: string) => invoke<CompanyResearch>("job_research", { companyId }),
  companiesList: () => invoke<Company[]>("companies_list"),
  companyNoteAdd: (companyId: string, text: string) =>
    invoke<CompanyNote>("company_note_add", { companyId, text }),
  matchPrefsGet: () => invoke<MatchPrefs>("match_prefs_get"),
  matchPrefsSet: (prefs: MatchPrefs) => invoke<void>("match_prefs_set", { prefs }),

  appList: (status: ApplicationStatus | null) =>
    invoke<Application[]>("app_list", { status }),
  appGet: (id: string) => invoke<Application>("app_get", { id }),
  appAdd: (input: ApplicationInput) => invoke<Application>("app_add", { input }),
  appUpdate: (id: string, input: ApplicationInput) =>
    invoke<Application>("app_update", { id, input }),
  /** `on` is YYYY-MM-DD and only matters for the first move to applied. */
  appSetStatus: (id: string, status: ApplicationStatus, on: string | null, note: string) =>
    invoke<Application>("app_set_status", { id, status, on, note }),
  appNote: (id: string, note: string) => invoke<void>("app_note", { id, note }),
  appHistory: (id: string) => invoke<ApplicationEvent[]>("app_history", { id }),

  agentsList: (limit: number) => invoke<RunSummary[]>("agents_list", { limit }),
  agentGet: (id: string) => invoke<RunDetail>("agent_get", { id }),
  /** An active run stops at its next poll; a halted run ends now. */
  agentCancel: (id: string) => invoke<void>("agent_cancel", { id }),
  agentResume: (id: string) => invoke<RunDetail>("agent_resume", { id }),

  pauseStatus: () => invoke<PauseState>("pause_status"),
  /** Stops everything automated now. Idempotent, and never clears. */
  pauseSet: () => invoke<PauseState>("pause_set"),
  /** Rust asks in a native dialog; only this button reaches it. */
  pauseClear: () => invoke<PauseState>("pause_clear"),
  /** Builds a new report from the application store. No model is involved. */
  analyticsRun: () => invoke<Report>("analytics_run"),
  analyticsLatest: () => invoke<Report | null>("analytics_latest"),
  appRemove: (id: string) => invoke<void>("app_remove", { id }),

  memoryList: (cls: MemoryClass | null) => invoke<QuestionRow[]>("memory_list", { class: cls }),
  memoryGet: (questionId: string) => invoke<QuestionRow>("memory_get", { questionId }),
  memoryAddQuestion: (text: string, country: string | null) =>
    invoke<Question>("memory_add_question", { text, country }),
  memoryAddAnswer: (questionId: string, text: string, sourceRef: string | null) =>
    invoke<Answer>("memory_add_answer", { questionId, text, sourceRef }),
  memoryEditAnswer: (answerId: string, text: string) =>
    invoke<Answer>("memory_edit_answer", { answerId, text }),
  /** Under a sealed class, Rust confirms an approval in a native dialog. */
  memoryApprove: (answerId: string, approved: boolean) =>
    invoke<void>("memory_approve", { answerId, approved }),
  /** Under a sealed class, Rust confirms in a native dialog. */
  memoryMarkReviewed: (answerId: string) => invoke<void>("memory_mark_reviewed", { answerId }),
  /** Rust confirms lowering in a native dialog. */
  memorySetClass: (questionId: string, cls: MemoryClass) =>
    invoke<Question>("memory_set_class", { questionId, class: cls }),
  /** Rust asks the user in a native dialog each time; script cannot answer it. */
  memoryReveal: (answerId: string) => invoke<string>("memory_reveal", { answerId }),
  /** Rust always asks the user in a native dialog; the dialog never holds the answer. */
  memoryConfirmUse: (answerId: string, applicationId: string) =>
    invoke<void>("memory_confirm_use", { answerId, applicationId }),
  memoryUse: (answerId: string, applicationId: string) =>
    invoke<UsedAnswer>("memory_use", { answerId, applicationId }),
  memoryApplicationAnswers: (applicationId: string) =>
    invoke<AppliedAnswer[]>("memory_application_answers", { applicationId }),
  memoryAuthorization: (country: string) =>
    invoke<AuthorizationAnswer>("memory_authorization", { country }),
  memoryRemoveQuestion: (id: string) => invoke<void>("memory_remove_question", { id }),
  memoryRemoveAnswer: (id: string) => invoke<void>("memory_remove_answer", { id }),
  memorySearch: (query: string, limit: number) =>
    invoke<MemoryHit[]>("memory_search", { query, limit }),
  /** What a press of Refresh would spend. Reads only. */
  memoryRefreshPlan: () => invoke<RefreshPlan>("memory_refresh_plan"),
  /** Rust names the count in its own dialog first, when any outside call is ahead. */
  memoryRefresh: () => invoke<MemoryRefreshed>("memory_refresh"),

  /** Rust opens the picker. Rejects with `cancelled` when the user closes it. */
  resumeImport: () => invoke<Imported>("resume_import"),
  resumeList: (kind: DraftKind | null) => invoke<Draft[]>("resume_list", { kind }),
  resumeVersions: (lineageId: string) => invoke<Draft[]>("resume_versions", { lineageId }),
  /** `manual_required` opens the editor; its message ends with the reason. */
  resumeGenerate: (jobTextId: string, exportAs?: ExportChoice) =>
    invoke<Generated>("resume_generate", { jobTextId, export: exportAs }),
  letterGenerate: (jobTextId: string, exportAs?: ExportChoice) =>
    invoke<Generated>("letter_generate", { jobTextId, export: exportAs }),
  resumeCompare: (generatedId: string, originalId: string | null) =>
    invoke<Compare>("resume_compare", { generatedId, originalId }),
  claimEdit: (claimId: string, input: ClaimInput) =>
    invoke<Draft>("claim_edit", { claimId, input }),
  claimRemove: (claimId: string) => invoke<Draft>("claim_remove", { claimId }),
  draftSave: (kind: DraftKind, lineageId: string | null, doc: ResumeDoc) =>
    invoke<Draft>("draft_save", { kind, lineageId, doc }),
  resumeExport: (draftId: string, template: Template, format: ExportFormat) =>
    invoke<string>("resume_export", { draftId, template, format }),

  aiStatus: () => invoke<AiStatus>("ai_status"),
  aiPull: (model: string) => invoke<void>("ai_pull", { model }),
  aiCancelPull: () => invoke<void>("ai_cancel_pull"),
  aiSetModels: (drafting: string | null, embedding: string | null) =>
    invoke<void>("ai_set_models", { drafting, embedding }),
  aiSetOrder: (order: Provider[]) => invoke<void>("ai_set_order", { order }),
  aiEnable: (provider: Provider, enabled: boolean) =>
    invoke<void>("ai_enable", { provider, enabled }),
  /** Rust opens the picker: no path crosses from here. */
  aiSetClaudePath: () => invoke<void>("ai_set_claude_path"),
  /** Back to finding `claude` by itself. Rust confirms in a native dialog. */
  aiClearClaudePath: () => invoke<void>("ai_clear_claude_path"),
  /** Rust opens the picker for the `gemini` or `codex` program: only the provider crosses. */
  aiSetCliPath: (provider: Provider) => invoke<void>("ai_set_cli_path", { provider }),
  /** Back to finding the program by itself. Rust confirms in a native dialog. */
  aiClearCliPath: (provider: Provider) => invoke<void>("ai_clear_cli_path", { provider }),
  /** Rust confirms in a native dialog. */
  aiSetOllamaUrl: (url: string) => invoke<void>("ai_set_ollama_url", { url }),
  /** Rust confirms in a native dialog. */
  aiConsentGive: (provider: Provider, categories: DataCategory[]) =>
    invoke<void>("ai_consent_give", { provider, categories }),
  aiConsentRevoke: (provider: Provider) => invoke<void>("ai_consent_revoke", { provider }),
  /** Rust reads the clipboard and confirms in a native dialog. The key never comes here. */
  aiKeyFromClipboard: (provider: Provider) => invoke<void>("ai_key_from_clipboard", { provider }),
  /** Rust confirms in a native dialog. */
  aiKeyRemove: (provider: Provider) => invoke<void>("ai_key_remove", { provider }),
  /** The consent record is the gate, so there is no dialog. Core holds the origin rules. */
  aiSetApiModel: (provider: Provider, model: string) =>
    invoke<void>("ai_set_api_model", { provider, model }),
  /** Rust refuses anything off this machine, then confirms in its own dialog. Empty clears. */
  aiSetEndpoint: (url: string) => invoke<void>("ai_set_endpoint", { url }),
  aiSharing: (provider: Provider) => invoke<Sharing>("ai_sharing", { provider }),
  /** Builds the prompt and sends nothing. */
  aiPreview: (jobTextId: string, provider: string) =>
    invoke<PromptPreview>("ai_preview", { jobTextId, provider }),
  aiIndex: () => invoke<number>("ai_index"),
  aiCancel: () => invoke<void>("ai_cancel"),

  keyState: () => invoke<KeyState>("key_state"),
  keyRetry: () => invoke<KeyState>("key_retry"),
  /** Rust opens the save dialog. The reply holds no key. */
  recoveryKeyExport: () => invoke<RecoveryReceipt>("recovery_key_export"),
  /** Rust opens the file dialog. */
  recoveryKeyImport: () => invoke<KeyState>("recovery_key_import"),
  /** Rust confirms in a native dialog naming the key id. */
  discardLockedFields: () => invoke<KeyState>("discard_locked_fields"),

  browserStatus: () => invoke<BrowserStatus>("browser_status"),
  /** Downloads the pinned server and Chromium. */
  browserSetup: () => invoke<BrowserStatus>("browser_setup"),
  applyPreflight: (applicationIds: string[]) =>
    invoke<Preflight>("apply_preflight", { applicationIds }),
  /** Rust asks in a native dialog whether sealed answers go to the run: nothing here can. */
  applyStart: (applicationIds: string[]) => invoke<string>("apply_start", { applicationIds }),
  applyView: (id: string) => invoke<ApplyRunView>("apply_view", { id }),
  applyLatest: () => invoke<ApplyRunView | null>("apply_latest"),
  /** Only these two answers exist, and only a person's press sends one. */
  applyResolvePause: (pauseId: string, resolution: "continue" | "take_over") =>
    invoke<void>("apply_resolve_pause", { pauseId, resolution }),
  applySettle: (id: string) => invoke<number>("apply_settle", { id }),
  applyCancel: (id: string) => invoke<void>("apply_cancel", { id }),
};

export function ptyOpen(cols: number, rows: number): Promise<void> {
  return invoke("pty_open", { cols, rows });
}

/** Hosts a `ready` apply run's session. Rust checks the id and builds the command. */
export function ptyOpenApply(cols: number, rows: number, applyId: string): Promise<void> {
  return invoke("pty_open_apply", { cols, rows, applyId });
}

/** `Array.from`: `invoke` JSON-encodes a `Uint8Array` as a map, which `Vec<u8>` refuses. */
export function ptyWrite(data: Uint8Array): Promise<void> {
  return invoke("pty_write", { data: Array.from(data) });
}

export function ptyResize(cols: number, rows: number): Promise<void> {
  return invoke("pty_resize", { cols, rows });
}

export function ptyClose(): Promise<void> {
  return invoke("pty_close");
}

export function onPtyData(cb: (bytes: Uint8Array) => void): Promise<() => void> {
  return getCurrentWebviewWindow().listen<number[]>("pty:data", (event) =>
    cb(Uint8Array.from(event.payload)),
  );
}

export function onPtyExit(cb: (code: number | null) => void): Promise<() => void> {
  return getCurrentWebviewWindow().listen<number | null>("pty:exit", (event) => cb(event.payload));
}

/** Frees output credit once xterm.js has parsed `bytes`, so the PTY paces the child. */
export function ptyAck(bytes: number): Promise<void> {
  return getCurrentWebviewWindow().emit("pty:ack", bytes);
}

/** The folder picker for a backup. Returns null when the user cancels. */
export async function pickFolder(): Promise<string | null> {
  const chosen = await open({ directory: true, multiple: false });
  return typeof chosen === "string" ? chosen : null;
}

/** Anything a command can reject with, as `{code, message}`. */
export function toAppError(error: unknown): AppError {
  if (typeof error === "object" && error !== null && "code" in error && "message" in error) {
    const { code, message } = error as AppError;
    return { code: String(code), message: String(message) };
  }
  return { code: "unknown", message: typeof error === "string" ? error : "Something went wrong." };
}
