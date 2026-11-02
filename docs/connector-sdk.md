# Connector SDK

Status: built in release 0.5. The code is in `crates/core/src/connectors`. This meets FR-13.5.

A connector adds a job source. It is a declaration and two pure functions. It opens no socket, starts no process, and writes nothing. One fetcher (`connectors/fetch.rs`) sends every request, and a gate decides whether a request may go.

Connectors must not use unauthorized scraping or automation, CAPTCHA or MFA bypass, or access-control circumvention. Release 0.5 has no connector that fills a form or submits an application. The `Automation` type has no variant for either.

## The four declarations

The master spec (section 5) requires each connector to declare four things. `ConnectorDecl` holds them, plus the names the code needs.

| Declaration | Field |
|---|---|
| Supported operations | `operations` |
| Permitted automation | `automation` |
| Rate limits | `rate_limit` |
| Policy-review status | `policy`, checked against the register row named by `read_row` |

## `ConnectorDecl`, field by field

| Field | Type | Meaning |
|---|---|---|
| `name` | `&'static str` | Stable id, such as `lever`. Stored with each source and job. |
| `label` | `&'static str` | Name shown in the Jobs screen. |
| `operations` | `&[Operation]` | `ListBoard` (a company's board), `ListFeed` (a public feed), or `ImportPasted` (a person pastes text or a link). The first two read; the last makes no request. |
| `automation` | `&[Automation]` | `ReadPublic` or `UserAssisted`. Nothing else exists in 0.5. |
| `rate_limit` | `Option<RateLimit>` | `min_interval` and `per_day`. Required for a read, `None` for an import-only source. |
| `policy` | `PolicyStatus` | `Allowed`, `SilentRead`, `UserAssisted`, `Blocked`, or `Unknown`. What the connector expects its register row to say. |
| `read_row` | `Option<(&str, &str)>` | Integration and Operation cells of the register row that governs the read. |
| `assisted_row` | `(&str, &str)` | The row for the person applying by hand. It must read `user-assisted`. |
| `hosts` | `&[&str]` | The only hosts a request may name. Empty only for an import-only source. |
| `attribution` | `Option<&str>` | Source name shown on each card, when the vendor asks for it. |
| `target` | `TargetKind` | What the person names: a `Board`, a `Feed`, or `None`. |

The register is `include_str!`'d, so a verdict is fixed when the app is built. No setting reaches it.

## The two pure functions

`Connector::request(&Target, page) -> PageRequest` builds the request: a host from `hosts`, a path, and the body kind it expects (`Json` or `Rss`). It checks the board or feed name first. `clean_slug` accepts letters, digits, `-` and `_`, up to 100 characters, so a name cannot leave its path.

`Connector::parse(&Target, &[u8]) -> Page` turns a body into `Posting` rows, a `next` page number, and a `skipped` count. A row that lacks a field the connector needs is skipped and counted, never guessed. A reply that is not empty but yields no usable row is a changed shape and fails as `bad_feed`. A connector whose paging needs the page it just read overrides `parse_page`. `link_hosts` names the hosts a stored link may use, when the vendor serves postings from known ones.

The fetcher runs `vet` on every parsed page, so no connector has to. `vet` skips a row whose link is not https, names a user or a port, or sits off `link_hosts`. It skips a title over 300 characters or an id over 200, keeps the first of a repeated id, makes a negative or empty pay range unknown, cuts company and location at 200 characters, keeps 50 tags of 80 characters, and cuts the description at 200 KB.

## The gate

`gate(decl, operation)` passes only an operation the connector declares, and only when the register row reads `allowed` or `silent-read`. It checks the operation first, then the status. The refusals are:

| Code | When |
|---|---|
| `operation_not_declared` | The connector does not declare the operation. Every user-assisted source gives this for a read. |
| `policy_unknown` | The status is `unknown`, or the declaration and the row disagree. |
| `policy_blocked` | The row is `blocked`. |
| `policy_user_assisted` | The row is `user-assisted`. |

`ImportPasted` passes on any connector that declares it, since it makes no request.

The fetcher goes through these steps for each attempt, and stops at the first refusal: the emergency pause, the gate, the declared host, the stored rate limit, then the request. A refusal is final. There is no retry, no other route, and no other client. The request carries one honest `User-Agent` (`JobPilot/<version>`), no cookie, no proxy, and follows no redirect.

## How `check` refuses a declaration

`check(decl)` runs for every connector in `all()`, and `every_registered_connector_passes_check` runs it. It returns a validation error when:

- the name or label is empty, or there are no operations;
- the assisted row is missing or is not `user-assisted`;
- a read declares no host;
- a read also declares `UserAssisted`, or lacks `ReadPublic`;
- a read has no `read_row`;
- a read has no rate limit, a zero interval, or a zero daily count.

## Rate-limit rules

- The limit is stored per connector and host in `connector_limits`: counters and times only, never text. The GUI, the CLI, and a restart share it.
- A slot is taken by one conditional `UPDATE`, so two handles that race for a slot cannot both pass.
- A wait of 30 seconds or less is slept. A longer wait is refused as `RateLimited`, with the true time to try again.
- The daily count resets at UTC midnight.
- A 429 or 503 with a `Retry-After` of 30 seconds or less is stored, slept, and tried once more. A longer one is stored and refused. With no usable header, the host is held for an hour. `Retry-After` is capped at 24 hours.
- A challenge page, a `cf-mitigated` header, a 401, 403, 407, 451, or a redirect stops the source for 24 hours as `source_blocked`. A person cannot override it.
- A 500-class reply or a connection failure gets one more attempt, never a third.
- A body over `MAX_FEED_BYTES` (16 MiB) stores nothing.

## A worked connector

This is Lever, cut down. The full file is `connectors/lever.rs`.

```rust
static DECL: ConnectorDecl = ConnectorDecl {
    name: "lever",
    label: "Lever",
    operations: &[Operation::ListBoard, Operation::ImportPasted],
    automation: &[Automation::ReadPublic],
    rate_limit: Some(RateLimit { min_interval: Duration::from_secs(2), per_day: 300 }),
    policy: PolicyStatus::Allowed,
    read_row: Some(("Lever", "Read public listings through Postings API")),
    assisted_row: ("Lever", "User applies on the hosted form in their own browser"),
    hosts: &["api.lever.co", "api.eu.lever.co"],
    attribution: None,
    target: TargetKind::Board,
};

impl Connector for Lever {
    fn decl(&self) -> &'static ConnectorDecl { &DECL }
    fn request(&self, target: &Target, page: u32) -> Result<PageRequest> { /* clean_slug, then host and path */ }
    fn parse(&self, target: &Target, body: &[u8]) -> Result<Page> { /* rows to Posting, skipped count */ }
}
```

To add a source:

1. Read the vendor's terms. Add a register row for each operation in `docs/policy-register.md`, with the primary source, the date read, and a short quote. The verdict comes from the source, not from what is convenient.
2. Add the module, declare it, and list it in `all()`.
3. Add a synthetic fixture and parser tests (see the fixture rule below).
4. Run the connector tests. A declaration that disagrees with its row reads as `unknown` and cannot fetch.

## The fixture rule

Fixtures live in `crates/core/tests/fixtures/connectors`. They hold synthetic text in the shape a source sends. They never hold a real company's posting. Each source fixture (Lever, RemoteOK, We Work Remotely, Greenhouse, Ashby) has one hidden `display:none` line, so the injection tests can prove the line never reaches a prompt. No test or CI step reaches a real job source: the fetcher's test constructor accepts loopback addresses only, and `only_fetch_rs_touches_the_network` in `connector_rules.rs` fails if any other source file names a network type.

## The `silent-read` status

`silent-read` is for a documented, keyless, read-only public listing API whose vendor says nothing about third-party readers. It passes the gate. It is never reported as `allowed`, and the Jobs screen shows the note "Vendor is silent on third-party readers. Read under ruling R1."

**Ruling R1 was made by the controller while planning release 0.5. It is not an owner decision.** The owner has not been asked and can overturn it. It covers exactly two rows: Greenhouse and Ashby. See the [policy register](policy-register.md#controller-ruling-r1-2026-11-02-read-two-public-job-board-apis-while-the-vendor-is-silent).

To turn an R1 read off, change two lines. Either one alone is enough, because a declaration that disagrees with its row reads as `unknown`:

1. In `docs/policy-register.md`, set the row's verdict cell from `silent-read` to `unknown`.
2. In the connector's declaration (`greenhouse.rs` or `ashby.rs`), set `policy: PolicyStatus::Unknown`.

`a_row_set_back_to_unknown_refuses_both` in `crates/core/tests/connectors.rs` pins the register half.

## Planned and held sources

Company career pages, Workday, Indeed, and LinkedIn have declarations that only import a paste. They make no request. Their read rows stay `unknown` or `blocked`, and the gate refuses a read on them with `operation_not_declared`.
