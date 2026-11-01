//! The `#[ignore]`d tests use the real OS keychain, under the service from
//! `.cargo/config.toml`. The rest never reach it.

use jobpilot_core::crypto::{self, KEY_ACCOUNT_PREFIX, KeyState};
use jobpilot_core::profile::{
    self, EmploymentType, ProfileBasic, Stated, WorkAuthorization, WorkMode,
};
use jobpilot_core::{AppPaths, Db, Error, audit, transfer};
use tempfile::TempDir;

const NOTE: &str = "ZXQ-AUTH-NOTE";

fn temp_root() -> (TempDir, AppPaths) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::new(tmp.path().join("data"));
    (tmp, paths)
}

fn entry(key_id: &str) -> keyring::Entry {
    let service = std::env::var("JOBPILOT_SECRET_SERVICE")
        .expect("set JOBPILOT_SECRET_SERVICE=jobpilot-test");
    assert_ne!(service, "io.github.gvsrusa.jobpilot");
    keyring::Entry::new(&service, &format!("{KEY_ACCOUNT_PREFIX}{key_id}")).unwrap()
}

/// Removes a key's keychain entry even when an assertion fails first.
struct EntryGuard(String);

impl Drop for EntryGuard {
    fn drop(&mut self) {
        let _ = entry(&self.0).delete_credential();
    }
}

async fn unlock(db: &Db) -> EntryGuard {
    let KeyState::Unlocked { key_id } = crypto::state(db).await.unwrap() else {
        panic!("keychain did not unlock");
    };
    EntryGuard(key_id)
}

fn sample() -> ProfileBasic {
    let list = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    ProfileBasic {
        name: "Ana".into(),
        email: "ana@example.com".into(),
        countries: list(&["PT", "CA"]),
        regions: list(&["Lisboa", "Ontario"]),
        cities: list(&["Lisbon", "Toronto"]),
        currencies: list(&["EUR", "CAD"]),
        time_zones: list(&["Europe/Lisbon"]),
        languages: list(&["en", "pt"]),
        work_modes: vec![WorkMode::Remote, WorkMode::Hybrid],
        employment_types: vec![EmploymentType {
            country: "PT".into(),
            kind: "Contract".into(),
        }],
    }
}

fn auth(country: &str) -> WorkAuthorization {
    WorkAuthorization {
        country: country.into(),
        authorized: Stated::Yes,
        needs_sponsorship: Stated::No,
        note: NOTE.into(),
    }
}

async fn raw_row(db: &Db, country: &str) -> (Vec<u8>, String) {
    sqlx::query_as("SELECT ciphertext, key_id FROM work_authorization WHERE country = ?")
        .bind(country)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

type Change = dyn Fn(&mut ProfileBasic);

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test]
async fn basic_round_trips_every_multi_valued_field() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let saved = profile::set_basic(&db, sample()).await.unwrap();
    assert_eq!(saved, sample());
    assert_eq!(profile::basic(&db).await.unwrap(), sample());
    // Plain work never touched the keychain.
    assert_eq!(db.key_state(), KeyState::NotLoaded);
    db.close().await;
}

#[tokio::test]
async fn validation_rejects_bad_codes_and_empty_chips() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let cases: Vec<Box<Change>> = vec![
        Box::new(|b| b.countries = vec!["Portugal".into()]),
        Box::new(|b| b.currencies = vec!["EU".into()]),
        Box::new(|b| b.cities = vec!["".into()]),
        Box::new(|b| b.languages = vec!["  ".into()]),
        Box::new(|b| b.cities = vec!["x".into(); 51]),
        Box::new(|b| b.email = "nope".into()),
        Box::new(|b| b.name = "n".repeat(201)),
    ];
    for change in cases {
        let mut basic = ProfileBasic::default();
        change(&mut basic);
        let result = profile::set_basic(&db, basic).await;
        assert!(matches!(result, Err(Error::Validation(_))), "{result:?}");
    }
    assert_eq!(profile::basic(&db).await.unwrap(), ProfileBasic::default());
    db.close().await;
}

