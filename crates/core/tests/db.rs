use std::fs;
use std::path::{Path, PathBuf};

use jobpilot_core::ai::DataCategory;
use jobpilot_core::ai::consent::{self, ConsentState};
use jobpilot_core::crypto::KeyState;
use jobpilot_core::{AppPaths, Db, Error, MIGRATOR, app_schema_version};
use sqlx::SqlSafeStr;
use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
use tempfile::TempDir;

fn temp_root() -> (TempDir, AppPaths) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path().join("data"));
    (tmp, paths)
}

async fn raw_pool(file: &Path) -> SqlitePool {
    SqlitePool::connect_with(SqliteConnectOptions::new().filename(file))
        .await
        .unwrap()
}

async fn add_row(db: &Db) {
    sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES ('k', 'kept', 't')")
        .execute(&db.pool)
        .await
        .unwrap();
}

async fn read_row(pool: &SqlitePool) -> String {
    sqlx::query_scalar("SELECT value FROM settings WHERE key = 'k'")
        .fetch_one(pool)
        .await
        .unwrap()
}

fn migrator_plus(sql: &'static str) -> Migrator {
    let mut all: Vec<Migration> = MIGRATOR.iter().cloned().collect();
    all.push(Migration::new(
        9000,
        "test".into(),
        MigrationType::Simple,
        sql.into_sql_str(),
        false,
    ));
    Migrator::with_migrations(all)
}

fn snapshots(paths: &AppPaths, prefix: &str) -> Vec<PathBuf> {
    fs::read_dir(paths.backups())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(prefix)
        })
        .collect()
}

#[tokio::test]
async fn opens_and_migrates_fresh_root() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();

    for dir in [paths.documents(), paths.backups(), paths.exports()] {
        assert!(dir.is_dir(), "{}", dir.display());
    }
    assert!(paths.db().is_file());
    assert_eq!(db.schema_version().await.unwrap(), app_schema_version());
    db.close().await;

    // No data existed and nothing is pending on reopen, so no snapshot is due.
    let db = Db::open(&paths).await.unwrap();
    assert_eq!(fs::read_dir(paths.backups()).unwrap().count(), 0);
    db.close().await;
}

#[tokio::test]
async fn fts5_is_available() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    sqlx::query("CREATE VIRTUAL TABLE t USING fts5(x)")
        .execute(&db.pool)
        .await
        .unwrap();
    db.close().await;
}

#[tokio::test]
async fn pragmas_are_set() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(journal, "wal");
    assert_eq!(foreign_keys, 1);
    db.close().await;
}

#[tokio::test]
async fn opens_under_unicode_path_with_spaces() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path().join("José Ñ 日本語"));
    let db = Db::open(&paths).await.unwrap();
    assert_eq!(db.schema_version().await.unwrap(), app_schema_version());
    db.close().await;
    assert!(paths.db().is_file());
}

#[tokio::test]
async fn refuses_newer_schema() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    add_row(&db).await;
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (9999, 'future', 1, x'00', 0)",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    db.close().await;

    let err = Db::open(&paths).await.err().unwrap();
    assert!(matches!(err, Error::SchemaTooNew { db: 9999, .. }), "{err}");

    let pool = raw_pool(&paths.db()).await;
    assert_eq!(read_row(&pool).await, "kept");
    pool.close().await;
    assert_eq!(fs::read_dir(paths.backups()).unwrap().count(), 0);
}

#[tokio::test]
async fn snapshot_precedes_migration() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    add_row(&db).await;
    fs::write(paths.documents().join("cv.txt"), "my cv").unwrap();
    db.close().await;

    // The migration destroys the row, so the snapshot can only hold it if it came first.
    let migrator = migrator_plus("DELETE FROM settings;");
    let db = Db::open_with(&paths, &migrator).await.unwrap();
    assert_eq!(db.schema_version().await.unwrap(), 9000);
    db.close().await;

    let found = snapshots(&paths, "pre-migrate-");
    assert_eq!(found.len(), 1, "{found:?}");
    let snapshot = &found[0];

    let pool = raw_pool(&snapshot.join("jobpilot.db")).await;
    assert_eq!(read_row(&pool).await, "kept");
    pool.close().await;

    assert_eq!(
        fs::read_to_string(snapshot.join("documents").join("cv.txt")).unwrap(),
        "my cv"
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(snapshot.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["format"], 1);
    assert_eq!(manifest["schema_version"], app_schema_version());
    assert!(manifest["created_at"].is_string());
}

