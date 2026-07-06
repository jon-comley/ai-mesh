# iPad LiDAR Room Scan (RoomPlan) — Research + Deferred Plan

**Status (2026-07-06): deferred.** Jon's Mac Studio has arrived but isn't
unboxed/set up yet — building this needs Xcode, which only runs on macOS
(no route to compile/sideload an iOS app from this project's usual Linux
dev environment). Revisit once the Mac is running. The iPad itself (iPad
Pro 11" M5) is already unboxed and confirmed to have the LiDAR Scanner
this whole approach depends on.

## How this got here

The original ask was "a proper scan of the room, like Apple do when they
scan your face" — i.e. the automatic, walk-around-and-it-builds-itself
experience Apple demos for RoomPlan. Two things were tried first and
dropped before landing on this plan; both are worth recording so they
aren't re-attempted the same way.

### Attempt 1: wall-photo backdrop (dropped, code removed 2026-07-06)

Shipped as Phase 4 of the home-ui-redesign plan: users could photograph
each wall and see it as a semi-transparent backdrop in the layout editor,
first behind the 2D top-down canvas, then (after the 2D version was
pointed out as geometrically wrong — a wall photo is a front-on/elevation
shot, the 2D view is a straight-down plan, no rotation reconciles those
two projections) as a texture on the matching wall in the 3D view instead.

Dropped entirely after live testing: a normal phone photo of a wall in a
typically-sized room inevitably includes floor, ceiling, and perspective
distortion — there's no way to get a clean edge-to-edge shot of just the
flat wall surface without specialist equipment. Stretching that photo to
exactly fill the wall's rectangle made the distortion worse, not better.
The whole `room_wall_photos` table, its 3 REST endpoints, and all the
layout.js/layout3d.js/style.css UI for it were removed in the same commit
that added this doc (see git history — `fix(home): remove wall-photo
backdrop feature`).

### Attempt 2: research into automatic/AR-assisted scanning on Android

Jon's daily phone is a Samsung Galaxy S22 (Android, no LiDAR/ToF). Researched
before writing any code, since RoomPlan-equivalent automatic scanning was
the actual ask:

- **Apple RoomPlan** requires LiDAR hardware (iPhone/iPad Pro only) and is
  a native iOS/Swift API — not reachable from any web technology, and
  wouldn't run on Android regardless of hardware.
- **Google's Scene Semantics API** (the closest-sounding ARCore feature)
  is *outdoor-only* — it labels sky/road/building/tree, nothing indoor.
- **ARCore's Depth API** has ~88% device coverage, which is itself the
  tell that it's mostly *software* depth-from-motion (single camera + ML),
  not real depth-sensor data — the S22 has no ToF/LiDAR, so it'd hit the
  software fallback. Good enough for AR occlusion, nowhere near precise
  enough for floor-plan-quality geometry.
- There is no shipped "RoomPlan for Android" product API from Google.
- **Confirmed via Magicplan's own documentation** (a leading commercial
  room-scanning app) that Android has *no* camera/AR scanning feature at
  all: ["Android devices are not supported for magicplan's scan features"](https://help.magicplan.app/scan-a-room-with-the-camera-of-your-mobile-device-android)
  due to no LiDAR. Their Android fallback is manual: a preset "square
  room," "import and draw" (trace over an uploaded floor plan image), or
  ["define corners"](https://help.magicplan.app/create-a-room-with-the-define-corners-feature) —
  tap each corner on a plain grid (1 tile = 1m²), then type in exact
  dimensions to refine. No camera, no AR, at all.
- **WebXR** (the browser-accessible path) has a real Plane Detection API
  and a newer Mesh Detection module, but: Android-Chrome-only (not iOS
  Safari, not embedded WebView), uneven real-device support, and mesh
  detection specifically is mostly Microsoft/Magic-Leap-headset-driven
  with thin phone-AR coverage. Live demos to actually look at:
  [three.js AR plane detection](https://threejs.org/examples/webxr_ar_plane_detection.html),
  [Immersive Web plane detection sample](https://immersive-web.github.io/webxr-samples/proposals/plane-detection.html).

**Conclusion**: even the best-resourced commercial apps hit the same wall
on non-LiDAR Android hardware that we would. "Scan like Face ID" isn't
reachable there from any app, browser or native. Skipped for Android.

### The unlock: iPad Pro (M5) has LiDAR

Confirmed directly from Apple's own tech specs page for the iPad Pro
11-inch (M5): **Sensors: Face ID, LiDAR Scanner, Three-axis gyro,
Accelerometer, Barometer, Ambient light sensors.** (Note for future
reference: iPad *Air* does not have LiDAR — only the Pro line does, and
there's no "iPad Air (M5)" in Apple's lineup; the earlier back-and-forth
in this project's own conversation history mixed up Pro/Air naming before
this was confirmed.)

This makes RoomPlan itself available — genuinely the "move it around and
watch it build the room" experience originally asked for — but only via a
**native iPadOS/Swift app**. iOS doesn't expose ARKit/LiDAR depth data to
web content at all (unlike the Android/WebXR situation above), so there's
no way to reach RoomPlan from Safari or any browser-based approach.

## What RoomPlan actually gives you (not photos)

Important distinction from the wall-photo attempt: RoomPlan's output is
**structural geometry**, not images. A `CapturedRoom` result gives wall
positions/lengths, door and window openings (with position along the
wall), and rough furniture bounding boxes — built from depth + mesh data,
not a set of clean photos. It would replace *manually typing room
dimensions and dragging opening markers into place* in the layout editor,
not the (now-removed) wall-photo feature — those were always two
different kinds of data serving different purposes.

If real wall photos are ever wanted again for visual reference (not
spatial alignment), the good news is that doesn't need a native app at
all: the iPad's Safari browser can already use ai-mesh's normal web
dashboard like any device. That path was removed along with the rest of
the wall-photo feature, but revisiting it — if ever — would be a much
smaller, purely web-based effort, decoupled entirely from RoomPlan.

## Planned integration shape (once the Mac is available)

1. **Native iPadOS app** (Swift, `RoomPlan` framework, iOS 16+): the
   standard RoomPlan capture UI — walk the iPad around the room, walls/
   openings highlight as detected, confirm when the scan looks complete
   (RoomPlan supports reviewing/adjusting before finalizing).
2. **Translate `CapturedRoom` → ai-mesh's existing REST shape** — no new
   backend endpoints needed, the data already maps onto what exists:
   - Room dimensions → `PATCH /api/rooms/{id}/dimensions` (`width_m`,
     `depth_m`, `height_m` — see `RoomRecord` in `registry/mod.rs`).
   - Each door/window → `POST /api/rooms/{id}/openings` (`opening_type`,
     `wall_edge`, `x_norm`, `width_norm`, `transmission` — see `Opening`
     in `registry/mod.rs`).
3. **Post to the coordinator** using the same mesh auth token the web
   dashboard already uses. Once posted, it's live everywhere — the
   Android phone (or any device) opening the layout editor immediately
   sees the captured dimensions/openings, since everything reads from the
   same coordinator database. No separate "transfer" step, same as every
   other cross-device sync in this app already works.

### Real technical challenges to solve when this is actually built

- **Coordinate-frame reconciliation.** RoomPlan's walls come out in an
  arbitrary ARKit-session-relative frame, not compass-oriented. ai-mesh's
  `wall_edge` model is N/S/E/W tied to `orientation_degrees` (true-north
  alignment). Need either: the device's own compass/heading (`CLHeading`)
  captured during the scan to align RoomPlan's walls to compass
  directions, or falling back to the *existing* manual orientation flow
  (phone-compass calibration already in the layout editor) as a
  post-scan step.
- **Rectangular-room assumption.** ai-mesh's room model is a simple box
  (`width_m` × `depth_m` × `height_m`, four compass walls) — no arbitrary
  polygon support. RoomPlan can capture non-rectangular rooms (alcoves,
  L-shapes). For v1, a non-rectangular capture should reduce to its
  minimum bounding rectangle rather than trying to extend the data model —
  matches "the existing spatial engine doesn't support arbitrary
  polygons" as a known, accepted v1 limitation rather than new scope.
- **Which existing room this scan updates** (or whether it creates a new
  one) needs a simple picker in the app — same room list the dashboard
  already has via `GET`-equivalent room data.

### Why this waits for the Mac specifically

This project has been Rust (coordinator) + vanilla JS (dashboard) with a
Linux dev workflow throughout — build, test, and verify entirely possible
from that same environment for every prior feature. A native iOS app is
a hard platform requirement: Xcode only runs on macOS, and there's no
route to compile or sideload a Swift/iOS app from Linux or Windows by any
official means. Every other piece of this project could be built and
verified end-to-end before Jon ever saw it; this would be the first thing
written without any ability to compile-check or run it first — better to
do that iteration loop for real once the Mac is actually usable, rather
than write speculative Swift that might not even build.