#[tokio::test]
async fn lower_case_codes_are_stored_upper_case() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let basic = ProfileBasic {
        countries: vec![" pt ".into()],
        currencies: vec!["eur".into()],
        ..ProfileBasic::default()
    };
    let saved = profile::set_basic(&db, basic).await.unwrap();
    assert_eq!(saved.countries, ["PT"]);
    assert_eq!(saved.currencies, ["EUR"]);
    db.close().await;
}

/// Every byte of the database file and its WAL, as SQLite leaves them on disk.
fn db_bytes(paths: &AppPaths) -> Vec<u8> {
    let mut all = Vec::new();
    for name in ["jobpilot.db", "jobpilot.db-wal"] {
        if let Ok(bytes) = std::fs::read(paths.root().join(name)) {
            all.extend(bytes);
        }
    }
    all
}

#[tokio::test]
#[ignore]
async fn authorization_is_ciphertext_at_rest() {
    const CONTROL: &str = "ZXQ-PLAIN-CONTROL";
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let _guard = unlock(&db).await;
    profile::set_authorization(&db, auth("PT")).await.unwrap();
    // A plain value written the same way, so the scan is proven able to find one.
    jobpilot_core::documents::create(&db, "control", CONTROL)
        .await
        .unwrap();

    let (blob, _) = raw_row(&db, "PT").await;
    assert!(blob.starts_with(b"JPK1"));
    assert!(!contains(&blob, NOTE.as_bytes()));
    let got = profile::get(&db).await.unwrap();
    assert_eq!(got.authorization, vec![auth("PT")]);
    assert!(!got.locked);

    // Open: the WAL is present and holds the writes.
    let open = db_bytes(&paths);
    assert!(contains(&open, CONTROL.as_bytes()), "scan cannot see plain");
    assert!(!contains(&open, NOTE.as_bytes()));
    db.close().await;
    let closed = db_bytes(&paths);
    assert!(contains(&closed, CONTROL.as_bytes()));
    assert!(!contains(&closed, NOTE.as_bytes()));
}

fn store_second_key(key_id: &str) -> [u8; 32] {
    let bytes = [7u8; 32];
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    entry(key_id).set_password(&hex).unwrap();
    bytes
}

/// A fresh id per test. The keychain tests run in parallel against one service, and a
/// fixed id let one test's cleanup or deletion remove the entry another was reading,
/// which showed up as `KeyMismatch` for that id.
fn fresh_key_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    format!("{:016x}", hasher.finish())
}

async fn seal_under_b(db: &Db, key_id: &str, bytes: [u8; 32], row: &WorkAuthorization) {
    let key_b = key_id;
    let key = jobpilot_core::crypto::DataKey::from_bytes(key_b, bytes);
    let plain = serde_json::to_vec(row).unwrap();
    let check = key
        .encrypt("data_keys", "key_check", key_b, crypto::key_check_text())
        .unwrap();
    sqlx::query(
        "INSERT OR IGNORE INTO data_keys (key_id, created_at, key_check, active) \
         VALUES (?, '2000-01-01T00:00:00Z', ?, 0)",
    )
    .bind(key_b)
    .bind(check)
    .execute(&db.pool)
    .await
    .unwrap();
    let sealed = key
        .encrypt("work_authorization", "row", &row.country, &plain)
        .unwrap();
    sqlx::query(
        "INSERT INTO work_authorization (country, ciphertext, key_id, updated_at) \
         VALUES (?, ?, ?, 't')",
    )
    .bind(&row.country)
    .bind(sealed)
    .bind(key_b)
    .execute(&db.pool)
    .await
    .unwrap();
}

#[tokio::test]
#[ignore]
async fn rows_under_two_keys_are_both_read() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let _a = unlock(&db).await;
    let key_b = fresh_key_id();
    let _b = EntryGuard(key_b.clone());
    profile::set_authorization(&db, auth("PT")).await.unwrap();
    seal_under_b(&db, &key_b, store_second_key(&key_b), &auth("CA")).await;

    let got = profile::get(&db).await.unwrap();
    assert!(!got.locked, "{:?}", got.key_state);
    assert_eq!(got.authorization, vec![auth("CA"), auth("PT")]);
    db.close().await;
}

