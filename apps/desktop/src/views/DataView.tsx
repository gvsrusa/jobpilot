import { useState, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { ErrorAlert } from "@/components/ErrorAlert";
import { PageHeading } from "@/components/PageHeading";
import { RestoreDialog, type RestoreTarget } from "@/components/RestoreDialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ipc, pickFolder, type KeyState } from "@/lib/ipc";
import { isLocked, offersImportAndDiscard } from "@/lib/keys";
import { useApplyStatus, useStatus } from "@/lib/status";
import { baseName, formatDate } from "@/lib/utils";

function Section(props: { id: string; title: string; action?: ReactNode; children: ReactNode }) {
  return (
    <section aria-labelledby={props.id} className="grid gap-3 rounded-lg border p-4">
      <div className="flex items-center justify-between">
        <h2 id={props.id} className="font-medium">
          {props.title}
        </h2>
        {props.action}
      </div>
      {props.children}
    </section>
  );
}

function BackupsSection({ onRestore }: { onRestore(t: RestoreTarget): void }) {
  const client = useQueryClient();
  const backups = useQuery({ queryKey: ["backups"], queryFn: ipc.backupList });
  const create = useMutation({
    mutationFn: ipc.backupCreate,
    onSuccess: () => client.invalidateQueries(),
  });
  const folder = useMutation({
    mutationFn: pickFolder,
    onSuccess: (path) => path && onRestore({ path }),
  });
  return (
    <Section
      id="backups"
      title="Backups"
      action={
        <Button onClick={() => create.mutate()} disabled={create.isPending}>
          {create.isPending ? "Creating..." : "Create backup"}
        </Button>
      }
    >
      <ErrorAlert error={backups.error ?? create.error ?? folder.error} />
      {backups.isSuccess && backups.data.length === 0 && (
        <p className="text-sm text-muted-foreground">
          No backups yet. Create one before you change anything important.
        </p>
      )}
      <ul className="grid gap-1 text-sm">
        {backups.data?.map((b) => (
          <li key={b.path} className="flex items-center justify-between gap-3">
            <span>
              <span className="font-mono">{baseName(b.path)}</span>{" "}
              <span className="text-muted-foreground">
                {formatDate(b.created_at)}, schema {b.schema_version ?? "unknown"}
              </span>
            </span>
            <Button
              variant="outline"
              size="sm"
              aria-label={`Restore ${baseName(b.path)}`}
              onClick={() => onRestore(b)}
            >
              Restore...
            </Button>
          </li>
        ))}
      </ul>
      <div>
        <Button variant="outline" size="sm" onClick={() => folder.mutate()}>
          Restore from another folder...
        </Button>
      </div>
    </Section>
  );
}

function ExportSection({ root }: { root: string }) {
  const [asking, setAsking] = useState(false);
  const exported = useMutation({
    mutationFn: ipc.dataExport,
    onSettled: () => setAsking(false),
  });
  return (
    <Section id="export" title="Export">
      <p className="text-sm">
        Writes all your data as plain text to {root}/exports. The file is not encrypted and
        includes everything in your documents.
      </p>
      <ErrorAlert error={exported.error} />
      {exported.data && (
        <p className="text-sm">
          Exported to <code className="break-all select-text">{exported.data}</code>
        </p>
      )}
      <div>
        <Button variant="outline" onClick={() => setAsking(true)} disabled={exported.isPending}>
          {exported.isPending ? "Exporting..." : "Export all data..."}
        </Button>
      </div>
      {asking && (
        <ConfirmDialog
          title="Write your data as plain text?"
          confirmLabel="Write plain text export"
          busyLabel="Exporting..."
          confirmVariant="default"
          pending={exported.isPending}
          onCancel={() => setAsking(false)}
          onConfirm={() => exported.mutate()}
        >
          <p>
            JobPilot keeps some fields encrypted: work authorization, notes, knowledge you marked
            sensitive, and sealed career answers. The export writes them as plain text. Anyone who can read the
            exports folder can read them, and a cloud-synced or shared folder would copy them.
          </p>
          <p>Delete the file when you no longer need it.</p>
        </ConfirmDialog>
      )}
    </Section>
  );
}

function SecretsSection() {
  const client = useQueryClient();
  const names = useQuery({ queryKey: ["secrets"], queryFn: ipc.secretNames });
  const [name, setName] = useState("");
  const [value, setValue] = useState("");
  const [removing, setRemoving] = useState<string | null>(null);
  const changed = () => client.invalidateQueries();
  const save = useMutation({
    mutationFn: () => ipc.secretSet(name, value),
    onSuccess: () => {
      // Write-only: the value leaves the page as soon as the command is done.
      setName("");
      setValue("");
      return changed();
    },
  });
  const remove = useMutation({
    mutationFn: (secret: string) => ipc.secretDelete(secret),
    onSuccess: () => {
      setRemoving(null);
      return changed();
    },
  });
  return (
    <Section id="secrets" title="Secrets (values are never shown)">
      <form
        className="flex items-end gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          save.mutate();
        }}
      >
        <div className="grid gap-1.5">
          <Label htmlFor="secret-name">Secret name</Label>
          <Input id="secret-name" value={name} onChange={(e) => setName(e.target.value)} />
        </div>
        <div className="grid gap-1.5">
          <Label htmlFor="secret-value">Secret value</Label>
          <Input
            id="secret-value"
            type="password"
            autoComplete="off"
            value={value}
            onChange={(e) => setValue(e.target.value)}
          />
        </div>
        <Button type="submit" disabled={save.isPending || name.trim() === "" || value === ""}>
          {save.isPending ? "Saving..." : "Save secret"}
        </Button>
      </form>
      {save.isPending && (
        <p role="status" className="text-sm text-muted-foreground">
          Waiting for the system keychain. It may ask for permission.
        </p>
      )}
      <ErrorAlert error={names.error ?? save.error} />
      {names.isSuccess && names.data.length === 0 && (
        <p className="text-sm text-muted-foreground">No secrets stored.</p>
      )}
      <ul className="grid gap-1 text-sm">
        {names.data?.map((n) => (
          <li key={n} className="flex items-center justify-between">
            <span>{n}</span>
            <Button
              variant="outline"
              size="sm"
              aria-label={`Remove ${n}`}
              onClick={() => setRemoving(n)}
            >
              Remove...
            </Button>
          </li>
        ))}
      </ul>
      {removing && (
        <ConfirmDialog
          title={`Remove the secret "${removing}"?`}
          confirmLabel="Remove secret"
          busyLabel="Removing..."
          pending={remove.isPending}
          error={remove.error}
          onCancel={() => setRemoving(null)}
          onConfirm={() => remove.mutate(removing)}
        >
          <p>The value is removed from your system keychain. JobPilot cannot show it first.</p>
        </ConfirmDialog>
      )}
    </Section>
  );
}

