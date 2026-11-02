import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { Application, ApplicationEvent, ApplicationInput, ApplicationStatus } from "../lib/ipc";
import { backend, count, renderApp } from "../test-utils";

type Args = Record<string, unknown>;

/** A small in-memory store behind the tracking commands. */
function tracking() {
  const apps: Application[] = [];
  const events = new Map<string, ApplicationEvent[]>();
  let n = 0;
  const log = (id: string, kind: ApplicationEvent["kind"], over: Partial<ApplicationEvent> = {}) => {
    const list = events.get(id) ?? [];
    list.push({
      id: ++n,
      at: "2026-11-01T10:00:00Z",
      actor: "gui",
      kind,
      from_status: null,
      to_status: null,
      note: "",
      ...over,
    });
    events.set(id, list);
  };
  const find = (id: unknown) => apps.find((a) => a.id === id)!;
  return backend({
    app_list: ({ status }: Args) => apps.filter((a) => !status || a.status === status),
    app_add: ({ input }: Args) => {
      const app: Application = {
        ...(input as ApplicationInput),
        id: `a${apps.length + 1}`,
        status: "saved",
        applied_on: null,
        created_at: "2026-11-01T10:00:00Z",
        updated_at: "2026-11-01T10:00:00Z",
      };
      apps.unshift(app);
      log(app.id, "created", { to_status: "saved" });
      return app;
    },
    app_update: ({ id, input }: Args) => {
      Object.assign(find(id), input);
      log(String(id), "edited");
      return find(id);
    },
    app_set_status: ({ id, status, on, note }: Args) => {
      const app = find(id);
      log(app.id, "status", { from_status: app.status, to_status: status as ApplicationStatus, note: String(note) });
      app.status = status as ApplicationStatus;
      if (status === "applied" && !app.applied_on) app.applied_on = (on as string) ?? "2026-11-01";
      return app;
    },
    app_note: ({ id, note }: Args) => {
      log(String(id), "note", { note: String(note) });
    },
    app_history: ({ id }: Args) => events.get(String(id)) ?? [],
    app_remove: ({ id }: Args) => {
      apps.splice(apps.indexOf(find(id)), 1);
    },
  });
}

async function open(user: ReturnType<typeof renderApp>["user"]) {
  await user.click(await screen.findByRole("button", { name: "Applications" }));
  await screen.findByRole("heading", { level: 1, name: "Applications" });
}

describe("applications", () => {
  it("ctrl_7_opens_applications", async () => {
    backend();
    const { user } = renderApp();
    await screen.findByRole("heading", { name: "Status" });
    await user.keyboard("{Control>}7{/Control}");
    expect(await screen.findByRole("heading", { level: 1, name: "Applications" })).toBeInTheDocument();
    expect(await screen.findByText("No applications yet. Add the first one.")).toBeInTheDocument();
  });

  it("applications_add_change_status_and_see_history", async () => {
    const api = tracking();
    const { user } = renderApp();
    await open(user);

    await user.click(screen.getByRole("button", { name: "Add application" }));
    await user.type(screen.getByLabelText("Company"), "Acme");
    await user.type(screen.getByLabelText("Role"), "Engineer");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(count(api, "app_add")).toBe(1));
    expect(api.calls.find((c) => c.cmd === "app_add")!.args.input).toMatchObject({
      company: "Acme",
      role: "Engineer",
      job_text_id: null,
    });

    const row = await screen.findByRole("button", { name: "Acme" });
    expect(row.closest("tr")).toHaveTextContent("Saved");
    await user.click(row);
    await user.selectOptions(await screen.findByLabelText("New status"), "applied");
    await user.type(screen.getByLabelText("Applied on (optional)"), "2026-10-02");
    await user.type(screen.getByLabelText("Status note (optional)"), "sent online");
    await user.click(screen.getByRole("button", { name: "Set status" }));
    await waitFor(() => expect(count(api, "app_set_status")).toBe(1));
    expect(api.calls.find((c) => c.cmd === "app_set_status")!.args).toMatchObject({
      id: "a1",
      status: "applied",
      on: "2026-10-02",
      note: "sent online",
    });

    await user.type(screen.getByLabelText("Add note"), "recruiter called");
    await user.click(screen.getByRole("button", { name: "Add note" }));
    const history = await screen.findByRole("list", { name: "History" });
    await waitFor(() => expect(within(history).getAllByRole("listitem")).toHaveLength(3));
    expect(history).toHaveTextContent("Saved to Applied");
    expect(history).toHaveTextContent("sent online");
    expect(history).toHaveTextContent("recruiter called");
    expect(row.closest("tr")).toHaveTextContent("Applied");
  });

  it("filters_by_status_and_removes_after_confirming", async () => {
    const api = tracking();
    const { user } = renderApp();
    await open(user);
    await user.click(screen.getByRole("button", { name: "Add application" }));
    await user.type(screen.getByLabelText("Company"), "Acme");
    await user.type(screen.getByLabelText("Role"), "Engineer");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await user.click(await screen.findByRole("button", { name: "Acme" }));

    await user.selectOptions(screen.getByLabelText("Filter by status"), "offer");
    expect(await screen.findByText("No applications with this status.")).toBeInTheDocument();
    await user.selectOptions(screen.getByLabelText("Filter by status"), "");
    await user.click(await screen.findByRole("button", { name: "Acme" }));

    await user.click(screen.getByRole("button", { name: "Remove..." }));
    expect(count(api, "app_remove")).toBe(0);
    await user.click(screen.getByRole("button", { name: "Remove application" }));
    await waitFor(() => expect(count(api, "app_remove")).toBe(1));
    expect(await screen.findByText("No applications yet. Add the first one.")).toBeInTheDocument();
  });

  it("shows_a_refusal_with_its_code", async () => {
    backend({
      app_add: () => {
        throw { code: "validation", message: "invalid input: a country is two letters" };
      },
    });
    const { user } = renderApp();
    await open(user);
    await user.click(screen.getByRole("button", { name: "Add application" }));
    await user.type(screen.getByLabelText("Company"), "Acme");
    await user.type(screen.getByLabelText("Role"), "Engineer");
    await user.click(screen.getByRole("button", { name: "Save" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("code: validation");
  });

  it("an_application_link_is_never_an_anchor", async () => {
    tracking();
    const { user, container } = renderApp();
    await open(user);
    await user.click(screen.getByRole("button", { name: "Add application" }));
    await user.type(screen.getByLabelText("Company"), "Acme");
    await user.type(screen.getByLabelText("Role"), "Engineer");
    await user.type(screen.getByLabelText(/^Link/), "https://example.com/job?x=1");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await user.click(await screen.findByRole("button", { name: "Acme" }));
    expect(screen.getByLabelText(/^Link/)).toHaveValue("https://example.com/job?x=1");
    expect(screen.getByText(/untrusted/i)).toBeInTheDocument();
    expect(container.querySelectorAll("a, [role='link']")).toHaveLength(0);
  });
});
