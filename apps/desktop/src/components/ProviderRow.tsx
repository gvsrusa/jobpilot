import { useMutation, useQueryClient } from "@tanstack/react-query";
import { labelOf } from "@/components/ConsentDialog";
import { ErrorAlert } from "@/components/ErrorAlert";
import { Button } from "@/components/ui/button";
import { ipc, type ExternalStatus } from "@/lib/ipc";
import { SUSPENDED } from "@/lib/reasons";

/** A provider's reason code, in plain words. */
const UNAVAILABLE: Record<string, string> = {
  not_built: "not built yet",
};

/** Stored or not, and the two actions. Rust does both: no key passes through this screen. */
function KeyControls({ row }: { row: ExternalStatus }) {
  const client = useQueryClient();
  const changed = () => client.resetQueries({ queryKey: ["ai"] });
  // A failure can come after the key is stored. Invalidating keeps this row, and its alert, up.
  const paste = useMutation({
    mutationFn: () => ipc.aiKeyFromClipboard(row.name),
    onSettled: () => client.invalidateQueries({ queryKey: ["ai"] }),
  });
  const remove = useMutation({ mutationFn: () => ipc.aiKeyRemove(row.name), onSuccess: changed });
  return (
    <div className="grid gap-2">
      <div className="flex flex-wrap items-center gap-3">
        <span>{row.key_stored ? "Key stored" : "No key"}</span>
        <Button
          size="sm"
          variant="outline"
          aria-label={`${row.label}: Paste key from clipboard...`}
          disabled={paste.isPending}
          onClick={() => paste.mutate()}
        >
          Paste key from clipboard...
        </Button>
        {row.key_stored && (
          <Button
            size="sm"
            variant="outline"
            aria-label={`${row.label}: Remove key`}
            disabled={remove.isPending}
            onClick={() => remove.mutate()}
          >
            Remove key
          </Button>
        )}
      </div>
      <p className="text-muted-foreground">Copy your key first. The key stays out of this window.</p>
      <ErrorAlert error={paste.error ?? remove.error} />
    </div>
  );
}

/** One external provider. Using it needs a record and a provider that can run. */
export function ProviderRow(props: {
  row: ExternalStatus;
  enabled: boolean;
  onUse(on: boolean): void;
  onReview(): void;
  onRevoke(): void;
}) {
  const { row, enabled } = props;
  const reason = row.reason ?? "";
  return (
    <li aria-label={row.label} className="grid gap-2 border-t pt-2 text-sm first:border-t-0 first:pt-0">
      <div className="flex flex-wrap items-center gap-3">
        <span className="font-medium">{row.label}</span>
        {!row.available && <span>Unavailable: {UNAVAILABLE[reason] ?? reason}</span>}
        {row.path && <span className="break-all">{row.path}</span>}
      </div>
      {row.suspended && <p role="alert">{SUSPENDED}</p>}
      {row.kind !== "cli" && <KeyControls row={row} />}
      {row.consent && <p>Consent given for: {row.consent.map(labelOf).join(", ")}.</p>}
      <div className="flex flex-wrap items-center gap-3">
        {row.consent && (
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={enabled}
              disabled={!row.available}
              onChange={(e) => props.onUse(e.target.checked)}
            />
            {`Use ${row.label}`}
          </label>
        )}
        <Button size="sm" variant="outline" aria-label={`Review ${row.label}...`} onClick={props.onReview}>
          Review...
        </Button>
        {(row.consent || row.suspended) && (
          <Button size="sm" variant="outline" aria-label={`Revoke consent for ${row.label}`} onClick={props.onRevoke}>
            Revoke consent
          </Button>
        )}
      </div>
    </li>
  );
}