function ActivitySection() {
  const audit = useQuery({ queryKey: ["audit"], queryFn: () => ipc.auditRecent(20) });
  return (
    <Section id="activity" title="Recent activity">
      <ErrorAlert error={audit.error} />
      {audit.isSuccess && audit.data.length === 0 && (
        <p className="text-sm text-muted-foreground">No activity yet.</p>
      )}
      <ul className="grid gap-1 text-sm">
        {audit.data?.map((a) => (
          <li key={a.id} className="flex gap-3">
            <span className="text-muted-foreground">{formatDate(a.at)}</span>
            <span>{a.actor}</span>
            <span className="font-mono">{a.action}</span>
            <span className="break-all">{a.target}</span>
          </li>
        ))}
      </ul>
    </Section>
  );
}

function DeleteSection({ root }: { root: string }) {
  const applyStatus = useApplyStatus();
  const [open, setOpen] = useState(false);
  const [typed, setTyped] = useState("");
  const [withBackups, setWithBackups] = useState(false);
  const [done, setDone] = useState(false);
  const close = () => {
    setOpen(false);
    setTyped("");
    setWithBackups(false);
    del.reset();
  };
  const del = useMutation({
    mutationFn: () => ipc.dataDelete(typed, withBackups),
    onSuccess: (status) => {
      close();
      setDone(true);
      applyStatus(status);
    },
  });
  return (
    <Section id="delete" title="Delete all data">
      {done && <p role="status" className="text-sm">All JobPilot data was deleted.</p>}
      <div>
        <Button
          variant="destructive"
          onClick={() => {
            setDone(false);
            setOpen(true);
          }}
        >
          Delete all data...
        </Button>
      </div>
      {open && (
        <ConfirmDialog
          title="Delete all JobPilot data?"
          confirmLabel="Delete everything"
          busyLabel="Deleting..."
          confirmDisabled={typed !== "DELETE"}
          pending={del.isPending}
          error={del.error}
          onCancel={close}
          onConfirm={() => del.mutate()}
        >
          <p>
            This permanently removes, from {root}: the database, all documents, exports, the
            activity log, and the secrets JobPilot stored in your system keychain.
          </p>
          <div className="flex items-center gap-2">
            <input
              id="delete-backups"
              type="checkbox"
              checked={withBackups}
              onChange={(e) => setWithBackups(e.target.checked)}
            />
            <Label htmlFor="delete-backups">Also delete backups and snapshots</Label>
          </div>
          <p>
            Keychain secrets are removed for this data folder. A copied folder or a restored backup
            that shares this folder's key id loses its key too, and its locked fields can then
            be read only with a recovery file. This cannot reach a recovery file, backup, or
            export you saved elsewhere. Deleting the activity log is part of this action, so no
            record of it remains.
          </p>
          <div className="grid gap-1.5">
            <Label htmlFor="delete-confirm">Type DELETE to confirm</Label>
            <Input
              id="delete-confirm"
              autoComplete="off"
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
            />
          </div>
        </ConfirmDialog>
      )}
    </Section>
  );
}

