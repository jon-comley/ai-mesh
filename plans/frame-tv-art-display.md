# Frame TV Art Display — Local Replacement for Samsung's Art Store Subscription

## Context

Buying a Samsung QE32LS03C (32" Frame TV) for the art-mode look, without
paying £4.99/month for Samsung's Art Store subscription. Goal: a Pi Zero 2 W
hidden in an in-wall recess feeds the TV a fullscreen HDMI art slideshow
sourced from public-domain/open-access collections, driven by ai-mesh
(rotation schedule, room automation triggers, remote control) — the same
mesh that already runs lighting and sensors. The TV's own Art Mode /
SmartThings / cloud features are never used; the TV is treated as a dumb
HDMI display panel that ai-mesh happens to also have a remote-control channel
into (switch input, power, brightness).

This document is the corrected/expanded version of an initial spec. Section
11 lists what changed and why — worth reading before building, since a few
of the original assumptions don't hold on this exact hardware.

**2026-07-06 — development order pivoted, ahead of the recess.** The
electrician for §2's wall recess/socket isn't booked yet, so building
against the Pi Zero 2 W in-place isn't possible right now. Rather than wait,
development starts on a **Pi 4** with the TV on a stand in the bedroom —
same aarch64 target, same mesh agent, same `capability-art` code; only the
enclosure/heat/mini-HDMI specifics in §3 are Pi-Zero-only and don't apply
yet. §10's sequencing is reordered below to match: the software (step 3)
now comes *before* the electrician/recess step, not after. See the updated
§10 for the concrete new order and §3 for a call-out of what changes when
this eventually moves to the Pi Zero 2 W.

**2026-07-08 — superseded: the Pi 4 stays permanently; §3's move to the
Pi Zero 2 W is not happening.** Decided in
`plans/audio-output-integration.md` ("Placement decision"): this node is
becoming the mesh's audio-output workhorse too (HDMI audio injection to
the soundbar/TV, per that plan's Phase 2+), on top of art mode. The Zero
2 W can't take on that job — no reliable 1080p HDMI path and no spare
headroom — so it's redeployed elsewhere instead (currently leaning
towards hosting the new kitchen Bluetooth speaker; see that plan). §3
below (mini-HDMI, heat-in-a-sealed-cavity, VideoCore IV decode limits,
etc.) is kept as historical record of why the Zero was considered and
ruled out physically too, not as a pending task.

---

## 1. Purchase & mounting

- Samsung QE32LS03C, native 1080p, no One Connect Box, no Ethernet port.
- TV sits flush; a custom in-wall recess behind it hides all hardware and
  cabling; a double socket inside the recess powers the TV and the Pi.

## 2. Wall recess & cable routing

**⚠ Get a qualified electrician for this, don't DIY it.** The wall has
tanking/a shower on the other side — that makes it a "special location" for
UK wiring regs (BS 7671 zones around a wet area impose extra requirements:
RCD protection, cable routing/zone restrictions, IP-rated enclosures where
relevant), and a new socket fed from a spur off the ring main is notifiable
electrical work under Part P of the Building Regs in England/Wales unless
done or certified by a registered electrician (a Part P registered
electrician self-certifies; otherwise it needs Building Control sign-off).
This isn't a "figure it out from a blog post" job given the wet-wall
adjacency — get it designed and signed off by someone qualified before any
foam/plasterboard gets cut. Once that's decided, the *layout* is simple and
matches the original spec:

- Only the new spur feeding the double socket into the ring main runs inside
  the wall.
- Everything else stays inside the recess: TV IEC lead, Pi power lead, the
  HDMI cable between them, and the Ethernet cable entering from the side or
  bottom (not routed down the wall).

**Sealed-cavity heat** — foam-filled, no venting. A Pi Zero 2 W is genuinely
low-power (roughly 1–2 W idle, a few W under load), so this isn't a serious
risk on its own, but a fully sealed foam cavity has nowhere to shed even
that — and the recess also holds a power brick and a USB-Ethernet adapter,
both mild heat sources, sitting right behind a TV that generates its own
heat in use. Two cheap mitigations: prefer a recess box/enclosure that isn't
literally airtight (a few mm gap or a vent slot facing away from the room
side is enough — this doesn't need to fight the acoustic sealing, just avoid
a fully closed thermos), and once the Pi's running, let ai-mesh's existing
per-node health monitoring (`HealthUpdate`, already tracks CPU/RAM/GPU/temp)
watch this node's temperature like any other — no new code needed, just
keep an eye on the Health tab for the first few weeks in different seasons.

## 3. Hardware: Pi Zero 2 W as the HDMI client (superseded 2026-07-08 — not happening, see note above)

