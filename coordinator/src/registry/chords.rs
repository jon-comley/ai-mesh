// Songs tab persistence: chord sheets saved from Ultimate Guitar or pasted.
// See plans/chords-songbook.md.
use super::{Registry, gen_uuid, now_unix_millis};
use ::chords::Tab;
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use tracing::warn;

/// A saved song as the library lists it — everything but the sheet itself.
#[derive(Debug, Clone, Serialize)]
pub struct ChordSongSummary {
    pub id: String,
    pub artist: String,
    pub title: String,
    pub artist_key: String,
    pub title_key: String,
    pub ug_tab_id: Option<i64>,
    pub source_url: Option<String>,
    pub kind: Option<String>,
    pub version: Option<i64>,
    pub rating: Option<f64>,
    pub votes: Option<i64>,
    pub saved_ms: i64,
    pub last_played_ms: Option<i64>,
}

/// A saved song with its sheet and the player settings kept for it.
#[derive(Debug, Clone, Serialize)]
pub struct ChordSong {
    #[serde(flatten)]
    pub summary: ChordSongSummary,
    pub capo: Option<i64>,
    pub tuning: Option<String>,
    pub tonality: Option<String>,
    pub content: String,
    pub transpose: i64,
    pub scroll_speed: Option<f64>,
    pub youtube_url: Option<String>,
    pub notes: Option<String>,
}

/// Player settings to change. `None` leaves a field alone; an empty string
/// clears `youtube_url` or `notes`.
#[derive(Debug, Default, Deserialize)]
pub struct ChordSongPrefs {
    pub transpose: Option<i64>,
    pub scroll_speed: Option<f64>,
    pub youtube_url: Option<String>,
    pub notes: Option<String>,
}

/// What a library sorts and groups by: lowercase, single-spaced, without a
/// leading "The " — so The Beatles file under B, beside Bastille.
pub fn sort_key(s: &str) -> String {
    let lower = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    match lower.strip_prefix("the ") {
        Some(rest) if !rest.is_empty() => rest.to_string(),
        _ => lower,
    }
}

const SUMMARY_COLS: &str = "id, artist, title, artist_key, title_key, ug_tab_id, source_url, kind, \
                            version, rating, votes, saved_ms, last_played_ms";

fn summary_from_row(row: &Row<'_>) -> rusqlite::Result<ChordSongSummary> {
    Ok(ChordSongSummary {
        id: row.get(0)?,
        artist: row.get(1)?,
        title: row.get(2)?,
        artist_key: row.get(3)?,
        title_key: row.get(4)?,
        ug_tab_id: row.get(5)?,
        source_url: row.get(6)?,
        kind: row.get(7)?,
        version: row.get(8)?,
        rating: row.get(9)?,
        votes: row.get(10)?,
        saved_ms: row.get(11)?,
        last_played_ms: row.get(12)?,
    })
}

fn blank_to_none(s: &str) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty()).then_some(s)
}

