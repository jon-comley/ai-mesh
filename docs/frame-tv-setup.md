# Frame TV Art Display — Pi Zero 2 W Setup

Hands-on provisioning guide for the node described in
`plans/frame-tv-art-display.md`. Read that first for the *why*; this is the
*how*, in the same style as `docs/pi1-lighting-setup.md`.

---

## 0. Before touching any hardware

The recess and the new double socket are electrical work on a wall with
tanking/a shower on the other side — get this designed and signed off by a
qualified (Part P registered) electrician before any cutting starts. See
`plans/frame-tv-art-display.md` §2 for why this isn't a DIY job here
specifically. Nothing below can usefully happen until the socket exists.

---

## 1. Flash the Pi

```bash
# On any machine with an SD card reader — Raspberry Pi Imager is the easy path
# OS: Raspberry Pi OS Lite (64-bit) — no desktop needed
```

In the Imager's advanced options (gear icon / Ctrl+Shift+X) before writing:
- Set hostname (e.g. `frametv`)
- Enable SSH, set a password or your public key
- Set Wi-Fi **only if you don't have the Ethernet adapter yet** — the plan
  calls for wired Ethernet via the USB-OTG port for reliability; Wi-Fi is a
  fallback for first boot, not the intended final setup.

## 2. First boot

```bash
ssh <user>@frametv.local   # or its DHCP-assigned IP
sudo raspi-config          # confirm locale/timezone; expand filesystem if needed
sudo apt update && sudo apt upgrade -y
```

Confirm the USB-Ethernet adapter is recognised and has a link:

```bash
ip link show   # should show the adapter, not just eth0-via-onboard (Zero has no onboard Ethernet)
```

Set a DHCP reservation for this Pi on the router, same reasoning as pi1's
SLZB-06 reservation (`project_zigbee_bridge_stale_ip` memory) — anything
with a hardcoded downstream dependency on an IP should get one; this node
being the coordinator's target for `art` commands is exactly that case.

## 3. Fullscreen art viewer

```bash
sudo apt install -y feh
```

Minimal kiosk test — confirm HDMI output before wiring up ai-mesh at all:

```bash
DISPLAY=:0 feh --fullscreen --hide-pointer /path/to/test-image.jpg
```

If nothing shows: check the mini-HDMI cable/adapter (not a full-size or
micro-HDMI connector — the Pi Zero 2 W is specifically mini-HDMI), and that
`/boot/firmware/config.txt` isn't forcing a resolution the TV doesn't like
(`hdmi_force_hotplug=1` plus leaving `hdmi_mode`/`hdmi_group` on auto is
usually right — only pin an explicit mode if auto-detect picks the wrong one).

`feh`'s remote-control mechanism (for the agent to drive "next image"
without restarting the process) is a signal-based reload — verify this
works manually before wiring up `capability-art`:

```bash
feh --fullscreen --hide-pointer /path/to/dir/ &
# swap the displayed file, then:
kill -USR1 $(pgrep feh)   # reloads from the same file list
```

## 4. Install the ai-mesh agent

Same cross-compiled aarch64 binary path as pi1 — no new build target needed.

```bash
just deploy-node frametv   # from the coordinator/OmniLink1 side, once this
                           # node's nodes/frametv.env exists (see below)
```

`nodes/frametv.env`:
```
NODE_HOST=<pi's reserved IP>
NODE_USER=<your ssh user>
NODE_OS=linux
NODE_ROLE=compute
NODE_FEATURES=art
```

No `MQTT_HOST`/`ZIGBEE_HOST` needed — this node doesn't touch Zigbee at all.

Confirm on the Nodes tab: the node appears with `art` in its feature list
and a green heartbeat, same as any other node.

## 5. TV: enable the local WebSocket control channel

- TV must be on the same network/VLAN as the Pi and coordinator.
- First connection to `wss://<tv-ip>:8002/api/v2/channels/samsung.remote.control`
  triggers an on-screen pairing prompt on the TV — accept it once. The
  response includes an auth token; persist it (coordinator-side config,
  same treatment as the gateway API key — masked, never re-displayed).
- If the prompt never appears: confirm the TV's network settings allow
  local "IP control" / aren't blocking the connection (Samsung sometimes
  calls this "Mobile connection" or similar in network settings — exact
  wording varies by firmware; check the TV's network menu if the WS
  connection is refused outright rather than prompting).

## 6. Switch the TV's input to the Pi and verify end to end

1. Manually switch the TV to the Pi's HDMI input (first time — before the
   WebSocket control is wired up).
2. Confirm `feh`'s test image displays correctly at native 1080p (no
   overscan/underscan cropping — adjust the TV's picture size setting if the
   image edges are cut off).
3. Once `capability-art` is deployed: `POST /api/art/show` with a real
   catalogue image from the coordinator and confirm it appears on the TV
   within a couple of seconds.

## 7. Ongoing checks

- Watch this node's temperature on the Health tab for the first few weeks,
  particularly through a full heating-on/off cycle if the recess is on an
  exterior or otherwise temperature-variable wall — see
  `plans/frame-tv-art-display.md` §2 for why this is worth a look given the
  sealed cavity.
- If movies are ever streamed to this node: stick to H.264 sources (or
  transcode H.265 to H.264 server-side first) — see the plan's §4 for why
  H.265 software decode isn't realistic on this hardware.

---

## Troubleshooting

**No HDMI output at all**
Wrong connector (needs mini-HDMI on the Pi end) is the most common cause,
followed by a forced HDMI mode in `config.txt` that the TV doesn't support —
try removing any pinned `hdmi_mode`/`hdmi_group` overrides and let auto-detect run.

**Pi randomly reboots / USB-Ethernet adapter drops**
Under-voltage — swap in a genuinely rated 5V/2.5A+ supply and a short good-quality
cable before suspecting anything else; the Zero is unforgiving about this,
especially once a USB Ethernet adapter is also drawing from the same rail.

**TV WebSocket connection refused (not just no pairing prompt)**
Check the TV's network settings for anything gating local/IP control access
— firmware-dependent wording, but usually somewhere in the network or
external-device settings menu.
