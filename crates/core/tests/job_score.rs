//! The weighted score, the objective filters, and the authorization note. No network, no model.

use jobpilot_core::jobs::prefs::{self, MatchPrefs};
use jobpilot_core::jobs::rules::Seniority;
use jobpilot_core::jobs::score::{
    Candidate, Criterion, candidate, inputs_key, location_points, score, store,
};
use jobpilot_core::jobs::{
    AuthNote, Job, JobFilter, PastedJob, PayPeriod, PayRange, PayWish, authorization_note, detail,
    get, import_pasted, list,
};
use jobpilot_core::profile::{self, ProfileBasic, WorkMode};
use jobpilot_core::{AppPaths, Db, knowledge};
use tempfile::TempDir;

async fn open() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

fn eur(amount: i64) -> PayWish {
    PayWish {
        amount,
        currency: "EUR".into(),
        period: PayPeriod::Year,
    }
}

fn blank_job() -> Job {
    Job {
        id: "j".into(),
        job_text_id: "j".into(),
        connector: "import".into(),
        title: "Engineer".into(),
        company: None,
        company_id: None,
        location: String::new(),
        country: None,
        work_mode: None,
        employment_type: None,
        url: None,
        attribution: None,
        posted_on: None,
        pay: None,
        seniority: None,
        skills: Vec::new(),
        fields_by: "none".into(),
        duplicate_of: None,
        duplicates: 0,
        findings: 0,
        first_seen_at: String::new(),
        last_seen_at: String::new(),
        score: None,
        stale: false,
        scored: false,
        not_stated: Vec::new(),
    }
}

fn job_full() -> Job {
    Job {
        skills: vec!["Rust".into(), "SQL".into(), "Kubernetes".into()],
        seniority: Some("senior".into()),
        work_mode: Some("remote".into()),
        location: "Anywhere in the World".into(),
        pay: Some(PayRange {
            min: Some(70_000),
            max: Some(80_000),
            currency: Some("EUR".into()),
            period: Some(PayPeriod::Year),
        }),
        ..blank_job()
    }
}

fn job_no_pay() -> Job {
    Job {
        pay: None,
        ..job_full()
    }
}

fn pt() -> ProfileBasic {
    ProfileBasic {
        countries: vec!["PT".into()],
        ..Default::default()
    }
}

fn lisbon() -> ProfileBasic {
    ProfileBasic {
        countries: vec!["PT".into()],
        cities: vec!["Lisbon".into()],
        ..Default::default()
    }
}

fn me_full() -> Candidate {
    Candidate {
        skills: vec!["Rust".into(), "SQL".into()],
        basic: ProfileBasic {
            countries: vec!["PT".into()],
            work_modes: vec![WorkMode::Remote],
            ..Default::default()
        },
        prefs: MatchPrefs {
            min_pay: Some(eur(75_000)),
            seniority: Some(Seniority::Senior),
        },
    }
}

#[test]
fn the_score_is_weighted_and_every_row_shows_its_weight_and_points() {
    let e = score(&job_full(), &me_full());
    assert_eq!(e.rows.iter().map(|r| r.weight).sum::<u32>(), 100);
    assert_eq!(e.compared_weight, 100);
    assert_eq!(
        e.score,
        Some(e.rows.iter().map(|r| r.points.unwrap()).sum())
    );
    let skills = &e.rows[0];
    assert_eq!(
        (skills.criterion, skills.points),
        (Criterion::Skills, Some(27))
    );
    assert_eq!(e.rows[1].points, Some(20));
    assert_eq!(e.rows[2].points, Some(20));
    assert_eq!(e.rows[3].points, Some(20));
    assert_eq!(e.rows[3].note, "posting 70-80k EUR, you want 75k+ EUR");
}

#[test]
fn a_pay_below_the_wish_scores_its_ratio() {
    let mut me = me_full();
    me.prefs.min_pay = Some(eur(160_000));
    let e = score(&job_full(), &me);
    assert_eq!(e.rows[3].points, Some(10));
    assert_eq!(e.rows[3].note, "posting 70-80k EUR, you want 160k+ EUR");
}

#[test]
fn a_criterion_left_out_rescales_and_says_so() {
    let e = score(&job_no_pay(), &me_full());
    assert_eq!(e.compared_weight, 80);
    assert!(
        e.rows
            .iter()
            .any(|r| r.criterion == Criterion::Pay && r.points.is_none())
    );
    // 27 + 20 + 20 = 67 of 80.
    assert_eq!(e.score, Some(84));
}

