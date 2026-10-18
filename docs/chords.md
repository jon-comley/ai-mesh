# Songs (chord sheets from Ultimate Guitar)

The **Songs** tab finds chord sheets on [Ultimate Guitar](https://www.ultimate-guitar.com)
(UG), lets you flip between the different versions people have posted of a song,
and saves the ones you like into a library to play from next time. Design:
`plans/chords-songbook.md`.

It runs on the coordinator only, with no node involvement, no deploy step and no
account or key. UG has no public API, so the coordinator reads the same pages a
browser does, sending one request each time you search or open a version.

## Using it

- **Find a song.** Press *+ Find a song*, type a title, an artist or both, and press
  Enter. Results are grouped by song, chords before tabs and the best-rated version
  first. Tap a song to open its best version, or *N versions ▾* to pick one.
- **Switch versions.** In the song view, the dropdown and ‹ › move between every
  version of the song. Saved versions are marked ★. For a song opened from your
  library, *More versions* fetches UG's full list.
- **Save.** *Save* stores the whole sheet on the coordinator, so it opens instantly
  and keeps working if UG is down or the version is removed. You can save several
  versions of one song. Under ⋯, *Refresh from Ultimate Guitar* re-fetches the
  sheet and keeps your settings, and *Delete from library* removes it.
- **Library.** *A–Z* lists every saved song by title. *By artist* groups them
  under each artist, and each artist can be collapsed. "The Beatles" files under
  B. The filter box narrows either list, and *Recently played* shows the last few
  songs you opened.
- **Playing.**
  - **♭ / ♯** transposes every chord. The capo, any unusual tuning and the key
    are shown under the title.
  - **A− / A+** changes the text size, remembered on that device.
  - **⛶** goes full screen for a music stand. Esc leaves it.
  - The screen is kept awake while a song is open, where the browser allows it.
- **Auto-scroll** has an on/off switch, off by default and remembered on that device.
  - **Off:** the sheet scrolls only by hand.
  - **On:** ▶ starts it and − / + set the speed. Tapping the sheet, or Space on a
    keyboard, pauses and resumes it.
  - Scrolling by hand pauses it, so you can go back a verse.
  - It stops at the end of the song.
  - A saved song keeps its own speed and transpose.
- **Listen along.**
  - **YouTube** opens a YouTube search for the song. Paste a video's link and it
    plays in a small player above the sheet, and a saved song remembers it.
    YouTube shows its adverts unless you're signed in to YouTube Premium in that
    browser; ai-mesh can't remove them.
  - **Spotify** plays the song on the house speakers through the mesh's Spotify
    player (`docs/music.md`), with no adverts. It needs the music node running.
- **Paste instead.** *Paste a link or the chords instead* opens a UG link directly,
  or saves chords copied from anywhere: chord lines above lyric lines, which are
  recognised and transpose like a UG sheet.

## Starter songs

The first time the coordinator starts, it fetches four versions into the library in
the background: No Woman No Cry (Bob Marley), Pompeii (Bastille), Angel (Jimi Hendrix)
and Clint Eastwood (Gorillaz). The list is `STARTER_SONGS` in
`coordinator/src/http/api/chords.rs`.
- **Each is added once.** Delete one and it stays deleted.
- **A blocked fetch is retried.** If UG blocks one, it's tried again at the next
  start, and the coordinator's log shows `starter song not added`.

Only the links are in this repo. A chord sheet includes the song's lyrics, which are
copyrighted, so sheets are only ever stored in the coordinator's database
(`/var/lib/ai-mesh/ai_mesh.db`), never committed.

## If Ultimate Guitar changes or blocks

An error saying UG "didn't return the song" means the page came back without the data
the coordinator reads. Usually that's a bot check, or UG has changed its page.
Pasting the chords still works in the meantime.

The coordinator reads the JSON UG embeds in each page, in
`<div class="js-store" data-content="…">`:
- **Search:** `store.page.data.results`
- **A tab page:** `store.page.data.tab`, `.tab_view.wiki_tab.content` (the sheet),
  `.tab_view.meta` (capo, tuning) and `.tab_view.versions`.

To check or fix it:

1. Save a real search page and a real tab page from a browser into
   `chords/tests/fixtures/` (`search.html`, `tab.html`). Replace any lyrics in
   them with invented lines before committing: this repo is public.
2. Run `cargo test -p chords`. The failing assertions point at the field that moved.
3. Fix the paths in `chords/src/ug.rs` (`parse_search`, `parse_tab`).
