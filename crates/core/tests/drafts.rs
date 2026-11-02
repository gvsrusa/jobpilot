//! The `#[ignore]`d test uses the real OS keychain, under the service from
//! `.cargo/config.toml`. The rest never reach it.

use jobpilot_core::audit;
use jobpilot_core::crypto::{self, KEY_ACCOUNT_PREFIX, KeyState};
use jobpilot_core::drafts::{self, AUTHORIZATION_PATTERNS, ClaimInput, ExportFormat, check_claims};
use jobpilot_core::knowledge::{self, KnowledgeItem};
use jobpilot_core::profile::{self, ProfileBasic, Stated, WorkAuthorization};
use jobpilot_core::render::{Entry, ResumeDoc, Section, Template, lines};
use jobpilot_core::{AppPaths, Db, Error};
use tempfile::TempDir;

const AUTH: &str = "I am authorized to work in Canada";

async fn setup() -> (TempDir, AppPaths, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path().join("data"));
    let db = Db::open(&paths).await.unwrap();
    let basic = ProfileBasic {
        name: "Ada Lovelace".into(),
        email: "ada@example.com".into(),
        ..ProfileBasic::default()
    };
    profile::set_basic(&db, basic).await.unwrap();
    (tmp, paths, db)
}

fn doc(bullets: &[&str]) -> ResumeDoc {
    ResumeDoc {
        name: "Ada Lovelace".into(),
        contact: vec!["ada@example.com".into()],
        summary: Vec::new(),
        sections: vec![Section {
            heading: "Experience".into(),
            entries: vec![Entry {
                title: String::new(),
                subtitle: String::new(),
                dates: String::new(),
                bullets: bullets.iter().map(|b| b.to_string()).collect(),
            }],
        }],
    }
}

fn input(text: &str, ids: &[&str]) -> ClaimInput {
    ClaimInput {
        text: text.into(),
        evidence_ids: ids.iter().map(|id| id.to_string()).collect(),
    }
}

fn item(id: &str, body: &str, source: &str, approved: bool, marked: bool) -> KnowledgeItem {
    KnowledgeItem {
        id: id.into(),
        kind: "note".into(),
        body: body.into(),
        source: source.into(),
        source_ref: None,
        approved,
        marked_sensitive: marked,
        sealed: false,
    }
}

async fn user_item(db: &Db, body: &str) -> String {
    knowledge::add(db, "note", body, "user", None)
        .await
        .unwrap()
        .id
}

async fn approved_parsed(db: &Db, body: &str) -> String {
    let id = knowledge::add(db, "note", body, "parsed", Some("draft:d#line:0"))
        .await
        .unwrap()
        .id;
    knowledge::approve(db, &id, true).await.unwrap();
    id
}

async fn uploaded(db: &Db, doc: ResumeDoc) -> drafts::Draft {
    drafts::create_version(
        db,
        "resume",
        None,
        "uploaded",
        "Base resume",
        None,
        None,
        doc,
        Vec::new(),
    )
    .await
    .unwrap()
}

/// A generated version, in a new lineage or `lineage`, with one claim per line.
async fn generated(
    db: &Db,
    lineage: Option<&str>,
    claims: Vec<ClaimInput>,
) -> jobpilot_core::Result<drafts::Draft> {
    let bullets: Vec<&str> = claims.iter().map(|c| c.text.as_str()).collect();
    let doc = doc(&bullets);
    drafts::create_version(
        db,
        "resume",
        lineage,
        "generated",
        "Tailored resume",
        None,
        Some("ollama"),
        doc,
        claims,
    )
    .await
}

async fn statuses(db: &Db, draft_id: &str) -> Vec<String> {
    let claims = drafts::claims(db, draft_id).await.unwrap();
    claims.into_iter().map(|c| c.status).collect()
}

fn blocked(result: &jobpilot_core::Result<std::path::PathBuf>) -> bool {
    matches!(result, Err(Error::Refused(reason)) if reason == "claims_blocked")
}

async fn export(
    db: &Db,
    paths: &AppPaths,
    draft_id: &str,
) -> jobpilot_core::Result<std::path::PathBuf> {
    drafts::export(
        db,
        draft_id,
        Template::Classic,
        ExportFormat::Pdf,
        &paths.exports(),
        "cli",
    )
    .await
}

fn exported_files(paths: &AppPaths) -> usize {
    match std::fs::read_dir(paths.exports()) {
        Ok(dir) => dir.count(),
        Err(_) => 0,
    }
}

