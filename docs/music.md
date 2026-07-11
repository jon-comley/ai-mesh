# Music (Spotify)

Say "play Blackbird by the Beatles", "pause", "skip", "go back 30 seconds",
"turn it up", or "what's playing?" in dashboard chat or to the voice puck.
Design and phases: `plans/spotify-music.md`.

How it works: the coordinator's `music_control` tool routes commands to the
music node (pi2), whose agent drives the Spotify Web API — search, playback
control, status. Audio comes from a `librespot` Spotify Connect player whose
raw PCM feeds **snapserver** (multi-room transport, sample-synced); the
agent supervises a local **snapclient** that plays the stream into the
paired Bluetooth speaker. Snapcast adds a fixed ~1 s buffer — irrelevant for
music, and it's what makes rooms play in sync.

**Adding a room later**: put a speaker on any Linux box, install
`snapclient`, point it at pi2 (`snapclient --host <pi2-ip>`) — it joins the
synced stream immediately. Per-room on/off via the `music_control` `rooms`
param is deferred until a second speaker actually exists.

## One-time setup

### 1. Spotify account with Premium

Playback control is a Premium-only Spotify feature — on a free account every
player command fails with "playback control needs Spotify Premium".

1. Go to spotify.com → Sign up (or log in).
2. Premium → Individual plan → complete payment (usually a free first month).

### 2. Developer app (free, same account)

This gives ai-mesh its own API credentials.

1. Go to developer.spotify.com → Log in with the same Spotify account.
2. Dashboard → accept the developer terms on first visit → **Create app**.
3. Fill in:
   - App name: `ai-mesh` (anything works)
   - Description: anything
   - **Redirect URI: `http://127.0.0.1:8888/callback`** — exactly this;
     Spotify no longer accepts `http://localhost`, and the port must match.
   - Which API/SDKs: tick **Web API**.
4. Save, then open the app's **Settings**: copy the **Client ID**, and click
   *View client secret* to copy the **Client Secret**.

### 3. Authorize the control plane

WSL2 has no browser, so this is a paste-the-URL flow:

```
SPOTIFY_CLIENT_ID=... SPOTIFY_CLIENT_SECRET=... just spotify-auth
```

Open the printed URL in any browser (Windows, phone), log in, approve. The
browser then fails to load a `http://127.0.0.1:8888/callback?code=...` page —
that's expected. Copy the full address-bar URL of that dead page and paste it
into the terminal. The helper writes `~/.config/ai-mesh/spotify.env`
(chmod 600, never committed).

### 4. Push credentials to the music node

```
just spotify-push-creds pi2
```

Installs them as a systemd drop-in
(`/etc/systemd/system/ai-mesh-agent.service.d/spotify.conf`) and restarts the
agent. Drop-ins survive `just deploy-node pi2` re-runs.

### 5. The playback device (librespot)

```
just build-librespot
just deploy-librespot pi2
just spotify-login pi2
```

`spotify-login` is a **second, independent login**: step 3 authorized the
Web API control plane (refresh token in the drop-in), this one authenticates
the librespot playback device (credentials cached on pi2 at
`~/.ai-mesh/spotify-cache/credentials.json`). Redoing one never fixes the
other.

## Troubleshooting

| Symptom | Cause |
|---|---|
| "Spotify credentials are not configured on this node" | Steps 3–4 not done (or drop-in lost — re-run `just spotify-push-creds pi2`) |
| "the Spotify player 'AI Mesh' isn't registered yet — is librespot running?" | librespot down on pi2, or step 5's login never done — check `journalctl -u ai-mesh-agent` on pi2 |
| "playback control needs Spotify Premium" | The account is on the free tier |
| "Spotify authorisation failed" | Refresh token revoked — re-run steps 3–4 |
| "the music player didn't answer in time" | pi2's agent is down or the mesh link dropped — check `just nodes` |
| Command works but no sound | Check the chain in order: `systemctl status ai-mesh-snapserver` on pi2 (reads the FIFO), then the agent journal for snapclient restarts, then the Bluetooth sink — a stale/vanished sink silently falls back to the default sink; re-pair via the dashboard, the supervisor picks it up on next snapclient restart |
| "no music node connected" | pi2 offline, or its agent built without the `music` feature |

Two independent credential stores, one more time: the Web API refresh token
(steps 3–4, control plane) and librespot's `credentials.json` (step 5,
playback). They fail independently and are fixed independently.