**Development is currently happening on a Pi 4 instead** (TV on a stand,
no recess yet — see the 2026-07-06 note above). None of the software this
section leads into (`capability-art`, the mesh wiring) is Pi-Zero-specific;
everything below is what changes for the *eventual* move to the Pi Zero 2
W once the recess and that hardware are both ready — full-size HDMI vs.
mini-HDMI, a real Ethernet port vs. a USB-OTG adapter, hardware HEVC decode
vs. H.264-only, and far more RAM/CPU headroom on the Pi 4 in the meantime.
None of that affects `capability-art` itself, which only shells out to
`fbi` (see §4's correction below) and speaks the mesh protocol — the
migration should be close to a straight card-swap (reflash, set
`NODE_FEATURES=art`, move the HDMI cable) rather than a rewrite, which is
the whole point of building it this way now.

- **Pi Zero 2 W** — quad-core Cortex-A53 (BCM2710A1), 512 MB RAM. Same
  aarch64 architecture ai-mesh already cross-compiles for (pi1's Pi 5 uses
  the same target), so the existing `deploy-node` build path works
  unmodified.
- **Connector correction**: the Pi Zero 2 W's HDMI port is **mini-HDMI**, not
  a full-size port — you need a mini-HDMI-to-HDMI cable or adapter, not a
  generic "flat HDMI ribbon." Flat/ribbon HDMI extenders do exist and work
  fine at 1080p, just make sure the connector on the Pi end is mini-HDMI.
- Two micro-USB ports: one is power-only (`PWR IN`), the other is USB-OTG
  (data) — the USB-Ethernet adapter goes on the OTG port. Use a genuinely
  good 5V/2.5A+ supply and a short, decent-quality cable; Pi Zero boards are
  known to be sensitive to under-voltage, and adding a USB Ethernet dongle's
  draw on top of HDMI + CPU load is exactly the kind of load that exposes a
  marginal supply.
- Passive cooling only — no fan needed at this power/heat level.

## 4. Software stack on the Pi

- **OS**: Raspberry Pi OS Lite (no desktop environment — a fullscreen
  image viewer doesn't need one, and it keeps the 512 MB budget comfortable).
- **Art viewer: `fbi` against the raw framebuffer, corrected twice from the
  original `feh`/`pqiv` choice.** Confirmed against the first real node (a
  Pi 4, brought up ahead of this Pi Zero migration — see the top-of-document
  2026-07-06 note) that a Lite install has no X server at all, so an
  X11-only viewer like feh can't start. The first fix, `mpv --vo=drm`,
  does run without X — but Debian's `mpv` package drags in a full
  GTK/X11/audio stack as *unused linked dependencies* (~600 MB across 265
  packages, confirmed on the real node) purely because that's how the
  distro build was configured, wildly disproportionate for a single-purpose
  kiosk display and a bad fit for the eventual 512 MB Pi Zero 2 W. `fbi`
  needs only a handful of small packages and writes straight to `/dev/fb0`.
  Two things had to be true on the node for that device to exist and be
  usable at all, both confirmed hands-on and now automated in
  `scripts/install-node-linux.sh` for the next node:
  - This SoC's default full-KMS driver (`vc4-kms-v3d`) exposes no `/dev/fb0`
    — switching to the legacy "fake KMS" overlay (`vc4-fkms-v3d`, still
    hardware-accelerated) plus `hdmi_force_hotplug=1` in
    `/boot/firmware/config.txt` (so the Pi assumes a display exists even
    before the TV is wired up) is what makes `/dev/fb0` appear, confirmed via
    `dmesg` on the real node.
  - Even with `/dev/fb0` readable by the agent's own user (already in the
    `video` group), `fbi` still needs to control the active VT via
    `/dev/tty1`, and VT-switching ioctls require actual root regardless of
    file permissions on that device — a udev rule opening up group access
    to `/dev/tty1` was tried and confirmed *not* sufficient (fbi silently
    exited). Since the agent's systemd unit already runs as an account with
    full passwordless sudo (this project's existing solo-home-lab
    NOPASSWD rationale — see `scripts/install-node-linux.sh`), `capability-art`
    runs fbi via `sudo -n` rather than opening a new permission hole.
  - `sudo`'s own process exits as soon as it has forked fbi (fbi
    immediately re-forks and detaches to hold the console) — confirmed via
    `ps --forest` on the real node that a `Child` handle captured at spawn
    time is stale within moments. `capability-art` kills the old viewer by
    process name (`pkill -x fbi`) and checks liveness the same way
    (`pgrep -x fbi`) rather than holding a child handle — a custom
    Python/OpenGL viewer remains more work for no benefit at this stage;
    only worth it later for effects like crossfade transitions.
- **ai-mesh agent** with a new `art` feature (see §6) — receives
  show/next/mode commands from the coordinator over the existing mesh TCP
  connection, same as every other node.
