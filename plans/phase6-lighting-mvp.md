# Phase 6: Lighting — MVP (Bulb On/Off)

## Context

The modular capability architecture is complete. `LightingCapability` is a stub — it receives `LightCommand` messages and logs them. This branch wires it to real hardware via Zigbee2MQTT over MQTT.

Scope: on/off/brightness/toggle only. The crate boundary must be right from the start so Hue, IKEA, and color/circadian stories slot in without restructuring.

**What's already in place (confirmed by codebase inspection):**
- All shared types: `LightTarget`, `LightAction`, `LightCommandRequest`, `LightStateReport` in `shared/src/messages.rs`
- `MeshMessage::LightState` match arm in `coordinator/src/server.rs:423` — already logs + acks
- `agent/Cargo.toml` already has `lighting = ["dep:capability-lighting"]` feature
- `nodes/pi1.env` already has `NODE_FEATURES=llm,lighting`
- `capabilities/lighting` already in workspace

---

## Architecture

```
coordinator
    │  LightCommand (MeshMessage wire)
    ▼
capabilities/lighting          ← thin Capability impl (replace stub)
    │  delegates via ZigbeeClient
    ▼
capabilities/zigbee (NEW)      ← MQTT + Z2M interface + device registry
    │  MQTT pub/sub
    ▼
Mosquitto (pi1:1883)
    │  Zigbee2MQTT
    │  Zigbee radio TCP
    ▼
SLZB-06 → bulbs
```

Future device-specific crates (`capabilities/hue`, `capabilities/ikea`) would sit between lighting and zigbee for device-specific JSON translation. For on/off/brightness, Z2M normalises already — those crates are not needed yet.

---

## Crate: `capabilities/zigbee` (new)

### `capabilities/zigbee/Cargo.toml`
```toml
[package]
name = "capability-zigbee"
version = "0.1.0"
edition = "2024"

[dependencies]
rumqttc    = { version = "0.24", features = ["use-rustls"] }
thiserror  = "1"
serde_json = "1"
tokio      = { version = "1", features = ["full"] }
tracing    = "0.1"
shared     = { path = "../../shared" }
```

### Module layout
```
capabilities/zigbee/src/
  lib.rs        — re-exports, public API
  client.rs     — ZigbeeClient, connect/reconnect, event broadcast
  discovery.rs  — parse zigbee2mqtt/bridge/devices → DeviceRegistry
  command.rs    — LightAction + LightTarget → Z2M JSON + topic
  error.rs      — ZigbeeError (thiserror)
```

### Public API (`lib.rs` re-exports)

```rust
pub struct ZigbeeClient { ... }  // Arc-sharable

impl ZigbeeClient {
    pub async fn connect(host: &str, port: u16) -> Result<Self, ZigbeeError>;
    pub async fn send_command(&self, target: &LightTarget, action: &LightAction)
        -> Result<(), ZigbeeError>;
    pub fn device_registry(&self) -> Arc<DeviceRegistry>;
    pub fn subscribe(&self) -> broadcast::Receiver<ZigbeeEvent>;  // each caller gets own receiver
}

pub enum ZigbeeEvent {
    StateChanged(LightStateReport),
    DeviceDiscovered(DeviceInfo),
    ConnectionLost,
    ConnectionRestored,
}
```

Using `broadcast::Sender<ZigbeeEvent>` internally (not `mpsc`) is critical: `start()` is called on every coordinator reconnect, so each connection needs its own `Receiver` without consuming a single shared one.

**`connect()` must spawn the EventLoop poll task itself.** rumqttc's `AsyncClient` is inert until its `EventLoop` is continuously polled — without this, publishes hang silently and no messages are received. Spawn inside `connect()` so the capability never owns the MQTT lifecycle:

```rust
pub async fn connect(host: &str, port: u16) -> Result<Self, ZigbeeError> {
    let mut mqttoptions = MqttOptions::new("ai-mesh-lighting", host, port);
    mqttoptions.set_keep_alive(Duration::from_secs(30));
    let (client, mut eventloop) = AsyncClient::new(mqttoptions, 64);

    // Must be spawned before any subscribe/publish calls or they will hang.
    tokio::spawn(async move {
        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Packet::Publish(p))) => { /* forward to broadcast */ }
                Err(e) => { warn!("MQTT event loop error: {e}"); break; }
                _ => {}
            }
        }
    });

    // subscribe, build ZigbeeClient, return Ok(Self)
}
```

