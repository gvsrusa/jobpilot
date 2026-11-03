//! `apply-mcp`: the MCP server an apply session talks to, in front of a Playwright MCP child.
//! It offers three apply tools and thirteen browser tools, listed at once from a pinned copy;
//! the child starts on the first browser call. It checks every browser call with the guards,
//! scans every snapshot it forwards or takes, and holds a tool reply while a person handles a
//! stop. Nothing the session sends can end a pause.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use super::brief::{self, Brief};
use super::guard::{self, BROWSER_TOOLS, PauseKind};
use super::{Held, Item, PAUSE_HOLD};
use crate::ai::CancellationToken;
use crate::applications;
use crate::browser::mcp::{
    self, Incoming, McpClient, ToolReply, error_reply, read_message, reply, tool_text,
};
use crate::browser::runtime::{self, BrowserSpec, RuntimeState};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::generate::{CLOSE, OPEN, neutralise};
use crate::memory::{self, Class};
use crate::pause;
use crate::permissions::{self, Action, Decision, Grant, Mode};

const HANDLED: &str = "A person handled this field. Take a snapshot and go on.";
const STOPPED: &str = "Stopped for a person. Call apply_next.";
const VALUE_REFUSED: &str = "not an approved answer or profile fact; call apply_pause";
const UNLABELED: &str = "unlabeled_field: this field has no label, so it cannot be checked. \
    Call apply_pause for a person, or apply_result with outcome skipped and reason no_form.";
const SECOND_SUBMIT: &str = "A second submit click on this application.";
const PAUSE_KINDS: [&str; 7] = [
    "login",
    "captcha",
    "mfa",
    "identity",
    "legal",
    "no_answer",
    "other",
];

macro_rules! now {
    () => {
        "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
    };
}

/// Test settings. A release build ignores both: the hold is `PAUSE_HOLD`, and the runtime
/// reads no debug variable.
#[doc(hidden)]
#[derive(Debug, Clone, Default)]
pub struct Knobs {
    pub hold: Option<Duration>,
    /// Read as `JOBPILOT_PLAYWRIGHT_CLI`, so a test needs no process-wide variable.
    pub playwright_cli: Option<PathBuf>,
}

/// Serves one session for the run whose folder is `dir`, reading one call at a time, until
/// the input ends, the emergency pause is on, or the run is cancelled.
pub async fn serve(
    db: &Db,
    dir: &Path,
    input: impl AsyncBufRead + Unpin,
    output: impl AsyncWrite + Unpin,
) -> Result<()> {
    serve_with(db, dir, input, output, Knobs::default()).await
}

#[doc(hidden)]
pub async fn serve_with(
    db: &Db,
    dir: &Path,
    input: impl AsyncBufRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
    knobs: Knobs,
) -> Result<()> {
    let run_id = dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::Validation("the run folder has no id".into()))?
        .to_owned();
    let actor: String = sqlx::query_scalar("SELECT actor FROM apply_runs WHERE id = ?")
        .bind(&run_id)
        .fetch_optional(&db.pool)
        .await?
        .ok_or(Error::NotFound)?;
    let hold = match cfg!(debug_assertions) {
        true => knobs.hold.unwrap_or(PAUSE_HOLD),
        false => PAUSE_HOLD,
    };
    let mut session = Session {
        db,
        dir: dir.to_path_buf(),
        run_id,
        actor,
        hold,
        playwright_cli: knobs.playwright_cli,
        client: None,
        cancel: CancellationToken::new(),
        current: None,
    };
    let served = session.run(input, &mut output).await;
    if let Some(client) = session.client.take() {
        client.close().await;
    }
    served
}

/// The application in hand, and what the guards read for it.
struct Current {
    item: Item,
    brief: Brief,
    allowed: Vec<String>,
    /// The newest snapshot this session saw, forwarded or its own.
    last: String,
    /// Stops a person already answered, by `guard::hit_key`.
    acks: HashSet<String>,
    /// What the session typed into each field, by ref, so the scan knows the owner's own
    /// answer when the page shows it back.
    typed: guard::Typed,
}

enum Step {
    Go,
    /// Taken over or timed out: the item is no longer this session's.
    Stopped,
}

