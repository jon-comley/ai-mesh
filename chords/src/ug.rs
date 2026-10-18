//! Reading Ultimate Guitar pages. No I/O here: every function takes a page or
//! a value already fetched, so all of it is tested against fixtures.
//!
//! UG has no public API. Every page it serves — search results and a single
//! tab alike — carries its data as one HTML-escaped JSON blob in
//! `<div class="js-store" data-content="…">`, which is what its own scripts
//! render from. Reading that is far steadier than reading the markup around
//! it, which UG restyles freely.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Errors, phrased for a person reading a toast.
#[derive(Debug, PartialEq)]
pub enum UgError {
    /// The page came back without UG's data in it — almost always a bot check.
    Blocked,
    NotFound,
    Unreachable(String),
    Other(String),
}

impl std::fmt::Display for UgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UgError::Blocked => write!(
                f,
                "Ultimate Guitar didn't return the song (it may be blocking automated requests) — \
                 paste the chords instead"
            ),
            UgError::NotFound => write!(f, "Ultimate Guitar has no page at that address"),
            UgError::Unreachable(msg) => write!(f, "could not reach Ultimate Guitar: {msg}"),
            UgError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for UgError {}

/// One search result: a single version of a song.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub tab_id: i64,
    pub song_name: String,
    pub artist_name: String,
    /// "Chords", "Tabs", "Ukulele Chords", "Bass Tabs".
    pub kind: String,
    pub version: Option<i64>,
    pub rating: Option<f64>,
    pub votes: Option<i64>,
    pub tab_url: String,
}

/// Every version found of one song, best first.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SongGroup {
    pub song_name: String,
    pub artist_name: String,
    pub versions: Vec<SearchHit>,
}

/// One chord sheet, from UG or pasted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    #[serde(default)]
    pub tab_id: Option<i64>,
    pub song_name: String,
    pub artist_name: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub version: Option<i64>,
    #[serde(default)]
    pub rating: Option<f64>,
    #[serde(default)]
    pub votes: Option<i64>,
    #[serde(default)]
    pub capo: Option<i64>,
    #[serde(default)]
    pub tuning: Option<String>,
    #[serde(default)]
    pub tonality: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// The sheet in UG's own markup: `[ch]G[/ch]` around each chord,
    /// `[tab]…[/tab]` around chord-over-lyric blocks, `[Verse]` headings.
    /// Kept as-is so the dashboard owns rendering and transposing.
    pub content: String,
    /// Other versions of the same song UG lists on the tab's page. Never
    /// stored — it's what the dashboard's version switcher offers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<SearchHit>,
}

/// Kinds that come with a free text sheet. "Official", "Pro", "Power" and
/// "Video" are paid or interactive and have nothing to show here.
const SHEET_KINDS: &[&str] = &["Chords", "Tabs", "Ukulele Chords", "Bass Tabs"];

/// Hosts a sheet may be fetched from. The fetch endpoint takes a URL from the
/// browser, so without this it would fetch anything for anyone with a token.
const UG_HOSTS: &[&str] = &[
    "tabs.ultimate-guitar.com",
    "www.ultimate-guitar.com",
    "ultimate-guitar.com",
];

/// Whether `url` is a UG tab page — https, a UG host, a `/tab/` path.
pub fn is_ug_url(url: &str) -> bool {
    let Some(rest) = url.trim().strip_prefix("https://") else {
        return false;
    };
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    UG_HOSTS.contains(&host.to_ascii_lowercase().as_str()) && path.starts_with("tab/")
}

/// The decoded JSON from a page's `js-store` element.
pub fn extract_store(html: &str) -> Result<Value, UgError> {
    let at = html.find("class=\"js-store\"").ok_or(UgError::Blocked)?;
    // The attribute may sit either side of `class`; look from the start of the tag.
    let tag_start = html[..at].rfind('<').unwrap_or(at);
    let marker = "data-content=\"";
    let start = html[tag_start..]
        .find(marker)
        .map(|i| tag_start + i + marker.len())
        .ok_or(UgError::Blocked)?;
    let len = html[start..].find('"').ok_or(UgError::Blocked)?;
    let json = decode_entities(&html[start..start + len]);
    serde_json::from_str(&json)
        .map_err(|e| UgError::Other(format!("unexpected Ultimate Guitar page: {e}")))
}

