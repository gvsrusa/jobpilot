//! The provider table against `docs/policy-register.md`, and consent for every provider.

use jobpilot_core::ai::claude_code::PATH_SETTING;
use jobpilot_core::ai::consent::{self, ConsentState, give_for};
use jobpilot_core::ai::register::{
    PROVIDERS, ProviderKind, REGISTER, date_read, info, info_for, register_date, row_text, rows_of,
};
use jobpilot_core::ai::{
    self, Capability, DataCategory, PROVIDER_NAMES, ProviderId, TaskType, set_enabled, set_order,
    sharing,
};
use jobpilot_core::{AppPaths, Db};
use tempfile::TempDir;

const CLAUDE: &str = "claude_code/cli_login";
const KEY: &str = "openai/api_key";
const ALL: [DataCategory; 3] = [
    DataCategory::JobText,
    DataCategory::ProfileBasic,
    DataCategory::ResumeText,
];

async fn setup() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    // A name that is no path, so `ai::status` never starts an installed `claude --version`.
    jobpilot_core::settings::set(&db, PATH_SETTING, &serde_json::json!("not-a-path"))
        .await
        .unwrap();
    (tmp, db)
}

#[test]
fn every_external_provider_has_its_register_rows() {
    for info in PROVIDERS {
        assert!(row_text(&info.id.to_string()).is_some(), "{}", info.name);
        assert!(!info.data_use.is_empty(), "{}", info.name);
        assert!(!info.destination.is_empty(), "{}", info.name);
    }
}

/// `row_text` splits on `" | "`, so a cell holding that text would move every cell after it.
#[test]
fn each_mapped_row_has_seven_cells_and_one_line() {
    for info in PROVIDERS {
        let (integration, operation) = info.vendor_row;
        let lines: Vec<&str> = REGISTER
            .lines()
            .filter(|line| line.starts_with(&format!("| {integration} | {operation} | ")))
            .collect();
        assert_eq!(lines.len(), 1, "{}", info.name);
        let cells = lines[0].trim_matches('|').split(" | ").count();
        assert_eq!(cells, 7, "{}", info.name);
        if let Some(prefix) = info.owner_row {
            let owners = REGISTER
                .lines()
                .filter(|line| line.starts_with(&format!("| {prefix}")))
                .count();
            assert_eq!(owners, 1, "{}", info.name);
        }
    }
}

#[test]
fn the_date_read_is_not_part_of_the_row_text() {
    let text = row_text("openai/api_key").unwrap();
    assert!(!text.contains("| 2026-11-01 |"));
    assert!(!text.contains("2026-11-01\n"));
    assert!(text.contains("not for consumer use") || text.contains("consumers or individuals"));
    assert_eq!(date_read(KEY).as_deref(), Some("2026-11-01"));
}

#[test]
fn a_moved_date_read_changes_nothing_and_a_changed_cell_does() {
    let later = REGISTER.replace(
        "| https://openai.com/policies/services-agreement/ | 2026-11-01 |",
        "| https://openai.com/policies/services-agreement/ | 2027-01-01 |",
    );
    assert_ne!(later, REGISTER);
    assert_eq!(rows_of(&later, KEY), rows_of(REGISTER, KEY));
    let reworded = REGISTER.replace("consumers or individuals", "people");
    assert_ne!(rows_of(&reworded, KEY), rows_of(REGISTER, KEY));
}

#[test]
fn the_owner_row_is_part_of_the_row_text() {
    let reworded = REGISTER.replace(
        "| Direct API: OpenAI, Anthropic, Google (paid key), OpenRouter |",
        "| Direct API: OpenAI, Anthropic, Google, OpenRouter |",
    );
    assert_ne!(reworded, REGISTER);
    assert_ne!(rows_of(&reworded, KEY), rows_of(REGISTER, KEY));
    let openai = rows_of(REGISTER, KEY).unwrap();
    assert!(openai.ends_with("|") && openai.contains("\n| Direct API: OpenAI"));
}

#[test]
fn windows_line_endings_change_nothing() {
    assert_eq!(
        rows_of(&REGISTER.replace('\n', "\r\n"), KEY),
        rows_of(REGISTER, KEY)
    );
    let text = rows_of(&REGISTER.replace('\n', "\r\n"), KEY).unwrap();
    assert!(!text.contains('\r'));
}

#[test]
fn the_register_date_comes_from_the_third_line() {
    assert_eq!(register_date(), "2026-11-01");
}

#[test]
fn left_out_providers_have_no_way_in() {
    for name in [
        "vertex_ai",
        "vertex",
        "gemini_free",
        "antigravity",
        "ollama_cloud",
        "chatgpt_plan",
    ] {
        assert!(info(name).is_none(), "{name}");
    }
    assert!(info("ollama").is_none(), "Ollama needs no consent record");
    assert!(info_for("openai/chatgpt_plan").is_none());
}