#[tokio::test]
#[ignore]
async fn a_row_under_a_lost_key_locks_instead_of_failing() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let _a = unlock(&db).await;
    let key_b = fresh_key_id();
    let _b = EntryGuard(key_b.clone());
    profile::set_basic(&db, sample()).await.unwrap();
    profile::set_authorization(&db, auth("PT")).await.unwrap();
    seal_under_b(&db, &key_b, store_second_key(&key_b), &auth("CA")).await;
    entry(&key_b).delete_credential().unwrap();

    let got = profile::get(&db).await.unwrap();
    assert!(got.locked);
    assert!(got.authorization.is_empty());
    assert_eq!(got.basic, sample());
    assert!(
        matches!(
            got.key_state,
            KeyState::KeyMissing { .. } | KeyState::KeyMismatch { .. }
        ),
        "{:?}",
        got.key_state
    );

    let dir = transfer::export_all(&db, "cli").await.unwrap();
    assert!(dir.join("data.json").is_file());
    let text = std::fs::read_to_string(dir.join("work_authorization.json")).unwrap();
    assert!(text.contains("\"locked\""), "{text}");
    assert!(!text.contains(NOTE));
    let entries = audit::recent(&db, 10).await.unwrap();
    let export = entries.iter().find(|e| e.action == "export").unwrap();
    assert_eq!(export.detail["sensitive_plain"], false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("work_authorization.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    db.close().await;
}

#[tokio::test]
#[ignore]
async fn save_basic_while_locked_keeps_ciphertext() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let guard = unlock(&db).await;
    profile::set_authorization(&db, auth("PT")).await.unwrap();
    let before = raw_row(&db, "PT").await;
    db.close().await;

    entry(&guard.0).delete_credential().unwrap();
    let db = Db::open(&paths).await.unwrap();
    profile::set_basic(&db, sample()).await.unwrap();
    let got = profile::get(&db).await.unwrap();
    assert!(got.locked);
    assert!(got.authorization.is_empty());
    assert_eq!(got.basic, sample());
    assert_eq!(raw_row(&db, "PT").await, before);
    let result = profile::set_authorization(&db, auth("CA")).await;
    assert!(result.is_err(), "{result:?}");
    assert_eq!(raw_row(&db, "PT").await, before);
    db.close().await;
}

#[tokio::test]
async fn adding_a_country_never_adds_a_stated_value() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let basic = ProfileBasic {
        countries: vec!["DE".into()],
        ..ProfileBasic::default()
    };
    profile::set_basic(&db, basic).await.unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM work_authorization")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    db.close().await;
}

#[test]
fn stated_defaults_to_not_stated() {
    assert_eq!(Stated::default(), Stated::NotStated);
}

#[tokio::test]
#[ignore]
async fn export_writes_authorization_plain_and_audits_the_warning() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let _guard = unlock(&db).await;
    profile::set_authorization(&db, auth("PT")).await.unwrap();

    let dir = transfer::export_all(&db, "cli").await.unwrap();
    let text = std::fs::read_to_string(dir.join("work_authorization.json")).unwrap();
    assert!(text.contains("\"authorized\":\"yes\""), "{text}");
    let entries = audit::recent(&db, 10).await.unwrap();
    let export = entries.iter().find(|e| e.action == "export").unwrap();
    assert_eq!(export.detail["sensitive_plain"], true);
    db.close().await;
}

#[tokio::test]
async fn export_with_no_authorization_asks_nothing_of_the_keychain() {
    let (_tmp, paths) = temp_root();
    let db = Db::open(&paths).await.unwrap();
    let dir = transfer::export_all(&db, "cli").await.unwrap();
    let text = std::fs::read_to_string(dir.join("work_authorization.json")).unwrap();
    assert_eq!(text, "[]");
    assert_eq!(db.key_state(), KeyState::NotLoaded);
    db.close().await;
}