#[tokio::test]
async fn failed_migration_keeps_snapshot_and_data() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    add_row(&db).await;
    db.close().await;

    // The first statement succeeds, so the rollback is exercised, not just a parse error.
    let migrator = migrator_plus("DELETE FROM settings; THIS IS NOT SQL;");
    let err = Db::open_with(&paths, &migrator).await.err().unwrap();
    assert!(matches!(err, Error::MigrationFailed(_)), "{err}");

    assert_eq!(snapshots(&paths, "pre-migrate-").len(), 1);
    let pool = raw_pool(&paths.db()).await;
    assert_eq!(read_row(&pool).await, "kept");
    pool.close().await;

    // The failure left no lock behind and the database still opens at the old version.
    let db = Db::open(&paths).await.unwrap();
    assert_eq!(db.schema_version().await.unwrap(), app_schema_version());
    db.close().await;
}

// A 0.1 database gains the 0.2 tables with its data intact and no key: the key waits for
// the first sensitive use.
#[tokio::test]
async fn foundation_database_migrates_to_data_keys_without_a_key() {
    let (_tmp, paths) = temp_root();
    let foundation: Vec<Migration> = MIGRATOR
        .iter()
        .filter(|m| m.version == 1)
        .cloned()
        .collect();
    let db = Db::open_with(&paths, &Migrator::with_migrations(foundation))
        .await
        .unwrap();
    assert_eq!(db.schema_version().await.unwrap(), 1);
    add_row(&db).await;
    db.close().await;

    let db = Db::open(&paths).await.unwrap();

    assert_eq!(db.schema_version().await.unwrap(), app_schema_version());
    assert_eq!(read_row(&db.pool).await, "kept");
    assert_eq!(snapshots(&paths, "pre-migrate-1").len(), 1);
    assert_eq!(db.key_state(), KeyState::NotLoaded);
    for table in ["data_keys", "profile", "work_authorization"] {
        let rows: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(rows, 0, "{table}");
    }
    let orphan = sqlx::query(
        "INSERT INTO work_authorization (country, ciphertext, key_id, updated_at) \
         VALUES ('PT', x'00', 'no-such-key', 't')",
    )
    .execute(&db.pool)
    .await;
    assert!(orphan.is_err(), "ciphertext needs a listed key");
    let second_profile = sqlx::query("INSERT INTO profile VALUES ('other', '{}', 't')")
        .execute(&db.pool)
        .await;
    assert!(second_profile.is_err(), "one profile per root");
    db.close().await;
}

// A 0.2 database (schema 5) holds a document, a profile, a knowledge item, a job text, and a
// draft. Opening it with 0.3 keeps every row, leaves the new tables empty, and snapshots first.
#[tokio::test]
async fn a_0_2_database_upgrades_to_0_3() {
    let (_tmp, paths) = temp_root();
    let release_0_2: Vec<Migration> = MIGRATOR
        .iter()
        .filter(|m| m.version <= 5)
        .cloned()
        .collect();
    let db = Db::open_with(&paths, &Migrator::with_migrations(release_0_2))
        .await
        .unwrap();
    assert_eq!(db.schema_version().await.unwrap(), 5);
    for sql in [
        "INSERT INTO documents (id, title, body, created_at, updated_at) \
         VALUES ('d1', 'Old CV', 'document body', 't', 't')",
        "INSERT INTO profile (id, basic, updated_at) VALUES ('me', '{\"name\":\"Ada\"}', 't')",
        "INSERT INTO knowledge_items (id, kind, body, source, created_at, updated_at) \
         VALUES ('k1', 'skill', 'Rust', 'user', 't', 't')",
        "INSERT INTO job_texts (id, title, raw, normalized, created_at) \
         VALUES ('j1', 'Dev', 'raw posting', 'normal posting', 't')",
        "INSERT INTO drafts (id, kind, lineage_id, version, origin, title, job_text_id, body, \
         created_at) VALUES ('dr1', 'resume', 'l1', 1, 'generated', 'Draft', 'j1', '{}', 't')",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&db.pool)
            .await
            .unwrap();
    }
    db.close().await;

    let db = Db::open(&paths).await.unwrap();

    assert_eq!(db.schema_version().await.unwrap(), app_schema_version());
    assert_eq!(snapshots(&paths, "pre-migrate-5").len(), 1);
    for (sql, want) in [
        (
            "SELECT body FROM documents WHERE id = 'd1'",
            "document body",
        ),
        (
            "SELECT basic FROM profile WHERE id = 'me'",
            "{\"name\":\"Ada\"}",
        ),
        ("SELECT body FROM knowledge_items WHERE id = 'k1'", "Rust"),
        (
            "SELECT normalized FROM job_texts WHERE id = 'j1'",
            "normal posting",
        ),
        ("SELECT title FROM drafts WHERE id = 'dr1'", "Draft"),
    ] {
        let got: String = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(got, want, "{sql}");
    }
    let run_id: Option<String> = sqlx::query_scalar("SELECT run_id FROM drafts WHERE id = 'dr1'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(run_id, None, "an old draft has no run");
    for table in [
        "applications",
        "application_events",
        "memory_questions",
        "memory_answers",
        "application_answers",
        "memory_embeddings",
        "agent_runs",
        "agent_run_log",
        "reports",
    ] {
        let rows: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(rows, 0, "{table}");
    }
    db.close().await;
}

