import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { ErrorAlert } from "@/components/ErrorAlert";
import { TerminalPanel } from "@/components/TerminalPanel";
import { TitleBar } from "@/components/TitleBar";
import { Button } from "@/components/ui/button";
import { isPauseKey, usePause } from "@/lib/pause";
import { useUiStore, type View } from "@/lib/store";
import { useStatus } from "@/lib/status";
import { cn } from "@/lib/utils";
import { DataView } from "@/views/DataView";
import { AgentsView } from "@/views/AgentsView";
import { AiView } from "@/views/AiView";
import { ApplicationsView } from "@/views/ApplicationsView";
import { DocumentsView } from "@/views/DocumentsView";
import { JobsView } from "@/views/JobsView";
import { MemoryView } from "@/views/MemoryView";
import { ProfileView } from "@/views/ProfileView";
import { ResumesView } from "@/views/ResumesView";
import { RecoveryView } from "@/views/RecoveryView";
import { StatusView } from "@/views/StatusView";

const NAV: { view: View; label: string }[] = [
  { view: "status", label: "Status" },
  { view: "documents", label: "Documents" },
  { view: "data", label: "Data" },
  { view: "profile", label: "Profile" },
  { view: "resumes", label: "Resumes" },
  { view: "ai", label: "AI" },
  { view: "applications", label: "Applications" },
  { view: "memory", label: "Memory" },
  { view: "agents", label: "Agents" },
  { view: "jobs", label: "Jobs" },
];

function Shell({ root }: { root: string }) {
  const { view, setView } = useUiStore();
  const nav = useRef<HTMLElement>(null);
  const main = useRef<HTMLElement>(null);
  const [terminal, setTerminal] = useState(false);
  // A run started from the screen is hosted in the panel, so the panel opens for it.
  const applyRun = useUiStore((s) => s.applyRun);
  const applyHost = useUiStore((s) => s.applyHost);
  useEffect(() => {
    if (applyRun && applyHost) setTerminal(true);
  }, [applyRun, applyHost]);

  // On a view change, focus moves to the main view heading. Not on launch: that would put
  // the first tab stop past Emergency pause.
  const shown = useRef(view);
  useEffect(() => {
    if (shown.current === view) return;
    shown.current = view;
    main.current?.querySelector("h1")?.focus();
  }, [view]);

  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      const index = e.key === "0" ? 9 : Number(e.key) - 1;
      if ((e.ctrlKey || e.metaKey) && NAV[index]) {
        e.preventDefault();
        setView(NAV[index].view);
      } else if (e.key === "F6") {
        e.preventDefault();
        const heading = main.current?.querySelector<HTMLElement>("h1");
        const inNav = nav.current?.contains(document.activeElement);
        const inPanel = document.activeElement?.closest("[data-terminal-panel]");
        const shell = document.querySelector<HTMLElement>("[data-terminal-panel]:not([hidden]) textarea");
        const target = inNav
          ? (shell ?? heading)
          : inPanel
            ? heading
            : nav.current?.querySelector<HTMLElement>("[aria-current]");
        target?.focus();
      }
    };
    // Capture: a focused terminal would otherwise take this as a keystroke.
    const onToggle = (e: globalThis.KeyboardEvent) => {
      if (e.ctrlKey && e.code === "Backquote") {
        e.preventDefault();
        e.stopPropagation();
        setTerminal((open) => !open);
      }
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("keydown", onToggle, true);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("keydown", onToggle, true);
    };
  }, [setView]);

  // Hiding the panel hands the keyboard back; the panel takes it when it opens.
  const opened = useRef(false);
  useEffect(() => {
    if (opened.current && !terminal) main.current?.querySelector("h1")?.focus();
    opened.current = terminal;
  }, [terminal]);

  const onNavKey = (e: KeyboardEvent<HTMLElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const items = [...e.currentTarget.querySelectorAll<HTMLElement>("button")];
    const next = items[items.indexOf(document.activeElement as HTMLElement) + (e.key === "ArrowDown" ? 1 : -1)];
    if (next) {
      e.preventDefault();
      next.focus();
    }
  };

  return (
    <div className="flex flex-1 flex-col">
      <div className="flex flex-1">
        <nav ref={nav} aria-label="Main" onKeyDown={onNavKey} className="flex w-44 flex-col gap-1 border-r p-3">
          {NAV.map((item) => (
            <Button
              key={item.view}
              variant={view === item.view ? "secondary" : "ghost"}
              className={cn("justify-start")}
              aria-current={view === item.view ? "page" : undefined}
              onClick={() => setView(item.view)}
            >
              {item.label}
            </Button>
          ))}
        </nav>
        <main ref={main} className="flex-1 p-6">
          {view === "status" && <StatusView />}
          {view === "documents" && <DocumentsView />}
          {view === "data" && <DataView />}
          {view === "profile" && <ProfileView />}
          {view === "resumes" && <ResumesView />}
          {view === "ai" && <AiView />}
          {view === "applications" && <ApplicationsView />}
          {view === "memory" && <MemoryView />}
          {view === "agents" && <AgentsView />}
          {view === "jobs" && <JobsView />}
        </main>
      </div>
      <TerminalPanel open={terminal} onClose={() => setTerminal(false)} />
      <footer className="flex items-center justify-between border-t px-4 py-2 text-sm text-muted-foreground">
        <span className="flex items-center gap-3">
          Your data stays on this computer. No account.
          <Button size="sm" variant="ghost" onClick={() => setTerminal((open) => !open)}>
            Terminal
          </Button>
        </span>
        <span>
          Data: <span className="select-text">{root}</span>
        </span>
      </footer>
    </div>
  );
}

export default function App() {
  const status = useStatus();
  const pause = usePause();

  // Here, not in `Shell`, so a focused terminal, an open dialog, the recovery screen, and the
  // loading state are all covered. Capture, so nothing in the page takes the key first. It
  // only ever sets: clearing is the title bar's button and a native dialog.
  const set = pause.set.mutate;
  useEffect(() => {
    const onPause = (e: globalThis.KeyboardEvent) => {
      if (!isPauseKey(e)) return;
      e.preventDefault();
      e.stopPropagation();
      set();
    };
    window.addEventListener("keydown", onPause, true);
    return () => window.removeEventListener("keydown", onPause, true);
  }, [set]);

  let body;
  if (status.isPending) {
    body = <p className="p-6 text-sm text-muted-foreground">Opening your data...</p>;
  } else if (status.isError) {
    body = (
      <main className="grid max-w-xl gap-3 p-6">
        <ErrorAlert error={status.error} />
        <div>
          <Button onClick={() => void status.refetch()}>Check again</Button>
        </div>
      </main>
    );
  } else if (status.data.state === "recovery") {
    // `Ok` is not enough: "recovery" replaces the shell and hides the navigation.
    body = <RecoveryView status={status.data} />;
  } else {
    body = <Shell root={status.data.data_root} />;
  }
  return (
    <div className="flex min-h-screen flex-col">
      <TitleBar version={status.data?.app_version} pause={pause} />
      {body}
    </div>
  );
}
