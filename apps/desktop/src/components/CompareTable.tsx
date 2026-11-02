import { useRef } from "react";
import { cn } from "@/lib/utils";
import type { Claim, Compare, DraftKind, JobText } from "@/lib/ipc";

const STATUS: Record<Claim["status"], string> = {
  ok: "OK",
  no_evidence: "NO EVIDENCE: remove or fix",
  authorization_statement: "AUTHORIZATION STATEMENT: remove or fix",
  removed: "REMOVED",
};

const WHY: Partial<Record<Claim["status"], string>> = {
  no_evidence:
    "No approved knowledge item backs this line. Cite one with Edit claim, or remove the line.",
  authorization_statement:
    "This line uses words about work authorization, visas, or sponsorship, and JobPilot never states those for you. The check matches words, not meaning, so an employer such as \"Visa Inc.\" is held back too. A line like this passes only when the same sentence is a knowledge item you typed yourself, word for word.",
};

type Props = {
  compare: Compare;
  kind: DraftKind;
  posting: JobText | null;
  selected: number;
  onSelect(index: number): void;
  onEdit(): void;
};

/**
 * Original and generated side by side, with the evidence and the posting beside the draft.
 * All of it renders as text: the posting and the evidence are untrusted.
 */
export function CompareTable({ compare, kind, posting, selected, onSelect, onEdit }: Props) {
  const tag = (id: string) => {
    const at = compare.evidence.findIndex((e) => e.id === id);
    return at < 0 ? "[--]" : `[E${at + 1}]`;
  };
  const list = useRef<HTMLUListElement>(null);
  const cited = new Set(compare.generated[selected]?.evidence_ids ?? []);
  const move = (to: number) => {
    const next = Math.max(0, Math.min(compare.generated.length - 1, to));
    onSelect(next);
    list.current?.querySelectorAll<HTMLElement>("[role=option]")[next]?.focus();
  };

  return (
    <div className="grid gap-4">
      <div className={cn("grid gap-4", kind === "resume" ? "grid-cols-[1fr_1.4fr_1fr]" : "grid-cols-[1.4fr_1fr]")}>
        {kind === "resume" && (
          <section aria-labelledby="orig-h" className="grid content-start gap-2">
            <h2 id="orig-h" className="text-sm font-medium">
              Original
            </h2>
            {compare.original.length === 0 ? (
              <p className="text-sm text-muted-foreground">No original resume to compare.</p>
            ) : (
              compare.original.map((line, i) => (
                <p key={i} className="text-sm">
                  {line}
                </p>
              ))
            )}
          </section>
        )}
        <section aria-labelledby="gen-h" className="grid content-start gap-2">
          <h2 id="gen-h" className="text-sm font-medium">
            Generated
          </h2>
          <ul
            ref={list}
            role="listbox"
            aria-labelledby="gen-h"
            className="grid gap-2"
            onKeyDown={(e) => {
              if (e.key === "ArrowDown") (e.preventDefault(), move(selected + 1));
              else if (e.key === "ArrowUp") (e.preventDefault(), move(selected - 1));
              else if (e.key === "e") (e.preventDefault(), onEdit());
            }}
          >
            {compare.generated.map((claim, i) => {
              const flagged = claim.status !== "ok";
              return (
                <li
                  key={claim.id || i}
                  role="option"
                  aria-selected={i === selected}
                  tabIndex={i === selected ? 0 : -1}
                  onClick={() => onSelect(i)}
                  className={cn("rounded-lg border p-2 text-sm", i === selected && "ring-2 ring-ring")}
                >
                  <p>{claim.text}</p>
                  <p className="mt-1 flex flex-wrap items-center gap-2 text-xs">
                    {claim.evidence_ids.length === 0 ? (
                      <span>[--]</span>
                    ) : (
                      claim.evidence_ids.map((id) => <span key={id}>{tag(id)}</span>)
                    )}
                    <span className={cn("font-medium", flagged && "text-destructive")}>
                      {STATUS[claim.status]}
                    </span>
                  </p>
                  {flagged && WHY[claim.status] && (
                    <p className="mt-1 text-xs text-muted-foreground">{WHY[claim.status]}</p>
                  )}
                </li>
              );
            })}
          </ul>
        </section>
        <aside aria-label="Posting" className="grid content-start gap-2">
          <h2 className="text-sm font-medium">Posting{posting ? `: ${posting.title}` : ""}</h2>
          {posting ? (
            <pre className="max-h-96 overflow-auto whitespace-pre-wrap rounded-lg border p-2 text-xs">
              {posting.normalized}
            </pre>
          ) : (
            <p className="text-sm text-muted-foreground">This draft is not tied to a job posting.</p>
          )}
        </aside>
      </div>
      <section aria-labelledby="ev-h" className="grid gap-1 rounded-lg border p-3">
        <h2 id="ev-h" className="text-sm font-medium">
          Evidence
        </h2>
        {compare.evidence.length === 0 && (
          <p className="text-sm text-muted-foreground">No evidence is cited yet.</p>
        )}
        {compare.evidence.map((item, i) => (
          <p key={item.id} className={cn("text-sm", cited.has(item.id) && "font-medium")}>
            <span className="mr-2 font-mono">E{i + 1}</span>
            {/* `compare` lists evidence only (approved, unmarked), so no body here is sealed. */}
            {`${item.kind}: "${item.body}"`}
          </p>
        ))}
      </section>
    </div>
  );
}
