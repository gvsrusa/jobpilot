//! The deterministic parsing rules: place, pay, seniority, skills. No network, no model.

use jobpilot_core::connectors::{self, Posting};
use jobpilot_core::jobs::geo::{Scope, countries, country_code, in_region, scope_of};
use jobpilot_core::jobs::pay::{PayCompare, compare, parse_pay, yearly};
use jobpilot_core::jobs::rules::{
    Seniority, fields, mentions_authorization, seniority_of, skills_in, vocabulary,
};
use jobpilot_core::jobs::{
    PastedJob, PayPeriod, PayRange, PayWish, Upserted, apply_rules, get, import_pasted,
    upsert_posting,
};
use jobpilot_core::{AppPaths, Db};
use tempfile::TempDir;

async fn open() -> (TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&AppPaths::new(tmp.path().join("data")))
        .await
        .unwrap();
    (tmp, db)
}

fn range(min: i64, currency: Option<&str>, period: Option<PayPeriod>) -> PayRange {
    PayRange {
        min: Some(min),
        max: Some(min),
        currency: currency.map(str::to_string),
        period,
    }
}

fn usd(amount: i64) -> PayRange {
    range(amount, Some("USD"), Some(PayPeriod::Year))
}

fn no_currency(amount: i64) -> PayRange {
    range(amount, None, Some(PayPeriod::Year))
}

fn eur_hourly(amount: i64) -> PayRange {
    range(amount, Some("EUR"), Some(PayPeriod::Hour))
}

fn eur_year_wish(amount: i64) -> PayWish {
    PayWish {
        amount,
        currency: "EUR".into(),
        period: PayPeriod::Year,
    }
}

#[test]
fn the_table_holds_every_country_once() {
    assert_eq!(countries().len(), 249);
    let mut codes: Vec<&str> = countries().iter().map(|c| c.code).collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), 249);
}

#[test]
fn places_resolve() {
    assert!(matches!(scope_of("Anywhere in the World"), Scope::Anywhere));
    assert!(matches!(scope_of("Remote, Portugal"), Scope::Country("PT")));
    assert!(matches!(scope_of("Dallas, TX"), Scope::Country("US")));
    assert!(matches!(scope_of("Colorado"), Scope::Country("US")));
    assert!(matches!(scope_of("Germany Munich"), Scope::Country("DE")));
    assert!(matches!(scope_of("EMEA"), Scope::Region("EMEA")));
    assert!(matches!(scope_of("Lausanne"), Scope::Unknown));
    assert!(in_region("EMEA", "PT") && !in_region("EMEA", "US"));
}

#[test]
fn longest_match_wins_and_aliases_resolve() {
    assert!(matches!(scope_of("New Mexico"), Scope::Country("US")));
    assert!(matches!(
        scope_of("Mexico City, Mexico"),
        Scope::Country("MX")
    ));
    assert!(matches!(scope_of("Remote - UK"), Scope::Country("GB")));
    assert!(matches!(
        scope_of("Berlin, Deutschland"),
        Scope::Country("DE")
    ));
    assert!(matches!(scope_of("Madrid, España"), Scope::Country("ES")));
    assert!(matches!(scope_of("Holland"), Scope::Country("NL")));
    assert!(matches!(scope_of("Toronto, ON"), Scope::Country("CA")));
    assert!(matches!(scope_of("Remote, Worldwide"), Scope::Anywhere));
    assert!(matches!(scope_of("Remote - EU"), Scope::Region("EU")));
    assert!(matches!(
        scope_of("North America"),
        Scope::Region("NORTH AMERICA")
    ));
    assert!(matches!(scope_of("Nigeria"), Scope::Country("NG")));
    assert!(matches!(scope_of(""), Scope::Unknown));
    // A two-letter form counts only after a comma, and in capitals.
    assert!(matches!(scope_of("or tx"), Scope::Unknown));
    assert_eq!(country_code("Austin, TX, USA"), Some("US"));
    assert_eq!(country_code("EMEA"), None);
    assert!(in_region("EU", "FR") && !in_region("EU", "GB"));
    assert!(in_region("AMERICAS", "BR") && in_region("AMERICAS", "US"));
    assert!(in_region("apac", "JP"));
}

