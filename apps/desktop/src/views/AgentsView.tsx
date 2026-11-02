import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ErrorAlert } from "@/components/ErrorAlert";
import { PageHeading } from "@/components/PageHeading";
import { Button } from "@/components/ui/button";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { ipc, type Agent, type RunState, type RunSummary } from "@/lib/ipc";
import { formatDate } from "@/lib/utils";

const LIMIT = 100;
const POLL_MS = 2000;

const AGENTS: Record<Agent, string> = {
  resume: "Resume",
  cover_letter: "Cover letter",
  career_memory: "Career memory",
  tracking: "Tracking",
  analytics: "Analytics",
};

const STATES: Record<RunState, string> = {
  queued: "Queued",
  checking: "Checking",
  running: "Running",
  checkpointed: "Checkpointed",
  retrying: "Retrying",
  waiting_for_human: "Waiting for you",
  halted: "Halted",
  completed: "Completed",
  manual_required: "Manual work needed",
  refused: "Refused",
  failed: "Failed",
  cancelled: "Cancelled",
};

const ENDED: RunState[] = ["completed", "manual_required", "refused", "failed", "cancelled"];
/** Still moving: the row changes without anyone here touching it. */
const isActive = (run: RunSummary) => run.state !== "halted" && !ENDED.includes(run.state);

const time = (iso: string) => {
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? iso : date.toLocaleTimeString();
};

function RunLog({ id }: { id: string }) {
  const client = useQueryClient();
  const detail = useQuery({
    queryKey: ["agents", "get", id],
    queryFn: () => ipc.agentGet(id),
    refetchInterval: (query) => {
      const run = query.state.data?.run;
      return run && isActive(run) ? POLL_MS : false;
    },
  });
  const refresh = () => client.invalidateQueries({ queryKey: ["agents"] });
  const cancel = useMutation({ mutationFn: () => ipc.agentCancel(id), onSuccess: refresh });
  const resume = useMutation({ mutationFn: () => ipc.agentResume(id), onSettled: refresh });

  const run = detail.data?.run;
  const halted = run?.state === "halted";
  return (
    <section aria-label="Selected run" className="grid gap-3">
      <ErrorAlert error={detail.error ?? cancel.error ?? resume.error} />
      {run && (isActive(run) || halted) && (
        <div className="flex gap-2">
          {halted && (
            <Button onClick={() => resume.mutate()} disabled={resume.isPending}>
              Resume
            </Button>
          )}
          <Button variant="outline" onClick={() => cancel.mutate()} disabled={cancel.isPending}>
            Cancel run
          </Button>
        </div>
      )}
      {detail.data && (
        <Table aria-label="Run log">
          <TableHeader>
            <TableRow>
              <TableHead>Time</TableHead>
              <TableHead>Step</TableHead>
              <TableHead>State</TableHead>
              <TableHead>Duration</TableHead>
              <TableHead>Provider</TableHead>
              <TableHead>Categories</TableHead>
              <TableHead>Outcome</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {detail.data.log.map((row, index) => (
              <TableRow key={index}>
                <TableCell>{time(row.at)}</TableCell>
                <TableCell>{row.step}</TableCell>
                <TableCell>{STATES[row.state]}</TableCell>
                <TableCell>{row.duration_ms} ms</TableCell>
                <TableCell>{row.provider ?? "-"}</TableCell>
                <TableCell>{row.data_categories.join(", ") || "-"}</TableCell>
                <TableCell>{row.outcome}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </section>
  );
}

export function AgentsView() {
  const [selected, setSelected] = useState<string | null>(null);
  const runs = useQuery({
    queryKey: ["agents", "list"],
    queryFn: () => ipc.agentsList(LIMIT),
    refetchInterval: (query) => (query.state.data?.some(isActive) ? POLL_MS : false),
  });

  const rows = runs.data ?? [];
  return (
    <div className="grid gap-4">
      <PageHeading>Agents</PageHeading>
      <ErrorAlert error={runs.error} />
      {runs.isSuccess && rows.length === 0 && (
        <p className="text-sm text-muted-foreground">No agent has run yet.</p>
      )}
      {rows.length > 0 && (
        <Table aria-label="Agent runs">
          <TableHeader>
            <TableRow>
              <TableHead>Agent</TableHead>
              <TableHead>State</TableHead>
              <TableHead>Step</TableHead>
              <TableHead>Started</TableHead>
              <TableHead>Reason</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((run) => (
              <TableRow key={run.id}>
                <TableCell>
                  <Button
                    variant={run.id === selected ? "secondary" : "ghost"}
                    size="sm"
                    aria-current={run.id === selected ? "true" : undefined}
                    onClick={() => setSelected(run.id)}
                  >
                    {AGENTS[run.agent]}
                  </Button>
                </TableCell>
                <TableCell>{STATES[run.state]}</TableCell>
                <TableCell>
                  {run.step} of {run.steps}
                </TableCell>
                <TableCell>
                  {formatDate(run.created_at)} {time(run.created_at)}
                </TableCell>
                <TableCell>{run.reason ?? "-"}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {selected !== null && <RunLog key={selected} id={selected} />}
    </div>
  );
}
