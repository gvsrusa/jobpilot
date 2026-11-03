import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ErrorAlert } from "@/components/ErrorAlert";
import { Button } from "@/components/ui/button";
import { ipc, onPtyData, onPtyExit, ptyAck, ptyClose, ptyOpen, ptyOpenApply, ptyResize, ptyWrite } from "@/lib/ipc";
import { useUiStore } from "@/lib/store";
import { cn } from "@/lib/utils";

/** Output is untrusted, so a link in it (OSC 8) must never open anything. */
export const TERMINAL_OPTIONS = {
  convertEol: true,
  scrollback: 2000,
  linkHandler: { activate: () => undefined },
};

/** A run left `ready` with no session would block the next one: end it. */
function dropRun(id: string) {
  void ipc.applyCancel(id).catch(() => undefined);
  if (useUiStore.getState().applyRun === id) useUiStore.getState().setApplyRun(null);
}

/**
 * The bottom panel. A session starts when the panel first opens and lives until Close panel,
 * Restart, or unmount: hiding the panel keeps it, so Ctrl+` cannot end a Claude session.
 * Scrollback lives in this memory only and is never stored.
 *
 * With `applyRun` set, the session is that run's host, opened by `ptyOpenApply` with the run
 * id alone. A shell already open is ended only after the owner says so. Nothing reads the
 * output: the run reports through its own rows.
 */
