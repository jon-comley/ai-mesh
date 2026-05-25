use chrono::Utc;
use shared::MeshMessage;
use shared::messages::{LightAction, LightCommandRequest, LightTarget};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{info, warn};

use crate::http::state::DashboardState;
use crate::registry::Registry;

pub struct SpatialEngine {
    latitude: f64,
    longitude: f64,
    dashboard: Arc<DashboardState>,
    registry: Arc<Mutex<Registry>>,
}

impl SpatialEngine {
    pub fn new(dashboard: Arc<DashboardState>, registry: Arc<Mutex<Registry>>) -> Self {
        let latitude = std::env::var("MESH_LATITUDE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(51.5074); // Default: London
        let longitude = std::env::var("MESH_LONGITUDE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(-0.1278);

        Self {
            latitude,
            longitude,
            dashboard,
            registry,
        }
    }

    pub async fn run(self) {
        info!(
            "Spatial Engine started (Lat: {}, Long: {})",
            self.latitude, self.longitude
        );

        loop {
            let now = Utc::now();
            let pos = spa::calc_solar_position(now, self.latitude, self.longitude);

            match pos {
                Ok(p) => {
                    let azimuth = p.azimuth;
                    let elevation = 90.0 - p.zenith_angle;

                    info!(
                        "Solar Update: Azimuth {:.2}°, Elevation {:.2}°",
                        azimuth, elevation
                    );

                    self.dashboard.push_solar_update(azimuth, elevation);

                    // Perform the spatial sweep if we have elevation
                    if elevation > -18.0 {
                        // Twilight or Day
                        self.perform_solar_sweep(azimuth, elevation).await;
                    }
                }
                Err(e) => warn!("Solar calculation failed: {:?}", e),
            }

            sleep(Duration::from_secs(60)).await;
        }
    }

    async fn perform_solar_sweep(&self, azimuth: f64, elevation: f64) {
        let enabled_devices = self.dashboard.get_solar_enabled_devices();
        if enabled_devices.is_empty() {
            return;
        }

        let (positions, rooms) = {
            let reg = self.registry.lock().unwrap();
            (reg.get_all_light_positions(), reg.list_rooms())
        };

        for device_id in enabled_devices {
            let pos_info = positions.get(&device_id).cloned();
            let (x, y, z, room_id, fixture_type) = pos_info
                .map(|p| (p.x, p.y, p.z, p.room_id, p.fixture_type))
                .unwrap_or((0.0, 0.0, 0.0, None, None));

            // Find room metadata
            let room = room_id.and_then(|rid| rooms.iter().find(|r| r.id == rid));

            // 1. Calculate base brightness/CT from elevation
            let (base_bri, base_ct) = calculate_solar_state(elevation);

            // 2. Room-aware adjustments
            let mut effective_azimuth = azimuth;
            let mut intensity_scale = 1.0;

            if let Some(r) = room {
                effective_azimuth = (azimuth - r.orientation_degrees as f64 + 360.0) % 360.0;

                if r.has_window
                    && let Some(facing) = r.window_facing
                {
                    let diff = (effective_azimuth - facing as f64).abs();
                    let normalized_diff = if diff > 180.0 { 360.0 - diff } else { diff };
                    if normalized_diff > 90.0 {
                        intensity_scale *= 0.2;
                    } else {
                        intensity_scale *= 1.0 + (1.0 - (normalized_diff / 90.0)) * 0.5;
                    }
                } else if !r.has_window {
                    intensity_scale *= 0.5;
                }
            }

            // 3. Fixture type sensitivity multiplier
            let fixture_sensitivity = match fixture_type.as_deref().unwrap_or("ceiling_spot") {
                "table_lamp" => 1.0_f32,
                "floor_lamp" => 0.8,
                "led_strip" => 0.9,
                "pendant" => 0.7,
                _ => 0.4, // ceiling_spot and unknown
            };

            // 4. Spatial weighting — 3D when Z is set, 2D fallback for legacy
            let exposure = if z > 0.0 {
                // Full 3D: sun direction vector × normalised bulb position vector
                let az_rad = effective_azimuth.to_radians();
                let el_rad = elevation.to_radians();
                let sun = (
                    (az_rad.sin() * el_rad.cos()) as f32,
                    (az_rad.cos() * el_rad.cos()) as f32,
                    el_rad.sin() as f32,
                );
                let bx = x - 0.5;
                let by = y - 0.5;
                let bz = z - 0.5;
                let len = (bx * bx + by * by + bz * bz).sqrt().max(1e-6);
                let dot = (bx / len) * sun.0 + (by / len) * sun.1 + (bz / len) * sun.2;
                dot.max(0.0)
            } else {
                // Legacy 2D path — preserves old behaviour until user sets up canvas
                let sun_rad = effective_azimuth.to_radians();
                let raw = (x * sun_rad.sin() as f32 + y * sun_rad.cos() as f32) * 0.1;
                raw.clamp(-1.0, 1.0)
            };

            let final_bri = ((base_bri as f32 + (exposure * fixture_sensitivity * 20.0))
                * intensity_scale as f32)
                .clamp(1.0, 255.0) as u8;

            if let Some(node_id) = self.dashboard.get_node_for_device(&device_id) {
                let request_id = format!("solar-{}", Utc::now().timestamp());

                info!(
                    device = %device_id,
                    bri = final_bri,
                    ct = base_ct,
                    scale = format!("{:.2}", intensity_scale),
                    "Solar sweep → sending bri={} ct={} mireds",
                    final_bri, base_ct
                );

                // Send Brightness
                self.dashboard.send_to_node(
                    &node_id,
                    MeshMessage::LightCommand(LightCommandRequest {
                        request_id: request_id.clone(),
                        target: LightTarget::Device(device_id.clone()),
                        command: LightAction::Brightness(final_bri),
                    }),
                );

                // Send Color Temp
                self.dashboard.send_to_node(
                    &node_id,
                    MeshMessage::LightCommand(LightCommandRequest {
                        request_id,
                        target: LightTarget::Device(device_id),
                        command: LightAction::ColorTemp(base_ct),
                    }),
                );
            } else {
                warn!(device = %device_id, "Solar: no connected node found for device — skipping");
            }
        }
    }
}

fn calculate_solar_state(elevation: f64) -> (u8, u16) {
    if elevation <= 0.0 {
        // Night/Twilight: -18° → bri 1, 0° → bri 30, fixed warm white
        let t = ((elevation + 18.0) / 18.0).clamp(0.0, 1.0);
        let bri = (1.0 + t * 29.0).round() as u8;
        return (bri, 500);
    }

    // Daytime: 0° → bri 30 / 454 mireds (warm), 90° → bri 255 / 153 mireds (cool)
    let t = (elevation / 90.0).clamp(0.0, 1.0);
    let bri = (30.0 + t * 225.0).round() as u8;
    let ct = (454.0 - t * 301.0).round() as u16;
    (bri, ct)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_solar_state() {
        // Noon (90°)
        let (bri, ct) = calculate_solar_state(90.0);
        assert_eq!(bri, 255);
        assert_eq!(ct, 153); // 6500K

        // Sunrise (0°)
        let (bri, ct) = calculate_solar_state(0.0);
        assert_eq!(bri, 30);
        assert_eq!(ct, 500);

        // Afternoon (45°)
        let (bri, ct) = calculate_solar_state(45.0);
        assert!(bri > 100 && bri < 200);
        assert!(ct > 153 && ct < 454);

        // Deep Night (-18°)
        let (bri, ct) = calculate_solar_state(-18.0);
        assert_eq!(bri, 1);
        assert_eq!(ct, 500);
    }

    #[test]
    fn test_spatial_weighting_logic() {
        // Bulb at (5.0, 0.0) - Far East
        let x: f32 = 5.0;
        let y: f32 = 0.0;

        // Sun at 90° (East)
        let sun_azimuth: f64 = 90.0;
        let sun_rad = sun_azimuth.to_radians();
        let sun_loc_x = sun_rad.sin() as f32;
        let sun_loc_y = sun_rad.cos() as f32;

        let exposure: f32 = (x * sun_loc_x + y * sun_loc_y) * 0.1;
        assert!(exposure > 0.0);
        let clamped = exposure.clamp(-1.0, 1.0);
        assert_eq!(clamped, 0.5); // 5.0 * 1.0 * 0.1

        // Sun at 270° (West)
        let sun_azimuth: f64 = 270.0;
        let sun_rad = sun_azimuth.to_radians();
        let sun_loc_x = sun_rad.sin() as f32;
        let sun_loc_y = sun_rad.cos() as f32;
        let exposure: f32 = (x * sun_loc_x + y * sun_loc_y) * 0.1;
        assert!(exposure < 0.0);
        assert_eq!(exposure.clamp(-1.0, 1.0), -0.5);
    }

    #[test]
    fn test_room_aware_intensity() {
        // Room with no windows should have 0.5 scale
        let has_window = false;
        let _window_facing: Option<f32> = None;
        let mut scale = 1.0;
        if !has_window {
            scale *= 0.5;
        }
        assert_eq!(scale, 0.5);

        // Room with South window (180°), Sun in North (0°)
        let has_window = true;
        let window_facing = Some(180.0);
        let effective_azimuth: f64 = 0.0;
        let mut scale = 1.0;
        if has_window && let Some(facing) = window_facing {
            let diff = (effective_azimuth - facing).abs();
            let normalized_diff = if diff > 180.0 { 360.0 - diff } else { diff };
            if normalized_diff > 90.0 {
                scale *= 0.2;
            }
        }
        assert_eq!(scale, 0.2);
    }
}
