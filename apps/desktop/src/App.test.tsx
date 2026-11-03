import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { useUiStore } from "./lib/store";
import { backend, count, READY, recovery, renderApp } from "./test-utils";

describe("shell", () => {
  it("shows_shell_with_status", async () => {
    backend();
    renderApp();
    expect(await screen.findByRole("heading", { name: "Status" })).toBeInTheDocument();
    const nav = screen.getByRole("navigation");
    for (const name of ["Status", "Documents", "Data"]) {
      expect(within(nav).getByRole("button", { name })).toBeInTheDocument();
    }
    expect(screen.getByText("Your data stays on this computer. No account.")).toBeInTheDocument();
    expect(screen.getByText("v0.1.0")).toBeInTheDocument();
    expect(screen.getAllByText("/home/me/jobpilot").length).toBeGreaterThan(0);
  });

  it("switches_view_with_navigation_and_shortcut", async () => {
    backend();
    const { user } = renderApp();
    await screen.findByRole("heading", { name: "Status" });
    await user.click(screen.getByRole("button", { name: "Documents" }));
    expect(await screen.findByRole("heading", { name: "Documents" })).toBeInTheDocument();
    await user.keyboard("{Control>}3{/Control}");
    expect(await screen.findByRole("heading", { name: "Data and privacy" })).toBeInTheDocument();
  });
});

describe("terminal panel", () => {
  const panel = () => screen.queryByText("jobpilot CLI only");

  it("setting a run opens the panel", async () => {
    const api = backend();
    renderApp();
    await screen.findByRole("heading", { name: "Status" });
    expect(panel()).not.toBeVisible();

    act(() => useUiStore.getState().setApplyRun("run1"));

    expect(await screen.findByText("Claude Code apply run")).toBeVisible();
    await waitFor(() => expect(count(api, "pty_open_apply")).toBe(1));
    expect(count(api, "pty_open")).toBe(0);
  });

  it("ctrl_backquote_toggles_the_panel_and_keeps_the_session", async () => {
    const api = backend();
    const { user } = renderApp();
    await screen.findByRole("heading", { name: "Status" });
    expect(panel()).not.toBeVisible();
    expect(count(api, "pty_open")).toBe(0);

    await user.keyboard("{Control>}[Backquote]{/Control}");
    expect(panel()).toBeVisible();
    await waitFor(() => expect(count(api, "pty_open")).toBe(1));
    expect(document.activeElement).toHaveClass("xterm-helper-textarea");

    await user.keyboard("{Control>}[Backquote]{/Control}");
    expect(panel()).not.toBeVisible();
    await user.keyboard("{Control>}[Backquote]{/Control}");
    expect(panel()).toBeVisible();
    expect(count(api, "pty_open")).toBe(1);
  });

  it("button_opens_it_and_close_panel_ends_the_session", async () => {
    const api = backend();
    const { user } = renderApp();
    await user.click(await screen.findByRole("button", { name: "Terminal" }));
    await waitFor(() => expect(count(api, "pty_open")).toBe(1));
    await user.click(screen.getByRole("button", { name: "Close panel" }));
    expect(panel()).not.toBeVisible();
    await user.click(screen.getByRole("button", { name: "Terminal" }));
    await waitFor(() => expect(count(api, "pty_open")).toBe(2));
  });

  it("f6_walks_main_nav_terminal_buttons_main", async () => {
    const api = backend();
    const { user } = renderApp();
    const heading = await screen.findByRole("heading", { name: "Status" });
    await user.keyboard("{Control>}[Backquote]{/Control}");
    await waitFor(() => expect(count(api, "pty_open")).toBe(1));
    await user.click(heading);
    heading.focus();
    await user.keyboard("{F6}");
    expect(screen.getByRole("button", { name: "Status" })).toHaveFocus();
    await user.keyboard("{F6}");
    expect(document.activeElement).toHaveClass("xterm-helper-textarea");
    await user.keyboard("{F6}");
    expect(screen.getByRole("button", { name: "Clear" })).toHaveFocus();
    await user.keyboard("{F6}");
    expect(heading).toHaveFocus();
  });
});

describe("recovery", () => {
  it("shows_recovery_screen when status is recovery", async () => {
    backend({ status: () => recovery("schema_too_new", "data schema 9, app 1") });
    renderApp();
    expect(
      await screen.findByRole("heading", { name: "JobPilot AI could not open your data" }),
    ).toBeInTheDocument();
    expect(screen.getByText("/home/me/jobpilot")).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("data schema 9, app 1");
    expect(screen.getByRole("alert")).toHaveTextContent("schema_too_new");
    expect(screen.queryByRole("navigation")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Restore from a backup/ })).toBeEnabled();
  });

  it.each([
    ["db", true],
    ["migration_failed", true],
    ["schema_too_new", true],
    ["restored_not_opened", true],
    ["in_use", false],
    ["root_not_owned", false],
    ["io", false],
    ["unavailable", false],
    ["something_new", false],
  ])("restore_gating for %s is %s", async (code, enabled) => {
    backend({ status: () => recovery(code) });
    renderApp();
    const restore = await screen.findByRole("button", { name: /Restore from a backup/ });
    if (enabled) expect(restore).toBeEnabled();
    else expect(restore).toBeDisabled();
    expect(screen.getByRole("button", { name: "Try again" })).toBeEnabled();
  });

  it("restored_not_opened_never_says_nothing_changed", async () => {
    backend({ status: () => recovery("restored_not_opened", "could not open") });
    renderApp();
    await screen.findByRole("heading", { name: "JobPilot AI could not open your data" });
    expect(document.body).toHaveTextContent("The restore finished");
    expect(document.body).not.toHaveTextContent(/nothing (was )?changed/i);
  });

  it("retry_open_runs_only_on_click and a second failure replaces the error", async () => {
    const b = backend({
      status: () => recovery("migration_failed", "first"),
      retry_open: () => recovery("in_use", "second"),
    });
    const { user } = renderApp();
    await screen.findByRole("alert");
    expect(count(b, "retry_open")).toBe(0);
    await user.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("second"));
    expect(screen.getByRole("alert")).toHaveTextContent("in_use");
    expect(count(b, "retry_open")).toBe(1);
    expect(screen.getByRole("button", { name: /Restore from a backup/ })).toBeDisabled();
  });

  it("retry_open_success_returns_to_shell", async () => {
    backend({ status: () => recovery("in_use"), retry_open: () => READY });
    const { user } = renderApp();
    await user.click(await screen.findByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("navigation")).toBeInTheDocument();
  });

  it("restore_that_returns_recovery_routes_to_recovery_with_its_error", async () => {
    const b = backend({
      status: () => recovery("schema_too_new"),
      backup_list: () => [
        { path: "/home/me/jobpilot/backups/pre-restore-1", created_at: "2026-11-01", schema_version: 1 },
      ],
      backup_restore: () => recovery("restored_not_opened", "reopen failed"),
    });
    const { user } = renderApp();
    await user.click(await screen.findByRole("button", { name: /Restore from a backup/ }));
    await user.click(await screen.findByRole("button", { name: /pre-restore-1/ }));
    await user.click(await screen.findByRole("button", { name: "Replace my data" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("restored_not_opened"));
    expect(b.calls.find((c) => c.cmd === "backup_restore")?.args).toEqual({
      source: "/home/me/jobpilot/backups/pre-restore-1",
    });
    expect(document.body).toHaveTextContent("The restore finished");
  });
});