impl Registry {
    /// Every saved song, A–Z by title.
    pub fn list_chord_songs(&self) -> Vec<ChordSongSummary> {
        let sql = format!("SELECT {SUMMARY_COLS} FROM chord_songs ORDER BY title_key, artist_key");
        let mut stmt = match self.conn.prepare(&sql) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "list_chord_songs prepare failed");
                return Vec::new();
            }
        };
        stmt.query_map([], summary_from_row)
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    pub fn get_chord_song(&self, id: &str) -> Option<ChordSong> {
        let sql = format!(
            "SELECT {SUMMARY_COLS}, capo, tuning, tonality, content, transpose, scroll_speed,
                    youtube_url, notes
             FROM chord_songs WHERE id = ?1"
        );
        self.conn
            .query_row(&sql, params![id], |row| {
                Ok(ChordSong {
                    summary: summary_from_row(row)?,
                    capo: row.get(13)?,
                    tuning: row.get(14)?,
                    tonality: row.get(15)?,
                    content: row.get(16)?,
                    transpose: row.get(17)?,
                    scroll_speed: row.get(18)?,
                    youtube_url: row.get(19)?,
                    notes: row.get(20)?,
                })
            })
            .optional()
            .unwrap_or_else(|e| {
                warn!(error = %e, "get_chord_song failed");
                None
            })
    }

    /// The saved song for a UG version, if that version is saved.
    pub fn chord_song_for_tab(&self, ug_tab_id: i64) -> Option<String> {
        self.conn
            .query_row(
                "SELECT id FROM chord_songs WHERE ug_tab_id = ?1",
                params![ug_tab_id],
                |row| row.get(0),
            )
            .optional()
            .unwrap_or(None)
    }

    /// Save a sheet. Saving a UG version that is already saved returns the
    /// saved one unchanged rather than a duplicate.
    pub fn save_chord_song(&self, tab: &Tab) -> Option<ChordSong> {
        if let Some(existing) = tab.tab_id.and_then(|t| self.chord_song_for_tab(t)) {
            return self.get_chord_song(&existing);
        }
        let id = gen_uuid();
        if let Err(e) = self.conn.execute(
            "INSERT INTO chord_songs (id, artist, title, artist_key, title_key, ug_tab_id, source_url,
                 kind, version, rating, votes, capo, tuning, tonality, content, saved_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                id,
                tab.artist_name,
                tab.song_name,
                sort_key(&tab.artist_name),
                sort_key(&tab.song_name),
                tab.tab_id,
                tab.url,
                tab.kind,
                tab.version,
                tab.rating,
                tab.votes,
                tab.capo,
                tab.tuning,
                tab.tonality,
                tab.content,
                now_unix_millis(),
            ],
        ) {
            warn!(error = %e, "save_chord_song failed");
            return None;
        }
        self.get_chord_song(&id)
    }

    /// Replace a saved song's sheet with a fresh copy, keeping its player
    /// settings. `None` if there is no such song.
    pub fn refresh_chord_song(&self, id: &str, tab: &Tab) -> Option<ChordSong> {
        let n = self
            .conn
            .execute(
                "UPDATE chord_songs SET rating = ?2, votes = ?3, capo = ?4, tuning = ?5,
                     tonality = ?6, content = ?7
                 WHERE id = ?1",
                params![
                    id,
                    tab.rating,
                    tab.votes,
                    tab.capo,
                    tab.tuning,
                    tab.tonality,
                    tab.content
                ],
            )
            .unwrap_or_else(|e| {
                warn!(error = %e, "refresh_chord_song failed");
                0
            });
        if n == 0 {
            return None;
        }
        self.get_chord_song(id)
    }

    pub fn update_chord_song_prefs(&self, id: &str, prefs: &ChordSongPrefs) -> Option<ChordSong> {
        let mut song = self.get_chord_song(id)?;
        if let Some(t) = prefs.transpose {
            // Twelve semitones is the same key again; keep the sign so "-1"
            // reads as one down rather than eleven up.
            song.transpose = t % 12;
        }
        if let Some(s) = prefs.scroll_speed {
            song.scroll_speed = Some(s.clamp(1.0, 500.0));
        }
        if let Some(u) = &prefs.youtube_url {
            song.youtube_url = blank_to_none(u).map(str::to_string);
        }
        if let Some(n) = &prefs.notes {
            song.notes = blank_to_none(n).map(str::to_string);
        }
        if let Err(e) = self.conn.execute(
            "UPDATE chord_songs SET transpose = ?2, scroll_speed = ?3, youtube_url = ?4, notes = ?5
             WHERE id = ?1",
            params![
                id,
                song.transpose,
                song.scroll_speed,
                song.youtube_url,
                song.notes
            ],
        ) {
            warn!(error = %e, "update_chord_song_prefs failed");
        }
        Some(song)
    }

    pub fn touch_chord_song_played(&self, id: &str) {
        if let Err(e) = self.conn.execute(
            "UPDATE chord_songs SET last_played_ms = ?2 WHERE id = ?1",
            params![id, now_unix_millis()],
        ) {
            warn!(error = %e, "touch_chord_song_played failed");
        }
    }

    pub fn delete_chord_song(&self, id: &str) -> bool {
        self.conn
            .execute("DELETE FROM chord_songs WHERE id = ?1", params![id])
            .map(|n| n > 0)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: Option<i64>, artist: &str, title: &str) -> Tab {
        let mut t = ::chords::parse_pasted(artist, title, "[ch]G[/ch] la");
        t.tab_id = id;
        t.capo = Some(2);
        t
    }

    #[test]
    fn sort_keys_ignore_case_spacing_and_a_leading_the() {
        assert_eq!(sort_key("The  Beatles"), "beatles");
        assert_eq!(sort_key("Bastille"), "bastille");
        assert_eq!(sort_key("The"), "the");
        assert_eq!(sort_key("Theatre"), "theatre");
    }

    #[test]
    fn a_saved_song_round_trips_and_lists_a_to_z() {
        let reg = Registry::new();
        let pompeii = reg
            .save_chord_song(&tab(Some(1), "Bastille", "Pompeii"))
            .unwrap();
        reg.save_chord_song(&tab(Some(2), "Jimi Hendrix", "Angel"))
            .unwrap();
        reg.save_chord_song(&tab(None, "The Beatles", "Yesterday"))
            .unwrap();

        let titles: Vec<_> = reg
            .list_chord_songs()
            .into_iter()
            .map(|s| s.title)
            .collect();
        assert_eq!(titles, ["Angel", "Pompeii", "Yesterday"]);

        let got = reg.get_chord_song(&pompeii.summary.id).unwrap();
        assert_eq!(got.content, "[ch]G[/ch] la");
        assert_eq!(got.capo, Some(2));
        assert_eq!(got.transpose, 0);
        let beatles = reg
            .list_chord_songs()
            .into_iter()
            .find(|s| s.title == "Yesterday")
            .unwrap();
        assert_eq!(beatles.artist_key, "beatles");
    }

    #[test]
    fn saving_the_same_ug_version_twice_keeps_one() {
        let reg = Registry::new();
        let a = reg.save_chord_song(&tab(Some(7), "A", "S")).unwrap();
        let b = reg.save_chord_song(&tab(Some(7), "A", "S")).unwrap();
        assert_eq!(a.summary.id, b.summary.id);
        assert_eq!(reg.list_chord_songs().len(), 1);
        // A second version of the same song is a different save.
        reg.save_chord_song(&tab(Some(8), "A", "S")).unwrap();
        assert_eq!(reg.list_chord_songs().len(), 2);
    }

    #[test]
    fn prefs_update_independently_and_blank_clears() {
        let reg = Registry::new();
        let id = reg
            .save_chord_song(&tab(None, "A", "S"))
            .unwrap()
            .summary
            .id;
        let s = reg
            .update_chord_song_prefs(
                &id,
                &ChordSongPrefs {
                    transpose: Some(-1),
                    youtube_url: Some("https://youtu.be/abc".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(s.transpose, -1);
        assert_eq!(s.youtube_url.as_deref(), Some("https://youtu.be/abc"));

        let s = reg
            .update_chord_song_prefs(
                &id,
                &ChordSongPrefs {
                    scroll_speed: Some(30.0),
                    youtube_url: Some("  ".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(s.transpose, -1, "untouched");
        assert_eq!(s.scroll_speed, Some(30.0));
        assert_eq!(s.youtube_url, None);
        assert!(
            reg.update_chord_song_prefs("nope", &ChordSongPrefs::default())
                .is_none()
        );
    }

    #[test]
    fn refresh_replaces_the_sheet_and_keeps_settings() {
        let reg = Registry::new();
        let id = reg
            .save_chord_song(&tab(Some(1), "A", "S"))
            .unwrap()
            .summary
            .id;
        reg.update_chord_song_prefs(
            &id,
            &ChordSongPrefs {
                transpose: Some(3),
                ..Default::default()
            },
        );
        let mut fresh = tab(Some(1), "A", "S");
        fresh.content = "[ch]Am[/ch] new".into();
        let s = reg.refresh_chord_song(&id, &fresh).unwrap();
        assert_eq!(s.content, "[ch]Am[/ch] new");
        assert_eq!(s.transpose, 3);
    }

    #[test]
    fn played_and_deleted() {
        let reg = Registry::new();
        let id = reg
            .save_chord_song(&tab(None, "A", "S"))
            .unwrap()
            .summary
            .id;
        assert_eq!(
            reg.get_chord_song(&id).unwrap().summary.last_played_ms,
            None
        );
        reg.touch_chord_song_played(&id);
        assert!(
            reg.get_chord_song(&id)
                .unwrap()
                .summary
                .last_played_ms
                .is_some()
        );
        assert!(reg.delete_chord_song(&id));
        assert!(!reg.delete_chord_song(&id));
        assert!(reg.get_chord_song(&id).is_none());
    }
}
