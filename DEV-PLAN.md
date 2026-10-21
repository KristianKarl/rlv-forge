# RLV Forge development plan

## Goal

Build **RLV Forge**, a native Rust app for preparing Real Life Video (RLV)
indoor cycling routes, with offline GPX editing inspired by the reference
[GPX Elevation Smoother web app](https://mtbsettimo.altervista.org/gpx-elevation-smoother/gpxsmoother.html).
Retain its numbered workflow, light background, blue accents, original/new
chart colours, range selection, and GPX export, with automatic smoothing
as the only desktop smoothing action and no separate Instructions page.
Implement the behaviour independently in Rust rather than embedding a web page.
Also support an Incyclist-oriented smoothing mode, GPX-only cuts with levelled
elevation joins, read-only prepared video, pairing-XML export and global undo/redo.

## Architecture

| Area | Choice |
| --- | --- |
| Desktop UI | egui/eframe with a native OpenGL window, no webview |
| Charts | egui painter; elevation, slope, and slope-coloured profile |
| Route map | Walkers/egui, OpenStreetMap tiles, local HTTP cache, local route overlay |
| File dialogs | rfd using native dialogs / Linux XDG desktop portals |
| GPX parsing | roxmltree with validated coordinates and elevations |
| GPX export | Byte-range edits to original XML, preserving unknown extensions |
| Processing | Separate UI-independent Rust library with typed operations |
| Timeline | Original-clock editable GPX cuts, map-picked sync points, piecewise timing, levelled joins |
| Video | Local FFmpeg/ffprobe; bounded sequential playback decoder; byte-identical video copies on export |
| Incyclist | Relative video/GPX references, UUID, source frames and geographic cut markers |
| History | Shared in-memory undo/redo for edits, file replacements, settings and views |
| Projects | Versioned atomic `.gpxproj` saves with embedded original/edited GPX and external video references |
| Responsiveness | Background file/media/cut work, cancellable copying; viewport-bounded charts |
| Development | Pinned Nix flake and Cargo lockfile |

## Implementation sequence

### 1. Reproducible foundation

- Create a Cargo binary and processing library.
- Use **RLV Forge** for display branding, `rlv-forge` for the Cargo package
  and executable, and `rlv_forge` for the Rust library crate.
- Supply `flake.nix` with Rust, formatting, linting, language-server, and native
  graphics dependencies; generate `flake.lock`.
- Document `nix develop`, run/build commands, and desktop portal requirements.

Acceptance: a fresh `nix develop` shell can format, compile, and run the app
without relying on a pre-existing Rust installation.

### 2. Lossless GPX import/export

- Import GPX 1.0/1.1 track segments and routes, including qualified tags.
- Keep original XML and per-point elevation edit ranges.
- Calculate cumulative horizontal distances without joining separate segments.
- Fill missing elevations with a visible warning; reject malformed input.
- Export edited elevations, track/route names, and descriptions while
  retaining timestamps, coordinates, namespaces, and sensor extensions.

Acceptance: both supplied sample files round-trip with the same point count,
coordinates, timestamps, and sensor extension subtrees.

### 3. Editing engine

- Add a one-click **Auto Smooth** operation for route-preserving cleanup:
  resample by distance, reject isolated elevation spikes using a Hampel
  filter, and apply distance-weighted local linear regression without
  limiting grades. Anchor selection/segment endpoints and do not smooth
  across large recording gaps. Run it in a background task and report
  changed-point counts and maximum displacement.
- Retain legacy averaging, slope-range, flattening, and elevation-shift
  operations in the processing library for compatibility, without exposing
  their controls in the desktop interface.
- Add an Incyclist mode that smooths grades by distance and reintegrates them
  while retaining both block endpoints; do not impose maximum grades.
- Restrict every operation to selected points; do not cross segment boundaries.
- Recompute slopes after edits; retain the immutable original elevation values.
- Retain the preceding grade at stationary/sub-centimetre steps rather than
  introducing false zero-grade spikes; reset grade at segment boundaries.
- Reject invalid parameters atomically and support reset.

Acceptance: deterministic tests cover numerical results, accumulation,
selection boundaries, duplicate coordinates, missing elevations, multiple
segments, reset, and invalid parameters. Automatic smoothing must reduce
synthetic noise/spikes while preserving sustained steep climbs, broad hills,
endpoints, and equivalent profiles recorded at different point densities.
Trainer-mode tests also cover reduced grade changes, preserved steep climbs,
selection boundaries and sampling-density independence.

### 4. Native reference-style interface

- Reproduce the numbered open/edit/inspect/select/reset/export workflow.
- Provide **Auto Smooth** as the only smoothing control, without manual
  parameter fields or an Instructions tab. Keep guidance in the README
  and relevant tooltips.
- Add original/new overlays, original visibility toggle, chart tabs, hover
  details, selected average slope, and slope-coloured profile.
- Provide draggable triangular overview handles, numeric distance bounds,
  main-chart zoom/pan, Shift+wheel horizontal panning at a fixed zoom, and
  full-range reset.
- Add file drop and optional command-line input.
- Expose errors and missing-elevation warnings in the UI.
- Add export metadata, native save dialog, optional XML preview, and clipboard.
- Confirm before discarding unsaved changes.
- Add a responsive upper-right OpenStreetMap panel that fits newly loaded
  tracks, supports pan/zoom and refitting, and highlights the selected range
  in blue over a muted full-route overlay. Synchronize map bounds whenever
  the distance range changes in chart section 3 or 4, including numeric
  bounds, panning, and restoring the full ride; preserve independent map
  navigation until the next chart-range change.
- Preserve segment gaps on the route overlay, show start/end markers and
  attribution, cache visible tiles, identify tile requests, and surface
  loading failures with retry. Allow disabling online tiles.
- Move the red map marker to the chart-3 cursor's distance, interpolating
  between samples without bridging disconnected segments. Restore the
  route-end marker when the cursor leaves the chart.

Acceptance: load either example, compare charts, edit a subsection, reset,
and save a GPX that reopens correctly. Editing/export need no network;
online map backgrounds request visible tiles without uploading GPX documents.

### 5. Verification and delivery

- Run rustfmt, Clippy with warnings denied, library/UI helper tests, and
  integration tests using both sample files.
- Build the native executable and smoke-test window startup on Linux.
- Check the Nix shell independently of the host's broken/missing Rust tools.
- Record supported behaviours and intentional deviations in the README.

### 6. Prepared video, GPX cuts, history and Incyclist export

- Use prepared video exclusively as a read-only reference with scrub/silent
  preview and frame-interval stepping. Remove video-trim, output-frame-rate
  and encoding controls from the app.
- Decode playback continuously with one FFmpeg process and a bounded frame
  queue, using a wall-clock presentation schedule and updating the existing
  texture. Sample VFR timestamps at the reported average rate. Stop and reap
  playback on pause, seek, file replacement and page exit; reject stale previews.
- Show a shared-style elevation profile centered on the GPX red marker, with
  its distance span derived from the map's zoom and latitude. Keep the center on
  the marker during manual map exploration; use the full original track during
  picking and the current smoothed/cut track otherwise. Cache slopes by track
  identity rather than recomputing them on playback frames.
- Allow profile click-to-seek, horizontal drag-to-pan and horizontal/Shift-wheel
  seeking through the same video/GPX synchronization as the sliders. Keep video
  frozen during landmark picking and leave plain vertical scrolling to the page.
- Keep timeline fields, preview guidance, map status and coverage messages at
  stable sizes while scrubbing; preserve the profile frame outside GPX coverage.
- Align video and timestamped GPX through a signed offset or matching anchor.
  Show the corresponding route-map position and report missing time coverage.
- Pick numbered video-to-original-GPX sync points on the full reference route;
  expose repeated/stationary occurrences instead of silently choosing a pass.
  Interpolate landmarks to correct drift. Explicit Before/After pairs remove
  GPX intervals, hold the map position across the video edit, then jump.
  Automatically apply newly picked points. An unpaired cut boundary acts as a
  landmark until a consecutive Before/After pair is complete, then the GPX cut
  applies automatically. Keep explicit Apply/Cancel for manual table edits and
  shared undo/redo.
- Keep manual cuts as a fallback, editable in whole original-GPX seconds.
  Rebuild from the uncut smoothed source, union overlaps, preserve subsequent
  rows' time basis and smoothing provenance. Keep untimed distance rows sequential.
  Retain the full original map route while the cursor skips cuts.
- Link full-width video and map-side GPX sliders in both directions through
  the applied mapping, with ten-second steps and explicit shared coverage.
  Use identical synchronized GPX generation for standalone and bundle export,
  preserving exact anchor samples and rejecting timestamp collisions.
- Cut selected recorded samples or marked current-GPX time intervals, including
  stationary stops. Interpolate timed boundaries only within connected segments,
  preserve retained sensor extensions, and remove cut durations from timestamps.
  Untimed tracks/routes support distance-based sample cuts.
- Level each new elevation join with a constant shift of all following
  elevations, preserving gradients in retained sections, including after reset.
  Run GPX cuts off the UI thread.
- Maintain one undo/redo history for smoothing, cuts, reset, metadata, alignment,
  file replacement, settings and chart/map views. Group continuous interactions;
  support header buttons and platform-standard shortcuts in editing fields too.
- Stage a byte-for-byte copy of the entire prepared video, preserving its
  extension and all streams. Copying is cancellable; never publish partial
  bundles or overwrite existing ones. Export matching GPX, source frame metadata,
  relative filenames, a fresh UUID and Incyclist geographic cut markers.

Acceptance: cuts close time, distance and elevation gaps while preserving retained
gradients and sensor data. Repeated cuts and reset round-trip correctly. Exported
video bytes equal the prepared source, including audio and container metadata.
Verify source extensions, cancellation, no-overwrite behavior, namespaced GPX,
invalid-input errors and global undo/redo with redo-branch invalidation.
Installed Incyclist playback and native desktop interaction remain manual checks.

### 7. Seek-follow and resumable projects

- Pan to center the red cursor when seeking on either timeline, including
  landmark picking and cut jumps; preserve zoom and permit manual exploration
  until the next seek.
- Match Walkers' clipped-viewport tile projection when drawing the route, sync
  pins and picking locations so scrolling never offsets overlays from map tiles.
- Save/open versioned `.gpxproj` files through header actions, keyboard
  shortcuts, drag/drop and the first command-line argument.
- Write the `rlv-forge-project` format identifier while accepting legacy
  `gpx-smoother-project` files; keep the `.gpxproj` extension.
- Embed original XML, current elevations and sample provenance for both the
  current GPX and uncut smoothed source. Preserve the reset/export baseline,
  sensor extensions and smoothing when cuts are later revised.
- Restore cuts, sync anchors, alignment, paused playhead, table drafts, export
  metadata and chart/map preferences. Keep undo/redo session-only.
- Reference prepared video externally and report missing files with relinking
  guidance. Never modify or embed video.
- Stage and atomically replace project files; validate all loaded state before
  replacing the current ride. Offer save-and-continue before leaving unsaved work.

Acceptance: save/restart/reopen preserves editing and export results, even when
the original GPX is gone. Invalid files and failed saves preserve existing work.
Seeking after cuts centers the cursor without changing zoom. Keep eframe/egui
compatible with Walkers when updating dependencies.

## Intentional differences from the web app

- Smoothing changes never propagate outside the selected distance interval;
  GPX cuts intentionally shift subsequent elevations to level the new join.
- Smoothing windows stop at selection/segment boundaries.
- Distances use haversine rather than an ellipsoidal geodesic calculation.
- Grade values are recomputed consistently after every elevation edit.
- File export has no browser data-URL size limit; only the optional displayed
  XML preview is truncated.
- Preserve unrelated GPX XML instead of reconstructing a reduced document.
- Use native file/clipboard APIs; no browser fallback is necessary.

## Follow-up scope, not required for the first version

- Reusable smoothing presets.
- Per-track selection and metadata editing for multi-track documents.
- Geodesic distance refinements and distance-weighted smoothing options.
- Automated desktop interaction tests and packaged platform installers.
- Native appearance checks on macOS and non-Nix Linux desktops.
- Automatic GPX/video landmark detection. Manual sync points already correct
  clock drift; video editing
  itself is intentionally external to this app.
