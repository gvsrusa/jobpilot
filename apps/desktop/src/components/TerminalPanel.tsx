import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { ErrorAlert } from "@/components/ErrorAlert";
import { Button } from "@/components/ui/button";
import { onPtyData, onPtyExit, ptyClose, ptyOpen, ptyResize, ptyWrite } from "@/lib/ipc";
import { cn } from "@/lib/utils";

/**
 * The bottom panel. A session starts when the panel first opens and lives until Close panel,
 * Restart, or unmount: hiding the panel keeps it, so Ctrl+` cannot end a Claude session.
 * Scrollback lives in this memory only and is never stored.
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

  useEffect(() => {
    if (!open) return;
    setWanted(true);
    fit.current?.fit();
    terminal.current?.focus();
  }, [open]);

  useEffect(() => {
    if (!wanted || !host.current) return;
    const term = new Terminal({ convertEol: true, scrollback: 2000 });
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
    const opened = turn.current.then(async () => {
      if (stopped) return;
      unlisten.push(
        await onPtyData((bytes) => term.write(bytes)),
        await onPtyExit(() => {
          finished = true;
          setEnded(true);
        }),
      );
      await ptyClose();
      if (!stopped) await ptyOpen(term.cols, term.rows);
    });
    opened.catch((error: unknown) => {
      finished = true;
      if (!stopped) setFailure(error);
    });

    const input = term.onData((data) => {
      if (finished) return;
      setTyped(true);
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
      window.removeEventListener("resize", refit);
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
    setWanted(false);
    onClose();
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
          <span className="text-sm text-muted-foreground">jobpilot CLI only</span>
        </span>
        <span className="flex items-center gap-2">
          {ended && <span role="status" className="text-sm text-muted-foreground">Session ended.</span>}
          {(failure || ended) && (
            <Button size="sm" onClick={() => setRun((n) => n + 1)}>
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
    </section>
  );
}
