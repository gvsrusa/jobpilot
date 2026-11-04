// Stands in for the Playwright MCP server in tests: newline JSON-RPC on stdio, no browser.
// MOCK_SNAPSHOT: file that browser_snapshot answers with. MOCK_CALLS: file each call is
// appended to. MOCK_EXIT_AFTER=n: exit after the nth reply to a tool call. MOCK_BIG=1: answer
// browser_snapshot with a line over the cap. MOCK_NOISE=1: before each tool reply, send a
// notification, a ping, and a stray reply with a string id; an answer to the ping is logged.
// A MOCK_SNAPSHOT page that starts with "!error" makes browser_snapshot a tool error; one that
// starts with "!rpcerror" makes every other tool a JSON-RPC error. MOCK_AFTER_FILL: once
// browser_fill_form was called, browser_snapshot answers with this file in place of MOCK_SNAPSHOT.
// MOCK_CLICK_REF: a ref whose click matters. MOCK_AFTER_CLICK: once that ref was clicked,
// browser_snapshot answers with this file. MOCK_DIE_ON_CLICK=1: the click on that ref is logged,
// then the server exits with no reply, as a crash mid-click would. MOCK_LATE and MOCK_LATE_AT=n:
// from the nth browser_snapshot after the fill on, until the click, it answers with MOCK_LATE.
// MOCK_START_DELAY_MS=n: read nothing for n ms after starting, as a slow server start would.
// Each start appends {"started": pid} to MOCK_CALLS.
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
let filled = false;
let clicked = false;
let lateSnapshots = 0;

if (process.env.MOCK_CALLS) fs.appendFileSync(process.env.MOCK_CALLS, JSON.stringify({ started: process.pid }) + "\n");
const delay = Number(process.env.MOCK_START_DELAY_MS || 0);
const begin = () => readline.createInterface({ input: process.stdin }).on("line", (line) => {
  const message = JSON.parse(line);
  const { id, method, params } = message;
  if (id === undefined) return;
  if (method === undefined) {
    if (process.env.MOCK_CALLS) fs.appendFileSync(process.env.MOCK_CALLS, JSON.stringify({ pong: id }) + "\n");
    return;
  }
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
    if (name === "browser_fill_form") filled = true;
    if (name === "browser_click" && process.env.MOCK_CLICK_REF && args?.target === process.env.MOCK_CLICK_REF) {
      clicked = true;
      if (process.env.MOCK_DIE_ON_CLICK) process.exit(0);
    }
    const after = process.env.MOCK_AFTER_FILL;
    const afterClick = process.env.MOCK_AFTER_CLICK;
    let shown = filled && after && fs.existsSync(after) ? after : process.env.MOCK_SNAPSHOT;
    if (filled && name === "browser_snapshot") lateSnapshots += 1;
    const late = process.env.MOCK_LATE;
    if (filled && late && lateSnapshots >= Number(process.env.MOCK_LATE_AT) && fs.existsSync(late)) shown = late;
    if (clicked && afterClick && fs.existsSync(afterClick)) shown = afterClick;
    const page = shown && fs.existsSync(shown) ? fs.readFileSync(shown, "utf8") : "";
    if (page.startsWith("!rpcerror") && name !== "browser_snapshot") {
      send({ jsonrpc: "2.0", id, error: { code: -32000, message: "failed" } });
      return;
    }
    let text = "ok " + name;
    if (name === "browser_snapshot" && process.env.MOCK_BIG) {
      text = "x".repeat(9 * 1024 * 1024);
    } else if (name === "browser_snapshot" && shown) {
      text = page;
    }
    if (process.env.MOCK_NOISE) {
      send({ jsonrpc: "2.0", method: "notifications/message", params: { level: "info" } });
      send({ jsonrpc: "2.0", id: "p-1", method: "ping" });
      send({ jsonrpc: "2.0", id: String(id), result: { content: [{ type: "text", text: "stray" }] } });
    }
    send({ jsonrpc: "2.0", id, result: { content: [{ type: "text", text }], isError: name === "fail" || text.startsWith("!error") } });
    calls += 1;
    if (process.env.MOCK_EXIT_AFTER && calls >= Number(process.env.MOCK_EXIT_AFTER)) {
      process.stdout.write("", () => process.exit(0));
    }
  } else {
    send({ jsonrpc: "2.0", id, error: { code: -32601, message: "no such method" } });
  }
});
if (delay > 0) setTimeout(begin, delay); else begin();
