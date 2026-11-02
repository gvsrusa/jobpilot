import { useId, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ErrorAlert } from "@/components/ErrorAlert";
import { SealedText } from "@/components/KnowledgeSection";
import { PageHeading } from "@/components/PageHeading";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { explainReason } from "@/lib/reasons";
import { CLASSES, ipc, type Answer, type KnowledgeItem, type MemoryClass, type QuestionRow } from "@/lib/ipc";

const LABELS: Record<MemoryClass, string> = {
  normal: "Normal",
  sensitive: "Sensitive",
  legal: "Legal",
  work_authorization: "Work authorization",
};

const selectClass =
  "h-8 rounded-lg border border-input bg-transparent px-2.5 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50";

const itemLabel = (item: KnowledgeItem) =>
  item.sealed ? `Sealed ${item.kind}` : `${item.kind}: ${item.body.slice(0, 60)}`;

type Removing = { kind: "question" | "answer"; id: string };

function useRefresh() {
  const client = useQueryClient();
  return () => client.invalidateQueries({ queryKey: ["memory"] });
}

/** Stored text is inert here: never an anchor, and a sealed body comes only from Show. */
function AnswerRow({
  answer,
  items,
  noValue,
  onRemove,
}: {
  answer: Answer;
  items: KnowledgeItem[];
  noValue: boolean;
  onRemove: () => void;
}) {
  const refresh = useRefresh();
  const editorId = useId();
  const [editing, setEditing] = useState<string | null>(null);
  const approve = useMutation({
    mutationFn: () => ipc.memoryApprove(answer.id, !answer.approved),
    onSettled: refresh,
  });
  const reviewed = useMutation({ mutationFn: () => ipc.memoryMarkReviewed(answer.id), onSettled: refresh });
  const edit = useMutation({
    mutationFn: () => ipc.memoryEditAnswer(answer.id, editing!),
    onSuccess: () => {
      setEditing(null);
      return refresh();
    },
  });
  const cleared = !answer.sealed && answer.body === "";
  const busy = approve.isPending || reviewed.isPending;
  const item = items.find((i) => i.id === answer.source_ref);

  return (
    <li className="grid gap-1 rounded border p-2 text-sm">
      {answer.sealed ? (
        <SealedText key={answer.version} reveal={() => ipc.memoryReveal(answer.id)} />
      ) : cleared ? (
        <span className="text-muted-foreground">
          The text was cleared when its key was given up. Write it again, or remove it.
        </span>
      ) : (
        <span className="whitespace-pre-wrap select-text">{answer.body}</span>
      )}
      {editing !== null && (
        <form
          className="grid gap-1.5"
          onSubmit={(e) => {
            e.preventDefault();
            if (editing.trim() && !edit.isPending) edit.mutate();
          }}
        >
          <Label htmlFor={editorId}>New text</Label>
          <Textarea id={editorId} value={editing} onChange={(e) => setEditing(e.target.value)} />
          <span className="flex gap-2">
            <Button type="submit" size="sm" disabled={!editing.trim() || edit.isPending}>
              Save
            </Button>
            <Button type="button" size="sm" variant="outline" onClick={() => setEditing(null)}>
              Cancel
            </Button>
          </span>
        </form>
      )}
      <ErrorAlert error={approve.error ?? reviewed.error ?? edit.error} />
      <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
        <span>{answer.approved ? "Approved" : "Waiting"}</span>
        {answer.stale && <span>Stale</span>}
        <span>
          Source: {answer.source}
          {answer.source_ref && `, ${item ? itemLabel(item) : answer.source_ref}`}
        </span>
        {!noValue && (
          <>
            <Button size="sm" variant="outline" disabled={busy || cleared} onClick={() => approve.mutate()}>
              {answer.approved ? "Unapprove" : "Approve"}
            </Button>
            <Button size="sm" variant="outline" disabled={busy || cleared} onClick={() => reviewed.mutate()}>
              Reviewed
            </Button>
            <Button size="sm" variant="outline" onClick={() => setEditing(answer.body)}>
              Edit
            </Button>
          </>
        )}
        <Button size="sm" variant="outline" onClick={onRemove}>
          Remove
        </Button>
      </div>
    </li>
  );
}