#[test]
fn pay_is_read_as_written() {
    let p = parse_pay("$81K – $87K").unwrap();
    assert_eq!(
        (p.min, p.max, p.currency.as_deref()),
        (Some(81000), Some(87000), Some("USD"))
    );
    let e = parse_pay("70-80k EUR").unwrap();
    assert_eq!(
        (e.min, e.max, e.currency.as_deref()),
        (Some(70000), Some(80000), Some("EUR"))
    );
    assert_eq!(parse_pay("€70.000 per year").unwrap().min, Some(70000));
    assert_eq!(
        parse_pay("£45 per hour").unwrap().period,
        Some(PayPeriod::Hour)
    );
    assert_eq!(
        parse_pay("CAD 120,000").unwrap().currency.as_deref(),
        Some("CAD")
    );
}

#[test]
fn a_written_currency_beats_the_dollar_sign_and_a_period_is_read() {
    assert_eq!(
        parse_pay("$120,000 CAD per year")
            .unwrap()
            .currency
            .as_deref(),
        Some("CAD")
    );
    assert_eq!(
        parse_pay("AUD $100k").unwrap().currency.as_deref(),
        Some("AUD")
    );
    assert_eq!(
        parse_pay("CHF 9,000 per month").unwrap().period,
        Some(PayPeriod::Month)
    );
    assert_eq!(
        parse_pay("$50 - $60 /hr").unwrap().period,
        Some(PayPeriod::Hour)
    );
    assert_eq!(
        parse_pay("$90 per day").unwrap().period,
        Some(PayPeriod::Day)
    );
    assert_eq!(
        parse_pay("$1,200 per week").unwrap().period,
        Some(PayPeriod::Week)
    );
    assert_eq!(parse_pay("$100k").unwrap().period, Some(PayPeriod::Year));
    assert_eq!(
        parse_pay("¥6,000,000").unwrap().currency.as_deref(),
        Some("JPY")
    );
    assert_eq!(
        parse_pay("₹1.500.000 per year").unwrap().min,
        Some(1_500_000)
    );
    // Under 1,000 with no period is not pay.
    assert_eq!(parse_pay("$45"), None);
    let amid = parse_pay("We pay well. Salary: $120,000 - $150,000 a year, plus bonus").unwrap();
    assert_eq!((amid.min, amid.max), (Some(120000), Some(150000)));
}

#[test]
fn figures_that_are_not_pay_are_not_read() {
    for text in [
        "0.5% – 1.75% equity",
        "2000 employees",
        "founded in 2019",
        "20% bonus",
        "401k match",
        "$10,000 signing bonus",
        "bonus of $15,000",
        "raised $500,000 in funding",
        "$50M",
    ] {
        assert_eq!(parse_pay(text), None, "{text}");
    }
}

#[test]
fn pay_in_another_currency_is_not_compared() {
    let wish = PayWish {
        amount: 90000,
        currency: "EUR".into(),
        period: PayPeriod::Year,
    };
    assert!(matches!(
        compare(&usd(100000), &wish),
        PayCompare::NotCompared("currency")
    ));
    assert!(matches!(
        compare(&no_currency(100000), &wish),
        PayCompare::NotCompared("currency")
    ));
}

#[test]
fn pay_compares_across_periods_at_the_pinned_factors() {
    assert_eq!(yearly(50, PayPeriod::Hour), 104000);
    assert_eq!(yearly(100, PayPeriod::Day), 26000);
    assert_eq!(yearly(1000, PayPeriod::Week), 52000);
    assert_eq!(yearly(5000, PayPeriod::Month), 60000);
    assert!(matches!(
        compare(&eur_hourly(50), &eur_year_wish(90000)),
        PayCompare::Meets
    ));
    let PayCompare::Below { ratio } = compare(&eur_hourly(20), &eur_year_wish(90000)) else {
        panic!("below")
    };
    assert!((ratio - 41600.0 / 90000.0).abs() < 1e-4);
}

