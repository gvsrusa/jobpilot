# JobPilot AI: UI Wireframes

Status: draft for owner approval. Date: 2026-11-01. Source: [master spec](superpowers/specs/2026-11-01-jobpilot-master-spec.md) section 9 (UI wireframes), [PRD](prd.md), [milestones](milestones.md), [architecture](architecture.md#64-appsdesktopsrc-react-frontend), [policy register](policy-register.md), [AI providers](ai-providers.md), [agents](agents.md), and [threat model](security/threat-model.md). Requirement IDs (`FR-<section>.<n>`) come from the PRD. Nothing here is implemented yet.

These are layout sketches, not visual design. They fix what each screen shows, what it refuses to show, and how a keyboard user moves through it. Colours, type, and spacing belong to the shadcn/ui theme.

## 1. Conventions

### Release map

Only the first five screens (shell, recovery, status, documents, data) belong to release 0.1. They match plan Task B8: three views (`status`, `documents`, `data`) plus the recovery screen. Every later screen is a proposal for the release named, and a release plan may change it.

| # | Screen | Release | Requirements |
|---|---|---|---|
| 2 | App shell and navigation | 0.1 (grows each release) | FR-1.2, FR-1.5, FR-1.6 |
| 3 | Recovery | **0.1** (locked-fields panel on Data added 0.2) | FR-2.5, FR-2.6 |
| 4 | Status | **0.1** | FR-2.1, FR-2.3 |
| 5 | Documents | **0.1** | FR-2.2, FR-2.3, FR-2.11, FR-3.21, FR-3.23 |
| 6 | Data and privacy | **0.1** (key banner added 0.2) | FR-2.6 to FR-2.9, FR-2.8, FR-12.7, FR-12.9, FR-12.10 |
| 7 | Candidate profile | 0.2 | FR-4.1 to FR-4.10 |
| 8 | Job list | 0.5 | FR-5.1, FR-5.11 to FR-5.14, FR-6.3, FR-6.11 |
| 9 | Job detail with score explanation | 0.5 | FR-6.1, FR-6.4 to FR-6.10 |
| 10 | Resume compare with evidence | 0.2 | FR-7.9 to FR-7.12 |
| 11 | Application review and pause state | 0.6 | FR-9.2 to FR-9.6, FR-9.16 to FR-9.21 |
| 12 | Terminal panel | 0.2 | FR-10.1, FR-10.2, FR-12.5 |
| 13 | AI provider consent | 0.4 | FR-3.12, FR-3.30 |

### Rules every screen follows

- **Policy reality, not aspiration.** Screens show what the [policy register](policy-register.md) allows today. `unknown` behaves as `blocked`. A control that the register forbids is shown disabled with its reason, or is absent when it must not exist (a provider enable switch, a Claude login button). No setting overrides a verdict.
- **Secrets are never displayed.** Entry fields for secrets are write-only and cleared after save. The list shows names only (FR-12.2, M-03). There is no reveal button anywhere.
- **Untrusted text is inert.** Job, company, and page text renders as plain text with no HTML, no remote images, and no live links (M-04, FR-12.3). It carries an "untrusted" label.
- **Authorization values are user-entered only.** No screen pre-fills, suggests, defaults, infers, or autocompletes work authorization or sponsorship (FR-4.9, FR-4.10). The empty state is "Not stated".
- **Errors** appear in an element with `role="alert"` and show the stable error `code` and the message from the IPC error `{code, message}`. Messages never echo a secret or file contents.
- **Every control has a visible label** and works by keyboard (plan Task B8). No remote fonts or assets.
- **Offline.** No screen in 0.1 needs a network, and 0.1 code makes no network call.

### Keyboard model shared by all screens

| Key | Action |
|---|---|
| Tab, Shift+Tab | Next, previous control in the focus order listed per screen |
| F6, Shift+F6 | Cycle regions: navigation, main view, bottom panel (when open) |
| Ctrl+1 to Ctrl+3 (Cmd on macOS) | Open Status, Documents, Data. Later releases append numbers. |
| Esc | Close the open dialog and return focus to the control that opened it |
| Enter, Space | Activate the focused button or toggle |
| Arrow keys | Move within a list, table, or radio group; Tab leaves the group |

Dialogs trap focus, open with focus on the least destructive control, and restore focus on close. Focus order is the DOM order, left to right then top to bottom, unless stated.

### Notation

`[Button]` is a button. `[ ]` is a text input. `(x)` and `( )` are selected and unselected radios. `[x]` and `[ ]` in a list are checkboxes. `{...}` is dynamic text. `~~~` marks a disabled control. Boxes are plain ASCII, at most 80 columns.

## 2. App shell and navigation (release 0.1)

The shell is a left navigation and one main view. Zustand holds only the current view (`status`, `documents`, `data`). TanStack Query holds all server state. In recovery state the navigation is hidden (see screen 3).

```text
+------------------------------------------------------------------------------+
| JobPilot AI                                                           v0.1.0 |
+-------------+----------------------------------------------------------------+
| Status      |                                                                |
| Documents   |   Main view: one of Status, Documents, Data                    |
| Data        |                                                                |
|             |   Errors from IPC appear here, in an alert, above the view.    |
|             |                                                                |
|             |                                                                |
|             |                                                                |
+-------------+----------------------------------------------------------------+
| Local only. No account. No network.                    Data: {data root}     |
+------------------------------------------------------------------------------+
```

Later releases extend the navigation. Each entry appears only in the release that ships it, and nothing is greyed out in advance.

```text
 Navigation entry   Release   Screen
 -----------------  -------   -----------------------------------------
 Status             0.1       4
 Documents          0.1       5
 Data               0.1       6
 Profile            0.2       7
 Resumes            0.2       10 (resume compare)
 Applications       0.3       history; review screen 11 joins in 0.6
 AI                 0.4       13 (providers and consent)
 Jobs               0.5       8, 9

 Bottom panel: Terminal 0.2 (screen 12)
 Title bar: Emergency pause 0.3, visible on every screen
```

- **Release:** 0.1 for three entries and the footer. The other entries are marked above.
- **Keyboard:** Ctrl/Cmd+1 to 3 switch views. Up and Down move within the navigation list. Enter opens the entry. F6 moves between navigation and main view.
- **Focus order:** navigation list, main view in its own order, footer text (not focusable). On view change, focus moves to the main view heading.
- **Empty state:** not applicable. The shell always has at least three entries.
- **Error state:** IPC errors that are not tied to one control show in a page-level alert at the top of the main view. A second app launch focuses this window (single-instance plugin) and shows no error.

## 3. Recovery screen (release 0.1)

Shown instead of the shell when the database failed to open at startup. The navigation is hidden, because data commands return `unavailable` in this state. `status` and `retry_open` always work. The data root path is always shown, so the user knows which folder to inspect or back up by hand.

```text
+------------------------------------------------------------------------------+
| JobPilot AI could not open your data                                         |
+------------------------------------------------------------------------------+
| ! {message from core}                                        code: {code}    |
|                                                                              |
| Data folder (nothing in it has been changed or deleted by this screen):      |
|   {data root, full path, selectable text}                      [Copy path]   |
|                                                                              |
| What happened and what to do                                                 |
|   {one block, chosen by code, see below}                                     |
|                                                                              |
|   [Try again]      [Restore from a backup...]      [Quit]                    |
+------------------------------------------------------------------------------+
```

Text block by error code. The code is always shown, so the user can report it.

| Code | Block shown | Buttons |
|---|---|---|
| `schema_too_new` | "This data was written by a newer JobPilot (data schema {db}, this app understands {app}). Nothing was changed. Install a newer JobPilot to open it. Restoring an older backup replaces your current data." | Try again enabled but explained as useful only after an update. Restore enabled with warning. |
| `in_use` | "Another JobPilot window or the `jobpilot` command is using this data. Close it, then try again. Nothing was changed." | Try again is the default action. Restore is disabled, because restore is refused while another process holds the data (ADR 4). |
| Migration failure (code from the core error for a failed migration; shown verbatim) | "Updating your data to this version failed. Your data from before the update is kept as a snapshot named `pre-migrate-{from}-{stamp}` in {data root}/backups. Nothing was deleted." | Try again enabled. Restore opens with the `pre-migrate` snapshot listed first. |
| Any other code | The core message, the code, and "Nothing was changed by this screen." | Try again and Restore enabled. |

Restore opens the backup picker, then the restore warning from the Data screen (screen 6), including the `pre-restore` snapshot notice. Restore here is the same command as on the Data screen.

Release 0.2 adds a Locked fields panel. It sits at the top of the Data screen (with a link from Status), not on this blocking screen, because key problems do not stop the app: it starts normally and sensitive fields show as locked. The states come from the threat model, section 9.

```text
+------------------------------------------------------------------------------+
| Locked fields (0.2)                            key id: {key_id}              |
+------------------------------------------------------------------------------+
| key_unavailable  Your keychain did not answer. Unlock it, then retry.        |
|                  No import and no discard are offered.      [Retry]          |
| key_missing      The key for this data is not on this machine.               |
| key_mismatch     The key found does not match this data.                     |
|   (both)         [Import recovery key file...]  [Discard locked fields...]   |
+------------------------------------------------------------------------------+
```

- Import and discard go through a Rust-owned native dialog, not a webview form. Discard names the key id, writes a `pre-discard` snapshot first, and is irreversible for the live data (threat model section 9).
- **Release:** 0.1. The key-state panel is 0.2.
- **Keyboard:** Tab through the path copy button, then Try again, Restore, Quit. The heading takes initial focus so a screen reader reads the problem first. Enter activates. Esc does nothing (there is nowhere to go back to).
- **Focus order:** heading, alert text, Copy path, Try again, Restore from a backup, Quit.
- **Empty state:** not applicable.
- **Error state:** a failed Try again keeps the screen and replaces the alert with the new code and message. A failed restore shows its error in an alert above the buttons, and live data is unchanged (a failure before the renames leaves live data as it was). Tests: `shows_recovery_screen`, `refuses_newer_schema`, `failed_migration_keeps_snapshot_and_data`.

## 4. Status (release 0.1)

```text
+-------------+----------------------------------------------------------------+
| Status      | Status                                                         |
| Documents   +----------------------------------------------------------------+
| Data        | Database          Ready                                        |
|             | Data schema       {schema_version}                             |
|             | App version       {app_version}                                |
|             | Data folder       {data root}                  [Copy path]     |
|             | Backups           {n} on disk, newest {date}                   |
|             |                                                                |
|             | Same data is used by the `jobpilot` command line tool.         |
+-------------+----------------------------------------------------------------+
```

- **Release:** 0.1. Later releases add rows (key state in 0.2, emergency pause in 0.3, register date in 0.4) without changing the layout.
- **Keyboard:** the page has one interactive control. Tab reaches Copy path. Ctrl/Cmd+1 opens this view.
- **Focus order:** heading, Copy path.
- **Empty state:** a fresh install shows schema version `1`, "0 backups", and the default data folder. `JOBPILOT_HOME`, when set, is shown as the data folder so a redirected root is visible (M-06).
- **Error state:** `status` never fails. If the database is not open, the recovery screen replaces this view.

## 5. Documents (release 0.1)

Local documents with full-text search (FTS5). Search is debounced by 200 ms and shows snippets. Document text is plain text, rendered inertly.

```text
+-------------+----------------------------------------------------------------+
| Status      | Documents                                       [New document] |
| Documents   +----------------------------------------------------------------+
| Data        | Search [                                  ]   {n} results      |
|             +--------------------------+-------------------------------------+
|             | Title            Updated | Title  [                          ] |
|             | > Resume 2026   Nov 01   | Body                                |
|             |   Cover notes   Oct 29   | [                                 ] |
|             |   Interview Qs  Oct 20   | [                                 ] |
|             |                          | [                                 ] |
|             |  (search hits show a     | Do not store visa status,           |
|             |   snippet under each     | citizenship, or salary here. This   |
|             |   title)                 | text is not encrypted.              |
|             |                          |                                     |
|             |                          | [Save]  [Delete...]                 |
+-------------+--------------------------+-------------------------------------+
```

The warning under the editor comes from M-09: 0.1 has no field encryption and cannot tell sensitive text from other text.

Delete asks for a plain confirmation naming the document title. It does not use the typed `DELETE` of delete-all.

- **Release:** 0.1.
- **Keyboard:** `/` focuses Search when no text field has focus. Up and Down move through the list, Enter opens the document in the editor, and Tab moves into the editor. Ctrl/Cmd+S saves. Esc in the delete dialog cancels.
- **Focus order:** New document, Search, document list (one tab stop, arrows inside), Title, Body, Save, Delete.
- **Empty state:** no documents: "No documents yet. Create one to keep text you can search offline." with [New document] focused. Search with no match: "No documents match '{query}'." and the list is empty.
- **Error state:** an IPC failure, such as `in_use` while a restore runs in the CLI, shows in an alert above the list with its code. The editor keeps its unsaved text. A save conflict is not possible in 0.1 (single user).

## 6. Data and privacy (release 0.1)

Backup, restore, export, delete-all, secrets, and recent audit rows, with the privacy warnings from M-09. In 0.1 destructive actions confirm inside the webview. From 0.2 they confirm through a Rust-owned native dialog (M-13), and the layout stays the same.

```text
+-------------+----------------------------------------------------------------+
| Status      | Data and privacy                                               |
| Documents   +----------------------------------------------------------------+
| Data        | Before you store anything                                      |
|             |  - Turn on disk encryption (FileVault, BitLocker).             |
|             |  - Keep sensitive details out of free-form documents.          |
|             |  - Exports are plain text.                                     |
|             |  - Do not put the data folder in a cloud-synced folder.        |
|             +----------------------------------------------------------------+
|             | Backups                                       [Create backup]  |
|             |  {label}-{stamp}   {size}   [Restore...]                       |
|             |  [Restore from another folder...]                              |
|             +----------------------------------------------------------------+
|             | Export                                                         |
|             |  Writes all your data as plain text to {data root}/exports.    |
|             |  [Export all data...]                                          |
|             +----------------------------------------------------------------+
|             | Secrets (values are never shown)                               |
|             |  Name [              ]  Value [**********]       [Save secret] |
|             |  api-key-example   saved Nov 01                  [Remove...]   |
|             +----------------------------------------------------------------+
|             | Recent activity                                                |
|             |  {time}  gui   backup.create   {target}                        |
|             +----------------------------------------------------------------+
|             | Delete all data                                                |
|             |  [Delete all data...]                                          |
+-------------+----------------------------------------------------------------+
```

Restore warning (dialog opened by Restore). Focus starts on Cancel.

```text
+------------------------------------------------------------------+
| Replace your current data with this backup?                      |
+------------------------------------------------------------------+
| Backup: {label}-{stamp}   schema {n}   made {date}               |
|                                                                  |
| This replaces everything now in JobPilot: documents, settings,   |
| secret names, and the activity log. A snapshot of your current   |
| data is saved first as pre-restore-{stamp}, so you can undo      |
| this by restoring that snapshot.                                 |
| A backup from an older version is updated to this version.       |
|                                                                  |
|                         [Cancel]   [Replace my data]             |
+------------------------------------------------------------------+
```

Delete-all dialog. The delete button stays disabled until the field equals the exact text `DELETE`. The IPC call carries `confirm: "DELETE"`.

```text
+------------------------------------------------------------------+
| Delete all JobPilot data?                                        |
+------------------------------------------------------------------+
| This permanently removes, from {data root}:                      |
|   the database, all documents, exports, the activity log, and    |
|   the secrets JobPilot stored in your system keychain.           |
| [ ] Also delete backups and snapshots                            |
|                                                                  |
| It cannot reach a backup or export you moved elsewhere.          |
| Deleting the activity log is part of this action, so no record   |
| of it remains.                                                   |
|                                                                  |
| Type DELETE to confirm:  [                    ]                  |
|                                                                  |
|                         [Cancel]   [Delete everything] ~~~       |
+------------------------------------------------------------------+
```

- From 0.2 the delete text adds: removing this folder's keychain entries also removes the key for any copied folder or restored backup that shares its key id, and cannot reach a recovery file you saved. A "No recovery key exported" banner shows at the top until the user exports one (threat model section 9).
- Secret handling: after Save secret the value field is cleared and the value appears nowhere in the page. Only the name remains. There is no read command (plan Task B7 and test `secret_ui_never_displays_values`).
- Export warns first: the file is plain text, including everything in `documents`.
- **Release:** 0.1. Native-dialog confirmation and the recovery-key banner are 0.2.
- **Keyboard:** Tab runs top to bottom through the sections. The backup list is one tab stop with arrows inside. In a dialog, Tab cycles inside it, Esc cancels. In the delete dialog, Enter on the field does nothing while the field is not `DELETE`, and activates Delete everything when it is.
- **Focus order:** Create backup, backup list (Restore per row), Restore from another folder, Export all data, secret Name, secret Value, Save secret, Remove (per row), activity list, Delete all data. Restore picker uses the single `dialog:allow-open` permission for choosing a folder.
- **Empty state:** no backups: "No backups yet. Create one before you change anything important." No secrets: "No secrets stored." No activity: "No activity yet."
- **Error state:** each action shows its error in an alert inside its own section: `in_use` ("Close the other JobPilot window or the `jobpilot` command, then try again."), `invalid_backup` ("This folder is not a valid JobPilot backup. Nothing was changed."), and a keychain failure for secrets without any value in the message. A failed restore or delete leaves live data as it was, and the screen reopens the database before returning.

## 7. Candidate profile (release 0.2)

Multi-valued location, currency, time zone, language, and work-mode fields (FR-4.1 to FR-4.8). Sensitive fields are encrypted at rest from 0.2 (FR-12.1) and show as locked when the key is unavailable (screen 3).

```text
+-------------+----------------------------------------------------------------+
| Status      | Profile                                          [Save]        |
| ...         +----------------------------------------------------------------+
| Profile     | Name  [                    ]   Email [                       ] |
|             | Countries      [Add country]   {Portugal x} {Canada x}         |
|             | Regions/cities [Add]           {Lisbon x} {Toronto x}          |
|             | Currencies     [Add]           {EUR x} {CAD x}                 |
|             | Time zones     [Add]           {Europe/Lisbon x}               |
|             | Languages      [Add]           {English x} {Portuguese x}      |
|             | Work mode      [x] Remote [x] Hybrid [ ] Onsite                |
|             | Employment     [Add, per country]  {Contract (PT) x}           |
|             +----------------------------------------------------------------+
|             | Work authorization and sponsorship                             |
|             | You enter this yourself. JobPilot never guesses it, never      |
|             | fills it in for you, and never works it out from your          |
|             | citizenship, location, or resume.                              |
|             |                                                                |
|             | Country        What you state          Needs sponsorship       |
|             | {Portugal}     [ Not stated      v ]   [ Not stated      v ]   |
|             | {Canada}       [ Not stated      v ]   [ Not stated      v ]   |
|             | Free note (yours, optional) [                              ]   |
|             | [Add a country]                                                |
+-------------+----------------------------------------------------------------+
```

- The two selects have `Not stated` as the only default and are never changed by another field. Choices are written by the user. There is no "suggested" label and no autofill. Anything the screen shows here is what the user typed.
- A country added under Countries starts a new row here with `Not stated`.
- **Release:** 0.2.
- **Keyboard:** Tab moves between fields. In a chip field, Enter on Add opens an input, Enter commits a chip, and Backspace on an empty input removes the last chip. Selects open with Space and move with arrows.
- **Focus order:** Save, Name, Email, each chip field in the order shown, work-mode checkboxes, then the authorization table row by row (country, statement, sponsorship), note, Add a country.
- **Empty state:** a new profile is empty. The authorization table is empty with the text "No country added yet." The profile can stay empty and the app still works.
- **Error state:** a field that fails validation shows an inline message tied to the field. A locked field (`key_unavailable`, `key_missing`, `key_mismatch`) shows "Locked, see Recovery" and cannot be edited. Save errors appear in an alert above the form.

## 8. Job list (release 0.5)

Jobs come only from connectors whose register read verdict is `allowed` (Lever, RemoteOK, We Work Remotely today) or from user-assisted import. The register verdict is shown, so the user sees why a source has no connector.

```text
+-------------+----------------------------------------------------------------+
| ...         | Jobs                                [Import a posting...]      |
| Jobs        +----------------------------------------------------------------+
|             | Filter: Country [ v ] Mode [ v ] Min salary [    ] [Apply]     |
|             | Filters are exact rules. Score uses your stated preferences.   |
|             +----------------------------------------------------------------+
|             | Score  Title / Company          Where       Source             |
|             |  82    Backend Engineer, Acme   Remote, PT  RemoteOK >        ||
|             |  74    Platform Dev, Beta       Lisbon       Lever             |
|             |  --    Data Eng, Gamma          Toronto      Imported          |
|             |        (no score: nothing in your profile to compare)          |
|             +----------------------------------------------------------------+
|             | Sources                                                        |
|             |  Lever          read: allowed        [Fetch: {company site}]   |
|             |  RemoteOK       read: allowed        attribution shown         |
|             |  We Work Remotely  read: allowed (RSS)                         |
|             |  Greenhouse     read: unknown   No connector. Import instead.  |
|             |  Ashby          read: unknown   No connector. Import instead.  |
|             |  Workday        read: unknown   No connector. Import instead.  |
|             |  Indeed         read: blocked   No connector. Import instead.  |
|             |  LinkedIn       read: unknown   User-assisted only.            |
+-------------+----------------------------------------------------------------+
```

- Rows for sources without an `allowed` read verdict have no fetch button, only the import route. Attribution ("Remote OK", "We Work Remotely") shows on every card from those sources. Job text is labelled untrusted.
- **Release:** 0.5.
- **Keyboard:** Tab to the filter bar, then the job table (one tab stop, arrows move rows, Enter opens screen 9, `i` opens import). Home and End jump to the first and last row.
- **Focus order:** Import a posting, filter controls, Apply, job table, Sources list (Fetch buttons only where they exist).
- **Empty state:** "No jobs yet. Import a posting, or fetch from a source marked allowed." The Sources list is still shown.
- **Error state:** a fetch that hits a rate limit or a refusal shows the reason in an alert on that source row ("Refused by policy: unknown"). A source never retries beyond its declared limit.

## 9. Job detail with score explanation (release 0.5)

```text
+-------------+----------------------------------------------------------------+
| ...         | < Jobs   Backend Engineer, Acme              Score 82 / 100    |
| Jobs        +----------------------------------------------------------------+
|             | Source: RemoteOK (link)   Untrusted text. Shown as plain text. |
|             +-----------------------------+----------------------------------+
|             | Why this score              | Posting text                     |
|             |  Criterion   Wt  Pts        | {plain text, no HTML, no         |
|             |  Skills      40  34         |  images, no live links}          |
|             |  Seniority   20  18         |                                  |
|             |  Location    20  20         |                                  |
|             |  Pay         20  10         |                                  |
|             |                             |                                  |
|             | Missing skills: Kubernetes  |                                  |
|             | Seniority: senior (posting) |                                  |
|             |   vs senior (your profile)  |                                  |
|             | Pay: posting 70-80k EUR,    |                                  |
|             |   you want 90k+ EUR         |                                  |
|             | Location: remote, PT ok     |                                  |
|             |                             |                                  |
|             | Work authorization:         |                                  |
|             |  Portugal: not stated by    |                                  |
|             |  you. Not checked.          |                                  |
|             |                             |                                  |
|             | Scored by: rules. [Add AI   |                                  |
|             | evaluation] (if enabled)    |                                  |
|             +-----------------------------+----------------------------------+
|             | [Add to tracker]  [Open posting in my browser]  [Prepare...]   |
+-------------+----------------------------------------------------------------+
```

- The explanation lists every criterion with its weight and the points it gave (FR-6.4, FR-6.10). Objective filters are rules (FR-6.11). Semantic evaluation runs only when an AI backend is enabled and consented (FR-6.12), and labels itself as such.
- Work authorization is only ever one of `not stated by you` or `you stated: {value} for {country}`. It is never shown as eligible or ineligible (FR-4.10, agents.md Matching).
- **Release:** 0.5.
- **Keyboard:** Alt+Left or the Back link returns to the list with the same row focused. Tab goes explanation first, then posting text (scrollable region, focusable, arrows scroll), then the action row.
- **Focus order:** Back, Score explanation table, Add AI evaluation (only if enabled), posting text, Add to tracker, Open posting in my browser, Prepare.
- **Empty state:** a job with no parsed fields shows "Could not read this posting's fields" and lets the user fix them by hand. Missing skills reads "None found" only when the knowledge base is not empty. With an empty profile it reads "Add your profile to see this."
- **Error state:** an AI evaluation that fails or is refused shows the router reason (consent missing, provider ineligible, validation failed) and the rules score stays visible. The score is never blank because of an AI failure.

## 10. Resume compare with evidence (release 0.2)

```text
+-------------+----------------------------------------------------------------+
| ...         | Resume: Backend Engineer, Acme       Version [ v3 (current) v] |
| Resumes     +----------------------------------------------------------------+
|             | Original (v1)               | Generated (v3)                   |
|             +-----------------------------+----------------------------------+
|             | Led a team of 4 on the      | Led a team of 4 building the     |
|             | billing service.            | billing service. [E1] OK         |
|             |                             |                                  |
|             | Reduced build time 30%.     | Reduced build time 30%. [E2] OK  |
|             |                             | Expert in Kubernetes. [--] NO    |
|             |                             |   EVIDENCE: remove or fix        |
|             +-----------------------------+----------------------------------+
|             | Evidence                                                       |
|             |  E1  Resume v1 line 7: "Led 4 engineers on billing"            |
|             |  E2  Document "Build notes": "cut CI time from 20 to 14 min"   |
|             +----------------------------------------------------------------+
|             | 1 claim has no evidence. Export is blocked until it is fixed.  |
|             | [Edit claim]  [Remove claim]  [Export DOCX]~~~  [Export PDF]~~~|
+-------------+----------------------------------------------------------------+
```

- Every sentence in the generated column carries an evidence tag that opens its source (FR-7.11). A claim with no evidence is flagged and blocks export (FR-7.12). Evidence counts only when it is user-entered or user-approved; saving a draft does not make it evidence.
- Versions are listed and never overwritten (FR-7.9). Comparison runs against any chosen earlier version (FR-7.10).
- **Release:** 0.2.
- **Keyboard:** the two columns scroll together. Up and Down move between claims, Enter opens the evidence panel for the focused claim, `e` edits it. Tab leaves the compare area for the buttons.
- **Focus order:** version select, claim list (one tab stop, arrows inside), evidence panel, Edit claim, Remove claim, Export DOCX, Export PDF.
- **Empty state:** no generated resume: "No generated resume for this job yet." with [Generate] if a backend is enabled, or [Open editor] if not (FR-3.23). No original: only the generated column shows.
- **Error state:** a generation that returns `manual_required` opens the editor with a note. A failed evidence check is shown as the flagged claim above, not as a generic error. Export errors show in an alert.

## 11. Application review and pause state (release 0.6)

The review screen shows one application before anything happens on the site. It works today in **Manual mode**, the only mode with an eligible register row for hosted forms. The mode choice shows each mode's status and the register reason when a mode is not available.

```text
+-------------+----------------------------------------------------------------+
| ...         | Review application: Platform Dev, Beta (Greenhouse form)       |
| Applications+----------------------------------------------------------------+
|             | Site rule (register 2026-11-01)                                |
|             |  Greenhouse hosted form: apply by hand = user-assisted         |
|             |                          fill by tool  = unknown               |
|             +----------------------------------------------------------------+
|             | How do you want to apply?                                      |
|             |  (x) Manual    You fill and submit in your own browser.        |
|             |                JobPilot opens the link, gives you files and    |
|             |                answers, and records what happened.             |
|             |  ( ) Copilot   ~~~ Not available                               |
|             |                Why: this site's Copilot rule is "unknown",     |
|             |                which is treated as blocked until the site      |
|             |                answers. Not changeable in settings.            |
|             |  ( ) Autonomous ~~~ Not available. Off by default.             |
|             |                Why: no site or connector qualifies. "unknown"  |
|             |                or "blocked" on every hosted form today.        |
|             +----------------------------------------------------------------+
|             | Material for this application                                  |
|             |  [x] Resume v3 (PDF)          [x] Cover letter v2 (PDF)        |
|             |  Answers you approved for this form:                           |
|             |   Notice period      approved, fresh      [Copy]               |
|             |   Salary expectation SENSITIVE: confirm   [Confirm and copy]   |
|             |   Work authorization  not stated by you   [Enter it yourself]  |
|             +----------------------------------------------------------------+
|             | [Open the form in my browser]   [Record outcome...]            |
+-------------+----------------------------------------------------------------+
```

Why a mode is unavailable comes from the site's register row and the mode rules. The text shown is fixed per verdict:

| Register verdict for that mode and site | Text under the disabled mode |
|---|---|
| `unknown` | "The site has not said whether tools may do this. Treated as blocked until it answers." |
| `blocked` | "The site's terms forbid this. JobPilot will not do it, even if you ask." (Indeed) |
| Autonomous with no eligible connector | "No site or connector qualifies today. Needs a site rule of allowed, verified permission, and your explicit authorization." |
| Autonomous with an OpenAI-origin model | "Not allowed with OpenAI models for submissions with no human review." |

Copilot and Autonomous become selectable only for a site with its own `allowed` row, or for local fixture forms during development (no real site is eligible today). No test or setting turns a disabled mode on for a real site.

### Pause state

When a Copilot or Autonomous run reaches a mandatory pause, the review screen is replaced by the pause panel. Today this appears only against local fixture forms, because no real site is eligible. The six pauses from the agent specification are listed so the user always sees the full set, with the active one marked.

```text
+-------------+----------------------------------------------------------------+
| ...         | Run paused: waiting for you                                    |
| Applications+----------------------------------------------------------------+
|             | Nothing has been submitted. The run waits 30 minutes, then     |
|             | stops and hands this over to you.                              |
|             +----------------------------------------------------------------+
|             | Mandatory pauses                                               |
|             |  [>] Legal declaration (FR-9.16)       ACTIVE                  |
|             |        "I certify that the information is true..."             |
|             |        Read it and answer it yourself in the browser.          |
|             |        JobPilot never answers this, including any question     |
|             |        about AI use or who wrote the material.                 |
|             |  [ ] Unverified sensitive information (FR-9.17)                |
|             |  [ ] Identity verification (FR-9.18)                           |
|             |  [ ] CAPTCHA (FR-9.19)                                         |
|             |  [ ] Multi-factor prompt (FR-9.20)                             |
|             |  [ ] Other step that needs a person (FR-9.21)                  |
|             |        Unmapped field, changed page, redirect off the site,    |
|             |        unexpected dialog, unclear submission result.           |
|             +----------------------------------------------------------------+
|             | Page text is shown as plain text and cannot end a pause.       |
|             | Only you can, with the buttons below.                          |
|             |                                                                |
|             | [I did this in the browser: continue]  [Take over by hand]     |
|             | [Cancel run]                                                   |
+-------------+----------------------------------------------------------------+
```

- No control on this panel answers a pause, fills a legal declaration, solves a CAPTCHA, or reads a code. Continue re-runs the permission checks first. After a pause, the form is reviewed again.
- If the app cannot tell whether a submission went through, the panel shows the "other step" pause with "Check the site. JobPilot will not try again."
- In Manual mode there is no run to pause. The sensitive rule still applies on the review screen: a sensitive answer shows `Confirm and copy`, never a plain copy (FR-8.8).
- Work authorization and sponsorship answers come only from what the user entered on the profile screen. If none exists the row says "not stated by you", and the button is `Enter it yourself`. There is no suggested value.
- The emergency pause control (0.3) is in the title bar on this screen too. When set, the run moves to Halted and this panel shows "Stopped by emergency pause. Clear it to resume or cancel."
- **Release:** 0.6 (Manual mode is FR-9.2, delivered with the 0.6 runtime). It uses the 0.3 application records.
- **Keyboard:** the mode radios are one group (arrows move, Space selects). Disabled modes can be focused so a screen reader reads the reason, but cannot be selected. Pause rows are read-only. Buttons at the bottom take Tab. `Esc` does not cancel the run; Cancel run is a button and asks to confirm.
- **Focus order:** site-rule block (read-only), mode radios (reasons read with each), material checkboxes, per-answer buttons, Open the form, Record outcome. In pause state: pause list (read-only, active row announced), Continue, Take over, Cancel run.
- **Empty state:** no resume or cover letter yet: "Nothing prepared. [Prepare documents]" and Open the form stays available. No approved answers: "No approved answers for this form. You can fill it by hand."
- **Error state:** a site with no register row at all shows as `unknown`, so Manual only. If the posting link is not `https`, Open the form is disabled with the reason. Any error shows in an alert with its code. A run that ends `manual_required` shows what to do next.

## 12. Terminal panel (release 0.2)

A bottom panel running xterm.js over a Rust PTY. It is a restricted terminal, not a general shell (M-14): in 0.2 it runs the bundled `jobpilot` CLI.

```text
+-------------+----------------------------------------------------------------+
| ...         | (main view)                                                    |
|             +----------------------------------------------------------------+
|             | Terminal   jobpilot CLI only        [Clear] [Close panel]      |
|             +----------------------------------------------------------------+
|             | $ jobpilot status                                              |
|             | schema 2, data folder ...                                      |
|             | $ jobpilot doc add "x" --body "y"                              |
|             | $ _                                                            |
|             +----------------------------------------------------------------+
|             | Runs the same services as the app. Output is not saved.        |
+-------------+----------------------------------------------------------------+
```

- Commands that need a native confirmation, and the recovery-key commands, are refused here: "Run this in an ordinary terminal, or use the app." The panel shows that line when the PTY layer rejects a command. No command prints a secret value.
- Scrollback is not stored and is never read into a workflow. Agents have no terminal access.
- **Release:** 0.2.
- **Keyboard:** Ctrl+` (Cmd+` on macOS is reserved by the OS, so Ctrl+` on both) toggles the panel and focuses it. Inside the terminal every key goes to the shell, including Tab and Esc. **F6 leaves the terminal** for the next region, so a keyboard user is never trapped. F6 is never sent to the shell.
- **Focus order:** main view, then (F6) navigation, then (F6) terminal, then Clear and Close panel. The terminal area is one tab stop, and the screen reader announces it as a terminal with the leave key.
- **Empty state:** an empty panel shows the prompt and "Type `jobpilot --help`."
- **Error state:** a failed PTY start shows an alert in the panel header with its code and a [Restart] button. A refused command prints the refusal line above and keeps the session. A rejected command never reaches a shell.

## 13. AI provider consent (release 0.4)

Providers are listed with the register verdict and the reason. An ineligible provider shows the reason and has **no enable control at all**: no switch, no key field, no sign-in button. No setting overrides a verdict. Verdicts and quoted reasons come from the [policy register](policy-register.md) at the date shown, and the screen shows that date.

```text
+-------------+----------------------------------------------------------------+
| ...         | AI providers                         Register date 2026-11-01  |
| AI          +----------------------------------------------------------------+
|             | No provider is needed. JobPilot works with none.               |
|             | Always on, always last: No-AI (rules, search, manual editing). |
|             +----------------------------------------------------------------+
|             | Ollama on this computer (localhost)         [x] On             |
|             |   Data stays on this machine. No consent needed.               |
|             | Your own local server (localhost address)   [Add endpoint...]  |
|             |   Eligible: runs on your machine. No presets.                  |
|             +----------------------------------------------------------------+
|             | Sign in with ChatGPT                        [Review...]        |
|             |   Allowed, if you are eligible (Plus or Pro). Preview.         |
|             |   Shown only if the owner adopts this route (decision D1).     |
|             +----------------------------------------------------------------+
|             | Not available                                                  |
|             |  OpenAI API key       unknown  The terms cover businesses.     |
|             |                       Waiting for OpenAI to say whether a      |
|             |                       person's key may be used here.           |
|             |  Anthropic API key    unknown  Same: "not for consumer use".   |
|             |  Google Gemini key    unknown (paid), blocked (free key:       |
|             |                       no personal data allowed).               |
|             |  OpenRouter           account allowed, but no model qualifies  |
|             |                       yet. Each model's own terms apply.       |
|             |  Claude Code          Claude sign-in is not offered. API-key   |
|             |                       mode unknown. Run it yourself in your    |
|             |                       own terminal; JobPilot sends it nothing. |
|             |  Gemini CLI           Consumer sign-in no longer exists.       |
|             |  Codex CLI            Run it yourself; API-key mode unknown.   |
|             |  Web endpoints (not   unknown. Only localhost is supported.    |
|             |  on your computer)                                             |
+-------------+----------------------------------------------------------------+
```

Consent dialog (opened by Review on an eligible, non-local provider). Nothing is sent until the user confirms. Focus starts on Cancel.

```text
+------------------------------------------------------------------+
| Share data with {provider} ({sign-in method})                    |
+------------------------------------------------------------------+
| Allow these kinds of data to be sent:                            |
|  [ ] Job text            [ ] Company text                        |
|  [ ] Basic profile       [ ] Resume text                         |
|  [ ] Career memory answers                                       |
|  [ ] Sensitive (work authorization, pay history, answers marked  |
|      sensitive)   Off unless you tick it separately.             |
|                                                                  |
| What will be sent for the next task:                             |
|   {task name}: {categories}  [Show exactly what will be sent]    |
|                                                                  |
| How {provider} uses it (terms read {register date}):             |
|   {disclosure line from the register}                            |
|                                                                  |
| You can revoke this any time. Revoking stops calls at once but   |
| cannot recall data already sent.                                 |
|                                                                  |
|                       [Cancel]   [Allow selected] ~~~            |
+------------------------------------------------------------------+
```

- Consent is per provider, per sign-in method, and per category. [Allow selected] stays disabled until at least one category is ticked. No category is ticked by default.
- A verdict change in a release suspends the affected consent until the user confirms again, and the list row says "Suspended: terms changed".
- Consent records, timestamps, and revocations are listed under the provider and shown in audit entries (FR-12.7).
- **Release:** 0.4. Subscription rows depend on decision D1, which is open.
- **Keyboard:** the provider list is one tab stop with arrows. Enter on a row opens its detail. Ineligible rows are focusable so their reason is read. In the dialog, the checkboxes take Space, the focus stays inside, and Esc cancels.
- **Focus order:** Ollama toggle, Add endpoint, Review, then the Not available list (read-only rows, in order). Dialog: Cancel, category checkboxes, Show exactly what will be sent, Allow selected.
- **Empty state:** no provider enabled other than No-AI: "JobPilot works without one. Tasks that need AI end with a manual path." This is a normal state, not an error.
- **Error state:** a revoked or suspended consent shows an alert on the row and blocks calls. A provider that returns a usage-limit error shows "Manage usage" with the vendor link and stops for that task. A user-not-eligible response ends that provider for the session with an explanation and no retry loop.

## 14. Coverage

| Brief item | Where |
|---|---|
| App shell and navigation | Screen 2 |
| Recovery screen | Screen 3 |
| Status | Screen 4 |
| Documents | Screen 5 |
| Data and privacy | Screen 6 |
| Profile | Screen 7 |
| Job list | Screen 8 |
| Job detail with score explanation | Screen 9 |
| Resume compare with evidence | Screen 10 |
| Application review with pause state | Screen 11 |
| Terminal panel | Screen 12 |
| AI provider consent | Screen 13 |

Open items for the owner: the migration-failure error code is whatever the core `Db` error serializes to, and the recovery screen shows it verbatim; the pause-panel timeout (30 minutes) and the Ctrl+` and F6 keys are proposals; the Sign in with ChatGPT row exists only if decision D1 adopts it.
