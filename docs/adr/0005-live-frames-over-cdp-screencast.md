# ADR 0005 Live frames over CDP screencast

Status: accepted for the M1 spike, 2026-09-24. Revisit before the M2 pilot.

## Context

ADR 0001 chose a GPUI shell with an external Helium process and required a spike
before committing to interaction. M1 asks for pages that update without a
capture button, a URL bar, input to a selected device and opt-in sync, while
blocking browser I/O stays off the GPUI thread and nothing leaks on close.

## Decision

- `broxser-engine::LiveSession` owns one headless Helium per open workspace on a
  worker thread. The UI sends commands through a bounded queue and reads a status
  snapshot and the latest frame per device; it never waits on CDP.
- Frames come from `Page.startScreencast` as JPEG. Each device keeps at most one
  unread frame; a newer frame replaces it. Every frame is acknowledged on
  receipt, including replaced and hidden-device frames, so streams never stall.
  Hidden devices stop their screencast. Frame size is capped at the displayed size.
- The desktop decodes frames on GPUI's background executor, one per device at a
  time, and drops the previous image from GPUI's sprite atlas when it replaces it.
- Input uses `Input.dispatchMouseEvent` and `Input.dispatchKeyEvent` in CSS pixels,
  mapped from the painted frame bounds. Pointer moves and wheel deltas are
  coalesced with one dispatch in flight per device.
- Sync reuses the core router and stays inside one session. A link navigation
  synchronizes only right after a forwarded click or key on that device; scripted
  navigations, forms, typing and pointer events never synchronize. Wheel deltas
  are mirrored at the destination's center and dropped after it navigates.
- Crashes, detached targets, browser exit and transport errors end in explicit
  states. A restart is a user action that restores configuration and loads the
  URL once; it never replays clicks, typing or navigations.
- Closing the window waits for the worker to stop the browser and delete its
  profile, because GPUI 0.2.2 ends the process when its last Linux window closes.

This is frame streaming into a native window, not an embedded browser surface.

## Evidence and limits

Engine live tests pass on Helium 0.18.1.1 and Chromium 141: frames without a
capture action, clicks at DPR2 and DPR1 reaching the right device, wheel and
keys, sync within a session without loops or replay after restart, bounded
frames and hidden-device pauses, renderer crash with explicit reload, and browser
exit with cleanup. An X11 window check covered the same flows at 38%, 50% and 75%
canvas scale. See `docs/validation.md` for numbers.

- Headless screencasts deliver emulated DPR>1 viewports at CSS resolution; the
  360×640 DPR2 test device streamed 360×640 frames, not 720×1280. Live frames are
  therefore softer on HiDPI displays. Static capture keeps physical pixels.
- A renderer crash is reported only once the renderer process has exited. Where
  a crash collector receives core dumps, the kernel streamed a 20+ GB dump first
  and held the crash report for more than 45 s; Broxser therefore starts the
  browser with a zero core limit and `coredump_filter` (see `SECURITY.md`).
- Only the latest frame is shown; animation smoothness and input latency have not
  been measured on real hardware.
- IME, page clipboard, touch gestures, drag and drop, popups, downloads,
  permissions, file upload, JavaScript dialogs and accessibility are not
  supported. Each needs its own gate.

## Options for later

If frame quality or latency fails the pilot: request frames at physical size via
`Page.captureScreenshot` bursts for DPR>1 devices, move to a pipe transport with
shared memory, or evaluate a hostable Chromium surface (CEF or Content) in a new
ADR. A Helium fork stays out of scope without such evidence.