#[test]
fn pay_with_an_unknown_period_is_never_compared() {
    let mut job = job_full();
    job.pay.as_mut().unwrap().period = None;
    let e = score(&job, &me_full());
    assert_eq!(e.rows[3].points, None);
}

#[test]
fn an_empty_profile_scores_nothing() {
    let e = score(&job_full(), &Candidate::default());
    assert_eq!(e.score, None);
    assert_eq!(e.compared_weight, 0);
}

#[test]
fn missing_skills_are_listed_and_never_added_to_the_candidate() {
    let me = me_full();
    let e = score(&job_full(), &me);
    assert_eq!(e.matched_skills, ["Rust", "SQL"]);
    assert_eq!(e.missing_skills, ["Kubernetes"]);
    assert_eq!(me.skills, ["Rust", "SQL"]);
}

#[test]
fn seniority_one_step_apart_is_half() {
    let mut me = me_full();
    me.prefs.seniority = Some(Seniority::Mid);
    assert_eq!(score(&job_full(), &me).rows[1].points, Some(10));
    me.prefs.seniority = Some(Seniority::Junior);
    assert_eq!(score(&job_full(), &me).rows[1].points, Some(0));
}

#[test]
fn a_mode_outside_the_work_modes_is_zero() {
    let mut me = me_full();
    me.basic.work_modes = vec![WorkMode::Onsite];
    assert_eq!(score(&job_full(), &me).rows[2].points, Some(0));
}

#[test]
fn the_same_input_gives_the_same_score() {
    let (a, b) = (
        score(&job_full(), &me_full()),
        score(&job_full(), &me_full()),
    );
    assert_eq!(a, b);
    assert_eq!(
        inputs_key(&job_full(), &me_full()),
        inputs_key(&job_full(), &me_full())
    );
}

#[test]
fn location_rows_hold_the_pinned_points() {
    assert_eq!(
        location_points("remote", "Anywhere in the World", &pt()),
        20
    );
    assert_eq!(location_points("remote", "", &pt()), 15);
    assert_eq!(location_points("remote", "United States", &pt()), 0);
    assert_eq!(location_points("remote", "Portugal", &pt()), 20);
    assert_eq!(location_points("onsite", "Lisbon", &lisbon()), 20);
    assert_eq!(location_points("onsite", "Porto, Portugal", &lisbon()), 10);
    assert_eq!(location_points("onsite", "Berlin, Germany", &lisbon()), 0);
}

fn loc_points(job: &Job, basic: ProfileBasic) -> Option<u32> {
    let me = Candidate {
        basic,
        ..Default::default()
    };
    score(job, &me).rows[2].points
}

fn placed(mode: Option<&str>, location: &str, country: Option<&str>) -> Job {
    Job {
        work_mode: mode.map(str::to_owned),
        location: location.into(),
        country: country.map(str::to_owned),
        ..blank_job()
    }
}

fn basic(countries: &[&str], cities: &[&str], modes: Vec<WorkMode>) -> ProfileBasic {
    ProfileBasic {
        countries: countries.iter().map(|c| c.to_string()).collect(),
        cities: cities.iter().map(|c| c.to_string()).collect(),
        work_modes: modes,
        ..Default::default()
    }
}

#[test]
fn an_unstated_mode_is_not_made_up() {
    let remote_only = basic(&["PT"], &[], vec![WorkMode::Remote]);
    let got = loc_points(&placed(None, "Lisbon, Portugal", None), remote_only);
    assert!(got.is_some_and(|p| p > 0), "{got:?}");
    let got = loc_points(&placed(None, "Anywhere", None), basic(&["PT"], &[], vec![]));
    assert_eq!(got, Some(20));
}

#[test]
fn a_profile_with_no_place_cannot_judge_a_place() {
    let modes = basic(&[], &[], vec![WorkMode::Remote]);
    assert_eq!(location_points("remote", "", &modes), 15);
    assert_eq!(
        loc_points(&placed(Some("remote"), "United States", None), modes),
        None
    );
    let cities = basic(&[], &["Lisbon"], vec![WorkMode::Onsite]);
    assert_eq!(
        loc_points(&placed(Some("onsite"), "Berlin", None), cities.clone()),
        None
    );
    assert_eq!(
        loc_points(&placed(Some("onsite"), "Lisbon", None), cities),
        Some(20)
    );
}

#[test]
fn an_onsite_job_with_no_place_is_not_compared() {
    let pt = basic(&["PT"], &[], vec![]);
    assert_eq!(
        loc_points(&placed(Some("onsite"), "", None), pt.clone()),
        None
    );
    assert_eq!(loc_points(&placed(Some("onsite"), "Foo", None), pt), None);
}