struct Session<'a> {
    db: &'a Db,
    dir: PathBuf,
    run_id: String,
    actor: String,
    hold: Duration,
    playwright_cli: Option<PathBuf>,
    client: Option<McpClient>,
    cancel: CancellationToken,
    current: Option<Current>,
}

fn refusal(text: &str) -> Value {
    tool_text(text, true)
}

fn said(value: Value) -> Value {
    tool_text(&value.to_string(), false)
}

fn wrap(text: &str) -> String {
    format!("{OPEN}\n{}\n{CLOSE}", neutralise(text))
}

/// What a refusal tells the session. Codes only, never page text.
fn refusal_text(error: &Error) -> Option<String> {
    match error {
        Error::Refused(why) | Error::Failed(why) | Error::Validation(why) => Some(why.clone()),
        Error::TooLarge { .. } => Some("reply_too_large".into()),
        Error::NotFound => Some("not_found".into()),
        _ => None,
    }
}

fn is_ping(line: &str) -> bool {
    serde_json::from_str::<Value>(line).is_ok_and(|message| message["method"] == "ping")
}

/// The apply tools, then the pinned browser tools. Nothing here waits for the Playwright
/// server, which starts on the first browser call.
fn tools() -> Vec<Value> {
    let mut tools = apply_tools();
    tools.extend(browser_tools());
    tools
}

fn apply_tools() -> Vec<Value> {
    vec![
        json!({
            "name": "apply_next",
            "description": "The next application to fill: its facts, approved answers, and files. \
                Call it first, and again after apply_result.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
        }),
        json!({
            "name": "apply_pause",
            "description": "Ask a person: a login, CAPTCHA, code, identity check, legal statement, \
                or a question with no approved answer (no_answer, with the question).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": {"type": "string", "enum": PAUSE_KINDS},
                    "question": {"type": "string"},
                    "note": {"type": "string"},
                },
                "required": ["kind"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "apply_result",
            "description": "End the current application, with one of the listed reason codes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "outcome": {"type": "string", "enum": ["submitted", "skipped", "failed"]},
                    "reason": {"type": "string", "enum": REASONS},
                },
                "required": ["outcome"],
                "additionalProperties": false,
            },
        }),
    ]
}

/// The thirteen browser tools as `@playwright/mcp` 0.0.84 lists them, taken from its own
/// `tools/list`. Served from here so the list answers before the server has started: Claude Code
/// connects in the background, and a first turn can run before a slow server's tools arrive.
const PINNED_BROWSER_TOOLS: &str = include_str!("../../assets/playwright_mcp_tools.json");

/// The pinned entries of the allowed browser tools. A real-runtime test checks that they still
/// match the pinned server's own.
#[doc(hidden)]
pub fn browser_tools() -> Vec<Value> {
    let pinned: Vec<Value> =
        serde_json::from_str(PINNED_BROWSER_TOOLS).expect("the pinned tool list is JSON");
    pinned
        .into_iter()
        .filter(|tool| {
            tool["name"]
                .as_str()
                .is_some_and(|name| BROWSER_TOOLS.contains(&name))
        })
        .collect()
}

/// The element a call acts on, by its snapshot ref.
fn target_of(args: &Value) -> String {
    ["target", "ref"]
        .iter()
        .find_map(|key| args.get(key).and_then(Value::as_str))
        .unwrap_or("")
        .to_owned()
}

/// Targets whose value the call selects as an option: every `browser_select_option` target,
/// and each `browser_fill_form` field not declared as text or a checkbox or radio.
fn picks_options(tool: &str, args: &Value) -> HashSet<String> {
    match tool {
        "browser_select_option" => guard::targets_in(tool, args).into_iter().collect(),
        "browser_fill_form" => args
            .get("fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|field| {
                !matches!(
                    field.get("type").and_then(Value::as_str),
                    Some("textbox" | "slider" | "checkbox" | "radio")
                )
            })
            .map(target_of)
            .collect(),
        _ => HashSet::new(),
    }
}

