# RLV Forge

**Prepare real rides for indoor training.**

RLV Forge is a native Rust desktop app for preparing **Real Life Video (RLV)**
indoor cycling routes: import GPX or FIT files, smooth elevations, cut unwanted
route sections, align the route with prepared video, and export [Incyclist-ready bundles](https://incyclist.com/faq/#own-video).
Its elevation-editing workflow was inspired by the
[GPX Elevation Smoother web app](https://mtbsettimo.altervista.org/gpx-elevation-smoother/gpxsmoother.html).
No browser, server, or account is required. Editing and GPX export work
offline; the optional OpenStreetMap background uses an internet connection.
The **Syncing** page uses an already-prepared video as a read-only
reference. Match video frames to locations on the original GPX route, skip
omitted sections, level elevation joins, and export a ready-to-import
Incyclist bundle on **Delivery** without changing video.

The export bundle is target for the [InCyclist app](https://incyclist.com)

## Develop and run

With Nix and flakes enabled:

```sh
# Run the packaged app without entering a development shell:
nix run .
# Optionally pass a track or saved project:
nix run . -- --gpxproj /path/to/ride.gpxproj
# Or build once and run the packaged executable:
nix build
./result/bin/rlv-forge
```

The package wraps the executable with its runtime libraries and puts
FFmpeg/ffprobe and, on Linux, Zenity on `PATH`. It still requires a graphical
desktop and working host GPU drivers.

For development:

```sh
nix develop
cargo run
# Optionally open a track on launch:
cargo run -- --fit-gpx data/2026-09-22_120200.gpx
# FIT activities are also supported:
cargo run -- --fit-gpx data/2026-10-24.fit
# Optionally open a video alongside the track:
cargo run -- --fit-gpx data/2026-09-22_120200.gpx --video /path/to/ride.mp4
# Resume saved editing work:
cargo run -- --gpxproj /path/to/ride.gpxproj
# Show command-line help or version (including the abbreviated build commit):
cargo run -- --help
cargo run -- --version
```

`--gpxproj` cannot be combined with `--fit-gpx` or `--video`.
`--fit-gpx` and `--video` can be used together or independently.
Legacy positional track/project and video arguments are still supported.
When building from source without Git metadata, set `RLV_FORGE_GIT_SHA`
to the abbreviated commit SHA; otherwise the version reports `unknown`
and the build emits a warning. Nix flake builds supply the revision automatically.

The flake provides Rust, Cargo, rustfmt, Clippy, rust-analyzer, pkg-config,
FFmpeg/ffprobe, Linux X11/Wayland/OpenGL and D-Bus libraries, and Zenity for
fallback file dialogs. It supports x86_64/aarch64 Linux and
Apple Silicon macOS (the pinned nixpkgs no longer supports Intel macOS).
On Linux the file picker uses the desktop's XDG portal; run the app
inside a graphical desktop with a working portal service. If the portal is
unavailable, the file picker falls back to Zenity. Raw Cargo release builds
still need the development shell
(`nix develop --command ./target/release/rlv-forge`); use `nix run .` or
`./result/bin/rlv-forge` to run without it. After changes to
`flake.nix`, exit and re-enter the shell to refresh its environment. Initial dependency
downloads require network access. Keep `Cargo.lock` and `flake.lock` for
reproducible dependency resolution.

Without Nix, install stable Rust and your platform's native development
dependencies for egui/winit (on Linux: a C linker, pkg-config,
X11/Wayland, libxkbcommon, OpenGL, and D-Bus). A working XDG desktop portal
or Zenity is also needed for file dialogs. Then use the same Cargo commands.
Video preview and pairing additionally require `ffmpeg` and `ffprobe` on
`PATH`. Media tests also use the H.264/libx264 encoder to create fixtures;
the app never re-encodes video. GPX-only editing does not require FFmpeg.

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

The Cargo package and executable are named `rlv-forge`; the Rust library crate
is `rlv_forge`. The release executable is `target/release/rlv-forge`.

## Workflow

1. Start on **Syncing** and use **Edit > Open GPX/FIT...** or drop a GPX or FIT file. Sample tracks are included in `data/`,
   including `2026-10-24.fit` and `2026-10-24.gpx` from the same ride.
   Open the prepared video and use **Set video start** to match its first frame
   to the original GPX route. **Confirm video start** trims earlier samples from
   the working copy and aligns video zero there. Add other sync landmarks/cuts
   before smoothing, so bad elevations in discarded sections cannot influence
   smoothing. The original GPX/FIT file is never edited.
2. Switch to **Smoothing**, then choose **Smooth** below the elevation chart for one-click noise reduction on the selected
   range. Additional clicks apply another pass to the modified data.
   **Incyclist: soften grade transitions** and **Show original** are checked
   by default. The former enables trainer-oriented smoothing without imposing
   a maximum grade.
3. Compare original (blue) and new (green) elevation/slope charts, or use
   the slope-coloured elevation profile. Hover for point details.
4. Drag the overview's triangular handles or enter start/end kilometres to
   choose the edited range. The main chart supports scroll-to-zoom,
   drag-to-pan, **Shift+mouse wheel** to pan left/right without changing the
   zoom level, and double-click-to-reset the range. Changing the range in
   either chart section (including numeric bounds and **Full ride**) selects
   that portion for editing. Smoothing has no map; route positioning stays on
   Syncing.
5. Choose **Cut selected GPX section & level elevation** to remove unwanted
   recorded samples. The following retained section is shifted vertically
   to meet the preceding one, preserving its gradients. Cuts immediately
   update the charts and distance; they do not edit any video.
6. GPX is exported automatically beside the source as
   `<source-name>-smoothed.gpx` after import and data edits, including cuts,
   undo and redo (video-aligned when sync points are applied). The generated
   file is replaced, never the source. View-only changes do not export.
   Write errors are shown in the app; editing and project saving remain
   available. **Reset The Data** re-reads the original GPX/FIT from disk,
   replacing the working track and clearing cuts, sync points, drafts, and
   alignment. It keeps the project save path and prepared video reference,
   selects the full ride, and updates the automatic GPX export. A missing or
   invalid source leaves the working data intact and shows an error. Reset
   can be undone and is the last control on Smoothing; no export metadata or
   GPX text preview is shown.

The connected top tabs are **Syncing**, **Smoothing**, and **Delivery**. Click
to select a tab, or use Left/Right and Home/End while a tab has keyboard focus.
All remain accessible with missing-input guidance. **File** contains project operations
and Exit; **Edit** contains Undo/Redo and the current page's import action.
Cuts and sync points have resizable, scrollable right sidebars on their
respective pages.
Smoothing has no map; the Syncing map matches the video viewport.

Numeric edit boxes support exponential dragging: small movements give fine
adjustments, and sensitivity doubles every 50 logical pixels from the click.
Returning to the click restores the starting value; holding still stops
changes. Hold Shift when starting a drag for finer adjustment (the mode stays
fixed until release); clicking to type still works.

The interface exposes automatic smoothing only, without manual smoothing
parameters or a separate Instructions page. Slopes are percentages.
Distances use the haversine formula on latitude/longitude, excluding gaps
between track segments.
Stationary samples and steps of 1 cm or less retain the preceding slope
within the same segment, avoiding artificial spikes to zero at repeated
GPS positions. Each segment starts at zero slope until a moving step.

FIT activities are converted internally to GPX, retaining recorded GPS
positions, elevations, timestamps, and available power, heart rate, cadence,
and temperature samples. Enhanced altitude is preferred when available.
Records without GPS coordinates are skipped, with a segment break so missing
GPS coverage does not add distance. Activities without GPS cannot be imported.
Other FIT messages and developer fields are not retained. Editing, video
alignment, and project saving work as for GPX imports; exports remain GPX,
not FIT. The imported FIT file is not changed.

### Save and resume editing

Use **File > Save project** (**Ctrl/Cmd+S**) to save an editable `.gpxproj` file,
or **File > Save project as...** (**Ctrl/Cmd+Shift+S**) for a separate copy.
**File > Open project...**, dropping a project onto the app, or opening it from the
command line restores the work after restarting.

RLV Forge keeps the `.gpxproj` extension and can open projects saved by its
predecessor, GPX Smoother. Newly saved projects use the `rlv-forge-project`
format identifier, including when resaving an older project; these files
require RLV Forge rather than the older app.

Projects contain the original and smoothed GPX, applied cuts and sync points,
unapplied table drafts, video alignment and playhead, export metadata, and
chart/map preferences. Playback resumes paused. Finish or cancel active
landmark picking before saving. Undo/redo history is session-only and is not
stored in the project.

The GPX (including converted FIT imports) is embedded, so its original file
is not needed to resume. Video is referenced by its path, never embedded or changed; keep the prepared video
available. If it has moved, reopen it with **Edit > Open prepared video...** on
**Syncing**; the app preserves sync points when the replacement's duration supports
them. Project saves replace the destination atomically and cannot overwrite
the referenced GPX/FIT or video. Invalid project files leave the current work intact.
Before opening another file or closing with unsaved work, choose **Save project
and continue**, **Discard changes**, or **Cancel**. Exporting GPX or an Incyclist
bundle is separate from saving an editable project.

### Automatic smoothing

**Smooth** prioritizes preserving the route, not making a trainer ride
artificially flat. It operates on the current elevations in the selected
chart range and runs in a background task. The status bar reports the number
of changed points and the largest elevation change. Reset re-imports the
original file from disk, discarding track edits rather than only restoring
elevations.

The algorithm first groups duplicate locations by median elevation, then
resamples each continuous selected track segment onto an approximately
5 m distance grid. This prevents stationary samples and uneven recording
frequency from changing the smoothing strength. A seven-sample Hampel
filter removes isolated outliers beyond three robust standard deviations
(with a 1 m minimum rejection threshold). A tricube-weighted local linear
regression over a 25 m radius then softens short-distance noise without
imposing any slope or slope-change limits. Local linear fitting preserves
sustained uphill/downhill grades rather than biasing them toward flat.

Results are interpolated back to the existing points; coordinates and point
counts do not change. The first/last selected elevations in each segment
are fixed, and gaps longer than 50 m are treated as separate blocks with
fixed endpoints. Blocks with fewer than five distinct positions or less
than 10 m of travel are left unchanged. A continuous block longer than
1,000 km reports an error instead of allocating an unbounded working grid.

No smoother can reliably distinguish every real, narrow terrain feature
from a GPS spike. Short features may soften, and an erroneous endpoint is
deliberately retained. Check the overlaid charts before exporting; repeated
clicks apply additional passes.

### Incyclist smoothing

The Incyclist option adds a tricube-weighted, distance-based smoothing pass
on the resampled grades over a 25 m radius. The resulting grades are
integrated back into elevations, with the endpoint residual distributed
as a constant grade correction. This softens short grade transitions while
preserving sustained climbs, block endpoints, coordinates, timestamps and
point count. It does not enforce a slope cap or a per-recorded-point
slope-change limit. The same selection and recording-gap rules apply.

GPX 1.0/1.1 tracks and routes, including namespaced documents, are supported.
Missing elevations inherit the previous value within the same segment, or
100 m at its start, with a visible warning. Invalid input reports an error.
Only selected elevations change; selection edges may introduce a grade
discontinuity. Edits do not propagate beyond the selection. Disconnected
segments are not joined.

Smoothing-only export retains all points, coordinates, timestamps, sensor
extensions, and unrelated XML. GPX cuts intentionally remove samples and
close the timeline as described below. Export name/description fields are applied to every track or
route; document-level metadata is retained unchanged. Elevations that
change are written with six decimal places. Large GPX files have no
browser download-size limit. The optional on-screen XML preview is
truncated to 20,000 characters; clipboard copy and file export are complete.
Unsaved edits are protected by confirmation before reset, opening another
file, or closing the app.

## Prepared video, GPX cuts and Incyclist pairing

1. Prepare the video outside this app, including any removed stops or ride
   sections. Import the corresponding GPX/FIT working copy on **Syncing**.
2. On **Syncing**, use **Edit > Open prepared video...**. The video and map have
   equally sized viewports with their controls directly below them. The track
   is shared with **Smoothing**, but synchronization comes first.
   Press **Set video start** to freeze the first frame at 0:00. Move the GPX
   marker to that frame's route location, then **Confirm video start** to trim
   earlier GPX data and anchor video zero in one undoable edit. Confirmation
   waits for the first frame to load; **Cancel picking** leaves data unchanged.
   The **Video start** row keeps video time fixed at zero; editing its GPX time
   can move the start earlier or later without altering the source file.
   Use **Set video end** to freeze the final frame, choose its matching GPX
   position, then **Confirm video end** to trim later GPX data in one undoable
   edit. The GPX marker keeps its current position when the video jumps; it does
   not default to the GPX end or a saved endpoint. Confirmation waits for that
   frame's preview. The trailing route is
   previewed in red, and cancellation leaves data unchanged. The **Video end**
   row fixes video time at the final frame and lets you revise the GPX endpoint.
   Both boundary tools preserve zoom and modify only the working copy.
   Export holds the final GPX location through the final frame's display time,
   retaining full video coverage without keeping unwanted trailing route data.
   Then pause on another recognizable frame and wait for its
   preview, then press **Pick Landmark**. The button stays pressed and the
   video, its slider, playback buttons and keyboard playback are locked.
   Use the map's **Original GPX seconds** slider, numeric field, or **-10 s /
   +10 s** buttons to position the red marker independently, across the full
   original GPX timeline, including cut sections. Map navigation never seeks
   or updates the frozen video.
   You can also click the original route. If it visits that location more
   than once, including stationary samples, choose the correct occurrence.
   Press **Pick Landmark** again to create and automatically apply the sync-point
   row and unlock the video, or **Cancel picking** to discard the selection. Orange numbered
   pins identify created points.
3. Repeat at other landmarks, preferably near both ends. The **Sync points**
   table lets you edit landmarks' video/original-GPX seconds and point type,
   seek to a frame, or remove a row. Cuts appear as one row with **Video seconds**
   and **Duration**: the original GPX seconds removed, not video duration.
   Editing duration keeps the GPX start fixed; hover over **Cut** to see its
   original GPX bounds. Editing video time moves both video boundaries together,
   preserving their gap; removing the row removes both boundaries.
   The map's current GPX marker previews valid table edits at the unchanged
   video time, so adjusting a cut's duration also updates your position farther
   along the route. Sliders and exports keep using the applied timing until you
   apply; cancelling restores the applied map position.
   Newly picked points apply automatically. For manual
   table edits, choose **Apply sync points** to commit the changes
   or **Cancel sync changes** to discard the draft. One landmark sets an offset;
   multiple landmarks interpolate timing between them to correct drift.
   After start alignment and cuts, use **Smoothing** to process only the retained data.
   Outside the landmarks, the nearest non-cut interval's rate is used
   (one GPX second per video second when there is no such interval).
   To skip a section already removed from the video, position the video and GPX
   at its start and press **Create cut**. The button stays selected as
   **Done create cut**, capturing both times and freezing the video. Move only
   the GPX to the end of the omitted section, then press **Done create cut**.
   Both boundaries apply together as one undoable edit: the second uses the
   chosen GPX end time and the captured video time plus one frame. **Cancel
   picking** discards the unfinished cut. While choosing the end, a thick red
   line previews the original route section to be removed; it clears when the
   cut is completed or cancelled. Invalid selections leave creation
   active for correction; a following playable video frame is required.
   The paired points must be consecutive in video order and advance in original
   GPX time. Applying the pair removes and levels the intervening GPX. The red
   marker holds the start location for one frame, then jumps to the end.
   Older unpaired boundaries remain editable as alignment landmarks;
   ordinary landmarks never infer removed sections.
4. The Syncing map retains the full imported route in grey, including removed
   sections; retained selections are highlighted in blue. **Fit track** fits
   the full original route. The **GPX seconds** slider under the map and the
   full-width video slider are linked through the applied sync mapping.
   Scrubbing either pauses playback and updates the other timeline, preview
   and marker. The video preview updates while dragging as frames finish decoding,
   then catches up to the final position after release.
   Each seek pans the map to center the red marker without changing
   its zoom, including when jumping over cuts or independently picking landmarks.
   Creating and applying landmarks or sync cuts also preserves the current zoom;
   use **Fit track** to explicitly fit the full route again.
   You can still drag to explore; the next seek recenters the marker.
   Scrolling the page keeps the route, sync pins and picking aligned with the
   map tiles, including when the map is only partly visible.
   Both have **-10 s / +10 s** buttons; video also has one-second
   and frame-interval stepping. GPX seeks clamp to shared video coverage.
   The GPX clock excludes cuts, while the video clock follows the prepared
   file; drift correction can make their rates differ.
   **Speed** beneath the map shows km/h during playback, calculated from the
   timestamped GPX and adjusted to the applied video synchronization. Cut jumps
   add no riding distance; held cut positions show zero. A fixed placeholder is
   shown when paused, picking landmarks, or outside GPX coverage.
   Playback uses one continuous FFmpeg decoder with a bounded frame buffer,
   presenting frames at the source's reported frame rate instead of repeatedly
   decoding isolated thumbnails. It remains silent; **Space** toggles it outside
   text fields. Variable-rate video is sampled by timestamp at its average rate,
   rather than treating source frame indices as time. Pausing, seeking, changing
   videos or leaving the page stops the decoder. Video files are never changed.
   Frame steps use the source's average interval, not exact
   variable-rate frame indices. While picking, only the GPX timeline moves;
   the two timelines become linked again after creating or cancelling the point.
   Manual offset controls are disabled while sync points are active.
   **Remove all sync points**, followed by **Apply sync points**, restores
   manual alignment and keeps only the manual cuts described below.
   The **Elevation profile** below the playback controls uses the same gradient
   colours, shading and axes as the smoothing page, without its original-elevation
   comparison overlay. Its middle
   follows the red GPX marker, with the current slope gradient (%) printed above
   the red vertical line, while its distance span matches the map's horizontal
   ground span at the current zoom. Seeking pans both views; zooming the map zooms
   the profile. Click the profile to scrub to a location, drag it horizontally
   to pan and seek, or use horizontal/Shift+mouse-wheel scrolling. These actions
   pause and seek the video through the applied alignment, drift correction and
   cuts. While picking a landmark, they move only the GPX and keep video frozen.
   Manually dragging the map away leaves the profile centered on the
   red dot. During landmark picking it shows the original GPX; otherwise it shows
   the smoothed, cut track. The frame stays in place outside GPX coverage.
   Timeline fields, map status and coverage hints reserve consistent space so
   scrubbing or loading a new preview cannot shift the sliders or other controls.
5. On **Delivery**, enter an optional country and new bundle-folder name, then **Export
   Incyclist bundle...**. The app copies the **entire video byte-for-byte**:
   codec, container, frame rate, audio and all other bytes stay unchanged.
   Its extension is preserved (`ride.mp4`, `ride.mov`, etc.). The GPX is
   aligned to the full video interval; export fails if GPX coverage is
   insufficient. With sync points applied, **Export coverage needs attention**
   shows the uncovered video ranges, missing seconds, and the first/last
   sync points and extrapolated timing rates responsible. Boundary landmarks
   are suggested automatically where they can be added without conflicting
   with existing landmarks or cut pairs. Choose **Review suggested boundary
   landmarks** to validate and stage them in the sync table, then verify the
   footage on **Syncing** and **Apply sync points** (or **Cancel sync changes**).
   Suggestions assume the first/last retained GPX location matches the video
   boundary; they are not automatic visual matches and never change the video.
   Automatic GPX export uses the same
   synchronized GPX, including its video-relative timestamps. Existing
   bundle folders are never overwritten. An export progress bar reports
   progress. **Also create a ZIP archive** (unchecked by default) additionally
   creates `<bundle-folder>.zip` beside the normal bundle directory. The ZIP
   contains the same three files and supports large videos without
   recompressing them; existing archives are never overwritten.
   GPX timestamps and cut markers use the same millisecond rounding, including
   when landmarks fall on fractional-frame times. Cut markers store total
   seconds (for example, `0:00:502.143`) to match Incyclist's exact timestamp
   lookup without floating-point rounding from hour/minute conversion.
   **Cancel export** prevents publishing a partial bundle.
6. Import **`ride.xml`** in Incyclist, keeping its referenced video and
   **`ride.gpx`** beside it. References use relative filenames.

### Manual cut and alignment fallback

Use the chart range and **Cut selected GPX section & level elevation** for
recorded-sample cuts on **Smoothing**, including untimed tracks/routes.
On **Syncing**, use **Create cut** to select an omitted original-GPX interval,
including stationary stops. The older manual GPX in/out controls and their
footer hints are no longer shown on Syncing; alignment controls remain available.

Successful manual cuts appear in the **GPX cuts** table only in the
**Smoothing** sidebar, not on Syncing.
Timed rows show **GPX time + Duration**, using the **original, uncut GPX clock**
with millisecond precision. Moving the start preserves duration; editing
duration moves the end. Smoothing has no map; use Syncing for visual alignment.
Revising an earlier row does not change later rows' reference times;
overlaps are combined. **Apply cut changes** rebuilds from the full smoothed
source; **Cancel cut changes** discards the draft. Smoothing before and after
cutting is retained. Untimed rows show starting distance and removed distance
in kilometres, relative to the sequential track before each cut.
Applied sync-derived cuts remain in the Sync points
table, separate from manual cuts. Manual cuts cannot remove a sync anchor.
Undo/redo restores applied tables and tracks together; opening a new GPX clears
both. Apply or cancel one table's draft before editing the other.

Without sync points, **GPX elapsed at video start** supplies the signed
alignment offset. A positive value means GPX started earlier. Alternatively,
enter the GPX time matching a video frame and press **Set alignment**.

At an interior GPX cut, the first elevation after the cut is matched to the
last retained elevation before it. The same constant shift is applied to
**all following elevations**, preserving uphill/downhill gradients within
each retained section. This handles both upward and downward elevation
gaps; absolute altitude is intentionally no longer geographically accurate.
Reset restores the retained original elevation profile with the same join
levelling, not the removed sections. Use **Undo** to restore a cut.

Cuts split GPX segments, excluding geographic jumps from riding distance.
Timed cuts interpolate boundary coordinates/elevations as necessary without
inventing sensor readings. Retained samples keep their sensor extensions.
Timestamps are rebased to 2000-01-01 and removed durations close up; the
preceding section normally ends one millisecond before an interior join to
preserve strict timestamp ordering. Synchronized exports instead preserve
explicit Before/After locations at their distinct video timestamps, inserting
landmark samples as needed. Export rejects millisecond timestamp collisions,
missing coverage, and jump endpoints without retained samples on both sides.
Cuts do not snap to video frames. Untimed
sample cuts do not invent timestamps.

Timed cutting/pairing requires **one GPX track** (multiple segments are
supported), with valid, strictly increasing timestamps on every point.
Malformed or unsupported timed recordings report an error rather than
silently leaving a time gap. Boundaries cannot be interpolated across
disconnected recording gaps. Pairing also needs moving samples at least
1 m apart and neighbors around geographic cuts. The XML contains geographic
cut markers for Incyclist's segment-flattening importer, a fresh UUID, the
source frame rate and measured frame count.

Save an editable `.gpxproj` project to preserve edits and alignment between
sessions; GPX/bundle export is separate. Closing or replacing a GPX warns about
unsaved work. Video replacement is undoable and retains applied sync points;
verify them against the new video. Replacements whose duration invalidates a
sync point are rejected without changing the current ride. This app does not
trim, re-encode or otherwise edit videos, and does not find matching landmarks
automatically. Check landmarks near both ends before riding.

### Undo and redo

The header's **Undo / Redo** buttons share one history for smoothing, GPX
cuts, applied sync points, reset, file replacements, metadata, alignment, cut bounds, export
settings, chart selection/options and map navigation/online-tile settings.
Continuous drags and text edits are grouped. Keyboard shortcuts are
**Ctrl+Z**, **Ctrl+Shift+Z** or **Ctrl+Y** (use **Cmd** on macOS), including
when an editing field is focused. New edits discard the redo branch.
History is in-memory and unavailable while a background task runs.
Playback, hover, clipboard operations and files already exported are not
reversed; undo never deletes or rewrites an exported file.

The pairing format follows the [Incyclist FAQ](https://incyclist.com/faq/#own-video),
its linked [video-production guide](https://cycling.vangestel.online/indoor/faq/video-production/index.html),
and the public Incyclist GPX/cut importer. Check resistance and landmark
alignment in your installed Incyclist version before a full trainer ride.

## OpenStreetMap

The map uses [OpenStreetMap](https://www.openstreetmap.org/copyright) raster
tiles through the native Walkers/egui widget. Only visible tiles are requested,
using an application-specific User-Agent and a persistent HTTP cache under
your platform's cache directory (`rlv-forge/osm-tiles`), respecting server
caching headers. No bulk downloading or offline map prefetch is performed.
The previous app's map cache is not reused; RLV Forge caches tiles as needed.
Keep the visible OpenStreetMap attribution when redistributing the app.

Tile requests reveal the viewed map area and your IP address to the tile
provider; the GPX document, timestamps, and sensor data are not uploaded.
Turn **Online tiles** off to stop tile downloads and keep just the local route
overlay. If tiles cannot load, the route remains visible and a warning with
a retry button appears. HTTP errors are also logged to the terminal.

See [DEV-PLAN.md](DEV-PLAN.md) for the implementation plan and acceptance criteria.
