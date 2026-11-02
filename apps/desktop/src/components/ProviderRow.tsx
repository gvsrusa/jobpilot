import { useId, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { labelOf } from "@/components/ConsentDialog";
import { ErrorAlert } from "@/components/ErrorAlert";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ipc, toAppError, type ExternalStatus } from "@/lib/ipc";
import { ENDPOINT_SUSPENDED, explainReason, SUSPENDED } from "@/lib/reasons";

/** A provider's reason code, in plain words. */
const UNAVAILABLE: Record<string, string> = {
  not_built: "not built yet",
  no_key: "no key stored",
  no_api_model: "no model chosen",
  no_endpoint: "no address set",
  not_installed: "not installed",
  not_searched: "turn on to look for it",
  wrong_path: "the path set for it is not that program",
  version_failed: "did not answer --version",
  no_answer: "did not answer in time",
  cli_too_old: "older than the version JobPilot checked",
};

/** An AI-screen change. The row and any alert stay up while the status refreshes. */
function useRowAction<V>(fn: (v: V) => Promise<unknown>) {
  const client = useQueryClient();
  return useMutation({ mutationFn: fn, onSuccess: () => client.invalidateQueries({ queryKey: ["ai"] }) });
}

/** The vendor's own id, typed by hand. Core refuses one that is not eligible. */
function ModelControls({ row }: { row: ExternalStatus }) {
  const id = useId();
  const [model, setModel] = useState(row.model ?? "");
  const save = useRowAction((value: string) => ipc.aiSetApiModel(row.name, value));
  const failure = save.error && toAppError(save.error);
  return (
    <div className="grid gap-2">
      <form
        className="flex flex-wrap items-end gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          save.mutate(model);
        }}
      >
        <div className="grid gap-1">
          <Label htmlFor={id}>Model id</Label>
          <Input id={id} value={model} onChange={(e) => setModel(e.target.value)} />
        </div>
        <Button
          type="submit"
          size="sm"
          variant="outline"
          aria-label={`${row.label}: Save model id`}
          disabled={save.isPending || model.trim() === ""}
        >
          Save
        </Button>
      </form>
      <ErrorAlert error={failure && { ...failure, message: explainReason(failure) ?? failure.message }} />
    </div>
  );
}

/** Rust refuses an address off this machine, then asks in its own dialog. */
function EndpointControls({ row }: { row: ExternalStatus }) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const [address, setAddress] = useState("");
  const save = useRowAction(async (url: string) => {
    await ipc.aiSetEndpoint(url);
    setOpen(false);
  });
  return (
    <div className="grid gap-2">
      <p className="text-muted-foreground">
        Loopback only: a server you run on this computer. It still needs your consent, as that server can pass on what it gets.
      </p>
      <div className="flex flex-wrap items-center gap-3">
        {!open && (
          <Button size="sm" variant="outline" onClick={() => setOpen(true)}>
            {row.path ? "Change endpoint..." : "Add endpoint..."}
          </Button>
        )}
        {row.path && (
          <Button size="sm" variant="outline" disabled={save.isPending} onClick={() => save.mutate("")}>
            Clear endpoint...
          </Button>
        )}
      </div>
      {open && (
        <form
          className="flex flex-wrap items-end gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            save.mutate(address);
          }}
        >
          <div className="grid gap-1">
            <Label htmlFor={id}>Endpoint address (loopback only)</Label>
            <Input
              id={id}
              placeholder="http://127.0.0.1:1234/v1"
              value={address}
              onChange={(e) => setAddress(e.target.value)}
            />
          </div>
          <Button type="submit" size="sm" variant="outline" disabled={save.isPending || address.trim() === ""}>
            Use this address...
          </Button>
        </form>
      )}
      <ErrorAlert error={save.error} />
    </div>
  );
}

/** The program a CLI row runs. Rust opens the picker and checks the name: only the provider crosses. */
function CliControls({ row }: { row: ExternalStatus }) {
  const program = row.name.replace(/_cli$/, "");
  const choose = useRowAction(() => ipc.aiSetCliPath(row.name));
  const clear = useRowAction(() => ipc.aiClearCliPath(row.name));
  return (
    <div className="grid gap-2">
      <div className="flex flex-wrap items-center gap-3">
        {!row.path && (
          <Button size="sm" variant="outline" disabled={choose.isPending} onClick={() => choose.mutate(undefined)}>
            {`Choose ${program}...`}
          </Button>
        )}
        {(row.path || row.reason === "wrong_path") && (
          <Button size="sm" variant="outline" disabled={clear.isPending} onClick={() => clear.mutate(undefined)}>
            Use automatic
          </Button>
        )}
      </div>
      <ErrorAlert error={choose.error ?? clear.error} />
    </div>
  );
}

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
  // Turning it on is what runs the full search, so "not found yet" must not block that, and
  // nothing may stop a provider that is on from being turned off, least of all it being gone.
  const lookOnUse = reason === "not_searched";
  return (
    <li aria-label={row.label} className="grid gap-2 border-t pt-2 text-sm first:border-t-0 first:pt-0">
      <div className="flex flex-wrap items-center gap-3">
        <span className="font-medium">{row.label}</span>
        {!row.available && <span>Unavailable: {UNAVAILABLE[reason] ?? reason}</span>}
        {row.path && <span className="break-all">{row.path}</span>}
      </div>
      {row.suspended && <p role="alert">{row.kind === "endpoint" ? ENDPOINT_SUSPENDED : SUSPENDED}</p>}
      {row.kind === "endpoint" && <EndpointControls row={row} />}
      {row.kind === "cli" && <CliControls row={row} />}
      {row.kind === "cli" && row.runs_job_text && (
        <p className="text-muted-foreground">Runs job text with every tool denied.</p>
      )}
      {row.kind === "cli" && !row.runs_job_text && (
        <p className="text-muted-foreground">No task can use this yet: it cannot prove it has no tools.</p>
      )}
      {row.newer_than_checked && (
        <p className="text-muted-foreground">This version is newer than the one JobPilot checked.</p>
      )}
      {row.name === "gemini_cli" && (
        <p className="text-muted-foreground">
          The CLI keeps its own transcript of each prompt under ~/.gemini/tmp, 30 days by default.
        </p>
      )}
      {row.kind !== "cli" && <ModelControls row={row} />}
      {row.kind !== "cli" && <KeyControls row={row} />}
      {row.name === "google" && <p className="text-muted-foreground">Free keys are not supported.</p>}
      {row.consent && <p>Consent given for: {row.consent.map(labelOf).join(", ")}.</p>}
      <div className="flex flex-wrap items-center gap-3">
        {row.consent && (
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={enabled}
              disabled={!enabled && !row.available && !lookOnUse}
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
