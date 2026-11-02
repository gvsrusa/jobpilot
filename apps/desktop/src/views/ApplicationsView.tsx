import { useState, type KeyboardEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ErrorAlert } from "@/components/ErrorAlert";
import { PageHeading } from "@/components/PageHeading";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Textarea } from "@/components/ui/textarea";
import { useUiStore } from "@/lib/store";
import {
  ipc,
  STATUSES,
  toAppError,
  type Application,
  type ApplicationEvent,
  type ApplicationInput,
  type ApplicationStatus,
  type Answer,
  type Question,
} from "@/lib/ipc";
import { formatDate, formatDay } from "@/lib/utils";

const NEW = "new";
const EMPTY: ApplicationInput = {
  company: "",
  role: "",
  job_text_id: null,
  resume_draft_id: null,
  letter_draft_id: null,
  link: "",
  country: "",
  notes: "",
};

const CANCELLED = "confirmation_required";
/** The user closed the confirmation dialog: their answer, not an error. */
class Declined extends Error {}
const STATED = { yes: "Yes", no: "No", not_stated: "Not stated by you" } as const;

const label = (status: string) => status.charAt(0).toUpperCase() + status.slice(1);

const selectClass =
  "h-8 rounded-lg border border-input bg-transparent px-2.5 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50";

const inputOf = (app: Application): ApplicationInput => ({
  company: app.company,
  role: app.role,
  job_text_id: app.job_text_id,
  resume_draft_id: app.resume_draft_id,
  letter_draft_id: app.letter_draft_id,
  link: app.link,
  country: app.country,
  notes: app.notes,
});

function describe(event: ApplicationEvent): string {
  switch (event.kind) {
    case "created":
      return "Added";
    case "status":
      return `${label(event.from_status ?? "")} to ${label(event.to_status ?? "")}`;
    case "note":
      return "Note";
    case "edited":
      return "Details edited";
  }
}

type Choice = { value: string; text: string; question: Question; answer: Answer | null };

/**
 * Taking an answer's text for one application. A sensitive or legal answer is confirmed in a
 * native dialog first, then used, and the text lands in a read-only field and on the
 * clipboard. A work authorization question has no stored answer: it shows what the profile
 * says for one country, and there is no way to type or suggest a value here.
 */