#[test]
fn the_ids_say_the_sign_in_method() {
    let keys: Vec<String> = ProviderId::ALL.iter().map(ToString::to_string).collect();
    assert_eq!(
        keys,
        [
            "ollama",
            "claude_code/cli_login",
            "no_ai",
            "openai/api_key",
            "anthropic/api_key",
            "google/paid_key",
            "openrouter/api_key",
            "endpoint/loopback",
            "gemini_cli/cli_login",
            "codex_cli/cli_login",
        ]
    );
    assert_eq!(info_for(KEY).unwrap().name, "openai");
    assert_eq!(info("google").unwrap().kind, ProviderKind::Api);
    assert_eq!(info("endpoint").unwrap().kind, ProviderKind::Endpoint);
    assert_eq!(info("codex_cli").unwrap().kind, ProviderKind::Cli);
    assert_eq!(
        serde_json::to_value(ProviderId::OpenAi).unwrap(),
        serde_json::json!("openai")
    );
    assert_eq!(
        serde_json::to_value(ProviderId::OpenRouter).unwrap(),
        serde_json::json!("openrouter")
    );
}

#[test]
fn only_codex_cannot_prove_no_tools() {
    for info in PROVIDERS {
        assert!(info.capabilities.contains(&Capability::StructuredOutput));
        assert_eq!(
            info.capabilities.contains(&Capability::NoTools),
            info.name != "codex_cli",
            "{}",
            info.name
        );
    }
}

#[test]
fn every_table_name_is_a_setting_name() {
    for info in PROVIDERS {
        assert!(PROVIDER_NAMES.contains(&info.name), "{}", info.name);
        assert_eq!(info.id.to_string().split('/').next(), Some(info.name));
    }
}

#[tokio::test]
async fn a_left_out_name_is_refused_by_every_setting() {
    let (_tmp, db) = setup().await;
    let enabled = set_enabled(&db, "vertex_ai", true).await.unwrap_err();
    assert_eq!(enabled.code(), "validation");
    assert!(enabled.to_string().contains("gemini_cli"), "{enabled}");
    assert_eq!(
        set_order(&db, &["antigravity".into()])
            .await
            .unwrap_err()
            .code(),
        "validation"
    );
}

#[tokio::test]
async fn every_new_provider_defaults_off_and_can_be_named() {
    let (_tmp, db) = setup().await;
    let status = ai::status(&db).await.unwrap();
    for name in PROVIDER_NAMES.iter().filter(|name| **name != "ollama") {
        assert_eq!(status.enabled[*name], false, "{name}");
    }
    set_enabled(&db, "openrouter", true).await.unwrap();
    let status = ai::status(&db).await.unwrap();
    assert_eq!(status.enabled["openrouter"], true);
    assert!(status.order.iter().any(|name| name == "openrouter"));
}

#[tokio::test]
async fn status_lists_every_provider_but_ollama_and_claude() {
    let (_tmp, db) = setup().await;
    sqlx::query("INSERT INTO secret_index (name, created_at) VALUES ('ai-key-openai', 't')")
        .execute(&db.pool)
        .await
        .unwrap();
    jobpilot_core::settings::set(&db, "ai.model.openai", &serde_json::json!("gpt-x"))
        .await
        .unwrap();
    give_for(&db, "anthropic", &ALL, false, "cli")
        .await
        .unwrap();

    let status = ai::status(&db).await.unwrap();

    let names: Vec<&str> = status.external.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "openai",
            "anthropic",
            "google",
            "openrouter",
            "endpoint",
            "gemini_cli",
            "codex_cli"
        ]
    );
    assert_eq!(status.register_date, "2026-11-01");
    let openai = &status.external[0];
    assert!(openai.key_stored);
    assert_eq!(openai.model.as_deref(), Some("gpt-x"));
    assert!(!openai.available);
    assert_eq!(openai.reason.as_deref(), Some("not_built"));
    assert_eq!(openai.consent, None);
    assert!(openai.runs_job_text);
    assert_eq!(status.external[1].consent.as_deref(), Some(&ALL[..]));
    assert!(!status.external[1].key_stored);
    assert!(
        !status.external[6].runs_job_text,
        "Codex proves no no_tools"
    );
    assert!(!status.claude_code.suspended);
}

#[tokio::test]
async fn a_changed_row_suspends_and_a_new_consent_lifts_it() {
    let (_tmp, db) = setup().await;
    consent::give(&db, CLAUDE, &ALL, "2026-11-01", "gui")
        .await
        .unwrap();
    assert!(matches!(
        consent::state(&db, CLAUDE).await.unwrap(),
        ConsentState::Active(_)
    ));
    sqlx::query("UPDATE consent SET register_row = 'older words'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        consent::state(&db, CLAUDE).await.unwrap(),
        ConsentState::Suspended(ALL.to_vec())
    );
    assert_eq!(consent::active(&db, CLAUDE).await.unwrap(), None);
    let status = ai::status(&db).await.unwrap();
    assert!(status.claude_code.suspended);
    assert_eq!(status.claude_code.consent, None);

    consent::give(&db, CLAUDE, &ALL, "2026-11-01", "gui")
        .await
        .unwrap();
    assert!(matches!(
        consent::state(&db, CLAUDE).await.unwrap(),
        ConsentState::Active(_)
    ));
    assert_eq!(
        consent::active(&db, CLAUDE).await.unwrap(),
        Some(ALL.to_vec())
    );
}

