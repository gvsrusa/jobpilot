import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ErrorAlert } from "@/components/ErrorAlert";
import { PageHeading } from "@/components/PageHeading";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Textarea } from "@/components/ui/textarea";
import { ipc } from "@/lib/ipc";
import { formatDate } from "@/lib/utils";

const NEW = "new";
const DEBOUNCE_MS = 200;

type Row = { id: string; title: string; updated?: string; snippet?: string };

export function DocumentsView() {
  const client = useQueryClient();
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [draft, setDraft] = useState({ title: "", body: "" });
  const [deleting, setDeleting] = useState(false);
  const searchBox = useRef<HTMLInputElement>(null);
  const titleBox = useRef<HTMLInputElement>(null);
  const newButton = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    const timer = setTimeout(() => setDebounced(query.trim()), DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [query]);

  const docs = useQuery({ queryKey: ["documents"], queryFn: ipc.documentsList });
  const searching = debounced !== "";
  const hits = useQuery({
    queryKey: ["documents", "search", debounced],
    queryFn: () => ipc.documentsSearch(debounced),
    enabled: searching,
  });

  const rows: Row[] = searching
    ? (hits.data ?? []).map((h) => ({ id: h.id, title: h.title, snippet: h.snippet }))
    : (docs.data ?? []).map((d) => ({ id: d.id, title: d.title, updated: d.updated_at }));
  const listed = searching ? hits.isSuccess : docs.isSuccess;

  // Empty state: a new user lands on the one thing to do.
  useEffect(() => {
    if (docs.data?.length === 0 && !searching) newButton.current?.focus();
  }, [docs.data?.length, searching]);

  // `/` focuses Search when no text field has focus.
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      const el = e.target as HTMLElement;
      const typing = el.matches?.("input, textarea, select, [contenteditable]");
      if (e.key === "/" && !typing && !e.ctrlKey && !e.metaKey) {
        e.preventDefault();
        searchBox.current?.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const select = (id: string) => {
    const doc = docs.data?.find((d) => d.id === id);
    if (!doc) return;
    setSelected(id);
    setDraft({ title: doc.title, body: doc.body });
  };
  const startNew = () => {
    setSelected(NEW);
    setDraft({ title: "", body: "" });
    setTimeout(() => titleBox.current?.focus());
  };

  const refresh = () => client.invalidateQueries({ queryKey: ["documents"] });
  const save = useMutation({
    mutationFn: () =>
      selected === NEW
        ? ipc.documentsCreate(draft.title, draft.body)
        : ipc.documentsUpdate(selected!, draft.title, draft.body),
    onSuccess: (doc) => {
      setSelected(doc.id);
      return refresh();
    },
  });
  const remove = useMutation({
    mutationFn: () => ipc.documentsDelete(selected!),
    onSuccess: () => {
      setDeleting(false);
      setSelected(null);
      setDraft({ title: "", body: "" });
      return refresh();
    },
  });

  const canSave = selected !== null && draft.title.trim() !== "" && !save.isPending;
  const submit = () => canSave && save.mutate();

  // Arrow keys move through the list; only one row is a tab stop.
  const onListKey = (e: KeyboardEvent<HTMLElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const items = [...e.currentTarget.querySelectorAll<HTMLButtonElement>("button[data-row]")];
    const at = items.indexOf(document.activeElement as HTMLButtonElement);
    const next = items[at + (e.key === "ArrowDown" ? 1 : -1)];
    if (next) {
      e.preventDefault();
      next.focus();
    }
  };

  const tabStop = rows.find((r) => r.id === selected)?.id ?? rows[0]?.id;

  return (
    <div className="grid gap-4">
      <div className="flex items-center justify-between">
        <PageHeading>Documents</PageHeading>
        <Button ref={newButton} onClick={startNew}>
          New document
        </Button>
      </div>
      <ErrorAlert error={docs.error ?? hits.error ?? save.error} />
      <div className="flex items-center gap-3">
        <Label htmlFor="doc-search">Search</Label>
        <Input
          id="doc-search"
          ref={searchBox}
          className="max-w-sm"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        {searching && hits.isSuccess && (
          <span className="text-sm text-muted-foreground">
            {rows.length} {rows.length === 1 ? "result" : "results"}
          </span>
        )}
      </div>
      <div className="grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)] gap-6">
        <div onKeyDown={onListKey}>
          {listed && rows.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              {searching
                ? `No documents match '${debounced}'.`
                : "No documents yet. Create one to keep text you can search offline."}
            </p>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Title</TableHead>
                  <TableHead className="text-right">{searching ? "" : "Updated"}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {rows.map((r) => (
                  <TableRow key={r.id} data-state={r.id === selected ? "selected" : undefined}>
                    <TableCell className="whitespace-normal">
                      <button
                        type="button"
                        data-row
                        tabIndex={r.id === tabStop ? 0 : -1}
                        aria-current={r.id === selected ? "true" : undefined}
                        className="text-left font-medium underline-offset-4 hover:underline focus-visible:underline"
                        onClick={() => select(r.id)}
                      >
                        {r.title}
                      </button>
                      {/* Snippets are raw body text: React renders them as text only. */}
                      {r.snippet !== undefined && (
                        <p className="text-sm text-muted-foreground">{r.snippet}</p>
                      )}
                    </TableCell>
                    <TableCell className="text-right text-muted-foreground">
                      {r.updated && formatDate(r.updated)}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </div>
        {selected === null ? (
          <p className="text-sm text-muted-foreground">
            Select a document, or create a new one.
          </p>
        ) : (
          <form
            className="grid content-start gap-3"
            onSubmit={(e) => {
              e.preventDefault();
              submit();
            }}
            onKeyDown={(e) => {
              if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
                e.preventDefault();
                submit();
              }
            }}
          >
            <div className="grid gap-1.5">
              <Label htmlFor="doc-title">Title</Label>
              <Input
                id="doc-title"
                ref={titleBox}
                value={draft.title}
                onChange={(e) => setDraft({ ...draft, title: e.target.value })}
              />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="doc-body">Body</Label>
              <Textarea
                id="doc-body"
                rows={12}
                value={draft.body}
                onChange={(e) => setDraft({ ...draft, body: e.target.value })}
              />
            </div>
            <p className="text-sm text-muted-foreground">
              Do not store visa status, citizenship, or salary here. This text is not encrypted.
            </p>
            <div className="flex gap-2">
              <Button type="submit" disabled={!canSave}>
                {save.isPending ? "Saving..." : "Save"}
              </Button>
              <Button
                type="button"
                variant="outline"
                disabled={selected === NEW}
                onClick={() => setDeleting(true)}
              >
                Delete...
              </Button>
            </div>
          </form>
        )}
      </div>
      {deleting && (
        <ConfirmDialog
          title="Delete this document?"
          confirmLabel="Delete document"
          busyLabel="Deleting..."
          pending={remove.isPending}
          error={remove.error}
          onCancel={() => setDeleting(false)}
          onConfirm={() => remove.mutate()}
        >
          <p>
            "{draft.title}" will be removed from JobPilot. Backups you already made still hold it.
          </p>
        </ConfirmDialog>
      )}
    </div>
  );
}