- **Local image cache** — the agent downloads and caches art locally so the
  slideshow keeps working through a coordinator restart or a brief network
  blip; the coordinator is the source of truth for the catalogue and
  rotation schedule, not a hard dependency for every frame change.

### HDMI output

Locked to 1080p (matches the panel natively — no upscaling needed),
always fullscreen, Samsung's own TV UI never involved once the input is
switched to the Pi.

### Movie streaming — corrected expectations

The original spec claimed both H.264 *and* H.265 1080p playback. Correcting
that: the Pi Zero 2 W's GPU (VideoCore IV, same generation as the Pi 3) has
**hardware-accelerated H.264 decode**, which should handle 1080p H.264
comfortably. It has **no hardware HEVC/H.265 decode** — that arrived with
the Pi 4's VideoCore VI. Software-decoding 1080p H.265 on a quad-core
Cortex-A53 with 512 MB RAM is unlikely to keep up in real time; expect
stutter or dropped frames rather than smooth playback. If H.265 sources
matter, transcode them to H.264 on the server before they reach this node
(any of the existing compute nodes can do this) rather than relying on the
Pi to software-decode them live. DLNA/SMB/NFS/Kodi/VLC client support is all
realistic for H.264 sources from any device on the LAN.

## 5. Open-source / public-domain art (and now text) sources

- **WikiArt**, **Rijksmuseum Open Access**, **The Met Open Access**,
  **Unsplash** curated art collections — all legally reusable, no
  subscription.
- **Project Gutenberg** and **Wikisource** for poems from the same era/
  movement — both explicitly public-domain-focused (same ethos as the image
  sources above) and both have machine-readable access (Gutenberg's catalog
  mirrors/metadata, Wikisource's API) rather than needing to scrape a
  reading-room page by hand.
- **Album covers — a real licensing distinction from everything else here,
  worth being explicit about.** "Album covers from the 1980s", "album
  covers from UK indie bands" is a fun addition, but unlike WikiArt/
  Rijksmuseum/Met/Gutenberg/Wikisource — chosen specifically because
  they're public domain or open-access — album cover art is ordinary
  copyrighted commercial artwork, typically owned by the label/artist, and
  a 1980s cover is nowhere near public domain. That's not a reason to skip
  it: displaying it on a private, non-commercial home display for personal
  viewing (nothing published, resold, or redistributed) is a genuinely
  different situation from redistributing it, and is broadly the same
  territory as displaying a physical sleeve or printed poster at home, or
  any music app/media centre (Kodi, Plex, a phone's music player) showing
  cover art for a library you own — all of which already do exactly this
  without controversy. It's worth keeping that framing (strictly personal,
  private, unpublished) rather than treating it as equivalent to the
  public-domain sources above. For sourcing: **Cover Art Archive**
  (coverartarchive.org, run by the Internet Archive alongside MusicBrainz)
  is the standard, purpose-built API apps use for exactly this — keyed off
  a MusicBrainz release id, not a scrape of a retailer's product photos.
- Image pipeline: download high-res → crop to 16:9 → generate a matte/
  border overlay to mimic the Frame's art-mode look → brightness-correct →
  store on the coordinator (or wherever the art catalogue lives) → Pi pulls
  via the mesh, caches locally, displays fullscreen.
- **Poem pipeline is the same shape, different rendering step**: instead of
  cropping a downloaded photo, typeset the poem's text onto a 16:9 image
  (a simple text-layout pass — e.g. Pillow — on a plain/parchment
  background matching the matte aesthetic, title + poet + year as a
  caption). Output is still just a 16:9 image file, so it drops into the
  exact same catalogue/rotation/display pipeline as a painting — no
  special-casing needed anywhere downstream of ingest. `medium: "poem"`
  (see below) is what tells `art_show`/curation this entry is text-derived,
  nothing else needs to know.
- Both pipelines are one-time or occasional batch jobs (curate a library,
  not a live feed), so neither needs to run on the Pi itself — do it on a
  compute node with more headroom and just ship finished 16:9 images to the
  catalogue.
- **Capture metadata at ingest, not just the image** — creator, title,
  year/creation date, movement/style, source collection, and medium/object
  type (painting, sculpture, drawing, print, decorative art, **poem**, ...).
  Renamed from "artist" to **creator** here specifically because poems make
  "artist" the wrong word for a poet — cheap to get right now, before any
  code exists, rather than a rename later. This isn't extra scraping work:
  WikiArt/Rijksmuseum/Met Open Access return this metadata alongside the
  image already; Gutenberg/Wikisource return author/date alongside the
  text. It just needs to be kept rather than discarded during ingest. This
  is what makes §6's voice-browsing idea possible later without redoing the
  catalogue.
- **Not paintings-only — sculpture, poems, and other media belong in the
  same catalogue.** Image coverage varies by source: Met Open Access spans
  every curatorial department (sculpture, decorative arts, arms and armor,
  etc.), not just paintings, and Rijksmuseum's collection API covers
  sculpture and applied art too; WikiArt is comparatively painting/drawing/
  print-focused, so lean on the Met/Rijksmuseum for sculpture specifically.
  One `art_catalogue` table with a `medium` field, not separate tables per
  medium — `movement` tags (e.g. Pre-Raphaelite) should be applied across
  media at curation time where it's genuinely apt (sculpture from
  artists/circles associated with a movement; poets in the same circle as
  the movement's painters — Dante Gabriel Rossetti was both), so a single
  filtered query in §6 naturally surfaces a mixed set rather than needing a
  separate parameter per medium to opt it in.

## 6. ai-mesh integration

Follows the same shape as every other domain in this mesh (`capability-*`
crate on the node + a coordinator `api/*.rs` module) — no new architectural
pattern needed, this slots into what already exists:

### New capability crate: `capability-art`

- Mirrors `capability-reaper`'s shape (a thin capability that drives an
  external process, rather than talking to Zigbee/MQTT like
  lighting/sensors do): manages the running `fbi` process, switches
  the currently-displayed image via its control mechanism, and reports back
  status.
