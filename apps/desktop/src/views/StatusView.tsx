import { useEffect } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CopyPath } from "@/components/CopyPath";
import { ErrorAlert } from "@/components/ErrorAlert";
import { PageHeading } from "@/components/PageHeading";
import { Button } from "@/components/ui/button";
import { ipc, onUpdateAvailable, toAppError, type UpdateInfo } from "@/lib/ipc";
import { describeKey, isLocked } from "@/lib/keys";
import { useUiStore } from "@/lib/store";
import { useStatus } from "@/lib/status";
import { formatDate } from "@/lib/utils";

export function StatusView() {
  const status = useStatus();
  const backups = useQuery({ queryKey: ["backups"], queryFn: ipc.backupList });
  const setView = useUiStore((u) => u.setView);
  const s = status.data;
  const newest = backups.data?.map((b) => b.created_at).sort().pop();
  const count = backups.data?.length;
  return (
    <div className="grid gap-4">
      <PageHeading>Status</PageHeading>
      <ErrorAlert error={status.error ?? backups.error} />
      {s && (
        <dl className="grid max-w-xl grid-cols-[10rem_1fr] items-center gap-x-4 gap-y-3 text-sm">
          <dt className="text-muted-foreground">Database</dt>
          <dd>Ready</dd>
          <dt className="text-muted-foreground">Data schema</dt>
          <dd>{s.schema_version ?? "unknown"}</dd>
          <dt className="text-muted-foreground">App version</dt>
          <dd>{s.app_version}</dd>
          <dt className="text-muted-foreground">Data folder</dt>
          <dd className="flex items-center justify-between gap-3">
            <code className="break-all select-text">{s.data_root}</code>
            <CopyPath path={s.data_root} />
          </dd>
          <dt className="text-muted-foreground">Encryption key</dt>
          <dd className="flex items-center justify-between gap-3">
            <span>{describeKey(s.key_state)}</span>
            {isLocked(s.key_state) && (
              <Button size="sm" variant="outline" onClick={() => setView("data")}>
                Open Data
              </Button>
            )}
          </dd>
          <dt className="text-muted-foreground">Backups</dt>
          <dd>
            {count === undefined
              ? "Checking..."
              : `${count} on disk${newest ? `, newest ${formatDate(newest)}` : ""}`}
          </dd>
        </dl>
      )}
      <p className="text-sm text-muted-foreground">
        Same data is used by the <code>jobpilot</code> command line tool.
      </p>
      <Updates version={s?.app_version} />
    </div>
  );
}

const BLOCKED = "Not now: an apply run is active or the emergency pause is on. Update when it has ended.";

/** A person's check and install. The only unprompted check is the opt-in one at launch. */
function Updates({ version }: { version?: string }) {
  const client = useQueryClient();
  const pending = useQuery({ queryKey: ["update-pending"], queryFn: ipc.updatePending });
  const auto = useQuery({ queryKey: ["update-auto"], queryFn: ipc.updateAutoGet });
  const check = useMutation({
    mutationFn: ipc.updateCheck,
    onSuccess: (found) => client.setQueryData(["update-pending"], found),
  });
  const install = useMutation({ mutationFn: ipc.updateInstall });
  const setAuto = useMutation({
    mutationFn: ipc.updateAutoSet,
    onSuccess: (_, on) => client.setQueryData(["update-auto"], on),
  });
  useEffect(() => {
    const stop = onUpdateAvailable((info) => client.setQueryData<UpdateInfo | null>(["update-pending"], info));
    return () => void stop.then((unlisten) => unlisten());
  }, [client]);

  const found = check.isError ? null : pending.data;
  const blocked = install.isError && toAppError(install.error).code === "update_blocked_by_run";
  return (
    <section aria-labelledby="updates-heading" className="grid max-w-xl gap-3 text-sm">
      <h2 id="updates-heading" className="font-medium">
        Updates
      </h2>
      <p>Current version {version ?? "unknown"}</p>
      <div>
        <Button size="sm" variant="outline" disabled={check.isPending} onClick={() => check.mutate()}>
          {check.isPending ? "Checking..." : "Check for updates"}
        </Button>
      </div>
      {check.isError && (
        <>
          <p>Couldn't check for updates</p>
          <ErrorAlert error={check.error} />
        </>
      )}
      {check.isSuccess && !check.data && <p>JobPilot is up to date.</p>}
      {found && (
        <div className="grid gap-2">
          <p>{`Version ${found.version} is available`}</p>
          {found.notes && <p className="whitespace-pre-wrap text-muted-foreground">{found.notes}</p>}
          <div>
            <Button size="sm" disabled={install.isPending} onClick={() => install.mutate()}>
              {install.isPending ? "Installing..." : "Install and restart"}
            </Button>
          </div>
          {blocked ? <p>{BLOCKED}</p> : <ErrorAlert error={install.error} />}
        </div>
      )}
      <label className="flex items-center gap-2">
        <input
          type="checkbox"
          role="switch"
          checked={auto.data ?? false}
          disabled={auto.data === undefined || setAuto.isPending}
          onChange={(e) => setAuto.mutate(e.target.checked)}
        />
        Check for updates when JobPilot starts
      </label>
      <p className="text-muted-foreground">
        Off by default. When on, JobPilot asks GitHub for a newer version each time it starts. It never installs one
        on its own.
      </p>
      <ErrorAlert error={auto.error ?? setAuto.error} />
    </section>
  );
}
