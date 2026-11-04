import { useMutation } from "@tanstack/react-query";
import { ipc, toAppError } from "@/lib/ipc";
import { useApplyStatus } from "@/lib/status";
import { baseName, formatDate } from "@/lib/utils";
import { ConfirmDialog } from "./ConfirmDialog";

/** A snapshot `backup_list` gave, or `{}`: a folder the user picks next in a system dialog. */
export type RestoreTarget = { path?: string; created_at?: string; schema_version?: number | null };

/**
 * The restore warning, shared by the Data and Recovery screens. The command answers with
 * a `Status`; if that is "recovery" the app routes to the recovery view, which is the
 * real result of the restore.
 */
export function RestoreDialog({ target, onClose }: { target: RestoreTarget; onClose(): void }) {
  const applyStatus = useApplyStatus();
  const restore = useMutation({
    mutationFn: () => ipc.backupRestore(target.path ? baseName(target.path) : null),
    onSuccess: (status) => {
      applyStatus(status);
      onClose();
    },
    // The user closed the folder picker: nothing changed, and there is nothing to report.
    onError: (error) => toAppError(error).code === "cancelled" && onClose(),
  });
  const schema = target.schema_version ?? "unknown";
  const path = target.path;
  return (
    <ConfirmDialog
      title="Replace your current data with this backup?"
      confirmLabel="Replace my data"
      busyLabel="Replacing your data..."
      pending={restore.isPending}
      error={restore.error}
      onCancel={onClose}
      onConfirm={() => restore.mutate()}
    >
      {path ? (
        <>
          <p>
            Backup: <span className="font-mono">{baseName(path)}</span>, schema {schema}
            {target.created_at && <>, made {formatDate(target.created_at)}</>}
          </p>
          <p className="break-all font-mono text-xs">{path}</p>
        </>
      ) : (
        <p>You choose the folder next, in a system dialog.</p>
      )}
      <p>
        This replaces everything now in JobPilot: documents, settings, and the activity log.
        Secret names stay listed, because the secrets stay in your keychain. A snapshot of your
        current data is saved first as pre-restore-{"{stamp}"}, so you can undo this by restoring
        that snapshot. A backup from an older version is updated to this version.
      </p>
      <p>
        Any consent you gave to any AI provider is revoked by a restore, so nothing leaves this
        computer until you give it again on the AI screen.
      </p>
    </ConfirmDialog>
  );
}