/// Undo HTML attribute escaping: the named entities UG uses, plus numeric ones.
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        // Look for the `;` by byte: slicing the str at a fixed byte offset
        // panics when that offset lands inside a multi-byte character, which
        // real pages (’, é) have and ASCII fixtures never did.
        let Some(semi) = rest.bytes().take(12).position(|b| b == b';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let decoded = match entity {
            "quot" => Some('"'),
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// UG's numbers arrive as numbers or, now and then, numeric strings.
fn int(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_f64().map(|f| f as i64))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn float(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or("").trim().to_string()
}

fn hit(v: &Value) -> Option<SearchHit> {
    let kind = v["type"].as_str()?;
    if !SHEET_KINDS.contains(&kind) {
        return None;
    }
    let tab_url = text(&v["tab_url"]);
    if !is_ug_url(&tab_url) {
        return None;
    }
    Some(SearchHit {
        tab_id: int(&v["id"])?,
        song_name: text(&v["song_name"]),
        artist_name: text(&v["artist_name"]),
        kind: kind.to_string(),
        version: int(&v["version"]),
        rating: float(&v["rating"]),
        votes: int(&v["votes"]),
        tab_url,
    })
}

/// The sheet versions on a search page, in UG's order.
pub fn parse_search(store: &Value) -> Vec<SearchHit> {
    store["store"]["page"]["data"]["results"]
        .as_array()
        .map(|results| results.iter().filter_map(hit).collect())
        .unwrap_or_default()
}

/// The sheet on a tab page, with the other versions UG lists beside it.
pub fn parse_tab(store: &Value) -> Result<Tab, UgError> {
    let data = &store["store"]["page"]["data"];
    let tab = &data["tab"];
    let view = &data["tab_view"];
    let content = view["wiki_tab"]["content"]
        .as_str()
        .filter(|c| !c.trim().is_empty())
        .ok_or_else(|| UgError::Other("that Ultimate Guitar page has no chord sheet".into()))?;
    // An empty `meta` arrives as `[]` rather than `{}`, so read fields, never
    // deserialize the object whole.
    let meta = &view["meta"];
    let tuning = meta["tuning"]["value"]
        .as_str()
        .or_else(|| meta["tuning"].as_str())
        .map(str::to_string);
    let this_id = int(&tab["id"]);
    let versions = view["versions"]
        .as_array()
        .map(|vs| {
            vs.iter()
                .filter_map(hit)
                .filter(|h| Some(h.tab_id) != this_id)
                .collect()
        })
        .unwrap_or_default();
    Ok(Tab {
        tab_id: this_id,
        song_name: text(&tab["song_name"]),
        artist_name: text(&tab["artist_name"]),
        kind: tab["type"].as_str().map(str::to_string),
        version: int(&tab["version"]),
        rating: float(&tab["rating"]),
        votes: int(&tab["votes"]),
        capo: int(&meta["capo"]).filter(|c| *c > 0),
        tuning,
        tonality: meta["tonality"]
            .as_str()
            .or_else(|| tab["tonality_name"].as_str())
            .filter(|t| !t.is_empty())
            .map(str::to_string),
        url: tab["tab_url"].as_str().map(str::to_string),
        content: content.replace("\r\n", "\n"),
        versions,
    })
}

/// Lowercased, single-spaced — what makes "Pompeii" and "pompeii " one song.
fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn kind_rank(kind: &str) -> u8 {
    match kind {
        "Chords" => 0,
        "Tabs" => 1,
        "Ukulele Chords" => 2,
        _ => 3,
    }
}

/// How much to trust a version: its rating, weighted by how many people
/// rated it, so a single 5-star vote doesn't outrank 1,000 at 4.8.
fn score(h: &SearchHit) -> f64 {
    h.rating.unwrap_or(0.0) * ((h.votes.unwrap_or(0).max(0) as f64) + 1.0).ln()
}

/// Group search hits into songs — the version switcher's list. Songs keep
/// UG's relevance order; versions within a song go chords first, best first.
pub fn group_versions(hits: Vec<SearchHit>) -> Vec<SongGroup> {
    let mut groups: Vec<(String, SongGroup)> = Vec::new();
    for h in hits {
        let key = format!("{}\u{0}{}", norm(&h.artist_name), norm(&h.song_name));
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, g)) => g.versions.push(h),
            None => groups.push((
                key,
                SongGroup {
                    song_name: h.song_name.clone(),
                    artist_name: h.artist_name.clone(),
                    versions: vec![h],
                },
            )),
        }
    }
    groups
        .into_iter()
        .map(|(_, mut g)| {
            g.versions.sort_by(|a, b| {
                kind_rank(&a.kind)
                    .cmp(&kind_rank(&b.kind))
                    .then(score(b).total_cmp(&score(a)))
            });
            g
        })
        .collect()
}

