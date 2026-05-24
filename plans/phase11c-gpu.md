# Phase 11C-C7 — GPU Health Metrics

**Goal:** Extend the health timeline with per-node GPU utilisation and VRAM metrics.
Add a GPU sparkline row to the Health panel dashboard, shown only when a node reports GPU data.

Reviewed by Bing + Gemini. All design decisions finalised.

---

## Architecture decisions

| Question | Decision | Rationale |
|---|---|---|
| Wire fields | `gpu_usage_pct: Option<f32>`, `gpu_vram_used_gb: Option<f32>`, `gpu_vram_total_gb: Option<f32>` added to `HeartbeatPayload` | Genuinely optional — CPU-only nodes have no GPU; `Option` is correct, not a compat shim |
| Serialisation | `#[serde(default)]` on all three fields | Pre-C7 agents omit the fields; deserialisation yields `None` gracefully |
| Timestamp source | Coordinator-stamped (inherited from existing `HealthSample` pipeline) | Consistent with CPU/RAM; no change needed |
| GPU% read — Linux | sysfs: `/sys/class/drm/card0/device/gpu_busy_percent` (integer 0–100) | Standard `amdgpu` driver interface; zero-dependency file read |
| VRAM read — Linux | sysfs: `mem_info_vram_used` / `mem_info_vram_total` (bytes) | Same driver interface |
| GPU% read — Windows | WMI `Win32_PerfFormattedData_GPUPerformanceCounters_GPUEngine`, sum `engtype_3D` utilisation | Industry standard for AMD/Intel/NVIDIA on Windows; requires `wmi` crate |
| VRAM read — Windows | WMI or DXGI `IDXGIAdapter3::QueryVideoMemoryInfo` | Gives current reservation budget |
| iGPU VRAM label | Dashboard shows "VRAM (shared)" for iGPU nodes | AMD Radeon 780M draws from system RAM; total is dynamic, not a fixed ceiling |
| No-GPU nodes | Return `None`; dashboard hides the GPU row entirely | pi1 VideoCore has no `amdgpu` sysfs; files absent → `None` automatically |
| ARM64 Windows (`wmi`) | Deferred — Mac mini M4 runs macOS, not Windows | No current Windows ARM64 hardware |
| HealthSample | Add the same three `Option<f32>` fields to coordinator's `HealthSample` | Keeps GPU data in the same ring buffer and broadcast path as CPU/RAM |
| Dashboard render | GPU sparkline in Health panel, below RAM row, hidden when all samples have `None` | Clean degradation; CPU-only nodes are unaffected |

---

## Implementation phases

### C7a — Linux sysfs pipeline (do first)

**Goal:** validate the full data pipeline — wire protocol → coordinator → dashboard — even though no current Linux node has an `amdgpu` GPU. pi1 will always return `None`; the pipeline is still correctly exercised.

Files to change:
1. `shared/src/hardware.rs` — add three `Option<f32>` fields to `HeartbeatPayload`
2. `agent/src/hardware.rs` (or new `agent/src/gpu.rs`) — Linux sysfs read, fall back to `None`
3. `coordinator/src/http/state.rs` — add fields to `HealthSample`; update `push_health()` signature
4. `coordinator/src/server.rs` — extract GPU fields from heartbeat, pass to `push_health()`
5. `coordinator/src/http/static/health.js` — GPU sparkline row, hidden when no data
6. Tests — unit tests for sysfs read (absent files → `None`), `HealthSample` serialisation

### C7b — Windows WMI (do second)

**Goal:** produce live GPU sparklines on beelink1 (AMD Radeon 780M).

Files to change:
1. `agent/src/gpu.rs` — Windows branch: `wmi` crate query for GPU util%; WMI or DXGI for VRAM
2. `Cargo.toml` (agent) — add `wmi` as a Windows-only dependency: `[target.'cfg(target_os = "windows")'.dependencies]`
3. Tests — mock WMI response parsing

**C7b is when GPU sparklines first appear on the dashboard.**

---

## Wire protocol changes (`shared/src/hardware.rs`)

```rust
pub struct HeartbeatPayload {
    // ... existing fields ...
    pub cpu_usage_pct: f32,
    pub ram_used_gb:   f32,
    pub ram_total_gb:  f32,
    // C7 additions:
    #[serde(default)]
    pub gpu_usage_pct:    Option<f32>,
    #[serde(default)]
    pub gpu_vram_used_gb: Option<f32>,
    #[serde(default)]
    pub gpu_vram_total_gb: Option<f32>,
}
```

---

## Linux sysfs read (`agent/src/gpu.rs`)

```rust
pub struct GpuSample {
    pub usage_pct:    f32,
    pub vram_used_gb: f32,
    pub vram_total_gb: f32,
}

pub fn read_gpu_sample() -> Option<GpuSample> {
    let usage = read_sysfs_u64("/sys/class/drm/card0/device/gpu_busy_percent")?;
    let used  = read_sysfs_u64("/sys/class/drm/card0/device/mem_info_vram_used")?;
    let total = read_sysfs_u64("/sys/class/drm/card0/device/mem_info_vram_total")?;
    if total == 0 { return None; }
    Some(GpuSample {
        usage_pct:    usage as f32,
        vram_used_gb: used  as f32 / 1_073_741_824.0,
        vram_total_gb: total as f32 / 1_073_741_824.0,
    })
}

fn read_sysfs_u64(path: &str) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}
```

On pi1 (no `amdgpu`): files absent → `read_sysfs_u64` returns `None` → `read_gpu_sample` returns `None`. Correct.

---

## Coordinator `HealthSample` extension

```rust
pub struct HealthSample {
    pub ts_ms:         u64,
    pub cpu_pct:       f32,
    pub ram_used_gb:   f32,
    pub ram_total_gb:  f32,
    // C7 additions:
    pub gpu_pct:       Option<f32>,
    pub gpu_vram_used_gb:  Option<f32>,
    pub gpu_vram_total_gb: Option<f32>,
}
```

`push_health()` gains three extra `Option<f32>` parameters.

---

## Dashboard GPU sparkline logic (health.js)

```js
// Only render GPU row if at least one sample has gpu data.
const hasGpu = samp.some(s => s.gpu_pct != null);
if (hasGpu) {
  const gpuData  = samp.map(s => s.gpu_pct ?? 0);
  const vramData = samp.map(s =>
    s.gpu_vram_total_gb > 0 ? (s.gpu_vram_used_gb / s.gpu_vram_total_gb) * 100 : 0
  );
  // render GPU sparkline + VRAM (shared) sparkline
}
```

---

## Key unknowns resolved

1. **`wmi` crate on ARM64 Windows** — moot; Mac mini M4 is macOS. Revisit if a Windows ARM64 node ever joins.
2. **iGPU VRAM label** — show "VRAM (shared)" to distinguish from discrete VRAM ceiling.
3. **`gpu_busy_percent` on pi1** — file absent → `None` → GPU row hidden. Expected and correct.

---

## Test plan

### C7a unit tests
- `read_gpu_sample` returns `None` when sysfs files are absent (mock with non-existent path)
- `read_gpu_sample` returns correct values when files exist (tempfile with known content)
- `HealthSample` serialises `None` GPU fields correctly (field absent in JSON)
- `HealthSample` serialises non-`None` GPU fields correctly

### C7b unit tests
- WMI query result parsing produces correct `GpuSample`
- Zero-GPU WMI result returns `None`

### Live validation (both phases)
- `just restart-coordinator` → open dashboard → Health tab
- C7a: GPU row absent on all nodes (expected — no Linux AMD node)
- C7b: GPU row present on beelink1; absent on pi1 and OmniLink1