#[tokio::test]
async fn a_fresh_database_is_schema_17() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    assert_eq!(db.schema_version().await.unwrap(), 17);
    assert_eq!(app_schema_version(), 17);
    db.close().await;
}

// A 0.3 database (schema 11) holds a Claude consent record. Opening it with 0.4 keeps the
// record, snapshots first, and reads it as suspended until the user confirms again.
#[tokio::test]
async fn a_0_3_database_upgrades_to_0_4() {
    const CLAUDE: &str = "claude_code/cli_login";
    let (_tmp, paths) = temp_root();
    let release_0_3: Vec<Migration> = MIGRATOR
        .iter()
        .filter(|m| m.version <= 11)
        .cloned()
        .collect();
    let db = Db::open_with(&paths, &Migrator::with_migrations(release_0_3))
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO consent (provider, categories, given_at, terms_date) \
         VALUES (?, '[\"job_text\",\"resume_text\"]', '2026-10-01T00:00:00.000Z', '2026-11-01')",
    )
    .bind(CLAUDE)
    .execute(&db.pool)
    .await
    .unwrap();
    db.close().await;

    let db = Db::open(&paths).await.unwrap();

    assert_eq!(db.schema_version().await.unwrap(), 17);
    assert_eq!(snapshots(&paths, "pre-migrate-11-").len(), 1);
    let kept: (String, String) = sqlx::query_as("SELECT provider, register_row FROM consent")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(kept, (CLAUDE.to_string(), String::new()));
    assert_eq!(
        consent::state(&db, CLAUDE).await.unwrap(),
        ConsentState::Suspended(vec![DataCategory::JobText, DataCategory::ResumeText])
    );
    assert_eq!(consent::active(&db, CLAUDE).await.unwrap(), None);
    db.close().await;
}

// One row the rules cannot read must not lock a person out of the rest. A job with no text
// cannot be made with foreign keys on, so this connection turns them off.
#[tokio::test]
async fn a_job_the_rules_cannot_read_does_not_stop_the_open() {
    let (_tmp, paths) = temp_root();
    let db = Db::open_with(&paths, &MIGRATOR).await.unwrap();
    let mut one = db.pool.acquire().await.unwrap();
    for sql in [
        "PRAGMA foreign_keys = OFF",
        "INSERT INTO jobs (id, job_text_id, connector, title, first_seen_at, last_seen_at) \
         VALUES ('bad', 'no-such-text', 'import', 'Bad', 't', 't')",
        "PRAGMA foreign_keys = ON",
        "INSERT INTO job_texts (id, title, raw, normalized, created_at) \
         VALUES ('j1', 'Dev', 'raw', 'We need Rust', 't')",
        "INSERT INTO jobs (id, job_text_id, connector, title, first_seen_at, last_seen_at) \
         VALUES ('j1', 'j1', 'import', 'Dev', 't', 't')",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&mut *one)
            .await
            .unwrap();
    }
    drop(one);
    db.close().await;

    let db = Db::open(&paths).await.unwrap();

    let by: String = sqlx::query_scalar("SELECT fields_by FROM jobs WHERE id = 'j1'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(by, "rules", "the row after the bad one is still read");
    db.close().await;
}

