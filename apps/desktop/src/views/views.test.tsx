import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { backend, count, recovery, renderApp } from "../test-utils";

async function open(user: ReturnType<typeof renderApp>["user"], name: string) {
  await user.click(await screen.findByRole("button", { name }));
  await screen.findByRole("heading", { name: name === "Data" ? "Data and privacy" : name });
}

describe("documents", () => {
  it("documents_create_then_list", async () => {
    const b = backend();
    const { user } = renderApp();
    await open(user, "Documents");
    expect(await screen.findByText(/No documents yet/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "New document" }));
    await user.type(screen.getByLabelText("Title"), "Resume 2026");
    await user.type(screen.getByLabelText("Body"), "rust and sqlite");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("button", { name: "Resume 2026" })).toBeInTheDocument();
    expect(b.calls.find((c) => c.cmd === "documents_create")?.args).toEqual({
      title: "Resume 2026",
      body: "rust and sqlite",
    });
    expect(screen.getByText(/not encrypted/)).toBeInTheDocument();
  });

  it("search_shows_snippets_and_empty_state", async () => {
    const b = backend();
    const { user } = renderApp();
    await open(user, "Documents");
    await user.click(screen.getByRole("button", { name: "New document" }));
    await user.type(screen.getByLabelText("Title"), "Notes");
    await user.type(screen.getByLabelText("Body"), "borrow checker");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await screen.findByRole("button", { name: "Notes" });

    const search = screen.getByLabelText("Search");
    await user.type(search, "borrow");
    expect(await screen.findByText("borrow checker", { selector: "p" })).toBeInTheDocument();
    expect(screen.getByText("1 result")).toBeInTheDocument();
    // debounced: one search for the final text, not one per keystroke
    expect(count(b, "documents_search")).toBe(1);

    await user.clear(search);
    await user.type(search, "zzz");
    expect(await screen.findByText("No documents match 'zzz'.")).toBeInTheDocument();
  });

  it("snippets_render_as_text_not_html", async () => {
    backend({
      documents_search: () => [{ id: "1", title: "T", snippet: "<img src=x onerror=alert(1)> <b>bold</b>" }],
    });
    const { user } = renderApp();
    await open(user, "Documents");
    await user.type(screen.getByLabelText("Search"), "x");
    const snippet = await screen.findByText(/onerror/);
    expect(snippet.textContent).toContain("<b>bold</b>");
    expect(document.querySelector("img")).toBeNull();
    expect(document.querySelector("b")).toBeNull();
  });

  it("shows_error_message_from_ipc", async () => {
    backend({
      documents_list: () => Promise.reject({ code: "in_use", message: "another process holds the data" }),
    });
    const { user } = renderApp();
    await open(user, "Documents");
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("another process holds the data");
    expect(alert).toHaveTextContent("in_use");
  });

  it("delete_document_confirms_with_title", async () => {
    const b = backend();
    const { user } = renderApp();
    await open(user, "Documents");
    await user.click(screen.getByRole("button", { name: "New document" }));
    await user.type(screen.getByLabelText("Title"), "Old draft");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await screen.findByRole("button", { name: "Old draft" });
    await user.click(screen.getByRole("button", { name: /Delete/ }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("Old draft");
    await user.click(within(dialog).getByRole("button", { name: "Delete document" }));
    await waitFor(() => expect(count(b, "documents_delete")).toBe(1));
  });
});

