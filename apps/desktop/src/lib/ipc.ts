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
export type Stated = "not_stated" | "yes" | "no";

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
  /** Empty while `locked`. Render it as text only. */
  body: string;
  source: string;
  source_ref: string | null;
  approved: boolean;
  marked_sensitive: boolean;
  locked: boolean;
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
  provider: "ollama" | "claude_code" | "no_ai";
};

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

export type Provider = "ollama" | "claude_code";

export type AiStatus = {
  ollama: OllamaStatus;
  claude_code: { available: boolean; path: string | null; consent: DataCategory[] | null };
  hardware: { total_memory: number; cpus: number; gpu: string; free_disk: number };
  order: string[];
  enabled: { ollama: boolean; claude_code: boolean };
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

export type AuditEntry = {
  id: number;
  at: string;
  actor: string;
  action: string;
  target: string | null;
  detail: unknown;
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
  dataExport: () => invoke<string>("data_export"),
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
  kbSearch: (query: string, limit: number) => invoke<Hit[]>("kb_search", { query, limit }),

  jobPaste: (title: string, raw: string) => invoke<JobText>("job_paste", { title, raw }),
  jobList: () => invoke<JobText[]>("job_list"),
  jobGet: (id: string) => invoke<JobText>("job_get", { id }),

  /** Rust opens the picker. Rejects with `cancelled` when the user closes it. */
  resumeImport: () => invoke<Imported>("resume_import"),
  resumeList: (kind: DraftKind | null) => invoke<Draft[]>("resume_list", { kind }),
  resumeVersions: (lineageId: string) => invoke<Draft[]>("resume_versions", { lineageId }),
  /** `manual_required` opens the editor; its message ends with the reason. */
  resumeGenerate: (jobTextId: string) => invoke<Generated>("resume_generate", { jobTextId }),
  letterGenerate: (jobTextId: string) => invoke<Generated>("letter_generate", { jobTextId }),
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
  /** Rust confirms in a native dialog. */
  aiSetOllamaUrl: (url: string) => invoke<void>("ai_set_ollama_url", { url }),
  /** Rust confirms in a native dialog. */
  aiConsentGive: (provider: "claude_code", categories: DataCategory[]) =>
    invoke<void>("ai_consent_give", { provider, categories }),
  aiConsentRevoke: (provider: "claude_code") => invoke<void>("ai_consent_revoke", { provider }),
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
};

export function ptyOpen(cols: number, rows: number): Promise<void> {
  return invoke("pty_open", { cols, rows });
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
