import type { ReactElement } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { mockIPC } from "@tauri-apps/api/mocks";
import App from "./App";
import { useUiStore } from "./lib/store";
import type { BackupInfo, Document, Status } from "./lib/ipc";

export const READY: Status = {
  state: "ready",
  schema_version: 1,
  app_version: "0.1.0",
  data_root: "/home/me/jobpilot",
  error: null,
};

export function recovery(code: string, message = `message for ${code}`): Status {
  return { ...READY, state: "recovery", schema_version: null, error: { code, message } };
}

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
    data_export: () => "/home/me/jobpilot/exports/export-1",
    data_delete: () => READY,
    secret_names: () => secrets,
    secret_set: ({ name }) => {
      secrets.push(String(name));
    },
    secret_delete: ({ name }) => {
      secrets.splice(secrets.indexOf(String(name)), 1);
    },
    audit_recent: () => [],
    settings_get: () => null,
    settings_set: () => undefined,
  };
  let current: Status | undefined;
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
  });
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
