# Live test plan — 2026-07-10 fixes

Kitchen lights are bricked (13 orphaned bulbs, known issue from 2026-07-07) — scene/effect tests use a different room instead.

## Step 0 — Deploy

- [ ] `just deploy-coordinator pi1`
- [ ] `just deploy-node pi2`

Picks up: scene_load fix, effect-exclusion, Snake path fix, Bluetooth pairing, art fullscreen/UA fix, audio ack-loop.

## Step 1 — Bluetooth pairing (Fishman amp)

No lighting dependency.

- [ ] Dashboard → AV section → pi2's bluetooth row → **Scan for Bluetooth**
- [ ] Fishman shows up live with a plausible signal bar (check while the scan countdown is still running, not just at the end)
- [ ] Click **Use this device** → success toast, no error
- [ ] Trigger a room-routed voice reply or announcement to that room → audio actually comes out of the Fishman (real test — pairing succeeding alone doesn't prove playback routes correctly)

## Step 2 — Frame TV art fullscreen

No lighting dependency.

- [ ] `POST /api/art/show` with a Wikimedia Commons URL (the one that 403'd before) → succeeds now (User-Agent fix)
- [ ] Look at the TV → no matte border, true edge-to-edge (fullscreen config fix)

## Step 3 — Puck audio fallback

No lighting dependency.

- [ ] Ask the puck a normal question out loud → it answers through its own speaker (re-confirms the fix survived today's redeploys)

## Step 4 — Scene + effect fixes

Needs a working room (not kitchen) — fill in: `________________`

- [ ] Activate an effect (e.g. Aurora) in that room
- [ ] Ask the assistant by voice/chat to change one bulb in that room (e.g. "turn the lamp red") → takes effect *and stays* past the next tick (the exclusion fix — previously the effect would silently revert it)
- [ ] Save a scene while that effect is running
- [ ] Recall the scene via voice/chat (`scene_load` tool — previously dead, always replied "not yet implemented") → actually recalls now
- [ ] If Snake is available in that room: reorder/add a bulb while it's running → no panic/crash in the coordinator logs
