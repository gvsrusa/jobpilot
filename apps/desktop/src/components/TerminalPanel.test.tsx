import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { emit, listen } from "@tauri-apps/api/event";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ptyWrite } from "@/lib/ipc";
import { useUiStore } from "@/lib/store";
import { TERMINAL_OPTIONS, TerminalPanel } from "./TerminalPanel";

type Call = { cmd: string; args: Record<string, unknown> };

function backend(overrides: Record<string, () => unknown> = {}) {
  const calls: Call[] = [];
  mockWindows("main");
  mockIPC(
    (cmd, args) => {
      calls.push({ cmd, args: args as Call["args"] });
      return overrides[cmd]?.();
    },
    { shouldMockEvents: true },
  );
  const of = (cmd: string) => calls.filter((call) => call.cmd === cmd);
  return { calls, of };
}

const terminalInput = () => document.querySelector<HTMLTextAreaElement>(".xterm-helper-textarea")!;

function panel(onClose = vi.fn()) {
  return {
    onClose,
    user: userEvent.setup(),
    ...render(
      <>
        <main>
          <h1 tabIndex={-1}>View</h1>
        </main>
        <TerminalPanel open onClose={onClose} />
      </>,
    ),
  };
}

describe("TerminalPanel", () => {
  beforeEach(() => useUiStore.setState({ applyRun: null }));

  it("renders_header_footer_and_empty_hint", async () => {
    const { of } = backend();
    panel();
    expect(screen.getByText("Terminal")).toBeInTheDocument();
    expect(screen.getByText("jobpilot CLI only")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Clear" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Close panel" })).toBeInTheDocument();
    expect(screen.getByText("Runs the same services as the app. Output is not saved.")).toBeInTheDocument();
    expect(screen.getByText("Type `jobpilot --help`.")).toBeInTheDocument();
    await waitFor(() => expect(of("pty_open")).toHaveLength(1));
  });

  it("opens_with_a_size_and_nothing_else", async () => {
    const { of } = backend();
    panel();
    await waitFor(() => expect(of("pty_open")).toHaveLength(1));
    expect(Object.keys(of("pty_open")[0].args).sort()).toEqual(["cols", "rows"]);
  });

  it("stays_closed_until_first_opened", async () => {
    const { of } = backend();
    render(<TerminalPanel open={false} onClose={vi.fn()} />);
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(of("pty_open")).toHaveLength(0);
    expect(screen.queryByText("Terminal")).not.toBeVisible();
  });

  it("f6_moves_focus_out_of_the_terminal", async () => {
    const { of } = backend();
    panel();
    await waitFor(() => expect(of("pty_open")).toHaveLength(1));
    terminalInput().focus();
    expect(terminalInput()).toHaveFocus();
    // A real keyCode, or xterm.js would have nothing to send even if it saw the key.
    fireEvent.keyDown(terminalInput(), { key: "F6", code: "F6", keyCode: 117 });
    expect(screen.getByRole("button", { name: "Clear" })).toHaveFocus();
    expect(of("pty_write")).toHaveLength(0);
  });

  it("keys_go_to_the_pty_as_bytes", async () => {
    const { of } = backend();
    panel();
    await waitFor(() => expect(of("pty_open")).toHaveLength(1));
    terminalInput().focus();
    // As the webview fires it: user-event gives no real keyCode for a held Control.
    fireEvent.keyDown(terminalInput(), { key: "c", code: "KeyC", keyCode: 67, ctrlKey: true });
    await waitFor(() => expect(of("pty_write")).not.toHaveLength(0));
    expect(of("pty_write")[0].args).toEqual({ data: [3] });
  });

  it("ptyWrite_sends_an_array", async () => {
    const { of } = backend();
    await ptyWrite(new TextEncoder().encode("hé"));
    expect(of("pty_write")[0].args).toEqual({ data: [104, 195, 169] });
  });

  it("output_reaches_the_terminal_and_clear_empties_it", async () => {
    const { of } = backend();
    const { user } = panel();
    await waitFor(() => expect(of("pty_open")).toHaveLength(1));
    await emit("pty:data", [...new TextEncoder().encode("hello from the shell\r\n")]);
    await waitFor(() =>
      expect(document.querySelector(".xterm-rows")?.textContent).toContain("hello from the shell"),
    );
    await user.click(screen.getByRole("button", { name: "Clear" }));
    await waitFor(() =>
      expect(document.querySelector(".xterm-rows")?.textContent).not.toContain("hello"),
    );
  });

  it("acks_output_only_once_the_terminal_has_taken_it", async () => {
    const { of } = backend();
    const acks: unknown[] = [];
    panel();
    await waitFor(() => expect(of("pty_open")).toHaveLength(1));
    await listen("pty:ack", (event) => void acks.push(event.payload));
    await emit("pty:data", [...new TextEncoder().encode("twelve bytes")]);
    await waitFor(() => expect(acks).toEqual([12]));
  });

  it("replies_from_the_terminal_are_not_typing", async () => {
    const { of } = backend();
    panel();
    await waitFor(() => expect(of("pty_open")).toHaveLength(1));
    // A cursor position query: ConPTY sends this on start, and xterm.js answers it itself.
    await emit("pty:data", [...new TextEncoder().encode("\x1b[6n")]);
    await waitFor(() => expect(of("pty_write")).toHaveLength(1));
    expect(screen.getByText("Type `jobpilot --help`.")).toBeInTheDocument();
  });

  it("the_empty_hint_goes_with_the_first_key", async () => {
    const { of } = backend();
    const { user } = panel();
    await waitFor(() => expect(of("pty_open")).toHaveLength(1));
    terminalInput().focus();
    await user.keyboard("a");
    await waitFor(() =>
      expect(screen.queryByText("Type `jobpilot --help`.")).not.toBeInTheDocument(),
    );
  });

  it("links_in_output_open_nothing", () => {
    const open = vi.spyOn(window, "open");
    TERMINAL_OPTIONS.linkHandler.activate();
    expect(open).not.toHaveBeenCalled();
    open.mockRestore();
  });

  it("failed_open_shows_code_and_restart", async () => {
    let fail = true;
    const { of } = backend({
      pty_open: () => {
        if (fail) throw { code: "unavailable", message: "not available on this system" };
      },
    });
    const { user } = panel();
    expect(await screen.findByRole("alert")).toHaveTextContent("code: unavailable");
    fail = false;
    await user.click(screen.getByRole("button", { name: "Restart" }));
    await waitFor(() => expect(of("pty_open")).toHaveLength(2));
    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
  });

  it("refused_paste_keeps_the_session", async () => {
    const { of } = backend({
      pty_write: () => {
        throw { code: "too_large", message: "too large: the limit is 65536 bytes" };
      },
    });
    const { user } = panel();
    await waitFor(() => expect(of("pty_open")).toHaveLength(1));
    terminalInput().focus();
    await user.keyboard("a");
    expect(await screen.findByRole("alert")).toHaveTextContent("code: too_large");
    expect(screen.queryByRole("button", { name: "Restart" })).not.toBeInTheDocument();
  });

  it("exit_offers_restart", async () => {
    const { of } = backend();
    const { user } = panel();
    await waitFor(() => expect(of("pty_open")).toHaveLength(1));
    await emit("pty:exit", 0);
    await user.click(await screen.findByRole("button", { name: "Restart" }));
    await waitFor(() => expect(of("pty_open")).toHaveLength(2));
  });

  it("close_calls_pty_close", async () => {
    const { of } = backend();
    const { user, onClose } = panel();
    await waitFor(() => expect(of("pty_open")).toHaveLength(1));
    const before = of("pty_close").length;
    await user.click(screen.getByRole("button", { name: "Close panel" }));
    await waitFor(() => expect(of("pty_close").length).toBeGreaterThan(before));
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("unmount_ends_the_session", async () => {
    const { of } = backend();
    const { unmount } = panel();
    await waitFor(() => expect(of("pty_open")).toHaveLength(1));
    const before = of("pty_close").length;
    unmount();
    await waitFor(() => expect(of("pty_close").length).toBeGreaterThan(before));
  });

  it("the panel opens the apply session only with a run id", async () => {
    const { of } = backend();
    useUiStore.setState({ applyRun: "run1" });
    panel();
    await waitFor(() => expect(of("pty_open_apply")).toHaveLength(1));
    expect(Object.keys(of("pty_open_apply")[0].args).sort()).toEqual(["applyId", "cols", "rows"]);
    expect(of("pty_open_apply")[0].args.applyId).toBe("run1");
    expect(of("pty_open")).toHaveLength(0);
    expect(screen.getByText("Claude Code apply run")).toBeInTheDocument();
    expect(screen.queryByText("jobpilot CLI only")).not.toBeInTheDocument();
  });

  it("declining the end-session dialog cancels the ready run", async () => {
    const { of } = backend();
    const { user } = panel();
    await waitFor(() => expect(of("pty_open")).toHaveLength(1));

    act(() => useUiStore.getState().setApplyRun("run1"));

    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("This ends the terminal session.");
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(of("apply_cancel")).toHaveLength(1));
    expect(of("apply_cancel")[0].args).toEqual({ id: "run1" });
    expect(useUiStore.getState().applyRun).toBeNull();
    expect(of("pty_open_apply")).toHaveLength(0);
    expect(screen.getByText("jobpilot CLI only")).toBeInTheDocument();
  });

  it("confirming the end-session dialog opens the apply session", async () => {
    const { of } = backend();
    const { user } = panel();
    await waitFor(() => expect(of("pty_open")).toHaveLength(1));

    act(() => useUiStore.getState().setApplyRun("run1"));
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: "End session" }));

    await waitFor(() => expect(of("pty_open_apply")).toHaveLength(1));
    expect(of("apply_cancel")).toHaveLength(0);
    expect(screen.getByText("Claude Code apply run")).toBeInTheDocument();
  });

  it("a failed apply open cancels the ready run", async () => {
    const { of } = backend({
      pty_open_apply: () => {
        throw { code: "refused", message: "refused: apply_run_not_ready" };
      },
    });
    useUiStore.setState({ applyRun: "run1" });
    panel();
    expect(await screen.findByRole("alert")).toHaveTextContent("code: refused");
    await waitFor(() => expect(of("apply_cancel")).toHaveLength(1));
    expect(useUiStore.getState().applyRun).toBeNull();
  });

  it("a hosted run is not opened again when the panel reopens", async () => {
    const { of } = backend();
    useUiStore.setState({ applyRun: "run1" });
    const user = userEvent.setup();
    const onClose = vi.fn();
    const ui = (open: boolean) => <TerminalPanel open={open} onClose={onClose} />;
    const { rerender } = render(ui(true));
    await waitFor(() => expect(of("pty_open_apply")).toHaveLength(1));

    // Done on the run screen, then the panel closed and opened again.
    act(() => useUiStore.getState().setApplyRun(null));
    await user.click(screen.getByRole("button", { name: "Close panel" }));
    rerender(ui(false));
    rerender(ui(true));

    await waitFor(() => expect(of("pty_open")).toHaveLength(1));
    expect(of("pty_open_apply")).toHaveLength(1);
    expect(of("apply_cancel")).toHaveLength(0);
    expect(screen.getByText("jobpilot CLI only")).toBeInTheDocument();
  });

  it("reopening the panel during a run neither re-hosts nor cancels it", async () => {
    const { of } = backend();
    useUiStore.setState({ applyRun: "run1" });
    const user = userEvent.setup();
    const ui = (open: boolean) => <TerminalPanel open={open} onClose={vi.fn()} />;
    const { rerender } = render(ui(true));
    await waitFor(() => expect(of("pty_open_apply")).toHaveLength(1));

    await user.click(screen.getByRole("button", { name: "Close panel" }));
    rerender(ui(false));
    rerender(ui(true));

    await waitFor(() => expect(of("pty_open")).toHaveLength(1));
    expect(of("pty_open_apply")).toHaveLength(1);
    expect(of("apply_cancel")).toHaveLength(0);
    expect(useUiStore.getState().applyRun).toBe("run1");
  });
});