function QuestionCard({
  row,
  items,
  onRemove,
}: {
  row: QuestionRow;
  items: KnowledgeItem[];
  onRemove: (removing: Removing) => void;
}) {
  const { question, answers } = row;
  const refresh = useRefresh();
  const ids = useId();
  const [text, setText] = useState("");
  const [source, setSource] = useState("");
  const setClass = useMutation({
    mutationFn: (to: MemoryClass) => ipc.memorySetClass(question.id, to),
    onSettled: refresh,
  });
  const add = useMutation({
    mutationFn: () => ipc.memoryAddAnswer(question.id, text, source || null),
    onSuccess: () => {
      setText("");
      setSource("");
      return refresh();
    },
  });
  const noValue = question.class === "work_authorization";
  const hint = setClass.error ? explainReason(setClass.error) : null;

  return (
    <li className="grid gap-3 rounded-lg border p-4">
      <div className="grid gap-1">
        <span className="font-medium whitespace-pre-wrap select-text">{question.text}</span>
        <span className="flex flex-wrap items-center gap-2 text-xs">
          <span className="rounded border px-1.5 py-0.5">{LABELS[question.class]}</span>
          {question.country && <span>{question.country}</span>}
        </span>
        {question.findings.length > 0 && (
          <ul aria-label="Findings" className="text-xs text-muted-foreground">
            {question.findings.map((finding, at) => (
              <li key={at}>
                Noticed ({finding.kind}): <span className="select-text">{finding.excerpt}</span>
              </li>
            ))}
          </ul>
        )}
      </div>
      <div className="flex flex-wrap items-end gap-3">
        <div className="grid gap-1.5">
          <Label htmlFor={`${ids}-class`}>Class</Label>
          <select
            id={`${ids}-class`}
            className={selectClass}
            value={question.class}
            disabled={setClass.isPending}
            onChange={(e) => setClass.mutate(e.target.value as MemoryClass)}
          >
            {CLASSES.map((c) => (
              <option key={c} value={c}>
                {LABELS[c]}
              </option>
            ))}
          </select>
        </div>
        <Button variant="outline" size="sm" onClick={() => onRemove({ kind: "question", id: question.id })}>
          Remove question
        </Button>
      </div>
      <ErrorAlert error={setClass.error} />
      {hint && <p className="text-sm text-muted-foreground">{hint}.</p>}
      {answers.length > 0 && (
        <ul aria-label="Answers" className="grid gap-2">
          {answers.map((answer) => (
            <AnswerRow
              key={answer.id}
              answer={answer}
              items={items}
              noValue={noValue}
              onRemove={() => onRemove({ kind: "answer", id: answer.id })}
            />
          ))}
        </ul>
      )}
      {noValue ? (
        <p className="text-sm text-muted-foreground">Answered from your profile, for one country at a time.</p>
      ) : (
        <form
          className="grid gap-2"
          aria-label="Add an answer"
          onSubmit={(e) => {
            e.preventDefault();
            if (text.trim() && !add.isPending) add.mutate();
          }}
        >
          <div className="grid gap-1.5">
            <Label htmlFor={`${ids}-answer`}>Answer</Label>
            <Textarea id={`${ids}-answer`} value={text} onChange={(e) => setText(e.target.value)} />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor={`${ids}-source`}>Source (optional)</Label>
            <select
              id={`${ids}-source`}
              className={selectClass}
              value={source}
              onChange={(e) => setSource(e.target.value)}
            >
              <option value="">None</option>
              {items.map((item) => (
                <option key={item.id} value={item.id}>
                  {itemLabel(item)}
                </option>
              ))}
            </select>
          </div>
          <ErrorAlert error={add.error} />
          <div>
            <Button type="submit" size="sm" variant="outline" disabled={!text.trim() || add.isPending}>
              Add answer
            </Button>
          </div>
        </form>
      )}
    </li>
  );
}

const SEARCH_LIMIT = 10;

/** Hits show the question and, for a normal class, its approved answer. A sealed class
 * never shows a value here: it is confirmed on an application. All of it is inert text. */
