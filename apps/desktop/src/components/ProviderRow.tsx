import { labelOf } from "@/components/ConsentDialog";
import { Button } from "@/components/ui/button";
import type { ExternalStatus } from "@/lib/ipc";
import { SUSPENDED } from "@/lib/reasons";

/** A provider's reason code, in plain words. */
const UNAVAILABLE: Record<string, string> = {
  not_built: "not built yet",
};

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