- New `MeshMessage` variants (wire-versioned, same convention as
  `PermitJoin`/`SensorState` etc. from the sensors work):
  `ArtShow { image_id }`, `ArtNext`, `ArtMode { on: bool }` (fullscreen art
  vs. e.g. a "TV is off" blank state), `ArtStatus` (report current image +
  viewer health back to the coordinator).
- Agent feature flag: `art` (`nodes/<pi-zero-node>.env`:
  `NODE_FEATURES=art`), same pattern as `lighting`/`sensors`/`reaper`.

### New coordinator module: `coordinator/src/http/api/art.rs`

Following the api/mod.rs domain recipe (sibling to `lights.rs`/`sensors.rs`):

- `GET /api/art` — list the catalogue.
- `GET /api/art/{id}` — one image's metadata.
- `POST /api/art/show` — push a specific image to the display node now.
- `POST /api/art/next` — advance the rotation.
- `POST /api/art/schedule` — configure rotation timing/rules.
- A registry table (`art_catalogue`, mirroring how `scenes`/`room_effects`
  are modelled) holding image metadata (artist, title, year, movement,
  source collection — see §5) + local file paths; the rotation schedule as
  its own small table or a `dashboard_preferences`-style key/value row if
  the ruleset stays simple. `POST /api/art/schedule` should accept an
  optional filter (artist/movement/year range) + sort order alongside
  timing, so `/api/art/next` advances through a *filtered, ordered* subset
  when one's active (see the voice-browsing idea below) rather than only
  ever the whole catalogue.

### Automation triggers — this is where the sensor work already done pays off

The original spec listed "time of day, occupancy, ambient light, heating
zones, blinds position" as automation triggers — every one of those maps
directly onto infrastructure this mesh already has or has planned:

- **Occupancy** — motion sensors already shipped (SNZB-03P R2, Phase B).
- **Ambient light** — the same SNZB-03P R2 already reports illuminance
  (lux) — added specifically for this kind of use case, not just room
  cards.
- **Time of day** — the effects engine's solar calculator already computes
  sun position continuously; trivial to also drive an art rotation rule.
- **Blinds position** — Phase E (`plans/multi-domain-home.md`) — not built
  yet, hardware-gated, but the same spatial/solar engine this would hook
  into already exists.

None of this needs new sensor hardware or a new automation engine — it's a
new *consumer* of data pipelines from `plans/multi-domain-home.md` Phases B
and E. Sequencing note: art automation rules that depend on occupancy/light
should land after this project's own basic slideshow works, not before —
get the dumb version right first.

### Voice/chat browsing (idea — captured, not yet scoped in detail)

**2026-07-06 — a related but different mechanism shipped first.**
`POST /api/art/search {query, interval_secs?}` now exists (coordinator side,
`coordinator/src/http/api/art.rs`) and does something adjacent to what this
section sketches, worth distinguishing clearly: it searches the Met
Museum's Open Access API *live*, on every call — no local `art_catalogue`
table, no ingest pipeline (§5's batch curation work is still entirely
unbuilt) — filters to public-domain results with a usable image, optionally
asks whatever local model is Ready to pick/order the best subset (falls
back to the raw order otherwise), and drives the existing rotation/
auto-advance machinery (`next_art`, the background timer in
`spawn_art_rotation_timer`) — all via `ArtShow`, no new wire message. This
is a REST endpoint reachable by curl/dashboard, **not yet an intent-router
tool** — "show me some Monet" spoken or typed into chat still won't reach
it; that wiring (a real `art_show`-shaped tool schema registered with
`collect_tool_schemas`, same as `get_climate`) is the gap left between this
and the sketch below. Once `art_catalogue`/§5 curation exists, whether
`art_show` ends up querying that table, this live Met search, or both, is
still an open question — they're not mutually exclusive (a curated local
table for reliability/offline use; live search for "anything the Met has").