#[test]
fn the_stated_country_is_used() {
    let pt = basic(&["PT"], &[], vec![]);
    for code in ["DE", "CA", "IN", "NL"] {
        assert_eq!(
            loc_points(&placed(Some("onsite"), "Foo", Some(code)), pt.clone()),
            Some(0),
            "{code}"
        );
        assert_eq!(
            loc_points(&placed(Some("remote"), "", Some(code)), pt.clone()),
            Some(0),
            "{code}"
        );
    }
    assert_eq!(
        loc_points(&placed(Some("remote"), "Foo", Some("PT")), pt.clone()),
        Some(20)
    );
    assert_eq!(
        loc_points(&placed(Some("onsite"), "", Some("PT")), pt),
        Some(10)
    );
}

#[test]
fn a_city_matches_a_whole_part_of_the_place() {
    let york = basic(&["US"], &["York"], vec![]);
    assert_eq!(
        loc_points(
            &placed(Some("onsite"), "New York, United States", None),
            york.clone()
        ),
        Some(10)
    );
    assert_eq!(
        loc_points(&placed(Some("onsite"), "York, United States", None), york),
        Some(20)
    );
}

#[test]
fn pay_with_no_period_is_not_compared_and_a_period_is_shown() {
    let mut job = job_full();
    job.pay = Some(PayRange {
        min: Some(50),
        max: None,
        currency: Some("EUR".into()),
        period: None,
    });
    let mut me = me_full();
    me.prefs.min_pay = Some(eur(90_000));
    let row = score(&job, &me).rows.remove(3);
    assert_eq!(row.points, None, "{}", row.note);
    job.pay.as_mut().unwrap().period = Some(PayPeriod::Hour);
    let row = score(&job, &me).rows.remove(3);
    assert_eq!(row.points, Some(20));
    assert!(row.note.contains("hour"), "{}", row.note);
}

