//! Diffing a search result against what's already been seen for a hunt.

use crate::client::Listing;
use std::collections::HashSet;

/// Listings from `results` whose `item_id` isn't in `seen_ids` — the ones
/// worth evaluating and notifying on this cycle.
pub fn new_listings(seen_ids: &HashSet<String>, results: &[Listing]) -> Vec<Listing> {
    results
        .iter()
        .filter(|l| !seen_ids.contains(&l.item_id))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(id: &str) -> Listing {
        Listing {
            item_id: id.to_string(),
            title: format!("item {id}"),
            price_minor: Some(1000),
            currency: Some("GBP".into()),
            image_url: None,
            item_web_url: format!("https://ebay.co.uk/itm/{id}"),
            condition: Some("Used".into()),
        }
    }

    #[test]
    fn filters_out_already_seen() {
        let mut seen = HashSet::new();
        seen.insert("1".to_string());
        let results = vec![listing("1"), listing("2"), listing("3")];
        let fresh = new_listings(&seen, &results);
        assert_eq!(
            fresh.iter().map(|l| l.item_id.as_str()).collect::<Vec<_>>(),
            vec!["2", "3"]
        );
    }

    #[test]
    fn empty_seen_set_returns_everything() {
        let results = vec![listing("1"), listing("2")];
        let fresh = new_listings(&HashSet::new(), &results);
        assert_eq!(fresh.len(), 2);
    }

    #[test]
    fn all_seen_returns_empty() {
        let seen: HashSet<String> = ["1", "2"].iter().map(|s| s.to_string()).collect();
        let results = vec![listing("1"), listing("2")];
        assert!(new_listings(&seen, &results).is_empty());
    }
}
