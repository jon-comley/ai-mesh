#!/usr/bin/env python3
"""Push named climate/motion sensor readings from the local coordinator to glebefish.com.

Reads MESH_AUTH_TOKEN (for /api/sensors, /api/lights/names) and
TELEMETRY_PUSH_KEY (for the glebefish push) from the environment — the
systemd unit loads these from /var/lib/ai-mesh/coordinator.state and
/var/lib/ai-mesh/glebefish-push.env respectively.

Only devices whose registry name (from /api/lights/names) starts with one of
PUBLISHED_PREFIXES are published; everything else (bulbs, switches, contact
sensors) stays private to the mesh.
"""

import json
import os
import sys
import urllib.error
import urllib.request

COORDINATOR_URL = "http://127.0.0.1:9001"
PUSH_URL = "https://glebefish.com/api/sensor"
PUBLISHED_PREFIXES = ("Sonoff Temp/Humidity Sensor", "Sonoff Motion Sensor")

# Cloudflare's bot protection on glebefish.com blocks the default
# "Python-urllib/x.y" User-Agent outright (HTTP 403, Cloudflare error 1010).
# Any identifying, non-default UA passes — this isn't evasion, just avoiding
# a specific blocklisted signature.
USER_AGENT = "ai-mesh-sensor-push/1.0 (+https://glebefish.com)"


def fetch_json(url: str) -> object:
    with urllib.request.urlopen(url, timeout=5) as resp:
        return json.loads(resp.read())


def build_readings(sensors: list, names: dict) -> list:
    readings = []
    for s in sensors:
        name = names.get(s.get("device_id", ""))
        if not name or not name.startswith(PUBLISHED_PREFIXES):
            continue
        reading = {"name": name, "online": bool(s.get("online", False))}
        if "temperature" in s:
            reading["temperature_c"] = s["temperature"]
        if "humidity" in s:
            reading["humidity_pct"] = s["humidity"]
        if "illuminance" in s:
            reading["illuminance_lux"] = s["illuminance"]
        if "occupancy" in s:
            reading["occupancy"] = s["occupancy"]
        readings.append(reading)
    return readings


def main() -> int:
    mesh_token = os.environ.get("MESH_AUTH_TOKEN", "")
    push_key = os.environ.get("TELEMETRY_PUSH_KEY")
    if not push_key:
        print("TELEMETRY_PUSH_KEY not set", file=sys.stderr)
        return 1

    try:
        sensors = fetch_json(f"{COORDINATOR_URL}/api/sensors?token={mesh_token}")
        names = fetch_json(f"{COORDINATOR_URL}/api/lights/names?token={mesh_token}")
    except (urllib.error.URLError, TimeoutError) as exc:
        print(f"failed to read from coordinator: {exc}", file=sys.stderr)
        return 1

    readings = build_readings(sensors, names)
    if not readings:
        print("no matching sensors found, nothing to push", file=sys.stderr)
        return 1

    body = json.dumps({"readings": readings}).encode()
    req = urllib.request.Request(
        PUSH_URL,
        data=body,
        method="POST",
        headers={
            "Content-Type": "application/json",
            "X-Telemetry-Key": push_key,
            "User-Agent": USER_AGENT,
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            print(f"pushed {len(readings)} readings: HTTP {resp.status}")
    except urllib.error.HTTPError as exc:
        print(f"push rejected: {exc.code} {exc.read().decode(errors='replace')}", file=sys.stderr)
        return 1
    except urllib.error.URLError as exc:
        print(f"push failed: {exc}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
