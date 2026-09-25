pub mod client;
pub mod diff;
pub mod ntfy;
pub mod schedule;

pub use client::{EbayClient, EbayError, ItemDetail, Listing, marketplace_currency};

use serde::{Deserialize, Serialize};

/// One search term attached to a hunt — either the item's own name or an
/// LLM-suggested misspelling/mis-listing variant, individually toggleable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermEntry {
    pub text: String,
    pub enabled: bool,
    pub is_misspelling: bool,
}

/// What keeps a hunt on the right kind of thing.
///
/// Terms alone match on words, and words do not know a car from a car part: a
/// hunt for a "Ford Fiesta Zetec" finds every bumper and wing mirror carrying that
/// name. A category confines the search to where the thing itself is listed, and
/// a price ceiling drops everything above what the hunter would ever pay. All of
/// it is optional and absent means "search everything", which is how every hunt
/// behaved before 2026-09-25.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HuntFilter {
    /// The marketplace's own category id (eBay UK "Cars" is 9801).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_id: Option<String>,
    /// The category's name, for showing to a person. Never sent to eBay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_name: Option<String>,
    /// Highest price wanted, in minor units (pence) of the marketplace's currency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_price_minor: Option<i64>,
}

impl HuntFilter {
    /// True when nothing is set.
    pub fn is_empty(&self) -> bool {
        self.category_id.is_none() && self.max_price_minor.is_none()
    }
}

/// A saved search: the pasted source item, the terms to search with, and
/// the daily timeslots (minutes-since-midnight) at which to run it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HuntSpec {
    pub id: String,
    pub name: String,
    pub source_url: String,
    /// What the hunt is actually for, in the user's own words — "headless CI
    /// runner, core count matters most, storage secondary". Free text, fed to
    /// the LLM alongside the listings so its verdict and score are about
    /// fitness for a purpose rather than similarity to `name`. Empty is normal
    /// and means "judge it against the name alone", which is what every hunt
    /// did before 2026-09-14.
    #[serde(default)]
    pub goal: String,
    pub terms: Vec<TermEntry>,
    /// Minutes-since-midnight, e.g. 510 = 08:30.
    pub timeslots: Vec<u16>,
    pub marketplace: String,
    pub enabled: bool,
    /// Category and price ceiling. Flattened, so the JSON stays one flat object
    /// and an older client that has never heard of them is unaffected.
    #[serde(default, flatten)]
    pub filter: HuntFilter,
}

impl HuntSpec {
    /// The enabled terms' text, as passed to `EbayClient::search`.
    pub fn active_terms(&self) -> Vec<String> {
        self.terms
            .iter()
            .filter(|t| t.enabled)
            .map(|t| t.text.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunt(filter: HuntFilter) -> HuntSpec {
        HuntSpec {
            id: "h1".into(),
            name: "Runaround".into(),
            source_url: String::new(),
            goal: String::new(),
            terms: vec![],
            timeslots: vec![],
            marketplace: "EBAY_GB".into(),
            enabled: true,
            filter,
        }
    }

    #[test]
    fn the_filter_sits_flat_in_the_json_not_nested() {
        let json = serde_json::to_value(hunt(HuntFilter {
            category_id: Some("9801".into()),
            category_name: Some("Cars".into()),
            max_price_minor: Some(150_000),
        }))
        .unwrap();

        assert_eq!(json["category_id"], "9801");
        assert_eq!(json["category_name"], "Cars");
        assert_eq!(json["max_price_minor"], 150_000);
        assert!(json.get("filter").is_none());
    }

    #[test]
    fn an_unset_filter_adds_nothing_to_the_json() {
        let json = serde_json::to_value(hunt(HuntFilter::default())).unwrap();

        assert!(json.get("category_id").is_none());
        assert!(json.get("max_price_minor").is_none());
    }

    #[test]
    fn a_hunt_saved_before_filters_existed_still_loads() {
        let old = r#"{"id":"h1","name":"Strat","source_url":"","terms":[],"timeslots":[],
                      "marketplace":"EBAY_GB","enabled":true}"#;

        let hunt: HuntSpec = serde_json::from_str(old).unwrap();

        assert!(hunt.filter.is_empty());
        assert_eq!(hunt.goal, "");
    }

    #[test]
    fn a_category_or_a_price_makes_a_filter_non_empty() {
        assert!(HuntFilter::default().is_empty());
        assert!(!HuntFilter { category_id: Some("1".into()), ..Default::default() }.is_empty());
        assert!(!HuntFilter { max_price_minor: Some(5), ..Default::default() }.is_empty());
    }
}