export function TerminalPanel({ open, onClose }: { open: boolean; onClose(): void }) {
  const host = useRef<HTMLDivElement>(null);
  const clear = useRef<HTMLButtonElement>(null);
  const terminal = useRef<Terminal | null>(null);
  const fit = useRef<FitAddon | null>(null);
  // Sessions take turns, so a close from one always lands before the next open.
  const turn = useRef<Promise<unknown>>(Promise.resolve());
  const [wanted, setWanted] = useState(false);
  const [run, setRun] = useState(0);
  const [failure, setFailure] = useState<unknown>(null);
  const [writeError, setWriteError] = useState<unknown>(null);
  const [ended, setEnded] = useState(false);
  const [typed, setTyped] = useState(false);
  const applyRun = useUiStore((s) => s.applyRun);
  // The run the next open hosts, read once: a reopen after it is a shell.
  const launch = useRef<string | null>(null);
  // The run the current session hosts, for the header.
  const [hosting, setHosting] = useState<string | null>(null);
  // Runs this panel has hosted: never opened again, and never cancelled from here.
  const hosted = useRef(new Set<string>());
  // A run waiting for the owner to end the open session.
  const [asking, setAsking] = useState<string | null>(null);
  // A session is open and has not ended.
  const live = useRef(false);

  useEffect(() => {
    if (!applyRun) {
      launch.current = null;
      return;
    }
    if (hosted.current.has(applyRun)) return;
    if (live.current) {
      setAsking(applyRun);
    } else {
      launch.current = applyRun;
      setWanted(true);
      setRun((n) => n + 1);
    }
  }, [applyRun]);

  useEffect(() => {
    if (!open) return;
    setWanted(true);
    fit.current?.fit();
    terminal.current?.focus();
  }, [open]);

  useEffect(() => {
    if (!wanted || !host.current) return;
    const term = new Terminal(TERMINAL_OPTIONS);
    const addon = new FitAddon();
    term.loadAddon(addon);
    term.open(host.current);
    addon.fit();
    term.focus();
    terminal.current = term;
    fit.current = addon;

    let stopped = false;
    let finished = false;
    const unlisten: (() => void)[] = [];
    // Taken once the open is tried, not here: StrictMode runs this twice in development.
    const target = launch.current;
    setHosting(target);
    const opened = turn.current.then(async () => {
      if (stopped) return;
      unlisten.push(
        await onPtyData((bytes) => term.write(bytes, () => void ptyAck(bytes.length))),
        await onPtyExit(() => {
          finished = true;
          live.current = false;
          setEnded(true);
        }),
      );
      await ptyClose();
      if (stopped) return;
      if (target) {
        if (launch.current === target) launch.current = null;
        try {
          await ptyOpenApply(term.cols, term.rows, target);
        } catch (error) {
          // A run that never had a session would block the next one.
          if (!hosted.current.has(target)) dropRun(target);
          throw error;
        }
        hosted.current.add(target);
      } else {
        await ptyOpen(term.cols, term.rows);
      }
      if (!stopped && !finished) live.current = true;
    });
    opened.catch((error: unknown) => {
      finished = true;
      if (!stopped) setFailure(error);
    });

    // Not `onData`: that also carries the terminal's own replies to the shell.
    const key = term.onKey(() => setTyped(true));
    const input = term.onData((data) => {
      if (finished) return;
      ptyWrite(new TextEncoder().encode(data)).then(() => setWriteError(null), setWriteError);
    });
    const resized = term.onResize(({ cols, rows }) => {
      // A resize that loses to the session ending has nothing left to do.
      if (!finished) void ptyResize(cols, rows).catch(() => undefined);
    });
    const refit = () => {
      if (!host.current?.closest("[hidden]")) addon.fit();
    };
    window.addEventListener("resize", refit);

    return () => {
      stopped = true;
      live.current = false;
      window.removeEventListener("resize", refit);
      key.dispose();
      input.dispose();
      resized.dispose();
      term.dispose();
      terminal.current = null;
      fit.current = null;
      setFailure(null);
      setWriteError(null);
      setEnded(false);
      turn.current = opened
        .catch(() => undefined)
        .then(() => {
          unlisten.forEach((stop) => stop());
          return ptyClose();
        })
        .catch(() => undefined);
    };
  }, [wanted, run]);

  // Capture: xterm.js must not see F6, or it would go to the shell as a keystroke.
  const leaveOnF6 = (e: KeyboardEvent<HTMLElement>) => {
    if (e.key !== "F6") return;
    e.preventDefault();
    e.stopPropagation();
    clear.current?.focus();
  };

  const closePanel = () => {
    launch.current = null;
    setWanted(false);
    onClose();
  };

  // A run's host cannot be opened again once it has started: Restart goes back to the shell.
  const restart = () => {
    launch.current = null;
    setRun((n) => n + 1);
  };

  return (
    <section
      aria-label="Terminal"
      data-terminal-panel
      hidden={!open}
      className={cn("flex-col border-t", open ? "flex" : "hidden")}
    >
      <header className="flex items-center justify-between gap-4 border-b px-4 py-1">
        <span className="flex items-baseline gap-3">
          <span className="font-semibold">Terminal</span>
          <span className="text-sm text-muted-foreground">{hosting ? "Claude Code apply run" : "jobpilot CLI only"}</span>
        </span>
        <span className="flex items-center gap-2">
          {ended && <span role="status" className="text-sm text-muted-foreground">Session ended.</span>}
          {(failure || ended) && (
            <Button size="sm" onClick={restart}>
              Restart
            </Button>
          )}
          <Button ref={clear} size="sm" variant="ghost" onClick={() => terminal.current?.clear()}>
            Clear
          </Button>
          <Button size="sm" variant="ghost" onClick={closePanel}>
            Close panel
          </Button>
        </span>
      </header>
      <ErrorAlert error={failure ?? writeError} />
      {!typed && <p className="px-4 pt-1 text-sm text-muted-foreground">Type `jobpilot --help`.</p>}
      <div
        ref={host}
        role="group"
        aria-label="Terminal. Press F6 to leave."
        onKeyDownCapture={leaveOnF6}
        className="h-64 p-1"
      />
      <footer className="border-t px-4 py-1 text-sm text-muted-foreground">
        Runs the same services as the app. Output is not saved.
      </footer>
      {asking && (
        <ConfirmDialog
          title="Start the apply run here?"
          confirmLabel="End session"
          busyLabel="Ending..."
          confirmVariant="default"
          pending={false}
          onConfirm={() => {
            setAsking(null);
            launch.current = asking;
            setWanted(true);
            setRun((n) => n + 1);
          }}
          onCancel={() => {
            setAsking(null);
            dropRun(asking);
          }}
        >
          <p>This ends the terminal session.</p>
        </ConfirmDialog>
      )}
    </section>
  );
}
