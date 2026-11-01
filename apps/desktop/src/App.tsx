import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { ErrorAlert } from "@/components/ErrorAlert";
import { TerminalPanel } from "@/components/TerminalPanel";
import { Button } from "@/components/ui/button";
import { useUiStore, type View } from "@/lib/store";
import { useStatus } from "@/lib/status";
import { cn } from "@/lib/utils";
import { DataView } from "@/views/DataView";
import { DocumentsView } from "@/views/DocumentsView";
import { RecoveryView } from "@/views/RecoveryView";
import { StatusView } from "@/views/StatusView";

const NAV: { view: View; label: string }[] = [
  { view: "status", label: "Status" },
  { view: "documents", label: "Documents" },
  { view: "data", label: "Data" },
];

function Shell({ version, root }: { version: string; root: string }) {
  const { view, setView } = useUiStore();
  const nav = useRef<HTMLElement>(null);
  const main = useRef<HTMLElement>(null);
  const [terminal, setTerminal] = useState(false);

  // On a view change, focus moves to the main view heading.
  useEffect(() => main.current?.querySelector("h1")?.focus(), [view]);

  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      const index = Number(e.key) - 1;
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
    <div className="flex min-h-screen flex-col">
      <header className="flex items-center justify-between border-b px-4 py-2">
        <span className="font-semibold">JobPilot AI</span>
        <span className="text-sm text-muted-foreground">v{version}</span>
      </header>
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
        </main>
      </div>
      <TerminalPanel open={terminal} onClose={() => setTerminal(false)} />
      <footer className="flex items-center justify-between border-t px-4 py-2 text-sm text-muted-foreground">
        <span className="flex items-center gap-3">
          Local only. No account. No network.
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
  if (status.isPending) {
    return <p className="p-6 text-sm text-muted-foreground">Opening your data...</p>;
  }
  if (status.isError) {
    return (
      <main className="grid max-w-xl gap-3 p-6">
        <ErrorAlert error={status.error} />
        <div>
          <Button onClick={() => void status.refetch()}>Check again</Button>
        </div>
      </main>
    );
  }
  // `Ok` is not enough: "recovery" replaces the shell and hides the navigation.
  if (status.data.state === "recovery") return <RecoveryView status={status.data} />;
  return <Shell version={status.data.app_version} root={status.data.data_root} />;
}
