import { useState, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ErrorAlert } from "@/components/ErrorAlert";
import { PageHeading } from "@/components/PageHeading";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ipc, toAppError, type AiStatus, type DataCategory, type Fit, type Provider } from "@/lib/ipc";

const FIT: Record<Fit, string> = {
  fits: "Fits this computer",
  may_slow: "May be slow here",
  too_large: "Too large for this computer",
};

const GB = 2 ** 30;
const gb = (bytes: number) => `${(bytes / GB).toFixed(bytes < GB ? 2 : 1)} GB`;
const size = (bytes: number) => (bytes < 1_000_000_000 ? `${Math.round(bytes / 1e6)} MB` : `${(bytes / 1e9).toFixed(1)} GB`);

// ponytail: Ollama has no listing of what it could download, so this short list is kept here.
// Names and sizes were checked on ollama.com on 2026-11-01 (llama3.2:3b 2.0GB, qwen2.5:7b
// 4.7GB, nomic-embed-text 274MB). Past a handful of entries, move it to a data file.
const SUGGESTED = [
  { name: "llama3.2:3b", bytes: 2_000_000_000 },
  { name: "qwen2.5:7b", bytes: 4_700_000_000 },
  { name: "nomic-embed-text", bytes: 274_000_000 },
];

/** Core's `hardware::fit`, same thresholds: size plus 20% fits within 75% of memory. */
function fitFor(bytes: number, memory: number): Fit {
  const need = BigInt(bytes) * 6n;
  const total = BigInt(memory) * 5n;
  if (need * 4n <= total * 3n) return "fits";
  return need <= total ? "may_slow" : "too_large";
}

// ponytail: Task 20 has no network-specific code; these are the ones a failed connection can produce.
const NETWORK_CODES = new Set(["io", "timeout"]);

const CATEGORIES: [DataCategory, string][] = [
  ["job_text", "Job text"],
  ["company_text", "Company text"],
  ["profile_basic", "Basic profile"],
  ["resume_text", "Resume text"],
  ["career_memory", "Career memory answers"],
];

const PROVIDERS: Record<Provider, string> = { ollama: "Ollama", claude_code: "Claude Code" };

function Section(props: { id: string; title: string; children: ReactNode }) {
  return (
    <section aria-labelledby={props.id} className="grid gap-3 rounded-lg border p-4">
      <h2 id={props.id} className="font-medium">
        {props.title}
      </h2>
      {props.children}
    </section>
  );
}

