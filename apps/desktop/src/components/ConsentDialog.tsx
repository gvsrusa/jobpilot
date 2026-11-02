import { useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ErrorAlert } from "@/components/ErrorAlert";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ipc, type DataCategory, type Provider, type Sharing, type TaskSharing } from "@/lib/ipc";

const CATEGORIES: [DataCategory, string][] = [
  ["job_text", "Job text"],
  ["company_text", "Company text"],
  ["profile_basic", "Basic profile"],
  ["resume_text", "Resume text"],
  ["career_memory", "Career memory answers"],
];

/** Core's `DRAFT_DATA`. A provider is skipped for drafting unless consent covers all three. */
export const DRAFTING: DataCategory[] = ["job_text", "profile_basic", "resume_text"];

export const labelOf = (category: DataCategory) =>
  CATEGORIES.find(([value]) => value === category)?.[1] ?? (category === "sensitive" ? "Sensitive" : category);

const TASKS: Record<TaskSharing["task"], string> = {
  parse_job: "Reading a posting",
  draft_resume: "Resume",
  draft_cover_letter: "Cover letter",
  classify_answer: "Sorting an answer",
};

/** The part of the consent key after the slash, in words. */
const SIGN_IN: Record<string, string> = {
  cli_login: "your own login",
  api_key: "your own key",
  paid_key: "your own paid key",
  loopback: "your own server",
};

const skipped = (reason: string) =>
  reason === "no_tools" ? "Skipped: this provider cannot prove it has no tools" : `Skipped: ${reason}`;

/** The real prompt for the newest pasted posting, as inert text. Nothing is sent to build it. */
function ExactPrompt() {
  const jobs = useQuery({ queryKey: ["jobs"], queryFn: ipc.jobList });
  const newest = jobs.data?.[0];
  const preview = useMutation({ mutationFn: ipc.aiPreview });
  if (jobs.isPending) return null;
  if (!newest) return <p className="text-sm text-muted-foreground">Paste a posting to see this.</p>;
  return (
    <div className="grid justify-items-start gap-1.5">
      <Button type="button" variant="outline" size="sm" onClick={() => preview.mutate(newest.id)}>
        Show exactly what will be sent
      </Button>
      <ErrorAlert error={preview.error} />
      {preview.data && (
        <pre className="max-h-60 w-full overflow-auto rounded-lg border p-2 text-xs whitespace-pre-wrap select-text">
          {preview.data.system}
          {"\n\n"}
          {preview.data.user}
        </pre>
      )}
    </div>
  );
}