// A 0.4 database (schema 12) holds a job text and a draft citing it. Opening it with 0.5
// snapshots first, keeps every row, and adds one jobs row that carries the text's id.
#[tokio::test]
async fn a_0_4_database_upgrades_to_0_5() {
    let (_tmp, paths) = temp_root();
    let release_0_4: Vec<Migration> = MIGRATOR
        .iter()
        .filter(|m| m.version <= 12)
        .cloned()
        .collect();
    let db = Db::open_with(&paths, &Migrator::with_migrations(release_0_4))
        .await
        .unwrap();
    assert_eq!(db.schema_version().await.unwrap(), 12);
    for sql in [
        "INSERT INTO job_texts (id, title, raw, normalized, created_at) \
         VALUES ('j1', 'Dev', 'raw posting', 'We need Rust and Python. Pay is $150,000 per year. \
         5+ years of experience required. Must be authorized to work in the US.', \
         '2026-10-01T00:00:00.000Z')",
        "INSERT INTO job_texts (id, title, raw, normalized, created_at) \
         VALUES ('j2', 'Ops', 'raw posting', 'We need Rust', '2026-10-02T00:00:00.000Z')",
        "INSERT INTO drafts (id, kind, lineage_id, version, origin, title, job_text_id, body, \
         created_at) VALUES ('dr1', 'resume', 'l1', 1, 'generated', 'Draft', 'j1', '{}', 't')",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&db.pool)
            .await
            .unwrap();
    }
    db.close().await;

    // One row a person has set since, and one with a company: open_with stops at the migration.
    let db = Db::open_with(&paths, &MIGRATOR).await.unwrap();
    for sql in [
        "UPDATE jobs SET fields_by = 'user' WHERE id = 'j2'",
        "INSERT INTO companies (id, name, key, created_at) VALUES ('c1', 'Acme', 'acme', 't')",
        "UPDATE jobs SET company_id = 'c1' WHERE id = 'j1'",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&db.pool)
            .await
            .unwrap();
    }
    db.close().await;

    let db = Db::open(&paths).await.unwrap();

    assert_eq!(db.schema_version().await.unwrap(), 17);
    assert_eq!(snapshots(&paths, "pre-migrate-12-").len(), 1);
    let draft: String = sqlx::query_scalar("SELECT job_text_id FROM drafts WHERE id = 'dr1'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(draft, "j1");
    let rows: Vec<(String, String, String, String, String)> =
        sqlx::query_as("SELECT id, job_text_id, connector, title, fields_by FROM jobs LIMIT 1")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![(
            "j1".into(),
            "j1".into(),
            "import".into(),
            "Dev".into(),
            "rules".into()
        )]
    );
    // The rules read it as they read a job pasted today.
    let job = jobpilot_core::jobs::get(&db, "j1").await.unwrap();
    assert_eq!(job.skills, ["Python", "Rust"]);
    assert_eq!(job.seniority.as_deref(), Some("senior"));
    assert_eq!(job.pay.unwrap().min, Some(150_000));
    let (authorization, key): (i64, String) =
        sqlx::query_as("SELECT mentions_authorization, dedupe_key FROM jobs WHERE id = 'j1'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(authorization, 1);
    assert!(key.starts_with("acme|dev|"), "{key}");
    // A person's row is not read at all.
    let kept = jobpilot_core::jobs::get(&db, "j2").await.unwrap();
    assert!(kept.skills.is_empty());
    assert_eq!(kept.fields_by, "user");
    // It runs once: a row the rules have read is not read again at the next open.
    sqlx::query("UPDATE jobs SET skills = '[]' WHERE id = 'j1'")
        .execute(&db.pool)
        .await
        .unwrap();
    db.close().await;
    let db = Db::open(&paths).await.unwrap();
    let job = jobpilot_core::jobs::get(&db, "j1").await.unwrap();
    assert!(job.skills.is_empty());
    let found = jobpilot_core::jobs::list(
        &db,
        &jobpilot_core::jobs::JobFilter {
            query: Some("Rust".into()),
            ..Default::default()
        },
        10,
    )
    .await
    .unwrap();
    assert_eq!(found.len(), 2, "the backfilled rows are searchable");
    assert_eq!(jobpilot_core::jobs::text_id(&db, "j1").await.unwrap(), "j1");
    // The generator reads the text by the same id.
    let text = jobpilot_core::knowledge::get_job_text(&db, "j1")
        .await
        .unwrap();
    assert!(text.normalized.starts_with("We need Rust"));
    db.close().await;
}

