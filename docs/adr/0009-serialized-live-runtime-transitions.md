# ADR 0009 Serialized live runtime transitions

Status: accepted for P1.2, 2026-09-25. Amends ADR 0005.

## Context

The live window restarts its runtime with **Restart runtime** after the browser
has stopped, and closes it with the window close button or Ctrl+Q. An audit of
`main` at `f947727` found that neither transition knew about the other:

- `restart` took the session, stopped it off the UI thread and started a new one
  afterwards, but started a runtime at once whenever it held no session. The
  button stayed visible during the restart because a missing session counted as
  stopped, so a second click started a second browser.
- Starting a runtime replaced the held session, so the second start dropped a
  running session on the UI thread, which waits until its browser has stopped.
- `request_close` let the window close at once when it held no session, also
  while a restart was stopping the previous runtime. The restart's continuation
  then started a browser after the close had been requested.
- Wake-ups, decoded frames and the restart continuation did not say which runtime
  they came from. A frame decoded for a stopped runtime would replace the new
  runtime's image and clear its decode flag.

Reproduced on `f947727` in a real X11 window (Xvfb, xdotool), the local fixture
and a private `TMPDIR`, after killing the owned browser so that Restart appears.
Each browser launch creates one profile directory, so profiles were counted:

| Action | Result on `f947727` |
| --- | --- |
| Restart once | 1 browser, 3 document requests (one per device) |
| Restart twice without delay | 2 browsers within 9–113 ms, in 4 of 4 runs; the extra one stopped by a drop on the UI thread |
| Restart, then Ctrl+Q | A browser launched 80–85 ms after Ctrl+Q and stopped while the window closed, 2 of 2 runs |
| Restart twice, then Ctrl+Q | 2 browsers, one launched after the other had been stopped for the close, 2 of 2 runs |

A stale frame was not observed. Restart only appears after the worker has
stopped the browser and cleared its frames, so no decode is in flight when a
person clicks it; in the double-Restart runs, the replaced runtime was stopped
before its first frame.

## Options

| Option | Assessment |
| --- | --- |
| Hide the button after the first click | Covers the double click, not close during a restart or late results |
| One transition at a time plus a runtime generation in the view | Covers restart, close and late results in one small state machine that tests can drive without GPUI |
| A restart operation in the engine | The engine would hold two runtimes during a restart; more API for the same guarantees |
| Runtime IDs on engine frames | The view takes each frame from one known session, so it already knows the source |

## Decision

- The live view runs at most one transition, Restart or Close, at a time. Restart
  is offered only while no transition runs and the runtime has stopped or failed
  to start. A click during a transition changes nothing; it is not queued.
- Each runtime belongs to a generation. Starting a restart or a close advances
  it. Wake-ups and decoded frames of an older generation are discarded; an old
  frame is released without touching the newer runtime's state.
- Close during a restart takes it over: the previous runtime finishes stopping
  off the UI thread, nothing starts, and the window closes afterwards, like a
  normal close. Nothing starts once a close has been requested.
- A runtime starts only when the view holds none, so a running session is never
  dropped on the UI thread.
- Restart keeps its contract: it restores configuration and loads the URL bar URL
  once per device, and replays no input. During the restart the device cards show
  no old frame or status and the status bar reads "Restarting…".
- The engine is unchanged.

## Consequences and limits

- A second click while a restart runs is ignored; after the new runtime has
  started, Restart appears again only if that runtime stops.
- Close during a restart waits for the previous browser to stop, which takes as
  long as a normal close.
- A stopped runtime's last frame stays visible, labelled stopped, until Restart.
- The restart continuation runs on the UI thread after the previous runtime has
  stopped; a slow browser exit delays the new start, not the UI.
- Restart is still offered only for a stopped runtime; restarting a running one
  is not supported.

## Validation

Unit tests drive the transition rules without GPUI: a second restart, close
during a restart, a second close and results of a replaced runtime.
`scripts/desktop-smoke.sh` adds two X11 runs: Restart clicked twice must start
exactly one browser, and with Ctrl+Q right after the clicks at most that one; both
must leave no browser process, profile or window. See `docs/validation.md` for
results.
