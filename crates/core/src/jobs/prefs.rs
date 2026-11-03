//! What the person wants from a job, beyond the profile: a pay floor and a seniority. It is
//! kept as one setting, `jobs.prefs`, and read by scoring and by the pay filter.

use serde::{Deserialize, Serialize};

use super::PayWish;
use super::rules::Seniority;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::settings;

const KEY: &str = "jobs.prefs";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MatchPrefs {
    pub min_pay: Option<PayWish>,
    pub seniority: Option<Seniority>,
}

pub async fn get(db: &Db) -> Result<MatchPrefs> {
    match settings::get(db, KEY).await? {
        Some(value) => serde_json::from_value(value).map_err(|e| Error::Validation(e.to_string())),
        None => Ok(MatchPrefs::default()),
    }
}

pub async fn set(db: &Db, prefs: &MatchPrefs) -> Result<()> {
    let mut prefs = prefs.clone();
    if let Some(wish) = &mut prefs.min_pay {
        if wish.amount <= 0 {
            return Err(Error::Validation("a pay wish is more than 0".into()));
        }
        wish.currency = wish.currency.trim().to_ascii_uppercase();
        if wish.currency.len() != 3 || !wish.currency.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(Error::Validation(
                "a currency is three letters, like EUR".into(),
            ));
        }
    }
    let value = serde_json::to_value(&prefs).map_err(|e| Error::Validation(e.to_string()))?;
    settings::set(db, KEY, &value).await
}
