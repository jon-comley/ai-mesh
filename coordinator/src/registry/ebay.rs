// eBay bargain-finder ("Hunts") persistence: saved searches, the seen-listing
// dedup set, and the finds feed. See plans/ebay-bargain-finder.md.
use super::{Registry, gen_uuid, now_unix_millis};
use ::ebay::{HuntSpec, Listing, TermEntry};
use rusqlite::params;
use tracing::warn;

/// One row in `ebay_finds` — a listing surfaced for a hunt, with its LLM
/// bargain verdict (`None` in heuristic-only mode, or when the LLM's reply
/// omitted this item).
#[derive(Debug, Clone, serde::Serialize)]
pub struct EbayFindRecord {
    pub id: String,
    pub hunt_id: String,
    pub item_id: String,
    pub title: String,
    pub price_minor: Option<i64>,
    pub currency: Option<String>,
    pub image_url: Option<String>,
    pub item_web_url: String,
    pub matched_term: String,
    pub verdict: Option<String>,
    pub found_ms: i64,
    pub reviewed: bool,
}

impl Registry {
    pub fn list_hunts(&self) -> Vec<HuntSpec> {
        let mut stmt = match self.conn.prepare(
            "SELECT id, name, source_url, terms_json, timeslots_json, marketplace, enabled
             FROM ebay_hunts ORDER BY created_ms ASC",
        ) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "list_hunts prepare failed");
                return vec![];
            }
        };
        stmt.query_map([], |row| {
            let terms_json: String = row.get(3)?;
            let timeslots_json: String = row.get(4)?;
            Ok(HuntSpec {
                id: row.get(0)?,
                name: row.get(1)?,
                source_url: row.get(2)?,
                terms: serde_json::from_str(&terms_json).unwrap_or_default(),
                timeslots: serde_json::from_str(&timeslots_json).unwrap_or_default(),
                marketplace: row.get(5)?,
                enabled: row.get::<_, i64>(6)? != 0,
            })
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    pub fn get_hunt(&self, id: &str) -> Option<HuntSpec> {
        self.list_hunts().into_iter().find(|h| h.id == id)
    }

    pub fn create_hunt(
        &self,
        name: &str,
        source_url: &str,
        terms: Vec<TermEntry>,
        timeslots: Vec<u16>,
        marketplace: &str,
    ) -> HuntSpec {
        let id = gen_uuid();
        let terms_json = serde_json::to_string(&terms).unwrap_or_else(|_| "[]".into());
        let timeslots_json = serde_json::to_string(&timeslots).unwrap_or_else(|_| "[]".into());
        if let Err(e) = self.conn.execute(
            "INSERT INTO ebay_hunts (id, name, source_url, terms_json, timeslots_json, marketplace, enabled, created_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7)",
            params![id, name, source_url, terms_json, timeslots_json, marketplace, now_unix_millis()],
        ) {
            warn!(error = %e, "create_hunt failed");
        }
        HuntSpec {
            id,
            name: name.to_owned(),
            source_url: source_url.to_owned(),
            terms,
            timeslots,
            marketplace: marketplace.to_owned(),
            enabled: true,
        }
    }

    /// Update a hunt's mutable fields. `None` leaves that field unchanged.
    #[allow(clippy::too_many_arguments)]
    pub fn update_hunt(
        &self,
        id: &str,
        name: Option<&str>,
        terms: Option<Vec<TermEntry>>,
        timeslots: Option<Vec<u16>>,
        marketplace: Option<&str>,
        enabled: Option<bool>,
    ) -> Option<HuntSpec> {
        let mut hunt = self.get_hunt(id)?;
        if let Some(name) = name {
            hunt.name = name.to_owned();
        }
        if let Some(terms) = terms {
            hunt.terms = terms;
        }
        if let Some(timeslots) = timeslots {
            hunt.timeslots = timeslots;
        }
        if let Some(marketplace) = marketplace {
            hunt.marketplace = marketplace.to_owned();
        }
        if let Some(enabled) = enabled {
            hunt.enabled = enabled;
        }
        let terms_json = serde_json::to_string(&hunt.terms).unwrap_or_else(|_| "[]".into());
        let timeslots_json = serde_json::to_string(&hunt.timeslots).unwrap_or_else(|_| "[]".into());
        if let Err(e) = self.conn.execute(
            "UPDATE ebay_hunts SET name = ?2, terms_json = ?3, timeslots_json = ?4, marketplace = ?5, enabled = ?6 WHERE id = ?1",
            params![id, hunt.name, terms_json, timeslots_json, hunt.marketplace, hunt.enabled as i64],
        ) {
            warn!(error = %e, "update_hunt failed");
        }
        Some(hunt)
    }

    /// Returns true if a hunt existed and was deleted.
    pub fn delete_hunt(&self, id: &str) -> bool {
        self.conn
            .execute("DELETE FROM ebay_hunts WHERE id = ?1", params![id])
            .map(|n| n > 0)
            .unwrap_or(false)
    }

    pub fn has_seen_listing(&self, hunt_id: &str, item_id: &str) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM ebay_seen_listings WHERE hunt_id = ?1 AND item_id = ?2",
                params![hunt_id, item_id],
                |_| Ok(()),
            )
            .is_ok()
    }

    pub fn seen_listing_ids(&self, hunt_id: &str) -> std::collections::HashSet<String> {
        self.conn
            .prepare("SELECT item_id FROM ebay_seen_listings WHERE hunt_id = ?1")
            .and_then(|mut stmt| {
                stmt.query_map(params![hunt_id], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()
            })
            .unwrap_or_default()
    }

    pub fn mark_listing_seen(&self, hunt_id: &str, item_id: &str) {
        let _ = self.conn.execute(
            "INSERT INTO ebay_seen_listings (hunt_id, item_id, first_seen_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT (hunt_id, item_id) DO NOTHING",
            params![hunt_id, item_id, now_unix_millis()],
        );
    }

    /// Insert one find. `verdict` is `None` in heuristic-only mode, or when
    /// the LLM's batched reply omitted this item.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_find(
        &self,
        hunt_id: &str,
        listing: &Listing,
        matched_term: &str,
        verdict: Option<&str>,
    ) -> EbayFindRecord {
        let id = gen_uuid();
        let found_ms = now_unix_millis();
        if let Err(e) = self.conn.execute(
            "INSERT INTO ebay_finds (id, hunt_id, item_id, title, price_minor, currency, image_url, item_web_url, matched_term, verdict, found_ms, reviewed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0)",
            params![
                id, hunt_id, listing.item_id, listing.title, listing.price_minor,
                listing.currency, listing.image_url, listing.item_web_url, matched_term,
                verdict, found_ms,
            ],
        ) {
            warn!(error = %e, "insert_find failed");
        }
        EbayFindRecord {
            id,
            hunt_id: hunt_id.to_owned(),
            item_id: listing.item_id.clone(),
            title: listing.title.clone(),
            price_minor: listing.price_minor,
            currency: listing.currency.clone(),
            image_url: listing.image_url.clone(),
            item_web_url: listing.item_web_url.clone(),
            matched_term: matched_term.to_owned(),
            verdict: verdict.map(|s| s.to_owned()),
            found_ms,
            reviewed: false,
        }
    }

    /// Most recent finds, newest first, optionally scoped to one hunt.
    pub fn list_finds(&self, hunt_id: Option<&str>, limit: u32) -> Vec<EbayFindRecord> {
        let sql = if hunt_id.is_some() {
            "SELECT id, hunt_id, item_id, title, price_minor, currency, image_url, item_web_url, matched_term, verdict, found_ms, reviewed
             FROM ebay_finds WHERE hunt_id = ?1 ORDER BY found_ms DESC LIMIT ?2"
        } else {
            "SELECT id, hunt_id, item_id, title, price_minor, currency, image_url, item_web_url, matched_term, verdict, found_ms, reviewed
             FROM ebay_finds ORDER BY found_ms DESC LIMIT ?1"
        };
        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<EbayFindRecord> {
            Ok(EbayFindRecord {
                id: row.get(0)?,
                hunt_id: row.get(1)?,
                item_id: row.get(2)?,
                title: row.get(3)?,
                price_minor: row.get(4)?,
                currency: row.get(5)?,
                image_url: row.get(6)?,
                item_web_url: row.get(7)?,
                matched_term: row.get(8)?,
                verdict: row.get(9)?,
                found_ms: row.get(10)?,
                reviewed: row.get::<_, i64>(11)? != 0,
            })
        };
        let result = if let Some(hunt_id) = hunt_id {
            self.conn.prepare(sql).and_then(|mut stmt| {
                stmt.query_map(params![hunt_id, limit], map_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
        } else {
            self.conn.prepare(sql).and_then(|mut stmt| {
                stmt.query_map(params![limit], map_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
        };
        result.unwrap_or_default()
    }

    /// Returns true if a find existed and was marked reviewed.
    pub fn mark_find_reviewed(&self, id: &str) -> bool {
        self.conn
            .execute(
                "UPDATE ebay_finds SET reviewed = 1 WHERE id = ?1",
                params![id],
            )
            .map(|n| n > 0)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_listing(id: &str) -> Listing {
        Listing {
            item_id: id.to_string(),
            title: format!("Fender Strat {id}"),
            price_minor: Some(45000),
            currency: Some("GBP".into()),
            image_url: Some("https://example.com/img.jpg".into()),
            item_web_url: format!("https://ebay.co.uk/itm/{id}"),
            condition: Some("Used".into()),
        }
    }

    fn sample_terms() -> Vec<TermEntry> {
        vec![TermEntry {
            text: "fender stratocaster".into(),
            enabled: true,
            is_misspelling: false,
        }]
    }

    #[test]
    fn create_and_list_hunts_roundtrips() {
        let reg = Registry::new();
        let hunt = reg.create_hunt(
            "Strat",
            "https://ebay.co.uk/itm/123",
            sample_terms(),
            vec![480, 1080],
            "EBAY_GB",
        );
        let hunts = reg.list_hunts();
        assert_eq!(hunts.len(), 1);
        assert_eq!(hunts[0].id, hunt.id);
        assert_eq!(hunts[0].name, "Strat");
        assert_eq!(hunts[0].timeslots, vec![480, 1080]);
        assert!(hunts[0].enabled);
    }

    #[test]
    fn update_hunt_changes_only_given_fields() {
        let reg = Registry::new();
        let hunt = reg.create_hunt(
            "Strat",
            "https://ebay.co.uk/itm/123",
            sample_terms(),
            vec![480],
            "EBAY_GB",
        );
        let updated = reg
            .update_hunt(
                &hunt.id,
                None,
                None,
                Some(vec![600, 720]),
                None,
                Some(false),
            )
            .unwrap();
        assert_eq!(updated.name, "Strat");
        assert_eq!(updated.timeslots, vec![600, 720]);
        assert!(!updated.enabled);
    }

    #[test]
    fn update_hunt_returns_none_for_unknown_id() {
        let reg = Registry::new();
        assert!(
            reg.update_hunt("nope", None, None, None, None, None)
                .is_none()
        );
    }

    #[test]
    fn delete_hunt_removes_it() {
        let reg = Registry::new();
        let hunt = reg.create_hunt("Strat", "https://x", sample_terms(), vec![], "EBAY_GB");
        assert!(reg.delete_hunt(&hunt.id));
        assert!(reg.get_hunt(&hunt.id).is_none());
        assert!(!reg.delete_hunt(&hunt.id));
    }

    /// `ebay_seen_listings`/`ebay_finds` rows carry a `FOREIGN KEY` on
    /// `hunt_id` (cascades on hunt deletion), so tests need a real hunt row
    /// rather than an arbitrary string id.
    fn sample_hunt(reg: &Registry) -> String {
        reg.create_hunt(
            "Strat",
            "https://ebay.co.uk/itm/123",
            sample_terms(),
            vec![],
            "EBAY_GB",
        )
        .id
    }

    #[test]
    fn seen_listings_dedup() {
        let reg = Registry::new();
        let hunt_id = sample_hunt(&reg);
        assert!(!reg.has_seen_listing(&hunt_id, "item1"));
        reg.mark_listing_seen(&hunt_id, "item1");
        assert!(reg.has_seen_listing(&hunt_id, "item1"));
        assert!(!reg.has_seen_listing(&hunt_id, "item2"));
        let seen = reg.seen_listing_ids(&hunt_id);
        assert!(seen.contains("item1"));
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn insert_and_list_finds_newest_first() {
        let reg = Registry::new();
        let hunt_id = sample_hunt(&reg);
        reg.insert_find(
            &hunt_id,
            &sample_listing("1"),
            "fender strat",
            Some("bargain: rare colour"),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        reg.insert_find(&hunt_id, &sample_listing("2"), "fender strat", None);
        let finds = reg.list_finds(None, 10);
        assert_eq!(finds.len(), 2);
        assert_eq!(finds[0].item_id, "2");
        assert_eq!(finds[0].verdict, None);
        assert_eq!(finds[1].item_id, "1");
        assert_eq!(finds[1].verdict.as_deref(), Some("bargain: rare colour"));
    }

    #[test]
    fn list_finds_filters_by_hunt() {
        let reg = Registry::new();
        let hunt1 = sample_hunt(&reg);
        let hunt2 = sample_hunt(&reg);
        reg.insert_find(&hunt1, &sample_listing("1"), "term", None);
        reg.insert_find(&hunt2, &sample_listing("2"), "term", None);
        let finds = reg.list_finds(Some(&hunt1), 10);
        assert_eq!(finds.len(), 1);
        assert_eq!(finds[0].hunt_id, hunt1);
    }

    #[test]
    fn mark_find_reviewed_updates_flag() {
        let reg = Registry::new();
        let hunt_id = sample_hunt(&reg);
        let find = reg.insert_find(&hunt_id, &sample_listing("1"), "term", None);
        assert!(!find.reviewed);
        assert!(reg.mark_find_reviewed(&find.id));
        let finds = reg.list_finds(None, 10);
        assert!(finds[0].reviewed);
        assert!(!reg.mark_find_reviewed("nope"));
    }
}