async fn job_in(db: &Db, title: &str, country: Option<&str>, mode: Option<&str>) -> Job {
    let job = import_pasted(
        db,
        PastedJob {
            title: title.into(),
            company: "Acme".into(),
            url: None,
            text: format!("{title} plain text"),
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE jobs SET country = ?, work_mode = ? WHERE id = ?")
        .bind(country)
        .bind(mode)
        .bind(&job.id)
        .execute(&db.pool)
        .await
        .unwrap();
    job
}

async fn set_pay(db: &Db, id: &str, min: i64, max: i64, currency: &str) {
    sqlx::query(
        "UPDATE jobs SET pay_min = ?, pay_max = ?, pay_currency = ?, pay_period = 'year' \
         WHERE id = ?",
    )
    .bind(min)
    .bind(max)
    .bind(currency)
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn scoring_works_with_no_ai_backend_and_no_network() {
    let (_tmp, db) = open().await;
    knowledge::add(&db, "skill", "Rust, Go; SQL\nDocker", "user", None)
        .await
        .unwrap();
    let job = job_in(&db, "Backend", Some("PT"), Some("remote")).await;
    sqlx::query("UPDATE jobs SET skills = '[\"Rust\",\"Kubernetes\"]' WHERE id = ?")
        .bind(&job.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let job = get(&db, &job.id).await.unwrap();
    let me = candidate(&db).await.unwrap();
    for skill in ["Rust", "Go", "SQL", "Docker"] {
        assert!(me.skills.iter().any(|s| s == skill), "{skill}");
    }
    let e = score(&job, &me);
    assert_eq!(e.by, "rules");
    assert_eq!(e.matched_skills, ["Rust"]);
    store(&db, &job.id, &e, &inputs_key(&job, &me))
        .await
        .unwrap();
    assert_eq!(
        get(&db, &job.id).await.unwrap().score,
        e.score.map(i64::from)
    );
    db.close().await;
}

#[tokio::test]
async fn a_min_pay_filter_keeps_unstated_pay() {
    let (_tmp, db) = open().await;
    let unstated = job_in(&db, "Unstated", None, None).await;
    let usd_job = job_in(&db, "Usd", None, None).await;
    let low_eur = job_in(&db, "Low", None, None).await;
    let high_eur = job_in(&db, "High", None, None).await;
    set_pay(&db, &usd_job.id, 50_000, 60_000, "USD").await;
    set_pay(&db, &low_eur.id, 40_000, 50_000, "EUR").await;
    set_pay(&db, &high_eur.id, 90_000, 100_000, "EUR").await;
    let kept = list(
        &db,
        &JobFilter {
            min_pay: Some(eur(90_000)),
            ..Default::default()
        },
        50,
    )
    .await
    .unwrap();
    assert!(kept.iter().any(|j| j.id == unstated.id));
    assert!(kept.iter().any(|j| j.id == usd_job.id));
    for id in [&unstated.id, &usd_job.id] {
        let job = kept.iter().find(|j| &j.id == id).unwrap();
        assert!(job.not_stated.contains(&"pay"), "{:?}", job.not_stated);
    }
    let high = kept.iter().find(|j| j.id == high_eur.id).unwrap();
    assert!(high.not_stated.is_empty());
    assert!(kept.iter().any(|j| j.id == high_eur.id));
    assert!(!kept.iter().any(|j| j.id == low_eur.id));
    db.close().await;
}

#[tokio::test]
async fn a_country_filter_is_exact_and_keeps_unknown_rows_marked() {
    let (_tmp, db) = open().await;
    let pt = job_in(&db, "Pt", Some("PT"), Some("onsite")).await;
    let de = job_in(&db, "De", Some("DE"), Some("onsite")).await;
    let unknown = job_in(&db, "Unknown", None, None).await;
    let anywhere = job_in(&db, "Anywhere", None, Some("remote")).await;
    sqlx::query("UPDATE jobs SET location = 'Worldwide' WHERE id = ?")
        .bind(&anywhere.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let emea = job_in(&db, "Emea", None, Some("hybrid")).await;
    sqlx::query("UPDATE jobs SET location = 'EMEA' WHERE id = ?")
        .bind(&emea.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let filter = JobFilter {
        country: Some("PT".into()),
        work_mode: Some("onsite".into()),
        ..Default::default()
    };
    let kept = list(&db, &filter, 50).await.unwrap();
    let ids: Vec<&str> = kept.iter().map(|j| j.id.as_str()).collect();
    assert!(ids.contains(&pt.id.as_str()));
    assert!(!ids.contains(&de.id.as_str()));
    let unknown = kept.iter().find(|j| j.id == unknown.id).unwrap();
    assert_eq!(unknown.not_stated, ["work_mode", "country"]);
    assert!(
        kept.iter()
            .find(|j| j.id == pt.id)
            .unwrap()
            .not_stated
            .is_empty()
    );
    let any_mode = list(
        &db,
        &JobFilter {
            country: Some("PT".into()),
            ..Default::default()
        },
        50,
    )
    .await
    .unwrap();
    assert!(any_mode.iter().any(|j| j.id == anywhere.id));
    assert!(any_mode.iter().any(|j| j.id == emea.id));
    assert!(!any_mode.iter().any(|j| j.id == de.id));
    db.close().await;
}

#[tokio::test]
async fn a_changed_profile_marks_a_score_stale() {
    let (_tmp, db) = open().await;
    let job = job_in(&db, "Backend", Some("PT"), Some("remote")).await;
    let me = candidate(&db).await.unwrap();
    let current = get(&db, &job.id).await.unwrap();
    store(
        &db,
        &job.id,
        &score(&current, &me),
        &inputs_key(&current, &me),
    )
    .await
    .unwrap();
    assert!(!get(&db, &job.id).await.unwrap().stale);
    prefs::set(
        &db,
        &MatchPrefs {
            min_pay: Some(eur(80_000)),
            seniority: None,
        },
    )
    .await
    .unwrap();
    assert!(get(&db, &job.id).await.unwrap().stale);
    assert!(list(&db, &JobFilter::default(), 5).await.unwrap()[0].stale);
    let mut basic = profile::basic(&db).await.unwrap();
    basic.cities = vec!["Lisbon".into()];
    profile::set_basic(&db, basic).await.unwrap();
    let me = candidate(&db).await.unwrap();
    store(
        &db,
        &job.id,
        &score(&current, &me),
        &inputs_key(&current, &me),
    )
    .await
    .unwrap();
    assert!(!get(&db, &job.id).await.unwrap().stale);
    db.close().await;
}

#[tokio::test]
async fn prefs_round_trip_and_refuse_nonsense() {
    let (_tmp, db) = open().await;
    assert_eq!(prefs::get(&db).await.unwrap(), MatchPrefs::default());
    let wanted = MatchPrefs {
        min_pay: Some(eur(90_000)),
        seniority: Some(Seniority::Staff),
    };
    prefs::set(&db, &wanted).await.unwrap();
    assert_eq!(prefs::get(&db).await.unwrap(), wanted);
    let bad = MatchPrefs {
        min_pay: Some(eur(0)),
        seniority: None,
    };
    assert!(prefs::set(&db, &bad).await.is_err());
    db.close().await;
}

#[tokio::test]
async fn a_missing_authorization_row_reads_not_stated() {
    let (_tmp, db) = open().await;
    assert!(matches!(
        authorization_note(&db, Some("PT")).await.unwrap(),
        AuthNote::NotStated { .. }
    ));
    assert!(matches!(
        authorization_note(&db, None).await.unwrap(),
        AuthNote::NoCountry
    ));
    db.close().await;
}

#[tokio::test]
async fn the_detail_holds_the_text_the_group_and_the_stored_explanation() {
    let (_tmp, db) = open().await;
    let job = job_in(&db, "Backend", Some("PT"), Some("remote")).await;
    let none = detail(&db, &job.id).await.unwrap();
    assert!(none.explanation.is_none());
    assert!(none.text.contains("plain text"));
    assert!(
        none.group.is_empty(),
        "a job with no duplicate has no others"
    );
    assert!(matches!(none.authorization, AuthNote::NotStated { .. }));
    let me = candidate(&db).await.unwrap();
    let current = get(&db, &job.id).await.unwrap();
    let e = score(&current, &me);
    store(&db, &job.id, &e, &inputs_key(&current, &me))
        .await
        .unwrap();
    assert_eq!(detail(&db, &job.id).await.unwrap().explanation, Some(e));
    db.close().await;
}

#[test]
fn no_score_row_or_filter_reads_authorization() {
    let words = ["authorized", "sponsorship", "authorization"];
    let score_rs = include_str!("../src/jobs/score.rs").to_lowercase();
    let prefs_rs = include_str!("../src/jobs/prefs.rs").to_lowercase();
    let module = include_str!("../src/jobs/mod.rs");
    let from = module.find("pub async fn list(").unwrap();
    let to = module.find("pub async fn get(").unwrap();
    let filter = module[from..to].to_lowercase();
    for word in words {
        assert!(!score_rs.contains(word), "score.rs names {word}");
        assert!(!prefs_rs.contains(word), "prefs.rs names {word}");
        assert!(!filter.contains(word), "the filter names {word}");
    }
}

#[tokio::test]
async fn a_score_row_holds_no_authorization_value() {
    let (_tmp, db) = open().await;
    let job = job_in(&db, "Backend", Some("PT"), Some("remote")).await;
    sqlx::query("UPDATE jobs SET mentions_authorization = 1 WHERE id = ?")
        .bind(&job.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let me = candidate(&db).await.unwrap();
    let current = get(&db, &job.id).await.unwrap();
    store(
        &db,
        &job.id,
        &score(&current, &me),
        &inputs_key(&current, &me),
    )
    .await
    .unwrap();
    let (explanation, inputs): (String, String) =
        sqlx::query_as("SELECT explanation, inputs FROM match_results WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    for text in [explanation.to_lowercase(), inputs.to_lowercase()] {
        for word in ["authoriz", "sponsor"] {
            assert!(!text.contains(word), "{word}");
        }
    }
    db.close().await;
}

#[tokio::test]
#[ignore = "uses the keychain"]
async fn a_stated_value_is_shown_for_that_country_only() {
    let (_tmp, db) = open().await;
    profile::set_authorization(
        &db,
        profile::WorkAuthorization {
            country: "PT".into(),
            authorized: profile::Stated::Yes,
            needs_sponsorship: profile::Stated::No,
            note: String::new(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        authorization_note(&db, Some("PT")).await.unwrap(),
        AuthNote::Stated { .. }
    ));
    assert!(matches!(
        authorization_note(&db, Some("CA")).await.unwrap(),
        AuthNote::NotStated { .. }
    ));
    db.close().await;
}

#[tokio::test]
#[ignore = "uses the keychain"]
async fn a_locked_key_is_not_read_as_not_stated() {
    let (_tmp, db) = open().await;
    profile::set_authorization(
        &db,
        profile::WorkAuthorization {
            country: "PT".into(),
            authorized: profile::Stated::Yes,
            needs_sponsorship: profile::Stated::No,
            note: String::new(),
        },
    )
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO data_keys (key_id, created_at, key_check, active) \
         VALUES ('gone', 't', x'00', 0)",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE work_authorization SET key_id = 'gone'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(matches!(
        authorization_note(&db, Some("PT")).await.unwrap(),
        AuthNote::Locked
    ));
    db.close().await;
}
