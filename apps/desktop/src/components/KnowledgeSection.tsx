import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { ipc, type KnowledgeItem } from "@/lib/ipc";
import { ConfirmDialog } from "./ConfirmDialog";
import { ErrorAlert } from "./ErrorAlert";

/** Core's `knowledge::KINDS`. */
const KINDS: [string, string][] = [
  ["experience", "Experience"],
  ["education", "Education"],
  ["skill", "Skill"],
  ["project", "Project"],
  ["achievement", "Achievement"],
  ["note", "Note"],
];

type Group = { key: string; label: string; imported: boolean; items: KnowledgeItem[] };

/** Core's `import_resume` writes `draft:<id>#line:<n>`. */
const draftOf = (item: KnowledgeItem) => /^draft:([^#]+)#/.exec(item.source_ref ?? "")?.[1] ?? null;

/** One group per imported resume, then what the user typed. */
function bySource(items: KnowledgeItem[], titleOf: (draftId: string) => string | undefined): Group[] {
  const groups = new Map<string, Group>();
  for (const item of items) {
    const draft = item.source === "user" ? null : draftOf(item);
    const key = item.source === "user" ? "user" : (draft ?? "imported");
    const group = groups.get(key) ?? {
      key,
      label: item.source === "user" ? "Added by you" : `From ${(draft && titleOf(draft)) ?? "an imported resume"}`,
      imported: item.source !== "user",
      items: [],
    };
    groups.set(key, { ...group, items: [...group.items, item] });
  }
  return [...groups.values()].sort((a, b) => Number(b.imported) - Number(a.imported));
}

const stateOf = (item: KnowledgeItem) => {
  if (item.marked_sensitive) return "Sensitive, never sent";
  return item.approved ? "Approved" : "Not approved";
};

/** What Approve all acts on: a marked or locked item could not be cited anyway. */
const waiting = (item: KnowledgeItem) => !item.approved && !item.marked_sensitive && !item.locked;

/** What drafts are written from. Item text comes from a parsed file: render it as text only. */
export function KnowledgeSection() {
  const client = useQueryClient();
  const items = useQuery({ queryKey: ["knowledge"], queryFn: ipc.kbList });
  const resumes = useQuery({ queryKey: ["drafts", "resume"], queryFn: () => ipc.resumeList("resume") });
  const [kind, setKind] = useState(KINDS[0][0]);
  const [body, setBody] = useState("");
  const [removing, setRemoving] = useState<KnowledgeItem | null>(null);

  // Invalidate, so the list stays in place. A compare is reset: its evidence has changed.
  const changed = () =>
    Promise.all([
      client.invalidateQueries({ queryKey: ["knowledge"] }),
      client.resetQueries({ queryKey: ["compare"] }),
    ]);
  const approve = useMutation({
    mutationFn: ({ id, approved }: { id: string; approved: boolean }) => ipc.kbApprove(id, approved),
    onSettled: changed,
  });
  // One at a time: a failure stops here, and what went through stays approved and shows so.
  const approveAll = useMutation({
    mutationFn: async (group: Group) => {
      for (const item of group.items.filter(waiting)) await ipc.kbApprove(item.id, true);
    },
    onSettled: changed,
  });
  const mark = useMutation({
    mutationFn: ({ id, marked }: { id: string; marked: boolean }) => ipc.kbMarkSensitive(id, marked),
    onSettled: changed,
  });
  const add = useMutation({
    mutationFn: () => ipc.kbAdd(kind, body),
    onSuccess: () => {
      setBody("");
      return changed();
    },
  });
  const remove = useMutation({
    mutationFn: (id: string) => ipc.kbRemove(id),
    onSuccess: () => {
      setRemoving(null);
      return changed();
    },
  });

  const busy = approve.isPending || approveAll.isPending || mark.isPending;
  const titleOf = (draftId: string) => resumes.data?.find((d) => d.id === draftId)?.title;
  const groups = bySource(items.data ?? [], titleOf);

  return (
    <section aria-labelledby="knowledge" className="grid gap-3 rounded-lg border p-4">
      <h2 id="knowledge" className="font-medium">
        Knowledge
      </h2>
      <p className="text-sm">
        Drafts are written only from approved items. Approve what is true. Mark anything about
        visas, citizenship, or pay as sensitive: a sensitive item never reaches a model.
      </p>
      <ErrorAlert error={items.error ?? approve.error ?? approveAll.error ?? mark.error ?? add.error} />
      {items.isSuccess && groups.length === 0 && (
        <p className="text-sm text-muted-foreground">Nothing yet. Import a resume, or add an item below.</p>
      )}
      {groups.map((group) => (
        <div key={group.key} className="grid gap-2">
          <div className="flex flex-wrap items-center gap-3">
            <h3 className="text-sm font-medium">{group.label}</h3>
            {group.imported && (
              <Button
                size="sm"
                variant="outline"
                disabled={busy || !group.items.some(waiting)}
                onClick={() => approveAll.mutate(group)}
              >
                Approve all from this resume
              </Button>
            )}
          </div>
          <ul className="grid max-h-80 gap-2 overflow-y-auto text-sm">
            {group.items.map((item) => (
              <li key={item.id} className="grid gap-1 rounded border p-2">
                <span className="whitespace-pre-wrap">{item.locked ? "Locked, see Recovery" : item.body}</span>
                <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
                  <span>{item.kind}</span>
                  <span>{stateOf(item)}</span>
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={busy || item.locked}
                    onClick={() => approve.mutate({ id: item.id, approved: !item.approved })}
                  >
                    {item.approved ? "Unapprove" : "Approve"}
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={busy || item.locked}
                    onClick={() => mark.mutate({ id: item.id, marked: !item.marked_sensitive })}
                  >
                    {item.marked_sensitive ? "Unmark sensitive" : "Mark sensitive"}
                  </Button>
                  <Button size="sm" variant="outline" disabled={busy} onClick={() => setRemoving(item)}>
                    Remove
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        </div>
      ))}
      <form
        className="grid gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          add.mutate();
        }}
      >
        <div className="grid gap-1.5">
          <Label htmlFor="knowledge-kind">Kind of item</Label>
          <select
            id="knowledge-kind"
            className="h-8 w-fit rounded-lg border bg-background px-2 text-sm"
            value={kind}
            onChange={(e) => setKind(e.target.value)}
          >
            {KINDS.map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </select>
        </div>
        <div className="grid gap-1.5">
          <Label htmlFor="knowledge-body">New item</Label>
          <Textarea id="knowledge-body" value={body} onChange={(e) => setBody(e.target.value)} />
          <p className="text-xs text-muted-foreground">What you add here counts as approved.</p>
        </div>
        <Button type="submit" size="sm" variant="outline" className="w-fit" disabled={add.isPending || body.trim() === ""}>
          Add
        </Button>
      </form>
      {removing && (
        <ConfirmDialog
          title="Remove this item?"
          confirmLabel="Remove"
          busyLabel="Removing..."
          pending={remove.isPending}
          error={remove.error}
          onCancel={() => setRemoving(null)}
          onConfirm={() => remove.mutate(removing.id)}
        >
          <p>{removing.locked ? "This item is locked, so its text cannot be shown." : removing.body}</p>
          <p>This cannot be undone. A draft that cites it no longer counts it as evidence.</p>
        </ConfirmDialog>
      )}
    </section>
  );
}
