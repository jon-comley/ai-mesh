# Hunts (eBay Bargain Finder)

The **Hunts** tab watches eBay for bargains on something you want: paste a
listing URL, the coordinator asks the configured LLM for good search terms
(the clean name plus realistic misspellings/mis-listings sellers use), you
pick daily timeslots, and it searches on that schedule — surfacing new
matches in a live ticker and, if configured, a phone push via
[ntfy](https://ntfy.sh). Design: `plans/ebay-bargain-finder.md`.

Coordinator-only — no agent/node involvement, no deploy step. eBay
credentials and the ntfy topic are entered once in the tab's settings block
and persisted server-side (`dashboard_preferences`), so they survive
redeploys the same way the Online AI tab's API key does.

## One-time setup

### 1. eBay developer credentials

1. Go to [developer.ebay.com](https://developer.ebay.com) → sign in with (or
   create) an eBay account → **Get an API key**.
2. Create a **production** keyset (not sandbox — sandbox data is fake and
   useless for real bargain-hunting). Note the **App ID (Client ID)** and
   **Cert ID (Client Secret)**.
3. Open the **Hunts** tab in the dashboard → *eBay & notification settings* →
   paste both in and save.

No OAuth consent flow is needed: Hunts only calls the Browse API's public
`item_summary/search` and `item/get_item_by_legacy_id` endpoints, which use
an app-level (client-credentials) token, not a user login.

### 2. Notifications (optional)

Hunts works without this — new matches still show up in the ticker — but a
push needs an [ntfy](https://ntfy.sh) topic:

1. Pick a topic name. ntfy topics are **public and guessable by default** —
   use a long random string (e.g. `ai-mesh-hunts-<random>`), or self-host
   ntfy, rather than something like `my-ebay-hunts`.
2. Install the ntfy app (iOS/Android) or subscribe in a browser at
   `https://ntfy.sh/<your-topic>`, and subscribe to that topic.
3. Paste the full topic URL (e.g. `https://ntfy.sh/ai-mesh-hunts-x7q2p9`)
   into the Hunts tab's settings block and save.

### 3. Online AI (recommended, not required)

Term generation and the bargain-vs-not verdict both go through the same
cloud gateway as the **Online AI** tab (`docs/online-ai-gateway.md`). Without
it configured, Hunts still works in a **heuristic fallback**: search terms
default to just the item's own title (no misspelling suggestions), and every
new match is treated as notify-worthy with no bargain/not-a-bargain
reasoning — the ticker shows "not yet judged" instead of a verdict.

## Using it

1. **New hunt** → paste an eBay item URL → **Analyze**. This looks the item
   up and (if Online AI is configured) fills in a set of term chips — tap a
   chip to enable/disable it, × to remove it, or type your own in the box
   below.
2. Tap timeslots on the 24-hour strip for when this hunt should run each day
   (local time — handles BST/GMT transitions automatically).
3. **Create hunt**. It's armed immediately and re-arms itself on every
   coordinator restart from what's persisted in SQLite — you don't need to
   keep anything open for it to keep checking.
4. From an existing hunt: **Check now** runs one cycle immediately (doesn't
   wait for the next timeslot), the enable/disable button pauses it without
   deleting it, and **Delete** removes it along with its history.
5. The ticker (main panel) shows every new match, newest first, with its
   price, which term matched, and the LLM's verdict if judged. **Dismiss**
   just marks it reviewed — it doesn't delete anything.

## Troubleshooting

| Symptom | Cause |
|---|---|
| "eBay client_id/client_secret not configured" | Step 1 not done, or a typo — re-check in the settings block (the secret is masked after saving; re-paste it to correct it) |
| "could not find an eBay item id in that URL" | The pasted URL isn't a recognisable eBay listing link (`.../itm/<id>` or `.../itm/<slug>/<id>`) — copy the URL straight from eBay's own share/address bar, not a shortened link |
| Terms are just the plain title, no misspellings | Online AI isn't configured (step 3) — this is the heuristic fallback, not an error |
| Finds show "not yet judged" for everything | Same as above, or the LLM's reply omitted that item from its batch verdict — it's still a real match, just unscored |
| No phone push even though matches appear in the ticker | ntfy topic not set (step 2), or the LLM judged the match as *not* a bargain (pushes only fire for judged bargains or in heuristic mode — see `plans/ebay-bargain-finder.md`) |
| A hunt seems to have stopped checking | Confirm it's still enabled (sidebar shows "on"/"off"); a coordinator restart re-arms every enabled hunt automatically, so this should be self-healing |
