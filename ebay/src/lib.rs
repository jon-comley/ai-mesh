pub mod client;
pub mod diff;
pub mod ntfy;
pub mod schedule;

pub use client::{EbayClient, EbayError, ItemDetail, Listing};

use serde::{Deserialize, Serialize};

/// One search term attached to a hunt — either the item's own name or an
/// LLM-suggested misspelling/mis-listing variant, individually toggleable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermEntry {
    pub text: String,
    pub enabled: bool,
    pub is_misspelling: bool,
}

/// A saved search: the pasted source item, the terms to search with, and
/// the daily timeslots (minutes-since-midnight) at which to run it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HuntSpec {
    pub id: String,
    pub name: String,
    pub source_url: String,
    pub terms: Vec<TermEntry>,
    /// Minutes-since-midnight, e.g. 510 = 08:30.
    pub timeslots: Vec<u16>,
    pub marketplace: String,
    pub enabled: bool,
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
