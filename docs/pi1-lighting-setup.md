# pi1 Lighting Infrastructure Setup

One-time manual setup on pi1 (192.168.1.11) to support the `lighting` capability.
The agent binary handles MQTT automatically once Mosquitto and Z2M are running.

---

## Hardware

- **Raspberry Pi 5** — 8 GB RAM, runs Mosquitto + Z2M + ai-mesh agent
- **SLZB-06** Zigbee coordinator — PoE device; power via USB-C, network via ethernet
  - IP: `192.168.1.16`, port `6638`
  - Firmware: EmberZNet 8.0.2 / EZSP v14 (Z2M adapter type: `ember`)

---

## 1. Install Mosquitto

```bash
ssh jonno@192.168.1.11
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
mosquitto_pub -h 192.168.1.11 -t test -m hello
mosquitto_sub -h 192.168.1.11 -t test -C 1   # should print "hello"
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

serial:
  port: tcp://192.168.1.16:6638
  adapter: ember          # required for EZSP v13+; was 'ezsp' on older firmware

permit_join: false        # set true temporarily when pairing
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

If Z2M reports EZSP version < 13, update the EFR32 radio firmware via the SLZB-06 web UI at `http://192.168.1.16`:

1. Open the web UI → Firmware tab
2. Select the latest EmberZNet release (v3.x.x) and flash
3. Restart Z2M — it should now report EZSP v14

---

## 5. Pairing bulbs

From OmniLink1:

```bash
just pair-bulb
```

This opens a 254-second pairing window and streams join events. Power-cycle the
bulb to trigger pairing. When it joins, Z2M will interview it and log the IEEE address.

**Rename the device** after pairing:

```bash
mosquitto_pub -h 192.168.1.11 \
  -t 'zigbee2mqtt/bridge/request/device/rename' \
  -m '{"from":"0xXXXXXXXXXXXXXXXX","to":"my_bulb"}'
```

Confirm:

```bash
mosquitto_sub -h 192.168.1.11 -t 'zigbee2mqtt/bridge/devices' -C 1 \
  | python3 -m json.tool | grep friendly_name
```

---

## 6. Z2M groups

Groups let you control multiple bulbs with one command. Create an `all` group
and add devices to it:

```bash
mosquitto_pub -h 192.168.1.11 \
  -t 'zigbee2mqtt/bridge/request/group/add' \
  -m '{"friendly_name":"all"}'

mosquitto_pub -h 192.168.1.11 \
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