"Show me some Monet", "show the collection in date order", "show me the
Pre-Raphaelites and include sculpture" — a natural fit for the same
tool-calling pattern already proven out for sensors (`get_climate` in
`plans/multi-domain-home.md` Phase C: a tool the intent router can call,
answered/acted from the coordinator's own data, no node round-trip needed
for the decision itself). This is arguably the strongest showcase yet for
this project's actual differentiator — "talk to your house, and it never
leaves your house" — since browsing an art collection by natural-language
criteria is exactly the kind of thing that's awkward with a remote and
effortless with voice/chat.

Sketch, not a commitment yet:

- A new intent tool, e.g. `art_show { creator?, movement?, medium?,
  year_from?, year_to?, sort?: 'chronological' | 'random' }` — the
  coordinator filters/sorts its own `art_catalogue` table (no LLM
  involvement in the actual query, same division of labour as
  `get_climate`), picks the resulting image (or sets it as the active
  filtered rotation via `/api/art/schedule` so `art_next` continues through
  the same filtered set), and sends the existing `ArtShow { image_id }`
  message to the display node — no new wire message needed beyond what §6
  already specifies. `medium` is optional and additive, not exclusive — "the
  Pre-Raphaelites" without a medium filter should already include any
  sculpture tagged with that movement per §5, not just paintings; a medium
  filter narrows further ("just the sculptures") rather than being required
  to opt sculpture in at all.
- **A second, read-only tool for "what is this?"** — the "if asked" framing
  matters: this should be a query answered on demand, not an always-on
  caption burned into the display (Samsung's own Art Mode does the latter;
  deliberately not copying that here unless it turns out to be wanted
  later). Sketch: `art_current` (no args) — answered from whatever the
  coordinator already believes is showing via the `ArtStatus` report §6
  already plans the display node sending back, returning creator/title/year.
  Same "no node round-trip for the answer" shape as `get_climate` — the
  coordinator already has this data cached, it just needs a tool that reads
  it.
- Depends on §5's metadata capture (creator/year/movement/medium per image)
  — already planned there specifically so this doesn't require
  re-processing the catalogue later.
- Worth testing voice-recognition accuracy on art-movement names
  specifically (e.g. "Pre-Raphaelite") once local speech input exists — the
  main plan's whisper marker (`plans/multi-domain-home.md` Phase C item 4)
  — obscure proper nouns are exactly where a local STT model is likelier to
  mishear than boring commands like "turn off the lights."
- Sequencing: after the basic catalogue + manual show/next/schedule work
  (§10 steps 3–4) and after Phase C's `get_climate` pattern exists to copy
  from — this is a small addition once both are in place, not a new
  architectural lift.

### "Educate us on Victorian times" — a different shape of request

Genuinely different from `art_show`/`art_current` above: those are answered
*from the coordinator's own catalogue data, no LLM involvement in the
actual query* — the same division of labour as `get_climate`. "Educate us
on Victorian times" instead wants the LLM to **generate** explanatory
content, not just filter existing rows. Worth keeping that distinction
explicit rather than blurring it, because it changes the reliability
story: a displayed painting or a Gutenberg poem's actual text is exactly
what it claims to be, but text the model generates about "facts" carries
real hallucination risk — more so on a small local model running on
constrained hardware (Beelink/pi1) than a large cloud one. This is worth
building, but worth building honestly:

- Sketch: a new tool, e.g. `art_educate { topic }` — the coordinator (a)
  runs the *existing* `art_show`-style filter logic to select a handful of
  matching catalogue images/poems for the topic ("Victorian" resolving to
  roughly 1837–1901 and/or tagged movements), and (b) asks the local model
  to write a short piece of context about the topic, same intent-response
  mechanism already used everywhere else in this mesh.
- The generated text reuses the **poem-rendering pipeline** from §5
  unchanged (typeset text onto a 16:9 "card," same matte aesthetic) — one
  text→image renderer serves poems and generated facts alike, no new
  rendering code.
- Result is a short curated sequence (fact card + a few matching images),
  set as the active rotation the same way a filtered `art_show` already
  does — not a single image.
- **Mitigate hallucination rather than ignore it**: keep prompts scoped to
  broad, well-established context (a widely-documented era, not obscure
  claims), and prefer this over trying to make it authoritative. A
  curator-written blurb per movement/era (stored in the catalogue, shown
  instead of a live-generated one) is a meaningfully safer v1 if accuracy
  matters more than novelty — live generation can be a stretch goal once
  the rest of this works, not necessarily the first cut.