function ConsentForm({ sharing, onClose }: { sharing: Sharing; onClose(): void }) {
  const client = useQueryClient();
  const { label } = sharing;
  const cancel = useRef<HTMLButtonElement>(null);
  const [picked, setPicked] = useState<DataCategory[]>(sharing.given ?? []);
  const toggle = (c: DataCategory, on: boolean) => setPicked(on ? [...picked, c] : picked.filter((p) => p !== c));
  // Rust shows its own native confirmation inside `ai_consent_give`; enabling follows it.
  const give = useMutation({
    mutationFn: async (categories: DataCategory[]) => {
      await ipc.aiConsentGive(sharing.name, categories);
      await ipc.aiEnable(sharing.name, true);
    },
    onSuccess: async () => {
      await client.resetQueries({ queryKey: ["ai"] });
      onClose();
    },
  });
  const allowDrafting = () => {
    const categories = [...picked, ...DRAFTING.filter((c) => !picked.includes(c))];
    setPicked(categories);
    give.mutate(categories);
  };
  const drafts = sharing.tasks.some((t) => t.task === "draft_resume" && t.runs);
  const runsAny = sharing.tasks.some((t) => t.runs);
  const signIn = SIGN_IN[sharing.key.split("/")[1] ?? ""];
  return (
    <Dialog open onOpenChange={(open) => !open && !give.isPending && onClose()}>
      <DialogContent
        showCloseButton={false}
        className="max-h-[85vh] overflow-y-auto sm:max-w-lg"
        onOpenAutoFocus={(e) => {
          e.preventDefault();
          cancel.current?.focus();
        }}
      >
        <DialogHeader>
          <DialogTitle>{`Share data with ${label}${signIn ? ` (${signIn})` : ""}`}</DialogTitle>
          <DialogDescription>
            Nothing is sent until you confirm. Only what you already allowed starts ticked.
          </DialogDescription>
        </DialogHeader>
        <form
          className="grid gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            give.mutate(picked);
          }}
        >
          {drafts && (
            <div className="grid justify-items-start gap-1.5 text-sm">
              <p>
                Resumes and cover letters need {DRAFTING.slice(0, -1).map(labelOf).join(", ")}, and{" "}
                {labelOf(DRAFTING[DRAFTING.length - 1])}. With fewer, {label} is skipped for them.
              </p>
              <Button type="button" variant="outline" size="sm" disabled={give.isPending} onClick={allowDrafting}>
                Allow what drafting needs
              </Button>
            </div>
          )}
          <fieldset className="grid gap-1.5">
            <legend className="mb-1 text-sm">Allow these kinds of data to be sent:</legend>
            {CATEGORIES.map(([value, name]) => (
              <label key={value} className="flex items-center gap-2 text-sm">
                <input type="checkbox" checked={picked.includes(value)} onChange={(e) => toggle(value, e.target.checked)} />
                {name}
              </label>
            ))}
            <label className="flex items-start gap-2 text-sm">
              <input
                type="checkbox"
                checked={picked.includes("sensitive")}
                onChange={(e) => toggle("sensitive", e.target.checked)}
              />
              <span>
                Sensitive
                <span className="block text-xs text-muted-foreground">
                  Work authorization, pay history, answers marked sensitive. Off unless you tick it
                  separately.
                </span>
              </span>
            </label>
          </fieldset>
          <div className="grid gap-1 text-sm">
            <h3 className="font-medium">What will be sent</h3>
            <ul className="grid gap-0.5">
              {sharing.tasks.map((t) => (
                <li key={t.task}>
                  {`${TASKS[t.task]}: `}
                  {t.skipped_because ? skipped(t.skipped_because) : t.categories.map(labelOf).join(", ")}
                </li>
              ))}
            </ul>
            <ExactPrompt />
          </div>
          <div className="grid gap-1 text-sm">
            <h3 className="font-medium">Where it goes</h3>
            <p>{sharing.destination}</p>
            {sharing.path && (
              <p className="break-all">
                {sharing.name === "endpoint" ? "Address" : "Program"}: {sharing.path}
              </p>
            )}
          </div>
          <div className="grid gap-1 text-sm">
            <h3 className="font-medium">{`How ${label} uses it (terms read ${sharing.terms_date})`}</h3>
            <p>{sharing.data_use}</p>
          </div>
          <p className="text-sm">
            {runsAny && `Job text goes to ${label} with no tools and is treated as data. `}
            You can revoke this any time. Revoking stops calls at once but cannot recall data
            already sent.
          </p>
          <ErrorAlert error={give.error} />
          {sharing.attestation && <p className="text-sm font-medium">{sharing.attestation}</p>}
          <DialogFooter>
            <Button ref={cancel} type="button" variant="outline" disabled={give.isPending} onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" disabled={picked.length === 0 || give.isPending}>
              {give.isPending ? "Waiting for confirmation..." : "Allow selected"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** Opens once the record is read, so a review of a suspended consent starts ticked. */
export function ConsentDialog({ provider, onClose }: { provider: Provider; onClose(): void }) {
  // No cache: what starts ticked must be what Rust holds now.
  const sharing = useQuery({
    queryKey: ["sharing", provider],
    queryFn: () => ipc.aiSharing(provider),
    gcTime: 0,
  });
  if (sharing.error) {
    return (
      <Dialog open onOpenChange={(open) => !open && onClose()}>
        <DialogContent showCloseButton={false}>
          <DialogHeader>
            <DialogTitle>Share data</DialogTitle>
            <DialogDescription>This could not be read, so nothing can be allowed.</DialogDescription>
          </DialogHeader>
          <ErrorAlert error={sharing.error} />
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              Cancel
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    );
  }
  return sharing.data ? <ConsentForm sharing={sharing.data} onClose={onClose} /> : null;
}