function ConsentDialog({ onClose }: { onClose(): void }) {
  const client = useQueryClient();
  const [picked, setPicked] = useState<DataCategory[]>([]);
  const toggle = (c: DataCategory, on: boolean) => setPicked(on ? [...picked, c] : picked.filter((p) => p !== c));
  // Rust shows its own native confirmation inside `ai_consent_give`; enabling follows it.
  const give = useMutation({
    mutationFn: async () => {
      await ipc.aiConsentGive("claude_code", picked);
      await ipc.aiEnable("claude_code", true);
    },
    onSuccess: async () => {
      await client.resetQueries({ queryKey: ["ai"] });
      onClose();
    },
  });
  return (
    <Dialog open onOpenChange={(open) => !open && !give.isPending && onClose()}>
      <DialogContent showCloseButton={false}>
        <DialogHeader>
          <DialogTitle>Share data with Claude Code (your own login)</DialogTitle>
          <DialogDescription>Nothing is sent until you confirm. No kind of data is ticked for you.</DialogDescription>
        </DialogHeader>
        <form
          className="grid gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            give.mutate();
          }}
        >
          <fieldset className="grid gap-1.5">
            <legend className="mb-1 text-sm">Allow these kinds of data to be sent:</legend>
            {CATEGORIES.map(([value, label]) => (
              <label key={value} className="flex items-center gap-2 text-sm">
                <input type="checkbox" checked={picked.includes(value)} onChange={(e) => toggle(value, e.target.checked)} />
                {label}
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
          <p className="text-sm">
            How Claude Code uses it: Consumer Terms allow training unless the user opts out.
          </p>
          <p className="text-sm">
            Job text goes to Claude Code with no tools and is treated as data. You can revoke this
            any time. Revoking stops calls at once but cannot recall data already sent.
          </p>
          <ErrorAlert error={give.error} />
          <DialogFooter>
            <Button type="button" variant="outline" disabled={give.isPending} onClick={onClose}>
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

/** A change to AI settings. The `["ai"]` query is reset after it, so the screen shows what Rust now holds. */
function useAction<V>(fn: (v: V) => Promise<unknown>) {
  const client = useQueryClient();
  return useMutation({ mutationFn: fn, onSuccess: () => client.resetQueries({ queryKey: ["ai"] }) });
}

export function AiView() {
  const client = useQueryClient();
  const ai = useQuery({ queryKey: ["ai"], queryFn: ipc.aiStatus });
  const [downloading, setDownloading] = useState<{ name: string; bytes: number; fit: Fit } | null>(null);
  const [consent, setConsent] = useState(false);
  const [address, setAddress] = useState("");
  const reset = () => client.resetQueries({ queryKey: ["ai"] });
  const enable = useAction(({ provider, on }: { provider: Provider; on: boolean }) => ipc.aiEnable(provider, on));
  const setModels = useAction(({ drafting, embedding }: { drafting: string | null; embedding: string | null }) =>
    ipc.aiSetModels(drafting, embedding),
  );
  const setOrder = useAction((order: Provider[]) => ipc.aiSetOrder(order));
  const pickClaude = useAction<void>(() => ipc.aiSetClaudePath());
  const clearClaude = useAction<void>(() => ipc.aiClearClaudePath());
  const revoke = useAction<void>(async () => {
    await ipc.aiConsentRevoke("claude_code");
    await ipc.aiEnable("claude_code", false);
  });
  const setUrl = useAction((url: string) => ipc.aiSetOllamaUrl(url));
  const pull = useMutation({ mutationFn: (name: string) => ipc.aiPull(name), onSettled: reset });
  const cancelPull = useMutation({ mutationFn: ipc.aiCancelPull });

  const s: AiStatus | undefined = ai.data;
  const running = s?.ollama.state === "running" ? s.ollama : null;
  const installed = running?.models.filter((m) => m.local) ?? [];
  const order: Provider[] = s
    ? [...(s.order as Provider[]), ...(["ollama", "claude_code"] as const).filter((p) => !s.order.includes(p))]
    : [];
  const move = (index: number, by: number) => {
    const next = [...order];
    [next[index], next[index + by]] = [next[index + by], next[index]];
    setOrder.mutate(next);
  };
  const pullError = pull.error && toAppError(pull.error);

  return (
    <div className="grid max-w-4xl gap-4">
      <PageHeading>AI: Ollama and models</PageHeading>
      <p className="text-sm">
        No AI is needed. JobPilot works with none. Local models run on this computer, so your data
        stays here. Cloud models are off.
      </p>
      <ErrorAlert
        error={
          ai.error ?? enable.error ?? setModels.error ?? setOrder.error ?? pickClaude.error ?? clearClaude.error ?? revoke.error ?? setUrl.error
        }
      />
      {s && (
        <>
          <Section id="ollama" title="Ollama">
            <div className="flex items-center justify-between gap-3 text-sm">
              <p>
                {s.ollama.state === "running" && `Running at ${s.ollama.url}`}
                {s.ollama.state === "not_running" &&
                  "Ollama is not running on this computer. Start it, then check again. JobPilot does not install or start it."}
                {s.ollama.state === "refused" && "JobPilot refused the Ollama address."}
              </p>
              <Button variant="outline" size="sm" onClick={() => void ai.refetch()}>
                Check again
              </Button>
            </div>
            {s.ollama.state === "refused" && (
              <ErrorAlert
                error={{
                  code: "refused",
                  message: `The address was refused (${s.ollama.reason}). Only a loopback address is allowed.`,
                }}
              />
            )}
            <p className="text-sm">
              This computer: {gb(s.hardware.total_memory)} memory, {s.hardware.gpu}, {gb(s.hardware.free_disk)} free
            </p>
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={s.enabled.ollama}
                onChange={(e) => enable.mutate({ provider: "ollama", on: e.target.checked })}
              />
              Use Ollama (On)
            </label>
            <form
              className="flex items-end gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                setUrl.mutate(address);
              }}
            >
              <div className="grid gap-1">
                <Label htmlFor="ollama-url">Ollama address (loopback only)</Label>
                <Input id="ollama-url" placeholder="http://127.0.0.1:11434" value={address} onChange={(e) => setAddress(e.target.value)} />
              </div>
              <Button type="submit" variant="outline" size="sm" disabled={address.trim() === ""}>
                Change address...
              </Button>
            </form>
          </Section>

          {running && (
            <>
              <Section id="installed" title="Installed models">
                {installed.length === 0 && (
                  <p className="text-sm text-muted-foreground">
                    No models installed. Download one below, or keep working without AI.
                  </p>
                )}
                <ul className="grid gap-1 text-sm">
                  {installed.map((m) => (
                    <li key={m.name} className="flex gap-3">
                      <span className="font-mono">{m.name}</span>
                      <span>{size(m.size)}</span>
                      <span data-fit>{FIT[m.fits]}</span>
                    </li>
                  ))}
                </ul>
                {installed.length > 0 && !installed.some((m) => m.family.toLowerCase().includes("embed") || m.name.includes("embed")) && (
                  <p className="text-sm">
                    No installed model can do embeddings yet. Download one below. Search then uses
                    No-AI matching.
                  </p>
                )}
                {installed.length > 0 &&
                  (
                    [
                      ["Drafting model", s.drafting_model, "drafting"],
                      ["Embedding model", s.embedding_model, "embedding"],
                    ] as const
                  ).map(([label, value, role]) => (
                    <div key={role} className="flex items-center gap-3 text-sm">
                      <Label htmlFor={`model-${role}`}>{label}</Label>
                      <select
                        id={`model-${role}`}
                        className="h-8 rounded-lg border bg-background px-2"
                        value={value ?? ""}
                        onChange={(e) =>
                          e.target.value &&
                          setModels.mutate({
                            drafting: role === "drafting" ? e.target.value : null,
                            embedding: role === "embedding" ? e.target.value : null,
                          })
                        }
                      >
                        <option value="">Not chosen</option>
                        {installed.map((m) => (
                          <option key={m.name} value={m.name}>
                            {m.name}
                          </option>
                        ))}
                      </select>
                    </div>
                  ))}
              </Section>

              <Section id="available" title="Suggested models (sizes approximate; nothing downloads unless you ask)">
                <ul className="grid gap-1 text-sm">
                  {SUGGESTED.filter((m) => !installed.some((i) => i.name === m.name)).map((m) => {
                    const fit = fitFor(m.bytes, s.hardware.total_memory);
                    const busy = pull.isPending && pull.variables === m.name;
                    return (
                      <li key={m.name} className="flex items-center gap-3">
                        <span className="font-mono">{m.name}</span>
                        <span>{size(m.bytes)}</span>
                        <span data-fit>{`Estimated: ${FIT[fit]}`}</span>
                        {busy ? (
                          <Button size="sm" variant="outline" onClick={() => cancelPull.mutate()}>
                            Cancel download
                          </Button>
                        ) : (
                          <Button
                            size="sm"
                            variant="outline"
                            aria-label={`Download ${m.name}`}
                            disabled={pull.isPending}
                            onClick={() => setDownloading({ name: m.name, bytes: m.bytes, fit })}
                          >
                            Download...
                          </Button>
                        )}
                      </li>
                    );
                  })}
                </ul>
                {pull.isPending && (
                  <p role="status" className="text-sm">
                    Downloading {pull.variables}. No progress is shown. A large model can take many
                    minutes; Cancel stops it.
                  </p>
                )}
                {pullError && pullError.code !== "cancelled" && (
                  <>
                    <ErrorAlert error={pull.error} />
                    {NETWORK_CODES.has(pullError.code) && (
                      <p className="text-sm">Downloading needs a network. Installed models keep working.</p>
                    )}
                  </>
                )}
                {pullError?.code === "cancelled" && <p className="text-sm">Download cancelled.</p>}
              </Section>
            </>
          )}

          <Section id="claude" title="Claude Code (your own login)">
            <p className="text-sm">
              {`Claude Code  ${s.claude_code.available ? "available" : "not found"}  ${s.claude_code.path ?? ""}`.trimEnd()}
            </p>
            <div className="flex items-center gap-3 text-sm">
              {!s.claude_code.available && <span>Unavailable: install claude, or point JobPilot at it.</span>}
              {!s.claude_code.available && (
                <Button size="sm" variant="outline" onClick={() => pickClaude.mutate()}>
                  Choose claude...
                </Button>
              )}
              <Button size="sm" variant="outline" onClick={() => clearClaude.mutate()}>
                Use automatic
              </Button>
            </div>
            {s.claude_code.consent ? (
              <div className="grid gap-2 text-sm">
                <p>
                  Consent given for: {s.claude_code.consent.join(", ") || "nothing"}. Consumer Terms
                  allow training unless the user opts out.
                </p>
                <div className="flex items-center gap-3">
                  <label className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      checked={s.enabled.claude_code}
                      disabled={!s.claude_code.available}
                      onChange={(e) => enable.mutate({ provider: "claude_code", on: e.target.checked })}
                    />
                    Use Claude Code
                  </label>
                  <Button size="sm" variant="outline" onClick={() => revoke.mutate()}>
                    Revoke consent
                  </Button>
                </div>
              </div>
            ) : (
              <div className="flex items-center gap-3 text-sm">
                <span>Off. Claude Code is used only after you consent below.</span>
                <Button size="sm" variant="outline" disabled={!s.claude_code.available} onClick={() => setConsent(true)}>
                  Review consent...
                </Button>
              </div>
            )}
          </Section>

          <Section id="order" title="Order">
            <p className="text-sm text-muted-foreground">
              Providers are tried in this order. No-AI (rules, search, manual editing) is always on
              and always last.
            </p>
            <ol className="grid gap-1 text-sm">
              {order.map((p, i) => (
                <li key={p} className="flex items-center gap-3">
                  <span className="font-mono">{p}</span>
                  <Button size="sm" variant="outline" disabled={i === 0 || setOrder.isPending} onClick={() => move(i, -1)}>
                    <span className="sr-only">{`Move ${p} up`}</span>
                    <span aria-hidden>Up</span>
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={i === order.length - 1 || setOrder.isPending}
                    onClick={() => move(i, 1)}
                  >
                    <span className="sr-only">{`Move ${p} down`}</span>
                    <span aria-hidden>Down</span>
                  </Button>
                  {!s.enabled[p] && <span className="text-muted-foreground">{PROVIDERS[p]} is off</span>}
                </li>
              ))}
            </ol>
          </Section>
        </>
      )}
      {ai.isPending && <p className="text-sm text-muted-foreground">Checking...</p>}

      {downloading && (
        <ConfirmDialog
          title={`Download ${downloading.name}?`}
          confirmLabel="Download"
          busyLabel="Starting..."
          confirmVariant="default"
          pending={false}
          onCancel={() => setDownloading(null)}
          onConfirm={() => {
            pull.mutate(downloading.name);
            setDownloading(null);
          }}
        >
          <p>
            {downloading.name} is about {size(downloading.bytes)}. Ollama downloads it to this
            computer. Personal data is never sent in a download.
          </p>
          {downloading.fit === "too_large" && (
            <p>This model is too large for this computer. It may not run, or may freeze other work.</p>
          )}
        </ConfirmDialog>
      )}
      {consent && <ConsentDialog onClose={() => setConsent(false)} />}
    </div>
  );
}