describe("data", () => {
  it("delete_needs_typed_DELETE", async () => {
    const b = backend();
    const { user } = renderApp();
    await open(user, "Data");
    await user.click(screen.getByRole("button", { name: /Delete all data/ }));
    const dialog = await screen.findByRole("dialog");
    const confirm = within(dialog).getByRole("button", { name: "Delete everything" });
    expect(confirm).toBeDisabled();
    const field = within(dialog).getByLabelText(/Type DELETE/);
    await user.type(field, "delete");
    expect(confirm).toBeDisabled();
    await user.clear(field);
    await user.type(field, "DELETE");
    expect(confirm).toBeEnabled();
    await user.click(within(dialog).getByLabelText(/Also delete backups/));
    await user.click(confirm);
    await waitFor(() => expect(count(b, "data_delete")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "data_delete")?.args).toEqual({
      confirm: "DELETE",
      includeBackups: true,
    });
  });

  it("second_delete_requires_typing_again_with_backups_unchecked", async () => {
    const b = backend();
    const { user } = renderApp();
    await open(user, "Data");
    await user.click(screen.getByRole("button", { name: /Delete all data/ }));
    let dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByLabelText(/Type DELETE/), "DELETE");
    await user.click(within(dialog).getByLabelText(/Also delete backups/));
    await user.click(within(dialog).getByRole("button", { name: "Delete everything" }));
    await waitFor(() => expect(count(b, "data_delete")).toBe(1));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());

    await user.click(screen.getByRole("button", { name: /Delete all data/ }));
    dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByLabelText(/Type DELETE/)).toHaveValue("");
    expect(within(dialog).getByLabelText(/Also delete backups/)).not.toBeChecked();
    expect(within(dialog).getByRole("button", { name: "Delete everything" })).toBeDisabled();
  });

  it("restore_and_delete_drop_cached_documents", async () => {
    backend({ documents_list: () => [{ id: "1", title: "Old", body: "", created_at: "", updated_at: "" }] });
    const { user, client } = renderApp();
    await open(user, "Documents");
    await screen.findByRole("button", { name: "Old" });
    await user.click(screen.getByRole("button", { name: "Data" }));
    await screen.findByRole("heading", { name: "Data and privacy" });
    expect(client.getQueryData(["documents"])).toBeDefined();
    await user.click(screen.getByRole("button", { name: /Delete all data/ }));
    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByLabelText(/Type DELETE/), "DELETE");
    await user.click(within(dialog).getByRole("button", { name: "Delete everything" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(client.getQueryData(["documents"])).toBeUndefined();
  });

  it("delete_that_leaves_recovery_routes_to_recovery", async () => {
    backend({ data_delete: () => recovery("io", "could not reopen") });
    const { user } = renderApp();
    await open(user, "Data");
    await user.click(screen.getByRole("button", { name: /Delete all data/ }));
    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByLabelText(/Type DELETE/), "DELETE");
    await user.click(within(dialog).getByRole("button", { name: "Delete everything" }));
    expect(await screen.findByText("JobPilot AI could not open your data")).toBeInTheDocument();
  });

  it("restore_warns_before_replacing_data", async () => {
    const b = backend({
      backup_list: () => [
        { path: "/home/me/jobpilot/backups/manual-1", created_at: "2026-11-01", schema_version: null },
      ],
    });
    const { user } = renderApp();
    await open(user, "Data");
    await user.click(await screen.findByRole("button", { name: /Restore.*manual-1/ }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("Replace your current data with this backup?");
    expect(dialog).toHaveTextContent("schema unknown");
    expect(dialog).toHaveTextContent("pre-restore");
    expect(within(dialog).getByRole("button", { name: "Cancel" })).toHaveFocus();
    expect(count(b, "backup_restore")).toBe(0);
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(count(b, "backup_restore")).toBe(0);
  });

  it("restore_from_another_folder_uses_directory_picker", async () => {
    const b = backend({ "plugin:dialog|open": () => "/Volumes/usb/jp-backup" });
    const { user } = renderApp();
    await open(user, "Data");
    await user.click(screen.getByRole("button", { name: /Restore from another folder/ }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("/Volumes/usb/jp-backup");
    await user.click(within(dialog).getByRole("button", { name: "Replace my data" }));
    await waitFor(() => expect(count(b, "backup_restore")).toBe(1));
    expect(b.calls.find((c) => c.cmd === "plugin:dialog|open")?.args).toMatchObject({
      options: { directory: true },
    });
  });

  it("restore_failure_shows_error_and_keeps_dialog", async () => {
    backend({
      backup_list: () => [{ path: "/x/backups/b1", created_at: "2026-11-01", schema_version: 1 }],
      backup_restore: () =>
        Promise.reject({ code: "invalid_backup", message: "not a JobPilot backup" }),
    });
    const { user } = renderApp();
    await open(user, "Data");
    await user.click(await screen.findByRole("button", { name: /Restore.*b1/ }));
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: "Replace my data" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("invalid_backup");
  });

  it("create_backup_lists_it", async () => {
    backend();
    const { user } = renderApp();
    await open(user, "Data");
    expect(await screen.findByText(/No backups yet/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Create backup" }));
    expect(await screen.findByText("manual-20261101")).toBeInTheDocument();
  });

  it("export_reports_path", async () => {
    backend();
    const { user } = renderApp();
    await open(user, "Data");
    await user.click(screen.getByRole("button", { name: /Export all data/ }));
    expect(await screen.findByText(/exports\/export-1/)).toBeInTheDocument();
  });

  it("secret_ui_never_displays_values", async () => {
    const b = backend();
    const { user } = renderApp();
    await open(user, "Data");
    expect(await screen.findByText("No secrets stored.")).toBeInTheDocument();
    await user.type(screen.getByLabelText("Secret name"), "api-key-example");
    const value = screen.getByLabelText("Secret value");
    expect(value).toHaveAttribute("type", "password");
    await user.type(value, "hunter2-super-secret");
    await user.click(screen.getByRole("button", { name: "Save secret" }));
    expect(await screen.findByText("api-key-example")).toBeInTheDocument();
    expect(b.calls.find((c) => c.cmd === "secret_set")?.args).toEqual({
      name: "api-key-example",
      value: "hunter2-super-secret",
    });
    expect(document.body.textContent).not.toContain("hunter2-super-secret");
    expect(screen.getByLabelText("Secret value")).toHaveValue("");
    expect(screen.getByLabelText("Secret name")).toHaveValue("");
  });

  it("shows_recent_activity", async () => {
    backend({
      audit_recent: () => [
        { id: 1, at: "2026-11-01T10:00:00Z", actor: "gui", action: "backup.create", target: "b1", detail: {} },
      ],
    });
    const { user } = renderApp();
    await open(user, "Data");
    expect(await screen.findByText("backup.create")).toBeInTheDocument();
  });
});
