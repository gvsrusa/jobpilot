import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { ApplyRunView, PauseState } from "./lib/ipc";
import { isPauseKey } from "./lib/pause";
import { useUiStore } from "./lib/store";
import { backend, count, READY, recovery, renderApp } from "./test-utils";

const VIEWS = ["Status", "Documents", "Data", "Profile", "Resumes", "AI", "Applications", "Memory", "Agents"];
const PAUSED = "Paused. Nothing automated runs.";
const OFF: PauseState = { paused: false, set_at: null, set_by: null };
const ON: PauseState = { paused: true, set_at: "2026-11-01T10:00:00.000Z", set_by: "cli" };
const KEYS = "{Control>}{Shift>}H{/Shift}{/Control}";

async function openView(user: ReturnType<typeof renderApp>["user"], label: string) {
  await screen.findByRole("heading", { name: "Status" });
  await user.click(screen.getByRole("button", { name: label }));
  await screen.findByRole("heading", { level: 1 });
}

describe("isPauseKey", () => {
  const key = (init: KeyboardEventInit) => new KeyboardEvent("keydown", init);
  it("takes_ctrl_or_cmd_with_shift_and_h_in_either_case", () => {
    expect(isPauseKey(key({ key: "H", ctrlKey: true, shiftKey: true }))).toBe(true);
    expect(isPauseKey(key({ key: "h", metaKey: true, shiftKey: true }))).toBe(true);
    expect(isPauseKey(key({ key: "H", shiftKey: true }))).toBe(false);
    expect(isPauseKey(key({ key: "H", ctrlKey: true }))).toBe(false);
    expect(isPauseKey(key({ key: "P", ctrlKey: true, shiftKey: true }))).toBe(false);
  });
});

