# JobPilot AI: UI Wireframes

Status: draft for owner approval. Date: 2026-11-02. Source: [master spec](superpowers/specs/2026-11-01-jobpilot-master-spec.md) section 9 (UI wireframes), [PRD](prd.md), [milestones](milestones.md), [architecture](architecture.md#64-appsdesktopsrc-react-frontend), [policy register](policy-register.md), [AI providers](ai-providers.md), [agents](agents.md), and [threat model](security/threat-model.md). Requirement IDs (`FR-<section>.<n>`) come from the PRD. Screens 2 to 14 and 16 to 19 are built (through release 0.6). Screens 8 and 9 shipped in 0.5, and screens 11 and 19 in 0.6.

These are layout sketches, not visual design. They fix what each screen shows, what it refuses to show, and how a keyboard user moves through it. Colours, type, and spacing belong to the shadcn/ui theme.

## 1. Conventions

### Release map

Only the first five screens (shell, recovery, status, documents, data) belong to release 0.1, and screens 16 to 18 belong to 0.3. They match plan Task B8: three views (`status`, `documents`, `data`) plus the recovery screen. Every screen in the table is built in the release named; a section marked "as built" records where the build moved from the sketch.

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
| 11 | Application review and pause state | **0.6** (four modes: Manual, Copilot, Autonomous, Claude Code apply) | FR-9.2 to FR-9.6, FR-9.16 to FR-9.21 |
| 12 | Terminal panel | 0.2 (header label for an apply run added 0.6) | FR-10.1, FR-10.2, FR-12.5 |
| 13 | AI provider consent | 0.2 (Claude Code row), 0.4 (other providers) | FR-3.12, FR-3.30 |
| 14 | AI providers: Ollama and models | 0.2 | FR-2.13, FR-3.2 to FR-3.8 |
| 16 | Applications | 0.3 | FR-2.12, FR-8.8, FR-10.9, FR-10.11 |
| 17 | Career memory | 0.3 | FR-8.1 to FR-8.8, FR-10.10 |
| 18 | Agents | 0.3 | FR-11.12, FR-11.15, FR-11.16, FR-10.12, FR-12.8 |
| 19 | Claude Code apply run | **0.6** | FR-9.7 to FR-9.21, FR-12.8 |

### Rules every screen follows

- **Policy reality, not aspiration.** Screens show what the [policy register](policy-register.md) allows today, with the owner decisions of 2026-11-01 applied to AI providers. For site rows, `unknown` behaves as `blocked`, except Copilot fill on a row recording "no clause found" (D2), and the Claude Code apply mode, which the owner's decision of 2026-11-01 opens on every site. A control that is forbidden is shown disabled with its reason, or is absent when it must not exist (a Claude login button). No setting overrides a site verdict.
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
| Ctrl+1 to Ctrl+9 (Cmd on macOS) | Open Status, Documents, Data, Profile, Resumes, AI, Applications, Memory, Agents, in that order. Ctrl+0 is the last free number: Jobs takes it in 0.5. |
| Ctrl+Shift+H (Cmd+Shift+H on macOS) | Emergency pause, on every screen. It only turns the pause on. See the note under screen 2. |
| Esc | Close the open dialog and return focus to the control that opened it |
| Enter, Space | Activate the focused button or toggle |
| Arrow keys | Move within a list, table, or radio group; Tab leaves the group |

Dialogs trap focus, open with focus on the least destructive control, and restore focus on close. Focus order is the DOM order, left to right then top to bottom, unless stated.

### Notation

`[Button]` is a button. `[ ]` is a text input. `(x)` and `( )` are selected and unselected radios. `[x]` and `[ ]` in a list are checkboxes. `{...}` is dynamic text. `~~~` marks a disabled control. Boxes are plain ASCII, at most 80 columns.

## 2. App shell and navigation (release 0.1)

The shell is a title bar, a left navigation, and one main view. Zustand holds only the current view (`status`, `documents`, `data`, and the later entries below). TanStack Query holds all server state. In recovery state the navigation is hidden (see screen 3).

```text
+------------------------------------------------------------------------------+
| JobPilot AI                                    [Emergency pause]      v0.3.0 |
+-------------+----------------------------------------------------------------+
| Status      |                                                                |
| Documents   |   Main view: one of the entries listed below                   |
| Data        |                                                                |
|             |   Errors from IPC appear here, in an alert, above the view.    |
|             |                                                                |
|             |                                                                |
|             |                                                                |
+-------------+----------------------------------------------------------------+
| Your data stays on this computer. No account.          Data: {data root}     |
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
 AI                 0.2       14 (Ollama, models, Claude Code consent); 13 grew in 0.4 (providers, keys, endpoint, CLIs)
 Applications       0.3       16 (history and summary); review screen 11 joins in 0.6
 Memory             0.3       17 (career memory)
 Agents             0.3       18 (runs and logs)
 Jobs               0.5       8, 9 (Ctrl+0)

 Bottom panel: Terminal 0.2 (screen 12)
 Title bar: Emergency pause 0.3, visible on every screen
```

- **Release:** 0.1 for three entries and the footer. The other entries are marked above. The title bar and its pause button arrive in 0.3.
- **Keyboard:** Ctrl/Cmd+1 to 9 switch views (Ctrl+0 is the last free number, for Jobs in 0.5). Up and Down move within the navigation list. Enter opens the entry. F6 moves between navigation and main view. Ctrl/Cmd+Shift+H turns the emergency pause on from any screen, including with focus in the terminal panel.
- **Title bar (0.3):** `[Emergency pause]` is a red button. Once the pause is on it reads "Paused. Nothing automated runs." beside `[Clear pause]`, which asks through a native dialog. With no data open the bar says "No data is open, so nothing is running." The native `Safety` menu holds the same item. On macOS the shortcut may not fire while the web view is hung (a `wry` limit, see [agents](agents.md#8-emergency-pause)); the menu item and `jobpilot pause set` are the reliable paths.
- **Focus order:** Emergency pause (the first tab stop on every screen), navigation list, main view in its own order, footer text (not focusable). On view change, focus moves to the main view heading.
- **Empty state:** not applicable. The shell always has at least three entries, and the pause button.
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
| `migration_failed` | "Updating your data to this version failed. Your data from before the update is kept as a snapshot named `pre-migrate-{from}-{stamp}` in {data root}/backups. Nothing was deleted." | Try again enabled. Restore opens with the `pre-migrate` snapshot listed first. |
| `restored_not_opened` | "The restore finished and your data now comes from the backup, but it could not be opened: {cause}. Your data from before the restore is kept as a snapshot named `pre-restore-{stamp}` in {data root}/backups." | Try again is the default action. Restore enabled with the `pre-restore` snapshot listed first. |
| `db` | The core message, the code, and "Nothing was changed by this screen." | Try again and Restore enabled. |
| `root_not_owned`, `io`, `unavailable`, or any code this build does not know | The core message, the code, and "Nothing was changed by this screen." | Try again enabled. Restore is disabled: the backend refuses an offline restore unless the database itself failed (`db`, `migration_failed`, `schema_too_new`, `restored_not_opened`), and the screen mirrors that. |

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
| Backup: {label}-{stamp}   schema {n or unknown}   made {date}    |
|                                                                  |
| This replaces everything now in JobPilot: documents, settings,   |
| and the activity log. Secret names stay listed, because the      |
| secrets stay in your keychain. A snapshot of your current data   |
| is saved first as pre-restore-{stamp}, so you can undo this by   |
| restoring that snapshot.                                         |
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
| Keychain secrets are removed for this data folder. A copied      |
| folder or restored backup that shares its key id loses its key   |
| too. This cannot reach a recovery file, backup, or export you    |
| saved elsewhere.                                                 |
| Deleting the activity log is part of this action, so no record   |
| of it remains.                                                   |
|                                                                  |
| Type DELETE to confirm:  [                    ]                  |
|                                                                  |
|                         [Cancel]   [Delete everything] ~~~       |
+------------------------------------------------------------------+
```

- Nothing deletes snapshots automatically. `pre-migrate`, `pre-restore`, and from 0.2 `pre-discard` folders stay in {data root}/backups until the user removes them or ticks "Also delete backups and snapshots".
- From 0.2 the delete text adds: removing this folder's keychain entries also removes the key for any copied folder or restored backup that shares its key id, and cannot reach a recovery file you saved. A "No recovery key exported" banner shows at the top until the user exports one (threat model section 9).
- Secret handling: after Save secret the value field is cleared and the value appears nowhere in the page. Only the name remains. There is no read command (plan Task B7 and test `secret_ui_never_displays_values`).
- Export warns first: the file is plain text, including everything in `documents`.
- **Release:** 0.1. Native-dialog confirmation and the recovery-key banner are 0.2.
- **Keyboard:** Tab runs top to bottom through the sections. The backup list is one tab stop with arrows inside. In a dialog, Tab cycles inside it, Esc cancels. In the delete dialog, Enter on the field does nothing while the field is not `DELETE`, and activates Delete everything when it is.
- **Focus order:** Create backup, backup list (Restore per row), Restore from another folder, Export all data, secret Name, secret Value, Save secret, Remove (per row), activity list, Delete all data. Restore picker uses the single `dialog:allow-open` permission for choosing a folder.
- **Empty state:** no backups: "No backups yet. Create one before you change anything important." No secrets: "No secrets stored." No activity: "No activity yet."
- **Error state:** each action shows its error in an alert inside its own section: `in_use` ("Close the other JobPilot window or the `jobpilot` command, then try again."), `invalid_backup` ("This folder is not a valid JobPilot backup. Nothing was changed."), and a keychain failure for secrets without any value in the message. A failed restore or delete leaves live data as it was, and the screen reopens the database before returning. The one exception is `restored_not_opened`: the restore finished and only the reopen failed, so this alert must not say nothing changed.

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

Built. Ctrl+0 opens it. Jobs come from connectors whose register read verdict is `allowed` (Lever, RemoteOK, We Work Remotely), from the two R1 reads (Greenhouse and Ashby), or from user-assisted import. The register verdict is shown, so the user sees why a source has no connector.

```text
+-------------+----------------------------------------------------------------+
| ...         | Jobs                                [Import a posting]         |
| Jobs        +----------------------------------------------------------------+
|             | Country [  ] Mode [Any v] Min salary [    ] Currency [   ]     |
|             | Per [year v]  [Apply]                                          |
|             | Filters are exact rules. A job that does not state a field     |
|             | stays listed and says so. Score uses your stated preferences.  |
|             +----------------------------------------------------------------+
|             | Job text came from outside and is shown as plain text.         |
|             | Score  Title / Company          Where       Source             |
|             |  82    Backend Engineer, Acme   Remote, PT  Remote OK         |
|             |        not stated: country                                     |
|             |  74    Platform Dev, Beta       Lisbon       Lever             |
|             |  --    Data Eng, Gamma          Toronto      Imported          |
|             |        No score: nothing in your profile to compare            |
|             +----------------------------------------------------------------+
|             | Sources                                                        |
|             |  Lever       read: allowed       [Board] [Fetch]               |
|             |  RemoteOK    read: allowed       [Fetch]  attribution shown    |
|             |  We Work Remotely  read: allowed [Fetch]  attribution shown    |
|             |  Greenhouse  read: ruling R1 [Board] [Fetch]              |
|             |    Vendor is silent on third-party readers. Read under         |
|             |    ruling R1.                                                  |
|             |  Ashby       read: ruling R1 [Board] [Fetch]  (same note) |
|             |  Workday     read: unknown   No connector. Import instead.     |
|             |  Indeed      read: blocked   No connector. Import instead.     |
|             |  LinkedIn    read: blocked   No connector. Import instead.     |
|             |  Company career page  read: unknown  No connector. ...         |
+-------------+----------------------------------------------------------------+
```

- A source that cannot fetch has no Fetch button, only the import route. A board source (Lever, Greenhouse, Ashby) asks for the board name first. Attribution ("Remote OK", "We Work Remotely") shows on every card from those sources. Job text is labelled as outside text and renders as plain text: no links, no images, no logos.
- The fixed R1 note is "Vendor is silent on third-party readers. Read under ruling R1." **R1 is a controller ruling, not an owner decision.** The GUI label for these two rows still reads `read: ruling R1`. That wording is wrong and is listed as a known issue in the [0.5 release note](releases/0.5.0.md#known-issues).
- A score shows `--` with "No score: nothing in your profile to compare" when nothing could be compared, and "Not scored yet" when no result is stored. A score made from older inputs shows "Out of date" under the number, and on the job detail beside it. The rules score runs when the screen opens, if a listed job has no result or a stale one, and after a fetch or an import, for every such job. With nothing to do it starts no run.
- A duplicate shows under its first source as "+{n} other sources". The list hides duplicates.
- **Release:** 0.5.
- **Keyboard:** Tab to the filter form, then the job table (one tab stop; arrows, Home, and End move rows; Enter opens screen 9; `i` opens the import dialog). Back from a job returns to the same row with the filters as they were left.
- **Focus order:** Import a posting, filter controls, Apply, job table, then each source's board field and Fetch button.
- **Empty state:** "No jobs yet. Import a posting, or fetch from a source marked allowed." The Sources list is still shown.
- **Error state:** a fetch that stops shows its reason in an alert on that source row, with the code: "Refused by policy: unknown", "Rate limited. Next fetch at {time}." (the time is the true hold, up to 24 hours), a source that refused or showed a challenge page, a board or feed that does not exist, a changed shape, an answer over 16 MiB, or a source too slow to answer. A source never retries beyond its declared limit. A fetch that works reads "{n} added, {n} updated, {n} skipped."

## 9. Job detail with score explanation (release 0.5)

Built in `JobDetail.tsx`.

```text
+-------------+----------------------------------------------------------------+
| ...         | [Back to Jobs]                                                 |
| Jobs        | Backend Engineer, Acme                  Score 82 / 100         |
|             | Source: Remote OK (link, remoteok.com)  Untrusted text. Shown  |
|             |   as plain text.                                               |
|             +-----------------------------+----------------------------------+
|             | Why this score              | Posting text                     |
|             |  Criterion Weight Points    | {plain text in a focusable,      |
|             |  skills      40    34       |  scrollable region; no HTML,     |
|             |  seniority   20    18       |  no images, no live links}       |
|             |  location    20    20       |                                  |
|             |  pay         20    not      | Findings: {kind}: {excerpt}      |
|             |                    compared |                                  |
|             | Compared weight: 80 of 100  |                                  |
|             | Missing skills: Kubernetes  |                                  |
|             | Pay: 70000-80000 EUR,       |                                  |
|             |   per year                  |                                  |
|             |                             |                                  |
|             | Work authorization          |                                  |
|             |  PT: not stated by you.     |                                  |
|             |  Not checked.               |                                  |
|             | Scored by: rules.           |                                  |
|             | [Add AI evaluation]         |                                  |
|             | (only if a backend is on)   |                                  |
|             | AI evaluation (own panel)   |                                  |
|             +-----------------------------+----------------------------------+
|             | [Add to tracker] [Open posting in my browser] [Prepare...]     |
|             | [Remove job]                                                   |
+-------------+----------------------------------------------------------------+
```

- The explanation lists every criterion with its weight and the points it gave (FR-6.4, FR-6.10). A criterion that could not be compared says `not compared`, and the compared weight shows below 100. Unknown is never a mismatch. Objective filters are rules (FR-6.11). Semantic evaluation runs only when an AI backend is enabled and consented (FR-6.12).
- The Source line shows the attribution or the source's name and, for an https link, a button labelled with the host. Rust reads the stored link and opens it in the default browser. The page sends only the job id. A link with a user name, a port, or plain http is refused at import, and again on open.
- **Work authorization** is exactly one line: "No country on this posting. Not checked.", "{country}: not stated by you. Not checked.", "You stated: authorized {value}, needs sponsorship {value} for {country}. Not checked against this posting.", or "Locked, see Recovery." It is never shown as eligible, ineligible, or likely. When the posting itself speaks of work authorization, a second line says so and tells the person to read it.
- **AI evaluation** is a separate panel: fit ("the model's own number: it is not part of the score"), summary, strengths with the evidence they cite, gaps, provider, date, and `dropped: {n}` when Rust cut values. "Scored by: rules." is always shown. A strength with no real evidence never reaches the panel.
- A job with a duplicate shows "Same job on {n} other sources" and a "Not a duplicate" button. Company notes show with their origin. **Prepare...** opens Resumes on this job's text. **Remove job** asks first, and says the text goes with the job unless a draft cites it, and that an application in the tracker stays but loses its posting text.
- **Release:** 0.5.
- **Keyboard:** Alt+Left or Back returns to the list with the same row focused. Opening a job moves focus to its heading. Tab goes through the explanation table, the work authorization line, Add AI evaluation (only if enabled), the posting text, then the action row.
- **Focus order:** Back, heading, Source link, Score explanation table, Add AI evaluation (only if enabled), posting text, Add to tracker, Open posting in my browser, Prepare..., Remove job.
- **Empty state:** a job with no explanation shows "Could not read this posting's fields, or it has not been scored yet." Missing skills reads "None found" only when the skills criterion was compared. With nothing to compare it reads "Add your profile to see this."
- **Error state:** an AI evaluation that fails or is refused shows "AI evaluation stopped. The rules score stands." in an alert with its code, and the rules score stays visible. The score is never blank because of an AI failure.

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

**As built in 0.6.** `Review and apply...` on the selected application opens this screen in place of the Applications list. The heading is "Review application: {role}, {company}" with `Back`, then "Site: {host}". The site-rule block of the sketch below is reduced to the host: the material carries no register rows. The mode group lists **four** modes, each with Rust's fixed note (from `apply::material`), and a mode that is not available is `aria-disabled`, focusable, and described by "Not available. {note}":

| Mode | Note when available | Note when not |
|---|---|---|
| Manual | "You fill and submit in your own browser. JobPilot opens the link, gives you files and answers, and records what happened." | (always available) |
| Copilot | "No clause found for this site. You review and submit." (Greenhouse, Lever, Ashby, and their fixtures) | "The site's terms forbid this. JobPilot will not do it, even if you ask." (Indeed), or "The site has not said whether tools may do this. Treated as blocked until it answers." (Workday, LinkedIn, any other) |
| Autonomous | (no site today) | "No site or connector qualifies today. Needs a site rule of allowed, verified permission, and your explicit authorization.", or the blocked text on Indeed; below it, "Connectors you authorized for Autonomous: {list or none}." and one `Revoke {connector}` button per grant, which revokes with no dialog (revoking only takes something away; granting stays in the CLI and its native dialog) |
| Claude Code apply | "Fills and submits with Claude Code in a visible browser. Add it to a run from Applications." | "Needs the browser runtime and Claude Code set up first. Add it to a run from Applications to see what is missing." |

Choosing Copilot shows `Start Copilot` and its live pause panel (`CopilotRun`). Choosing Claude Code apply shows "Select this application on Applications, then press Apply with Claude Code...": a run is started from screen 19, never from here. Answers are copied by Rust: `Copy` for a Normal answer, and `Confirm and copy` for every sealed one, which always asks the native dialog first; the text never reaches the screen, and Rust clears the clipboard after 60 s. Files show with `Copy path`, or "Nothing prepared." with `Prepare documents`. Work authorization shows "stated by you" with `Show what I stated`, or "not stated by you" with `Enter it yourself` (to Profile). `Open the form in my browser` is disabled with "The posting link is not https, so JobPilot will not open it." when it is not. `Record outcome...` opens Outcome, Applied on (optional), Note (optional), and `Save outcome`. The original sketch follows.

The review screen shows one application before anything happens on the site. Manual mode is always available on a `user-assisted` row. Copilot is available on sites whose register row records "no clause found" (Greenhouse, Lever, Ashby; decision D2). The mode choice shows each mode's status and the register reason when a mode is not available.

```text
+-------------+----------------------------------------------------------------+
| ...         | Review application: Platform Dev, Beta (Greenhouse form)       |
| Applications+----------------------------------------------------------------+
|             | Site rule (register 2026-11-01)                                |
|             |  Greenhouse hosted form: apply by hand = user-assisted         |
|             |                          fill by tool  = no clause found       |
|             +----------------------------------------------------------------+
|             | How do you want to apply?                                      |
|             |  (x) Manual    You fill and submit in your own browser.        |
|             |                JobPilot opens the link, gives you files and    |
|             |                answers, and records what happened.             |
|             |  ( ) Copilot   JobPilot fills the form and uploads files.      |
|             |                You review and click submit. No clause found    |
|             |                for this site; it will pause for CAPTCHA, ID    |
|             |                checks, and legal declarations.                 |
|             |  ( ) Autonomous ~~~ Not available. Off by default.             |
|             |                Why: no connector has verified permission.      |
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
| `unknown` with "no clause found" (Copilot, D2) | Copilot is selectable. Text beside it: "No clause found for this site. You review and submit." |
| `unknown` otherwise | "The site has not said whether tools may do this. Treated as blocked until it answers." (Workday) |
| `blocked` | "The site's terms forbid this. JobPilot will not do it, even if you ask." (Indeed) |
| Autonomous with no eligible connector | "No site or connector qualifies today. Needs a site rule of allowed, verified permission, and your explicit authorization." |
| Autonomous with an OpenAI-origin model | "Not allowed with OpenAI models for submissions with no human review." |

Copilot becomes selectable for a site with its own `allowed` row or a row recording "no clause found", and for local fixture forms during development. Autonomous stays default-off and needs verified permission and explicit authorization. No test or setting turns a disabled mode on for a site whose row is `blocked`.

### Pause state

**As built in 0.6** (`PausePanel`, shared by Copilot and screen 19): the heading is "Run paused: waiting for you", then "The run waits 30 minutes, then stops this application and hands it over to you." The six rows are listed with the active one marked `[>] ... ACTIVE` (`aria-current="step"`), the page's excerpt as inert text in a quote, and the row's fixed note. A `login` pause shows under "Other step that needs a person" with "Sign in, in the browser window." The buttons are `I did this in the browser: continue`, `Take over by hand`, and `Cancel run` (behind a confirmation). There is no Esc handler.

When a Copilot or Autonomous run reaches a mandatory pause, the review screen is replaced by the pause panel. As built, an Autonomous run does not pause on a legal declaration or an identity request: it ends the item `manual_required` (`legal_declaration`), and the person submits. This appears on any Copilot run, including Greenhouse, Lever, and Ashby forms, and on a Copilot run CAPTCHA, MFA, identity verification, and legal declarations always trigger it; an Autonomous run pauses on a CAPTCHA or MFA and ends the item on the other two, as above. The six pauses from the agent specification are listed so the user always sees the full set, with the active one marked.

```text
+-------------+----------------------------------------------------------------+
| ...         | Run paused: waiting for you                                    |
| Applications+----------------------------------------------------------------+
|             | Nothing has been submitted. The run waits {timeout}, then      |
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
- The emergency pause control (0.3) is in the title bar on this screen too. When set, the run ends `halted` and the browser closes. An apply run does not resume (decision 21): the owner clears the pause and starts a new run with what is left.
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

- **0.6: the apply run label.** While the panel hosts a Claude Code apply run (screen 19), the header reads `Terminal   Claude Code apply run` in place of `jobpilot CLI only`. The session is `jobpilot apply session <id>`, chosen by Rust (`pty_open_apply`); the webview sends only the run id and the size. If a session is already open, a confirmation asks first: "This ends the terminal session." with `End session` and Cancel; declining, or a failed open, cancels the run. Restart goes back to the plain CLI. Nothing reads the session's output.
- Commands that need a native confirmation, and the recovery-key commands, are refused here: "Run this in an ordinary terminal, or use the app." The panel shows that line when the PTY layer rejects a command. No command prints a secret value.
- Scrollback is not stored and is never read into a workflow. Agents have no terminal access.
- **Release:** 0.2.
- **Keyboard:** Ctrl+` (Cmd+` on macOS is reserved by the OS, so Ctrl+` on both) toggles the panel and focuses it. Inside the terminal every key goes to the shell, including Tab and Esc. **F6 leaves the terminal** for the next region, so a keyboard user is never trapped. F6 is never sent to the shell.
- **Focus order:** main view, then (F6) navigation, then (F6) terminal, then Clear and Close panel. The terminal area is one tab stop, and the screen reader announces it as a terminal with the leave key.
- **Empty state:** an empty panel shows the prompt and "Type `jobpilot --help`."
- **Error state:** a failed PTY start shows an alert in the panel header with its code and a [Restart] button. A refused command prints the refusal line above and keeps the session. A rejected command never reaches a shell.

## 13. AI provider consent (release 0.2 for Claude Code, 0.4 for the rest)

**As built in 0.4.** Providers are listed with a status and the reason a provider cannot run. An ineligible provider (free Gemini key, non-loopback endpoints, Vertex AI, Ollama cloud models, Antigravity) appears under **Not available** as text, with **no control at all**: no switch, no key button, no sign-in button. No setting overrides that. The list shows no register verdict and no Manage usage link. The page shows the register date (read from the register's third line) for the rows' terms. Each external provider has a row: Claude Code in its own section, then the API providers, the endpoint, and the two CLIs under **AI providers**. A key never goes through this screen: **Paste key from clipboard...** makes Rust read the clipboard, show a native dialog naming the provider and the key's last four characters, store the key, and clear the clipboard. There is no key field and no ChatGPT row (decision 24, Sign in with ChatGPT is not in 0.4).

```text
+-------------+----------------------------------------------------------------+
| ...         | AI providers                                                   |
| AI          +----------------------------------------------------------------+
|             | No AI is needed. JobPilot works with none. Local models run on |
|             | this computer, so your data stays here. Ollama cloud models    |
|             | are off.                                                       |
|             | (None enabled: "JobPilot works without one. Tasks that need AI |
|             |  end with a manual path.")                                     |
|             +----------------------------------------------------------------+
|             | Ollama                                      [x] On             |
|             |   Data stays on this machine. No consent needed.               |
|             +----------------------------------------------------------------+
|             | Claude Code (your own login)                                   |
|             |   Off. Used only after you consent.            [Review consent...] |
|             |   Not found? "Unavailable: install claude" [Choose claude...]  |
|             +----------------------------------------------------------------+
|             | AI providers                          Register date 2026-11-01 |
|             | OpenAI | Anthropic | Google Gemini | OpenRouter                |
|             |   Model: [ vendor id ]  No key / Key stored                    |
|             |   [Paste key from clipboard...]  [Remove key]                  |
|             |   Consent given for: ...  [x] Use {provider}  [Review...]      |
|             |   [Revoke consent]   (Google: "Free keys are not supported.")  |
|             | Compatible endpoint                                            |
|             |   [Add endpoint...] / [Change endpoint...]  model, optional key|
|             | Gemini CLI | Codex CLI                                         |
|             |   {path}  [Choose gemini...] [Use automatic]                   |
|             |   Gemini: "Runs job text with every tool denied." and the      |
|             |   transcript note. Codex: "No task can use this yet: it cannot |
|             |   prove it has no tools."                                      |
|             +----------------------------------------------------------------+
|             | Not available                                                  |
|             |  Google Gemini key, free key: no personal data allowed.        |
|             |  Antigravity CLI: no launcher. Run it in your own terminal.    |
|             |  Web endpoints (not on your computer): not supported.          |
|             |  Vertex AI: not supported.                                     |
|             |  Ollama cloud models: not supported.                           |
|             +----------------------------------------------------------------+
|             | Order                                                          |
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
- A changed register row, or an upgrade from a build that did not record the row, suspends the affected consent until the user confirms again. The row shows an alert: "Suspended: this consent was given before the terms on file now. Review it once to turn it back on." (the endpoint's says the address changed, or the terms did), with one **Review consent...** button. The dialog opens with the old categories ticked, so **Allow selected** is the one click, and Rust's native confirmation stays behind it. The owner's existing Claude consent is suspended this way once on upgrade to 0.4. The Resumes screen also says why Generate is gone while a consent is suspended.
- **Show exactly what will be sent** shows the real prompt for the newest pasted posting as inert text, from the same function a run uses. For Gemini CLI it shows the wire text, with each `@` as `\@`.
- A provider that needs an attestation (Google paid key, Gemini CLI) shows it in the dialog, and Allow selected stays disabled until it is ticked.
- Consent records, timestamps, and revocations are listed under the provider and shown in audit entries (FR-12.7).
- **Release:** 0.2 for the Claude Code row, 0.4 for the other providers. Subscription rows follow decision D1 and hosted API rows decision D4, both decided 2026-11-01. An unreleased control (Sign in with ChatGPT) is not drawn.
- **Keyboard:** the provider list is one tab stop with arrows. Enter on a row opens its detail. Ineligible rows are focusable so their reason is read. In the dialog, the checkboxes take Space, the focus stays inside, and Esc cancels.
- **Focus order:** Ollama toggle, the Claude Code section, each provider row's controls, then the Not available list (read-only rows, in order). Dialog: Cancel, category checkboxes, Show exactly what will be sent, Allow selected.
- **Empty state:** no provider enabled other than No-AI: "JobPilot works without one. Tasks that need AI end with a manual path." This is a normal state, not an error.
- **Error state:** a revoked or suspended consent shows an alert on the row and blocks calls. A provider that fails shows its reason in plain words (`quota_exhausted`, `auth_failed`, `model_not_found`, `cli_too_old`, `wrong_path`, and the rest) with its code. A usage-limit or spend-cap response stops that provider for the task and is not retried.

## 14. AI providers: Ollama and models (release 0.2)

This is the first AI screen. It sets up Ollama on this computer (FR-3.2 to FR-3.8) under the heading "AI providers". The Claude Code consent row (screen 13) joined this view in 0.2; the other providers joined in 0.4. A local model under loopback Ollama needs no consent, because data stays on the machine. Ollama cloud models are not offered: they are hidden, and refused by routing check 2 in docs/ai-providers.md.

```text
+-------------+----------------------------------------------------------------+
| ...         | AI providers                                                   |
| AI          +----------------------------------------------------------------+
|             | No AI is needed. JobPilot works with none. Local models run on |
|             | this computer, so your data stays here. Ollama cloud models are off. |
|             +----------------------------------------------------------------+
|             | Ollama         Running at {loopback address}     [Check again] |
|             | This computer  {memory} memory, {GPU or "no GPU found"},       |
|             |                {free disk} free                                |
|             +----------------------------------------------------------------+
|             | Installed models                                               |
|             |  {model}   {size}   Fits this computer                         |
|             |  {model}   {size}   May be slow here                           |
|             | Drafting model   [ {model}             v ]                     |
|             | Embedding model  [ {model}             v ]                     |
|             +----------------------------------------------------------------+
|             | Available to download (nothing downloads unless you ask)       |
|             |  {model}   {size}   Fits this computer          [Download...]  |
|             |  {model}   {size}   Too large for this computer [Download...]  |
|             |  Downloading {model}   {percent}                [Cancel]       |
+-------------+----------------------------------------------------------------+
```

Model rows and the status line change with what JobPilot finds:

| State | Detected when | Text shown | Actions |
|---|---|---|---|
| Running | Ollama answers on loopback. | The layout above. | Choose models, download, check again. |
| Not running | Nothing answers on loopback. | "Ollama is not running on this computer. Start it, then check again. JobPilot does not install or start it." | Check again. Model lists are hidden. Tasks use No-AI. |
| No compatible model | Ollama runs, but no installed model does structured output (FR-3.6) or embeddings (FR-3.7). | "No installed model can do this yet. Download one below." The missing role is named. | Download. Tasks needing that role use No-AI. |
| Offline | A download cannot reach the network. | "Downloading needs a network. Installed models keep working." | Installed models stay usable (FR-2.13). |

- Downloads start only from Download, followed by a dialog naming the model and its size (FR-3.4). Nothing downloads on first run or in the background. Personal data is never sent in a download.
- Hardware detection guides and never decides (FR-3.5). A model marked too large can still be downloaded, and the dialog warns first.
- The address must resolve to loopback (M-21). A non-loopback address is refused until it has its own register row.
- **Release:** 0.2.
- **Keyboard:** Tab moves between controls. The model lists are one tab stop each, with arrows inside. Selects open with Space and move with arrows. In the download dialog, focus starts on Cancel and Esc cancels. The AI entry takes the next number in the Ctrl/Cmd shortcut order.
- **Focus order:** heading, Check again, installed list, Drafting model, Embedding model, available list (Download per row), Cancel for a running download.
- **Empty state:** Ollama running with no models: "No models installed. Download one below, or keep working without AI." This is a normal state, not an error.
- **Error state:** a failed or cancelled download shows an alert with its code on that row and leaves installed models alone. A refused address shows the reason in an alert. The app keeps working with No-AI in every state above.

## 15. Coverage

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
| Local AI setup | Screen 14 |
| Applications | Screen 16 |
| Career memory | Screen 17 |
| Agents | Screen 18 |
| Claude Code apply run | Screen 19 |

Open items for the owner: the pause-panel `{timeout}` is built as 30 minutes ([agents](agents.md#4-the-nine-agents)), and the Ctrl+` and F6 keys are proposals; the Sign in with ChatGPT row is optional under decision D1.

## 16. Applications (release 0.3)

Track applications by hand and offline (FR-2.12). A status change runs through the tracking agent, so it shows in Agents (screen 18). A summary of counts and rates is built on request by the analytics agent and holds no company, role, or note. Link text is untrusted and is shown as inert text, never as an anchor.

```text
+-------------+----------------------------------------------------------------+
| ...         | Applications                              [Add application]    |
| Applications+----------------------------------------------------------------+
|             | Summary                                            [Refresh]   |
|             |  Total {n}   Applied in the last 30 days {n}                   |
|             |  Response rate {percent or "not enough data yet"}              |
|             |  Median days to a response {n days (from n responses)}         |
|             |  By status: {saved 2} {applied 5} {screening 1}                |
|             +----------------------------------------------------------------+
|             | Filter by status [ All statuses  v ]                           |
|             | Company     Role         Status      Applied     Updated       |
|             | > Beta      Platform Dev screening   2026-10-01  Nov 01        |
|             |   Acme      Engineer     applied     2026-10-12  Oct 12        |
|             +----------------------------------------------------------------+
|             | Company [        ] Role [        ]                             |
|             | Link (untrusted text, not clickable) [                       ] |
|             | Country (two letters) [  ]    Notes [                        ] |
|             | [Save]                                                         |
|             | Change status: New status [ v ] Applied on (optional)[YYYY-MM-DD]
|             |   Status note (optional) [               ]  [Change status]    |
|             | Add note [                          ] [Add note]               |
|             | History: {at} {actor} {from -> to} {note}                      |
|             | Answers for this application:                                  |
|             |  Answer to use [ Choose an answer v ]                          |
|             |  [Copy] or [Confirm and copy] (native dialog), then the text   |
|             |  Authorization country [  ] [Show what I stated] [Open Profile]|
|             |   -> {stated value, or "Not stated"}                           |
|             | [Remove...]                                                    |
+-------------+----------------------------------------------------------------+
```

- A sensitive or legal answer shows its text only after a person confirms it for this application, through a native dialog the web page cannot answer. Editing the answer voids the confirmation. A work-authorization question has no stored answer: the screen shows the value from Profile for the country asked, or "Not stated".
- While the emergency pause is on, `[Change status]` is refused with `paused` and says to clear the pause. Editing fields and adding notes still work.
- **Release:** 0.3.
- **Keyboard:** Ctrl/Cmd+7 opens it. Tab moves through filter, list, then the open application's forms. Enter on a row opens it. Esc closes the remove dialog.
- **Focus order:** Emergency pause, navigation, heading, Refresh, Add application, Filter, list, then the form in the order shown.
- **Empty state:** "No summary yet. Press Refresh to build one." A rate with nothing behind it reads "not enough data yet", never 0. No applications: "Select an application, or add one."
- **Error state:** an alert with the code above the part that failed. `paused`, `confirmation_required`, `refused`, and a stale-answer message each name what to do.

## 17. Career memory (release 0.3)

Questions you are asked again and again, with answers you approve (FR-8.1 to FR-8.8). Text is inert. Search is by meaning when a local embedding model is set, else by words. A sensitive or legal answer is never shown in search, and work-authorization answers are never found by search.

```text
+-------------+----------------------------------------------------------------+
| ...         | Career memory                                                  |
| Memory      +----------------------------------------------------------------+
|             | Search memory [                    ] [Search] [Refresh index]  |
|             |   At most {n} paid or subscription calls for {p} questions     |
|             |   (20 questions a press) / No outside calls                    |
|             |   {Checked n, raised n, indexed n}                             |
|             +----------------------------------------------------------------+
|             | Filter by class [ All v ]                                      |
|             | Question {Why this company?}      [ normal              ]      |
|             |   Noticed ({kind}): {excerpt}                                  |
|             |   Class [ normal v ]   [Remove question...]                   |
|             |   Answers: {text or "Sealed"}  v{n}  approved/not  fresh/stale |
|             |     [Approve] [Mark reviewed] [Edit] [Remove...]               |
|             |   Add an answer: Answer [          ] Source (optional) [ v ]   |
|             |     [Add answer]                                               |
|             |   (work authorization) "Answered from your profile, for one    |
|             |    country at a time."                                         |
|             +----------------------------------------------------------------+
|             | Add a question: Question [          ] Country (optional) [  ]  |
|             |   [Add question]                                               |
+-------------+----------------------------------------------------------------+
```

- Refresh index is the only thing that spends your Claude usage. Nothing refreshes by itself. It shows how many calls it will make before it makes any, at most 20 per press.
- Approval is yours. A class you set is final: a model may raise a class and never changes one you set.
- **Release:** 0.3.
- **Keyboard:** Ctrl/Cmd+8 opens it. Tab moves through search, filter, then each question card.
- **Focus order:** Emergency pause, navigation, heading, Search, Search, Refresh index, Filter, questions top to bottom (class, Remove, answers, add answer), Add a question.
- **Empty state:** "No questions yet. Add one you are often asked." A filter with no match: "No questions in this class." Search: "Nothing matches."
- **Error state:** an alert with the code. A sealed answer while the key is unavailable lists with an empty body and says why.

## 18. Agents (release 0.3)

Every run of the resume, cover letter, career memory, tracking, and analytics agents, with its log (FR-11.12). The log has ids, states, codes, and counts, never prompts, outputs, or text.

```text
+-------------+----------------------------------------------------------------+
| ...         | Agents                                                         |
| Agents      +----------------------------------------------------------------+
|             | Agent        State      Step   Started       Reason            |
|             | > resume     halted     2/3    Nov 01 10:04  paused            |
|             |   tracking   completed  1/1    Nov 01 09:58                    |
|             |   analytics  completed  3/3    Nov 01 09:50                    |
|             +----------------------------------------------------------------+
|             | Selected run                  [Resume] [Cancel run]            |
|             | Time   Step   State   Duration  Provider  Categories  Outcome  |
|             | {log rows, oldest first}                                       |
+-------------+----------------------------------------------------------------+
```

- `[Resume]` shows only for a halted run and passes through Checking again. `[Cancel run]` shows for a halted or active run. A cancelled run cannot resume. The list refreshes every 2 seconds.
- A halted run stays halted after the pause is cleared until you resume or cancel it. A run whose process died reads `halted` with the reason `interrupted`.
- **Release:** 0.3.
- **Keyboard:** Ctrl/Cmd+9 opens it. Tab reaches each row's open button, then Resume and Cancel run.
- **Focus order:** Emergency pause, navigation, heading, the run list, then the selected run's buttons and log table.
- **Empty state:** "No agent has run yet."
- **Error state:** an alert with the code, such as `paused` on a resume while the pause is on.

## 19. Claude Code apply run (release 0.6)

A run of the Claude Code apply mode, from the checks before it to what each application ended as (owner decision of 2026-11-01). It opens from Applications: each `saved` row has a `Select` checkbox in a "Run" column, and `Apply with Claude Code...` takes 1 to 25 selected applications. The heading is "Apply with Claude Code". It has four states.

**Setup needed**, when the browser runtime or `claude` is missing:

```text
+-------------+----------------------------------------------------------------+
| ...         | Apply with Claude Code                                         |
| Applications+----------------------------------------------------------------+
|             | Setup needed                                                   |
|             | {why: "The browser runtime is not installed.", or "Node is     |
|             |  not installed. Install Node, then set up the browser.", or    |
|             |  "The installed Node is too old. Update it, then set up the    |
|             |  browser."}                                                    |
|             | [Set up browser]                                               |
|             | Claude Code was not found. Install it, or choose its program   |
|             | on the AI screen.                                              |
|             +----------------------------------------------------------------+
|             | [Check again]  [Back]                                          |
+-------------+----------------------------------------------------------------+
```

**Preflight**, with the fixed copy verbatim:

```text
+-------------+----------------------------------------------------------------+
| ...         | Apply with Claude Code                                         |
| Applications+----------------------------------------------------------------+
|             | Preflight                                                      |
|             | Runs on your own accounts, in a visible browser. Once started, |
|             | it submits with no check per application. It stops for         |
|             | sign-in, CAPTCHA, codes, identity checks, and legal            |
|             | declarations. A site may restrict an account that uses         |
|             | automation.                                                    |
|             | Company Host Resume Letter Answers Sensitive Work    Blocks    |
|             |                            ready   held back auth.             |
|             | Beta    {h}  Ready  Ready  4       1         Stated  None      |
|             | Acme    {h}  None   Ready  2       0         Not     {why}     |
|             |                                              stated            |
|             | Sensitive answers and work authorization stay held back unless |
|             | you release them to this run, in the dialog that opens when    |
|             | you start it.                                                  |
|             +----------------------------------------------------------------+
|             | [Start run]  [Check again]  [Back]                             |
+-------------+----------------------------------------------------------------+
```

**Running**, polled every second, with the session in the terminal panel (screen 12), which opens by itself:

```text
+-------------+----------------------------------------------------------------+
| ...         | Apply with Claude Code                                         |
| Applications+----------------------------------------------------------------+
|             | Running in the terminal panel and a visible browser.           |
|             | #  Site            State         Result                        |
|             | 1  {host}          Submitted     Status recorded as applied.   |
|             | 2  {host}          In progress                                 |
|             | 3  {host}          Queued                                      |
|             +----------------------------------------------------------------+
|             | (the pause panel of screen 11 when a stop is open)             |
|             | [Cancel run]   (when no stop is open)                          |
+-------------+----------------------------------------------------------------+
```

**Ended**:

```text
+-------------+----------------------------------------------------------------+
| ...         | Apply with Claude Code                                         |
| Applications+----------------------------------------------------------------+
|             | Run ended.                                                     |
|             | #  Site     State         Result                               |
|             | 1  {host}   Submitted     The page said                        |
|             |                           > {confirmation text, inert}         |
|             |                           Status recorded as applied.          |
|             | 2  {host}   Uncertain     Check the site. JobPilot will not    |
|             |                           try again.                           |
|             | 3  {host}   Submitted     [Record status]                      |
|             | 4  {host}   Handed to you {reason}                             |
|             +----------------------------------------------------------------+
|             | [Done]                                                         |
+-------------+----------------------------------------------------------------+
```

- **Start run** is disabled while any application has a block or the Claude Code consent misses a category ("Your Claude Code consent does not cover: {list}. Review it on the AI screen."). When anything is sealed, Rust asks in a native dialog titled "Sealed answers": "Release {n} sensitive answers and work-authorization rows to this run for {m} applications?" with `Release to this run` and `Leave them out`. Closing the dialog starts no run. The webview cannot release anything. If the consent does not cover `sensitive`, the screen says so and those fields stop for you.
- Blocks are shown as text, one per line: "Only an application with status Saved can go in a run.", "The posting link is not https.", "It is already in a run that has not ended.", "The resume has a claim to fix before it can be exported.", "The cover letter has a claim to fix before it can be exported.", "This application was already submitted, or may have been. Record its outcome first." (any earlier submission, in any mode, until the outcome is recorded), or "Refused: {code}". The Resume and Letter cells read Ready, None, or Blocked with the reason (a claim to fix first, or a data key that is locked, missing, mismatched, or unreadable).
- **Record status** shows on an ended run for a submitted item whose status was not written (a pause landed between the click and the result); it calls `apply settle`.
- "The page said" is the page's claim, recorded as such, not as proof.
- Item states read: Queued, In progress, Filled, Submitted, Uncertain, Skipped, Handed to you.
- **Release:** 0.6.
- **Keyboard:** Tab moves through the table and the buttons. The Select checkboxes on Applications take Space. Esc does not cancel a run; Cancel run is a button and asks to confirm.
- **Focus order:** Emergency pause, navigation, heading, the state's content, then its buttons in the order shown.
- **Empty state:** Running with no item yet reads "Starting the session in the terminal panel."
- **Error state:** an alert with the code: `no_browser_runtime`, `claude_not_found`, `apply_run_active` (another run is open), `apply_run_not_ready`, `consent_required`, `paused`. The emergency pause ends the run `halted`; a halted run does not resume, and you start a new one with what is left.
