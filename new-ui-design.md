# Re-design the UI and the workflow

The current UI design and the workflow is not as optimal as I think it can be.

## Review status

Implementation was authorized after review and is on branch `ui/new-design`.

## Overall UI

The app has 3 main features:
1 Syncing a prepared video to a working copy of a GPX or FIT track, trimming unwanted data first
2 Smoothing the retained GPX after synchronization
3 Exporting it as a Incyclist bundle

### Change 1

Add a main menu which shall have:
* File
  * New project
  * Open project...
  * Save project
  * Save as project...
  * Exit
* Edit
  * Undo
  * Redo

The menu(s) shall replace buttons: Open project, Save project, Save project as, Undo, Redo

### Change 2

* I'd like the 3 features to exists in 3 different pages on the main canvas in the app.
* Switching between the pages are done by clicking on corresonding tabs placed on the top in the app. They shall be named:
  * Syncing
  * Smoothing
  * Delivery

The tabs shall replace the buttons GPX Smoothing, Video and Incyslist

Use a proper connected tab strip, not a row of selector buttons. The active
tab joins the page canvas, with rounded top corners and a visible inactive-tab
border. Support keyboard navigation with Left/Right arrows and Home/End.

All tabs remain accessible regardless of which inputs have been loaded. Show
guidance for missing inputs rather than preventing navigation.
New projects and imported GPX/FIT files start on Syncing. Both Syncing and
Smoothing expose **Edit > Open GPX/FIT...**; Syncing also exposes
**Open prepared video...**. Import does not run smoothing. Work only on the
in-memory/generated GPX copy, never overwrite the imported GPX/FIT source.

Project saves preserve the selected tab and the ZIP export option. Older
projects still open, mapping their former video-page selection to Syncing.
Leaving Syncing pauses playback.

### Numeric drag editing

Use exponential, position-based dragging for all draggable numeric fields.
Keep each field's existing sensitivity near the initial click, doubling
sensitivity every 50 logical pixels away from it. Right/up increases the
value; left/down decreases it, preserving the existing drag directions.
Holding the pointer still does not continue changing the value. Moving back
to the initial click restores the starting value, including after hitting
a field's limit. Preserve typed entry, decimal formatting, and value limits.
Hold Shift when starting a drag for 10x finer adjustment. Keep that precision
mode fixed until release so changing Shift mid-drag cannot jump the value or
break returning to the starting value.

## Smoothing

Simplified layout - save pixel real estate.

### Change 3

When tab Smoothing is selected, the main menu shall change to reflect the selection.
* Add to edit menu: Open GPX/FIT...
Remove the corresponding buttons.

Rename Auto Smooth to Smooth, and move it under the elevation chart.
Move both checkboxes, "Incyclist: soften grade transitions" and "Show original",
under the elevation chart and make both checked by default.

Do not show a map on Smoothing. This supersedes the earlier taller-map and
cut-endpoint map-focus designs. Keep the elevation charts, range controls,
smoothing controls, and cuts sidebar. The map belongs on Syncing, where it
keeps its video-matched dimensions.

Move the GPX cuts list into a sidebar to the right. Show this list only on
Smoothing, never on Syncing (with or without a prepared video loaded).
Use **GPX time + Duration** for timed cut rows, matching the Syncing list's
start-plus-duration editing with millisecond precision. GPX time is the start on
the original, uncut GPX clock; duration is the removed interval. Changing the
start preserves duration, and changing duration moves the end. Untimed cuts
retain their sequential-distance basis, displayed as distance plus duration
in kilometres.

Keep Apply/Cancel semantics. Applying cuts on Smoothing must not reset the
shared map zoom when returning to Syncing.

End the Smoothing page at the Reset The Data button. Remove everything below
it: the automatic-export explanatory label, file name, GPX name, description,
and optional GPX text preview / clipboard controls. Keep automatic GPX export
and preserve imported or saved project metadata internally.

Reset The Data re-reads the original GPX or FIT file from disk and replaces the
entire working track, including its coordinates, timestamps, elevations, and
metadata. Clear cuts, sync points, drafts, and alignment, then select the full
reloaded ride and automatically export it. Keep the current project save path
and prepared video reference. The reset is undoable. If the source is missing
or invalid, show an error and retain the current working data; do not silently
fall back to the embedded track.

Remove the Export GPX button. Automatically export beside the source file as
`<source-name>-smoothed.gpx`, using existing naming conventions:
* Export immediately after import and after every completed data edit, including
  cuts, undo, and redo.
* Replace the generated file automatically, but never overwrite the source file.
* View-only changes do not trigger an export.
* Briefly debounce consecutive edits so typing and dragging do not repeatedly
  write intermediate results. Imports and completed background edits queue an
  export immediately.
* Report write failures visibly without losing edits or preventing project
  saving. If the original folder no longer exists, automatic export cannot
  write there; the embedded track remains editable in the project.


## Syncing

Simplified layout - save pixel real estate.

### Change 4

Import the GPX/FIT working copy here before smoothing. Synchronization and
trimming come first; Smoothing then operates on the retained track.

After track edits on Smoothing, retain existing sync points and revalidate them
against the updated track rather than resetting them.

When tab Syncing is selected, the main menu shall change to reflect the selection.
* Add to edit menu: Open prepared video...
Remove the corresponding buttons.

Move the video controls under the video limiting the width to the width of the video frame.
Same applies to the controls of the map frame.

Move the sync points list into a sidebar to the right.

### Match the start of the video first