function ApplicationAnswers({ application }: { application: Application }) {
  const client = useQueryClient();
  const [choice, setChoice] = useState("");
  // The question's own country, never the application's: a Canadian question must not read
  // the US row. With none, the user is asked which country.
  const [country, setCountry] = useState("");
  const [text, setText] = useState<string | null>(null);
  const [stale, setStale] = useState(false);
  const [copy, setCopy] = useState<"copied" | "failed" | null>(null);

  const bank = useQuery({ queryKey: ["memory", "for-use"], queryFn: () => ipc.memoryList(null) });
  const key = ["applications", "answers", application.id];
  const applied = useQuery({ queryKey: key, queryFn: () => ipc.memoryApplicationAnswers(application.id) });

  const items = (bank.data ?? []).flatMap(({ question, answers }): Choice[] => {
    if (question.class === "work_authorization") {
      return [{ value: `q:${question.id}`, text: `${question.text} (work authorization)`, question, answer: null }];
    }
    const ready = answers.filter((a) => a.approved);
    return ready.map((answer, at) => ({
      value: `a:${answer.id}`,
      text: `${question.text} (${question.class}${ready.length > 1 ? `, answer ${at + 1}` : ""})`,
      question,
      answer,
    }));
  });
  const item = items.find((i) => i.value === choice);
  const current = item?.answer ? applied.data?.find((a) => a.answer_id === item.answer!.id)?.current : false;
  const needsConfirm = item?.question.class !== "normal" && !current;

  const use = useMutation({
    mutationFn: async (answerId: string) => {
      if (needsConfirm) {
        try {
          await ipc.memoryConfirmUse(answerId, application.id);
        } catch (error) {
          throw toAppError(error).code === CANCELLED ? new Declined() : error;
        }
      }
      return ipc.memoryUse(answerId, application.id);
    },
    onMutate: () => {
      setText(null);
      setCopy(null);
    },
    onSuccess: async (used) => {
      setText(used.text);
      setStale(used.stale);
      try {
        await navigator.clipboard.writeText(used.text);
        setCopy("copied");
      } catch {
        setCopy("failed");
      }
    },
    onSettled: () => client.invalidateQueries({ queryKey: key }),
  });
  const stated = useMutation({ mutationFn: () => ipc.memoryAuthorization(country) });

  const pick = (value: string) => {
    setChoice(value);
    setCountry(items.find((i) => i.value === value)?.question.country ?? "");
    setText(null);
    setCopy(null);
    use.reset();
    stated.reset();
  };
  const useError = use.error instanceof Declined ? null : use.error;

  return (
    <section className="grid gap-3" aria-labelledby="app-answers-heading">
      <h2 id="app-answers-heading" className="text-sm font-semibold">
        Answers for this application
      </h2>
      <ErrorAlert error={bank.error ?? applied.error} />
      <div className="grid gap-1.5">
        <Label htmlFor="app-answer-pick">Answer to use</Label>
        <select
          id="app-answer-pick"
          className={selectClass}
          value={choice}
          disabled={use.isPending}
          onChange={(e) => pick(e.target.value)}
        >
          <option value="">Choose an answer</option>
          {items.map((i) => (
            <option key={i.value} value={i.value}>
              {i.text}
            </option>
          ))}
        </select>
        {bank.isSuccess && items.length === 0 && (
          <p className="text-sm text-muted-foreground">
            No approved answers yet. Approve one on the Memory screen.
          </p>
        )}
      </div>
      {item?.answer && (
        <div className="grid gap-2">
          <div>
            <Button type="button" disabled={use.isPending} onClick={() => use.mutate(item.answer!.id)}>
              {use.isPending ? "Working..." : needsConfirm ? "Confirm and copy" : "Copy"}
            </Button>
          </div>
          <ErrorAlert error={useError} />
          {text !== null && (
            <div className="grid gap-1.5">
              <Label htmlFor="app-answer-text">Answer text</Label>
              <Textarea id="app-answer-text" readOnly rows={4} className="select-text" value={text} />
              {stale && (
                <p className="text-sm">This answer has not been reviewed for over 180 days.</p>
              )}
              <p className="text-sm text-muted-foreground" role="status">
                {copy === "copied" && "Copied to the clipboard."}
                {copy === "failed" && "Could not copy. The text is above to select and copy."}
              </p>
            </div>
          )}
        </div>
      )}
      {item && !item.answer && (
        <div className="grid gap-2">
          <p className="text-sm text-muted-foreground">
            Work authorization is read from your profile, one country at a time. Nothing is guessed
            from another country.
          </p>
          <div className="grid gap-1.5">
            <Label htmlFor="app-authz-country">Authorization country (two letters)</Label>
            <Input
              id="app-authz-country"
              className="max-w-24"
              value={country}
              onChange={(e) => {
                setCountry(e.target.value);
                stated.reset();
              }}
            />
          </div>
          <div className="flex gap-2">
            <Button
              type="button"
              disabled={stated.isPending || country.trim() === ""}
              onClick={() => stated.mutate()}
            >
              Show what I stated
            </Button>
            <Button type="button" variant="outline" onClick={() => useUiStore.getState().setView("profile")}>
              Open Profile
            </Button>
          </div>
          <ErrorAlert error={stated.error} />
          {stated.data &&
            (stated.data.stated ? (
              <ul aria-label="Stated values" className="text-sm">
                <li>
                  {stated.data.country} - Authorized: {STATED[stated.data.authorized]}
                </li>
                <li>Needs sponsorship: {STATED[stated.data.needs_sponsorship]}</li>
                {stated.data.note && <li className="select-text">{stated.data.note}</li>}
              </ul>
            ) : (
              <p className="text-sm">
                {stated.data.country}: {STATED.not_stated}
              </p>
            ))}
        </div>
      )}
    </section>
  );
}

