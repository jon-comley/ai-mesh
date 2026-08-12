# pi1 Lighting Infrastructure Setup

One-time manual setup on pi1 (pi1.local) to support the `lighting` and
`sensors` capabilities (both ride the same Mosquitto + Z2M bridge; see §9 for
the sensors specifics). The agent binary handles MQTT automatically once
Mosquitto and Z2M are running.

---

## Hardware

- **Raspberry Pi 5** — 8 GB RAM, runs Mosquitto + Z2M + ai-mesh agent
- **SLZB-06** Zigbee coordinator — PoE device; power via USB-C, network via ethernet
  - IP: `<slzb-06>`, port `6638` — **set a DHCP reservation** for this on the
    the mesh router. Z2M's `serial.port` hard-codes this address; if the lease changes,
    Z2M fails with `EHOSTUNREACH` and crash-loops, and all lights go dead with no
    error on the dashboard (this is exactly what the 2026-06-25 the mesh router migration broke
    — the SLZB-06 moved off `<slzb-06-old>` but the Z2M config still pointed at it).
  - Firmware: EmberZNet 8.0.2 / EZSP v14 (Z2M adapter type: `ember`)

---

## 1. Install Mosquitto

```bash
ssh jonno@pi1.local
sudo apt update
sudo apt install -y mosquitto mosquitto-clients
sudo systemctl enable --now mosquitto
```

**Important — Mosquitto 2.x only listens on localhost by default.** Allow remote
connections by creating `/etc/mosquitto/conf.d/remote.conf`:

```bash
sudo nano /etc/mosquitto/conf.d/remote.conf
```

```
listener 1883 0.0.0.0
allow_anonymous true
```

```bash
sudo systemctl restart mosquitto
```

Verify from OmniLink1:

```bash
mosquitto_pub -h pi1.local -t test -m hello
mosquitto_sub -h pi1.local -t test -C 1   # should print "hello"
```

---

## 2. Install Zigbee2MQTT

### 2a. Node.js

```bash
curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
sudo apt install -y nodejs
node --version   # v20.x
```

### 2b. Clone and install (use pnpm — Z2M has no package-lock.json)

```bash
sudo mkdir -p /opt/zigbee2mqtt
sudo chown jonno:jonno /opt/zigbee2mqtt
git clone --depth 1 https://github.com/Koenkk/zigbee2mqtt.git /opt/zigbee2mqtt
cd /opt/zigbee2mqtt
sudo npm install -g pnpm
pnpm install
```

### 2c. Configure

```bash
cp /opt/zigbee2mqtt/data/configuration.example.yaml /opt/zigbee2mqtt/data/configuration.yaml
nano /opt/zigbee2mqtt/data/configuration.yaml
```

```yaml
mqtt:
  server: mqtt://127.0.0.1
  retain: true            # broker holds last per-device state so agents get it on subscribe

serial:
  port: tcp://<slzb-06>:6638
  adapter: ember          # required for EZSP v13+; was 'ezsp' on older firmware

permit_join: false        # set true temporarily when pairing

availability:
  enabled: true           # REQUIRED: without this, z2m never marks unreachable
                          # bulbs offline, so a powered-off light keeps showing its
                          # last "on" state in the dashboard forever. With it on, z2m
                          # actively pings mains devices (active.timeout, default 10
                          # min) and publishes <device>/availability offline, which
                          # the lighting capability turns into an offline card.
```

### 2d. Verify it starts

```bash
cd /opt/zigbee2mqtt && npm start
```

Look for:
```
Zigbee2MQTT:info  Zigbee: coordinator ready
Zigbee2MQTT:info  MQTT: connected to server
```

Ctrl+C to stop, then install as a service.

---

## 3. Z2M as a systemd service

```bash
sudo nano /etc/systemd/system/zigbee2mqtt.service
```

```ini
[Unit]
Description=Zigbee2MQTT
After=network.target mosquitto.service

[Service]
Type=simple
User=jonno
WorkingDirectory=/opt/zigbee2mqtt
ExecStart=/usr/bin/npm start
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable --now zigbee2mqtt
sudo systemctl status zigbee2mqtt
```

---

## 4. SLZB-06 Firmware

If Z2M reports EZSP version < 13, update the EFR32 radio firmware via the SLZB-06 web UI at `http://<slzb-06>`:

1. Open the web UI → Firmware tab
2. Select the latest EmberZNet release (v3.x.x) and flash
3. Restart Z2M — it should now report EZSP v14

---

## 5. Pairing devices

**Primary path — the dashboard:** the Lighting tab's **Pair device** button
opens the 254-second bridge-wide window (`POST /api/zigbee/permit-join`) and
streams join/interview events live into the panel. Works from the phone.

**CLI fallback** from OmniLink1:

```bash
just pair-bulb
```

This opens a 254-second pairing window and streams join events. Power-cycle the
bulb to trigger pairing. When it joins, Z2M will interview it and log the IEEE address.

**Removing a device** from the dashboard's delete button unpairs it from the
Zigbee network (`bridge/request/device/remove`) as well as clearing the
coordinator's records — deleting while pi1 is offline only clears local
records, and the device reappears when the node returns.

