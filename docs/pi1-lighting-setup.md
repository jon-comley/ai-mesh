# pi1 Lighting Infrastructure Setup

One-time manual setup on pi1 to support the `lighting` capability. Run these steps before deploying the Phase C agent build.

---

## Prerequisites

- pi1 is reachable over SSH from OmniLink1
- SLZB-06 Zigbee coordinator is on the LAN and its IP is known (e.g. `192.168.1.x`)
- At least one Zigbee bulb to pair

---

## 1. Install Mosquitto

```bash
ssh jonno@192.168.1.11
sudo apt update
sudo apt install -y mosquitto mosquitto-clients
sudo systemctl enable --now mosquitto
```

Verify it's running:

```bash
systemctl status mosquitto
```

Quick smoke test (two terminals on pi1):

```bash
# terminal 1
mosquitto_sub -t 'test/#' -v

# terminal 2
mosquitto_pub -t 'test/hello' -m 'world'
```

---

## 2. Install Zigbee2MQTT

### 2a. Install Node.js (if not already)

```bash
curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
sudo apt install -y nodejs
node --version   # should print v20.x
```

### 2b. Install Zigbee2MQTT

```bash
sudo mkdir -p /opt/zigbee2mqtt
sudo chown jonno:jonno /opt/zigbee2mqtt
git clone --depth 1 https://github.com/Koenkk/zigbee2mqtt.git /opt/zigbee2mqtt
cd /opt/zigbee2mqtt
npm ci
```

### 2c. Configure

```bash
cp /opt/zigbee2mqtt/data/configuration.example.yaml /opt/zigbee2mqtt/data/configuration.yaml
nano /opt/zigbee2mqtt/data/configuration.yaml
```

Minimum required fields:

```yaml
mqtt:
  server: mqtt://127.0.0.1

serial:
  port: tcp://192.168.1.x:6638   # replace x with the SLZB-06 IP

permit_join: true   # enable during initial pairing; set false after
```

### 2d. Run Z2M manually first (verify it connects)

```bash
cd /opt/zigbee2mqtt
npm start
```

Look for:
```
Zigbee2MQTT:info  Zigbee: coordinator ready
Zigbee2MQTT:info  MQTT: connected to server
```

If you see those two lines, the SLZB-06 and Mosquitto connections are working. Ctrl+C to stop.

---

## 3. Pair a Bulb

With `permit_join: true` in the config and Z2M running:

1. Power-cycle the bulb (off → on, or use the switch)
2. Watch Z2M output for:
   ```
   Zigbee2MQTT:info  Successfully interviewed '0xXXXX', device has successfully been paired
   ```
3. Note the `friendly_name` assigned (default is the IEEE address; rename in config if desired)

Once paired, set `permit_join: false` in `configuration.yaml` to prevent accidental joins.

---

## 4. Install Z2M as a systemd service

```bash
sudo nano /etc/systemd/system/zigbee2mqtt.service
```

Paste:

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

## 5. Verify end-to-end (from OmniLink1)

Subscribe to all Z2M topics from OmniLink1 to confirm state messages are flowing:

```bash
mosquitto_sub -h 192.168.1.11 -t 'zigbee2mqtt/#' -v
```

You should see periodic state messages for paired devices. Power-cycle a bulb and watch the `state` topic update.

Check the device list (retained message — delivered immediately on subscribe):

```bash
mosquitto_sub -h 192.168.1.11 -t 'zigbee2mqtt/bridge/devices' -C 1 | python3 -m json.tool
```

---

## 6. Notes for the agent

- `MQTT_HOST` and `MQTT_PORT` in `nodes/pi1.env` default to `127.0.0.1:1883` — no changes needed
- The ai-mesh agent subscribes to `zigbee2mqtt/+/state`, `zigbee2mqtt/+/availability`, and `zigbee2mqtt/bridge/devices`
- Device `friendly_name` values in Z2M `configuration.yaml` are what you use in `mesh intent` commands (e.g. `"turn the kitchen_bulb on"`)

---

## Troubleshooting

**Z2M can't connect to SLZB-06**
- Confirm the IP in `serial.port` is correct: `ping 192.168.1.x`
- Check SLZB-06 web UI to confirm it's in Zigbee coordinator mode (not router mode)

**Mosquitto refusing connections from OmniLink1**
- By default Mosquitto 2.x only listens on localhost. Add to `/etc/mosquitto/mosquitto.conf`:
  ```
  listener 1883 0.0.0.0
  allow_anonymous true
  ```
  Then `sudo systemctl restart mosquitto`.

**Z2M crashes on startup**
- Check logs: `journalctl -u zigbee2mqtt -n 50`
- Usually a YAML syntax error in `configuration.yaml` or the SLZB-06 IP being wrong