/// Whether `token` reads as a chord: a root A–G, an optional sharp or flat,
/// any quality (m, maj7, sus4, add9, dim, aug, 7b5, (9)…), and an optional
/// slash bass note.
fn is_chord(token: &str) -> bool {
    let (main, bass) = match token.split_once('/') {
        Some((m, b)) => (m, Some(b)),
        None => (token, None),
    };
    if let Some(b) = bass
        && !is_root_only(b)
    {
        return false;
    }
    let mut rest = main;
    let Some(root) = rest.chars().next().filter(|c| ('A'..='G').contains(c)) else {
        return false;
    };
    rest = &rest[root.len_utf8()..];
    rest = rest
        .strip_prefix('#')
        .or_else(|| rest.strip_prefix('b'))
        .unwrap_or(rest);
    while !rest.is_empty() {
        let word = ["maj", "min", "dim", "aug", "sus", "add", "m", "M"]
            .iter()
            .find(|w| rest.starts_with(*w));
        if let Some(w) = word {
            rest = &rest[w.len()..];
            continue;
        }
        let c = rest.chars().next().unwrap();
        if c.is_ascii_digit() || "#b+-()°ø".contains(c) {
            rest = &rest[c.len_utf8()..];
        } else {
            return false;
        }
    }
    true
}

fn is_root_only(s: &str) -> bool {
    matches!(s.len(), 1 | 2)
        && s.starts_with(|c: char| ('A'..='G').contains(&c))
        && (s.len() == 1 || s.ends_with('#') || s.ends_with('b'))
}

/// Tokens that may share a chord line without making it a lyric: bar lines,
/// repeat counts, "no chord".
fn is_chord_line_filler(token: &str) -> bool {
    token.chars().all(|c| "|-/:.".contains(c))
        || token.eq_ignore_ascii_case("N.C.")
        || token.eq_ignore_ascii_case("NC")
        || (token.starts_with(['x', 'X']) && token[1..].chars().all(|c| c.is_ascii_digit()))
        || (token.starts_with('(') && token.ends_with(')') && token.len() <= 5)
}

/// A line made only of chords (and bar lines), with at least one chord.
fn is_chord_line(line: &str) -> bool {
    let mut any = false;
    for token in line.split_whitespace() {
        if is_chord(token) {
            any = true;
        } else if !is_chord_line_filler(token) {
            return false;
        }
    }
    any
}

