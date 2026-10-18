//! Chord sheets from Ultimate Guitar: search, fetch one version, and turn a
//! pasted sheet into the same markup. Coordinator-only, like `ebay` — see
//! plans/chords-songbook.md.

pub mod client;
pub mod ug;

pub use client::UgClient;
pub use ug::{SearchHit, SongGroup, Tab, UgError, group_versions, is_ug_url, parse_pasted};