#[test]
fn an_unknown_period_stays_unknown() {
    let unknown = range(100000, Some("EUR"), None);
    assert!(matches!(
        compare(&unknown, &eur_year_wish(90000)),
        PayCompare::NotCompared("period")
    ));
}

#[test]
fn seniority_reads_the_title_before_the_text() {
    assert_eq!(
        seniority_of("Senior Engineer", "2 years"),
        Some(Seniority::Senior)
    );
    assert_eq!(
        seniority_of("Engineer", "5+ years of experience"),
        Some(Seniority::Senior)
    );
    assert_eq!(seniority_of("Engineer", "we are a great team"), None);
}

#[test]
fn seniority_words_and_years_map_to_levels() {
    for (title, level) in [
        ("Software Engineering Intern", Seniority::Intern),
        ("Jr. Developer", Seniority::Junior),
        ("Graduate Engineer", Seniority::Junior),
        ("Mid-level Designer", Seniority::Mid),
        ("Sr Backend Engineer", Seniority::Senior),
        ("Principal Engineer", Seniority::Staff),
        ("Engineering Manager", Seniority::Staff),
        ("Head of Data", Seniority::Executive),
        ("VP Engineering", Seniority::Executive),
    ] {
        assert_eq!(seniority_of(title, ""), Some(level), "{title}");
    }
    assert_eq!(
        seniority_of("Engineer", "1 year of experience"),
        Some(Seniority::Junior)
    );
    assert_eq!(
        seniority_of("Engineer", "0-1 years of experience"),
        Some(Seniority::Junior)
    );
    assert_eq!(
        seniority_of("Engineer", "3 years of experience"),
        Some(Seniority::Mid)
    );
    assert_eq!(
        seniority_of("Engineer", "2+ years building APIs"),
        Some(Seniority::Mid)
    );
    assert!(Seniority::Junior < Seniority::Senior);
}

#[test]
fn go_and_r_are_not_matched_in_ordinary_words() {
    let v = vocabulary(&[]);
    assert!(skills_in("go to market with our R&D team", &[], &v).is_empty());
    assert_eq!(
        skills_in("Golang and C++ on .NET", &[], &v),
        ["C++", "Golang", ".NET"]
    );
    assert_eq!(
        skills_in("We write Go and R daily", &[], &v),
        ["Golang", "R"]
    );
    assert_eq!(
        skills_in("JavaScript, not Java", &[], &v),
        ["Java", "JavaScript"]
    );
    assert_eq!(
        skills_in("ASP.NET services in C#", &[], &v),
        ["ASP.NET", "C#"]
    );
    assert_eq!(
        skills_in("Node.js and PostgreSQL", &[], &v),
        ["Node.js", "PostgreSQL"]
    );
}

#[test]
fn noisy_tags_outside_the_vocabulary_are_dropped() {
    assert_eq!(
        skills_in(
            "",
            &["digital nomad".into(), "rust".into()],
            &vocabulary(&[])
        ),
        ["Rust"]
    );
}

#[test]
fn the_candidates_own_skills_join_the_vocabulary() {
    let v = vocabulary(&["Gleam".into(), "rust".into()]);
    assert_eq!(
        skills_in("We use gleam and Rust", &[], &v),
        ["Rust", "Gleam"]
    );
    assert_eq!(
        v.iter().filter(|s| s.eq_ignore_ascii_case("rust")).count(),
        1
    );
}

#[test]
fn authorization_words_set_a_flag_and_nothing_else() {
    assert!(mentions_authorization(
        "Must have the right to work in the UK. No sponsorship."
    ));
    assert!(!mentions_authorization("Build things in Rust."));
}

#[test]
fn fields_gather_what_the_rules_can_read() {
    let v = vocabulary(&[]);
    let f = fields(
        "Senior Rust Engineer",
        "Remote, Portugal",
        "Pay: €70.000 - €90.000 per year. Rust and Postgres. Right to work in the EU needed.",
        &[],
        &v,
    );
    assert_eq!(f.country.as_deref(), Some("PT"));
    assert_eq!(f.work_mode, Some("remote"));
    assert_eq!(f.seniority, Some(Seniority::Senior));
    assert_eq!(f.skills, ["PostgreSQL", "Rust"]);
    assert!(f.mentions_authorization);
    let pay = f.pay.unwrap();
    assert_eq!((pay.min, pay.max), (Some(70000), Some(90000)));
    let none = fields("Engineer", "Lausanne", "A team.", &[], &v);
    assert_eq!(none.country, None);
    assert_eq!(none.work_mode, None);
    assert_eq!(none.pay, None);
}

