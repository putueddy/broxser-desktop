# ADR 0008 Deadlines for live commands and navigation

Status: accepted for P1.1, 2026-09-25. Amends ADR 0005.

## Context

An audit of `main` at `035b6fe` found live work without any expiry:

| Work | Bound before this ADR |
| --- | --- |
| Live setup commands (version, contexts, targets, emulation, bindings) | 15 s each; a timeout stops the runtime |
| `Page.navigate` and `Page.reload` from Go, Reload, sync and workspace open | None; kept in one table of 256 shared by all devices |
| Coalesced pointer move and wheel | None; one in flight per device, so an unanswered one blocks hover or scrolling for good |
| Pointer buttons and keys | None; fire-and-forget entries in one table of 256 shared by all devices |
| Screencast start, stop and acknowledgements; input ignore | None; the same shared table |
| UI command queue, frames, CDP events | Bounded: 512 commands, one frame per device, 1024 events |
| Capture waits | 15 s per command, 30 s per load; a job is bounded by their sum, not by a job deadline |

Reproducers with a fake CDP peer and with Helium 0.18.1.1, run before the change:

- A page that leaves input unanswered, in Helium a key handler that blocks the
  page's main thread for four seconds: 300 key events stopped the whole runtime
  within three seconds with `too many unanswered CDP commands`. A phone in another
  session lost its frames and a click.
- A navigation or reload whose request the server holds: 35 s later the device
  still showed `loading: true` without an error, and the request stayed open.
- A superseded navigation: its late `net::ERR_ABORTED` answer was shown as the
  failure of the newer navigation that was still running.

A probe against the same Helium showed that `Page.navigate` answers when the
navigation commits (24 ms here) and not at all while the server holds the
request, whereas `Page.reload` answers 4 ms after it starts. `Page.stopLoading`
is answered by the browser process within 3 ms and closes the held request.

## Options

| Option | Assessment |
| --- | --- |
| Report a navigation timeout but let it run | The status would contradict a page that commits later, and the held connection stays |
| Stop the navigation at its deadline | The device ends in a known state and keeps its current document; the user can load again explicitly |
| Queue input for a page that does not answer | Clicks and keys would reach the page seconds later, when the user no longer expects them |
| Drop input while the page does not answer | Nothing arrives late; the user sees why input has no effect |
| Keep one table for every device | One stuck page stops all devices, as reproduced |
| Limit each device | One page cannot use up another device's capacity |

## Decision

- Every live command belongs to a device or to the browser and has a deadline.
  Nothing is sent again when a deadline passes: a missing answer does not prove
  that an action did not run.
- **Navigation.** Each device follows at most one navigation that Broxser started
  (Go, Reload, sync, opening the workspace); a newer one replaces it, and the
  older one's answer is discarded. A navigate ends with its answer; a reload ends
  with the main frame's next commit, stop or same-document navigation after its
  answer. If it has not ended within the load limit (30 s), Broxser sends
  `Page.stopLoading`, like the Stop button, and the device reports "Navigation got
  no response within 30 seconds; loading stopped, not retried". Navigations that
  the page starts itself (links, forms, scripts) keep the browser's behavior.
- **Input.** A device may leave at most 32 input events unanswered: buttons, keys
  and the coalesced move and wheel. When it reaches that limit, or answers no
  input for the command limit (15 s) while some is outstanding, the device reports
  "The page is not responding to input". New input for it is then dropped, never
  queued or sent later, until the page answers again. A newly committed document,
  a crash or a detached target forgets the old input. An error already shown,
  such as an open dialog or a crash, stays. Explicit navigation remains possible.
- **Isolation.** The per-device limits add up to the runtime's table
  (8 devices × 33 commands), so one device can never fill it. Other devices keep
  their frames, input and navigation.
- **Browser.** Fire-and-forget commands that the browser process answers itself
  (screencast start, stop and acknowledgements, input ignore) must be answered
  within 15 s. Otherwise the runtime stops with an explicit error and cleans up.
- Answers that the engine no longer waits for are discarded on arrival, so
  abandoned commands leave no state behind.

## Consequences and limits

- Input sent before a page is found unresponsive, at most 32 events, still
  reaches the page if it recovers, as in a browser. A dropped key or button
  release can leave that key or button held in the page until it is pressed again.
- Chromium can run devices of one session and site in one renderer process, so
  a page that blocks its main thread can stall its same-session peers too; each
  device is reported separately. Different sessions use separate BrowserContexts
  and renderer processes.
- A page-initiated navigation that hangs is not stopped; the desktop has no Stop
  button yet. A committed page that keeps loading stays interactive; live mode
  sets no load deadline, while capture keeps its 30 s load limit.
- A dev server that needs more than 30 s before it answers the first request is
  stopped; Go or Reload loads it again. The limit is not configurable yet.
- A browser that stops answering is noticed only while a command is outstanding;
  there is no heartbeat.
- Capture keeps per-operation limits and no job deadline.

## Validation

Default tests use a fake CDP peer: a flood of unanswered input on one device
leaves the others running and drops input beyond the limit until the page
answers; a single unanswered click is reported after the command limit; a
navigation and a reload without an answer are stopped and not retried; a
superseded navigation's late answer does not describe the new one. Helium tests
hold a reload at the fixture and block a page's main thread. See
`docs/validation.md` for results.
