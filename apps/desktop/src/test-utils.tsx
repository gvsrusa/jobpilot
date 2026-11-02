import type { ReactElement } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import App from "./App";
import { useUiStore } from "./lib/store";
import type { BackupInfo, Document, PauseState, Provider, Sharing, Status } from "./lib/ipc";

export const READY: Status = {
  state: "ready",
  schema_version: 1,
  app_version: "0.1.0",
  data_root: "/home/me/jobpilot",
  error: null,
  key_state: { state: "key_not_loaded" },
  swap_interrupted: false,
  recovery_key_exported: true,
  restored_key_state: null,
};

export function recovery(code: string, message = `message for ${code}`): Status {
  return {
    ...READY,
    state: "recovery",
    schema_version: null,
    error: { code, message },
    key_state: null,
  };
}

/** Core's `ai::sharing` for a provider that can run every task. */
export const sharing = (over: Partial<Sharing> = {}): Sharing => ({
  name: "claude_code",
  label: "Claude Code",
  key: "claude_code/cli_login",
  destination: "Anthropic, through the claude program on this computer",
  path: null,
  tasks: [
    { task: "parse_job", categories: ["job_text"], runs: true, skipped_because: null },
    { task: "draft_resume", categories: ["job_text", "profile_basic", "resume_text"], runs: true, skipped_because: null },
    { task: "draft_cover_letter", categories: ["job_text", "profile_basic", "resume_text"], runs: true, skipped_because: null },
    { task: "classify_answer", categories: ["job_text"], runs: true, skipped_because: null },
  ],
  data_use: "Consumer Terms allow training unless the user opts out.",
  terms_date: "2026-11-01",
  attestation: null,
  suspended: false,
  given: null,
  ...over,
});

type Handler = (args: Record<string, unknown>) => unknown;

export type Backend = { calls: { cmd: string; args: Record<string, unknown> }[] };

/** An in-memory stand-in for the Rust commands. `overrides` replace single commands. */
const isStatus = (value: unknown): value is Status =>
  typeof value === "object" && value !== null && "data_root" in value && "state" in value;

export function backend(overrides: Record<string, Handler> = {}): Backend {
  const calls: Backend["calls"] = [];
  const docs: Document[] = [];
  const secrets: string[] = [];
  const backups: BackupInfo[] = [];
  let pause: PauseState = { paused: false, set_at: null, set_by: null };
  const defaults: Record<string, Handler> = {
    status: () => READY,
    retry_open: () => READY,
    documents_list: () => docs,
    documents_create: ({ title, body }) => {
      const doc = {
        id: `doc-${docs.length + 1}`,
        title: String(title),
        body: String(body),
        created_at: "2026-11-01T10:00:00Z",
        updated_at: "2026-11-01T10:00:00Z",
      };
      docs.push(doc);
      return doc;
    },
    documents_update: ({ id, title, body }) => {
      const doc = docs.find((d) => d.id === id)!;
      Object.assign(doc, { title, body });
      return doc;
    },
    documents_delete: ({ id }) => {
      docs.splice(docs.findIndex((d) => d.id === id), 1);
    },
    documents_search: ({ query }) =>
      docs
        .filter((d) => d.body.includes(String(query)))
        .map((d) => ({ id: d.id, title: d.title, snippet: d.body })),
    backup_list: () => backups,
    backup_create: () => {
      const info = {
        path: "/home/me/jobpilot/backups/manual-20261101",
        created_at: "2026-11-01T10:00:00Z",
        schema_version: 1,
      };
      backups.push(info);
      return info;
    },
    backup_restore: () => READY,
    data_export: () => ({ path: "/home/me/jobpilot/exports/export-1", withheld: 0 }),
    data_delete: () => READY,
    secret_names: () => secrets,
    secret_set: ({ name }) => {
      secrets.push(String(name));
    },
    secret_delete: ({ name }) => {
      secrets.splice(secrets.indexOf(String(name)), 1);
    },
    pause_status: () => pause,
    pause_set: () => {
      pause = { paused: true, set_at: "2026-11-01T10:00:00.000Z", set_by: "gui" };
      return pause;
    },
    pause_clear: () => {
      pause = { paused: false, set_at: null, set_by: null };
      return pause;
    },
    audit_recent: () => [],
    ai_sharing: ({ provider }) => sharing({ name: provider as Provider }),
    app_list: () => [],
    app_history: () => [],
    analytics_latest: () => null,
    job_list: () => [],
    kb_list: () => [],
    memory_list: () => [],
    memory_application_answers: () => [],
    memory_search: () => [],
    memory_refresh_plan: () => ({ pending: 0, external_calls: 0 }),
    memory_refresh: () => ({
      sealed: 0,
      seal_blocked: null,
      classified: { checked: 0, raised: 0, skipped: true },
      embedded: 0,
    }),
    resume_list: () => [],
    key_state: () => ({ state: "key_not_loaded" }),
    settings_get: () => null,
    settings_set: () => undefined,
    pty_open: () => undefined,
    pty_write: () => undefined,
    pty_resize: () => undefined,
    pty_close: () => undefined,
  };
  let current: Status | undefined;
  mockWindows("main");
  mockIPC((cmd, args) => {
    const payload = (args ?? {}) as Record<string, unknown>;
    calls.push({ cmd, args: payload });
    // Like the real backend, `status` reports whatever the last Status-returning
    // command left behind.
    if (cmd === "status" && current) return current;
    const handler = overrides[cmd] ?? defaults[cmd];
    if (!handler) throw new Error(`unmocked command ${cmd}`);
    const result = handler(payload);
    if (isStatus(result)) current = result;
    return result;
  }, { shouldMockEvents: true });
  return { calls };
}

export function renderApp(ui: ReactElement = <App />) {
  useUiStore.setState({ view: "status" });
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return {
    client,
    user: userEvent.setup(),
    ...render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>),
  };
}

export const count = (b: Backend, cmd: string) => b.calls.filter((c) => c.cmd === cmd).length;
