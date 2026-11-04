import { act, screen, waitFor } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import { describe, expect, it } from "vitest";
import { backend, count, renderApp } from "../test-utils";

async function openUpdates(overrides: Parameters<typeof backend>[0] = {}) {
  const api = backend(overrides);
  const { user } = renderApp();
  await screen.findByRole("heading", { name: "Updates" });
  return { api, user };
}

describe("updates", () => {
  it("update_check_offline_reports_error", async () => {
    const { api, user } = await openUpdates({
      update_check: () => {
        throw { code: "update_check_failed", message: "error sending request" };
      },
    });
    await user.click(screen.getByRole("button", { name: "Check for updates" }));
    expect(await screen.findByText("Couldn't check for updates")).toBeInTheDocument();
    expect(screen.getByText("code: update_check_failed")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Install and restart" })).not.toBeInTheDocument();
    expect(count(api, "update_install")).toBe(0);
  });

  it("update_available_offers_install", async () => {
    const { api, user } = await openUpdates({
      update_check: () => ({ version: "1.0.1", notes: null, date: null }),
      update_install: () => undefined,
    });
    await user.click(screen.getByRole("button", { name: "Check for updates" }));
    expect(await screen.findByText("Version 1.0.1 is available")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Install and restart" }));
    await waitFor(() => expect(count(api, "update_install")).toBe(1));
  });

  it("an install refused during a run says why", async () => {
    const { user } = await openUpdates({
      update_check: () => ({ version: "1.0.1", notes: null, date: null }),
      update_install: () => {
        throw { code: "update_blocked_by_run", message: "an apply run is active or the emergency pause is on" };
      },
    });
    await user.click(screen.getByRole("button", { name: "Check for updates" }));
    await user.click(await screen.findByRole("button", { name: "Install and restart" }));
    expect(
      await screen.findByText("Not now: an apply run is active or the emergency pause is on. Update when it has ended."),
    ).toBeInTheDocument();
  });

  it("an up-to-date check says so", async () => {
    const { user } = await openUpdates({ update_check: () => null });
    await user.click(screen.getByRole("button", { name: "Check for updates" }));
    expect(await screen.findByText("JobPilot is up to date.")).toBeInTheDocument();
  });

  it("launch_check_toggle_defaults_off", async () => {
    const { api, user } = await openUpdates();
    const toggle = await screen.findByRole("switch", { name: "Check for updates when JobPilot starts" });
    await waitFor(() => expect(toggle).toBeEnabled());
    expect(toggle).not.toBeChecked();
    expect(count(api, "update_check")).toBe(0);
    await user.click(toggle);
    await waitFor(() =>
      expect(api.calls.find((c) => c.cmd === "update_auto_set")?.args).toEqual({ on: true }),
    );
  });

  it("an update found at launch shows without a check", async () => {
    await openUpdates({ update_pending: () => ({ version: "1.0.2", notes: null, date: null }) });
    expect(await screen.findByText("Version 1.0.2 is available")).toBeInTheDocument();
  });

  it("the launch check's event shows the version", async () => {
    await openUpdates();
    // The listener registers after the first render; send until it has heard.
    await waitFor(async () => {
      await act(() => emit("update-available", { version: "1.0.3", notes: null, date: null }));
      expect(screen.getByText("Version 1.0.3 is available")).toBeInTheDocument();
    });
  });
});