**Subscribe to specific topics, not `zigbee2mqtt/#`.** The wildcard floods the event loop with link quality, OTA progress, and diagnostic noise. Subscribe only to what the capability actually needs:

```rust
client.subscribe("zigbee2mqtt/+/state",        QoS::AtMostOnce).await?;
client.subscribe("zigbee2mqtt/+/availability", QoS::AtMostOnce).await?;
client.subscribe("zigbee2mqtt/bridge/devices", QoS::AtMostOnce).await?;
```

### `command.rs` — `LightAction` → Z2M JSON

| LightAction | Topic suffix | JSON body |
|---|---|---|
| `On` | `.../set` | `{"state":"ON"}` |
| `Off` | `.../set` | `{"state":"OFF"}` |
| `Toggle` | `.../set` | `{"state":"TOGGLE"}` |
| `Brightness(n)` | `.../set` | `{"brightness":n}` |
| `ColorTemp(mireds)` | `.../set` | `{"color_temp":mireds}` |
| `ColorXY{x,y}` | `.../set` | `{"color":{"x":x,"y":y}}` |

Topic selection:
- `LightTarget::Group(id)` → `zigbee2mqtt/group_{id}/set`
- `LightTarget::Device(name)` → `zigbee2mqtt/{name}/set`

### `discovery.rs`

Subscribe to `zigbee2mqtt/bridge/devices` (retained — delivered immediately on subscribe). Parse the JSON array, extract `ieee_address`, `friendly_name`, and feature list. Emit `ZigbeeEvent::DeviceDiscovered` for each entry. Skip any entry that lacks `ieee_address` without error — the Z2M coordinator itself appears in this array and has no `ieee_address`. All other parse failures → `warn!()` + skip.

### Error handling

`ZigbeeError` is `thiserror`-derived:
```rust
#[derive(Debug, Error)]
pub enum ZigbeeError {
    #[error("MQTT connect error: {0}")] MqttConnect(#[from] rumqttc::ConnectionError),
    #[error("MQTT publish error: {0}")] Publish(String),
    #[error("JSON error: {0}")] Json(#[from] serde_json::Error),
}
```

`rumqttc` handles MQTT-level reconnection automatically. Parse failures on inbound messages → `warn!()`, skip, never crash.

---

## Update: `capabilities/lighting`

### `capabilities/lighting/Cargo.toml`
Add:
```toml
capability-zigbee = { path = "../zigbee" }
tokio             = { version = "1", features = ["sync"] }
```

### `capabilities/lighting/src/lib.rs`

Replace stub with:

```rust
pub struct LightingCapability {
    zigbee: tokio::sync::OnceCell<Arc<ZigbeeClient>>,  // NOT std::sync::OnceLock
    node_id: String,
}
```

`std::sync::OnceLock` has no async initialiser and will not compile with `.await`. Use `tokio::sync::OnceCell::get_or_try_init(|| async { ... }).await` instead. rumqttc handles MQTT-level reconnects internally, so the cell is initialised once and never needs replacing.

**`start(tx)`**:
1. Initialise `zigbee` if not already set:
   ```rust
   let client = self.zigbee.get_or_try_init(|| async {
       ZigbeeClient::connect(&host, port).await.map(Arc::new)
   }).await.map_err(|e| e.to_string())?;
   ```
   - `host` / `port` from `MQTT_HOST` / `MQTT_PORT` env vars, defaulting to `127.0.0.1` / `1883`
   - On connect error: `get_or_try_init` returns `Err`, log `warn!`, return `Err(...)` — agent logs it, capability stays offline
2. `let rx = client.subscribe()` — fresh receiver for this connection's lifetime
3. `tokio::select!` loop:
   - `rx.recv()` → on `ZigbeeEvent::StateChanged(report)`, send `MeshMessage::LightState(report)` via `tx`
   - `tx.closed()` → break (coordinator disconnected; next reconnect calls `start()` again)

**`handle(LightCommand, tx)`**:
1. If `zigbee` not initialised: log `warn!`, return
2. Parse `target` + `action` from `LightCommandRequest`
3. `zigbee.send_command(&req.target, &req.command).await`
4. On error: `warn!()`, no panic

Existing `SceneLoad` stub behaviour (return `SceneLoaded` with `success: false`) is preserved unchanged.