// A 0.5 database (schema 15) holds an application and a job text. Opening it with 0.6 snapshots
// first, keeps every row, and adds the automation tables empty.
#[tokio::test]
async fn a_0_5_database_upgrades_to_0_6() {
    let (_tmp, paths) = temp_root();
    let release_0_5: Vec<Migration> = MIGRATOR
        .iter()
        .filter(|m| m.version <= 15)
        .cloned()
        .collect();
    let db = Db::open_with(&paths, &Migrator::with_migrations(release_0_5))
        .await
        .unwrap();
    assert_eq!(db.schema_version().await.unwrap(), 15);
    for sql in [
        "INSERT INTO job_texts (id, title, raw, normalized, created_at) \
         VALUES ('j1', 'Dev', 'raw posting', 'We need Rust', '2026-10-01T00:00:00.000Z')",
        "INSERT INTO applications (id, company, role, status, created_at, updated_at) \
         VALUES ('a1', 'Acme', 'Dev', 'saved', 't', 't')",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&db.pool)
            .await
            .unwrap();
    }
    db.close().await;

    let db = Db::open(&paths).await.unwrap();

    assert_eq!(db.schema_version().await.unwrap(), 17);
    assert_eq!(snapshots(&paths, "pre-migrate-15-").len(), 1);
    let kept: (String, String) = sqlx::query_as("SELECT id, company FROM applications")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(kept, ("a1".to_string(), "Acme".to_string()));
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM job_texts WHERE id = 'j1'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(jobs, 1);
    for table in [
        "apply_runs",
        "apply_items",
        "apply_pauses",
        "autonomous_grants",
    ] {
        let rows: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(rows, 0, "{table}");
    }
    db.close().await;
}

// Every earlier release's schema upgrades to the current one: snapshot first (except when already
// current), rows kept.
#[tokio::test]
async fn every_earlier_release_upgrades_to_1_0() {
    for (release, v) in [
        ("0.1", 1),
        ("0.2", 5),
        ("0.3", 11),
        ("0.4", 12),
        ("0.5", 15),
        ("0.6", 17),
    ] {
        let (_tmp, paths) = temp_root();
        let migrations: Vec<Migration> = MIGRATOR
            .iter()
            .filter(|m| m.version <= v)
            .cloned()
            .collect();
        let db = Db::open_with(&paths, &Migrator::with_migrations(migrations))
            .await
            .unwrap();
        assert_eq!(db.schema_version().await.unwrap(), v, "release {release}");
        let mut inserts = vec![
            "INSERT INTO documents (id, title, body, created_at, updated_at) \
             VALUES ('d1', 'Doc', 'body', 't', 't')",
        ];
        if v >= 6 {
            inserts.push(
                "INSERT INTO applications (id, company, role, status, created_at, updated_at) \
                 VALUES ('a1', 'Acme', 'Dev', 'saved', 't', 't')",
            );
        }
        for sql in inserts {
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .execute(&db.pool)
                .await
                .unwrap_or_else(|e| panic!("release {release}: insert failed: {e}"));
        }
        db.close().await;

        let db = Db::open(&paths).await.unwrap();

        assert_eq!(
            db.schema_version().await.unwrap(),
            jobpilot_core::db::app_schema_version(),
            "release {release}: reaches the current schema"
        );
        let docs: i64 = sqlx::query_scalar("SELECT count(*) FROM documents WHERE id = 'd1'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(docs, 1, "release {release}: document kept");
        if v >= 6 {
            let apps: i64 = sqlx::query_scalar("SELECT count(*) FROM applications WHERE id = 'a1'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
            assert_eq!(apps, 1, "release {release}: application kept");
        }
        let expected = usize::from(v < 17);
        assert_eq!(
            snapshots(&paths, &format!("pre-migrate-{v}-")).len(),
            expected,
            "release {release}: pre-migrate snapshot count"
        );
        db.close().await;
    }
}

/// S-7 (review-1.0): a root the app creates is user-only, wherever `JOBPILOT_HOME` points.
#[cfg(unix)]
#[tokio::test]
async fn a_new_data_root_is_user_only() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let mode = fs::metadata(paths.root()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700, "mode {mode:o}");
    db.close().await;
}