Use **Set video start** before further landmarks, cuts, or smoothing:
* Seek and freeze the prepared video at 0:00. Wait for that frame to load before
  allowing confirmation. Move only the GPX marker using the original-route map
  or timeline, and choose the matching occurrence on repeated route locations.
* Show the leading GPX section to be removed as a thick red preview.
* **Confirm video start** trims everything before the selected GPX position
  from the working copy and anchors video zero to that position. Apply both
  effects atomically as one undoable edit, preserving map zoom.
* **Cancel picking** discards the selection without trimming or changing sync.
  Invalid endpoints or conflicts retain the selection and report an error.
  Keep and revalidate existing later landmarks/cuts; do not silently discard them.
* Display the result as a **Video start** row with video seconds fixed at zero
  and editable original-GPX seconds. Its prefix trim belongs to that row, so
  revising the start earlier can restore earlier samples from the source copy.
  Removing the row and applying removes its prefix trim.
* Keep original source coordinates, timestamps, and elevations available for
  undo and revision. Never modify the original GPX/FIT file. Smooth only the
  retained working track, after start alignment and unwanted-data removal.

### Match the end of the video

Add **Set video end** alongside Set video start, using the same two-click flow:
* Seek and freeze the final playable video frame (duration minus one reported
  frame interval). Wait for its preview before allowing confirmation.
  Keep the GPX position that was current before the video seek, translated to
  the original GPX clock through the applied alignment and cuts. Do not default
  to the track end or a previously saved Video end position.
* Move only the GPX position to match that frame. Preview the trailing GPX
  section to be removed as a thick red line.
* **Confirm video end** anchors that frame to the chosen original-GPX time and
  trims all later data from the working copy. Apply both effects in one
  undoable edit and preserve map zoom. Never change the original GPX/FIT.
* **Cancel picking** discards the selection. Invalid positions or conflicting
  landmarks/cuts keep the operation active and report an error.
* Show a **Video end** row with fixed video time and editable original-GPX
  seconds. Revising it can restore or remove more trailing data from the
  source copy; removing it and applying removes its tail trim.
* Hold the chosen endpoint during the final frame's remaining display time.
  Export one stationary endpoint sample at the video's full duration, so
  frame-accurate matching still provides complete GPX/video coverage without
  retaining extra route data.

Remove **Remove a section from the smoothed GPX** and all UI below it from
Syncing: manual GPX in/out fields, video-time copy buttons, the manual cut action,
associated hints, duration-mismatch warning, and footer status. Keep the alignment
controls above it. Show errors and loading indicators above the workspace instead.
Manual cuts remain available on Smoothing; use **Create cut** for sync cuts.

### Creating and listing sync cuts

Keep **Pick Landmark** for ordinary alignment points. Replace the separate
before/after cut buttons with one **Create cut** button.

Preserve the current map zoom throughout landmark picking and cut creation,
including when their synchronization points are applied. Recenter to follow
the GPX marker as usual, but do not automatically refit the route when a cut
changes its retained distance. **Fit track** still explicitly refits the map.

* First press captures the current video time and original GPX time. Keep the
  button selected and change its label to **Done create cut**.
* Freeze video playback and seeking while the user moves the GPX position to
  the end of the omitted section. Highlight the pending omitted route from the
  captured start to the current GPX end as a thick red line on the map, updating
  as the GPX position moves. Follow the original route, including interpolated
  boundaries, without drawing across disconnected segments. Clear this preview
  on completion or cancellation; it does not change the applied track.
* Second press uses that GPX end time and the captured video time plus one
  frame (using the prepared video's reported frame rate). Apply both boundaries
  together as one undoable edit, then release the button.
* **Cancel picking** discards the unfinished cut without adding any boundaries.
  An invalid end position or conflicting sync point shows an error and leaves
  cut creation active for correction. The end must be later in the original
  GPX timeline and both ends must be recorded route locations.
* Do not start a cut if there is no following playable video frame.

Show each complete cut as **one row** in the sync points list: **Cut**,
**Video seconds**, and **Duration**. Duration is the original GPX time removed
(end minus start), not the one-frame video interval. Editing duration keeps the
original GPX start fixed and changes the end; show the original GPX bounds in
the cut's tooltip. Editing video time moves both video boundaries together,
preserving their gap. Seek goes to the cut's first video boundary; Remove
removes the whole pair. Table edits still require **Apply sync points**, or
can be discarded using **Cancel sync changes**. While post-editing cut duration,
preview the current GPX marker using the draft synchronization at the current
video time. Keep the video playhead unchanged, including when it has moved past
the cut. Valid sync point edits update this map preview immediately; show an
error for invalid edits. Sliders and exports continue using the applied mapping.
Cancel restores the applied marker position; Apply commits the previewed timing.
Ordinary landmarks keep their
original-GPX time field. Preserve older unpaired boundaries as individually
editable alignment points. Number map pins by displayed row, with the cut's
second boundary labelled as that row's end.

Both sidebars are resizable and scroll independently of the main canvas.

Move everything related to exporting to incyclist bundle to tab Delivery

## Delivery

Use the current UI for exporting the bundle.

### Change 5
* Add a progress bar while exporting.
* Add an unchecked-by-default checkbox enabling creation of a ZIP archive of the
  bundle in addition to the normal bundle directory.
* Name the archive `<bundle-folder>.zip` beside the bundle directory. Include
  the same video, `ride.gpx`, and `ride.xml` at the archive root. Store the files
  without recompressing the video and support large-video ZIP64 archives.
* Never overwrite an existing bundle directory or ZIP archive. Cancellation or
  failure must not leave a partially published bundle.
* Include video copying and optional ZIP creation in export progress, and retain
  the Cancel export action.