/// Two consents in one millisecond meet `give`'s `ON CONFLICT` clause, which must set the row
/// too. Closed rows fill every millisecond of the next few seconds, so `give` lands on one.
#[tokio::test]
async fn a_second_consent_in_one_millisecond_still_lifts_a_suspension() {
    let (_tmp, db) = setup().await;
    sqlx::query(
        "WITH RECURSIVE ms(n) AS (SELECT 0 UNION ALL SELECT n + 1 FROM ms WHERE n < 20000) \
         INSERT OR IGNORE INTO consent \
         (provider, categories, given_at, revoked_at, terms_date, register_row) \
         SELECT ?, '[\"job_text\"]', \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now', printf('+%.3f seconds', n / 1000.0)), \
         't', 'old', 'older words' FROM ms",
    )
    .bind(CLAUDE)
    .execute(&db.pool)
    .await
    .unwrap();
    consent::give(&db, CLAUDE, &ALL, "2026-11-01", "gui")
        .await
        .unwrap();
    assert!(matches!(
        consent::state(&db, CLAUDE).await.unwrap(),
        ConsentState::Active(_)
    ));
}

/// Tests give these names to prove they unlock nothing.
#[tokio::test]
async fn a_name_outside_the_table_is_recorded_with_no_row() {
    let (_tmp, db) = setup().await;
    consent::give(&db, "ollama", &ALL, "2026-11-01", "gui")
        .await
        .unwrap();
    let row: String = sqlx::query_scalar("SELECT register_row FROM consent")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(row, "");
    assert_eq!(consent::active(&db, CLAUDE).await.unwrap(), None);
}

#[tokio::test]
async fn attestation_is_required_where_the_table_says() {
    let (_tmp, db) = setup().await;
    let cats = [DataCategory::JobText];
    for name in ["google", "gemini_cli"] {
        assert_eq!(
            give_for(&db, name, &cats, false, "cli")
                .await
                .unwrap_err()
                .to_string(),
            "refused: attestation_needed"
        );
        let key = info(name).unwrap().id.to_string();
        assert_eq!(consent::active(&db, &key).await.unwrap(), None);
        give_for(&db, name, &cats, true, "cli").await.unwrap();
        assert_eq!(
            consent::active(&db, &key).await.unwrap(),
            Some(cats.to_vec())
        );
    }
    give_for(&db, "openai", &cats, false, "cli").await.unwrap();
    assert_eq!(
        give_for(&db, "vertex_ai", &cats, true, "cli")
            .await
            .unwrap_err()
            .code(),
        "validation"
    );
    assert_eq!(
        give_for(&db, "ollama", &cats, true, "cli")
            .await
            .unwrap_err()
            .code(),
        "validation"
    );
}

#[tokio::test]
async fn give_for_takes_its_date_from_the_register() {
    let (_tmp, db) = setup().await;
    give_for(&db, "openai", &ALL, false, "gui").await.unwrap();
    let date: String = sqlx::query_scalar("SELECT terms_date FROM consent")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(Some(date), date_read(KEY));
}

#[tokio::test]
async fn sharing_lists_every_task_and_says_why_codex_runs_none() {
    let (_tmp, db) = setup().await;
    let s = sharing(&db, "codex_cli").await.unwrap();
    assert_eq!(s.tasks.len(), 4);
    assert!(s.tasks.iter().all(|t| t.task != TaskType::Embed));
    assert!(
        s.tasks
            .iter()
            .all(|t| !t.runs && t.skipped_because.as_deref() == Some("no_tools"))
    );
}

#[tokio::test]
async fn sharing_says_what_each_task_sends_and_keeps_a_suspended_choice() {
    let (_tmp, db) = setup().await;
    let s = sharing(&db, "google").await.unwrap();
    assert_eq!(s.key, "google/paid_key");
    assert_eq!(s.terms_date, "2026-11-01");
    assert!(s.attestation.as_deref().unwrap().contains("billing on"));
    assert!(
        s.tasks
            .iter()
            .all(|t| t.runs && t.skipped_because.is_none())
    );
    let draft = s
        .tasks
        .iter()
        .find(|t| t.task == TaskType::DraftResume)
        .unwrap();
    assert_eq!(draft.categories, ALL);
    assert!(!s.suspended);
    assert_eq!(s.given, None);

    give_for(&db, "google", &ALL, true, "gui").await.unwrap();
    assert_eq!(
        sharing(&db, "google").await.unwrap().given.as_deref(),
        Some(&ALL[..])
    );
    sqlx::query("UPDATE consent SET register_row = ''")
        .execute(&db.pool)
        .await
        .unwrap();
    let s = sharing(&db, "google").await.unwrap();
    assert!(s.suspended);
    assert_eq!(s.given.as_deref(), Some(&ALL[..]));

    assert_eq!(
        sharing(&db, "vertex_ai").await.unwrap_err().code(),
        "validation"
    );
}