fn posting(location: &str, country: Option<&str>, raw: &str) -> Posting {
    Posting {
        external_id: "r1".into(),
        title: "Backend Engineer".into(),
        company: "Acme".into(),
        location: location.into(),
        country: country.map(str::to_string),
        work_mode: None,
        employment_type: None,
        url: "https://remoteOK.com/remote-jobs/r1".into(),
        apply_url: None,
        posted_on: None,
        pay: None,
        tags: vec![],
        raw: raw.into(),
    }
}

#[tokio::test]
async fn rules_fill_blanks_and_keep_what_a_connector_stated() {
    let (_tmp, db) = open().await;
    let connector = connectors::find("remoteok").unwrap();
    let decl = connector.decl();
    let stated = posting(
        "Remote, Portugal",
        Some("ES"),
        "Needs 5+ years. $90,000 per year. We use Rust and Python.",
    );
    let Upserted::Added(id) = upsert_posting(&db, decl, "", None, &stated).await.unwrap() else {
        panic!("added")
    };
    let job = get(&db, &id).await.unwrap();
    // What the connector stated stays, and the job stays the connector's.
    assert_eq!(job.country.as_deref(), Some("ES"));
    assert_eq!(job.fields_by, "connector");
    // What it left empty is filled by rules.
    assert_eq!(job.work_mode.as_deref(), Some("remote"));
    assert_eq!(job.seniority.as_deref(), Some("senior"));
    assert_eq!(job.skills, ["Python", "Rust"]);
    let pay = job.pay.unwrap();
    assert_eq!(
        (pay.min, pay.currency.as_deref()),
        (Some(90000), Some("USD"))
    );
    let key: String = sqlx::query_scalar("SELECT dedupe_key FROM jobs WHERE id = ?")
        .bind(&id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(key.starts_with("acme|backend engineer|es|remote"), "{key}");

    // A paste is usable at once: no agent, and the rules are named.
    let pasted = import_pasted(
        &db,
        PastedJob {
            title: "Junior Developer".into(),
            company: "Beta".into(),
            url: None,
            text: "You will use Rust. EUR 50,000 per year. Visa sponsorship is not offered.".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(pasted.fields_by, "rules");
    assert_eq!(pasted.seniority.as_deref(), Some("junior"));
    assert_eq!(pasted.skills, ["Rust"]);
    assert_eq!(pasted.pay.unwrap().period, Some(PayPeriod::Year));
    let flag: i64 = sqlx::query_scalar("SELECT mentions_authorization FROM jobs WHERE id = ?")
        .bind(&pasted.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(flag, 1);

    // Run again, nothing moves.
    apply_rules(&db, &pasted.id).await.unwrap();
    assert_eq!(get(&db, &pasted.id).await.unwrap().skills, ["Rust"]);
    assert_eq!(
        apply_rules(&db, "nope").await.unwrap_err().code(),
        "not_found"
    );
}

#[tokio::test]
async fn rules_never_touch_a_field_a_person_set() {
    let (_tmp, db) = open().await;
    let job = import_pasted(
        &db,
        PastedJob {
            title: "Senior Engineer".into(),
            company: "Acme".into(),
            url: None,
            text: "Rust. $100,000 per year.".into(),
        },
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE jobs SET fields_by = 'user', skills = '[]', seniority = NULL, pay_min = NULL, \
         pay_max = NULL, pay_currency = NULL, pay_period = NULL WHERE id = ?",
    )
    .bind(&job.id)
    .execute(&db.pool)
    .await
    .unwrap();
    apply_rules(&db, &job.id).await.unwrap();
    let after = get(&db, &job.id).await.unwrap();
    assert_eq!(after.fields_by, "user");
    assert!(after.skills.is_empty());
    assert_eq!(after.seniority, None);
    assert_eq!(after.pay, None);
}

#[test]
fn a_suffix_that_is_both_a_region_and_a_country_is_unknown() {
    for place in [
        "Lausanne, DE",
        "Leiden, NL",
        "Remote, IN",
        "Remote, CA",
        "Eilat, IL",
        "Trujillo, PE",
    ] {
        assert!(matches!(scope_of(place), Scope::Unknown), "{place}");
    }
    assert!(matches!(scope_of("Austin, TX"), Scope::Country("US")));
    assert!(matches!(scope_of("Toronto, ON"), Scope::Country("CA")));
    assert!(matches!(scope_of("Remote, US"), Scope::Country("US")));
    assert!(matches!(scope_of("Paris, FR"), Scope::Country("FR")));
}

#[test]
fn more_than_one_place_named_is_unknown() {
    for place in [
        "France, Germany",
        "Remote (US or Canada)",
        "Dallas, TX, France",
        "EMEA, Germany",
    ] {
        assert!(matches!(scope_of(place), Scope::Unknown), "{place}");
    }
    // The same place twice, or a longer name over a shorter one, is one place.
    assert!(matches!(
        scope_of("Mexico City, Mexico"),
        Scope::Country("MX")
    ));
    assert!(matches!(scope_of("New Mexico, USA"), Scope::Country("US")));
}

#[test]
fn pay_words_for_every_period_are_read() {
    assert_eq!(
        parse_pay("$5,000 monthly").unwrap().period,
        Some(PayPeriod::Month)
    );
    assert_eq!(
        parse_pay("$1,200 weekly").unwrap().period,
        Some(PayPeriod::Week)
    );
    assert_eq!(
        parse_pay("$300 daily").unwrap().period,
        Some(PayPeriod::Day)
    );
    assert_eq!(
        parse_pay("$50 hourly").unwrap().period,
        Some(PayPeriod::Hour)
    );
    assert_eq!(
        parse_pay("£60,000 per annum").unwrap().period,
        Some(PayPeriod::Year)
    );
}

#[test]
fn a_figure_with_no_period_is_a_year_only_from_20000() {
    assert_eq!(parse_pay("$20,000").unwrap().period, Some(PayPeriod::Year));
    let unsure = parse_pay("$5,000").unwrap();
    assert_eq!(unsure.period, None);
    assert!(matches!(
        compare(
            &unsure,
            &PayWish {
                amount: 1,
                currency: "USD".into(),
                period: PayPeriod::Year
            }
        ),
        PayCompare::NotCompared("period")
    ));
    assert_eq!(parse_pay("$500"), None);
}

#[test]
fn a_stipend_is_not_pay() {
    for text in ["$5,000 stipend", "stipend of $2,000", "a $30,000 stipend"] {
        assert_eq!(parse_pay(text), None, "{text}");
    }
}

#[test]
fn indian_grouping_is_read() {
    assert_eq!(
        parse_pay("₹12,00,000 per year").unwrap().min,
        Some(1_200_000)
    );
    assert_eq!(parse_pay("₹1,00,000 per month").unwrap().min, Some(100_000));
    assert_eq!(
        parse_pay("$10,000,000 per year").unwrap().min,
        Some(10_000_000)
    );
    assert_eq!(parse_pay("$120,000 per year").unwrap().min, Some(120_000));
}

#[test]
fn a_bonus_beside_pay_does_not_take_the_pay_away() {
    let a = parse_pay("$100,000 – $120,000, annual bonus").unwrap();
    assert_eq!((a.min, a.max), (Some(100_000), Some(120_000)));
    let b = parse_pay("$120,000 + bonus").unwrap();
    assert_eq!(b.min, Some(120_000));
    assert_eq!(parse_pay("$10,000 annual bonus"), None);
}

#[test]
fn parser_input_is_capped() {
    let pad = "x ".repeat(150_000);
    assert!(parse_pay(&format!("{pad} $90,000 per year")).is_none());
    assert!(parse_pay(&format!("$90,000 per year {pad}")).is_some());
    let v = vocabulary(&[]);
    assert!(skills_in(&format!("{pad} Rust"), &[], &v).is_empty());
    assert_eq!(skills_in(&format!("Rust {pad}"), &[], &v), ["Rust"]);
    assert_eq!(seniority_of("Engineer", &format!("{pad} 7+ years")), None);
    assert!(!mentions_authorization(&format!("{pad} right to work")));
}

async fn stored(db: &Db, id: &str) -> (Option<String>, Option<String>, String, i64) {
    sqlx::query_as(
        "SELECT country, seniority, fields_by, mentions_authorization FROM jobs WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_changed_text_keeps_what_a_person_or_model_set_and_recomputes_rules() {
    let (_tmp, db) = open().await;
    let decl = connectors::find("remoteok").unwrap().decl();
    for by in ["user", "model"] {
        let mut p = posting("Remote, Portugal", Some("PT"), "Needs 5+ years.");
        p.external_id = format!("keep-{by}");
        let Upserted::Added(id) = upsert_posting(&db, decl, "", None, &p).await.unwrap() else {
            panic!("added")
        };
        sqlx::query(
            "UPDATE jobs SET fields_by = ?, country = 'FR', seniority = 'staff', location = 'Paris' \
             WHERE id = ?",
        )
        .bind(by)
        .bind(&id)
        .execute(&db.pool)
        .await
        .unwrap();
        let mut changed = p.clone();
        changed.raw = "Needs 1 year of experience. Right to work required.".into();
        changed.location = "Remote, Spain".into();
        changed.country = Some("ES".into());
        let Upserted::Updated(_) = upsert_posting(&db, decl, "", None, &changed).await.unwrap()
        else {
            panic!("updated")
        };
        let (country, seniority, fields_by, _) = stored(&db, &id).await;
        assert_eq!(country.as_deref(), Some("FR"), "{by}");
        assert_eq!(seniority.as_deref(), Some("staff"), "{by}");
        assert_eq!(fields_by, by);
        let location: String = sqlx::query_scalar("SELECT location FROM jobs WHERE id = ?")
            .bind(&id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(location, "Paris");
    }

    let mut p = posting("", None, "Needs 5+ years.");
    p.external_id = "ruled".into();
    let Upserted::Added(id) = upsert_posting(&db, decl, "", None, &p).await.unwrap() else {
        panic!("added")
    };
    sqlx::query("UPDATE jobs SET fields_by = 'rules' WHERE id = ?")
        .bind(&id)
        .execute(&db.pool)
        .await
        .unwrap();
    let mut changed = p.clone();
    changed.raw = "Needs 1 year of experience. Right to work required.".into();
    upsert_posting(&db, decl, "", None, &changed).await.unwrap();
    let (_, seniority, fields_by, flag) = stored(&db, &id).await;
    assert_eq!(seniority.as_deref(), Some("junior"));
    assert_eq!(fields_by, "rules");
    assert_eq!(flag, 1);
}

#[test]
fn a_known_city_settles_an_ambiguous_suffix() {
    for (place, code) in [
        ("San Francisco, CA", "US"),
        ("Boston, MA", "US"),
        ("Indianapolis, IN", "US"),
        ("Toronto, CA", "CA"),
        ("Mumbai, IN", "IN"),
        ("Berlin, DE", "DE"),
        ("Bogota, CO", "CO"),
        ("Amsterdam, NL", "NL"),
        ("Tel Aviv, IL", "IL"),
        ("Lima, PE", "PE"),
        ("Casablanca, MA", "MA"),
        ("Remote - Berlin, DE", "DE"),
        ("Denver, CO", "US"),
    ] {
        assert_eq!(country_code(place), Some(code), "{place}");
    }
    // A suffix that does not fit the city, a city not in the table, and no city all stay unknown.
    for place in [
        "Berlin, CA",
        "Lausanne, CA",
        "Remote, IN",
        "Mumbai, DE",
        "Boston, IN",
    ] {
        assert!(matches!(scope_of(place), Scope::Unknown), "{place}");
    }
}