- Sequencing: after both `art_show` (image selection) and the poem
  text-rendering pipeline exist — this is a genuinely later step, not
  parallel with the rest of §6.

## 7. TV control — local WebSocket API, no SmartThings

Samsung Tizen TVs (Frame included) expose a local remote-control WebSocket:

```
wss://<tv-ip>:8002/api/v2/channels/samsung.remote.control
```

- First connection triggers a pairing prompt **on the TV itself** — accept
  it once, then persist the auth token the API returns; every reconnect
  after that is silent. (This is the same protocol several open-source
  projects — e.g. Home Assistant's Samsung TV integration, the `samsungtvws`
  Python library — already use; worth looking at one of those for the exact
  handshake/keepalive details rather than reverse-engineering it from
  scratch.)
- Used for: switch to the Pi's HDMI input, power on/off, volume, basic menu
  nav if ever needed.
- **Deliberately not used**: the TV's own Art Mode upload/gallery API. That
  API exists (the same libraries above expose it) and would let you push
  images *into* Samsung's own Art Mode — but using it re-engages the exact
  Samsung art-mode ecosystem this project exists to avoid, and gets no
  benefit over just showing a fullscreen image on the HDMI input the Pi
  already owns. Skip it; treat the TV as a dumb panel end to end.

## 8. Final architecture

```
Coordinator (pi1)
  ├─ art_catalogue registry table + rotation schedule
  ├─ api/art.rs  (list / show / next / schedule)
  └─ TV control: samsung.remote.control WS client (input switch, power)

Pi Zero 2 W (new node, NODE_FEATURES=art)
  ├─ ai-mesh agent + capability-art
  ├─ fbi fullscreen viewer, framebuffer (agent-controlled, via sudo)
  ├─ local image cache
  └─ HDMI → Frame TV (1080p, art slideshow or movie source)

Frame TV (QE32LS03C)
  ├─ HDMI input from the Pi (always-on source, no Samsung UI)
  └─ Local WS control channel only (no SmartThings, no cloud, no subscription)
```

## 9. Verification

- Pi Zero 2 W boots Raspberry Pi OS Lite, ai-mesh agent connects and appears
  on the Nodes tab with `art` in its feature list.
- `POST /api/art/show` with a test image displays fullscreen on the TV
  within a couple of seconds.
- `POST /api/art/next` advances the rotation; confirm the local cache means
  it still works with the coordinator briefly unreachable.
- TV power-on/input-switch via the WebSocket API round-trips correctly
  after a TV power cycle (re-pairing prompt only appears once).
- Health tab shows the new node's temperature sitting stable over a few
  days in the sealed recess before calling the enclosure "done."

## 10. Sequencing

**Reordered 2026-07-06** — the electrician isn't booked yet, so step 1
below no longer blocks starting; development moved ahead onto a Pi 4 with
the TV on a stand instead. Steps 2–3 have already happened in that order
(software first, since the Pi 4 + stand needed no physical work to start
against); the electrician/recess/Pi-Zero-migration step is deferred to
whenever it's actually booked, not treated as a prerequisite for the rest
of this list anymore.

1. ~~Electrician-designed recess + socket~~ — **deferred, no longer
   blocking.** Book when convenient; do the Pi Zero 2 W migration (§3) at
   the same time, once both the recess and that hardware are ready.
2. **Done** — Pi 4 (bedroom, TV on a stand) flashed with Raspberry Pi OS
   Lite (64-bit), hardwired into the LAN at <pi2> (node name `pi2`,
   chosen hardware-agnostically so it carries over unchanged through the
   eventual Pi Zero swap), SSH key access established, `just deploy-node
   pi2` run successfully — heartbeating on the Nodes tab with `art` in its
   feature list. Bringing it up surfaced two real corrections, both now in
   §4 and automated in `scripts/install-node-linux.sh`: `feh` → `mpv` (no X
   server) → `fbi` (mpv's Debian package drags in a 600 MB GTK/X11 stack),
   plus the `vc4-fkms-v3d`/`hdmi_force_hotplug=1`/`sudo` details needed to
   make `fbi` actually work headless.
3. **Done (software), verified end to end** — `capability-art` crate
   (agent-side: downloads an image, shells out to `fbi` via `sudo -n`,
   reports status back) + coordinator `api/art.rs` (`POST /api/art/show`,
   `GET /api/art/status`) + the `MeshMessage::ArtShow`/`ArtStatus` wire
   types (`WIRE_VERSION` 9). `POST /api/art/show` against the live
   coordinator correctly reached `pi2` and spawned `fbi`; a second show
   correctly killed and replaced it (confirmed exactly one `fbi` process
   throughout via `pgrep`). No TV wired up yet, so nothing's visibly
   displayed — the framebuffer writes are real, just currently going
   nowhere to be seen. Builds clean and cross-compiles for aarch64; unit-tested
   (11 tests across `capability-art` + `coordinator`). Deliberately
   minimal v1 slice at the time: one image, no rotation. See step 3.5 below
   for the rotation/auto-advance work that followed the same day.
   `MeshMessage::ArtMode` still isn't built (no TV input-switching yet —
   step 5); `ArtNext` was never needed even once rotation existed, since
   advancing just resends the existing `ArtShow`.
