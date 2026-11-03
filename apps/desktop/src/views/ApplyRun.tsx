import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CancelRun, PausePanel } from "@/components/PausePanel";
import { ErrorAlert } from "@/components/ErrorAlert";
import { Button } from "@/components/ui/button";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { ipc, type ApplyItem, type AuthorizationState, type DocState, type Preflight } from "@/lib/ipc";
import { blockText, itemReason } from "@/lib/reasons";
import { useUiStore } from "@/lib/store";

export const FIXED_COPY =
  "Runs on your own accounts, in a visible browser. Once started, it submits with no check per application. It stops for sign-in, CAPTCHA, codes, identity checks, and legal declarations. A site may restrict an account that uses automation.";
export const UNCERTAIN = "Check the site. JobPilot will not try again.";

const RUNTIME: Record<Preflight["runtime"], string> = {
  no_node: "Node is not installed. Install Node, then set up the browser.",
  node_too_old: "The installed Node is too old. Update it, then set up the browser.",
  not_installed: "The browser runtime is not installed.",
  wrong_version: "The browser runtime is not the version JobPilot pins.",
  ready: "The browser runtime is ready.",
};

const AUTHORIZATION: Record<AuthorizationState, string> = {
  stated: "Stated by you",
  not_stated: "Not stated by you",
  no_country: "No country on the application",
};

const STATES: Record<string, string> = {
  queued: "Queued",
  active: "In progress",
  filled: "Filled",
  submitted: "Submitted",
  uncertain: "Uncertain",
  skipped: "Skipped",
  manual_required: "Handed to you",
};

function doc(state: DocState): string {
  if (state === "ready") return "Ready";
  if (state === "missing") return "None";
  return `Blocked: ${state.blocked}`;
}

/**
 * Wireframe 19: a Claude Code apply run, from the checks before it to what each application
 * ended as. Page text (a pause's excerpt, a confirmation) renders as inert text only.
 */
export function ApplyRun({ applicationIds, onClose }: { applicationIds: string[]; onClose(): void }) {
  const applyRun = useUiStore((s) => s.applyRun);
  return (
    <section aria-labelledby="apply-run-heading" className="grid gap-3 rounded-lg border p-4">
      <h2 id="apply-run-heading" className="font-semibold">
        Apply with Claude Code
      </h2>
      {applyRun ? <RunScreen id={applyRun} onClose={onClose} /> : <PreflightScreen ids={applicationIds} onClose={onClose} />}
    </section>
  );
}

