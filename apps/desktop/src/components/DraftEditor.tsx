import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
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
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { ipc, type Draft, type DraftKind, type Entry, type ResumeDoc, type Section } from "@/lib/ipc";

const lines = (text: string) =>
  text
    .split("\n")
    .map((l) => l.trim())
    .filter(Boolean);

const emptyEntry = (): Entry => ({ title: "", subtitle: "", dates: "", bullets: [] });

type Props = {
  kind: DraftKind;
  /** The lineage to add a version to, or null for a new one. */
  lineageId: string | null;
  start: ResumeDoc | null;
  /** Why the editor opened, such as a generation that needs manual work. */
  note?: string | null;
  onSaved(draft: Draft): void;
  onClose(): void;
};

/**
 * Edits a `ResumeDoc` and saves it as a new version through `draft_save`. Nothing here
 * writes in place: every Save is a version, and the core re-checks lines in a generated
 * lineage, so a line the user adds there is flagged until it cites evidence.
 */
export function DraftEditor({ kind, lineageId, start, note, onSaved, onClose }: Props) {
  const [name, setName] = useState(start?.name ?? "");
  const [contact, setContact] = useState((start?.contact ?? []).join("\n"));
  const [summary, setSummary] = useState((start?.summary ?? []).join("\n"));
  const [sections, setSections] = useState<Section[]>(start?.sections ?? []);
  const [bullets, setBullets] = useState<string[][]>(
    () => (start?.sections ?? []).map((s) => s.entries.map((e) => e.bullets.join("\n"))),
  );
  const save = useMutation({
    mutationFn: () => {
      const doc: ResumeDoc = {
        name: name.trim(),
        contact: lines(contact),
        summary: lines(summary),
        sections: sections.map((s, si) => ({
          ...s,
          entries: s.entries.map((e, ei) => ({ ...e, bullets: lines(bullets[si]?.[ei] ?? "") })),
        })),
      };
      return ipc.draftSave(kind, lineageId, doc);
    },
    onSuccess: onSaved,
  });

  const setEntry = (si: number, ei: number, change: Partial<Entry>) =>
    setSections(
      sections.map((s, i) =>
        i === si ? { ...s, entries: s.entries.map((e, j) => (j === ei ? { ...e, ...change } : e)) } : s,
      ),
    );

  return (
    <Dialog open onOpenChange={(open) => !open && !save.isPending && onClose()}>
      <DialogContent showCloseButton={false} className="max-h-[85vh] max-w-2xl overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{kind === "resume" ? "Resume editor" : "Cover letter editor"}</DialogTitle>
          <DialogDescription>
            Save adds a new version. Earlier versions are never overwritten.
          </DialogDescription>
        </DialogHeader>
        {note && <p className="text-sm">{note}</p>}
        <form
          className="grid gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            save.mutate();
          }}
        >
          <div className="grid gap-1.5">
            <Label htmlFor="draft-name">Name</Label>
            <Input id="draft-name" value={name} onChange={(e) => setName(e.target.value)} />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="draft-contact">Contact (one per line)</Label>
            <Textarea id="draft-contact" value={contact} onChange={(e) => setContact(e.target.value)} />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="draft-summary">{kind === "resume" ? "Summary (one per line)" : "Letter (one paragraph per line)"}</Label>
            <Textarea id="draft-summary" value={summary} onChange={(e) => setSummary(e.target.value)} />
          </div>
          {sections.map((section, si) => (
            <fieldset key={si} className="grid gap-2 rounded-lg border p-3">
              <legend className="px-1 text-sm">Section {si + 1}</legend>
              <div className="flex items-end gap-2">
                <div className="grid flex-1 gap-1.5">
                  <Label htmlFor={`sec-${si}`}>Heading</Label>
                  <Input
                    id={`sec-${si}`}
                    value={section.heading}
                    onChange={(e) =>
                      setSections(sections.map((s, i) => (i === si ? { ...s, heading: e.target.value } : s)))
                    }
                  />
                </div>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    setSections(sections.filter((_, i) => i !== si));
                    setBullets(bullets.filter((_, i) => i !== si));
                  }}
                >
                  Remove section {si + 1}
                </Button>
              </div>
              {section.entries.map((entry, ei) => (
                <div key={ei} className="grid gap-2 rounded-lg border p-2">
                  {(["title", "subtitle", "dates"] as const).map((field) => (
                    <div key={field} className="grid gap-1">
                      <Label htmlFor={`e-${si}-${ei}-${field}`} className="capitalize">
                        {`Entry ${ei + 1} ${field}`}
                      </Label>
                      <Input
                        id={`e-${si}-${ei}-${field}`}
                        value={entry[field]}
                        onChange={(e) => setEntry(si, ei, { [field]: e.target.value })}
                      />
                    </div>
                  ))}
                  <div className="grid gap-1">
                    <Label htmlFor={`e-${si}-${ei}-b`}>{`Entry ${ei + 1} bullets (one per line)`}</Label>
                    <Textarea
                      id={`e-${si}-${ei}-b`}
                      value={bullets[si]?.[ei] ?? ""}
                      onChange={(e) =>
                        setBullets(bullets.map((b, i) => (i === si ? b.map((t, j) => (j === ei ? e.target.value : t)) : b)))
                      }
                    />
                  </div>
                </div>
              ))}
              <div>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    setSections(sections.map((s, i) => (i === si ? { ...s, entries: [...s.entries, emptyEntry()] } : s)));
                    setBullets(bullets.map((b, i) => (i === si ? [...b, ""] : b)));
                  }}
                >
                  Add entry to section {si + 1}
                </Button>
              </div>
            </fieldset>
          ))}
          <div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => {
                setSections([...sections, { heading: "", entries: [] }]);
                setBullets([...bullets, []]);
              }}
            >
              Add section
            </Button>
          </div>
          <ErrorAlert error={save.error} />
          <DialogFooter>
            <Button type="button" variant="outline" disabled={save.isPending} onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" disabled={save.isPending}>
              {save.isPending ? "Saving..." : "Save"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
