//! Real credential-store behaviour that unit tests cannot show. The keychain tests are
//! `#[ignore]`d: they use the OS store under `JOBPILOT_SECRET_SERVICE` from
//! `.cargo/config.toml`, never the app's own service.

use std::path::{Path, PathBuf};

use jobpilot_core::crypto::{self, KEY_ACCOUNT_PREFIX, KeyState};
use jobpilot_core::{AppPaths, Db, secrets, transfer};

fn service() -> String {
    let service = std::env::var("JOBPILOT_SECRET_SERVICE")
        .expect("set JOBPILOT_SECRET_SERVICE=jobpilot-test");
    assert_ne!(service, "io.github.gvsrusa.jobpilot");
    service
}

/// With the entry gone, the answer is `KeyMissing` on every OS: absent is not locked.
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn no_entry_is_missing_on_this_os() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    let KeyState::Unlocked { key_id } = crypto::state(&db).await.unwrap() else {
        panic!("first use should make a key");
    };
    let name = format!("{KEY_ACCOUNT_PREFIX}{key_id}");
    let entry = keyring::Entry::new(&service(), &name).unwrap();
    entry.delete_credential().unwrap();

    assert!(secrets::get(&name).unwrap().is_none());
    let state = crypto::state(&db).await.unwrap();
    assert_eq!(state, KeyState::KeyMissing { key_id });
}

/// An over-long entry name makes CredRead fail with something other than NotFound.
/// That must stay an error (`Secret`), so `crypto` reports it as unavailable and
/// never as a missing entry.
#[cfg(target_os = "windows")]
#[tokio::test]
#[ignore = "touches the OS keychain"]
async fn credential_manager_error_maps_to_unavailable() {
    let name = "x".repeat(2000);
    let error = secrets::get(&name).unwrap_err();
    assert!(
        matches!(error, jobpilot_core::Error::Secret(_)),
        "{error:?}"
    );
}

/// A fake, and it stays one: no provider has ever seen it.
const SENTINEL: &str = "sk-ZXQ-sentinel-not-a-real-key";

/// Every table with a column holding `needle`, as text, shadow tables included.
async fn tables_holding(db: &Db, needle: &str) -> Vec<String> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_list WHERE schema = 'main' \
         AND type IN ('table', 'shadow') AND name NOT GLOB 'sqlite_*'",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let mut found = Vec::new();
    for table in tables {
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info(?) ORDER BY cid")
                .bind(&table)
                .fetch_all(&db.pool)
                .await
                .unwrap();
        for column in columns {
            let sql = format!(
                "SELECT count(*) FROM \"{table}\" WHERE instr(CAST(\"{column}\" AS TEXT), ?) > 0"
            );
            let hits: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
                .bind(needle)
                .fetch_one(&db.pool)
                .await
                .unwrap();
            if hits > 0 {
                found.push(table.clone());
                break;
            }
        }
    }
    found
}

/// Every file under `dir` whose bytes hold `needle`.
fn files_holding(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(files_holding(&path, needle));
        } else if std::fs::read(&path)
            .unwrap()
            .windows(needle.len())
            .any(|bytes| bytes == needle.as_bytes())
        {
            found.push(path);
        }
    }
    found
}

/// The two scans below find what is there, so an empty answer from them means something.
/// A row stands in for a stored key, so this one needs no keychain.
#[tokio::test]
async fn the_scans_find_what_they_look_for() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    sqlx::query("INSERT INTO secret_index (name, created_at) VALUES ('ai-key-scan', 't')")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(tables_holding(&db, "ai-key-scan").await, ["secret_index"]);
    assert!(tables_holding(&db, SENTINEL).await.is_empty());
    db.close().await;

    let dir = tmp.path().join("nested").join("deeper");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("hit.bin"), b"\x00ai-key-scan\xff").unwrap();
    std::fs::write(dir.join("miss.bin"), b"nothing").unwrap();
    assert_eq!(
        files_holding(&tmp.path().join("nested"), "ai-key-scan"),
        [dir.join("hit.bin")]
    );
}

#[ignore = "touches the OS keychain"]
#[tokio::test]
async fn provider_key_round_trips_and_appears_in_no_row_or_export() {
    service();
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    let name = secrets::api_key_name("openai");

    secrets::set_api_key(&db, "openai", SENTINEL, "test")
        .await
        .unwrap();
    assert_eq!(secrets::get(&name).unwrap().as_deref(), Some(SENTINEL));

    let exported = transfer::export_all(&db, "test").await.unwrap();
    assert_eq!(tables_holding(&db, SENTINEL).await, Vec::<String>::new());
    assert_eq!(files_holding(&exported, SENTINEL), Vec::<PathBuf>::new());
    let tables = tables_holding(&db, &name).await;
    assert!(tables.iter().any(|t| t == "secret_index"), "{tables:?}");
    assert!(
        !files_holding(&exported, &name).is_empty(),
        "the export lists the name"
    );

    secrets::delete_api_key(&db, "openai", "test")
        .await
        .unwrap();
    assert_eq!(secrets::get(&name).unwrap(), None);
    db.close().await;
}

#[ignore = "touches the OS keychain"]
#[tokio::test]
async fn a_key_of_the_longest_accepted_length_round_trips() {
    service();
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    let key = "k".repeat(512);
    secrets::set_api_key(&db, "anthropic", &key, "test")
        .await
        .unwrap();
    let name = secrets::api_key_name("anthropic");
    assert_eq!(secrets::get(&name).unwrap(), Some(key));
    secrets::delete_api_key(&db, "anthropic", "test")
        .await
        .unwrap();
    db.close().await;
}

#[ignore = "touches the OS keychain"]
#[tokio::test]
async fn delete_all_removes_provider_keys() {
    service();
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    secrets::set_api_key(&db, "google", SENTINEL, "test")
        .await
        .unwrap();
    let name = secrets::api_key_name("google");
    assert!(secrets::get(&name).unwrap().is_some());

    transfer::delete_all(db, "DELETE", true).await.unwrap();
    assert_eq!(secrets::get(&name).unwrap(), None);
}