describe("emergency pause", () => {
  it("a_fresh_launch_leaves_focus_at_the_top_so_tab_reaches_the_pause", async () => {
    backend();
    const { user } = renderApp();
    const button = await screen.findByRole("button", { name: "Emergency pause" });
    await screen.findByRole("heading", { level: 1 });
    await user.tab();
    expect(button).toHaveFocus();
  });

  it.each(VIEWS)("pause_is_the_first_tab_stop_on_%s", async (label) => {
    backend();
    const { user } = renderApp();
    await openView(user, label);
    (document.activeElement as HTMLElement | null)?.blur();
    document.body.focus();
    await user.tab();
    const button = screen.getByRole("button", { name: "Emergency pause" });
    expect(button).toHaveFocus();
    expect(button).toHaveAttribute("aria-keyshortcuts", "Control+Shift+H Meta+Shift+H");
    expect(button).toHaveAttribute("title", expect.stringContaining("Ctrl+Shift+H"));
  });

  it.each(VIEWS)("the_shortcut_pauses_from_%s", async (label) => {
    let state: PauseState = OFF;
    const api = backend({
      pause_status: () => state,
      pause_set: () => (state = ON),
    });
    const { user } = renderApp();
    await openView(user, label);
    await user.keyboard(KEYS);
    expect(await screen.findByText(PAUSED)).toHaveAttribute("role", "status");
    expect(count(api, "pause_set")).toBe(1);
    expect(count(api, "pause_clear")).toBe(0);
  });

  it("the_button_pauses_and_shows_the_state", async () => {
    let state: PauseState = OFF;
    const api = backend({ pause_status: () => state, pause_set: () => (state = ON) });
    const { user } = renderApp();
    await user.click(await screen.findByRole("button", { name: "Emergency pause" }));
    expect(await screen.findByText(PAUSED)).toHaveAttribute("role", "status");
    expect(screen.getByRole("button", { name: "Clear pause" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Emergency pause" })).not.toBeInTheDocument();
    expect(count(api, "pause_set")).toBe(1);
  });

  it("the_shortcut_works_with_focus_in_the_terminal", async () => {
    const api = backend({ pause_status: () => OFF, pause_set: () => ON });
    const { user } = renderApp();
    await screen.findByRole("heading", { name: "Status" });
    await user.keyboard("{Control>}[Backquote]{/Control}");
    await waitFor(() => expect(count(api, "pty_open")).toBe(1));
    const terminal = document.querySelector<HTMLElement>(".xterm-helper-textarea")!;
    expect(terminal).toHaveFocus();
    let reached = 0;
    terminal.addEventListener("keydown", () => reached++);
    terminal.addEventListener("keydown", () => reached++, true);

    fireEvent.keyDown(terminal, { key: "H", code: "KeyH", ctrlKey: true, shiftKey: true });

    await waitFor(() => expect(count(api, "pause_set")).toBe(1));
    expect(reached).toBe(0);
  });

  it("the_shortcut_works_in_recovery", async () => {
    const api = backend({
      status: () => recovery("schema_too_new"),
      pause_status: () => Promise.reject({ code: "unavailable", message: "not available" }),
      pause_set: () => Promise.reject({ code: "unavailable", message: "not available" }),
    });
    const { user } = renderApp();
    await screen.findByText(/newer JobPilot/);
    expect(screen.getByRole("button", { name: "Emergency pause" })).toBeInTheDocument();

    await user.keyboard(KEYS);

    await waitFor(() => expect(count(api, "pause_set")).toBe(1));
    const alert = await screen.findByText("No data is open, so nothing is running.");
    expect(alert.closest("[role=alert]")).toHaveTextContent("unavailable");
  });

  it("the_shortcut_works_while_loading_and_when_status_fails", async () => {
    const api = backend({
      status: () => Promise.reject({ code: "in_use", message: "another process holds the data" }),
      pause_set: () => ON,
      pause_status: () => OFF,
    });
    const { user } = renderApp();
    await screen.findByText("another process holds the data");
    expect(screen.getByRole("button", { name: "Emergency pause" })).toBeInTheDocument();
    await user.keyboard(KEYS);
    await waitFor(() => expect(count(api, "pause_set")).toBe(1));
  });

  it("the_shortcut_works_inside_an_open_dialog", async () => {
    const api = backend({ pause_status: () => OFF, pause_set: () => ON });
    const { user } = renderApp();
    await openView(user, "Data");
    await user.click(screen.getByRole("button", { name: /Delete all data/ }));
    const dialog = await screen.findByRole("dialog");
    within(dialog).getByLabelText(/Type DELETE/).focus();

    await user.keyboard(KEYS);

    await waitFor(() => expect(count(api, "pause_set")).toBe(1));
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });

  it("the_shortcut_never_clears", async () => {
    const api = backend({ pause_status: () => ON, pause_set: () => ON });
    const { user } = renderApp();
    await screen.findByText(PAUSED);
    await user.keyboard(KEYS);
    await user.keyboard(KEYS);
    await waitFor(() => expect(count(api, "pause_set")).toBe(2));
    expect(count(api, "pause_clear")).toBe(0);
    expect(screen.getByText(PAUSED)).toBeInTheDocument();
  });

  it("a_pause_set_elsewhere_shows_within_a_poll", async () => {
    let state: PauseState = OFF;
    backend({ pause_status: () => state });
    renderApp();
    await screen.findByRole("button", { name: "Emergency pause" });
    expect(screen.queryByText(PAUSED)).not.toBeInTheDocument();

    state = ON;

    expect(await screen.findByText(PAUSED, {}, { timeout: 4000 })).toBeInTheDocument();
  }, 8000);

  it("clear_pause_calls_pause_clear_only_from_its_button", async () => {
    let state: PauseState = ON;
    const api = backend({ pause_status: () => state, pause_clear: () => (state = OFF) });
    const { user } = renderApp();
    await screen.findByText(PAUSED);
    await user.keyboard(KEYS);
    await user.click(screen.getByRole("heading", { level: 1 }));
    expect(count(api, "pause_clear")).toBe(0);

    await user.click(screen.getByRole("button", { name: "Clear pause" }));

    await waitFor(() => expect(count(api, "pause_clear")).toBe(1));
    expect(await screen.findByRole("button", { name: "Emergency pause" })).toBeInTheDocument();
    expect(screen.queryByText(PAUSED)).not.toBeInTheDocument();
  });

  it("a_declined_clear_dialog_is_not_an_error", async () => {
    backend({
      pause_status: () => ON,
      pause_clear: () => Promise.reject({ code: "confirmation_required", message: "confirmation required" }),
    });
    const { user } = renderApp();
    await user.click(await screen.findByRole("button", { name: "Clear pause" }));
    await waitFor(() => expect(screen.getByText(PAUSED)).toBeInTheDocument());
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("the_shortcut_is_consumed_so_the_page_never_sees_it", async () => {
    backend({ pause_status: () => OFF, pause_set: () => ON });
    renderApp();
    await screen.findByRole("heading", { name: "Status" });
    const handled = fireEvent.keyDown(document.body, { key: "H", ctrlKey: true, shiftKey: true });
    expect(handled, "preventDefault was called").toBe(false);
    const other = fireEvent.keyDown(document.body, { key: "J", ctrlKey: true, shiftKey: true });
    expect(other).toBe(true);
  });

  it("pausing_and_clearing_refresh_the_run_list", async () => {
    const api = backend();
    const { user } = renderApp();
    await openView(user, "Agents");
    await waitFor(() => expect(count(api, "agents_list")).toBe(1));

    await user.keyboard(KEYS);
    await waitFor(() => expect(count(api, "agents_list")).toBe(2));

    await user.click(await screen.findByRole("button", { name: "Clear pause" }));
    await waitFor(() => expect(count(api, "agents_list")).toBe(3));
  });

  it("the_no_data_alert_goes_away_once_data_opens", async () => {
    let open = false;
    const unavailable = { code: "unavailable", message: "not available" };
    backend({
      status: () => (open ? READY : recovery("schema_too_new")),
      pause_status: () => (open ? OFF : Promise.reject(unavailable)),
      pause_set: () => Promise.reject(unavailable),
    });
    const { user, client } = renderApp();
    await screen.findByText(/newer JobPilot/);
    await user.keyboard(KEYS);
    await screen.findByText("No data is open, so nothing is running.");

    open = true;
    await client.invalidateQueries({ queryKey: ["status"] });

    await waitFor(
      () => expect(screen.queryByText("No data is open, so nothing is running.")).not.toBeInTheDocument(),
      { timeout: 4000 },
    );
    expect(screen.getByRole("button", { name: "Emergency pause" })).toBeInTheDocument();
  }, 8000);

  it("Esc does not cancel a run", async () => {
    const run: ApplyRunView = {
      id: "run1",
      mode: "claude_code",
      state: "running",
      items: [],
      active_pause: {
        id: "p1",
        item_id: "i1",
        kind: "captcha",
        origin: "page",
        excerpt: "Prove you are human",
        created_at: "2026-11-02T10:00:00.000Z",
      },
      created_at: "2026-11-02T10:00:00.000Z",
      ended_at: null,
    };
    const api = backend({ apply_view: () => run });
    const { user } = renderApp();
    await openView(user, "Applications");
    act(() => useUiStore.setState({ applyRun: "run1" }));
    await screen.findByText("Run paused: waiting for you");

    await user.keyboard("{Escape}");
    await user.click(screen.getByRole("button", { name: "Cancel run" }));
    const dialog = await screen.findByRole("dialog");
    await user.keyboard("{Escape}");
    await waitFor(() => expect(dialog).not.toBeInTheDocument());

    expect(count(api, "apply_cancel")).toBe(0);
    expect(screen.getByText("Run paused: waiting for you")).toBeInTheDocument();
  });
});