const KEY_TEXT: Record<"key_unavailable" | "key_missing" | "key_mismatch", string> = {
  key_unavailable:
    "Your keychain did not answer. Unlock it, then retry. No import and no discard are offered.",
  key_missing: "The key for this data is not on this machine.",
  key_mismatch: "The key found does not match this data.",
};

/** A key change moves what every sensitive field shows, so cached rows are dropped. */
function useKeyChange() {
  const client = useQueryClient();
  return (state: KeyState) => {
    client.setQueryData(["keystate"], state);
    void client.invalidateQueries({ queryKey: ["status"] });
    void client.resetQueries({ predicate: (q) => q.queryKey[0] !== "status" && q.queryKey[0] !== "keystate" });
  };
}

function LockedFields() {
  const key = useQuery({ queryKey: ["keystate"], queryFn: ipc.keyState });
  const changed = useKeyChange();
  const run = (fn: () => Promise<KeyState>) => ({ mutationFn: fn, onSuccess: changed });
  const retry = useMutation(run(ipc.keyRetry));
  const importKey = useMutation(run(ipc.recoveryKeyImport));
  const discard = useMutation(run(ipc.discardLockedFields));
  const state = key.data;
  const busy = retry.isPending || importKey.isPending || discard.isPending;
  const cancelled = (e: unknown) => (e && (e as { code?: string }).code === "cancelled" ? null : e);
  if (key.error) return <ErrorAlert error={key.error} />;
  if (!isLocked(state)) return null;
  return (
    <Section id="locked" title="Locked fields">
      <p className="text-sm text-muted-foreground">key id: {state.key_id}</p>
      <p className="text-sm">{KEY_TEXT[state.state]}</p>
      {state.state === "key_unavailable" && <p className="text-sm text-muted-foreground">{state.reason}</p>}
      <ErrorAlert error={cancelled(retry.error) ?? cancelled(importKey.error) ?? cancelled(discard.error)} />
      <div className="flex flex-wrap gap-2">
        <Button variant="outline" disabled={busy} onClick={() => retry.mutate()}>
          Retry
        </Button>
        {offersImportAndDiscard(state) && (
          <>
            <Button variant="outline" disabled={busy} onClick={() => importKey.mutate()}>
              Import recovery key file...
            </Button>
            <Button variant="destructive" disabled={busy} onClick={() => discard.mutate()}>
              Discard locked fields...
            </Button>
          </>
        )}
      </div>
    </Section>
  );
}