3.5. **Done, same day — on-the-fly slideshow, ahead of the batch pipeline.**
   Rather than wait for step 4's curated local catalogue, `POST
   /api/art/search {query, interval_secs?}` searches the Met's Open Access
   API live, filters to public-domain/has-image results, optionally has the
   Ready local LLM pick/order the best subset (20s cap, falls back to raw
   order — see the "Voice/chat browsing" section's 2026-07-06 note for the
   full design and how it relates to that section's catalogue-based
   sketch), and drives `next_art`/an auto-advance timer. Live-tested: "Leonardo
   da Vinci" found 8 real candidates, got an LLM-curated order from pi1's
   `llama3.2:1b`, displayed on `pi2`, both manual next and the timer
   advanced correctly. This is a REST endpoint, not an intent/voice tool —
   step 7 below is still the gap.
3.6. **Done 2026-07-13 — fbi replaced with mpv to kill the per-image blink.**
   First real end-to-end test against the actual HDMI-connected TV (steps 3
   and 3.5 above had only ever been verified via `pgrep`/logs, no TV wired
   up) surfaced that `fbi`'s kill-and-relaunch-per-image design causes a
   visible black-screen blink on every single slideshow advance — the
   console's graphics mode gets torn down and rebuilt each time. Investigated
   alternatives live on `pi2`: `imv` ruled out immediately (X11/Wayland only,
   no framebuffer/DRM backend at all — confirmed via `dpkg -L` and a live
   "Failed to create window" test); `mpv --vo=drm` confirmed working
   headless under the same `vc4-fkms-v3d` overlay `fbi` already needed, no
   config changes required. `capability-art` now launches one persistent
   `mpv` process and drives every later image change through its JSON IPC
   socket (`--input-ipc-server`) instead of killing/relaunching — no more
   blink. Two non-obvious things confirmed live along the way: a bare
   `loadfile` doesn't visibly redraw a frozen still image (needs a follow-up
   no-op `seek`), and mpv's built-in `vf=fade` crossfade filter gets
   permanently stuck at a black first frame for a held still image (its
   internal playback clock never advances for `--image-display-duration=inf`
   + `--keep-open=yes`), so a real animated crossfade is deferred — this step
   ships the blink-free hard-cut only. The original objection to mpv (~600 MB
   of Debian GTK/X11 deps, a bad fit for a 512 MB Pi Zero 2 W) no longer
   applies now that the Pi Zero migration is dropped (see the note at the
   top of this file) in favour of staying on the Pi 4.
4. Art library curation + pipeline (can happen in parallel with 3 — it's
   independent server-side work). Not started.
5. TV WebSocket control (input switch, power) — nice-to-have, not blocking
   the slideshow itself. Not started.
6. Automation triggers (occupancy/light/time-of-day) — deliberately last;
   depends on 3 working reliably first.
7. **Done 2026-07-13 — voice/chat browsing, `art_search` intent tool.** A
   real gap surfaced live: asking the assistant for "Rembrandt curated" did
   nothing (no tool existed to call), and nothing about the request was
   visible anywhere — the LLM just had no way to act on it. `perform_art_search`
   was factored out of `search_art`'s HTTP handler so both it and the new
   `art_search` tool (`{query, by_artist?, interval_secs?}`) share the exact
   same search/curate-or-filter/rotation/narration pipeline — free-text query,
   not limited to a fixed artist list, e.g. "show me some Rembrandt", "find
   pictures of ships", "display something calming". Also shipped
   `art_narration`'s own toggle the same session (see below) — the
   `art_current` "what is this?" read-only tool from the original step 7 scope
   is still open, smaller now that the rest of the plumbing exists.
7.5. **Done 2026-07-13 — spoken narration, `art_narration` toggle.** Each
   rotation advance (initial show, timer, or manual next) gets the local LLM
   to generate a short spoken-friendly fact about the artwork — grounded in
   the Met's title/artist/date/bio fields, not invented — read aloud through
   the existing TTS/announcement pipeline, fire-and-forget so it never holds
   up the image display. `art_narration` (`{"enabled": true/false}`) turns it
   on/off by voice/chat; on by default. Confirmed live the announcement plays
   through whichever backend is a node's *default* `AUDIO_BACKENDS` entry —
   on pi2 that's HDMI (the TV's own speakers), not the paired Bluetooth
   speaker, since broadcast-style announcements don't do per-room sink
   routing. Also surfaced (and fixed) that pi2's HDMI PipeWire sinks defaulted
   to 40% volume at the OS level — bumped to 100%, confirmed WirePlumber's own
   state-restore persists it across a full audio-stack restart.
8. True crossfade between images (parked, not started) — `mpv`'s own
   `--vf=fade` filter got stuck permanently at a black first frame for a
   held still image (`--image-display-duration=inf` + `--keep-open=yes`
   never advances mpv's internal playback clock, so the fade's `t=0` state
   never progresses — confirmed live during step 3.6's investigation). The
   current blink-free swap (persistent `mpv` process, `loadfile` + a
   `playback-restart`-gated `seek 0` to force the redraw) ships without a
   real fade. Revisiting this now has slightly better odds than when it was
   first tried: step 3.6 also added `wait_for_playback_restart`, an
   event-driven wait on mpv's own IPC stream instead of a blind sleep, which
   could plausibly be adapted to drive a real playing-forward-briefly
   fade-in instead of the current frozen-image workaround — but that's
   genuinely new design work, not a quick follow-on.

## 11. What changed from the original spec, and why

- **H.265 claim corrected** — no hardware HEVC decode on this SoC generation;
  software decode of 1080p H.265 is unrealistic on a Pi Zero 2 W. Transcode
  to H.264 server-side instead.
- **"Flat HDMI ribbon" → mini-HDMI** — the Pi Zero 2 W's HDMI port is
  mini-HDMI specifically; any cable/adapter needs the right connector.
- **Electrical work flagged explicitly** — the original spec described the
  recess/socket work as routine DIY; given the tanked/shower-adjacent wall,
  this needs a qualified (Part P registered) electrician, not a
  figure-it-out job. Added as an explicit callout rather than silently
  assumed.
- **Sealed-cavity heat** — added as a named (low but non-zero) risk with two
  cheap mitigations, rather than left unaddressed.
- **ai-mesh integration reshaped** to match this codebase's actual
  conventions — a `capability-art` crate + `api/art.rs` module + registry
  table, mirroring the existing lighting/sensors/reaper pattern precisely,
  rather than a generic bullet list of REST endpoints.
- **TV Art Mode API explicitly excluded** — the original spec's TV-control
  section only mentioned the remote-control API; worth being explicit that
  the *separate* Art Mode upload API is skipped on purpose, since it exists
  and it would be easy to reach for without noticing it re-introduces the
  exact ecosystem this project avoids.
- **Automation triggers cross-referenced** to the sensor work already
  shipped (Phase B) instead of treated as a from-scratch requirement.

## 12. Parked ideas (genuinely different shape — not scoped, not sequenced)

Things worth remembering but different enough in kind from the art/poem/
album-cover catalogue above that they shouldn't be force-fit into
`art_catalogue`'s image-file model — each would want its own small design
pass when it's actually time to build it, not now:

- **Karaoke** — the same Pi/HDMI/TV setup could double as a karaoke display
  (time-synced lyrics over a backing track), but this is a materially
  different data shape (audio + time-synced text, not a static image) and a
  materially different licensing situation from everything above: there's
  no broad public-domain equivalent to Cover Art Archive/Gutenberg for
  lyric-timing files, since both the lyrics and "instrumental backing
  track" karaoke versions are typically separately-licensed commercial
  products. Realistically this works best against music you already own
  plus self-timed/community `.lrc` lyric files (a long-standing hobbyist
  practice) rather than trying to build a clean "karaoke catalogue" the way
  §5 did for art. Likely its own small capability (`capability-karaoke`?)
  rather than a mode of `capability-art` — genuinely separate feature,
  worth a proper look when it's actually being built rather than bolted on
  here.
- **"Now playing" live album art from a music streaming service** —
  different in kind from the static curated catalogue (§5): not a rotation
  through pre-ingested images, but a live mode that polls whatever's
  currently playing and shows *that* album art, reverting to the normal
  slideshow when playback stops. Provider choice matters a lot for
  feasibility: Spotify's Web API exposes a currently-playing endpoint
  (album art URL included) and is the standard hobbyist path for exactly
  this — free API access, huge amount of prior art (smart-mirror/Home
  Assistant/Pi projects already do this). Amazon Music (the Prime-included
  tier) has no comparable public API for this — its developer surface is
  Alexa-Skills/advertising-oriented, not "read what's currently playing."
  Use the official API, not scraping — a documented "now playing" endpoint
  is exactly the kind of personal, non-commercial use these APIs are meant
  for and there's precedent for it; reverse-engineering a private endpoint
  instead is both a terms-of-service risk and fragile (breaks whenever the
  app changes, since it was never a public contract). Likely a new mode of
  `capability-art` (same fullscreen-image display mechanism, different feed
  — a poll loop instead of a curated rotation) rather than a new capability
  entirely, unlike karaoke above.
