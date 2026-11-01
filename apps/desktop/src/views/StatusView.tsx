import { useQuery } from "@tanstack/react-query";
import { CopyPath } from "@/components/CopyPath";
import { ErrorAlert } from "@/components/ErrorAlert";
import { PageHeading } from "@/components/PageHeading";
import { Button } from "@/components/ui/button";
import { ipc } from "@/lib/ipc";
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
    </div>
  );
}