function PreflightScreen({ ids, onClose }: { ids: string[]; onClose(): void }) {
  const client = useQueryClient();
  const setApplyRun = useUiStore((s) => s.setApplyRun);
  const key = ["apply", "preflight", ids];
  const checked = useQuery({ queryKey: key, queryFn: () => ipc.applyPreflight(ids) });
  const setup = useMutation({
    mutationFn: ipc.browserSetup,
    onSuccess: () => client.invalidateQueries({ queryKey: key }),
  });
  const start = useMutation({ mutationFn: () => ipc.applyStart(ids), onSuccess: (id) => setApplyRun(id) });

  const data = checked.data;
  const needsSetup = data !== undefined && (data.runtime !== "ready" || data.claude === null);
  const blocked = data !== undefined && (data.items.some((i) => i.blocks.length > 0) || data.consent_missing.length > 0);

  return (
    <div className="grid gap-3">
      <ErrorAlert error={checked.error ?? setup.error ?? start.error} />
      {checked.isPending && <p className="text-sm text-muted-foreground">Checking...</p>}
      {needsSetup && (
        <div className="grid gap-2">
          <h3 className="text-sm font-semibold">Setup needed</h3>
          {data.runtime !== "ready" && (
            <>
              <p className="text-sm">{RUNTIME[data.runtime]}</p>
              <div>
                <Button type="button" disabled={setup.isPending} onClick={() => setup.mutate()}>
                  {setup.isPending ? "Setting up..." : "Set up browser"}
                </Button>
              </div>
              {setup.isPending && (
                <p role="status" className="text-sm text-muted-foreground">
                  Downloading the browser runtime. This can take a few minutes.
                </p>
              )}
            </>
          )}
          {data.claude === null && (
            <p className="text-sm">Claude Code was not found. Install it, or choose its program on the AI screen.</p>
          )}
        </div>
      )}
      {data && !needsSetup && (
        <>
          <h3 className="text-sm font-semibold">Preflight</h3>
          <p className="text-sm">{FIXED_COPY}</p>
          <Table aria-label="Preflight">
            <TableHeader>
              <TableRow>
                <TableHead>Company</TableHead>
                <TableHead>Host</TableHead>
                <TableHead>Resume</TableHead>
                <TableHead>Letter</TableHead>
                <TableHead>Answers ready</TableHead>
                <TableHead>Sensitive held back</TableHead>
                <TableHead>Work authorization</TableHead>
                <TableHead>Blocks</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {data.items.map((item) => (
                <TableRow key={item.application_id}>
                  <TableCell className="whitespace-normal">{item.company}</TableCell>
                  <TableCell>{item.host}</TableCell>
                  <TableCell>{doc(item.resume)}</TableCell>
                  <TableCell>{doc(item.letter)}</TableCell>
                  <TableCell>{item.answers}</TableCell>
                  <TableCell>{item.sealed}</TableCell>
                  <TableCell>{AUTHORIZATION[item.authorization]}</TableCell>
                  <TableCell className="whitespace-normal">
                    {item.blocks.length === 0 ? (
                      "None"
                    ) : (
                      <ul>
                        {item.blocks.map((code) => (
                          <li key={code}>{blockText(code)}</li>
                        ))}
                      </ul>
                    )}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
          {data.sealed_total > 0 && (
            <p className="text-sm text-muted-foreground">
              Sensitive answers and work authorization stay held back unless you release them to this run, in the
              dialog that opens when you start it.
            </p>
          )}
          {data.consent_missing.length > 0 && (
            <p className="text-sm">
              Your Claude Code consent does not cover: {data.consent_missing.join(", ")}. Review it on the AI screen.
            </p>
          )}
        </>
      )}
      <div className="flex flex-wrap gap-2">
        {data && !needsSetup && (
          <Button type="button" disabled={blocked || start.isPending} onClick={() => start.mutate()}>
            {start.isPending ? "Starting..." : "Start run"}
          </Button>
        )}
        <Button type="button" variant="outline" disabled={checked.isFetching} onClick={() => void checked.refetch()}>
          Check again
        </Button>
        <Button type="button" variant="ghost" onClick={onClose}>
          Back
        </Button>
      </div>
    </div>
  );
}

function RunScreen({ id, onClose }: { id: string; onClose(): void }) {
  const client = useQueryClient();
  const setApplyRun = useUiStore((s) => s.setApplyRun);
  const key = ["apply", "run", id];
  const view = useQuery({
    queryKey: key,
    queryFn: () => ipc.applyView(id),
    refetchInterval: (query) => (query.state.data?.state === "ended" ? false : 1000),
  });
  const settle = useMutation({
    mutationFn: () => ipc.applySettle(id),
    onSuccess: () => client.invalidateQueries({ queryKey: key }),
  });
  const done = () => {
    setApplyRun(null);
    onClose();
  };

  const run = view.data;
  if (!run) {
    return <ErrorAlert error={view.error} />;
  }
  const ended = run.state === "ended";
  return (
    <div className="grid gap-3">
      <ErrorAlert error={view.error ?? settle.error} />
      <p role="status" className="text-sm">
        {run.state === "ready" && "Starting the session in the terminal panel."}
        {run.state === "running" && "Running in the terminal panel and a visible browser."}
        {ended && "Run ended."}
      </p>
      <Table aria-label="Applications in this run">
        <TableHeader>
          <TableRow>
            <TableHead>#</TableHead>
            <TableHead>Site</TableHead>
            <TableHead>State</TableHead>
            <TableHead>Result</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {run.items.map((item) => (
            <TableRow key={item.id}>
              <TableCell>{item.position + 1}</TableCell>
              <TableCell>{item.host}</TableCell>
              <TableCell>{STATES[item.state] ?? item.state}</TableCell>
              <TableCell className="whitespace-normal">
                <Result item={item} ended={ended} pending={settle.isPending} onSettle={() => settle.mutate()} />
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {!ended && run.active_pause && <PausePanel runId={run.id} pause={run.active_pause} />}
      <div className="flex flex-wrap gap-2">
        {!ended && !run.active_pause && <CancelRun runId={run.id} />}
        {ended && (
          <Button type="button" onClick={done}>
            Done
          </Button>
        )}
      </div>
    </div>
  );
}

function Result({
  item,
  ended,
  pending,
  onSettle,
}: {
  item: ApplyItem;
  ended: boolean;
  pending: boolean;
  onSettle(): void;
}) {
  return (
    <div className="grid gap-1">
      {item.state === "uncertain" && <span>{UNCERTAIN}</span>}
      {item.reason && <span className="text-muted-foreground">{itemReason(item.reason)}</span>}
      {item.confirmation_text && (
        <div className="grid gap-0.5">
          <span className="text-muted-foreground">The page said</span>
          <blockquote className="whitespace-pre-wrap border-l-2 pl-2">{item.confirmation_text}</blockquote>
        </div>
      )}
      {ended && item.state === "submitted" && !item.status_written && (
        <div>
          <Button type="button" size="sm" disabled={pending} onClick={onSettle}>
            Record status
          </Button>
        </div>
      )}
      {item.state === "submitted" && item.status_written && <span>Status recorded as applied.</span>}
    </div>
  );
}
