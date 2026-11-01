use std::io::{Cursor, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use jobpilot_core::parse::{self, MAX_FILE_BYTES, extract_text, import_resume};
use jobpilot_core::render::{Entry, ResumeDoc, Section, Template, lines, to_docx, to_pdf};
use jobpilot_core::{AppPaths, Db, Error};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn section(heading: &str, bullets: [&str; 3]) -> Section {
    Section {
        heading: heading.into(),
        entries: vec![Entry {
            bullets: bullets.map(String::from).to_vec(),
            ..Entry::default()
        }],
    }
}

fn doc() -> ResumeDoc {
    ResumeDoc {
        name: "Ada Lovelace".into(),
        contact: vec!["ada@example.com".into(), "London".into()],
        summary: vec![],
        sections: vec![
            section(
                "Experience",
                [
                    "Wrote the first program for the Analytical Engine.",
                    "Translated and annotated Menabrea's memoir on the engine.",
                    "Led a small group of correspondents on numerical methods.",
                ],
            ),
            section(
                "Skills",
                [
                    "Mathematics and analysis",
                    "Technical writing",
                    "Algorithm design",
                ],
            ),
        ],
    }
}

/// Run once with `--ignored`, then commit the files.
#[test]
#[ignore]
fn make_fixtures() {
    let dir = fixture("");
    std::fs::create_dir_all(&dir).unwrap();
    let docx = to_docx(&doc(), Template::Classic).unwrap();
    std::fs::write(dir.join("resume.docx"), &docx).unwrap();
    std::fs::write(
        dir.join("resume.pdf"),
        to_pdf(&doc(), Template::Classic).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("scanned.pdf"),
        to_pdf(&ResumeDoc::default(), Template::Classic).unwrap(),
    )
    .unwrap();

    // resume.docx plus one external relationship, copied entry by entry.
    let mut input = zip::ZipArchive::new(Cursor::new(&docx)).unwrap();
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for i in 0..input.len() {
        let mut file = input.by_index(i).unwrap();
        let name = file.name().to_string();
        let mut data = String::new();
        file.read_to_string(&mut data).unwrap();
        if name == "word/_rels/document.xml.rels" {
            data = data.replace(
                "</Relationships>",
                "<Relationship Id=\"rIdExt1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink\" Target=\"http://127.0.0.1:59321/x\" TargetMode=\"External\"/></Relationships>",
            );
            assert!(data.contains("59321"));
        }
        out.start_file(name, options).unwrap();
        out.write_all(data.as_bytes()).unwrap();
    }
    std::fs::write(
        dir.join("external-rel.docx"),
        out.finish().unwrap().into_inner(),
    )
    .unwrap();
}

async fn open() -> (tempfile::TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

async fn count(db: &Db, table: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

async fn round_trip(file: &str) {
    let (_tmp, db) = open().await;
    let wanted = lines(&doc());
    let done = import_resume(&db, &fixture(file), "Imported")
        .await
        .unwrap();
    assert_eq!(done.items, wanted.len());
    let rows: Vec<(String, String, Option<String>, bool)> = sqlx::query_as(
        "SELECT body, source, source_ref, approved FROM knowledge_items ORDER BY seq",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), wanted.len());
    for (n, (body, source, source_ref, approved)) in rows.iter().enumerate() {
        assert!(wanted.contains(body), "{body:?} not in {wanted:?}");
        assert_eq!(source, "parsed");
        assert_eq!(
            source_ref.as_deref(),
            Some(format!("draft:{}#line:{n}", done.draft_id).as_str())
        );
        assert!(!approved);
    }
    let (kind, origin, version, lineage, body): (String, String, i64, String, String) =
        sqlx::query_as("SELECT kind, origin, version, lineage_id, body FROM drafts")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        (kind.as_str(), origin.as_str(), version),
        ("resume", "uploaded", 1)
    );
    assert_eq!(lineage, done.lineage_id);
    let stored: ResumeDoc = serde_json::from_str(&body).unwrap();
    assert_eq!(stored.name, "Ada Lovelace");
    assert_eq!(lines(&stored).len(), wanted.len());
    // Heading and contact lines are not claim units.
    assert!(rows.iter().all(|r| r.0 != "Experience" && r.0 != "Skills"));
    assert!(rows.iter().all(|r| !r.0.contains("ada@example.com")));
}

#[tokio::test]
async fn docx_round_trips_lines_into_parsed_items() {
    round_trip("resume.docx").await;
}

#[tokio::test]
async fn pdf_round_trips_lines_into_parsed_items() {
    round_trip("resume.pdf").await;
}

#[test]
fn pdf_keeps_non_ascii_names() {
    let tmp = tempfile::tempdir().unwrap();
    let mut d = doc();
    d.name = "José Ñandú Жуков".into();
    let path = tmp.path().join("name.PDF");
    std::fs::write(&path, to_pdf(&d, Template::Classic).unwrap()).unwrap();
    assert!(extract_text(&path).unwrap().contains("José Ñandú Жуков"));
}

#[tokio::test]
async fn pdf_without_text_reports_no_text() {
    let (_tmp, db) = open().await;
    let result = import_resume(&db, &fixture("scanned.pdf"), "Scan").await;
    assert!(matches!(result, Err(Error::Validation(ref m)) if m == "no_text"));
    assert_eq!(count(&db, "drafts").await, 0);
    assert_eq!(count(&db, "knowledge_items").await, 0);
}

#[tokio::test]
async fn oversize_file_is_refused_before_parsing() {
    let (tmp, db) = open().await;
    let path = tmp.path().join("big.pdf");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(21 * 1024 * 1024).unwrap();
    let result = import_resume(&db, &path, "Big").await;
    assert!(matches!(
        result,
        Err(Error::TooLarge { limit }) if limit == MAX_FILE_BYTES
    ));
    assert_eq!(count(&db, "drafts").await, 0);
}

#[tokio::test]
async fn unknown_extension_is_refused() {
    let (tmp, db) = open().await;
    let path = tmp.path().join("resume.txt");
    std::fs::write(&path, "Ada\nExperience\n- x").unwrap();
    assert!(matches!(
        import_resume(&db, &path, "T").await,
        Err(Error::Validation(_))
    ));
    assert_eq!(count(&db, "drafts").await, 0);
}

#[tokio::test]
async fn directory_and_missing_paths_are_refused() {
    let (tmp, db) = open().await;
    let dir = tmp.path().join("folder.pdf");
    std::fs::create_dir(&dir).unwrap();
    assert!(import_resume(&db, &dir, "T").await.is_err());
    assert!(
        import_resume(&db, &tmp.path().join("none.docx"), "T")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn garbage_files_fail_cleanly() {
    let (tmp, db) = open().await;
    for name in ["junk.pdf", "junk.docx"] {
        let path = tmp.path().join(name);
        std::fs::write(&path, b"this is not a document at all").unwrap();
        let result = import_resume(&db, &path, "T").await;
        assert!(matches!(result, Err(Error::Validation(_))), "{name}");
    }
    assert_eq!(count(&db, "drafts").await, 0);
}

#[tokio::test]
async fn docx_with_external_relationship_is_parsed_without_fetching() {
    let listener = TcpListener::bind("127.0.0.1:59321").unwrap();
    listener.set_nonblocking(true).unwrap();
    let text = extract_text(&fixture("external-rel.docx")).unwrap();
    assert!(text.contains("Wrote the first program"));
    let (_tmp, db) = open().await;
    import_resume(&db, &fixture("external-rel.docx"), "Ext")
        .await
        .unwrap();
    assert!(
        matches!(listener.accept(), Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "something connected"
    );
}

#[test]
fn docx_zip_bomb_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("bomb.docx");
    let mut out = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    out.start_file(
        "word/document.xml",
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated),
    )
    .unwrap();
    let chunk = vec![b' '; 1024 * 1024];
    for _ in 0..200 {
        out.write_all(&chunk).unwrap();
    }
    out.finish().unwrap();
    assert!(std::fs::metadata(&path).unwrap().len() < 1024 * 1024);
    assert!(matches!(extract_text(&path), Err(Error::TooLarge { .. })));
}

#[test]
fn docx_text_is_paragraphs_only_with_entities() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("x.docx");
    let mut out = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    out.start_file(
        "word/document.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    out.write_all(
        br#"<?xml version="1.0"?><!DOCTYPE d [<!ENTITY e "boom">]><w:document><w:body>
<w:p><w:pPr><w:pStyle w:val="a>b"/></w:pPr><w:r><w:t xml:space="preserve">R&amp;D &#233; &e;</w:t></w:r><w:r><w:delText>gone</w:delText></w:r></w:p>
<w:p><w:r><w:t><![CDATA[cdata <ok>]]></w:t></w:r><w:r><w:instrText>FIELD</w:instrText></w:r></w:p>
</w:body></w:document>"#,
    )
    .unwrap();
    out.finish().unwrap();
    let text = extract_text(&path).unwrap();
    assert_eq!(text.lines().collect::<Vec<_>>(), ["R&D é ", "cdata <ok>"]);
}

#[test]
fn constants_match_the_brief() {
    assert_eq!(parse::MAX_FILE_BYTES, 20 * 1024 * 1024);
    assert_eq!(parse::MAX_TEXT_CHARS, 2_000_000);
    assert_eq!(parse::PARSE_TIMEOUT.as_secs(), 30);
}

#[tokio::test]
async fn hidden_characters_are_removed_and_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("h.docx");
    let mut out = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    out.start_file(
        "word/document.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    out.write_all(
        "<w:document><w:body><w:p><w:r><w:t>Ada</w:t></w:r></w:p><w:p><w:r><w:t>Skills</w:t></w:r></w:p>\
         <w:p><w:r><w:t>Ru\u{200b}st \u{202e}and\u{e0041} Cafe\u{301}</w:t></w:r></w:p>\
         <w:p><w:r><w:t>\u{200b}</w:t></w:r></w:p></w:body></w:document>"
            .as_bytes(),
    )
    .unwrap();
    out.finish().unwrap();
    let (_t, db) = open().await;
    let done = import_resume(&db, &path, "H").await.unwrap();
    assert_eq!(done.items, 1);
    assert!(done.warnings.iter().any(|w| w.contains("invisible")));
    let body: String = sqlx::query_scalar("SELECT body FROM knowledge_items")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(body, "Rust and Caf\u{e9}");
}
