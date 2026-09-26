# ADR 0012 Live frame resources: atlas fix, off-screen pause and scale changes

Status: implemented for P1.4, 2026-09-26. Amends ADR 0005.

## Context

P1.4 asks for measured input latency, CPU and memory with three and eight devices,
startup, frame drops and long sessions, and for fixes to DPR/HiDPI, resize and
multi-monitor scale based on those measurements. The measurements ran in the cloud
container of the earlier milestones: Xvfb without a window manager, Mesa
Lavapipe (Vulkan on the CPU), four shared cores, Helium 0.18.1.1, release builds.
They are evidence for relative changes, not hardware budgets.

An audit of `main` at `e8d9944` found:

- **Typing while pages animate crashes release builds.** GPUI 0.2.2 creates an atlas
  texture when a draw needs one, including the draw that key dispatch runs without
  presenting. Broxser replaces every device frame with a new image, so the texture
  of an image painted in that draw can lose its last tile before the next present.
  `BladeAtlas::remove` then destroys it at once, but its initialization and uploads
  stay queued. The next frame unwraps the empty slot in `BladeRenderer::draw`
  (`blade_atlas.rs:229` for the initialization, `:239` for an upload), or copies
  into a later texture that reuses the index.
- **Every device streams, whether or not the canvas shows it.** The canvas is a
  scrolled, wrapping list of device cards. At the default 50% zoom in a 1360 × 860
  window, one of three example devices and five of eight devices are below the
  fold. Their browsers still encode 60 JPEG frames per second for an animated
  page, and the desktop still decodes them. GPUI's `paint_image` also uploads every
  painted image to its atlas before applying the content mask, so a frame clipped by
  the scroll area costs an upload per frame.
- **Frame limits ignore scale changes.** The desktop asks for frames no larger than
  they are displayed, in physical pixels (ADR 0005). It sends that limit at start
  and on zoom only. Moving the window to a display with another scale factor, which
  GPUI reports as a bounds change on Wayland, kept the old limit.
- **Frames never exceed the CSS size.** Headless Helium delivers screencast frames
  at the device's CSS size for DPR 1, 2 and 3 and for limits up to three times
  that size. Frames are sharp while zoom × window scale is at most 1 and upscaled
  above it, for example above 50% zoom on a 2× display.

Measurements and reproductions are in `docs/validation.md` (P1.4).

## Options

- Atlas: work around it in Broxser, for example by keeping every frame image until
  the next present, or fix GPUI. Keeping images alive would hide the race rather
  than remove it and doubles atlas memory for animated pages. GPUI 0.2.2 is the
  newest published release, so the fix goes into the vendored crate beside the two
  IME patches (ADR 0011).
- Off-screen devices: hide them (ADR 0006), lower their frame rate, or pause only
  their screencast. Hiding also drops input, sync and IME, which a device that
  merely scrolled out of view must keep. A lower rate still encodes and decodes
  frames nobody sees. Pausing the screencast keeps every other behavior.
- Scale: re-send the limits on every bounds change, or only when the scale factor
  changes. Limits depend on zoom and scale, not on the window size, so only a scale
  change needs them.
- HiDPI: `Emulation.setDeviceMetricsOverride` with `scale: 2` stopped all frames.
  Starting Helium with `--force-device-scale-factor=2` gave frames up to twice the
  CSS size for every DPR, with correct click coordinates and page DPR. It applies
  to every device and cannot change while the browser runs; at the same displayed
  size it raised browser CPU by about 64% and p95 latency by 30–40 ms. Keeping
  CSS-size frames costs sharpness only above zoom × scale 1.

## Decision

- Patch `vendor/gpui-0.2.2/src/platform/blade/blade_atlas.rs`: when `remove`
  destroys a texture, drop its pending initialization and uploads. Document it in
  `vendor/gpui-0.2.2/BROXSER-PATCH.md` and drop it with the other patches once an
  upstream release carries a fix.
- The canvas decides on each paint whether any part of a device frame lies inside
  the visible canvas (GPUI's content mask). Off-screen frames are neither uploaded
  nor painted. When that changes, the view sends `Command::SetOnScreen`, and the
  runtime stops or restarts that device's screencast. Restarting yields a fresh
  frame even for a static page. Nothing else changes: an off-screen device keeps
  its page running, takes input (a selected device scrolled out of view still gets
  keys), keeps its IME target and sync routes, and hiding or showing it does not
  restart its stream while it stays off screen. A command the runtime does not
  accept is sent again after a later paint; a new runtime streams every visible
  device until the first paint pauses those still off screen.
- The view re-sends frame limits when the window's scale factor differs from the
  one of the last limits.
- Frames stay at the CSS size. A sharper HiDPI mode, if wanted, is an explicit
  choice with its measured cost, qualified on a HiDPI display in a separate change.

## Consequences

- A device that scrolls back into view shows its previous frame until the new
  stream's first frame arrives, one browser round trip later.
- Pausing saves browser encoding, CDP transfer and desktop decoding for animated or
  busy pages; a static page produces no frames either way. With eight animated
  devices, five of them off screen, browser CPU fell from 170% to 97%, desktop PSS
  from 198 to 156 MB and p95 latency from 318 to 173 ms, and the visible devices
  showed about 38 instead of 24 frames per second.
- The atlas patch changes GPUI behavior only for textures destroyed before their
  first flush. It is covered by a real-window smoke run that types while every page
  animates; debug builds rarely reach the race, so that run needs a release build.

## Scope and limits

- Latency in the container stays above the 50 ms p95 target (System Design) in
  every measured scenario; it is dominated by software rendering and shared cores
  and is not a hardware result.
- A minimized or obscured window stops painting, so devices that were on screen
  keep streaming while it is.
- Wayland scale changes, fractional scaling, physical GPUs and multi-monitor
  setups were not available here. X11 fixes the scale factor at startup.

## Validation

- Engine: `live_off_screen_devices_pause_frames_keep_input_and_resume_fresh`
  (pause, input while paused, hide and show while off screen, change while paused,
  fresh frame on resume).
- Desktop: `scripts/desktop-smoke.sh` with a release build, including the run that
  types 1500 keys after the phone and tablet frames were seen animating in the
  window for 20 s. It fails when the browser or the fixture never gets that far.
- Before/after measurements with three and eight devices and a long session are in
  `docs/validation.md`.