Removal sends the device a graceful network Leave (`force: false`), so the
device wipes its pairing state and can be re-paired later. The device must be
powered and reachable to acknowledge the Leave — if it's dead or physically
gone, use `DELETE /api/lights/{id}?force=true`, which only drops z2m's record.
Never force-remove a live device: it keeps its network keys, stays silently
joined, and can never re-pair on its own — recovery then means z2m
`database.db` surgery or a point-blank Touchlink reset (see
`docs/roadmap.md`, orphaned-bulbs incident, for the full recovery playbook).

**Rename the device** after pairing:

```bash
mosquitto_pub -h pi1.local \
  -t 'zigbee2mqtt/bridge/request/device/rename' \
  -m '{"from":"0xXXXXXXXXXXXXXXXX","to":"my_bulb"}'
```

Confirm:

```bash
mosquitto_sub -h pi1.local -t 'zigbee2mqtt/bridge/devices' -C 1 \
  | python3 -m json.tool | grep friendly_name
```

---

## 6. Z2M groups

Groups let you control multiple bulbs with one command. Create an `all` group
and add devices to it:

```bash
mosquitto_pub -h pi1.local \
  -t 'zigbee2mqtt/bridge/request/group/add' \
  -m '{"friendly_name":"all"}'

mosquitto_pub -h pi1.local \
  -t 'zigbee2mqtt/bridge/request/group/members/add' \
  -m '{"group":"all","device":"my_bulb"}'
```

Add every new bulb to the group so `just intent "turn all lights off"` works.

---

## 7. Agent configuration

`nodes/pi1.env` sets the MQTT connection for the agent:

```
MQTT_HOST=127.0.0.1
MQTT_PORT=1883
```

These are injected into the systemd service by `just deploy-node pi1`. The agent
connects on startup and subscribes to Z2M topics automatically.

---

## 8. Intent commands (from OmniLink1)

```bash
just intent "turn all lights off"
just intent "turn my_bulb on"
just intent "set my_bulb to 50% brightness"
just intent "make it warm like candlelight"
just intent "bright white light for working"
```

The LLM maps natural language to a `light_command` tool call. Device/group names
must match what Z2M knows (use `zigbee2mqtt/bridge/devices` to list them).

---

## 9. Sensors

No extra infrastructure — sensors join the same Z2M bridge. The agent needs the
`sensors` feature (`NODE_FEATURES=llm,lighting,sensors` in `nodes/pi1.env`,
baked in by `just deploy-node pi1`).

**Pairing** is identical to bulbs (§5 — the dashboard's Pair device button or
`just pair-bulb`; the window is bridge-wide because Zigbee pairing is not
device-type specific). Battery devices usually need a button held to start
joining; check the device manual. Rename after pairing exactly as in §5.

Everything downstream is automatic:

- The agent classifies each device from its Z2M `exposes` metadata — anything
  reporting temperature/humidity/occupancy/contact/illuminance without light
  controls lands as `DeviceType::Sensor`.
- Sensor publishes are parsed (temperature, humidity, battery, occupancy,
  contact, illuminance) and forwarded to the coordinator as `SensorState`;
  readings are merged field-wise, persisted across coordinator restarts, and
  served at `GET /api/sensors` plus pushed to the dashboard as `SensorUpdate`
  WS events. Readout cards render on the Lighting panel.
- Sensors are never state-polled (`/get` returns z2m errors for them) — they
  push on their own schedule.
- **Model note — SONOFF SNZB-02P / SNZB-03P R2 (verified 2026-07-04):**
  SNZB-02P is temperature + humidity + battery (no voltage). SNZB-03P R2
  is occupancy + a *numeric lux* `illuminance` + battery (no voltage either).
  The **base** SNZB-03P (non-R2) instead exposes a `dim`/`bright` enum on a
  property named `illumination` — different key, different shape — which
  this parser does not read; only the R2's numeric lux is captured.

**Availability caveat:** battery sensors are *passive* devices — z2m only
marks them offline after `availability.passive.timeout` (default **25 h**,
vs 10 min for mains-powered lights). A sensor with a dead battery can read
"online" with stale values for up to a day; the `battery` field is the
earlier warning signal.

---

## Troubleshooting

**Z2M: `Cannot permit join for more than 254 seconds`**
Use `"time": 254` not `300` in the permit_join request.

**Z2M: adapter version mismatch / EZSP < 13**
Flash the SLZB-06 radio firmware to EmberZNet ≥ 8.x via the web UI.

**Z2M uses pnpm, not npm**
`npm ci` fails because there is no `package-lock.json`. Use `pnpm install`.

**Mosquitto: connection refused from OmniLink1**
Check `/etc/mosquitto/conf.d/remote.conf` exists with `listener 1883 0.0.0.0` and `allow_anonymous true`.

**Agent: MQTT connect/disconnect storm in logs**
This was caused by rumqttc reconnecting with zero delay. Fixed in `capability-zigbee`
(5s reconnect delay + node-specific client ID). Redeploy if seen on an old binary.

**SLZB-06 not found on network**
The SLZB-06 needs power — use USB-C for power alongside ethernet (it is NOT PoE-powered from the ethernet port alone on all switches).

**Beelink SER8 unrecoverable BIOS screen**
Caused by Windows sleep/hibernate. Run `powercfg /h off` and set all sleep timeouts
to 0. The install script does this automatically on first provision.