/// A plain-text chord sheet (chord lines above lyric lines, as copied off any
/// site) turned into UG's markup, so pasted songs transpose like fetched ones.
/// Text that already carries `[ch]` markup is kept as it is.
pub fn parse_pasted(artist: &str, title: &str, text: &str) -> Tab {
    let text = text.replace("\r\n", "\n");
    let content = if text.contains("[ch]") {
        text
    } else {
        text.lines()
            .map(|line| {
                if !is_chord_line(line) {
                    return line.to_string();
                }
                // Wrap each chord where it stands, keeping every space so the
                // chords stay above the syllables they belong to.
                let mut out = String::with_capacity(line.len() + 16);
                let mut token = String::new();
                for c in line.chars().chain(std::iter::once(' ')) {
                    if c.is_whitespace() {
                        if !token.is_empty() {
                            if is_chord(&token) {
                                out.push_str(&format!("[ch]{token}[/ch]"));
                            } else {
                                out.push_str(&token);
                            }
                            token.clear();
                        }
                        out.push(c);
                    } else {
                        token.push(c);
                    }
                }
                out.pop(); // the sentinel space
                out
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    Tab {
        tab_id: None,
        song_name: title.trim().to_string(),
        artist_name: artist.trim().to_string(),
        kind: Some("Chords".into()),
        version: None,
        rating: None,
        votes: None,
        capo: None,
        tuning: None,
        tonality: None,
        url: None,
        content,
        versions: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEARCH: &str = include_str!("../tests/fixtures/search.html");
    const TAB: &str = include_str!("../tests/fixtures/tab.html");
    const BLOCKED: &str = include_str!("../tests/fixtures/blocked.html");

    #[test]
    fn ug_urls_are_accepted_and_nothing_else_is() {
        assert!(is_ug_url(
            "https://tabs.ultimate-guitar.com/tab/bastille/pompeii-chords-1227805"
        ));
        assert!(is_ug_url(
            "https://www.ultimate-guitar.com/tab/bob-marley/no-woman-no-cry-chords-45479"
        ));
        assert!(!is_ug_url("http://tabs.ultimate-guitar.com/tab/x"));
        assert!(!is_ug_url(
            "https://tabs.ultimate-guitar.com.evil.com/tab/x"
        ));
        assert!(!is_ug_url(
            "https://evil.com/tabs.ultimate-guitar.com/tab/x"
        ));
        assert!(!is_ug_url("https://tabs.ultimate-guitar.com/search.php"));
        assert!(!is_ug_url("https://127.0.0.1/tab/x"));
    }

    #[test]
    fn entities_are_decoded() {
        assert_eq!(
            decode_entities("{&quot;a&quot;:&quot;Don&#039;t &amp; &lt;b&gt; &#x41;&quot;}"),
            r#"{"a":"Don't & <b> A"}"#
        );
        assert_eq!(decode_entities("a & b &bogus; c"), "a & b &bogus; c");
    }

    #[test]
    fn entities_next_to_multibyte_text_do_not_panic() {
        // The panic seen on pi1 2026-10-18: 12 bytes after this `&` falls
        // inside the fourth `’`.
        assert_eq!(decode_entities("&’’’’ x"), "&’’’’ x");
        assert_eq!(decode_entities("Beyoncé &amp; José’s"), "Beyoncé & José’s");
        // Every split point of a mixed string, not just the ones thought of.
        let text = "Don&#039;t &amp; ’é😀&x&é;&quot;ü&";
        for i in 0..=text.len() {
            if text.is_char_boundary(i) {
                decode_entities(&text[i..]);
            }
        }
    }

    #[test]
    fn a_page_without_the_store_is_blocked() {
        assert_eq!(extract_store(BLOCKED), Err(UgError::Blocked));
    }

    #[test]
    fn search_keeps_only_free_sheets() {
        let hits = parse_search(&extract_store(SEARCH).unwrap());
        assert!(hits.iter().all(|h| SHEET_KINDS.contains(&h.kind.as_str())));
        assert!(!hits.iter().any(|h| h.kind == "Official" || h.kind == "Pro"));
        let first = &hits[0];
        assert_eq!(first.song_name, "Pompeii");
        assert_eq!(first.artist_name, "Bastille");
        assert_eq!(first.tab_id, 1227805);
        assert_eq!(first.kind, "Chords");
        assert_eq!(first.votes, Some(1204));
    }

    #[test]
    fn versions_group_by_song_chords_first_best_first() {
        let groups = group_versions(parse_search(&extract_store(SEARCH).unwrap()));
        assert_eq!(groups[0].song_name, "Pompeii");
        let kinds: Vec<_> = groups[0].versions.iter().map(|v| v.kind.as_str()).collect();
        assert_eq!(kinds.first(), Some(&"Chords"));
        assert_eq!(kinds.last(), Some(&"Tabs"));
        // 4.8 from 1,204 votes beats 5.0 from one.
        assert_eq!(groups[0].versions[0].tab_id, 1227805);
        // A differently-cased title is the same song.
        assert!(
            groups
                .iter()
                .filter(|g| norm(&g.song_name) == "pompeii")
                .count()
                == 1
        );
        assert!(groups.iter().any(|g| g.song_name == "Pompeii MMXXIII"));
    }

    #[test]
    fn a_tab_page_gives_the_sheet_and_its_other_versions() {
        let tab = parse_tab(&extract_store(TAB).unwrap()).unwrap();
        assert_eq!(tab.song_name, "Pompeii");
        assert_eq!(tab.artist_name, "Bastille");
        assert_eq!(tab.tab_id, Some(1227805));
        assert_eq!(tab.capo, Some(2));
        assert_eq!(tab.tuning.as_deref(), Some("E A D G B E"));
        assert!(tab.content.contains("[ch]Am[/ch]"));
        assert!(!tab.content.contains('\r'));
        assert!(tab.content.contains("café’s"), "multi-byte text survives");
        assert!(
            tab.content.contains("doesn't"),
            "entities inside the sheet are decoded"
        );
        assert_eq!(tab.versions.len(), 2);
        assert!(tab.versions.iter().all(|v| v.tab_id != 1227805));
    }

    #[test]
    fn empty_meta_is_an_array_and_still_parses() {
        let store = serde_json::json!({"store":{"page":{"data":{
            "tab": {"id": 1, "song_name": "S", "artist_name": "A", "type": "Chords"},
            "tab_view": {"wiki_tab": {"content": "[ch]G[/ch]"}, "meta": []}
        }}}});
        let tab = parse_tab(&store).unwrap();
        assert_eq!(tab.capo, None);
        assert_eq!(tab.tuning, None);
        assert!(tab.versions.is_empty());
    }

    #[test]
    fn a_page_with_no_sheet_says_so() {
        let store = serde_json::json!({"store":{"page":{"data":{"tab":{}, "tab_view":{}}}}});
        assert!(matches!(parse_tab(&store), Err(UgError::Other(_))));
    }

    #[test]
    fn chords_are_recognised() {
        for c in [
            "A", "Am", "C#m7", "Bb", "Ebmaj7", "Dsus4", "Cadd9", "G/B", "D/F#", "F#m7b5", "E7(#9)",
            "Bdim", "Caug", "A7sus2",
        ] {
            assert!(is_chord(c), "{c}");
        }
        for w in [
            "And", "Bad", "Am I", "Hello", "H", "G/H", "Emotion", "Cm/Fo",
        ] {
            assert!(!is_chord(w), "{w}");
        }
    }

    #[test]
    fn pasted_chord_lines_are_marked_up_in_place() {
        let tab = parse_pasted(
            " Bastille ",
            "Pompeii",
            "[Verse]\r\nAm       F\r\nHere is a line made up for the tests\r\nC   G/B  | x2\r\nA man",
        );
        assert_eq!(tab.artist_name, "Bastille");
        assert_eq!(
            tab.content,
            "[Verse]\n[ch]Am[/ch]       [ch]F[/ch]\nHere is a line made up for the tests\n\
             [ch]C[/ch]   [ch]G/B[/ch]  | x2\nA man"
        );
    }

    #[test]
    fn pasted_ug_markup_is_kept() {
        let tab = parse_pasted("A", "S", "[ch]G[/ch] la");
        assert_eq!(tab.content, "[ch]G[/ch] la");
    }
}