/// Every guard a typed or selected value passes, the same for apply-mcp and Copilot: the
/// field's label (`field_guard`), then the option a selection names (`option_guard`) or the
/// choice the target is (`choice_guard`). `picks` says the value selects an option. `None` is
/// a field with no label that is not a choice: it cannot be judged. `Err` is the pause a
/// person answers instead.
pub(crate) fn judge_field(
    snapshot: &str,
    target: &str,
    value: &str,
    picks: bool,
    brief: &Brief,
) -> Option<std::result::Result<(), PauseKind>> {
    let label = guard::label_of(snapshot, target);
    let choice = guard::is_choice(snapshot, target);
    let checked = match &label {
        Some(label) => guard::field_guard(label, value, brief),
        None if choice => Ok(()),
        None => return None,
    };
    Some(match (checked, picks, choice) {
        (Ok(()), true, _) => option_guard(snapshot, target, value, brief),
        (Ok(()), false, true) => guard::choice_guard(snapshot, target, brief),
        (checked, _, _) => checked,
    })
}

/// Judges the real option a value selects. The value must name, after folding, exactly one
/// option line in the target's subtree on the last snapshot; `choice_guard` then judges that
/// option. A value that names none or several, or holds a control character, is a pause:
/// Playwright also selects by an option's value attribute, which the snapshot does not show.
fn option_guard(
    snapshot: &str,
    target: &str,
    value: &str,
    brief: &Brief,
) -> std::result::Result<(), PauseKind> {
    if value.chars().any(char::is_control) {
        return Err(PauseKind::Other);
    }
    // Playwright retargets an element inside a label to the label's control, so option lines
    // under any other role could be a decoy for a select elsewhere on the page.
    if !matches!(
        guard::role_of(snapshot, target).as_deref(),
        Some("combobox" | "listbox")
    ) {
        return Err(PauseKind::Other);
    }
    let marker = format!("[ref={target}]");
    let lines: Vec<&str> = snapshot.lines().collect();
    // A marker quoted inside another line's name could point at the wrong line: a person decides.
    let mut at = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains(&marker));
    let (Some((index, line)), None) = (at.next(), at.next()) else {
        return Err(PauseKind::Other);
    };
    let indent = |line: &str| line.len() - line.trim_start().len();
    let depth = indent(line);
    let subtree = lines[index + 1..]
        .iter()
        .take_while(|line| line.trim().is_empty() || indent(line) > depth)
        .count();

    // A copy where each line in the subtree has a ref, as Playwright prints options without
    // one. Only the page's own lines are judged: the value is never written into the copy.
    let mut copy: Vec<String> = lines.iter().map(|line| (*line).to_owned()).collect();
    let mut free = 900_000_000_u64;
    let mut refs = Vec::new();
    for line in &mut copy[index + 1..index + 1 + subtree] {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(found) = ref_in(line) {
            refs.push(found);
            continue;
        }
        while snapshot.contains(&format!("[ref=e{free}]")) {
            free += 1;
        }
        let tag = format!("e{free}");
        free += 1;
        *line = match line.strip_suffix(':') {
            Some(head) => format!("{head} [ref={tag}]:"),
            None => format!("{line} [ref={tag}]"),
        };
        refs.push(tag);
    }
    let copy = copy.join("\n");
    let wanted = fold(value);
    // A ref resolves to one line by `index_of`'s rule; only a line inside the subtree counts.
    let inside = index + 1..=index + subtree;
    let mut matching = refs.iter().filter(|r| {
        guard::index_of(&copy, r).is_some_and(|at| inside.contains(&at))
            && guard::role_of(&copy, r).as_deref() == Some("option")
            && guard::label_of(&copy, r).is_some_and(|name| fold(&name) == wanted)
    });
    match (matching.next(), matching.next()) {
        (Some(option), None) => guard::choice_guard(&copy, option, brief),
        _ => Err(PauseKind::Other),
    }
}

/// The ref a snapshot line names: the last `[ref=…]` outside quotes, by `guard::index_of`'s
/// rule. A marker inside a quoted name is part of the name, not a ref.
fn ref_in(line: &str) -> Option<String> {
    let (at, _) = line
        .match_indices("[ref=")
        .filter(|(at, _)| guard::outside_quotes(line, *at))
        .last()?;
    let (found, _) = line[at + "[ref=".len()..].split_once(']')?;
    Some(found.to_owned())
}