function SearchSection() {
  const id = useId();
  const [query, setQuery] = useState("");
  const [asked, setAsked] = useState<string | null>(null);
  const search = useMutation({
    mutationFn: (q: string) => ipc.memorySearch(q, SEARCH_LIMIT),
    onSuccess: (_hits, q) => setAsked(q),
  });
  const client = useQueryClient();
  const plan = useQuery({ queryKey: ["memory", "refresh-plan"], queryFn: ipc.memoryRefreshPlan });
  const refresh = useMutation({
    mutationFn: ipc.memoryRefresh,
    // A raise changes classes, and the plan counts what is left.
    onSettled: () => client.invalidateQueries({ queryKey: ["memory"] }),
  });
  const hits = search.data ?? [];

  return (
    <section aria-label="Search" className="grid gap-2">
      <form
        className="flex flex-wrap items-end gap-2"
        role="search"
        onSubmit={(e) => {
          e.preventDefault();
          if (query.trim() && !search.isPending) search.mutate(query);
        }}
      >
        <div className="grid gap-1.5">
          <Label htmlFor={`${id}-q`}>Search memory</Label>
          <Input id={`${id}-q`} value={query} onChange={(e) => setQuery(e.target.value)} />
        </div>
        <Button type="submit" variant="outline" disabled={!query.trim() || search.isPending}>
          Search
        </Button>
        <Button
          type="button"
          variant="outline"
          disabled={refresh.isPending}
          onClick={() => refresh.mutate()}
        >
          {refresh.isPending ? "Refreshing..." : "Refresh index"}
        </Button>
        {plan.data && (
          <span className="pb-1.5 text-xs text-muted-foreground">
            {plan.data.claude_calls > 0
              ? `Up to ${plan.data.claude_calls} Claude call${plan.data.claude_calls === 1 ? "" : "s"}, limit 20`
              : "No Claude calls"}
          </span>
        )}
      </form>
      <ErrorAlert error={search.error ?? refresh.error ?? plan.error} />
      {refresh.isSuccess && (
        <div role="status" className="grid gap-0.5 text-sm text-muted-foreground">
          <p>
            Checked {refresh.data.classified.checked}, raised {refresh.data.classified.raised}, indexed{" "}
            {refresh.data.embedded}
          </p>
          {refresh.data.classified.skipped && <p>No AI took this; classes come from the rules.</p>}
        </div>
      )}
      {asked !== null && hits.length === 0 && (
        <p className="text-sm text-muted-foreground">Nothing matches.</p>
      )}
      {hits.length > 0 && (
        <ul aria-label="Search results" className="grid gap-2">
          {hits.map((hit) => (
            <li key={hit.question_id} className="grid gap-1 rounded border p-2 text-sm">
              <span className="font-medium whitespace-pre-wrap select-text">{hit.question}</span>
              <span className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
                <span className="rounded border px-1.5 py-0.5">{LABELS[hit.class]}</span>
                <span>{hit.via === "semantic" ? "by meaning" : "by text"}</span>
                {hit.stale && hit.answer !== null && <span>Stale</span>}
              </span>
              {hit.answer !== null ? (
                <span className="whitespace-pre-wrap select-text">{hit.answer}</span>
              ) : hit.needs_confirmation ? (
                <span className="text-muted-foreground">Confirm on an application</span>
              ) : (
                <span className="text-muted-foreground">No approved answer</span>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

export function MemoryView() {
  const refresh = useRefresh();
  const [filter, setFilter] = useState<MemoryClass | "">("");
  const [text, setText] = useState("");
  const [country, setCountry] = useState("");
  const [removing, setRemoving] = useState<Removing | null>(null);

  const rows = useQuery({
    queryKey: ["memory", filter],
    queryFn: () => ipc.memoryList(filter || null),
  });
  const items = useQuery({ queryKey: ["knowledge"], queryFn: ipc.kbList });
  const add = useMutation({
    mutationFn: () => ipc.memoryAddQuestion(text, country.trim() || null),
    onSuccess: () => {
      setText("");
      setCountry("");
      return refresh();
    },
  });
  const remove = useMutation({
    mutationFn: ({ kind, id }: Removing) =>
      kind === "question" ? ipc.memoryRemoveQuestion(id) : ipc.memoryRemoveAnswer(id),
    onSuccess: () => {
      setRemoving(null);
      return refresh();
    },
  });
  const list = rows.data ?? [];

  return (
    <div className="grid max-w-3xl gap-4">
      <PageHeading>Career memory</PageHeading>
      <ErrorAlert error={rows.error ?? items.error} />
      <SearchSection />
      <div className="flex items-center gap-3">
        <Label htmlFor="memory-filter">Filter by class</Label>
        <select
          id="memory-filter"
          className={selectClass}
          value={filter}
          onChange={(e) => setFilter(e.target.value as MemoryClass | "")}
        >
          <option value="">All classes</option>
          {CLASSES.map((c) => (
            <option key={c} value={c}>
              {LABELS[c]}
            </option>
          ))}
        </select>
      </div>
      <form
        className="grid gap-2"
        aria-label="Add a question"
        onSubmit={(e) => {
          e.preventDefault();
          if (text.trim() && !add.isPending) add.mutate();
        }}
      >
        <div className="grid gap-1.5">
          <Label htmlFor="memory-question">Question</Label>
          <Textarea id="memory-question" rows={2} value={text} onChange={(e) => setText(e.target.value)} />
        </div>
        <div className="grid gap-1.5">
          <Label htmlFor="memory-country">Country (optional, two letters)</Label>
          <Input
            id="memory-country"
            className="max-w-24"
            value={country}
            onChange={(e) => setCountry(e.target.value)}
          />
        </div>
        <ErrorAlert error={add.error} />
        <div>
          <Button type="submit" disabled={!text.trim() || add.isPending}>
            Add question
          </Button>
        </div>
      </form>
      {rows.isSuccess && list.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          {filter ? "No questions in this class." : "No questions yet. Add one you are often asked."}
        </p>
      ) : (
        <ul aria-label="Questions" className="grid gap-3">
          {list.map((row) => (
            <QuestionCard key={row.question.id} row={row} items={items.data ?? []} onRemove={setRemoving} />
          ))}
        </ul>
      )}
      {removing && (
        <ConfirmDialog
          title={removing.kind === "question" ? "Remove this question?" : "Remove this answer?"}
          confirmLabel="Remove"
          busyLabel="Removing..."
          pending={remove.isPending}
          error={remove.error}
          onCancel={() => setRemoving(null)}
          onConfirm={() => remove.mutate(removing)}
        >
          <p>
            {removing.kind === "question" ? "Its answers go with it. " : ""}
            This cannot be undone. Backups you already made still hold it.
          </p>
        </ConfirmDialog>
      )}
    </div>
  );
}