function KeyBanners() {
  const client = useQueryClient();
  const status = useStatus();
  const s = status.data;
  // `recovery_key_exported` is false with no key too, and then there is nothing to export.
  const key = useQuery({ queryKey: ["keystate"], queryFn: ipc.keyState });
  const exportKey = useMutation({
    mutationFn: ipc.recoveryKeyExport,
    onSuccess: () => client.invalidateQueries({ queryKey: ["status"] }),
  });
  const dismiss = useMutation({
    mutationFn: ipc.swapWarningClear,
    onSuccess: () => client.invalidateQueries({ queryKey: ["status"] }),
  });
  if (!s) return null;
  const restored = s.restored_key_state;
  const cancelled = exportKey.error && (exportKey.error as { code?: string }).code === "cancelled";
  return (
    <>
      {s.swap_interrupted && (
        <Alert>
          <AlertDescription className="flex items-center justify-between gap-3">
            <span>The last restore was interrupted. Check your data, then dismiss.</span>
            <Button size="sm" variant="outline" onClick={() => dismiss.mutate()}>
              Dismiss
            </Button>
          </AlertDescription>
        </Alert>
      )}
      {restored && isLocked(restored) && (
        <Alert variant="destructive">
          <AlertDescription>
            {restored.state === "key_missing" &&
              `The restored backup's key (${restored.key_id}) is not on this computer, so its locked fields stay locked. Import its recovery key file below.`}
            {restored.state === "key_mismatch" &&
              `The restored backup's key (${restored.key_id}) does not match its data, so its locked fields stay locked.`}
            {restored.state === "key_unavailable" &&
              "The keychain did not answer after the restore, so locked fields stay locked until you retry."}
          </AlertDescription>
        </Alert>
      )}
      {s.recovery_key_exported === false && key.data?.state === "unlocked" && (
        <Alert>
          <AlertTitle>No recovery key exported</AlertTitle>
          <AlertDescription className="grid gap-2">
            <span>
              Without a recovery file, a lost keychain means your locked fields cannot be read
              again. Save the file outside this data folder.
            </span>
            {exportKey.data && <span>Saved to {exportKey.data.path}</span>}
            {!cancelled && <ErrorAlert error={exportKey.error} />}
            <div>
              <Button size="sm" variant="outline" disabled={exportKey.isPending} onClick={() => exportKey.mutate()}>
                Export recovery key...
              </Button>
            </div>
          </AlertDescription>
        </Alert>
      )}
    </>
  );
}

export function DataView() {
  const status = useStatus();
  const [target, setTarget] = useState<RestoreTarget | null>(null);
  const root = status.data?.data_root ?? "your data folder";
  return (
    <div className="grid max-w-3xl gap-4">
      <PageHeading>Data and privacy</PageHeading>
      <KeyBanners />
      <LockedFields />
      <Section id="before" title="Before you store anything">
        <ul className="list-disc pl-5 text-sm">
          <li>Turn on disk encryption (FileVault, BitLocker).</li>
          <li>Keep sensitive details out of free-form documents.</li>
          <li>Exports are plain text.</li>
          <li>Do not put the data folder in a cloud-synced folder.</li>
        </ul>
      </Section>
      <BackupsSection onRestore={setTarget} />
      <ExportSection root={root} />
      <SecretsSection />
      <ActivitySection />
      <DeleteSection root={root} />
      {target && <RestoreDialog target={target} onClose={() => setTarget(null)} />}
    </div>
  );
}
