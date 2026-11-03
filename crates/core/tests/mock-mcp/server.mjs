// Stands in for the Playwright MCP server in tests: newline JSON-RPC on stdio, no browser.
// MOCK_SNAPSHOT: file that browser_snapshot answers with. MOCK_CALLS: file each call is
// appended to. MOCK_EXIT_AFTER=n: exit after the nth reply to a tool call. MOCK_BIG=1: answer
// browser_snapshot with a line over the cap.
import fs from "node:fs";
import readline from "node:readline";

const NAMES = [
  "browser_click", "browser_close", "browser_console_messages", "browser_drag",
  "browser_evaluate", "browser_file_upload", "browser_fill_form", "browser_handle_dialog",
  "browser_hover", "browser_install", "browser_navigate", "browser_navigate_back",
  "browser_network_requests", "browser_press_key", "browser_resize", "browser_run_code",
  "browser_select_option", "browser_snapshot", "browser_tabs", "browser_take_screenshot",
  "browser_type", "browser_wait_for", "browser_mouse_move_xy", "browser_mouse_click_xy",
  "browser_mouse_drag_xy",
];

const send = (message) => process.stdout.write(JSON.stringify(message) + "\n");
let calls = 0;

const lines = readline.createInterface({ input: process.stdin });
lines.on("line", (line) => {
  const message = JSON.parse(line);
  const { id, method, params } = message;
  if (id === undefined) return;
  if (method === "initialize") {
    send({
      jsonrpc: "2.0", id,
      result: {
        protocolVersion: "2025-06-18", capabilities: { tools: {} },
        serverInfo: { name: "mock-mcp", version: "0.0.0" },
      },
    });
  } else if (method === "tools/list") {
    const tools = NAMES.map((name) => ({ name, description: name, inputSchema: { type: "object" } }));
    send({ jsonrpc: "2.0", id, result: { tools } });
  } else if (method === "tools/call") {
    const { name, arguments: args } = params;
    if (process.env.MOCK_CALLS) {
      fs.appendFileSync(process.env.MOCK_CALLS, JSON.stringify({ name, args }) + "\n");
    }
    if (name === "slow") return;
    let text = "ok " + name;
    if (name === "browser_snapshot" && process.env.MOCK_BIG) {
      text = "x".repeat(9 * 1024 * 1024);
    } else if (name === "browser_snapshot" && process.env.MOCK_SNAPSHOT) {
      text = fs.readFileSync(process.env.MOCK_SNAPSHOT, "utf8");
    }
    send({ jsonrpc: "2.0", id, result: { content: [{ type: "text", text }], isError: name === "fail" } });
    calls += 1;
    if (process.env.MOCK_EXIT_AFTER && calls >= Number(process.env.MOCK_EXIT_AFTER)) {
      process.stdout.write("", () => process.exit(0));
    }
  } else {
    send({ jsonrpc: "2.0", id, error: { code: -32601, message: "no such method" } });
  }
});
