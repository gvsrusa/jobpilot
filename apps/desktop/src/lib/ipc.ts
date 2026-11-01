// ponytail: hand-kept in sync with commands.rs; add tauri-specta when the command count makes drift a real problem.
import { invoke } from "@tauri-apps/api/core";
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
};

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
