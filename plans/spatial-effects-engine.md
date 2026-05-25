# Phase S: Spatial Effects & Solar Engine

This phase introduces spatial awareness to the ai-mesh lighting system. Instead of controlling lights individually or by room, we treat the entire home as a 3D coordinate space. This enables "vector effects"—lighting changes that sweep across the room based on physical direction.

## The Vision: "The Sun in Your Room"
The primary goal is a cohesive **Sunlight Engine**. By knowing the physical location of every bulb and the current position of the sun, the system can:
1.  **Morning Sweep:** Start with a warm red glow at the "East" side of the room, slowly sweeping across to the "West" side as the sun rises.
2.  **Solar Tracking:** Shift color temperature and brightness based on the sun's elevation, but spatially weighted. Bulbs near windows (high X/Y) react more strongly to the exterior light state.
3.  **Spatial Telemetry:** Visualize cluster load as physical "ripples" starting from the node's physical location and propagating through the light grid.

---

## 1. Registry: Spatial Metadata
Currently, bulbs have names and rooms, but no location. We need to store `(x, y, z)` coordinates.

### Registry Changes (`coordinator/src/registry.rs`)
- **New Table:** `light_positions`
  ```sql
  CREATE TABLE IF NOT EXISTS light_positions (
      device_id TEXT PRIMARY KEY,
      x         REAL NOT NULL DEFAULT 0.0,
      y         REAL NOT NULL DEFAULT 0.0,
      z         REAL NOT NULL DEFAULT 0.0
  );
  ```
- **CRUD Methods:**
  - `set_light_position(device_id, x, y, z)`
  - `get_light_positions() -> HashMap<String, (f32, f32, f32)>`

---

## 2. Solar Engine
A background task in the coordinator that computes the sun's position.

### Dependencies
- Add `spa` (Solar Position Algorithm) or `sunrise` crate to `coordinator/Cargo.toml`. `spa` is preferred for exact Azimuth/Elevation.

### Configuration
- `MESH_LATITUDE`: e.g., `51.5074`
- `MESH_LONGITUDE`: e.g., `-0.1278`
- `MESH_ELEVATION`: (optional) meters above sea level.

### Implementation (`coordinator/src/solar.rs`)
- `SolarTask`: A loop running every 60 seconds.
- Calculates `Azimuth` (0–360°) and `Elevation` (-90 to +90°).
- Computes a `SolarVector` in 3D space.

---

## 3. Spatial Effects Logic
The "Sweep" is a function of a bulb's position and a global vector.

### The Math
For a light at position `P` and a solar vector `V`:
- `Intensity = dot_product(P_normalized, V_normalized)`
- `ColorTemp = interpolate(Warm, Cool, Elevation)`
- The "Sweep" effect is achieved by shifting the "center" of the gradient along the vector.

---

## 4. Dashboard: Floorplan View
Setting coordinates via CLI is hard. The dashboard needs a way to map bulbs.

### Implementation (`coordinator/src/http/static/spatial.js`)
- **Interactive Map:** A simple 2D grid where bulbs can be dragged.
- **Save Layout:** Sends a batch of `PATCH /api/lights/{id}/position` requests.
- **Sun Preview:** An icon showing the current azimuth of the sun relative to the room.

---

## Roadmap: Phase S Delivery

### S1: Spatial Registry (In Progress)
- [ ] Add `light_positions` table to `registry.rs`.
- [ ] Implement `get_all_positions` and `update_position` in the Registry.
- [ ] Add `POST /api/lights/{id}/position` to `api.rs`.

### S2: Solar Engine
- [ ] Add `spa` crate.
- [ ] Implement `solar.rs` background task.
- [ ] Log azimuth/elevation every minute.
- [ ] Broadcast `DashboardEvent::SolarUpdate` to WS clients.

### S3: Sunlight Sweep
- [ ] Implement the "Sunlight" effect task.
- [ ] Per-device logic: `calculate_target_state(pos, solar_info)`.
- [ ] Fan out `LightCommand` messages to agents.

### S4: Dashboard Floorplan
- [ ] Create `spatial.js`.
- [ ] Add "Map" tab to the dashboard.
- [ ] Implement drag-and-drop bulb positioning.