#[tokio::test]
async fn versions_increment_and_never_overwrite() {
    let (_tmp, _paths, db) = setup().await;
    let id = user_item(&db, "Wrote the first program.").await;
    let v1 = uploaded(&db, doc(&["Wrote the first program."])).await;
    assert_eq!((v1.version, v1.origin.as_str()), (1, "uploaded"));

    let v2 = generated(
        &db,
        Some(&v1.lineage_id),
        vec![input("Wrote a program.", &[&id])],
    )
    .await
    .unwrap();
    assert_eq!((v2.version, v2.origin.as_str()), (2, "generated"));
    assert_eq!(v2.provider.as_deref(), Some("ollama"));

    let v3 = drafts::create_version(
        &db,
        "resume",
        Some(&v1.lineage_id),
        "edited",
        "Tailored resume",
        None,
        None,
        doc(&["Wrote programs."]),
        vec![input("Wrote programs.", &[&id])],
    )
    .await
    .unwrap();
    assert_eq!((v3.version, v3.origin.as_str()), (3, "edited"));

    let all = drafts::versions(&db, &v1.lineage_id).await.unwrap();
    assert_eq!(all.iter().map(|d| d.version).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(all[0].doc, doc(&["Wrote the first program."]));
    assert_eq!(drafts::get(&db, &v1.id).await.unwrap().doc, v1.doc);
    assert_eq!(drafts::list(&db, Some("resume")).await.unwrap().len(), 3);
    assert!(
        drafts::list(&db, Some("cover_letter"))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(drafts::list(&db, None).await.unwrap()[0].id, v3.id);
}

#[tokio::test]
async fn claim_without_evidence_is_flagged_and_blocks_export() {
    let (_tmp, paths, db) = setup().await;
    let draft = generated(&db, None, vec![input("Expert in Kubernetes", &[])])
        .await
        .unwrap();
    assert_eq!(statuses(&db, &draft.id).await, ["no_evidence"]);
    assert!(blocked(&export(&db, &paths, &draft.id).await));
    assert_eq!(exported_files(&paths), 0);
}

#[tokio::test]
async fn parsed_unapproved_item_is_not_evidence() {
    let (_tmp, _paths, db) = setup().await;
    let id = knowledge::add(&db, "skill", "Rust", "parsed", None)
        .await
        .unwrap()
        .id;
    let draft = generated(&db, None, vec![input("Rust", &[&id])])
        .await
        .unwrap();
    assert_eq!(statuses(&db, &draft.id).await, ["no_evidence"]);
}

#[tokio::test]
async fn generated_output_is_never_evidence() {
    let (_tmp, _paths, db) = setup().await;
    let real = user_item(&db, "Rust").await;
    let first = generated(&db, None, vec![input("Rust", &[&real])])
        .await
        .unwrap();
    let citing_a_draft = generated(&db, None, vec![input("Rust", &[&first.id])])
        .await
        .unwrap();
    assert_eq!(statuses(&db, &citing_a_draft.id).await, ["no_evidence"]);

    let claim = &drafts::claims(&db, &first.id).await.unwrap()[0];
    let citing_a_claim = generated(&db, None, vec![input("Rust", &[&claim.id])])
        .await
        .unwrap();
    assert_eq!(statuses(&db, &citing_a_claim.id).await, ["no_evidence"]);

    // One real id does not carry a foreign one.
    let mixed = generated(&db, None, vec![input("Rust", &[&real, &first.id])])
        .await
        .unwrap();
    assert_eq!(statuses(&db, &mixed.id).await, ["no_evidence"]);
}

#[tokio::test]
async fn authorization_statement_is_flagged_unless_user_entered_verbatim() {
    let (_tmp, paths, db) = setup().await;
    let parsed = approved_parsed(&db, AUTH).await;
    let draft = generated(&db, None, vec![input(AUTH, &[&parsed])])
        .await
        .unwrap();
    assert_eq!(statuses(&db, &draft.id).await, ["authorization_statement"]);
    assert!(blocked(&export(&db, &paths, &draft.id).await));

    // Two lines read as one on the page. Only the pair is flagged, not its neighbours.
    let split = generated(
        &db,
        None,
        vec![
            input("Rust", &[&parsed]),
            input("Authorized to", &[&parsed]),
            input("work in Canada", &[&parsed]),
            input("Python", &[&parsed]),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        statuses(&db, &split.id).await,
        [
            "ok",
            "authorization_statement",
            "authorization_statement",
            "ok"
        ]
    );
    assert!(blocked(&export(&db, &paths, &split.id).await));

    let own = user_item(&db, AUTH).await;
    let draft = generated(&db, None, vec![input(AUTH, &[&own])])
        .await
        .unwrap();
    assert_eq!(statuses(&db, &draft.id).await, ["ok"]);
    assert!(export(&db, &paths, &draft.id).await.is_ok());
}

#[test]
fn every_authorization_pattern_is_flagged_however_it_is_dressed() {
    let evidence = [item("e", "anything", "parsed", true, false)];
    let status = |text: &str| {
        check_claims(&[input(text, &["e"])], &evidence)[0]
            .status
            .clone()
    };
    for pattern in AUTHORIZATION_PATTERNS {
        assert_eq!(
            status(&format!("I hold {pattern} here")),
            "authorization_statement",
            "{pattern}"
        );
        assert_eq!(
            status(&format!("I HOLD {} HERE", pattern.to_uppercase())),
            "authorization_statement",
            "{pattern} upper-cased"
        );
    }
    for dressed in [
        "Holds a work-permit",
        "right-to-work in the UK",
        "authorized   to\n work",
        "vi\u{200b}sa holder",
        "vi\u{00ad}sa holder",
        "vi\u{7}sa holder",
        "authorized to\nwork",
        "green\tcard",
        "green\r\ncard",
        "vi\u{b}sa holder",
        "vi\u{c}sa holder",
        "vi\u{85}sa holder",
        "vi\u{fffe}sa holder",
        "vi\u{ffff}sa holder",
        "vi\u{e0100}sa holder",
        "vi\u{61c}sa holder",
        "vi\u{180e}sa holder",
        "vi\u{fff9}sa holder",
        "vi\u{206a}sa holder",
        "vi\u{115f}sa holder",
        "vi\u{1160}sa holder",
        "vi\u{3164}sa holder",
        "vi\u{ffa0}sa holder",
        "vi\u{2800}sa holder",
        "vi\u{200d}sa holder",
        "vi\u{200c}sa holder",
        "vi\u{fe0f}sa holder",
        "vi\u{e0001}sa holder",
        "visas issued",
        "H-1B holder",
        "h1b holder",
        "On OPT until June",
        "Holds an EAD",
        "Sponsored by my employer",
        "EU Blue Card",
        "German passport",
        "Permanent residency",
        "legally allowed here",
        "unauthorized to work",
        "citi\u{2060}zen of Spain",
        "\u{ff56}\u{ff49}\u{ff53}\u{ff41} holder",
        "v\u{0456}sa".replace('\u{0456}', "i\u{0301}").as_str(),
        "green\u{a0}card",
        "Canadian citizenship",
        "work authorisation",
    ] {
        assert_eq!(status(dressed), "authorization_statement", "{dressed}");
    }
    for honest in [
        "Built an authorization service",
        "It is advisable to test",
        "Improvisation classes",
        "A visage of calm",
        "Adopted Rust",
        "Option pricing",
        "Led a team",
        "Read widely",
        "Optimized queries",
        "Unsponsored side project",
    ] {
        assert_eq!(status(honest), "ok", "{honest}");
    }
    assert_eq!(
        status("Worked with citizens' data"),
        "authorization_statement"
    );
}

#[test]
fn the_user_entered_exemption_is_exact_and_needs_the_user() {
    let user = |body: &str| item("u", body, "user", true, false);
    let parsed = item("p", AUTH, "parsed", true, false);
    let status = |text: &str, ids: &[&str], evidence: &[KnowledgeItem]| {
        check_claims(&[input(text, ids)], evidence)[0]
            .status
            .clone()
    };
    // Case and whitespace only.
    let own = [user("  i AM authorized\n to   work in canada ")];
    assert_eq!(status(AUTH, &["u"], &own), "ok");
    // Another sentence, a longer sentence, and a parsed item are not what the user said.
    let other = [user("I am authorized to work in Spain")];
    assert_eq!(status(AUTH, &["u"], &other), "authorization_statement");
    let longer = [user("I am authorized to work in Canada and Spain")];
    assert_eq!(status(AUTH, &["u"], &longer), "authorization_statement");
    assert_eq!(
        status(AUTH, &["p"], std::slice::from_ref(&parsed)),
        "authorization_statement"
    );
    // A user item that is no longer evidence exempts nothing.
    let withdrawn = [item("u", AUTH, "user", false, false)];
    assert_eq!(status(AUTH, &["u"], &withdrawn), "authorization_statement");
    let marked = [item("u", AUTH, "user", true, true)];
    assert_eq!(status(AUTH, &["u"], &marked), "authorization_statement");
    // Still a claim: it has to cite something.
    assert_eq!(status(AUTH, &[], &own), "no_evidence");
    // With nothing cited and no exemption, the authorization status is the one reported.
    assert_eq!(status(AUTH, &[], &[]), "authorization_statement");
}

#[test]
fn check_claims_counts_only_approved_unmarked_items() {
    let status = |evidence: KnowledgeItem| {
        check_claims(&[input("Rust", &["e"])], &[evidence])[0]
            .status
            .clone()
    };
    assert_eq!(status(item("e", "Rust", "parsed", true, false)), "ok");
    assert_eq!(status(item("e", "Rust", "user", true, false)), "ok");
    assert_eq!(
        status(item("e", "Rust", "parsed", false, false)),
        "no_evidence"
    );
    assert_eq!(status(item("e", "Rust", "user", true, true)), "no_evidence");
    let claims = check_claims(
        &[input("a", &["e"]), input("b", &["f"])],
        &[item("e", "a", "user", true, false)],
    );
    assert_eq!(
        claims.iter().map(|c| c.position).collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(claims[1].evidence_ids, ["f"]);
}

#[tokio::test]
async fn compare_aligns_original_lines_with_generated_claims() {
    let (_tmp, _paths, db) = setup().await;
    let id = user_item(&db, "Wrote a program.").await;
    user_item(&db, "Evidence nothing cites.").await;
    let v1 = uploaded(&db, doc(&["Wrote a program.", "Read Menabrea."])).await;
    let v2 = generated(
        &db,
        Some(&v1.lineage_id),
        vec![input("Wrote a program.", &[&id]), input("Led a team", &[])],
    )
    .await
    .unwrap();

    let compare = drafts::compare(&db, &v2.id, None).await.unwrap();
    assert_eq!(compare.original, lines(&v1.doc));
    assert_eq!(compare.generated.len(), 2);
    assert_eq!(compare.generated[1].text, "Led a team");
    assert_eq!(compare.blocked, 1);
    assert_eq!(compare.evidence.len(), 1);
    assert_eq!(compare.evidence[0].id, id);

    // The default is the newest uploaded resume, whichever lineage it is in.
    let later = uploaded(&db, doc(&["A later upload."])).await;
    let by_default = drafts::compare(&db, &v2.id, None).await.unwrap();
    assert_eq!(by_default.original, lines(&later.doc));
    let chosen = drafts::compare(&db, &v2.id, Some(&v1.id)).await.unwrap();
    assert_eq!(chosen.original, lines(&v1.doc));
}

#[tokio::test]
async fn compare_of_a_cover_letter_has_no_default_original() {
    let (_tmp, _paths, db) = setup().await;
    uploaded(&db, doc(&["A resume line."])).await;
    let id = user_item(&db, "Why this role.").await;
    let letter = ResumeDoc {
        summary: vec!["Why this role.".into()],
        ..ResumeDoc::default()
    };
    let draft = drafts::create_version(
        &db,
        "cover_letter",
        None,
        "generated",
        "Letter",
        None,
        Some("ollama"),
        letter,
        vec![input("Why this role.", &[&id])],
    )
    .await
    .unwrap();
    let compare = drafts::compare(&db, &draft.id, None).await.unwrap();
    assert!(compare.original.is_empty());
    assert_eq!(compare.blocked, 0);
}

#[tokio::test]
async fn edit_and_remove_claim_create_new_versions() {
    let (_tmp, paths, db) = setup().await;
    let a = user_item(&db, "Wrote a program.").await;
    let b = user_item(&db, "Read Menabrea.").await;
    let mut body = doc(&["Wrote a program.", "Read Menabrea."]);
    body.summary = vec!["Analyst.".into()];
    body.sections[0].entries[0].title = "Engineer".into();
    let claims = vec![
        input("Analyst.", &[&a]),
        input("Engineer", &[&a]),
        input("Wrote a program.", &[&a]),
        input("Read Menabrea.", &[&b]),
    ];
    let v1 = drafts::create_version(
        &db,
        "resume",
        None,
        "generated",
        "Tailored",
        None,
        Some("ollama"),
        body.clone(),
        claims,
    )
    .await
    .unwrap();
    let stored = drafts::claims(&db, &v1.id).await.unwrap();

    let v2 = drafts::edit_claim(&db, &stored[2].id, input("Wrote programs.", &[&a]))
        .await
        .unwrap();
    assert_eq!((v2.version, v2.origin.as_str()), (2, "edited"));
    assert_eq!(v2.lineage_id, v1.lineage_id);
    assert_eq!(v2.title, "Tailored");
    assert_eq!(
        lines(&v2.doc),
        ["Analyst.", "Engineer", "Wrote programs.", "Read Menabrea."]
    );
    assert_eq!(drafts::get(&db, &v1.id).await.unwrap().doc, body);

    // A title is a claim unit too, so its line is the one that changes.
    let again = drafts::claims(&db, &v2.id).await.unwrap();
    let v3 = drafts::edit_claim(&db, &again[1].id, input("Analyst", &[&a]))
        .await
        .unwrap();
    assert_eq!(v3.doc.sections[0].entries[0].title, "Analyst");

    let last = drafts::claims(&db, &v3.id).await.unwrap();
    let v4 = drafts::remove_claim(&db, &last[2].id).await.unwrap();
    assert_eq!((v4.version, v4.origin.as_str()), (4, "edited"));
    assert_eq!(lines(&v4.doc), ["Analyst.", "Analyst", "Read Menabrea."]);
    let left = drafts::claims(&db, &v4.id).await.unwrap();
    assert_eq!(
        left.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        lines(&v4.doc)
    );
    assert_eq!(
        left.iter().map(|c| c.position).collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert!(export(&db, &paths, &v4.id).await.is_ok());
    assert_eq!(
        drafts::versions(&db, &v1.lineage_id).await.unwrap().len(),
        4
    );
}

#[tokio::test]
async fn an_edit_is_checked_again() {
    let (_tmp, paths, db) = setup().await;
    let id = user_item(&db, "Rust").await;
    let draft = generated(&db, None, vec![input("Rust", &[&id])])
        .await
        .unwrap();
    let claim = &drafts::claims(&db, &draft.id).await.unwrap()[0];

    let invented = drafts::edit_claim(&db, &claim.id, input("Kubernetes", &[]))
        .await
        .unwrap();
    assert_eq!(statuses(&db, &invented.id).await, ["no_evidence"]);
    assert!(blocked(&export(&db, &paths, &invented.id).await));

    let latest = &drafts::claims(&db, &invented.id).await.unwrap()[0];
    let stated = drafts::edit_claim(&db, &latest.id, input(AUTH, &[&id]))
        .await
        .unwrap();
    assert_eq!(statuses(&db, &stated.id).await, ["authorization_statement"]);
    assert!(blocked(&export(&db, &paths, &stated.id).await));

    let latest = &drafts::claims(&db, &stated.id).await.unwrap()[0];
    assert!(matches!(
        drafts::edit_claim(&db, &latest.id, input("  ", &[&id])).await,
        Err(Error::Validation(_))
    ));
    assert!(matches!(
        drafts::edit_claim(&db, "nope", input("Rust", &[&id])).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        drafts::remove_claim(&db, "nope").await,
        Err(Error::NotFound)
    ));
}

#[tokio::test]
async fn save_manual_keeps_matching_evidence_and_blocks_new_lines() {
    let (_tmp, paths, db) = setup().await;
    let id = user_item(&db, "Wrote a program.").await;
    let v1 = generated(&db, None, vec![input("Wrote a program.", &[&id])])
        .await
        .unwrap();

    let v2 = drafts::save_manual(
        &db,
        "resume",
        Some(&v1.lineage_id),
        doc(&["Wrote a program.", "Typed by hand."]),
    )
    .await
    .unwrap();
    assert_eq!((v2.version, v2.origin.as_str()), (2, "edited"));
    assert_eq!(v2.title, "Tailored resume");
    let claims = drafts::claims(&db, &v2.id).await.unwrap();
    assert_eq!(claims[0].status, "ok");
    assert_eq!(claims[0].evidence_ids, std::slice::from_ref(&id));
    assert_eq!(claims[1].status, "no_evidence");
    assert!(claims[1].evidence_ids.is_empty());
    assert!(blocked(&export(&db, &paths, &v2.id).await));

    // A flagged line typed again keeps its flag.
    let flagged = generated(&db, None, vec![input(AUTH, &[&id])])
        .await
        .unwrap();
    let saved = drafts::save_manual(&db, "resume", Some(&flagged.lineage_id), doc(&[AUTH]))
        .await
        .unwrap();
    assert_eq!(statuses(&db, &saved.id).await, ["authorization_statement"]);

    // No generated version in the lineage: the user's own text, no claims.
    let own = uploaded(&db, doc(&["A line."])).await;
    let edited = drafts::save_manual(
        &db,
        "resume",
        Some(&own.lineage_id),
        doc(&["Any line.", AUTH]),
    )
    .await
    .unwrap();
    assert_eq!(edited.version, 2);
    assert_eq!(edited.title, "Base resume");
    assert!(drafts::claims(&db, &edited.id).await.unwrap().is_empty());
    assert!(export(&db, &paths, &edited.id).await.is_ok());

    // No lineage at all: a new one, titled from the name.
    let fresh = drafts::save_manual(&db, "resume", None, doc(&["Mine."]))
        .await
        .unwrap();
    assert_eq!((fresh.version, fresh.title.as_str()), (1, "Ada Lovelace"));
    assert_eq!(fresh.origin, "edited");
    assert!(export(&db, &paths, &fresh.id).await.is_ok());

    assert!(matches!(
        drafts::save_manual(&db, "resume", Some("nope"), doc(&[])).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        drafts::save_manual(&db, "cover_letter", Some(&own.lineage_id), doc(&[])).await,
        Err(Error::Validation(_))
    ));
}

#[tokio::test]
async fn export_writes_docx_and_pdf_when_all_ok() {
    let (_tmp, paths, db) = setup().await;
    let id = user_item(&db, "Wrote a program.").await;
    let mut body = doc(&["Wrote a program."]);
    body.name = "Ada Lovelace".into();
    let draft = drafts::create_version(
        &db,
        "resume",
        None,
        "generated",
        "../Senior Engineer: Résumé!",
        None,
        Some("ollama"),
        body,
        vec![input("Wrote a program.", &[&id])],
    )
    .await
    .unwrap();

    for (format, name) in [
        (ExportFormat::Docx, "senior-engineer-résumé-v1.docx"),
        (ExportFormat::Pdf, "senior-engineer-résumé-v1.pdf"),
    ] {
        let path = drafts::export(
            &db,
            &draft.id,
            Template::Compact,
            format,
            &paths.exports(),
            "cli",
        )
        .await
        .unwrap();
        assert_eq!(path, paths.exports().join(name));
        assert!(std::fs::metadata(&path).unwrap().len() > 0);
    }
    assert_eq!(exported_files(&paths), 2);

    let recent = audit::recent(&db, 10).await.unwrap();
    assert_eq!(recent[0].action, "export");
    assert_eq!(recent[0].actor, "cli");
    assert_eq!(recent[0].target.as_deref(), Some(draft.id.as_str()));
    assert!(!recent[0].detail.to_string().contains("Wrote"));
    assert_eq!(recent.iter().filter(|e| e.action == "export").count(), 2);
}

#[tokio::test]
async fn export_of_an_unknown_draft_is_not_found() {
    let (_tmp, paths, db) = setup().await;
    assert!(matches!(
        export(&db, &paths, "nope").await,
        Err(Error::NotFound)
    ));
}

#[tokio::test]
async fn an_uploaded_version_exports_beside_a_blocked_generated_one() {
    let (_tmp, paths, db) = setup().await;
    let v1 = uploaded(&db, doc(&["Mine."])).await;
    let v2 = generated(&db, Some(&v1.lineage_id), vec![input("Invented", &[])])
        .await
        .unwrap();
    assert!(blocked(&export(&db, &paths, &v2.id).await));
    assert!(export(&db, &paths, &v1.id).await.is_ok());
}

// ---- user-owned fields -----------------------------------------------------------------

async fn generate_doc(db: &Db, body: ResumeDoc, id: &str) -> jobpilot_core::Result<drafts::Draft> {
    let claims = lines(&body).iter().map(|l| input(l, &[id])).collect();
    drafts::create_version(
        db,
        "resume",
        None,
        "generated",
        "Tailored",
        None,
        Some("ollama"),
        body,
        claims,
    )
    .await
}

#[tokio::test]
async fn a_generated_draft_cannot_set_a_heading_name_or_contact() {
    let (_tmp, _paths, db) = setup().await;
    let id = user_item(&db, "Rust").await;
    let before = drafts::list(&db, None).await.unwrap().len();

    let mut heading = doc(&["Rust"]);
    heading.sections[0].heading = "Certified Kubernetes Administrator".into();
    let mut name = doc(&["Rust"]);
    name.name = "Dr. Ada Lovelace, PhD".into();
    let mut contact = doc(&["Rust"]);
    contact.contact = vec!["Licensed pilot".into()];
    // The check is on what is shown, so an empty section is held to it too.
    let mut empty_section = doc(&["Rust"]);
    empty_section.sections.push(Section {
        heading: "Licensed Pilot".into(),
        entries: Vec::new(),
    });
    let profile_set = ProfileBasic {
        name: "Ada Lovelace".into(),
        email: "ada@example.com".into(),
        cities: vec!["London".into()],
        ..ProfileBasic::default()
    };
    profile::set_basic(&db, profile_set).await.unwrap();

    for (label, body) in [
        ("heading", heading),
        ("name", name),
        ("contact", contact),
        ("empty section", empty_section),
    ] {
        assert!(
            matches!(
                generate_doc(&db, body, &id).await,
                Err(Error::Validation(_))
            ),
            "{label}"
        );
    }
    assert_eq!(drafts::list(&db, None).await.unwrap().len(), before);
}

#[tokio::test]
async fn an_edited_version_of_a_generated_lineage_cannot_either() {
    let (_tmp, _paths, db) = setup().await;
    let id = user_item(&db, "Rust").await;
    let v1 = generated(&db, None, vec![input("Rust", &[&id])])
        .await
        .unwrap();
    let mut body = doc(&["Rust"]);
    body.sections[0].heading = "Certified X".into();
    let result = drafts::create_version(
        &db,
        "resume",
        Some(&v1.lineage_id),
        "edited",
        "Tailored",
        None,
        None,
        body,
        vec![input("Rust", &[&id])],
    )
    .await;
    assert!(matches!(result, Err(Error::Validation(_))));
}

#[tokio::test]
async fn profile_values_known_headings_and_the_users_own_drafts_are_allowed() {
    let (_tmp, _paths, db) = setup().await;
    let id = user_item(&db, "Rust").await;
    let basic = ProfileBasic {
        name: "Ada Lovelace".into(),
        email: "ada@example.com".into(),
        cities: vec!["London".into()],
        ..ProfileBasic::default()
    };
    profile::set_basic(&db, basic).await.unwrap();

    let mut from_profile = doc(&["Rust"]);
    from_profile.contact = vec!["ada@example.com".into(), "London".into()];
    from_profile.sections[0].heading = "WORK EXPERIENCE".into();
    let draft = generate_doc(&db, from_profile, &id).await.unwrap();
    assert_eq!(statuses(&db, &draft.id).await, ["ok"]);

    // An empty name or contact is what an unset profile gives.
    let mut blank = doc(&["Rust"]);
    blank.name = String::new();
    blank.contact = Vec::new();
    generate_doc(&db, blank, &id).await.unwrap();

    // What the user's own file or editor holds is theirs.
    let mut upload = doc(&["Line."]);
    upload.name = "A. Lovelace".into();
    upload.contact = vec!["+44 20 7946 0000".into()];
    upload.sections[0].heading = "Selected Papers".into();
    uploaded(&db, upload.clone()).await;
    let mut typed = doc(&["Line."]);
    typed.sections[0].heading = "Engines I Have Known".into();
    drafts::save_manual(&db, "resume", None, typed)
        .await
        .unwrap();

    let mut reused = upload;
    reused.sections[0].entries[0].bullets = vec!["Rust".into()];
    reused.sections.push(Section {
        heading: "Engines I Have Known".into(),
        entries: Vec::new(),
    });
    generate_doc(&db, reused, &id).await.unwrap();
}

#[tokio::test]
async fn the_user_may_retitle_a_heading_in_a_generated_lineage() {
    let (_tmp, paths, db) = setup().await;
    let id = user_item(&db, "Rust").await;
    let v1 = generated(&db, None, vec![input("Rust", &[&id])])
        .await
        .unwrap();
    let mut body = doc(&["Rust"]);
    body.sections[0].heading = "What I Know".into();
    let v2 = drafts::save_manual(&db, "resume", Some(&v1.lineage_id), body)
        .await
        .unwrap();
    assert_eq!(statuses(&db, &v2.id).await, ["ok"]);
    assert!(export(&db, &paths, &v2.id).await.is_ok());

    // A later edit of a claim keeps it, and a profile change does not strand the draft.
    profile::set_basic(
        &db,
        ProfileBasic {
            name: "Augusta King".into(),
            ..ProfileBasic::default()
        },
    )
    .await
    .unwrap();
    let claim = &drafts::claims(&db, &v2.id).await.unwrap()[0];
    let v3 = drafts::edit_claim(&db, &claim.id, input("Rust and C", &[&id]))
        .await
        .unwrap();
    assert_eq!(v3.doc.sections[0].heading, "What I Know");
    assert_eq!(v3.doc.name, "Ada Lovelace");
}

#[tokio::test]
async fn an_older_version_cannot_be_forked_to_bring_a_line_back() {
    let (_tmp, paths, db) = setup().await;
    let id = user_item(&db, "Rust").await;
    let v1 = generated(
        &db,
        None,
        vec![input("Rust", &[&id]), input("Unwanted line", &[&id])],
    )
    .await
    .unwrap();
    let old = drafts::claims(&db, &v1.id).await.unwrap();
    let v2 = drafts::remove_claim(&db, &old[1].id).await.unwrap();

    for result in [
        drafts::edit_claim(&db, &old[0].id, input("Rust and C", &[&id])).await,
        drafts::remove_claim(&db, &old[0].id).await,
    ] {
        assert!(matches!(result, Err(Error::Refused(r)) if r == "not_latest_version"));
    }
    assert_eq!(
        drafts::versions(&db, &v1.lineage_id).await.unwrap().len(),
        2
    );
    let kept = drafts::claims(&db, &v2.id).await.unwrap();
    let v3 = drafts::edit_claim(&db, &kept[0].id, input("Rust and C", &[&id]))
        .await
        .unwrap();
    assert_eq!(lines(&v3.doc), ["Rust and C"]);
    assert!(export(&db, &paths, &v3.id).await.is_ok());
}

/// The upload an attacker needs nothing else for: `sectionize` makes any ALL CAPS line a
/// heading and any `|` line contact, so these values are "the user's" by the stored check.
#[tokio::test]
async fn a_statement_in_an_uploaded_heading_or_contact_is_not_carried_into_a_generated_draft() {
    let (_tmp, _paths, db) = setup().await;
    let id = user_item(&db, "Rust").await;
    let mut upload = doc(&["Line."]);
    upload.contact = vec![
        "US Citizen".into(),
        "ada@example.com".into(),
        "Authorized to".into(),
        "work in Canada".into(),
    ];
    upload.sections[0].heading = "US CITIZEN NO SPONSORSHIP NEEDED".into();
    uploaded(&db, upload.clone()).await;
    let before = drafts::list(&db, None).await.unwrap().len();

    let mut heading = doc(&["Rust"]);
    heading.sections[0].heading = upload.sections[0].heading.clone();
    let mut contact = doc(&["Rust"]);
    contact.contact = vec!["US Citizen".into(), "ada@example.com".into()];
    let mut split = doc(&["Rust"]);
    split.contact = vec!["Authorized to".into(), "work in Canada".into()];
    let mut name = doc(&["Rust"]);
    name.name = "Visa Holder".into();
    uploaded(
        &db,
        ResumeDoc {
            name: "Visa Holder".into(),
            ..ResumeDoc::default()
        },
    )
    .await;
    let before = before + 1;

    for (label, body) in [
        ("heading", heading),
        ("contact", contact),
        ("split contact", split),
        ("name", name),
    ] {
        assert!(
            matches!(
                generate_doc(&db, body, &id).await,
                Err(Error::Validation(_))
            ),
            "{label}"
        );
    }
    assert_eq!(drafts::list(&db, None).await.unwrap().len(), before);
}

#[tokio::test]
async fn an_uploaded_heading_is_not_held_against_the_upload_itself() {
    let (_tmp, paths, db) = setup().await;
    let id = user_item(&db, "Rust").await;
    let mut upload = doc(&["Line."]);
    upload.sections[0].heading = "VISA STATUS".into();
    let v1 = uploaded(&db, upload).await;
    generated(&db, Some(&v1.lineage_id), vec![input("Rust", &[&id])])
        .await
        .unwrap();
    assert_eq!(drafts::compare(&db, &v1.id, None).await.unwrap().blocked, 0);
    assert!(export(&db, &paths, &v1.id).await.is_ok());
}

#[tokio::test]
async fn profile_values_and_verbatim_evidence_may_read_like_a_statement() {
    let (_tmp, _paths, db) = setup().await;
    let id = user_item(&db, "Rust").await;
    let basic = ProfileBasic {
        name: "Vera Visa".into(),
        email: "citizen.kane@example.com".into(),
        ..ProfileBasic::default()
    };
    profile::set_basic(&db, basic).await.unwrap();
    let mut body = doc(&["Rust"]);
    body.name = "Vera Visa".into();
    body.contact = vec!["citizen.kane@example.com".into()];
    generate_doc(&db, body, &id).await.unwrap();

    // The user can keep such a line, by saying it themselves.
    let mut upload = doc(&["Line."]);
    upload.sections[0].heading = "Sponsorship Work".into();
    uploaded(&db, upload).await;
    let mut reused = doc(&["Rust"]);
    reused.name = "Vera Visa".into();
    reused.contact = Vec::new();
    reused.sections[0].heading = "Sponsorship Work".into();
    assert!(matches!(
        generate_doc(&db, reused.clone(), &id).await,
        Err(Error::Validation(_))
    ));
    user_item(&db, "sponsorship  work").await;
    generate_doc(&db, reused, &id).await.unwrap();
}

#[tokio::test]
async fn the_user_cannot_type_a_statement_into_a_generated_lineage_but_can_elsewhere() {
    let (_tmp, paths, db) = setup().await;
    let id = user_item(&db, "Rust").await;
    let v1 = generated(&db, None, vec![input("Rust", &[&id])])
        .await
        .unwrap();
    let mut body = doc(&["Rust"]);
    body.sections[0].heading = "Visa Status".into();
    assert!(matches!(
        drafts::save_manual(&db, "resume", Some(&v1.lineage_id), body.clone()).await,
        Err(Error::Validation(_))
    ));
    let own = drafts::save_manual(&db, "resume", None, body)
        .await
        .unwrap();
    assert!(export(&db, &paths, &own.id).await.is_ok());
}

// ---- what create_version refuses -------------------------------------------------------

#[tokio::test]
async fn generated_claims_must_equal_the_lines() {
    let (_tmp, _paths, db) = setup().await;
    let id = user_item(&db, "Rust").await;
    let attempt = |body: ResumeDoc, claims: Vec<ClaimInput>| {
        let db = &db;
        async move {
            drafts::create_version(
                db,
                "resume",
                None,
                "generated",
                "T",
                None,
                Some("ollama"),
                body,
                claims,
            )
            .await
        }
    };
    let one = doc(&["Rust"]);
    let two = doc(&["Rust", "Expert in Kubernetes"]);
    for (body, claims) in [
        (one.clone(), Vec::new()),
        (two.clone(), vec![input("Rust", &[&id])]),
        (one.clone(), vec![input("Rust", &[&id]), input("x", &[&id])]),
        (one.clone(), vec![input("rust", &[&id])]),
        (
            two,
            vec![input("Expert in Kubernetes", &[]), input("Rust", &[&id])],
        ),
    ] {
        assert!(matches!(
            attempt(body, claims).await,
            Err(Error::Validation(_))
        ));
    }
    assert!(drafts::list(&db, None).await.unwrap().is_empty());
}

#[tokio::test]
async fn origins_and_lineages_are_held_to_their_rules() {
    let (_tmp, _paths, db) = setup().await;
    let id = user_item(&db, "Rust").await;
    let job = knowledge::import_job_text(&db, "Job", "A posting.")
        .await
        .unwrap()
        .id;
    let v1 = uploaded(&db, doc(&["Mine."])).await;
    let gen_lineage = generated(&db, None, vec![input("Rust", &[&id])])
        .await
        .unwrap()
        .lineage_id;
    let call = |kind: &'static str,
                lineage: Option<String>,
                origin: &'static str,
                claims: Vec<ClaimInput>| {
        let db = &db;
        async move {
            drafts::create_version(
                db,
                kind,
                lineage.as_deref(),
                origin,
                "T",
                None,
                None,
                doc(&["Rust"]),
                claims,
            )
            .await
        }
    };
    // An upload cannot slip an unchecked version into a lineage.
    assert!(matches!(
        call("resume", Some(gen_lineage.clone()), "uploaded", vec![]).await,
        Err(Error::Validation(_))
    ));
    // Only `save_manual` writes an edited version outside a generated lineage, even one
    // whose claims line up.
    let rust = || vec![input("Rust", &[&id])];
    assert!(matches!(
        call("resume", Some(v1.lineage_id.clone()), "edited", rust()).await,
        Err(Error::Validation(_))
    ));
    assert!(matches!(
        call("resume", None, "edited", rust()).await,
        Err(Error::Validation(_))
    ));
    // Claims on an upload would be dropped, so they are refused.
    assert!(matches!(
        call("resume", None, "uploaded", vec![input("Rust", &[&id])]).await,
        Err(Error::Validation(_))
    ));
    assert!(matches!(
        call("resume", Some("nope".into()), "generated", vec![]).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        call(
            "cover_letter",
            Some(v1.lineage_id.clone()),
            "generated",
            vec![input("Rust", &[&id])]
        )
        .await,
        Err(Error::Validation(_))
    ));
    assert!(matches!(
        call("memo", None, "uploaded", vec![]).await,
        Err(Error::Validation(_))
    ));
    assert!(matches!(
        call("resume", None, "invented", vec![]).await,
        Err(Error::Validation(_))
    ));
    for (provider, job_text) in [(Some("ollama"), None), (None, Some(job.as_str()))] {
        let result = drafts::create_version(
            &db,
            "resume",
            None,
            "uploaded",
            "T",
            job_text,
            provider,
            doc(&["Rust"]),
            Vec::new(),
        )
        .await;
        assert!(matches!(result, Err(Error::Validation(_))));
    }
    let blank = drafts::create_version(
        &db,
        "resume",
        None,
        "uploaded",
        "  ",
        None,
        None,
        doc(&[]),
        Vec::new(),
    )
    .await;
    assert!(matches!(blank, Err(Error::Validation(_))));
}

// ---- evidence that changes after the check ----------------------------------------------

#[tokio::test]
async fn export_and_compare_use_the_evidence_as_it_is_now() {
    let (_tmp, paths, db) = setup().await;
    let id = approved_parsed(&db, "Rust").await;
    let draft = generated(&db, None, vec![input("Rust", &[&id])])
        .await
        .unwrap();
    assert!(export(&db, &paths, &draft.id).await.is_ok());

    knowledge::approve(&db, &id, false).await.unwrap();
    assert_eq!(statuses(&db, &draft.id).await, ["no_evidence"]);
    assert!(blocked(&export(&db, &paths, &draft.id).await));
    let compare = drafts::compare(&db, &draft.id, None).await.unwrap();
    assert_eq!(compare.blocked, 1);
    assert!(compare.evidence.is_empty());

    knowledge::approve(&db, &id, true).await.unwrap();
    assert!(export(&db, &paths, &draft.id).await.is_ok());
    knowledge::remove(&db, &id).await.unwrap();
    assert!(blocked(&export(&db, &paths, &draft.id).await));
}

#[tokio::test]
async fn approval_after_the_fact_does_not_clear_a_stored_flag() {
    let (_tmp, paths, db) = setup().await;
    let id = knowledge::add(&db, "skill", "Rust", "parsed", None)
        .await
        .unwrap()
        .id;
    let draft = generated(&db, None, vec![input("Rust", &[&id])])
        .await
        .unwrap();
    knowledge::approve(&db, &id, true).await.unwrap();
    assert_eq!(statuses(&db, &draft.id).await, ["no_evidence"]);
    assert!(blocked(&export(&db, &paths, &draft.id).await));
}

#[tokio::test]
async fn a_claim_row_that_went_missing_blocks_export() {
    let (_tmp, paths, db) = setup().await;
    let id = user_item(&db, "Rust").await;
    let draft = generated(&db, None, vec![input("Rust", &[&id])])
        .await
        .unwrap();
    sqlx::query("DELETE FROM claims WHERE draft_id = ?")
        .bind(&draft.id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(blocked(&export(&db, &paths, &draft.id).await));
}

/// A marked row with its text still readable: `review` then has sealed text to work with and
/// no keychain to ask. The app never leaves a row like this; `mark_sensitive` seals the body.
async fn mark_in_place(db: &Db, id: &str) {
    sqlx::query("UPDATE knowledge_items SET marked_sensitive = 1 WHERE id = ?")
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn marked_text_reaches_the_check_from_the_database() {
    let (_tmp, paths, db) = setup().await;
    let good = user_item(&db, "Rust").await;
    let marked = user_item(&db, "Treated for a rare condition").await;
    mark_in_place(&db, &marked).await;

    let draft = generated(
        &db,
        None,
        vec![
            input("Rust", &[&good]),
            input("Was treated for a RARE condition.", &[&good]),
        ],
    )
    .await
    .unwrap();
    assert_eq!(statuses(&db, &draft.id).await, ["ok", "no_evidence"]);
    assert!(blocked(&export(&db, &paths, &draft.id).await));

    // Split over two lines, it is still on the page.
    let split = generated(
        &db,
        None,
        vec![
            input("Treated for a", &[&good]),
            input("rare condition", &[&good]),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        statuses(&db, &split.id).await,
        ["no_evidence", "no_evidence"]
    );

    // In a heading it has no claim to carry a flag, so the draft is refused.
    let mut upload = doc(&["Line."]);
    upload.sections[0].heading = "Treated for a rare condition".into();
    uploaded(&db, upload.clone()).await;
    let mut reused = doc(&["Rust"]);
    reused.sections[0].heading = upload.sections[0].heading.clone();
    assert!(matches!(
        generate_doc(&db, reused, &good).await,
        Err(Error::Validation(_))
    ));
}

#[tokio::test]
async fn a_marked_item_does_not_turn_a_stock_heading_into_a_statement() {
    let (_tmp, paths, db) = setup().await;
    let good = user_item(&db, "Rust").await;
    let references = user_item(&db, "References").await;
    let skills = user_item(&db, "Skills").await;
    let mut body = doc(&["Rust"]);
    body.sections[0].heading = "REFERENCES:".into();
    body.sections.push(Section {
        heading: "Technical Skills".into(),
        entries: Vec::new(),
    });
    let v1 = generate_doc(&db, body.clone(), &good).await.unwrap();
    mark_in_place(&db, &references).await;
    mark_in_place(&db, &skills).await;

    let v2 = generate_doc(&db, body, &good).await.unwrap();
    for draft in [&v1, &v2] {
        assert_eq!(
            drafts::compare(&db, &draft.id, None).await.unwrap().blocked,
            0
        );
        assert!(export(&db, &paths, &draft.id).await.is_ok());
    }
    let claim = &drafts::claims(&db, &v2.id).await.unwrap()[0];
    drafts::edit_claim(&db, &claim.id, input("Rust and C", &[&good]))
        .await
        .unwrap();

    // Only the stock heading: a heading that merely holds the text is still scanned.
    let mut upload = doc(&["Line."]);
    upload.sections[0].heading = "Skills of a Spy".into();
    uploaded(&db, upload).await;
    let mut other = doc(&["Rust"]);
    other.sections[0].heading = "Skills of a Spy".into();
    assert!(matches!(
        generate_doc(&db, other, &good).await,
        Err(Error::Validation(_))
    ));
}

#[tokio::test]
async fn text_marked_after_the_draft_blocks_an_owned_field_at_export() {
    let (_tmp, paths, db) = setup().await;
    let good = user_item(&db, "Rust").await;
    let later = user_item(&db, "Selected Papers").await;
    let mut upload = doc(&["Line."]);
    upload.sections[0].heading = "Selected Papers".into();
    uploaded(&db, upload).await;
    let mut body = doc(&["Rust"]);
    body.sections[0].heading = "Selected Papers".into();
    let draft = generate_doc(&db, body, &good).await.unwrap();
    assert!(export(&db, &paths, &draft.id).await.is_ok());
    assert_eq!(
        drafts::compare(&db, &draft.id, None).await.unwrap().blocked,
        0
    );

    mark_in_place(&db, &later).await;
    assert_eq!(
        drafts::compare(&db, &draft.id, None).await.unwrap().blocked,
        1
    );
    assert!(blocked(&export(&db, &paths, &draft.id).await));
}

/// A marked body sealed under a key this root does not have, without touching a keychain.
async fn seal_unreadably(db: &Db, id: &str) {
    let mut connection = db.pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE knowledge_items SET body = '', body_enc = x'00', key_id = 'gone', \
         marked_sensitive = 1 WHERE id = ?",
    )
    .bind(id)
    .execute(&mut *connection)
    .await
    .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *connection)
        .await
        .unwrap();
}

/// The list no longer opens bodies, so the export gate must read them itself: a sealed item
/// with no key anywhere still stops the export, and nothing is written.
#[tokio::test]
async fn a_marked_item_still_stops_export_with_no_key() {
    let (_tmp, paths, db) = setup().await;
    let good = user_item(&db, "Rust").await;
    let draft = generated(&db, None, vec![input("Rust", &[&good])])
        .await
        .unwrap();
    // A sealed row naming a key this root never had: no keychain entry can exist.
    let sealed = user_item(&db, "A sealed sentence.").await;
    seal_unreadably(&db, &sealed).await;

    let result = export(&db, &paths, &draft.id).await;

    assert!(
        matches!(result, Err(Error::Locked | Error::KeyMissing { .. })),
        "{result:?}"
    );
    assert_eq!(exported_files(&paths), 0);
}

#[tokio::test]
async fn export_refuses_while_a_sealed_text_cannot_be_read() {
    let (_tmp, paths, db) = setup().await;
    let good = user_item(&db, "Rust").await;
    let sealed = user_item(&db, "A sealed sentence.").await;
    let draft = generated(&db, None, vec![input("Rust", &[&good])])
        .await
        .unwrap();
    let own = uploaded(&db, doc(&["Mine."])).await;

    seal_unreadably(&db, &sealed).await;
    assert!(matches!(
        export(&db, &paths, &draft.id).await,
        Err(Error::KeyMissing { .. })
    ));
    assert_eq!(exported_files(&paths), 0);
    // Compare still works, and the user's own version needs no key.
    assert_eq!(
        drafts::compare(&db, &draft.id, None).await.unwrap().blocked,
        0
    );
    assert!(export(&db, &paths, &own.id).await.is_ok());
    // A generated version can still be stored; the check waits at the export.
    let later = generated(&db, None, vec![input("Rust", &[&good])])
        .await
        .unwrap();
    assert_eq!(statuses(&db, &later.id).await, ["ok"]);
}

// ---- sealed text, with a real key -------------------------------------------------------

fn entry(key_id: &str) -> keyring::Entry {
    let service = std::env::var("JOBPILOT_SECRET_SERVICE")
        .expect("set JOBPILOT_SECRET_SERVICE=jobpilot-test");
    assert_ne!(service, "io.github.gvsrusa.jobpilot");
    keyring::Entry::new(&service, &format!("{KEY_ACCOUNT_PREFIX}{key_id}")).unwrap()
}

struct EntryGuard(String);

impl Drop for EntryGuard {
    fn drop(&mut self) {
        let _ = entry(&self.0).delete_credential();
    }
}

#[tokio::test]
#[ignore = "uses the OS keychain"]
async fn marked_text_and_authorization_notes_stay_out_of_generated_text() {
    let (_tmp, paths, db) = setup().await;
    let KeyState::Unlocked { key_id } = crypto::state(&db).await.unwrap() else {
        panic!("keychain did not unlock");
    };
    let _guard = EntryGuard(key_id);

    let good = user_item(&db, "Rust").await;
    let marked = user_item(&db, "Treated for a rare condition in 2019.").await;
    knowledge::mark_sensitive(&db, &marked, true).await.unwrap();
    profile::set_authorization(
        &db,
        WorkAuthorization {
            country: "PT".into(),
            authorized: Stated::Yes,
            needs_sponsorship: Stated::No,
            note: "Residence card D7 valid to 2031".into(),
        },
    )
    .await
    .unwrap();

    let draft = generated(
        &db,
        None,
        vec![
            input("Rust", &[&good]),
            input(
                "Was treated for a rare condition in 2019 at work.",
                &[&good],
            ),
            input("Holds a residence card d7 valid to 2031.", &[&good]),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        statuses(&db, &draft.id).await,
        ["ok", "no_evidence", "authorization_statement"]
    );
    assert!(blocked(&export(&db, &paths, &draft.id).await));

    // Sealed after the draft was checked: the export sees it.
    let late = user_item(&db, "Rust").await;
    let draft = generated(&db, None, vec![input("Rust", &[&late])])
        .await
        .unwrap();
    assert_eq!(statuses(&db, &draft.id).await, ["ok"]);
    knowledge::mark_sensitive(&db, &late, true).await.unwrap();
    assert!(blocked(&export(&db, &paths, &draft.id).await));
}

// ---- a statement across lines -----------------------------------------------------------

const STATED: &str = "authorization_statement";

fn entry_doc(title: &str, subtitle: &str, dates: &str, bullets: &[&str]) -> ResumeDoc {
    let mut body = doc(bullets);
    let entry = &mut body.sections[0].entries[0];
    entry.title = title.into();
    entry.subtitle = subtitle.into();
    entry.dates = dates.into();
    body
}

async fn every_template_refuses(db: &Db, paths: &AppPaths, draft_id: &str) {
    for template in [Template::Classic, Template::Compact] {
        let exported = drafts::export(
            db,
            draft_id,
            template,
            ExportFormat::Docx,
            &paths.exports(),
            "cli",
        )
        .await;
        assert!(blocked(&exported), "{exported:?}");
    }
    assert_eq!(exported_files(paths), 0);
}

#[tokio::test]
async fn a_statement_over_three_fields_of_an_entry_is_flagged() {
    let (_tmp, paths, db) = setup().await;
    let id = user_item(&db, "Shipped the compiler").await;

    let body = entry_doc("authorized", "to", "work", &[]);
    let draft = generate_doc(&db, body, &id).await.unwrap();

    assert_eq!(statuses(&db, &draft.id).await, [STATED; 3]);
    every_template_refuses(&db, &paths, &draft.id).await;
}

#[tokio::test]
async fn a_statement_compact_prints_on_one_row_flags_only_its_lines() {
    let (_tmp, paths, db) = setup().await;
    let id = user_item(&db, "Shipped the compiler").await;

    // `lines` has the subtitle between these two; Compact prints them side by side.
    let body = entry_doc(
        "Authorized to",
        "Acme",
        "work in the United States",
        &["Shipped the compiler"],
    );
    let draft = generate_doc(&db, body, &id).await.unwrap();

    assert_eq!(statuses(&db, &draft.id).await, [STATED, "ok", STATED, "ok"]);
    every_template_refuses(&db, &paths, &draft.id).await;
}

#[tokio::test]
async fn compact_reads_a_statement_from_the_dates_into_the_subtitle() {
    let (_tmp, _paths, db) = setup().await;
    let id = user_item(&db, "Shipped the compiler").await;

    let body = entry_doc("Engineer", "work in Portugal", "Authorized to", &[]);
    let draft = generate_doc(&db, body, &id).await.unwrap();

    assert_eq!(statuses(&db, &draft.id).await, ["ok", STATED, STATED]);
}

#[test]
fn a_statement_over_any_neighbouring_lines_flags_those_lines_alone() {
    let evidence = [item("e", "x", "user", true, false)];
    let flag = |texts: &[&str]| -> Vec<String> {
        let claims: Vec<ClaimInput> = texts.iter().map(|t| input(t, &["e"])).collect();
        let checked = check_claims(&claims, &evidence);
        checked.into_iter().map(|claim| claim.status).collect()
    };
    assert_eq!(
        flag(&["Rust", "authorized", "to", "work", "Go"]),
        ["ok", STATED, STATED, STATED, "ok"]
    );
    assert_eq!(
        flag(&["able", "to", "work", "in", "Portugal", "Go"]),
        [STATED, STATED, STATED, STATED, STATED, "ok"]
    );
    // The same words held apart by what lies between them.
    assert_eq!(
        flag(&["Authorized", "payments to", "work orders", "in Rust"]),
        ["ok"; 4]
    );
}

#[tokio::test]
async fn an_ordinary_entry_is_not_flagged() {
    let (_tmp, _paths, db) = setup().await;
    let id = user_item(&db, "Shipped the compiler").await;

    let body = entry_doc(
        "Engineer",
        "Acme",
        "2019 to 2024",
        &["Able to work in a team", "Authorized payments"],
    );
    let draft = generate_doc(&db, body, &id).await.unwrap();

    assert_eq!(statuses(&db, &draft.id).await, ["ok"; 5]);
}
