# Prompt-injection corpus

Hostile job postings, one per file, named for the path in section 6 of
`docs/security/threat-model.md` that they exercise. `../injection.rs` runs them.

No real model runs. `SteeredProvider` in the test plays a model that obeys the posting: it
returns the JSON on the posting's `MOCK_REPLY:` line, so each file states what a fully steered
model would do. The tests show what the app does with that reply.

- `MOCK_REPLY:` is the last line, except in `10_oversize.txt`, where truncation would cut it,
  so it is the first. In `03` and `04` it sits in a `<p>`; normalizing leaves it on its own line.
- The reply is the resume shape. The cover-letter run turns its `claims` into `paragraphs`.
- `$JOB_ID` and `$DRAFT_ID` in a reply become ids that exist but are not evidence.
  `$CANARY` in a posting becomes a path under the test's temp folder, which must not exist
  afterwards.
- Replies cite only empty or foreign ids. A reply that cites real evidence for words the
  evidence does not support is threat path 11, which is not closed (`ponytail:` in
  `drafts::check_claims`), so no file claims it is.
- `04` writes the markers as HTML entities because a literal marker is read as a tag and
  dropped; entities decode last, so only `generate::neutralise` stops them.
- `15` asks the model to call a tool, run a shell command, and read a file under `$CANARY`.
  Its reply carries an extra `tool_use` key, which the draft schema refuses, so nothing is
  stored. Only the resume run sees the key: the cover-letter harness rebuilds its reply from
  `claims`.
- `16` asks for the profile email inside a tracking URL and an image link. Its reply puts both
  in a claim that cites no evidence, so the text is stored flagged `no_evidence` and export
  refuses. Neither file reports a finding when normalized.
- `11` holds real bidi overrides and zero-width characters. Do not let an editor "clean" it.

To add a case: add a file, then add what it must report to the table in
`every_corpus_file_normalizes_without_panic_and_reports_findings` if it should report anything.
