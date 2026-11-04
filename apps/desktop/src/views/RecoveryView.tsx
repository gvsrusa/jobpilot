import { useEffect, useRef, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { CopyPath } from "@/components/CopyPath";
import { ErrorAlert } from "@/components/ErrorAlert";
import { RestoreDialog, type RestoreTarget } from "@/components/RestoreDialog";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ipc, toAppError, type BackupInfo, type Status } from "@/lib/ipc";
import { useApplyStatus } from "@/lib/status";
import { baseName, formatDate } from "@/lib/utils";

/**
 * Offline restore is allowed only when the database itself is what failed. For the other
 * codes (and any code this build does not know) the backend refuses, so the button is off.
 */
const RESTORABLE = new Set(["db", "migration_failed", "schema_too_new", "restored_not_opened"]);

function Explanation({ code, root }: { code: string; root: string }) {
  switch (code) {
    case "schema_too_new":
      return (
        <p>
          This data was written by a newer JobPilot. Nothing was changed. Install a newer
          JobPilot to open it. Restoring an older backup replaces your current data.
        </p>
      );
    case "in_use":
      return (
        <p>
          Another JobPilot window or the <code>jobpilot</code> command is using this data. Close
          it, then try again. Nothing was changed.
        </p>
      );
    case "migration_failed":
      return (
        <p>
          Updating your data to this version failed. Your data from before the update is kept as
          a snapshot named <code>pre-migrate-{"{from}-{stamp}"}</code> in {root}/backups. Nothing
          was deleted.
        </p>
      );
    case "restored_not_opened":
      return (
        <p>
          The restore finished and your data now comes from the backup, but it could not be
          opened (see the message above). Your data from before the restore is kept as a snapshot
          named <code>pre-restore-{"{stamp}"}</code> in {root}/backups.
        </p>
      );
    default:
      return <p>Nothing was changed by this screen.</p>;
  }
}

/** The snapshot that explains this failure goes first; the rest newest first. */
function ordered(backups: BackupInfo[], code: string): BackupInfo[] {
  const prefix = code === "migration_failed" ? "pre-migrate" : code === "restored_not_opened" ? "pre-restore" : "";
  const rank = (b: BackupInfo) => (prefix && baseName(b.path).startsWith(prefix) ? 0 : 1);
  return [...backups].sort(
    (a, b) => rank(a) - rank(b) || b.created_at.localeCompare(a.created_at),
  );
}

function BackupPicker({
  code,
  onPick,
  onClose,
}: {
  code: string;
  onPick(target: RestoreTarget): void;
  onClose(): void;
}) {
  const backups = useQuery({ queryKey: ["backups"], queryFn: ipc.backupList });
  const list = ordered(backups.data ?? [], code);
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent showCloseButton={false}>
        <DialogHeader>
          <DialogTitle>Choose a backup to restore</DialogTitle>
          <DialogDescription>
            Snapshots JobPilot saved are listed here. You can also pick a backup folder from
            anywhere.
          </DialogDescription>
        </DialogHeader>
        <ErrorAlert error={backups.error} />
        {backups.isSuccess && list.length === 0 && (
          <p className="text-sm text-muted-foreground">No backups found in the data folder.</p>
        )}
        <ul className="grid gap-1">
          {list.map((b) => (
            <li key={b.path}>
              <Button
                variant="ghost"
                className="w-full justify-between"
                onClick={() => onPick(b)}
              >
                <span className="font-mono">{baseName(b.path)}</span>
                <span className="text-muted-foreground">{formatDate(b.created_at)}</span>
              </Button>
            </li>
          ))}
        </ul>
        <div className="flex justify-between">
          <Button variant="outline" onClick={() => onPick({})}>
            Choose another folder...
          </Button>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

export function RecoveryView({ status }: { status: Status }) {
  const applyStatus = useApplyStatus();
  const heading = useRef<HTMLHeadingElement>(null);
  const [picking, setPicking] = useState(false);
  const [target, setTarget] = useState<RestoreTarget | null>(null);
  const error = status.error ?? { code: "unavailable", message: "The database is not open." };
  // Nothing calls this on a timer: each failed migration leaves another snapshot.
  const retry = useMutation({ mutationFn: ipc.retryOpen, onSuccess: applyStatus });
  const quit = useMutation({ mutationFn: () => getCurrentWindow().close() });
  const canRestore = RESTORABLE.has(error.code);

  useEffect(() => heading.current?.focus(), []);

  return (
    <main className="mx-auto flex min-h-screen max-w-2xl flex-col gap-5 p-8">
      <h1 ref={heading} tabIndex={-1} className="text-xl font-semibold outline-none">
        JobPilot AI could not open your data
      </h1>
      <ErrorAlert error={retry.error ?? error} />
      <section aria-labelledby="root-heading" className="grid gap-2">
        <h2 id="root-heading" className="text-sm font-medium">
          Data folder{" "}
          <span className="font-normal text-muted-foreground">
            (nothing in it has been changed or deleted by this screen)
          </span>
        </h2>
        <div className="flex items-center justify-between gap-3">
          <code className="break-all select-text">{status.data_root}</code>
          <CopyPath path={status.data_root} />
        </div>
      </section>
      <section aria-labelledby="what-heading" className="grid gap-2 text-sm">
        <h2 id="what-heading" className="font-medium">
          What happened and what to do
        </h2>
        <Explanation code={error.code} root={status.data_root} />
        {!canRestore && (
          <p className="text-muted-foreground">
            Restoring a backup is off for this problem, because it would replace data that
            was never read.
          </p>
        )}
      </section>
      <div className="flex gap-3">
        <Button onClick={() => retry.mutate()} disabled={retry.isPending}>
          {retry.isPending ? "Trying again..." : "Try again"}
        </Button>
        <Button variant="outline" disabled={!canRestore} onClick={() => setPicking(true)}>
          Restore from a backup...
        </Button>
        <Button variant="outline" onClick={() => quit.mutate()}>
          Quit
        </Button>
      </div>
      {quit.error && <ErrorAlert error={toAppError(quit.error)} />}
      {picking && !target && (
        <BackupPicker
          code={error.code}
          onClose={() => setPicking(false)}
          onPick={(t) => setTarget(t)}
        />
      )}
      {target && (
        <RestoreDialog
          target={target}
          onClose={() => {
            setTarget(null);
            setPicking(false);
          }}
        />
      )}
    </main>
  );
}