fn fold(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The reasons `apply_result` keeps. Anything else is stored as `other`, so no page text lands
/// on the row.
const REASONS: [&str; 8] = [
    "form_error",
    "site_down",
    "not_eligible",
    "duplicate",
    "no_form",
    "login_required",
    "closed",
    "other",
];

fn reason_code(args: &Value) -> Option<&'static str> {
    let reason = args.get("reason").and_then(Value::as_str)?.trim();
    Some(
        REASONS
            .into_iter()
            .find(|code| *code == reason)
            .unwrap_or("other"),
    )
}

impl Session<'_> {
    async fn run(
        &mut self,
        input: impl AsyncBufRead + Unpin,
        output: &mut (impl AsyncWrite + Unpin),
    ) -> Result<()> {
        let mut lines = input.lines();
        while let Some(line) = lines.next_line().await? {
            let Ok(message) = read_message(&line) else {
                continue;
            };
            let mut end = false;
            let mut failed = None;
            let answer = match message {
                Incoming::Notification => continue,
                Incoming::Initialize { id } => reply(
                    &id,
                    json!({
                        "protocolVersion": mcp::PROTOCOL,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "jobpilot", "version": env!("CARGO_PKG_VERSION")},
                    }),
                ),
                Incoming::ToolsList { id } => reply(&id, json!({"tools": tools()})),
                Incoming::Other { id } if is_ping(&line) => reply(&id, json!({})),
                Incoming::Other { id } => error_reply(&id, -32601, "no such method"),
                Incoming::ToolCall { id, name, args } => {
                    let result = match self.call(&name, args).await {
                        Ok(result) => result,
                        Err(Error::Paused) => {
                            end = true;
                            refusal("paused: the emergency pause is on")
                        }
                        Err(Error::Cancelled) => {
                            end = true;
                            refusal("cancelled: this run was stopped")
                        }
                        Err(error) => match refusal_text(&error) {
                            Some(text) => refusal(&text),
                            None => {
                                end = true;
                                let text = refusal(error.code());
                                failed = Some(error);
                                text
                            }
                        },
                    };
                    reply(&id, result)
                }
            };
            output.write_all(answer.as_bytes()).await?;
            output.write_all(b"\n").await?;
            output.flush().await?;
            if let Some(error) = failed {
                return Err(error);
            }
            if end {
                break;
            }
        }
        Ok(())
    }

    async fn call(&mut self, name: &str, args: Value) -> Result<Value> {
        match name {
            "apply_next" => self.next().await,
            "apply_pause" => self.ask(&args).await,
            "apply_result" => self.result(&args).await,
            _ => self.browser(name, args).await,
        }
    }

    fn files(&self) -> PathBuf {
        self.dir.join("files")
    }

    async fn client(&mut self) -> Result<&mut McpClient> {
        if self.client.is_none() {
            let cli = self.playwright_cli.clone().map(OsString::from);
            let get = |name: &str| match (name, &cli) {
                ("JOBPILOT_PLAYWRIGHT_CLI", Some(cli)) => Some(cli.clone()),
                _ => std::env::var_os(name),
            };
            let RuntimeState::Ready(found) = runtime::state_by(self.db, &get).await? else {
                return Err(Error::Refused("no_browser_runtime".into()));
            };
            let files = self.files();
            std::fs::create_dir_all(&files)?;
            let config = self.dir.join("playwright.json");
            let spec = BrowserSpec {
                profile: self.dir.join("profile"),
                output: self.dir.join("output"),
            };
            std::fs::write(&config, runtime::playwright_config(&spec).to_string())?;
            let (program, args, env) = runtime::command_for(&found, &config, &files);
            let started = McpClient::start(&program, &args, &env, &files)
                .await
                .map_err(|error| match error {
                    Error::Failed(why) => Error::Failed(why),
                    _ => Error::Failed("no_browser_runtime".into()),
                })?;
            self.client = Some(started);
        }
        Ok(self.client.as_mut().expect("started above"))
    }

    /// Step 1: the flag is off and nobody stopped the run.
    async fn ready(&self) -> Result<()> {
        pause::ensure_off(self.db).await?;
        if super::run_cancelled(self.db, &self.run_id).await? {
            return Err(Error::Cancelled);
        }
        Ok(())
    }

    /// Whether an item is active, read from the row each time, so an item another process
    /// ended is not acted on. An item a session finds active when it starts was left by an
    /// earlier session: it ends `session_ended`, as `finish_items` ends it. Its brief is not
    /// built again, which would stage its files and use its answers a second time.
    async fn current(&mut self) -> Result<bool> {
        let active: Option<String> = sqlx::query_scalar(
            "SELECT id FROM apply_items WHERE apply_run_id = ? AND state = 'active' \
             ORDER BY position LIMIT 1",
        )
        .bind(&self.run_id)
        .fetch_optional(&self.db.pool)
        .await?;
        let Some(id) = active else {
            self.current = None;
            return Ok(false);
        };
        if self.current.as_ref().is_some_and(|c| c.item.id == id) {
            return Ok(true);
        }
        super::end_left(self.db, &id).await?;
        self.current = None;
        Ok(false)
    }

    async fn set_item(&mut self, state: &str, reason: Option<&str>) -> Result<()> {
        let Some(current) = self.current.take() else {
            return Ok(());
        };
        sqlx::query(
            "UPDATE apply_items SET state = ?, reason = ? WHERE id = ? AND state = 'active'",
        )
        .bind(state)
        .bind(reason)
        .bind(&current.item.id)
        .execute(&self.db.pool)
        .await?;
        Ok(())
    }

    /// Holds the reply for a person. With `end_item`, a take-over or a timeout ends the item
    /// as `manual_required`.
    async fn pause(
        &mut self,
        kind: PauseKind,
        origin: &str,
        excerpt: &str,
        end_item: bool,
    ) -> Result<Step> {
        let item = match &self.current {
            Some(current) => current.item.clone(),
            None => return Ok(Step::Stopped),
        };
        let held = super::hold(
            self.db,
            &item,
            kind,
            origin,
            excerpt,
            &self.cancel,
            self.hold,
        )
        .await?;
        let reason = match held {
            Held::Continue => return Ok(Step::Go),
            Held::TakenOver => "take_over",
            Held::TimedOut => "pause_timed_out",
        };
        if end_item {
            self.set_item("manual_required", Some(reason)).await?;
        }
        Ok(Step::Stopped)
    }

    /// A field a person must fill. After Continue the call is not forwarded: the person did
    /// it in the browser.
    async fn by_person(&mut self, kind: PauseKind, label: &str) -> Result<Value> {
        Ok(match self.pause(kind, "guard", label, true).await? {
            Step::Go => said_text(HANDLED),
            Step::Stopped => said_text(STOPPED),
        })
    }

    async fn browser(&mut self, name: &str, args: Value) -> Result<Value> {
        self.ready().await?;
        if !self.current().await? {
            return Ok(refusal("call_apply_next_first"));
        }
        if !BROWSER_TOOLS.contains(&name) {
            return Ok(refusal("tool_not_allowed"));
        }
        if let Some(why) = guard::refused_args(name, &args, cfg!(debug_assertions)) {
            return Ok(refusal(why));
        }
        let current = self.current.as_ref().expect("checked above");
        let last = current.last.clone();
        let brief = current.brief.clone();
        let allowed = current.allowed.clone();
        let clicks = current.item.submit_clicks;
        let item_id = current.item.id.clone();

        if guard::targets_in(name, &args)
            .iter()
            .any(|target| !guard::target_known(&last, target))
        {
            return Ok(refusal("snapshot_first"));
        }
        if name == "browser_file_upload" {
            let paths: Option<Vec<String>> = match args.get("paths") {
                None | Some(Value::Null) => Some(Vec::new()),
                Some(Value::Array(paths)) => paths
                    .iter()
                    .map(|path| path.as_str().map(str::to_owned))
                    .collect(),
                Some(_) => None,
            };
            if !paths.is_some_and(|paths| guard::upload_ok(&paths, &self.files())) {
                return Ok(refusal("upload_refused"));
            }
        }
        if guard::values_in(name, &args)
            .iter()
            .any(|value| !guard::value_ok(value, &allowed))
        {
            return Ok(refusal(VALUE_REFUSED));
        }
        // The page's role decides, never the type the session declares: every field passes
        // `field_guard`, and a choice also passes `choice_guard`. A value the call selects is
        // itself judged as an option.
        let picked = picks_options(name, &args);
        for (target, value) in guard::fields_in(name, &args) {
            let Some(checked) =
                judge_field(&last, &target, &value, picked.contains(&target), &brief)
            else {
                return Ok(refusal(UNLABELED));
            };
            if let Err(kind) = checked {
                let label = guard::label_of(&last, &target).unwrap_or_default();
                return self.by_person(kind, &label).await;
            }
        }
        if name == "browser_click" {
            let target = target_of(&args);
            if guard::is_choice(&last, &target)
                && let Err(kind) = guard::choice_guard(&last, &target, &brief)
            {
                let label = guard::label_of(&last, &target).unwrap_or_default();
                return self.by_person(kind, &label).await;
            }
            if guard::submit_target(&last, &target) {
                if clicks >= 1 {
                    let step = self
                        .pause(PauseKind::Other, "guard", SECOND_SUBMIT, true)
                        .await?;
                    if let Step::Stopped = step {
                        return Ok(said_text(STOPPED));
                    }
                }
                // Written before the click, so a crash mid-click still reads as a click.
                sqlx::query(
                    "UPDATE apply_items SET submit_clicks = submit_clicks + 1 WHERE id = ?",
                )
                .bind(&item_id)
                .execute(&self.db.pool)
                .await?;
                if let Some(current) = self.current.as_mut() {
                    current.item.submit_clicks += 1;
                }
            }
        }

        // Only text the session sends as typing; a choice is judged by its own guards.
        let typing: Vec<(String, String)> = match name {
            "browser_type" | "browser_fill_form" => guard::fields_in(name, &args),
            _ => Vec::new(),
        };
        let forwarded = match self.forward(name, args).await {
            Ok(forwarded) => forwarded,
            Err(error) => {
                // The page may have changed: refs from the old one must not be trusted.
                self.forget_page();
                return Err(error);
            }
        };
        if !forwarded.is_error
            && let Some(current) = self.current.as_mut()
        {
            current.typed.extend(typing);
        }
        sqlx::query("UPDATE apply_items SET tool_calls = tool_calls + 1 WHERE id = ?")
            .bind(&item_id)
            .execute(&self.db.pool)
            .await?;
        let is_snapshot = name == "browser_snapshot" && !forwarded.is_error;
        if let Step::Stopped = self
            .watch(forwarded.text.clone(), is_snapshot, true)
            .await?
        {
            return Ok(said_text(STOPPED));
        }
        let text = match (&self.current, is_snapshot) {
            (Some(current), true) => current.last.clone(),
            _ => forwarded.text,
        };
        Ok(tool_text(&wrap(&text), forwarded.is_error))
    }

    /// The next targeted call needs a fresh snapshot.
    fn forget_page(&mut self) {
        if let Some(current) = self.current.as_mut() {
            current.last.clear();
        }
    }

    async fn forward(&mut self, name: &str, args: Value) -> Result<ToolReply> {
        let cancel = self.cancel.clone();
        let client = self.client().await?;
        client.call(name, args, &cancel).await
    }

    /// The session's own snapshot. A refusal (an open file chooser or dialog) or a lost
    /// browser is `None`: the next action scans.
    async fn snapshot(&mut self) -> Result<Option<String>> {
        let cancel = self.cancel.clone();
        let Ok(client) = self.client().await else {
            return Ok(None);
        };
        match client.call("browser_snapshot", json!({}), &cancel).await {
            Ok(seen) if !seen.is_error => Ok(Some(seen.text)),
            Err(Error::Cancelled) => Err(Error::Cancelled),
            Ok(_) | Err(_) => Ok(None),
        }
    }

    /// Scans `page`, then the session's own snapshot, until one shows no stop a person has
    /// not answered. Each Continue acknowledges that line and looks again.
    async fn watch(
        &mut self,
        mut page: String,
        mut is_snapshot: bool,
        end_item: bool,
    ) -> Result<Step> {
        loop {
            let Some(current) = self.current.as_mut() else {
                return Ok(Step::Stopped);
            };
            if is_snapshot {
                current.last = page.clone();
            }
            match guard::scan(&page, &current.acks, &current.typed) {
                Some(hit) => {
                    let step = self.pause(hit.kind, "page", &hit.line, end_item).await?;
                    if let Step::Stopped = step {
                        return Ok(Step::Stopped);
                    }
                    if let Some(current) = self.current.as_mut() {
                        current.acks.insert(guard::hit_key(&hit));
                    }
                }
                None if is_snapshot => return Ok(Step::Go),
                None => {}
            }
            match self.snapshot().await? {
                Some(next) => {
                    page = next;
                    is_snapshot = true;
                }
                None => {
                    self.forget_page();
                    return Ok(Step::Go);
                }
            }
        }
    }

    async fn next(&mut self) -> Result<Value> {
        if self.current().await? {
            return Ok(refusal("finish_this_application_first"));
        }
        pause::ensure_off(self.db).await?;
        loop {
            let run = super::view(self.db, &self.run_id).await?;
            if run.state == "ended" {
                return Ok(said(json!({"done": true})));
            }
            if super::run_cancelled(self.db, &self.run_id).await? {
                return Err(Error::Cancelled);
            }
            let Some(item) = run.items.into_iter().find(|item| item.state == "queued") else {
                sqlx::query(concat!(
                    "UPDATE apply_runs SET state = 'ended', ended_at = coalesce(ended_at, ",
                    now!(),
                    ") WHERE id = ?"
                ))
                .bind(&self.run_id)
                .execute(&self.db.pool)
                .await?;
                return Ok(said(json!({"done": true})));
            };
            match self.prepare(&item).await? {
                Err(reason) => {
                    sqlx::query(
                        "UPDATE apply_items SET state = 'skipped', reason = ? \
                         WHERE id = ? AND state = 'queued'",
                    )
                    .bind(reason)
                    .bind(&item.id)
                    .execute(&self.db.pool)
                    .await?;
                }
                Ok(brief) => {
                    sqlx::query(
                        "UPDATE apply_items SET state = 'active' WHERE id = ? AND state = 'queued'",
                    )
                    .bind(&item.id)
                    .execute(&self.db.pool)
                    .await?;
                    let text = serde_json::to_string(&brief)
                        .map_err(|error| Error::Validation(error.to_string()))?;
                    self.current = Some(Current {
                        allowed: brief::allowed_values(&brief),
                        item: Item {
                            state: "active".into(),
                            ..item
                        },
                        brief,
                        last: String::new(),
                        acks: HashSet::new(),
                        typed: guard::Typed::new(),
                    });
                    return Ok(said_text(&text));
                }
            }
        }
    }

    /// The brief, with its files staged, or the reason the item is skipped.
    async fn prepare(&self, item: &Item) -> Result<std::result::Result<Brief, String>> {
        let Some(application) = item.application_id.as_deref() else {
            return Ok(Err("application_removed".into()));
        };
        let app = match applications::get(self.db, application).await {
            Ok(app) => app,
            Err(Error::NotFound) => return Ok(Err("application_removed".into())),
            Err(error) => return Err(error),
        };
        let Ok(site) = permissions::site_of(&app.link) else {
            return Ok(Err("link_not_https".into()));
        };
        let grant = Grant {
            owner_started: true,
            authorized: false,
        };
        if let Decision::Refuse(reason) =
            permissions::decide(Mode::ClaudeCode, site, Action::Submit, &grant)
        {
            return Ok(Err(reason.into()));
        }
        match brief::build(self.db, &self.actor, item, &self.files()).await {
            Ok(brief) => Ok(Ok(brief)),
            Err(Error::Refused(reason)) => Ok(Err(reason)),
            Err(Error::NotFound | Error::Validation(_)) => Ok(Err("application_removed".into())),
            Err(error) => Err(error),
        }
    }

    async fn ask(&mut self, args: &Value) -> Result<Value> {
        if !self.current().await? {
            return Ok(refusal("call_apply_next_first"));
        }
        let text = |key: &str| {
            args.get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("")
                .to_owned()
        };
        let note = text("note");
        let (kind, excerpt) = match args.get("kind").and_then(Value::as_str).unwrap_or("") {
            "login" => (PauseKind::Login, note),
            "captcha" => (PauseKind::Captcha, note),
            "mfa" => (PauseKind::Mfa, note),
            "identity" => (PauseKind::Identity, note),
            "legal" => (PauseKind::Legal, note),
            "other" => (PauseKind::Other, note),
            "no_answer" => {
                let question = text("question");
                if question.is_empty() {
                    return Ok(refusal("question_required"));
                }
                let country = self
                    .current
                    .as_ref()
                    .map(|current| current.brief.country.clone())
                    .filter(|country| !country.is_empty());
                let stored =
                    match memory::add_form_question(self.db, &question, country.as_deref()).await {
                        Ok(stored) => stored,
                        Err(Error::Validation(why)) => return Ok(refusal(&why)),
                        Err(error) => return Err(error),
                    };
                let kind = match stored.class {
                    Class::Sensitive | Class::WorkAuthorization => PauseKind::Sensitive,
                    Class::Normal | Class::Legal => PauseKind::Other,
                };
                (kind, question)
            }
            _ => return Ok(refusal("bad_kind")),
        };
        Ok(match self.pause(kind, "model", &excerpt, true).await? {
            Step::Go => said(json!({"resolution": "continue"})),
            Step::Stopped => said(json!({"resolution": "skip"})),
        })
    }

    async fn result(&mut self, args: &Value) -> Result<Value> {
        if !self.current().await? {
            return Ok(refusal("call_apply_next_first"));
        }
        let outcome = args.get("outcome").and_then(Value::as_str).unwrap_or("");
        if !matches!(outcome, "submitted" | "skipped" | "failed") {
            return Ok(refusal("bad_outcome"));
        }
        let clicks = self
            .current
            .as_ref()
            .map_or(0, |current| current.item.submit_clicks);
        let state = match outcome {
            "submitted" if clicks >= 1 => return self.submitted().await,
            "submitted" => {
                self.set_item("uncertain", Some("no_submit_seen")).await?;
                "uncertain"
            }
            _ => {
                let state = if clicks > 0 { "uncertain" } else { outcome };
                self.set_item(state, reason_code(args)).await?;
                state
            }
        };
        self.settle().await?;
        Ok(said(json!({"recorded": state})))
    }

    /// Records the page as the page's claim, tracks it, then scans the capture. No ending of
    /// that last hold changes the submitted item.
    async fn submitted(&mut self) -> Result<Value> {
        let Some(item) = self.current.as_ref().map(|current| current.item.clone()) else {
            return Ok(refusal("call_apply_next_first"));
        };
        // Under the emergency pause no browser call goes out: the click stands as recorded.
        let paused = pause::ensure_off(self.db).await.is_err();
        let capture = match paused {
            true => None,
            false => self.snapshot().await?,
        };
        let by = super::Submitter {
            agent: "claude_apply",
            mode: "claude_code",
        };
        super::record_submitted(
            self.db,
            &self.actor,
            &self.run_id,
            &item,
            capture.as_deref(),
            by,
        )
        .await?;
        if paused {
            self.current = None;
            return Err(Error::Paused);
        }
        let settled = self.settle().await;
        let step = match capture {
            Some(page) => self.watch(page, true, false).await.map(|_| ()),
            None => Ok(()),
        };
        self.current = None;
        settled?;
        step?;
        Ok(said(json!({"recorded": "submitted"})))
    }

    /// While the emergency pause is on, or when a status cannot move, the status stays
    /// unwritten for `apply settle`. Only a database failure stops the session.
    async fn settle(&self) -> Result<()> {
        match super::settle(self.db, &self.actor, &self.run_id).await {
            Err(error @ Error::Db(_)) => Err(error),
            Ok(_) | Err(_) => Ok(()),
        }
    }
}

fn said_text(text: &str) -> Value {
    tool_text(text, false)
}