---

## Workspace change

`Cargo.toml` (root):
```toml
[workspace]
members = [
    "shared",
    "capabilities/core",
    "capabilities/llm",
    "capabilities/lighting",
    "capabilities/zigbee",   # add this
    "agent",
    "coordinator",
    "cli",
]
```

---

## Infrastructure: `docs/pi1-lighting-setup.md` (new)

Steps to run manually on pi1:

1. **Mosquitto**: `sudo apt install -y mosquitto mosquitto-clients && sudo systemctl enable --now mosquitto`
2. **Zigbee2MQTT**: install via npm; `configuration.yaml` key fields:
   - `serial.port: tcp://192.168.1.x:6638` (SLZB-06)
   - `mqtt.server: mqtt://127.0.0.1`
   - `permit_join: true` during pairing
3. **Pairing**: restart Z2M, power-cycle bulb
4. **Verify**: `mosquitto_sub -t 'zigbee2mqtt/#' -v`

No changes to `nodes/pi1.env` — MQTT is localhost on pi1 and the defaults (`127.0.0.1:1883`) cover it. Override via systemd environment if ever needed.

---

## Files to create/modify

| File | Action |
|------|--------|
| `Cargo.toml` | Add `capabilities/zigbee` to workspace members |
| `capabilities/zigbee/Cargo.toml` | Create |
| `capabilities/zigbee/src/lib.rs` | Create — re-exports + `ZigbeeClient` |
| `capabilities/zigbee/src/client.rs` | Create — MQTT connect, broadcast loop |
| `capabilities/zigbee/src/discovery.rs` | Create — bridge/devices parser |
| `capabilities/zigbee/src/command.rs` | Create — `LightAction`/`LightTarget` → Z2M JSON/topic |
| `capabilities/zigbee/src/error.rs` | Create — `ZigbeeError` |
| `capabilities/lighting/Cargo.toml` | Add `capability-zigbee` dep |
| `capabilities/lighting/src/lib.rs` | Replace stub with real delegation |
| `docs/pi1-lighting-setup.md` | Create — pi1 infra setup guide |

**Not changed** (already correct):
- `coordinator/src/server.rs` — `LightState` arm already present
- `agent/Cargo.toml` — `lighting` feature already wired
- `nodes/pi1.env` — `NODE_FEATURES=llm,lighting` already set

---

## Phased delivery

**Phase A — Infrastructure** (manual, on pi1, no Rust code)
- Install Mosquitto + Z2M, pair ≥1 bulb, verify with `mosquitto_sub`
- Write `docs/pi1-lighting-setup.md`

**Phase B — `capabilities/zigbee` crate**
- MQTT connect (async `rumqttc`); EventLoop poll task spawned inside `connect()`
- Subscribe to `zigbee2mqtt/+/state`, `zigbee2mqtt/+/availability`, `zigbee2mqtt/bridge/devices`
- `zigbee2mqtt/bridge/devices` → `DeviceRegistry` (skip entries without `ieee_address`)
- Inbound state messages → `broadcast::Sender<ZigbeeEvent>`
- `send_command()` for On/Off/Brightness/Toggle
- Unit tests: mock MQTT broker, verify command → JSON mapping

**Phase C — Wire `capabilities/lighting`**
- `tokio::sync::OnceCell<Arc<ZigbeeClient>>` struct field
- `start()` initialises MQTT once via `get_or_try_init`, subscribes, forwards `StateChanged`
- `handle()` delegates to `send_command()`
- `just deploy-node pi1`

**Phase D — End-to-end**
- `mesh intent "turn the light on"` → bulb ON
- `mesh intent "turn the light off"` → bulb OFF
- Confirm via `mosquitto_sub -h <pi1-old> -t 'zigbee2mqtt/#' -v`

---

## Verification

```bash
# Unit tests (no hardware needed)
cargo test -p capability-zigbee
cargo test -p capability-lighting

# Deploy
just deploy-node pi1

# Live trace (from controller)
mosquitto_sub -h <pi1-old> -t 'zigbee2mqtt/#' -v

# End-to-end
mesh intent "turn the kitchen light on"   # bulb turns on + Z2M topic shows state change
mesh intent "turn the kitchen light off"  # bulb turns off

# Regression
cargo test
just validate-routing    # LLM routing still passes
```