export function ApplicationsView() {
  const client = useQueryClient();
  const [filter, setFilter] = useState<ApplicationStatus | "">("");
  const [selected, setSelected] = useState<string | null>(null);
  const [form, setForm] = useState<ApplicationInput>(EMPTY);
  const [removing, setRemoving] = useState(false);
  const [next, setNext] = useState<ApplicationStatus>("applied");
  const [on, setOn] = useState("");
  const [statusNote, setStatusNote] = useState("");
  const [note, setNote] = useState("");

  const apps = useQuery({
    queryKey: ["applications", filter],
    queryFn: () => ipc.appList(filter || null),
  });
  const jobs = useQuery({ queryKey: ["jobs"], queryFn: ipc.jobList });
  const drafts = useQuery({ queryKey: ["drafts", "all"], queryFn: () => ipc.resumeList(null) });
  // Not from the filtered list: a status change can take the row out of the filter.
  const detail = useQuery({
    queryKey: ["applications", "get", selected],
    queryFn: () => ipc.appGet(selected!),
    enabled: selected !== null && selected !== NEW,
  });
  const history = useQuery({
    queryKey: ["applications", "history", selected],
    queryFn: () => ipc.appHistory(selected!),
    enabled: selected !== null && selected !== NEW,
  });

  const refresh = () => client.invalidateQueries({ queryKey: ["applications"] });
  const open = (app: Application) => {
    setSelected(app.id);
    setForm(inputOf(app));
    setNext(app.status);
    setOn("");
    setStatusNote("");
    setNote("");
  };
  const startNew = () => {
    setSelected(NEW);
    setForm(EMPTY);
  };

  const save = useMutation({
    mutationFn: () => (selected === NEW ? ipc.appAdd(form) : ipc.appUpdate(selected!, form)),
    onSuccess: (app) => {
      open(app);
      return refresh();
    },
  });
  const move = useMutation({
    mutationFn: () => ipc.appSetStatus(selected!, next, on.trim() || null, statusNote),
    onSuccess: (app) => {
      open(app);
      return refresh();
    },
  });
  const addNote = useMutation({
    mutationFn: () => ipc.appNote(selected!, note),
    onSuccess: () => {
      setNote("");
      return refresh();
    },
  });
  const remove = useMutation({
    mutationFn: () => ipc.appRemove(selected!),
    onSuccess: () => {
      setRemoving(false);
      setSelected(null);
      return refresh();
    },
  });

  const rows = apps.data ?? [];
  const current = selected === NEW ? undefined : detail.data;
  const canSave = form.company.trim() !== "" && form.role.trim() !== "" && !save.isPending;
  const set = <K extends keyof ApplicationInput>(key: K, value: ApplicationInput[K]) =>
    setForm({ ...form, [key]: value });

  const onListKey = (e: KeyboardEvent<HTMLElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const items = [...e.currentTarget.querySelectorAll<HTMLButtonElement>("button[data-row]")];
    const at = items.indexOf(document.activeElement as HTMLButtonElement);
    const target = items[at + (e.key === "ArrowDown" ? 1 : -1)];
    if (target) {
      e.preventDefault();
      target.focus();
    }
  };
  const tabStop = rows.find((a) => a.id === selected)?.id ?? rows[0]?.id;

  const resumes = (drafts.data ?? []).filter((d) => d.kind === "resume");
  const letters = (drafts.data ?? []).filter((d) => d.kind === "cover_letter");
  const choose = (
    id: string,
    text: string,
    value: string | null,
    key: "job_text_id" | "resume_draft_id" | "letter_draft_id",
    options: { id: string; title: string }[],
  ) => (
    <div className="grid gap-1.5">
      <Label htmlFor={id}>{text}</Label>
      <select
        id={id}
        className={selectClass}
        value={value ?? ""}
        onChange={(e) => set(key, e.target.value || null)}
      >
        <option value="">None</option>
        {options.map((o) => (
          <option key={o.id} value={o.id}>
            {o.title}
          </option>
        ))}
      </select>
    </div>
  );

  return (
    <div className="grid gap-4">
      <PageHeading>Applications</PageHeading>
      <ErrorAlert error={apps.error} />
      <div className="flex items-center gap-3">
        <Label htmlFor="app-filter">Filter by status</Label>
        <select
          id="app-filter"
          className={selectClass}
          value={filter}
          onChange={(e) => setFilter(e.target.value as ApplicationStatus | "")}
        >
          <option value="">All statuses</option>
          {STATUSES.map((s) => (
            <option key={s} value={s}>
              {label(s)}
            </option>
          ))}
        </select>
        <Button onClick={startNew}>Add application</Button>
      </div>
      <div className="grid grid-cols-[minmax(0,3fr)_minmax(0,2fr)] gap-6">
        <div onKeyDown={onListKey}>
          {apps.isSuccess && rows.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              {filter ? "No applications with this status." : "No applications yet. Add the first one."}
            </p>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Company</TableHead>
                  <TableHead>Role</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead>Applied</TableHead>
                  <TableHead>Updated</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {rows.map((a) => (
                  <TableRow key={a.id} data-state={a.id === selected ? "selected" : undefined}>
                    <TableCell className="whitespace-normal">
                      <button
                        type="button"
                        data-row
                        tabIndex={a.id === tabStop ? 0 : -1}
                        aria-current={a.id === selected ? "true" : undefined}
                        className="text-left font-medium underline-offset-4 hover:underline focus-visible:underline"
                        onClick={() => open(a)}
                      >
                        {a.company}
                      </button>
                    </TableCell>
                    <TableCell className="whitespace-normal">{a.role}</TableCell>
                    <TableCell>{label(a.status)}</TableCell>
                    <TableCell>{a.applied_on ? formatDay(a.applied_on) : ""}</TableCell>
                    <TableCell className="text-muted-foreground">{formatDate(a.updated_at)}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </div>
        {selected === null ? (
          <p className="text-sm text-muted-foreground">Select an application, or add one.</p>
        ) : (
          <div className="grid content-start gap-5">
            <form
              className="grid gap-3"
              onSubmit={(e) => {
                e.preventDefault();
                if (canSave) save.mutate();
              }}
            >
              <div className="grid gap-1.5">
                <Label htmlFor="app-company">Company</Label>
                <Input id="app-company" value={form.company} onChange={(e) => set("company", e.target.value)} />
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="app-role">Role</Label>
                <Input id="app-role" value={form.role} onChange={(e) => set("role", e.target.value)} />
              </div>
              {choose("app-job", "Job posting", form.job_text_id, "job_text_id", jobs.data ?? [])}
              {choose("app-resume", "Resume draft", form.resume_draft_id, "resume_draft_id", resumes)}
              {choose("app-letter", "Cover letter draft", form.letter_draft_id, "letter_draft_id", letters)}
              <div className="grid gap-1.5">
                {/* Text from outside the app: shown as text to select and copy, never a link. */}
                <Label htmlFor="app-link">Link (untrusted text, not clickable)</Label>
                <Input id="app-link" className="select-text" value={form.link} onChange={(e) => set("link", e.target.value)} />
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="app-country">Country (two letters)</Label>
                <Input id="app-country" className="max-w-24" value={form.country} onChange={(e) => set("country", e.target.value)} />
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="app-notes">Notes</Label>
                <Textarea id="app-notes" rows={4} value={form.notes} onChange={(e) => set("notes", e.target.value)} />
              </div>
              <ErrorAlert error={save.error} />
              <div>
                <Button type="submit" disabled={!canSave}>
                  {save.isPending ? "Saving..." : "Save"}
                </Button>
              </div>
            </form>
            {selected !== NEW && current && (
              <>
                <form
                  className="grid gap-3"
                  aria-label="Change status"
                  onSubmit={(e) => {
                    e.preventDefault();
                    if (!move.isPending) move.mutate();
                  }}
                >
                  <p className="text-sm">
                    Status: <span className="font-medium">{label(current.status)}</span>
                  </p>
                  <div className="grid gap-1.5">
                    <Label htmlFor="app-next">New status</Label>
                    <select
                      id="app-next"
                      className={selectClass}
                      value={next}
                      onChange={(e) => setNext(e.target.value as ApplicationStatus)}
                    >
                      {STATUSES.map((s) => (
                        <option key={s} value={s}>
                          {label(s)}
                        </option>
                      ))}
                    </select>
                  </div>
                  <div className="grid gap-1.5">
                    <Label htmlFor="app-on">Applied on (optional)</Label>
                    <Input id="app-on" placeholder="YYYY-MM-DD" className="max-w-40" value={on} onChange={(e) => setOn(e.target.value)} />
                  </div>
                  <div className="grid gap-1.5">
                    <Label htmlFor="app-status-note">Status note (optional)</Label>
                    <Input id="app-status-note" value={statusNote} onChange={(e) => setStatusNote(e.target.value)} />
                  </div>
                  <ErrorAlert error={move.error} />
                  <div>
                    <Button
                      type="submit"
                      disabled={move.isPending || (next === current.status && !statusNote.trim())}
                    >
                      {move.isPending ? "Saving..." : "Set status"}
                    </Button>
                  </div>
                </form>
                <form
                  className="grid gap-3"
                  aria-label="Add a note"
                  onSubmit={(e) => {
                    e.preventDefault();
                    if (note.trim() && !addNote.isPending) addNote.mutate();
                  }}
                >
                  <div className="grid gap-1.5">
                    <Label htmlFor="app-note">Add note</Label>
                    <Input id="app-note" value={note} onChange={(e) => setNote(e.target.value)} />
                  </div>
                  <ErrorAlert error={addNote.error} />
                  <div>
                    <Button type="submit" variant="outline" disabled={!note.trim() || addNote.isPending}>
                      Add note
                    </Button>
                  </div>
                </form>
                <ApplicationAnswers key={current.id} application={current} />
                <section className="grid gap-2">
                  <h2 className="text-sm font-semibold">History</h2>
                  <ErrorAlert error={history.error} />
                  <ul aria-label="History" className="grid gap-1 text-sm">
                    {(history.data ?? []).map((e) => (
                      <li key={e.id}>
                        <span className="text-muted-foreground">{formatDate(e.at)}</span> {describe(e)}
                        {e.note && <span className="block select-text">{e.note}</span>}
                      </li>
                    ))}
                  </ul>
                </section>
                <div>
                  <Button type="button" variant="outline" onClick={() => setRemoving(true)}>
                    Remove...
                  </Button>
                </div>
              </>
            )}
          </div>
        )}
      </div>
      {removing && (
        <ConfirmDialog
          title="Remove this application?"
          confirmLabel="Remove application"
          busyLabel="Removing..."
          pending={remove.isPending}
          error={remove.error}
          onCancel={() => setRemoving(false)}
          onConfirm={() => remove.mutate()}
        >
          <p>
            "{current?.company}" and its history will be removed from JobPilot. Backups you already
            made still hold it.
          </p>
        </ConfirmDialog>
      )}
    </div>
  );
}
