# Validation evidence

Evidence per milestone. It is not production qualification or a claim of Sizzy
parity. Keep failed, skipped and manual-only results visible.

## PR #24 review corrections, 28 September 2026 (Linux X11)

Review of `e8969de` found three P2 and three P3 issues, corrected together:

| Finding | Evidence and correction |
| --- | --- |
| P2: an off-screen selected device's console stays cached after Hide selects another | In an X11 window, scrolling the phone off screen then hiding it changed the panel heading to Tablet but kept the phone's `390` messages. Hide now refreshes the console when selection changes, without waiting for a changed status. The new smoke regression failed on the original PR and passed with the fix. |
| P2: Clear fails after the browser exits | The console survived the worker, but Clear went through its disconnected command queue. Clear now updates local memory and publishes counts/revision under the same lock order as incoming entries. Tests cover a stopped worker, concurrent Clear/push, callback reads and preserving other devices. |
| P2: newly enabled iframe Runtime retains logged objects past both message rings | On pinned Helium, a cross-site iframe logged a WeakRef-only object with a roughly 512 KiB array, then 1,101 strings. Forced GC collected the object before the PR, but the PR retained it until the page cleared its console. Runtime's debugger bindings outlive ring eviction. The fix releases the `console` object group in batches, at most one outstanding request per active session and one per 100 ms. Fake CDP tests cover coalescing, idle startup, timeout retries and retirement; the new live test collects the object without clearing browser history. |
| P3: missing frame identity looks like a main-frame log | A same-origin iframe's console call was marked as a frame, while its image 404 was not. Page-session Log events lack reliable frame identity. `ConsoleScope::Unknown` and the panel's `frame unknown` tag now expose that limit; Runtime contexts and iframe sessions still identify known scopes. |
| P3: truncation exceeds the advertised bounds | The ellipsis made text 1,001 characters; line/column suffixes exceeded the location cap. Both now count toward their respective 1,000/300-character limits, including Unicode text and maximal coordinates. |
| P3: Unicode separators and bidi controls bypass one-line sanitization | U+2028/U+2029 become spaces and directional formatting controls are removed; script joiners remain. Unit tests cover the separators, bidi overrides and a joined emoji. |

The focused engine run passed 11 console tests; both live Helium console tests
passed, including the new object-retention regression. The new X11 scenario
passed after failing on the original PR: Hide switches from the phone error to
the tablet warning, Clear works after browser exit, and another device's retained
messages remain. It left no browser process, profile or window.

`bash scripts/check.sh` passed: formatting, strict Clippy, 1 CLI, 12 core,
153 engine and 42 desktop tests (53 live tests ignored by default).
The complete Helium suite passed 53/53 in 231.9 s with four test threads,
while the X11 smoke suite ran concurrently.
The full `scripts/desktop-smoke.sh` passed all 17 scenarios, including the
new selection/stopped-runtime console regression; no browser process, profile
or window remained.

All browser tests use application-owned private profiles and the enabled sandbox.
GUI evidence uses a debug desktop, Xvfb 1600 × 1000 and Mesa Lavapipe. Physical GPU,
Wayland and full console-panel performance qualification remain outside this run.
The browser's own console history and the page heap are separate from Broxser's
bounded text ring; releasing debugger bindings does not erase that history.

## P2.3 device console, 27 September 2026 (cloud container)

Same container: Helium 0.18.1.1 (Chrome/154.0.8037.57) run by `broxsertest`
with the sandbox enabled, Xvfb 1600 × 1000, debug builds. Decisions are in
[ADR 0023](adr/0023-device-console-in-memory.md). This is the first part of
P2.3; bug-report export is not implemented.

### Before the change

- Code: every device session ran `Runtime.enable` (for the link and IME
  observers), so the browser already sent each console call and uncaught
  exception to the live worker, whose event loop matched neither and dropped
  them. No session enabled `Log`, and iframe sessions (ADR 0016) enabled
  neither. The browser's stdout and stderr are `/dev/null`. Nothing reached
  the UI, a log or a file.
- A scratch CDP probe (Node, a Broxser-like context, page target and
  recursive iframe auto-attach) on a fixture that logs every console type,
  throws, rejects, loads a missing image and script, fetches a 500 and a
  refused port, starts a worker and embeds a same-origin and a cross-site
  iframe received 20 `Runtime.consoleAPICalled` and 4 `Runtime.exceptionThrown`
  events: `warning` and `error` types, `startGroup`/`endGroup`, `count`
  labels, argument previews (`Object` with 2 properties, `Array(2)`, a `body`
  node) and the calling line; a 100 000-character string as one 100 345-byte
  event; same-origin frame messages on the page session in another execution
  context; cross-site iframe messages only on the iframe's session once
  `Runtime` was enabled there; nothing from the worker. The five failed
  requests (image, script, 500, `ERR_UNSAFE_PORT`, favicon) arrived only as
  `Log.entryAdded` (source `network`, level `error`), replayed when `Log` was
  enabled late; `Runtime.disable` and `enable` replayed all 22 messages.
- Flood: a page logging 100 000 messages took 2.5 s in its loop with
  `Runtime` enabled (1.3 s without) and sent 41.7 MB of events. In a scratch
  live test of three devices (debug build) whose phone page did the same, the
  desktop device's frames went from 60 per second to 5, 1, 1, 1 and 17 over
  the next seconds, with gaps up to 1.1 s, then back to 60; the runtime kept
  running. Keeping entries does not change that cost, which the events cause.

### After the change

- Each device's console holds its page's console calls (formatted on one
  line), uncaught errors and rejections, failed requests and other browser
  messages, from the page and its cross-site iframes (marked "frame"), plus a
  navigation entry per document; newest 200 entries, 1000 characters each,
  repeats counted, error and warning counts in the device status.
- In the fixture run each device showed its own `boom on 360` (600, 1000),
  `careful on …` as a warning, `plain {w: …}`, `Uncaught Error: uncaught on …`,
  `Uncaught (in promise) Error: rejected on …`, the image's 404 at
  `…/missing.png`, and the cross-site frame's `frame error on …` and
  `Uncaught Error: frame uncaught on …` located at
  `http://localhost:<port>/console-frame:<line>:<column>` without the query;
  never another device's width.
- The desktop card of each device on the smoke fixture shows "1 error ·
  1 warning · Console"; clicking it opens the Console panel for that device
  with the warning, the error (both at `console.html:<line>:<column>`) and the
  navigation, newest first; Clear empties that device only; `Ctrl+Shift+J`
  and the toolbar toggle open and close it. An earlier fixture that signalled
  with a missing address and had no icon link showed "3 errors" there: the
  404s of the signal and of the favicon, which can arrive after Clear, so the
  fixture now declares an inline icon and signals with an existing file.

### Checks

| Check | Result |
| --- | --- |
| Engine unit tests | `console_calls_become_bounded_one_line_entries`, `exceptions_and_log_entries_name_what_failed_and_where`, `locations_keep_only_scheme_host_and_path`, `repeats_collapse_and_old_entries_leave_while_counts_stay` passed |
| Fake CDP `device_consoles_are_bounded_attributed_and_cleared` | Passed: `Runtime.enable` and `Log.enable` on every device session; an iframe session's setup sends `Runtime.enable` and `Log.enable` before `Runtime.runIfWaitingForDebugger`; repeats collapse (×2), a same-process frame's warning is a subframe entry, Broxser's link world adds nothing, an unknown session reaches no device, 5000 characters become 1000 plus "…", the navigation entry drops query and fragment, Clear empties the phone only, 250 errors keep 200 entries and count 250 |
| Helium `live_console_keeps_each_devices_errors` | Passed 6 of 6 alone after two test fixes: the fixture first answered the "missing" image with 200, so no 404 was reported, and a favicon 404 can arrive after Clear, so the test now allows only that entry afterwards |
| `bash scripts/check.sh` | Passed in 29 s: fmt, `cargo test --locked` (1 CLI, 10 core, 103 engine, 37 desktop), strict Clippy for the workspace and the desktop crate |
| Live Helium suite (`--ignored`, 4 threads, `broxsertest`, sandbox on) | 46 of 46 passed in 77 s, the owner's iframe regressions included with the two new iframe setup steps |
| New smoke run "console panel counts and clears one device", alone and in the full run | Passed: the phone card's filled count appeared, a click on it opened the panel (its Clear found at the panel's top right), Clear removed the phone's count and kept the tablet's, `Ctrl+Shift+J` closed the panel and the phone frame returned |
| Full `scripts/desktop-smoke.sh` (Xvfb 1600 × 1000, debug build) | 14 of 14 scenarios passed in 1 m 29 s; no browser process, profile or window left |
| Real window, screenshots | With the final fixture: the cards with their counts, the panel with the phone's three entries (warning, error, navigation), the panel after Clear ("No errors or warnings", "No console messages.", the tablet's count still shown) and the canvas after `Ctrl+Shift+J` were captured with `xwd` and inspected |
| Rerun after the cherry-pick onto `main` (`5972c48`, PR #23 merged with the owner's review corrections and the stale-input follow-up; `live_view.rs`, `scripts/desktop-smoke.sh`, System Design and this file merged with both sides kept) | `check.sh` passed in 62 s: 1 CLI, 12 core, 147 engine and 42 desktop tests, 52 live tests ignored by default. Live Helium suite 52 of 52 in 85.4 s. The first full smoke failed in "input right after Apply removed a device": right after `Ctrl+Shift+W` it looked for Remove before the panel was drawn and found a card's error count, which has the danger color and lay in the Remove column, so Save was clicked instead of Apply and the desktop did not restart. That run now waits for the panel's Add column first: it passed 3 of 3 alone, a build without the `pointer` guard still panicked in it (2 of 2), and the full smoke passed 16 of 16 in 1 m 43 s with no browser process, profile or window left |

### Limits

- Dedicated, shared and service workers are not attached; their messages and
  errors are not shown.
- Text is what the page logged and may contain secrets the page printed;
  only locations lose query strings and user information.
- A console flood still delays other devices' frames while its events arrive
  (measured above); there is no rate limit in the browser to use.
- `Runtime.exceptionRevoked` (a rejection handled later) does not remove the
  earlier entry, and `console.clear()` is ignored by design.
- The Console panel renders every kept entry each frame while open; with 200
  long entries this was not measured for frame time.
- Export is the second part below.

## P2.3 bug reports, 27 September 2026 (cloud container)

Same container and builds. Decision:
[ADR 0024](adr/0024-bug-reports-on-request.md).

### Before the change

- The live desktop saved nothing. A report meant a system screenshot of the
  scaled canvas, where the screencast of a 390 × 844 phone at 50 % arrives at
  about 195 × 422 pixels (ADR 0012), and console text copied by hand from the
  panel, tokens included.
- `broxser capture` wrote a PNG per device and a JSON report of browser and
  frame sizes, without console output or page address.

### After the change

- Save report in the Console panel wrote, for the phone of the smoke fixture
  opened at `console.html?token=abc#frag`, a folder
  `2026-09-27-23-03-15-phone` (mode 0700) holding `screenshot.png` (390 × 844,
  2743 bytes) and `report.md` (969 bytes, both mode 0600), and the panel showed
  "Saved to" with the folder. The report:

      # Broxser bug report

      - Device: Phone (390 × 844 CSS px at 1×, mobile, touch)
      - Session: Guest
      - Page: http://127.0.0.1:48225/console.html
      - Browser: Chrome/154.0.8037.57, CDP 1.3, headless Helium (ADR 0019)
      - Broxser: 0.1.0
      - Saved: 2026-09-27 23:03:15 UTC
      - Screenshot: screenshot.png, 390 × 844 px
      …
      ## Console: 2 error(s), 1 warning(s), oldest first

          Info    Navigated to http://127.0.0.1:48225/console.html
          Error   smoke error on 390 · http://127.0.0.1:48225/console.html:17:11
          Warning smoke warning on 390 · http://127.0.0.1:48225/console.html:18:11
          Error   failed at http://127.0.0.1:48225/console.html · http://127.0.0.1:48225/console.html:20:11

  The page's address and the logged `location.href` lost `?token=abc#frag`;
  the panel itself still shows the message as the page logged it.
- Screenshots of the three test devices were 720 × 1280 (360 × 640 at 2×),
  600 × 800 and 1000 × 700 PNGs that decode.

### Checks

| Check | Result |
| --- | --- |
| Core redaction tests | `addresses_keep_only_scheme_host_and_path`, `text_loses_address_secrets_and_tokens_only` passed |
| Desktop report tests | `a_report_names_the_device_and_keeps_secrets_out`, `reports_go_to_new_private_folders_in_the_reports_directory`, `the_reports_directory_follows_the_override_then_the_download_directory` (a `user-dirs.dirs` with `$HOME/Unduhan`, and a disabled one), `times_are_utc_calendar_dates` passed |
| Fake CDP `screenshots_are_taken_on_request_checked_and_bounded` | Passed: PNG with its token and `captureBeyondViewport: false`; a non-PNG reply, a second request in flight, no reply within the command limit (1 s here), a hidden device and an open dialog each end with their reason; the held device is not "not responding" |
| Helium `live_screenshots_show_each_viewport_at_its_scale` | Passed 3 of 3 alone |
| `bash scripts/check.sh` | Passed in 37 s: fmt, `cargo test --locked` (1 CLI, 2 + 10 core, 107 engine, 41 desktop), strict Clippy for the workspace and the desktop crate |
| Live Helium suite (`--ignored`, 4 threads, `broxsertest`) | 48 of 48 passed in 76 s |
| New smoke run "console panel saves a redacted report" | Passed alone twice and in the full run: `Ctrl+Shift+J`, Save report, a 390 × 844 PNG and a report naming the phone and its error, without `smoke-secret` or `smoke-fragment`, modes 0700 and 0600 |
| Full `scripts/desktop-smoke.sh` (Xvfb 1600 × 1000, debug build) | 15 of 15 scenarios passed in 1 m 24 s |
| Rerun after the cherry-pick onto `main` (`66f3d1e`, PR #24 merged with the owner's corrections `965cfe4`; eight files merged with both sides kept: report lines tag entries by `ConsoleScope` like the panel (*frame*, *frame unknown*); `location` keeps the owner's bound on line and column and its empty result for an address without a valid scheme, now through `broxser_core::redact_url`; the smoke keeps `console_selection_run` and `report_run`, which now waits for a visible window) | `check.sh` passed in 52 s: 1 CLI, 14 core, 154 engine and 46 desktop tests, 54 live tests ignored by default. Live Helium suite 54 of 54 in 90.2 s. The owner's `console_selection_run` failed 2 of 2 in this container on its own commit: the phone's small "Error" label had 19 pixels within 8 of its color, one short of `find_color`'s 20, and at that tolerance a stale label would also have counted as absent. The run now uses 32 for presence and absence (39 pixels present, 7 after Hide): it passed 3 of 3, and a build without the owner's Hide fix failed it 2 of 2 with "kept the phone's console". Full smoke 18 of 18 in 1 m 54 s; no browser process, profile or window left |

Smoke findings on the way: the first report run exited the script under
`set -e` because `find` failed on the not yet created reports folder inside a
pipeline, which left the desktop running and its browser writing into a work
directory the script then removed (the guardian reported "remove the emptied
guarded profile: Directory not empty"; the folder was removed by hand). And a
first search for the accent-filled Save report could match the selected card's
accent border before the panel had opened; the run now waits for the panel's
danger-filled Clear first. One console run found no count on the phone card
within 15 s once and passed in the five runs after; it was not reproduced.

### Limits

- Redaction covers addresses, JWT-shaped tokens and bearer credentials only;
  other secrets a page prints reach the report, as it says.
- The screenshot shows the page as it is, personal data included.
- One device per report; the report folder is not opened for the user.
- The reports directory comes from the environment and `user-dirs.dirs`, not
  from a file dialog.

## PR #23 follow-up: input right after Apply, 28 September 2026 (cloud container)

Helium 0.18.1.1 run by `broxsertest` with its sandbox enabled, debug desktop,
Xvfb 1600 × 1000, on top of the review corrections below (`9d199c6`).

- Before: GPUI delivers input to the listeners of the last drawn frame, and
  Apply replaces the device list at once. On `9d199c6`, a pointer move over
  the third of three frames right after an Apply that removed the first device
  ended the desktop with exit 101, `index out of bounds: the len is 2 but the
  index is 2` in `pointer`; a second click instead ended it in
  `release_outside` (3 of 3 runs each). The events must arrive before the
  next redraw, at most one refresh later: xdotool's `click` sleeps after its
  release, so the reproducer presses and releases separately. A person rarely
  produces a second event that soon after a click; a bouncing button, a tap
  or automation can.
- Change: `pointer`, `release_outside`, `wheel` and `toggle_hidden` look their
  device up and ignore a missing index. Popup and download Dismiss act only
  while their report is still shown, since popup tokens and download counts
  start over with each runtime. By code reading, a stale event also reaches
  no page: the rebuilt devices have no bounds until drawn, and the stopping
  runtime takes no commands.
- New smoke run "input right after Apply removed a device": three 360 × 640
  devices, Remove the first, then Apply followed at once by a move, a wheel
  step and a click in the third frame, and a click on the sidebar's third
  Hide. It fails on `9d199c6` (exit 101, panic in `pointer`) and passes with
  the fix. Builds that each left out one guard panicked in `wheel`,
  `release_outside` and `toggle_hidden` respectively, 3 of 3 runs each, so
  every path is reached before the redraw.
- A scratch run opened a popup from the third device and clicked its report's
  Dismiss right after Apply: a build without that guard panicked in the
  Dismiss handler, the fix kept running and exited 0. A normal popup Dismiss
  still hides its report. Screenshots were inspected and not committed.
- `bash scripts/check.sh` passed in 40 s: 1 CLI, 12 core, 142 engine and 41
  desktop tests, formatting and strict Clippy. `scripts/desktop-smoke.sh`
  passed all 15 scenarios in 1 m 42 s with no browser process, profile or
  window left. The live Helium suite was not rerun: no engine change.

The timing runs rely on xdotool delivering the events within one refresh; on
a much slower or faster machine the smoke run can pass without reaching the
old listeners. Draft Remove buttons also keep their row index: a second Remove
within one frame removes the device that moved into that row, and the panel
notice names it. That path is unchanged.

## PR #23 review corrections, 28 September 2026 (Linux X11)

Helium 0.18.1.1 with its sandbox enabled, debug desktop, private Xvfb
1600 × 1000 and Mesa Lavapipe. The four review findings now have generation
and index checks before frame delivery, dedicated workers for workspace/state
I/O, URL validation before Save, and the first unused preset name after Remove.

- `bash scripts/check.sh` passed: formatting, strict Clippy, 1 CLI, 12 core,
  142 engine and 41 desktop tests. The new regressions cover removed/replaced
  frame indices, close/restart/save completion orders and preset name gaps.
- The complete live Helium suite passed 51/51 with four test threads in 91.8 s.
  An initial run at default concurrency passed 49/51: the certificate reload
  test saw no retained certificate error, and the rapid-swipe test's fresh tap
  had no click in its event report. Each passed alone before the full passing
  rerun; neither test nor engine code was changed for these results.
- `scripts/desktop-smoke.sh` passed all 14 scenarios, including the extended
  workspace run: invalid URL Save preserves exact file bytes, followed by a
  valid URL Save and immediate Ctrl+Q that persists that exact URL before exit.
  No browser process/profile or window remained. An early smoke stopped on
  X11 `X_SetInputFocus` / `BadMatch`; the harness now waits for visible windows
  before resizing and explicitly focusing them. The dialog case passed alone
  and the complete suite passed afterwards.
- A scratch `LD_PRELOAD` shim held `fsync` only for the test workspace/state
  temporary files until explicit release markers appeared. With the desktop
  and its children pinned to one CPU (`taskset`), the workspace panel still
  closed/reopened and repainted while Save was held. Repeated Save started
  one write. Close, repeated twice, cleaned every browser process/profile
  while both writes were held, and started only one state write. Releasing
  state alone kept the window open; releasing workspace too allowed exit 0
  with the requested URL and 1360 × 861 window size persisted.
- The same real-window run typed `file:///tmp/broxser-invalid-save`, clicked
  Save and verified an unchanged file plus the visible "Not saved" validation
  error. Screenshots of rejection, "Saving…" and waiting on close were inspected.
  The fault-injection shim and screenshots were temporary local test artifacts.

This qualifies the changed paths on X11 with software rendering. It does not
extend the physical GPU, Wayland or HiDPI qualification. A disk operation that
never returns can still delay final window closure, while browser cleanup and
the GUI remain independent of that I/O.

## P2.2a workspace panel, presets and application state, 27 September 2026 (cloud container)

Same container: Helium 0.18.1.1 run by `broxsertest` with the sandbox enabled,
Xvfb 1600 × 1000 (Vulkan software), debug build of the desktop. Decisions are in
[ADR 0022](adr/0022-workspace-panel-presets-and-application-state.md).

### Before the change

- The only way to add or remove a device was to edit the workspace file and
  start the desktop again; the running workspace could not be changed.
- Nothing survived a run: the next start needed `--workspace` again and opened
  a 1360 × 860 window whatever the previous size.

### After the change

- The panel (toolbar "Workspace", `Ctrl+Shift+W`) lists the draft's devices
  with Remove, the eight presets with Add, a notice line, and Apply (when the
  draft differs), Discard and Save. Adding "Phone" to the demo, which already
  has a `phone`, yields `phone-2` named "Phone 2"; a ninth device or one over
  the pixel budget is refused with the validation message and the draft stays.
- Apply is the existing restart: four devices load the page after adding one,
  three after removing one; the running pages never see the edit.
- Save writes the draft and the URL bar's address to the loaded file
  atomically; the demo workspace reports that it has no file.
- The state file gets the workspace path and the window size on close; a
  start without `--workspace` reopens the most recent existing file.

### Checks

| Check | Result |
| --- | --- |
| `cargo test -p broxser-core` | 10 passed, including the new `presets_add_named_unique_devices_and_roll_back_over_budget` and `application_state_remembers_workspaces_and_the_window_without_secrets` |
| `bash scripts/check.sh` | Passed in 34 s: fmt, `cargo test --locked` (1 CLI, 10 core, 89 engine, 36 desktop), strict Clippy for the workspace and the desktop crate |
| Live Helium suite as `broxsertest` (`--ignored`, 4 threads; the engine is untouched, `broxser-core` only gained derives) | 42 of 43 passed in 74 s; `live_subframe_navigations_never_sync` failed at its close with "browser processes still running" (the browser's exit outlasted the 5 s cleanup bound, as recorded under P1.4) and passed 3 of 3 alone afterwards |
| New smoke run "workspace panel edits a draft and saves it", alone | Passed in 5.9 s: `Ctrl+Shift+W` opened the panel; Add on the first preset; Apply restarted with four page loads; Remove on the first device; Apply restarted with three; Save wrote `small-phone` and no `phone` into the copy; after Ctrl+Q the state file named the copy and the window size and held no `http` |
| Full `scripts/desktop-smoke.sh` (Xvfb 1600 × 1000, debug build) | 10 of 10 scenarios passed in 1 m 18 s with the new run last; no browser process, profile or window left |
| Real window, screenshots | The panel after `Ctrl+Shift+W` (file line, Save, three device rows with Remove, eight presets with Add) and after Add (the notice, Apply and Discard beside Save) were captured with `xwd` and inspected; the toolbar "Workspace" toggle closed it again |
| Rerun after the cherry-pick onto `main` (`922bb96`, PR #22 merged; along the PR chain this commit was merged with the owner's `touch_run` in `scripts/desktop-smoke.sh`, both runs kept and `touch_run` now using `size_window`, and with the owner's Restart row in the README) | `check.sh` passed: 1 CLI, 10 core, 142 engine and 36 desktop tests, 51 live tests ignored by default. Live Helium suite 51 of 51 in 88.3 s; full smoke 14 of 14 runs in 1 m 31 s, the workspace run and the touch run included; no browser process, profile or window left |

Two smoke findings on the way: with the fixture's default caching, two devices of one session loaded the page from the browser cache, so requests did not count devices; the fixture now sends `Cache-Control: no-store`. And a desktop that reopens at its last size gets no configure event from a resize to that same size, so GPUI drew nothing and clicks hit nothing (the "Restart runtime" run failed twice); the smoke now keeps a private state file and resizes to 860 then 861 px. A failing "Restart runtime" run used to leave the desktop open, and the next run's window search found the stale window; it now closes the desktop before returning.

### Limits

- Names, sizes and sessions are not editable in the panel; the file is.
- No file dialog: a workspace is chosen on the command line or from the
  state's most recent file. The Linux desktop portal that GPUI's dialogs need
  is missing here, so it stays unmeasured.
- The panel exists in the live view only; static mode is unchanged.
- The smoke scenario clicks by button color at fixed panel columns; a theme or
  layout change moves them and needs the scenario updated with it.
- The window size restore was measured only through the smoke's state file
  and the resize behaviour above; no multi-monitor or HiDPI case.

## P2.2 persistent session audit, 27 September 2026 (cloud container)

Audit only; nothing is implemented, and the proposal is
[ADR 0021](adr/0021-persistent-sessions-one-profile-per-session.md) (not
accepted). Same container: Helium 0.18.1.1 (Chrome/154.0.8037.57) run by
`broxsertest` with the sandbox enabled and Broxser's launch flags (private
home, `--password-store=basic`), an on-disk profile as `--user-data-dir`, a
page in the browser's default context, and a scratch Node fixture on a fixed
port that sets `persistent` (Max-Age one day), `session` (`HttpOnly`) and
`visible` cookies, localStorage, sessionStorage and an IndexedDB value.
Chromium 141 (the Playwright build here) played the older browser. These are
reported observations from that scratch probe; its script and raw results were
not committed and have not been rerun in the documentation review below. They
do not qualify the current Broxser startup, which now uses separate temporary
discovery, or a persistent-session implementation.

### What the on-disk profile keeps

| Run | Cookies seen after the restart | localStorage / IndexedDB | sessionStorage |
| --- | --- | --- | --- |
| Clean close (`Browser.close`), restart | `persistent` only | Kept | Gone |
| Restart after a SIGKILL of an idle browser (the values had been flushed by the earlier close) | `persistent` only | Kept | Gone |
| SIGKILL 12 s after the login page set everything, restart | None; `Preferences` `profile.exit_type` was `Crashed` | Kept | Gone |
| `session.restore_on_startup = 1` seeded before the first run, clean close, restart | `persistent`, `session` (`HttpOnly`) and `visible` | Kept | Gone |
| The profile opened by Chromium 141, then by Helium again | Chromium answered `Browser.getVersion` and no `Target.getTargets` within 8 s; Helium afterwards saw everything as before | — | — |

Other measurements: an off-the-record context created in the same browser saw
no cookie, storage or permission of the profile; `Browser.setDownloadBehavior`
and `Browser.setPermission` sent without `browserContextId` applied to the
default context (`notifications` `denied` there, `prompt` in the created
context); the profile held 2.6 MB after one run and 5.0 MB after five; the CDP
endpoint came up in 150 ms with a warm profile; the previous run's
`DevToolsActivePort` stays in the directory and must not be trusted as the new
launch's endpoint. The
`Default/Cookies` SQLite file holds every cookie, session cookies included,
with `v10` values that a short script decrypted with the fixed password of
`--password-store=basic` (PBKDF2 of `peanuts`, 16 space bytes as the IV), so
at rest the profile is protected by its mode and the disk only.

### Limits

- Measured with one browser, one page and a local fixture; no application
  under test, no Helium update between runs.
- The cookie flush interval was not measured beyond "lost after 12 s, kept
  after a clean close". This establishes neither a maximum loss window nor
  power-loss durability; the SIGKILL survival case used already-flushed data.
- Reading `profile.exit_type` right after a close is not reliable (it still
  read `Crashed` once after a clean close).
- The downgrade probe changed both product and Chromium major and observed an
  eight-second target-query timeout. It proves neither categorical profile
  incompatibility nor corruption, and later Helium readability does not prove
  every profile file remained compatible or unchanged.
- Session-cookie survival did not qualify tab/navigation restoration or prove
  that default-context extensions cannot reload pages. The incognito seed of
  ADR 0004 leaves the default-context blocker enabled.
- Not measured: resource cost and disk growth of several persistent browsers,
  imported/copied workspace profile binding, exclusive ownership, all-path
  retention/Forget, the workspace UI, and any migration. A 150 ms endpoint and
  a 2.6–5.0 MB fixture profile are not end-to-end performance or storage bounds.

### PR #22 documentation review, 28 September 2026

Review of `7ab0349` compared the proposal with the current ownership code and
ADRs 0002–0004, 0007, 0019 and 0020. Corrections keep ADR 0021 **proposed** and
the runtime **ephemeral**:

- Removed the assumption that the existing incognito blocker seed works in a
  persistent default context. Qualification must cover the default context and
  absence of extension-induced reloads before application navigation.
- Made restore-on-startup a gated candidate, not an unconditional recipe for
  persistent login. Upstream tab-restore behavior depends on launch arguments;
  a request trace for the actual launcher is still needed to meet no-replay.
- Extended retention to owner shutdown, failed startup, Drop, guardian and
  stale recovery. Current ephemeral cleanup deletes through all these paths.
  Profile binding, exclusive ownership, fresh endpoints and safe Forget need
  explicit acceptance evidence before any persistent directory is opened.
- Replaced a creator-major-only check with full runtime/last-writer compatibility
  and qualified migration, without delaying browser security updates. Qualified
  the single downgrade timeout rather than treating it as a universal result.
- Removed claims of bounded cookie loss, a few-megabyte storage budget and no
  data outside the profile. Fixed-key durable storage needs separate acceptance;
  deleting a directory is not secure erasure or deletion of exports/backups.
- Corrected the cookie-export rationale: CDP can expose `HttpOnly` cookies;
  portable JSON secrecy and incomplete site storage are the reasons to reject it.

This review changes documentation only. No persistent profile, migration, restore
feature or gate acceptance has been implemented. No local live Helium or GUI
rerun is required for these document changes. Existing CI still runs its normal
checks; those protect the unchanged runtime and do not validate the proposal.

| Documentation-review check | Result |
| --- | --- |
| `bash scripts/check.sh` | Passed: fmt, strict Clippy, 1 CLI, 8 core, 142 engine and 36 desktop tests; 51 live tests ignored by default |
| Local Markdown links in the four changed documents | 34 links resolve |
| `git diff --check` and independent design review | Passed; no remaining actionable finding in the revised proposal |
| Local Helium and desktop smoke | Not rerun: no engine or GUI code changed; persistent-session acceptance remains unvalidated |

## P2.1 storage and credential boundary, 27 September 2026 (cloud container)

Same container: Helium 0.18.1.1 (Chrome/154.0.8037.57) run by the unprivileged
user `broxsertest` with the sandbox enabled; `broxser capture` with the example
workspace over a local HTTPS fixture (Python `http.server` with a server
certificate signed by a CA made for this audit) and over the HTTP fixture;
`strace -f` on file opens, `mkdir` and `connect`. The test CA and keys, the
policy file and the entries in the user's NSS database were created for the
audit and removed afterwards. Decisions are in
[ADR 0020](adr/0020-certificate-store-and-keyring-inside-the-profile.md).

### Audit: what the browser opened outside its profile before the change

| Run | Outside the private profile | Result |
| --- | --- | --- |
| HTTP capture | Reads `~/.cache/fontconfig` (30 cache files), `~/.local/share/vulkan` (14 opens), `~/.config/vulkan/*_layer.d` (14), `~/.local/share/glib-2.0` (1), `~/.config/user-dirs.dirs` (1); reads `/etc/chromium/policies/{managed,recommended}`; connects to the system bus (3), nscd and `/dev/log`; no write under the home | 3 frames |
| HTTPS, no CA anywhere | The above plus the user's NSS database: `~/.pki/nssdb/cert9.db` and `key4.db` opened `O_RDWR|O_CREAT`, `pkcs11.txt` read, `libnssckbi.so` looked up there (4 opens) | `net::ERR_CERT_AUTHORITY_INVALID` |
| HTTPS, CA imported into `~/.pki/nssdb` with `certutil` | Same 4 opens | Trusted: 3 frames, 6 fixture requests |
| HTTPS, CA in `/etc/chromium/policies/managed/*.json` (`CACertificates`), none in NSS | Same 4 opens | Trusted |
| HTTPS, policy file removed | Same | Invalid again |
| HTTPS, private `HOME`, no XDG variables, CA in the user's NSS | User database not opened; NSS database created under the private home; fontconfig rebuilt its caches there (about 40 cache files) | Invalid; the trusted (policy) run took 10.0 s against 7.4 s with shared caches |
| HTTPS, private `HOME` with the user's `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_CACHE_HOME` | Caches reused (30 opens), but `~/.local/share/pki/nssdb` created and opened (4) | Invalid with the CA in `~/.pki`; trusted with the policy |
| Direct Helium launch with `XDG_CURRENT_DESKTOP=GNOME` and a session bus address | 11 connects to the session bus; 10 with `--password-store=basic`; no keyring daemon here, so `os_crypt` in `Local State` stayed empty either way | — |

Chromium 154 has no environment variable for the NSS database path (`SSL_DIR`
is not in the binary); the path follows `HOME`, or `XDG_DATA_HOME` when set.

### After the change

Every run: `mkdir <profile>/home` with mode 0700, 0 opens of `~/.pki`, 0 opens
of `~/.local/share/pki`, 30 opens of `~/.cache/fontconfig` and none of a cache
under the private home, and no other path under `/home/broxsertest`.

| Run | NSS database | Result | Capture time |
| --- | --- | --- | --- |
| HTTPS, no CA anywhere | 14 opens under `<profile>/home/.local/share/pki/nssdb` | `net::ERR_CERT_AUTHORITY_INVALID (Broxser trusts the browser's built-in roots and the CACertificates policy, not a personal certificate store); not retried` | 4.9 s |
| HTTPS, CA in `~/.pki/nssdb` (trusted before) | Same | Invalid, same note | 4.6 s |
| HTTPS, CA by policy | Same | Trusted: 3 frames, 6 fixture requests | 7.7 s (7.4 s before) |
| HTTPS, policy file removed | Same | Invalid | 4.7 s |
| HTTP | Not opened | 3 frames | 4.9 s |
| HTTP with `XDG_CURRENT_DESKTOP=GNOME` and a session bus address | Not opened | 3 frames; 10 session bus connects | 4.9 s |

Afterwards: no profile under `/tmp`, the user's database without the test CA
(`certutil -L` count 0), no policy file.

### Checks

| Check | Result |
| --- | --- |
| `cargo test -p broxser-engine --lib` (fake browser and fake CDP) | 73 passed, 0 failed, 42 ignored (the live tests), 8.4 s |
| New unit tests: `environment_keeps_only_the_users_cache_directory`, `browser_runs_in_a_private_home`, `launch_keeps_sandbox_loopback_and_private_state` | Passed; `browser_runs_in_a_private_home` first read an empty environment while the fake browser exec'd `sleep` (1 failure in 7 runs), so it now waits for `HOME=` to appear (6 of 6 after) |
| Live Helium suite as `broxsertest` (`--ignored`, 4 threads) | 42 passed, 0 failed, 70 s; the new `live_certificate_trust_is_the_browsers_own_not_the_users` passed alone in 3.9 s (every device: `Navigation failed: net::ERR_CERT_AUTHORITY_INVALID (…); not retried`, `cert9.db` under `<profile>/home/.local/share/pki/nssdb`, profile removed) |
| Before the live change: the same test with the error page clearing the report | Failed as expected: every device ended at `chrome-error://chromewebdata/` with `error: None`, which hid the certificate failure from the status; the error page now keeps the report |
| `bash scripts/check.sh` | Passed in 2 m 44 s: formatting, `cargo test --locked` (1 CLI, 8 core, 73 engine, 25 desktop), strict Clippy for the workspace and the desktop crate |
| Desktop smoke (`scripts/desktop-smoke.sh`, Xvfb 1600 × 1000, Helium) | 9 of 9 scenarios passed in 1 m 19 s (SIGKILL, SIGINT, static SIGTERM, two restart scenarios, typing while pages animate, dialog, popup, download); no browser process, profile or preview directory left |
| Rerun after the cherry-pick onto `main` (`770f8ac`, PR #20 merged with the owner's discovery startup; the conflict in `BrowserProcess` startup was resolved so the private home and environment apply to every browser Broxser starts, the discovery browser included) | `check.sh` passed: 1 CLI, 8 core, 137 engine and 36 desktop tests, 50 live tests ignored by default. `strace -f` of `broxser capture` over the HTTP fixture: two private homes created (discovery and runtime browser), 0 opens of `~/.pki` or `~/.local/share/pki`, nothing else under the user's home besides `~/.cache/fontconfig`. Live Helium suite 50 of 50 in 80.7 s, including `live_certificate_trust_is_the_browsers_own_not_the_users`; full smoke 13 of 13 in 1 m 22 s; nothing left |

### Limits

- Client certificates from a user's NSS database are not offered any more;
  what a site that requires one shows in Broxser was not measured.
- Fonts under the user's home and a user `fontconfig` configuration are not
  seen by pages; system fonts are.
- The browser's certificate error page is shown in live mode; whether its
  "proceed" link works in headless mode was not measured, and Broxser adds no
  bypass of its own.
- The keyring item was not observed with a real Secret Service; the flag's
  effect was measured as one connect fewer to the session bus.
- Loading the system trust store through p11-kit was not measured.
- `CACertificates` needs a machine policy file (root); there is no per-user
  route, by decision.

### PR #21 review corrections, 28 September 2026 (local Linux)

Review of `d067235` found three gaps despite the initial passing CI:

- Replacing HOME moved Xlib's implicit `.Xauthority` lookup. On an authenticated
  disposable Xvfb, headed capture failed before CDP in 0.16 s with `XAUTHORITY`
  unset; explicitly naming the caller's test authority file succeeded. The
  launch environment now preserves explicit authorization paths and resolves
  implicit ones before HOME changes. Independent reruns passed both variants
  (three frames each), with the authority file unchanged.
- The new claim that only cache remained shared was too broad. In a disposable
  caller home, inherited `SSLKEYLOGFILE` left 388 bytes of TLS handshake secrets
  after certificate rejection and profile cleanup. Every browser launch now
  removes that variable. The rerun left no key log. Documentation scopes the
  guarantee to NSS/profile storage and profile-encryption keyring access;
  native display authorization and other environment integrations remain.
- Reload cleared a confirmed certificate failure, but `Page.reload` replies
  without an error even when its error page returns. The fake regression failed
  before the fix with `None` instead of the previous certificate report. Reload
  now retains that last confirmed failure until a successful document commits;
  a new explicit navigation clears/replaces it. Tests cover both response/event
  orders, successful recovery and exactly the requested navigation commands.

The TLS regression launches capture in a child with disposable legacy and XDG
NSS stores containing a test CA. NSS validates the imported test certificate,
but the private browser must reject it, leave the caller database unchanged,
write no ambient TLS key log and clean its private profiles. `certutil`
(`libnss3-tools` on Ubuntu) is a test prerequisite only. The OpenSSL fixture owns
its server before readiness checks so failed startup also cleans up.

The authenticated X11 and key-log reproductions are retained under ignored
`artifacts/pr21-review/isolation/`, including before/after observations and the
script. No personal NSS/keyring, user authority file or system CA policy was
modified. Secret Service/KWallet and other client-certificate providers remain
unqualified, as noted above.
The first full check also exposed a test-readiness race: procfs could briefly
report the inherited parent HOME around exec, not just an empty environment.
The child-environment test now waits for this launch's exact private HOME before
checking the remaining fields; a wrong mapping still fails after the deadline.

| Final local check | Result |
| --- | --- |
| `bash scripts/check.sh` | Passed: fmt, strict Clippy, 1 CLI, 8 core, 142 engine and 36 desktop tests; 51 live tests ignored by default |
| Full pinned Helium 0.18.1.1 suite, sandbox enabled, four threads | 51/51 passed in 90.70 s, including isolated caller NSS/key-log checks and certificate Reload/recovery |
| Full desktop smoke, Xvfb 1600 × 1000 with Mesa Lavapipe, debug build | 13/13 passed; no browser process, profile or window left |
| Authenticated disposable Xvfb, headed CLI capture | Implicit and explicit XAUTHORITY both passed, three frames each; authority unchanged and profiles cleaned |
| Independent launch-isolation review | No remaining actionable finding; no real Secret Service/KWallet qualification claimed |

Final logs are retained in ignored `artifacts/pr21-review/`. Existing vendored
GPUI and `proc-macro-error2` future-compatibility warnings remain non-failing.

## P1.7 QA fidelity, 27 September 2026 (cloud container)

Same container: Helium 0.18.1.1 (Chrome/154.0.8037.57) run by the unprivileged
user `broxsertest` with the sandbox enabled, Xvfb 1600 × 1000 for the headed
runs, Chromium 141.0.7390.37 (the Playwright build installed here) as the plain
Chromium comparison, no GPU. Decisions are in
[ADR 0019](adr/0019-qa-fidelity-headless-differences.md).

### Probe: what a page sees in each browser configuration

A scratch Node script, not committed, served one page that reports about 80
properties (user agent and client hints, navigator fields, screen and viewport,
media queries, WebGL, canvas and audio fingerprints, fonts, storage, permissions,
subresource headers, whether scripts under `/ads/` and `/tracker/` ran) and
loaded it with Broxser's device setup for a touch phone (390 × 844, mobile) and a
mouse desktop (1440 × 900). Configurations: Helium headless (Broxser's, five
runs), Helium headed on Xvfb, Chromium headless (two runs) and headed, and
Helium headless without the ADR 0004 preference.

| Property | Helium headless (Broxser before) | Helium headed | Chromium 141 |
| --- | --- | --- | --- |
| User agent | `HeadlessChrome/154.0.0.0` | `Chrome/154.0.0.0` | `HeadlessChrome/141` headless, `Chrome/141` headed |
| Client-hint brands | Chromium, Google Chrome, greased | Same | Chromium, greased (no Google Chrome) |
| Desktop device: `(hover: hover)`, `(pointer: fine)` | false, false (`hover: none`, `pointer: none`) | true, true | Same split as Helium |
| Phone device: `(hover: none)`, `(pointer: coarse)` | true, true | true, true | Same |
| Desktop device: `screen` | 800 × 600 | 1600 × 1000 (the X screen) | Same split as Helium |
| Phone device: `screen` | 390 × 844 | 390 × 844 | Same |
| WebGL renderer | ANGLE, SwiftShader | none (no GL on this display) | Same split |
| Canvas fingerprint | Different in each of five runs | Different again | Identical in every run |
| Audio fingerprint | Different in each run | Different | Identical |
| `hardwareConcurrency` | 2 once, 4 in four runs | 4 | 4 always |
| `deviceMemory` | 16 | 16 | 8 |
| Storage quota | 10 GB | 10 GB | about 1 GB |
| `webdriver`, plugins (5), `chrome` object, `Accept-Language` (`en-US,en;q=0.9`), time zone, locale, six fonts, permissions | Same in every configuration | | |
| `/ads/banner.js`, `/tracker/analytics.js` | Requested and run | Requested and run | Requested and run |

Attempts to align the first three rows: `Emulation.setEmulatedMedia` with
`hover` and `pointer` features changed nothing; `Emulation.setUserAgentOverride`
with a user agent alone removed every `Sec-CH-UA` header and emptied
`userAgentData`, and with metadata reproduced the headed headers exactly but
needs the brand list, which CDP does not expose. The launch flags
`--blink-settings=availablePointerTypes=4,primaryPointerType=4,availableHoverTypes=2,primaryHoverType=2`
and `--user-agent=<headed string>` gave the mouse device `hover: hover` and
`pointer: fine`, left the touch phone at `coarse` and `none`, and kept the
brands, with `Sec-CH-UA-Arch` and `Sec-CH-UA-Full-Version-List` blank.
`screenWidth`/`screenHeight` in the device metrics sized the desktop screen to
its viewport.

### Before and after

`live_pages_see_the_browser_a_user_would_run` at `c731d67` (before) and after
the change, a page reporting its user agent, brands, hover and pointer media,
screen and touch points on a touch phone, a mouse tablet and a mouse desktop:

| Device | Before | After |
| --- | --- | --- |
| Phone (touch) | `HeadlessChrome/154`, hover none, pointer coarse, screen 360 × 640, 1 touch point | `Chrome/154`, brands present, otherwise unchanged |
| Tablet and desktop (mouse) | `HeadlessChrome/154`, hover none, pointer none, screen 800 × 600 | `Chrome/154`, hover, fine pointer, screen 600 × 800 and 1000 × 700 |

| Check | Result |
| --- | --- |
| `headed_user_agent_comes_from_the_browsers_version` (unit) | Passed: `Helium 0.18.1.1 (Chromium 154.0.8037.57)` → 154, `Chromium 141.0.7390.37` → 141, no version → none; the fake browser answers `--version`; a missing executable gives none. A first version took the first four-part version and read Helium's own `0.18.1.1` |
| `live_pages_see_the_browser_a_user_would_run` (Helium) | Failed before the change as above; passed after (1.0 s) |
| `bash scripts/check.sh` with the fidelity change | Passed: 1 CLI, 8 core, 71 engine and 25 desktop tests; 41 live tests ignored by default; fmt and strict Clippy clean (two Clippy rounds over the version search, now `rfind`) |
| Live Helium suite (`--ignored`, 4 threads) | 41 of 41 passed in 80.6 s |
| Full `scripts/desktop-smoke.sh` (debug build) | 11 of 11 runs passed with the new launch flags; no browser process, profile or window left |
| Rerun after the cherry-pick onto `main` (`133ce2b`, PR #19 merged with the owner's touch fix; no conflicts) | `check.sh` passed: 1 CLI, 8 core, 119 engine and 36 desktop tests, 47 live tests ignored by default. Live Helium suite 47 of 47 in 73.3 s; full smoke 13 of 13 runs in 1 m 18 s, including touch cancellation through the canvas; no browser process, profile or window left |

### Limits

- The user agent preserves the browser's native platform and version, changing
  only its headless product marker. Two high-entropy client hints stay blank.
  An absent or unsupported native UA keeps the original owned browser; a
  discovery error stops startup instead of launching a fallback.
- WebGL is SwiftShader without a GPU, canvas and audio output carry Helium's
  per-session noise, `hardwareConcurrency` varied once, and session contexts
  have no content blocking (ADR 0004): representative of a Helium user with
  the blocker off on a similar machine, not of a Chrome user.
- Headed Helium was measured on Xvfb without a GPU; a user's machine differs
  in WebGL and screen.

### PR #20 ownership and startup review, 28 September 2026 (local Linux)

Review of `ca46c52` found that the new CLI version probe bypassed the established
browser ownership path. A successful wrapper that retained stdout in a helper
made the nominal three-second probe wait 6.020 s. A 64 MiB version response
raised the probe caller's peak RSS to 68,152 KiB. Through the capture API,
cancellation after 100 ms returned after 3.050 s and still launched the normal
browser; killing the owner left the unregistered version child alive after
5.2 s while the guardian and profile had already gone.

The replacement removes the CLI probe and its stdout reader. Live and capture
now share an owned discovery path: query `Browser.getVersion` in a private,
guarded browser on `about:blank`, normalize only the native headless UA token,
then require checked cleanup before one replacement launch. Discovery creates
no workspace contexts or targets and never loads a workspace URL. An ordinary
or unsupported UA uses the first runtime; errors and cancellation stop startup
without another launch. All spawns check cancellation again after profile and
guardian setup. Endpoint waits and commands retain their existing per-operation
deadlines; startup may include two sequential browser launches, not a global
three-second probe deadline.

A separate sandboxed Helium probe corrected the initial metadata assumption:
omitting optional brands/full versions from CDP UA metadata preserves native
defaults. A per-target override, however, normalized the page and dedicated
worker while service-worker JavaScript and requests still exposed
`HeadlessChrome`. The browser-wide launch override is retained to keep those
surfaces consistent. The new live regression checks page, dedicated-worker,
shared-worker and service-worker UA against their HTTP request headers; a capture regression checks
its headers and that the workspace is requested once per device. Measurements
and final logs are retained locally in ignored `artifacts/pr20-review/`.

That regression also exposed an existing iframe auto-attach interaction:
`waitForDebuggerOnStart: true` plus an iframe-only filter fetched a dedicated
worker script but never ran it, with no attachment event available to resume
it. A raw pinned-Helium probe reproduced the stall; the same worker ran with
the wait disabled. The fix explicitly includes worker types, validates their
ownership and immediately resumes them through the bounded cleanup path,
waiting for acknowledgement before detaching. Iframe interception stays paused
until configured; workers receive no Page setup or page input privileges.
The header probe also verified that worker-origin fetches omit `Sec-CH-UA`
both with the default headless UA and with the launch override. The regression
therefore checks client hints on page-origin requests and UA agreement on all
worker requests, preserving the browser's own client-hint policy.
The first complete live rerun passed 48/49 tests: the existing teardown-crash
test armed its fault at process start, so it aborted discovery cleanup before
the live-frame phase it intended to test. Fault injection is now armed only by
the owner test's explicit close command. The targeted rerun passed both teardown
points, with processes, endpoint and profile gone after 205 ms and 145 ms.

| Final local check | Result |
| --- | --- |
| `bash scripts/check.sh` | Passed: fmt, strict Clippy, 1 CLI, 8 core, 135 engine and 36 desktop tests; 49 live tests ignored by default |
| Full pinned Helium 0.18.1.1 suite, sandbox enabled, four threads | 49/49 passed in 68.52 s after correcting the teardown fault-injection phase |
| Full X11 desktop smoke, Xvfb 1600 × 1000 with Mesa Lavapipe, debug build | 13/13 passed; restart observed the two sequential discovery/runtime profiles, immediate close started none, and no browser process/profile/window remained |
| Independent ownership and worker review | No remaining actionable blocker; discovery, worker and existing iframe regressions passed |

Final logs are in `artifacts/pr20-review/` (ignored). The existing vendored GPUI
and `proc-macro-error2` future-compatibility warnings remain non-failing. Native
validation here covers X11; Wayland, a physical IME and company GPU hardware
qualification are not implied.

## P0 follow-up: survivors of the browser's exit wait, 27 September 2026 (cloud container)

Same container: Helium 0.18.1.1 run by `broxsertest` with the sandbox enabled.
Decision: [ADR 0007](adr/0007-browser-ownership-after-owner-death.md), "Stop
what outlives the wait".

### Before the change

- The cleanup deadlock recorded under P1.4 kept failing live tests at their
  close with "browser processes still running": on 27 September one full suite
  run of 45 failed `live_session_sync_stays_in_session_without_loops_or_replay`
  (it passed 3 of 3 alone), and a P2.2a run failed
  `live_subframe_navigations_never_sync` the same way. Afterwards two
  `helium_crashpad_handler` processes (`--database=<profile>/Crash Reports`),
  a `[helium]` renderer and its zombie were still running for each of two
  test profiles, one of them from a run hours earlier; the profiles had been
  removed. Killing the two handlers by PID released all four processes of
  each set within 5 s.
- Reproducer: a fake browser, `stuck-helper`, starts a helper that carries
  `--database=<profile>/Crash Reports` and never exits by itself.
  `shutdown_stops_helpers_that_would_outlive_the_browser` failed with "2
  processes of the browser or naming its profile did not exit after the
  browser was stopped"; `guardian_stops_helpers_that_would_outlive_the_browser`
  failed with "3 of 5 processes running, profile exists: false, after 10.0 s"
  after the owner's SIGKILL.

### After the change

Both reproducers pass: shutdown and the guardian wait five seconds, stop the
survivors by identity and finish with no process left. A process that only
mentions the profile is still waited for and never signaled
(`crash_database_argument_matches_only_the_profiles_own` covers the argument
match).

| Check | Result |
| --- | --- |
| `bash scripts/check.sh` | Passed: fmt, `cargo test --locked` (1 CLI, 8 core, 97 engine, 36 desktop), strict Clippy for the workspace and the desktop crate |
| Live Helium suite (`--ignored`, 4 threads, `broxsertest`) before the owner's PTZ commit, twice | 42 of 42 passed in 77 s and in 73 s; no browser process left afterwards, zombies aside |
| Live Helium suite on top of the PTZ commit `a075485` | The first run after a container restart failed the four capture tests that start first with "browser did not publish a CDP endpoint within 15 seconds" (four cold browser starts at once; shutdown is not involved) and passed the other 39; the next run passed 43 of 43 in 67 s with nothing left running |
| `scripts/desktop-smoke.sh` (Xvfb 1600 × 1000, debug build), before and on top of `a075485` | 12 of 12 scenarios passed each time (1 m 20 s, 1 m 22 s); no browser process, profile or window left |

### PR #18 ownership review corrections, 28 September 2026 (local Linux)

Review of `be3aed6` found two signal-ownership regressions. The new escalation
used the full recorded process list, but shutdown, Drop, guardian and recovery
all added broad profile references to that list. An unrelated observer already
naming the profile consequently became a SIGKILL target. The new crash-database
matcher also treated a space inside an ordinary argv entry as a flag boundary,
so shell source containing `--database=<profile>/...` could be mistaken for a
crash handler during the fresh scan.

The baseline regressions failed as expected: two argument-matching assertions
accepted embedded shell text, and the shutdown observer test found its own
disposable observer had been signaled. Test-owned child guards cleaned up the
fixtures; no user processes were targeted.

The fix separates verified browser descendants from wait-only profile observers
in all four cleanup paths. The owner retains its spawn-time browser identity
before constructing descendant snapshots, including after the child was reaped.
Crash database matching requires the start of a NUL-delimited argument;
space-separated exact-argument matching is restricted to Chromium's single
nonempty argv representation. Normal argv preserves spaces inside its values.

Regression coverage verifies initial and late observers, embedded marker text,
shutdown and fallback Drop, guardian EOF, stale recovery, sibling preservation,
and reaped/mismatched identities. Recovery still leaves unproven or foreign-boot
processes alone. The existing stuck-helper and late-helper tests continue to
require genuine browser helpers to stop. A persistent observer remains alive
and produces the existing incomplete-release error; recovery retains its profile
for a later attempt.

Independent source review found no remaining actionable introduced issue after
these corrections. Evidence is retained locally in ignored
`artifacts/pr18-fixes/` logs. The intermittent real Crashpad deadlock is not
claimed to have been forced by these deterministic subprocess tests.

| Final local check | Result |
| --- | --- |
| `CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2 bash scripts/check.sh` | Passed: formatting, strict Clippy, 1 CLI + 8 core + 103 engine + 36 desktop tests (148 total) |
| Full live Helium suite (`--ignored --test-threads=2 --nocapture`) | Passed: 43 of 43, 100.12 s; includes owner-death, orphan recovery and normal browser cleanup |
| Independent ownership review | Both signal-eligibility findings resolved; no additional actionable introduced issue found |
| `git diff --check` | Passed |

The live suite used Helium 0.18.1.1 (`Chrome/154.0.8037.57`) with sandboxing
enabled and application-owned private profiles. No GUI code changed, so no
additional native window check was required. Existing vendored GPUI and
`proc-macro-error2` future-compatibility warnings remain non-failing.

### Limits

- The deadlock itself is intermittent; the live runs above show no
  regression, not that it occurred and was resolved. The fake reproducer
  proves the mechanism.
- A shutdown that meets it takes about five seconds longer than a normal one
  (the first wait), then a few milliseconds for the kill and the second wait.
- The container's PID 1 does not reap orphans, so zombies of stopped browser
  processes stay listed here; they hold no memory or files.

## P1.6 unsupported browser interactions, 26 September 2026 (cloud container)

Same container as P1.5: Helium 0.18.1.1 (Chrome/154.0.8037.57), headless, run by
the unprivileged user `broxsertest` with the sandbox enabled and the profile seeded
as in ADR 0004. P1.6 is split per capability; this section records the audit of all
of them and the first change, JavaScript dialogs
([ADR 0014](adr/0014-javascript-dialogs.md)).

### Audit: what happens today

A scratch Node script, not committed, drove one page target with Broxser's setup
(device metrics, focus emulation, screencast) and clicked fixture pages through
`Input.dispatchMouseEvent`. `main` at `6495c2b` handled only
`Page.javascriptDialogOpening` (an error text) and counted popups.

| Capability | Helium 0.18.1.1 | Broxser on `main` |
| --- | --- | --- |
| `alert`, `confirm`, `prompt` | The page stops. The click that opened the dialog stays unanswered, later pointer input and `Input.insertText` are held until the dialog closes, keys are answered and dropped, `Runtime.evaluate` blocks, no screencast frames. No key event variant, mouse event or 8 s of waiting closes it. `Page.navigate` closes it (result false) and navigates | "The page opened a JavaScript dialog, which Broxser cannot show yet"; frozen frame; Go, Reload and link sync cancelled the dialog silently; the held click counted toward "not responding" |
| `beforeunload` (page with user interaction) | A link click or `Page.navigate` opens it; navigate waits for the answer, accept continues it, decline answers `net::ERR_ABORTED`; `Page.stopLoading` aborts the navigation and leaves the dialog open | Go on a dirty page: after the 30 s deadline "Navigation got no response…; loading stopped, not retried" with the page still frozen behind the question |
| `window.open`, `target=_blank`, named popup | `Page.windowOpen` and `Target.targetCreated` (type page, `openerId` = the device, same browser context); the popup loads and keeps running unseen; `Target.closeTarget` closes it and the opener sees `closed` | Counts "N popup(s) not shown"; never closes them |
| Download link, `download` attribute | `Page.downloadWillBegin` then `downloadProgress canceled` at once; no file anywhere; with `Browser.setDownloadBehavior deny` the same plus `Browser.downloadWillBegin` | Nothing shown; no download behavior set, so the outcome is headless Chromium's default |
| `<input type=file>` click, scripted `.click()` | Nothing happens; with `Page.setInterceptFileChooserDialog`, `Page.fileChooserOpened` and still nothing | Nothing shown |
| Geolocation; `Notification.requestPermission()`; `getUserMedia`; clipboard read; fullscreen; `print()` | Denied at once; never resolves (resolves "denied" after `Browser.setPermission denied`); `NotFoundError` (no devices here); `NotAllowedError`; `TypeError`; returns at once | Nothing shown; the page sees the browser's answers |
| Touch device (`mobile`, touch emulation) | Mouse events arrive as `pointerType=mouse`, no `touchstart`; media `(pointer: coarse)`, `(hover: none)`. `Input.dispatchTouchEvent` gives `pointerType=touch`, `touchstart`/`touchend`, a swipe scrolls; `Input.emulateTouchFromMouseEvent` taps produced nothing | Pointer input is mouse input on every device |
| Mouse device | `(hover: none)` and `(pointer: none)`: headless has no pointer device; `Emulation.setEmulatedMedia` accepts hover/pointer features without effect | Pages see no hover capability (QA fidelity, P1.7) |
| HTML5 drag and drop; text selection by drag | `dragstart`, `dragenter`, `drop` with data, `dragend` through mouse events; selection works on the mouse device, not on the touch device | Works as the browser does |
| Accessibility | — | GPUI 0.2.2 exposes no accessibility tree on Linux; the page's tree is not read |

The first probe run without the ADR 0004 preference again showed the bundled
blocker reloading a page.

### JavaScript dialogs: before the change

A temporary test through the live runtime on `main` at `6495c2b`, with the new
fixture pages:

| Step | Result |
| --- | --- |
| Click the alert button | Error text shown; 0 frames in the next second |
| Type `a` into the device | The dialog stays (typing never answers a dialog) |
| Click the confirm button, then Go | The phone navigated with the others; the page reported `alert closed` (the navigation cancelled the dialog) and never saw its confirm |
| Type into a page that asks before leaving, then Go with a 3 s load limit | After 4.5 s: "Navigation got no response within 3 seconds; loading stopped, not retried", URL unchanged, 0 frames in the next second, typing changed nothing: the question stayed open behind a frozen frame |

### JavaScript dialogs: after the change

| Check | Result |
| --- | --- |
| `dialog_blocks_input_and_navigation_until_the_user_answers` (fake CDP) | Passed: kind, bounded message (`MAX_DIALOG_CHARS` + `…`), no "not responding" past the command limit, keys and text dropped, Go refused for the device with the report and sent to the others, stale token ignored, `Page.handleJavaScriptDialog {accept:false}`, close clears the report, prompt default and `promptText` |
| `live_dialogs_wait_for_an_explicit_answer` (Helium) | Passed: alert freezes frames; typing and Go leave it open (peers navigate, the phone reports the refusal); a stale token answers nothing; OK resumes the page and its frames and typing reaches the field; confirm false and true, prompt "Broxser" and null, each reported by the page |
| `live_beforeunload_dialog_needs_an_explicit_leave_or_stay` (Helium, 3 s load limit) | Passed: Go on a dirty page asks and waits 4 s past the limit without an error; Stay keeps the page without an error; Leave navigates; the page's own link asks the same question, Stay and Leave |
| Both live tests in parallel, 2 threads | 3 of 3 runs passed after the fix below; before it, 1 of 4 and 2 of 2 runs failed |
| `bash scripts/check.sh` | Passed: 1 CLI, 8 core, 66 engine and 24 desktop tests; fmt and strict Clippy; 36 live tests ignored by default |
| Live Helium suite (`--ignored`, 4 threads) | 36 of 36 passed in 59.0 s. A first run passed 35 and failed `hidden_devices_reject_input_and_sync_without_replay` at its cleanup check, "browser processes still running": two crash handler processes, a renderer and a zombie of one browser stayed until the handlers were killed by hand, the cleanup deadlock recorded under P1.4; the test passed alone |
| `scripts/desktop-smoke.sh` (debug build, Xvfb, no window manager) | 9 of 9 runs passed, including the new "dialog answered on the card": the check is by pixel color (XGetImage), not by reading on-screen text — it finds the page's blue button, then the panel's orange border and the OK/Cancel button colors inside it; clicking them makes the page report `confirm=true` and `confirm=false` over HTTP, and the border color is gone once the card repaints; no browser process, profile or window left |

The parallel failures were a test problem worth recording: the test clicked a
field and typed at once, and under load the key was processed before the click.
Chromium routes pointer events through its compositor thread and key events to
the renderer's main thread directly, so a key sent right after a click can overtake
it. The tests now wait for the field's caret report (ADR 0011) before typing.

The smoke run finds the page's blue button and the panel's orange border in the
window with XGetImage, clicks them, and reads the page's answers from the fixture
log. A first version checked that the panel was gone by looking for its border
once, right after the page's report, before the card had repainted; it now waits
for the border to be absent.

### Popups ([ADR 0015](adr/0015-popups-closed-and-reported.md))

A second scratch Node script, not committed, opened windows from one page target
and counted, on the fixture, each window's document request, whether its script
ran and its periodic reports. Five windows per case unless noted:

| Case | Left running (as on `main`) | Closed at `Target.targetCreated` | Held by browser-level auto-attach, closed, then resumed |
| --- | --- | --- | --- |
| `window.open` with a gesture | 5 requests, 5 scripts, reports continue, 5 windows left | 5 requests, 1 script, no reports, 0 left | 0 requests, 0 scripts, 0 left |
| `target=_blank` link (`noopener`) | 5, 5, continue, left | 5, 0, none, 0 | 5, 0, none, 0 |
| Named window with features (2 opens) | 2, 2, continue, left | 0, 0, none, 0 | 0, 0, none, 0 |
| `window.open` from a same-origin frame (1) | 1, 1, continue, left | 1, 0, none, 0 | 0, 0, none, 0 |
| `window.open` without a gesture | Blocked by Chromium: `null`, no target | Same | Same |
| `about:blank` window written by the opener | Opened and kept | Closed | — |

`Page.windowOpen` precedes `Target.targetCreated` by about 4 ms and carries the
address; the target's own URL is still empty then. `openerId` names the device for
windows opened by its main frame and its same-origin frames, `noopener` included.
With auto-attach, detaching a held window instead of resuming it left the opener
page unresponsive, and page-level auto-attach did not attach popups at all.

A temporary test through the live runtime at `06e0f5d`, with a window that reports
every 200 ms and sets `window.opener.location = '/hijacked'` after 300 ms:

| Step | Before | After |
| --- | --- | --- |
| Click the `window.open` button on the phone | The phone moved to `/hijacked`; 7 reports in 1.5 s and 10 in the next 2 s; counted "1 popup(s) not shown" | The phone stays; no `/hijacked` request; no reports; "Closed a window the page opened" with its address and Open here |
| Reload the page, click the `target=_blank` link and the named window | 15 reports in 1.5 s from all three windows, including the first, whose opener page was gone | Each closed and reported, counted 3 |
| Open here on the named window's report | — | The phone loads it alone, once, without an opener; the tablet stays |

| Check | Result |
| --- | --- |
| `popups_are_closed_at_once_and_reported_for_their_device` (fake CDP) | Passed |
| `live_popups_are_closed_before_they_act_and_open_only_on_request` (Helium) | Passed 1 of 1 alone and 5 of 5 beside two other live tests on 3 threads |
| `scripts/desktop-smoke.sh`, new run "popup closed and opened on the card" | Passed 2 of 2 alone: no report from a running window after the click, the card's Open here loads the page (the phone frame turns green, the page reports that it runs as a page), and the report disappears. A first version found the card's one-pixel accent border when checking that Open here was gone; the check now counts filled areas only |
| `bash scripts/check.sh` with popups | Passed: 1 CLI, 8 core, 67 engine and 24 desktop tests; 37 live tests ignored by default |
| Live Helium suite with popups (`--ignored`, 4 threads) | 37 of 37 passed in 61.9 s |
| Full `scripts/desktop-smoke.sh` with popups (debug build) | 10 of 10 runs passed; no browser process, profile or window left |

### Downloads and file choosers ([ADR 0016](adr/0016-downloads-and-file-choosers-refused-and-reported.md))

A third scratch Node script, not committed, drove one page target with Broxser's
setup and a fixture whose file answers arrive slowly (2 MiB in 64 KiB chunks), so
an early cancel shows as an aborted body. Each case once, with the headless
default and then with `Browser.setDownloadBehavior` `deny` and events for the
context:

| Case | Headless default | Denied per context |
| --- | --- | --- |
| Attachment link (`Content-Disposition`), `application/octet-stream`, POST answered as an attachment, redirect to an attachment, `location.href` set by a script | Request sent and aborted; `Page.downloadWillBegin` then `downloadProgress` `canceled` within 2–8 ms; no file; the page stays | The same, plus `Browser.downloadWillBegin` and its cancellation |
| `download` attribute on a page link; `data:` URL with one | Request sent (none for `data:`) and cancelled; suggested name from the attribute | Same |
| `blob:` URL with a `download` attribute | No download event at all | `Browser.downloadWillBegin` with the blob URL, cancelled |
| Three `download` links clicked by one script | One request, one download event | Three requests, three events, each cancelled |
| Attachment link inside a same-origin frame | Reported with the frame's ID on the page session | Same, at the browser level too |
| Attachment link inside a cross-site frame (another renderer) | Nothing on the page session | `Browser.downloadWillBegin` only, with the frame ID the device session had seen attached and then detached (reason `swap`) |
| Hostile `Content-Disposition` (`../../.bashrc`, a right-to-left override, newline, bell, escape sequence) | Suggested name `_fdp.exe___[31m.txt`: the browser sanitizes the path, not every control character | Same |
| `Page.navigate` to an attachment | `net::ERR_ABORTED` with `isDownload: true`, no `loaderId`; download events follow | Same |
| `<input type=file>` single, `multiple`, `webkitdirectory`, scripted click with a gesture; `showOpenFilePicker`, `showSaveFilePicker`, `showDirectoryPicker` | No chooser: the input's `cancel` event fires, the pickers reject with `AbortError`; a scripted click without a gesture does nothing | With `Page.setInterceptFileChooserDialog`: `Page.fileChooserOpened` (frame, mode, node) and the page waits; with `cancel: true` the page still gets `cancel` |

No file appeared in the profile, the working directory or `~/Downloads` in any
case.

A temporary test through the live runtime at `6fefab7` (popups branch), with a
page holding an attachment link, a `download` attribute link, a file input and a
cross-site frame with its own attachment link:

| Step | Before | After |
| --- | --- | --- |
| Click the attachment link on the phone, the `download` link on the tablet, the frame's link on the desktop | Each request was sent and every device status stayed exactly as before: nothing shown | Each device reports "Refused a download the page started" with `report.pdf`, `notes.txt` and `frame.pdf` and the address; the pages stay; each request was sent once |
| Click the phone's file input | The page got `cancel` after the headless browser opened nothing; nothing shown | The page still gets `cancel`, no file; the phone counts one file chooser |
| Go to an attachment address | "Navigation failed: net::ERR_ABORTED; not retried" on all three devices | No error; every device reports the download `go.pdf` and stays on its page; three requests |
| Files anywhere | None | None |

| Check | Result |
| --- | --- |
| `downloads_and_file_choosers_are_refused_and_reported_for_their_device` (fake CDP) | Passed |
| `live_downloads_and_file_choosers_are_refused_and_reported` (Helium) | Passed 1 of 1 alone |
| `scripts/desktop-smoke.sh`, new run "download refused on the card" | Passed: the phone frame keeps its page, the card shows the report with the accent Dismiss below the frame, Dismiss hides it, and no file named `notes.txt` exists under the run's private `TMPDIR` or `~/Downloads`. A first version looked for the raised button color and matched the anti-aliased edges of the card's text instead; the button is now accent-filled and the run uses a 1000 px tall window so the report is not cut by the window edge |
| `bash scripts/check.sh` with downloads | Passed: 1 CLI, 8 core, 68 engine and 25 desktop tests (the new `activity_line` unit test included); 38 live tests ignored by default; fmt and strict Clippy clean |
| Live Helium suite with downloads (`--ignored`, 4 threads) | 37 of 38 passed in 68.9 s; `live_owner_death_while_frames_stream` failed its check that the dead owner's CDP port refuses connections ("the dead owner's CDP endpoint is open") while its browser processes and profile were gone. That check is a plain TCP connect, so a browser or fixture of one of the three other threads taking the freed port answers it; the test passed alone right after (8.4 s). The pre-existing test is not changed here |
| Full `scripts/desktop-smoke.sh` with downloads (debug build) | 11 of 11 runs passed; no browser process, profile or window left |
| Rerun after the cherry-pick onto `main` (42744df, PR #15 merged; conflicts in GOALS, README, `live_view.rs` imports and tests, and `dialog_text` resolved without behavior changes) | `bash scripts/check.sh` passed in 58 s (1 CLI, 8 core, 84 engine, 36 desktop tests, fmt and strict Clippy); live Helium suite 39 of 39 in 70 s; desktop smoke 9 of 9 in 1 m 10 s with nothing left running |
| CI on the PR head (run 87, `push` event) | Failed in the fake-CDP popup test `popups_are_closed_at_once_and_reported_for_their_device`: it compared the peer's `Target.closeTarget` requests right after the tablet's report appeared and saw four of five, because the report is visible before the peer has read the request that went with it; the `pull_request` run of the same commit passed. The check now waits (up to 10 s) for the expected number of close requests before comparing: 25 of 25 targeted runs and 3 of 3 full fake suites passed afterwards |

### PR #16 review corrections, 27 September 2026 (local Linux)

Review of `4fd12de` found four gaps despite a passing full check (129 tests) and
39 passing live Helium tests. A separate harness used the public `LiveSession`
API with the pinned Helium and fresh private profiles; a private Xvfb window
verified the desktop behavior:

- A top-level file input incremented `file_choosers` to 1. A file input inside
  a cross-site `localhost` iframe then emitted another server-observed `cancel`,
  but the count stayed at 1. Only the device's main renderer session had chooser
  interception enabled.
- With `127.0.0.1` containing a `localhost` iframe, which contained another
  `127.0.0.1` iframe, the nested frame requested its attachment once but the
  device stayed at `downloads: 0, download: None`. The browser reported the
  download with a frame ID absent from the main session's attachment events.
  A subsequent top-level download correctly incremented the count.
- Dismissing the first download, killing the owned browser and restarting
  caused the next runtime's first report to be hidden. The activity line still
  showed one refused download; the second download displayed its report again.
- A 1440 × 900 device at 100% in a 1360 × 861 window showed a report whose
  Dismiss button was beyond the right window edge. Returning to 50% made the
  button reachable.

The corrected desktop clears download dismissal on restart, limits the panel
to 360 UI pixels and puts Dismiss on the left. The new smoke scenario
`download_desktop_restart_run` checks the initial report, the first after a
browser kill/restart and another fresh report at 100%; every report must appear
and dismiss. The focused run passed twice with no browser processes or profiles
left, and its screenshots were inspected. The existing download smoke also
handles an absent `~/Downloads` directory without creating one or changing user
files.

Engine regression coverage exercises recursive iframe sessions, DOM ancestry
across renderer swaps, stale frame-tree replies, removal and document replacement,
and late detachment of an old session. The real nested-frame fixture checks
chooser cancellation and downloads on their owning devices, a top-level control,
one request per action, no selected files, and no replay after navigation.

Qualification also caught a failure in the initial fix's deadline handling:
one cross-site iframe ran a busy loop, and its page then inserted another iframe
using the same renderer. `Page.enable` for that new target could not answer;
treating that as a runtime error stopped a healthy device in another session.
The final implementation marks only the affected document's iframe activity as
incomplete and waits asynchronously for resume before detaching held sessions.
An independent public-API probe used an 18 s busy loop against the default
15 s setup deadline: the healthy peer accepted clicks throughout, the delayed
child loaded after recovery and accepted a click, and one attachment request
produced one download report without a lingering debugger pause or replay.
The warning persists until a fresh main document; the global 128-session safety
budget remains an explicit runtime-stop boundary, as recorded in ADR 0016.

| Focused check | Result |
| --- | --- |
| Nine new fake-CDP iframe regressions | Passed |
| Nested cross-site chooser/download Helium regression | Passed: correct owning device, one cancel per chooser and one request per download, no selected or saved files |
| Finite-busy Helium regression | Passed: a 4 s renderer stall exceeds its 1 s setup deadline, the healthy peer remains usable, and the child resumes after recovery |
| Independent original review harness | Passed: top chooser count 1 then iframe chooser count 2; nested download count 1 then top-level control count 2; no device/protocol errors |
| Independent default-deadline recovery harness | Passed: 18 s busy renderer, default 15 s deadline, healthy peer and recovered child remain usable |
| Focused desktop restart/layout regression | Passed twice at 100% zoom; each of three reports appeared and dismissed, with no browser processes or profiles left |

| Final integrated check | Result |
| --- | --- |
| `CARGO_TARGET_DIR=/home/ipei/webdev/broxser/target CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2 bash scripts/check.sh` | Passed: formatting, strict Clippy, 1 CLI + 8 core + 93 engine + 36 desktop tests (138 total); 41 live tests run separately |
| Full live Helium suite (`--ignored --test-threads=2 --nocapture`) | Passed: 41 of 41, 208.33 s, including both new regressions and existing popup/dialog/input/navigation controls |
| Rebuilt debug desktop | Passed; final executable used for the integrated window checks |
| Full `scripts/desktop-smoke.sh`, private Xvfb 1600 × 1200 with Mesa Lavapipe | Passed: 12 of 12 scenarios, including the desktop download/restart regression; no browser process, profile or window left. The known static preview directory after SIGTERM was removed by the harness |
| Independent engine review | No remaining actionable findings after the busy-renderer isolation and cleanup corrections; original review reproductions and default-deadline recovery both passed |

The first strict check found one nested `if` that Clippy required collapsing;
that was corrected and the whole check rerun successfully. The first full window
run stopped at the older double-Restart scenario's fixed 3 s startup sleep. The
harness now waits up to 30 s for the restarted browser's fixture request, matching
its initial-start readiness bound, and closes the app on that failure path.
The complete window suite then passed. The initial logs are retained alongside
the final runs. Existing vendored GPUI numeric-fallback and `proc-macro-error2`
future-compatibility warnings remain non-failing. Physical GPU and Wayland
rendering were not requalified by these X11 checks.

Review evidence is retained locally in the ignored `artifacts/pr16-review/`
directory; correction logs and screenshots are in `artifacts/pr16-fixes/`.
The native checks use Xvfb and Mesa Lavapipe with the browser sandbox enabled.

### Permissions ([ADR 0017](adr/0017-permission-prompts-denied.md))

A fourth scratch Node script, not committed, requested every permission-gated
API from one page target after a click and again without a gesture, timing each
answer, and read `navigator.permissions.query()` for eighteen names; then the
same with `Browser.setPermission` `denied` for the context.

| Request | Headless default | Denied per context |
| --- | --- | --- |
| `Notification.requestPermission()` | Resolves `denied` after 1.2–1.8 s, with or without a gesture; `query` says `prompt` | `denied` in 5–17 ms; `query` says `denied` |
| `IdleDetector.requestPermission()` | `denied` after 2.6–3.8 s (gesture); `NotAllowedError` at once without one; `query` says `prompt` | `denied` in 16 ms |
| `getUserMedia` (no devices here) | `NotFoundError` in 50–90 ms; `query` says `prompt` for camera and microphone | The same here; `query` says `denied` |
| `getDisplayMedia` | Never resolves (8 s) | Never resolves, even with `display-capture` denied |
| Geolocation, MIDI, clipboard read, pointer lock, window management, push subscription, local fonts | Denied or thrown at once | Same |
| Clipboard write, wake lock, fullscreen, persistent storage | Answered at once | Same, unless those permissions are denied too: then copy buttons and wake locks fail and `queryLocalFonts()` resolves |

`Browser.setPermission` takes web descriptor names (`notifications`,
`idle-detection`, `camera`, …); the `PermissionType` names of
`Browser.grantPermissions` are rejected as invalid descriptors. CDP sends no
event for a permission request.

A live Helium test at `43cb397`, with a page that asks for the notification
permission on load without a gesture:

| Step | Before | After |
| --- | --- | --- |
| Three devices load the page | `query` reported `prompt` on all three; two were answered `denied` after 1551 and 1812 ms and one was not answered within 10 s | All three answered `denied` within 500 ms (the test's bound; the probe measured under 20 ms) and `query` reports `denied` |

| Check | Result |
| --- | --- |
| `permission_prompts_are_denied_in_every_session_context` (fake CDP) | Passed |
| `live_permission_requests_are_denied_at_once` (Helium) | Failed before the change as above; passed after |
| `bash scripts/check.sh` with permissions | Passed: 1 CLI, 8 core, 69 engine and 25 desktop tests; 39 live tests ignored by default; fmt and strict Clippy clean |
| Live Helium suite with permissions (`--ignored`, 4 threads) | 39 of 39 passed in 75.0 s |

No desktop change, so no window check was run for this capability.

### PR #17 camera PTZ correction, 27 September 2026 (local Linux)

Review of `b62bc05` found that denying `{"name":"camera"}` left
`permissions.query({name: "camera", panTiltZoom: true})` at `prompt`. Chromium
uses a separate permission for camera movement controls. The pinned Helium
probe reproduced that state on both `127.0.0.1` and `localhost`, while ordinary
camera queries reported `denied` and media requests using simulated devices
rejected with `NotAllowedError`. Explicitly denying the PTZ descriptor changed
its query to `denied`; unrelated permission queries and a separate control
context were unchanged.

The shared setup helper now sends a fifth `Browser.setPermission` denial for
`{"name":"camera","panTiltZoom":true}` in each session context. Both live and
capture use that helper before creating device targets. The four existing
denials remain in place.

The fake-CDP regression independently lists all five expected descriptors for
each context. The new live test
`live_camera_permission_queries_deny_ptz_across_origins` collects 24 query
reports across three device widths, two session contexts and two origins.
Ordinary camera and PTZ must both be `denied`; clipboard-write and
screen-wake-lock must remain `granted`. It requests no media device access.

Before the patch, the focused permission run passed the existing notification
timing test and failed the two regression checks: the fifth descriptor was
missing and PTZ remained `prompt` in all six device/origin combinations. After
the patch, all three focused tests passed (0.89 s). Baseline and passing logs are
retained locally in the ignored `artifacts/pr17-fixes/` directory; the original
review probes are in `artifacts/pr17-review/`.

| Final check | Result |
| --- | --- |
| `CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2 bash scripts/check.sh` | Passed: formatting, strict Clippy, 1 CLI + 8 core + 94 engine + 36 desktop tests (139 total) |
| Full live Helium suite (`--ignored --test-threads=2 --nocapture`) | Passed: 43 of 43, 146.73 s, including the PTZ regression and existing notification timing test |
| `git diff --check` | Passed |

Validation used Helium 0.18.1.1 (`Chrome/154.0.8037.57`), sandbox enabled and
application-owned private profiles. No GUI code changed, so no additional
window check was required. The PTZ regression queries permission state without
accessing camera hardware. Existing vendored GPUI and `proc-macro-error2`
future-compatibility warnings remain non-failing.
### Touch input ([ADR 0018](adr/0018-touch-input-for-touch-devices.md))

The audit's touch probe (above) showed mouse events with `pointerType: mouse`
on a touch device and the event sequence `Input.dispatchTouchEvent` produces. A
live Helium test at `77a1762`, with a page that numbers and reports every
pointer, mouse and touch event on a box and its scroll position after a drag:

| Step | Before | After |
| --- | --- | --- |
| Tap the box on the touch phone | `pointerdown mouse`, `pointerup mouse`, `mousedown`, `mouseup`, `click mouse` | `pointerdown touch`, `touchstart` (1 touch), `pointerup touch`, `touchend` (0), `mousedown`, `mouseup`, `click touch` |
| Click the box on the mouse desktop | Mouse events | Unchanged: `pointerdown mouse`, `mousedown`, `pointerup mouse`, `mouseup`, `click mouse` |
| Drag 150 CSS px upward on the phone (press, three moves 40 ms apart, release) | Mouse moves, no scroll | `touchmove` reports and `scrollend` with a positive scroll offset |

| Check | Result |
| --- | --- |
| `touch_devices_send_touches_and_nothing_for_hover_or_other_buttons` (fake CDP) | Passed: `touchStart`/`touchMove`/`touchEnd` with the point and modifiers on the touch device, nothing for hover, right or middle, mouse events on the mouse device. A first version released before the coalesced move was sent by the loop; the move is then dropped, as for a mouse, so the test waits for it |
| `live_touch_devices_get_touches_and_mouse_devices_get_a_mouse` (Helium) | Failed before the change as above; passed after (1.2 s) |
| `bash scripts/check.sh` with touch | Passed: 1 CLI, 8 core, 70 engine and 25 desktop tests; 40 live tests ignored by default; fmt and strict Clippy clean |
| Live Helium suite with touch (`--ignored`, 4 threads) | 40 of 40 passed in 76.5 s |
| Full `scripts/desktop-smoke.sh` with touch (debug build); the example phone is a touch device, so its taps drive the dialog, popup and download runs | 11 of 11 runs passed through taps on the touch phone; no browser process, profile or window left |
| Rerun after the cherry-pick onto `main` (`a62b3b8`, PR #18 merged; no conflicts) | `check.sh` passed: 1 CLI, 8 core, 104 engine and 36 desktop tests, 44 live tests ignored by default. Live Helium suite 44 of 44 in 68.9 s; full smoke 12 of 12 runs in 1 m 16 s; no browser process, profile or window left |

### PR #19 touch review corrections, 28 September 2026 (local Linux)

Review of `f1d37ab` found three gaps:

- The live test waited for `click`, then asserted all preceding HTTP event
  reports had arrived. Separate requests can arrive out of order: CI run
  `36362294834` missed `pointerup`. Delaying that report by 150 ms reproduced
  the same failure on the original head; waiting for both complete event
  sequences passed with the delay still enabled. Swipe assertions also wait
  for both their move and scroll reports.
- A release discarded coalesced movement even though `touchEnd` has no
  coordinates. A rapid swipe could therefore activate the original element.
  The engine now preserves bounded unsent movement and the release position,
  or cancels when the input budget cannot fit them. Device-local ownership
  rejects orphan moves/releases and cancels interrupted browser touches before
  a new press. Old releases never replay after hide, dialogs, navigation or
  unresponsive input; cancellation is not queued behind a blocking dialog.
- Ignored right-button presses could still cancel IME before the touch filter,
  including through the desktop's explicit composition cancellation. Both
  layers now filter unsupported buttons first. The desktop cancels touch on
  release outside the canvas, device changes and focus loss, preventing a
  click at the last in-canvas point or hover continuing an abandoned touch.

Regression coverage lives in `live/touch_input.rs`, the existing touch event
test and the X11 smoke's `touch cancellation through the canvas` scenario.
The latter checks right/middle buttons, release outside the frame, focus
moving to the URL bar and a subsequent normal tap through a real GPUI window.
Its first run caught GPUI's root focus listener not firing when focus moved
to the descendant URL field: the second gesture ended in an unintended click.
Synchronous cancellation for Ctrl+L and a non-canvas focus guard fixed it;
the final history was `start, cancel, start, cancel, start, end, click`.

| Final local check | Result |
| --- | --- |
| `bash scripts/check.sh` | Passed: fmt, strict Clippy, 1 CLI, 8 core, 118 engine and 36 desktop tests; 46 live tests ignored by default |
| Pinned Helium 0.18.1.1 live suite, sandbox enabled, four threads | 46/46 passed in 74.46 s, including both new touch lifecycle/swipe reproducers |
| Full `scripts/desktop-smoke.sh`, Xvfb 1600 × 1000, Mesa Lavapipe 26.2.3, debug build | 13/13 scenarios passed, including touch cancellation through the canvas; no browser process, profile or window left |
| Independent final source/test review and `git diff --check` | No remaining actionable finding; clean diff |

Logs are retained locally in ignored `artifacts/pr19-review/`. The Xvfb host
needed a local software Vulkan driver; its package checksum was checked against
the distro package database. The existing vendored GPUI and
`proc-macro-error2` future-compatibility warnings remain non-failing. This run
does not qualify Wayland, a physical IME or company GPU hardware.

### Hover and pointer media, drag and drop, accessibility (documented limits)

The last P1.6 items are limits, recorded from the audit above rather than
changed:

- **Hover and pointer media.** The headless browser reports no pointing device
  of its own: a mouse device's page sees `(hover: none)` and `(pointer: none)`,
  and `Emulation.setEmulatedMedia` accepts `hover` and `pointer` features
  without effect. Touch devices see `(pointer: coarse)` and `(hover: none)`
  from touch emulation, which is right for them. CSS `:hover` styles still
  apply when the mouse device's pointer is over an element, since mouse moves
  are sent; only the media queries misreport. This is a fidelity difference
  for P1.7 (comparison with the user's browser), not something a card can fix.
  P1.7 closed it with launch flags (ADR 0019): mouse devices now see
  `(hover: hover)` and `(pointer: fine)`, touch devices keep `coarse` and `none`.
- **Drag and drop.** HTML5 drag and drop (`dragstart`, `dragenter`, `drop`
  with data, `dragend`) and text selection by dragging work through mouse
  events on mouse devices. On touch devices a drag is a swipe (ADR 0018);
  touch-based drag and drop (long press, then move) is not modeled, and text
  selection by touch is not available.
- **Accessibility.** GPUI 0.2.2 exposes no accessibility tree on Linux, so the
  Broxser window itself is not readable by a screen reader, and the page's
  accessibility tree (`Accessibility.getFullAXTree` over CDP) is not read or
  shown. Accessibility QA of a page needs the page opened in a browser with a
  screen reader; a Broxser view of the page's tree is a separate capability.

### Limits

- Touch devices get one finger: no pinch, multi-finger gestures, long press or
  right click, and no hover. A release preserves the final touch position;
  interrupted gestures are canceled without replaying their release. Scroll
  sync mirrors wheel input; swipe scrolling is local to its device.
- `getDisplayMedia()` never resolves in the headless browser, denied or not.
  Camera and microphone are denied on the probe's evidence about prompt-type
  permissions; no device here could exercise them.
- A refused download's document request still reaches the server; export and
  attachment flows produce no file in Broxser, and upload flows see a cancelled
  chooser. The suggested file name is the browser's, sanitized as a path, shown
  on one line without control characters.
- A closed window's first document request reaches the server, and its script
  can start before the close; flows that need their window (sign-in, payment)
  do not complete.
- A prompt's text field is the URL bar's single-line field: no IME composition,
  partial selection or copy.
- Dialogs opened by a subframe are reported for the device like the main frame's;
  the panel does not say which frame asked.
- A dialog on a hidden device is answered only after showing the device; hiding
  does not answer it.

### Review fixes, 27 September 2026

PR #14 review found gaps in the change above. Engine
(`crates/broxser-engine/src/live.rs`):

1. A prompt's default is now bounded as an answer Broxser can send back
   unchanged (`prompt_text`): line breaks and tabs become a space, other
   control characters are dropped, cut at `MAX_DIALOG_CHARS` with no cut mark.
   The message keeps `dialog_text`'s rule (control characters dropped except
   line breaks and tabs, cut and marked with `…`).
2. A prompt answer over 2048 characters or holding a control character is not
   sent; the dialog stays open and the device reports `PROMPT_REJECTED`,
   clearing on the next sent answer or when the dialog closes.
3. Only one answer per dialog is outstanding at a time: once
   `Page.handleJavaScriptDialog` is sent, a further answer under the same
   token is ignored until `Page.javascriptDialogClosed`, or the dialog is
   cleared by a new document, a crash, a detach or a runtime stop. The reply to
   that command is now tracked (`Pending::DialogAnswer`): if it is itself an
   error and the same dialog is still open, the answer is taken back so the
   dialog can be answered again, and the browser's message is shown as a
   protocol error ("Input error" in the desktop), not a dialog-specific one.
4. An unknown or missing dialog type reports `UNKNOWN_DIALOG` without blocking
   input or navigation for that device; it clears once the browser reports the
   dialog closed.
5. Staying on a `beforeunload` ends only the Broxser navigation the question
   belongs to: a tracked navigate or reload with no page-initiated main-frame
   `Page.frameRequestedNavigation` in the current tab (no `disposition`, or
   `disposition: "currentTab"`) since it was sent — a link or script that opens
   a new tab, window or download no longer counts. A `beforeunload` the page's
   own link raises while a Broxser navigation is pending leaves that navigation
   tracked, running under its own deadline.
6. The IME refresh sent when a hidden device is shown (`Runtime.evaluate`) is
   renderer-answered and held while a dialog is open; it is now sent without a
   deadline (`send_ignored`), skipped while a dialog is open and re-sent when
   the dialog closes. Before this fix, showing a hidden device during its own
   dialog stopped the whole runtime once the 15 s command limit passed.
7. Opening a dialog invalidates IME state locally without sending a
   composition cancel to the page, and the not-responding check pauses
   instead of tripping while the dialog is open.
8. A refused Go or workspace open (open dialog) now retires the device's
   pending link, the same as a sent one would (ADR 0013); a refused synced link
   is different and leaves the device's link-sync tracking untouched, so a
   link that commits later still syncs.
9. Stopping the runtime clears every device's dialog and dialog-related status
   text.

Desktop (`crates/broxser-desktop/src/live_view.rs`,
`crates/broxser-desktop/src/url_input.rs`):

11. Keys a prompt field does not consume never reach a page: the canvas
    forwards input to the selected page only while the canvas itself holds
    focus.
12. The prompt field holds one line of at most 2048 characters (typing and
    paste capped). OK and Enter send the field's text unchanged, without
    trimming; Escape cancels like the Cancel button; either returns keyboard
    focus to the canvas.
13. A new prompt on the selected device takes focus only from the canvas or
    that device's own previous field, never from the URL bar or another
    device's field; a held or auto-repeating Enter does not answer a newly
    focused prompt.
14. The panel is shown only while the runtime is running, is at most 360 px
    wide, anchored at the frame's left edge with its buttons on the left, and
    its message box is limited to 120 px high and scrolls.
15. Prompt fields are cleared on restart and close; focus returns to the
    canvas when a focused field disappears.
16. An auto-repeated Enter or Escape that the prompt guard ignores no longer
    spoils the field's select-all-to-replace state: `LineEdit::key` clears
    select-all unconditionally for Enter and Escape, so the field puts it back
    (`restore_select_all`) when the repeat is the one the guard drops.
17. Focus moving into a prompt field — by auto-focus or a later click — also
    invalidates the desktop's own IME mark (`cx.on_focus_in`); nothing reaches
    the page, since the engine already cleared that device's IME state when
    the dialog opened.

Helium 0.18.1.1 probe, same container as the audit above: while a dialog is
open, `Page.startScreencast`, `Page.stopScreencast`,
`Input.setIgnoreInputEvents` and `Page.screencastFrameAck` are answered in
0-1 ms; `Runtime.evaluate` and `Emulation.setFocusEmulationEnabled` are held
until it closes. A Go on a dirty page, then Stay, answers `net::ERR_ABORTED`
before `Page.javascriptDialogClosed`. A Reload replies before the dialog
opens. A page link, `location.href` or `location.reload()` during a pending
slow Go emits `Page.frameRequestedNavigation` right before its own
`beforeunload`, and the Go continues after Stay. A page `history.back()`
cancels the Go with `net::ERR_ABORTED` before asking.

New tests: fake CDP `dialog_text_and_prompt_defaults_are_bounded`,
`showing_a_device_during_its_dialog_waits_for_nothing_the_page_answers`,
`prompt_default_can_be_sent_back_and_rejected_answers_are_reported`,
`a_dialog_takes_one_answer_and_never_the_previous_dialogs`,
`a_dialog_answer_the_browser_refuses_can_be_sent_again`,
`staying_ends_the_broxser_navigation_that_asked_in_either_reply_order`,
`staying_ends_a_reload_that_asked`,
`leaving_continues_the_navigation_under_a_fresh_deadline`,
`staying_on_the_pages_own_link_keeps_the_broxser_navigation`,
`staying_ends_the_broxser_navigation_after_a_page_request_outside_its_tab`,
`a_dialog_drops_the_composition_without_a_cancel_or_a_not_responding_report`,
`a_dialog_broxser_cannot_show_is_reported_and_blocks_nothing`,
`a_refused_go_retires_the_devices_link_and_a_refused_synced_link_keeps_it`,
`a_stopped_runtime_leaves_no_dialog_shown`,
`a_dialog_refuses_reload_and_synced_links_and_drops_pointer_input` and
`a_new_document_a_crash_or_a_detach_ends_the_dialog_and_its_token`; live
Helium `live_dialog_survives_hide_show_scroll_and_zoom`, and
`live_dialogs_wait_for_an_explicit_answer` revised to wait for settled frames
and for the previous dialog to close.

Verification actually run: the fake CDP engine suite, now 82 tests, passed.
Live Helium: the three ignored dialog tests
(`live_dialogs_wait_for_an_explicit_answer`,
`live_dialog_survives_hide_show_scroll_and_zoom`,
`live_beforeunload_dialog_needs_an_explicit_leave_or_stay`) were re-run after
the link-retiring and dialog-answer-retry fixes and again passed 3 of 3 runs
each, and the full ignored suite (`--ignored`, 4 threads) was re-run and
passed 37 of 37. Desktop: 35 unit tests passed and strict Clippy is clean.

Real X11 window check of the revised panel, 27 September 2026: the same
container after a restart (Xvfb 1600 × 1000 without a window manager, xdotool,
debug build of `0fe8b01`, Helium 0.18.1.1 as the unprivileged user with the
sandbox enabled). A scratch page, not committed, offered a prompt with a
default, a 40-line alert, a confirm, a button that focuses a text field and
opens a prompt 3 s later, and reported every `keydown` it saw with its
`innerWidth` to the fixture log. Every step clicked and typed into the actual
GPUI window; results were read from the page's reports and from XGetImage
screenshots of the window.

| Check | Result |
| --- | --- |
| Prompt on the selected phone | The field took focus by itself with the default selected; typing `Grace` replaced it and Enter answered `Grace`; the panel closed; no page saw a key |
| Escape | Typing, then Escape answered `null` (cancelled); the panel closed |
| Prompt on the phone while the tablet is selected | No auto-focus: a typed `k` reached the tablet's page. A click into the field focused it; Tab, Left, Right, Shift+Insert, Ctrl+X and `q` then reached no page; Enter answered `late defaultq`; a typed `m` afterwards reached the tablet's page, so focus had returned to the canvas and the keyboard was not dead |
| 40-line alert | The message box stopped at 120 px and scrolled with the wheel; OK closed it. Wheel clicks over the message box also scrolled the canvas behind it (cosmetic; the page got nothing) |
| Confirm | Cancel answered `false`; the panel closed |
| Desktop device (1440 CSS px, a 720 px frame at 50 %) in a 700 px wide window, the other devices hidden | Panel 359 px wide at the frame's left edge, Cancel and OK at x 275–405 inside the window although the frame ran past its right edge; the field took focus and Enter answered `Fable`. At 560 px the window edge cut the panel with both buttons still visible; Escape cancelled |
| `scripts/desktop-smoke.sh` | 9 of 9 runs passed on the second attempt, including "dialog answered on the card", with no browser process or profile left. The first attempt after the container restart failed in its first case ("live close: browser did not start"): Helium's cold start did not publish its CDP endpoint within the 15 s startup limit and the desktop showed "Stopped: browser did not publish a CDP endpoint within 15 seconds"; the script exits on that failure without stopping the desktop it started, which is a gap in the script, not in this PR |

Not run: composing with an IME, then a prompt taking focus, then returning to
the canvas. The Fcitx5 runtime of P1.3 lived under `/tmp`, which the container
restart cleared, and this container's proxy now refuses the Arch package
mirrors (403), so it could not be rebuilt here; that check stays pending for a
machine with the runtime (`scripts/ime-smoke.py --manual-seconds 120`).

Flake note, kept honest: the frames check in
`live_dialogs_wait_for_an_explicit_answer` failed once under load — a frame
already in flight when the page stopped could still land after a fixed
300 ms sleep. The test now waits, via a `settled_frames` helper, until no
frame has arrived for 700 ms before reading the count.

## P1.5 modern application navigation, 26 September 2026 (cloud container)

Same container as P1.4: Helium 0.18.1.1 (Chrome/154.0.8037.57, protocol 1.3),
headless, run by the unprivileged user `broxsertest` with the sandbox enabled and
the profile seeded as in ADR 0004. Decisions are in
[ADR 0013](adr/0013-modern-navigation-sync.md).

### Probe: what Helium reports for each kind of navigation

A scratch Node script, not committed, drove one page target with Broxser's own
setup (device metrics, focus emulation, the link observer in its isolated world)
and clicked links on a local fixture through `Input.dispatchMouseEvent`. It
recorded the `Page` and `Runtime.bindingCalled` events. Scheduled-navigation
events are omitted below; `C` and `Y` are the observer's activation and
`beforeunload` confirmation reports.

| Scenario | Main-frame events | `main` at `e2bf9cb` |
| --- | --- | --- |
| Link → 302 → `/final`; 307 → 302 chain; 302 to another origin | `C`, `frameRequestedNavigation anchorClick`, `Y`, `frameStartedNavigating differentDocument` with one loader, `frameNavigated` of that loader at the final URL | Never synchronized: the committed URL differs from the link |
| Link → 302 → `/final#x`; link to `/final#x` | As above; `frameNavigated` has `url=/final` and `urlFragment=#x` | Never synchronized; the status showed `/final` |
| Link to an unreachable host | `frameNavigated url=chrome-error://chromewebdata/ unreachableUrl=<link>`, then Chromium's own `reload` of the error page after 1 s | Not synchronized, because the URLs differed |
| Hash link `#sec` | `C`, then only `navigatedWithinDocument fragment`; no request, loader or `beforeunload` | Never synchronized |
| Link whose handler calls `pushState` or `replaceState`; Navigation API `intercept` | `C`, then only `navigatedWithinDocument` (`historyApi`, `other`) | Never synchronized |
| Router that pushes 300 ms after the click | `C` at 8 ms, `navigatedWithinDocument` at 311 ms | Never synchronized |
| Router that calls `replaceState(current URL)` then `pushState(link)` | `C`, `navigatedWithinDocument` with the current URL, then with the link's URL | Never synchronized |
| Button that calls `pushState` or sets `location.hash`; script `a.click()` on a hash link | `navigatedWithinDocument` without any `C` | Not synchronized (correct) |
| Link inside an iframe (page, hash and `pushState`); main-document link with `target=frame` | Every event names the subframe; `C` arrives from the frame's own isolated context | Not synchronized (correct) |
| Link to a 204; redirect to a 204; download; redirect to a download | `frameStartedNavigating`, `frameStoppedLoading`, `downloadWillBegin` for downloads; no `frameNavigated` | Not synchronized (correct) |
| Second link 300 ms after a slow first one | The second `frameRequestedNavigation` and loader replace the first; only the second commits | Only the second synchronized (correct) |
| Go, or a script's `location.href`, 300 ms after a slow link | A new loader (`scriptInitiated` for the script) replaces the link's | Not synchronized (correct) |
| `Page.navigate` to the current document plus `#sec`, as a peer receives it | `frameStartedNavigating sameDocument`, `navigatedWithinDocument fragment`, no request | — |
| `history.back()` to a `pushState` entry | `frameStartedNavigating historySameDocument`, `navigatedWithinDocument fragment` | Not synchronized (no activation) |

The first probe run started Helium without the seeded profile preference. Helium's
bundled blocker then reloaded the page of the redirect chain 2.5 s after it
committed and held the plain redirect for 2.3 s (ADR 0004). With the preference,
no run showed either.

### Before the change

A temporary survey test loaded each fixture page on the three devices, clicked
the phone's link (or pressed Enter on the focused link) with navigation sync on,
and read the device URLs 3 s later:

| Page | Phone | Tablet (same session) | Desktop (other session) |
| --- | --- | --- | --- |
| `/redirect-chain` (307 → 302 → `/landed`) | `/landed` | `/redirect-chain` | `/redirect-chain` |
| `/redirect-fragment` (302 → `/landed#part`) | `/landed` (fragment lost) | `/redirect-fragment` | `/redirect-fragment` |
| `/redirect-away` (302 to `localhost`) | `http://localhost:…/landed` | `/redirect-away` | `/redirect-away` |
| `/fragment-link` (link to `/landed#part`) | `/landed` (fragment lost) | `/fragment-link` | `/fragment-link` |
| `/hash` (link to `#part`) | `/hash#part` | `/hash` | `/hash` |
| `/spa`, `/spa-late`, `/navigation-api`, `/spa-keyboard` (routers) | the route | the start page | the start page |

The new tests on `main` at `e2bf9cb`:

| Test | Before | After |
| --- | --- | --- |
| `redirected_link_commit_synchronizes_the_link_and_keeps_fragments` (fake CDP) | Failed: the status never showed `/landed#part` | Passed |
| `same_document_link_navigation_follows_only_a_live_activation` (fake CDP) | Failed: no `Page.navigate` reached the tablet | Passed |
| `live_link_sync_follows_redirects_and_fragments_with_the_link_url` | Failed: the tablet stayed on `/redirect-chain` | Passed |
| `live_same_document_link_navigations_sync_within_the_session` | Failed: the tablet stayed on `/hash` | Passed |
| `live_script_and_stale_same_document_changes_never_sync` | Passed | Passed |
| `live_subframe_navigations_never_sync` | Passed, after a fixture fix: the frame's hash link scrolled the scrollable page, so the next click missed | Passed |
| `live_cancelled_and_superseded_link_navigations_sync_at_most_the_latest` | Passed | Passed |

The four passing tests pin behavior that was already correct but untested.

### After the change

| Check | Result |
| --- | --- |
| `bash scripts/check.sh` | Passed: 1 CLI, 8 core, 65 engine and 24 desktop tests; fmt and strict Clippy; 34 live tests ignored by default |
| Live Helium suite (`--ignored`, 4 threads) | 34 of 34 passed in 53.4 s, including the five new tests and every earlier link, hidden-input, reload and deadline test |
| Five new live tests alone (3 threads) | 5 of 5 passed in 16.2 s |

The change touches the engine only; no desktop code changed, so no window check
was run.

### Limits

- `history.back()` and `forward()`, page-started cross-document navigations (meta
  refresh, `location.href`, forms), new tabs and downloads stay outside the sync
  contract, as in ADR 0006.
- Peers load an SPA route as a full document from the server; an application
  whose server does not serve its client-side routes shows its 404 on the peers.
- A same-document navigation follows an activation for up to 10 s. A slower
  router does not synchronize; a page that pushes the link's URL within that
  window for another reason does.
- The fixture serves 302/307/204 and dropped connections; it does not cover
  `Content-Disposition` downloads inside the live tests (the probe did, through
  `Browser.setDownloadBehavior: deny`).

## P1.4 frame quality and resource use, 26 September 2026 (cloud container)

Release builds in the same container as P1.3: Xvfb 1600 × 1000 without a window
manager, Mesa Lavapipe (Vulkan on the CPU), four shared cores, Helium 0.18.1.1,
desktop and browser as the unprivileged user `broxsertest` with the sandbox
enabled. The window was 1360 × 861 at the default 50% zoom. "Before" is `main` at
`e8d9944` with only the atlas patch, so that its runs could finish; "after" is this
change. Decisions are in [ADR 0012](adr/0012-live-frame-resources.md). These are
relative results under software rendering, not hardware budgets.

A scratch harness, not committed, pressed keys with XTest and read window pixels
with XGetImage. Latency is the time from a key press until a probed pixel of the
selected device's frame changes: 100 presses 250–350 ms apart on pages that flip a
region on every key. CPU and PSS of the desktop and of its browser process tree
were sampled for 30 s, and screenshots recorded each card's frame counters.

Without a window manager the GPUI window draws only after it has X input focus,
and a focus request sent before the window is mapped is lost. Early runs that
focused too soon measured an undrawn window (desktop CPU 10–20%, black window)
and were discarded. The harness now retries focus until the toolbar is drawn; one
run whose window took longer than 30 s to draw is excluded below.

### Typing while pages animate

| Release build | `main` `e8d9944` | With the atlas patch |
| --- | --- | --- |
| 3 devices, 100 presses 250–350 ms apart | 2 of 4 runs panicked, after 84 and 34 presses | 0 of 3 |
| 3 devices, 1500 keys 10 ms apart | 3 of 3 panicked | Passed in every smoke run below |
| 8 devices, 1500 keys 10 ms apart | 3 of 3 panicked: `blade_atlas.rs:229` twice, `:239` once | 0 of 3 |
| `desktop-smoke.sh`, new "typing while pages animate" run | Failed: exit 101 in `BladeRenderer::draw` | Passed |

A debug build of `main` did not panic in a 150-press run or two 1500-key runs; it
rarely reaches the race, so the smoke run needs a release build to cover it.

Review of the first smoke run found that it typed even when its readiness wait
timed out, so it passed with `BROXSER_HELIUM_BIN=/bin/false` and no fixture request
at all. It also left the desktop running when no window appeared. The run now
requires a fixture request and then reads the phone and tablet frames from the
window with XGetImage: each must change in six consecutive half seconds, which a
page that only finishes loading does not. It types only after that, and every path
closes the desktop and checks for leftovers. Requiring three fixture requests was
dropped: the two Guest devices share one browser context and often loaded the page
from its cache. A scratch copy of the script ran only this function:

| Case | Result |
| --- | --- |
| `BROXSER_HELIUM_BIN=/bin/false` | Failed: no fixture request within 30 s |
| Static `index.html`; held `/hang` | Failed: the phone frame did not animate |
| Animation fixture, release build with the patch | Passed 3 of 3 |
| Animation fixture, release build of `main` | Failed 2 of 2: the desktop panicked while typing |
| Same, typing at once instead of after 20 s of animation | The desktop panicked in 2 of 4 runs, so the run waits 20 s |

No case left a browser process, profile, window or temporary directory.

### Before and after, default zoom

Averages of two runs; the eight-device static page has one "before" run.

| Devices, page | Desktop CPU | Browser CPU | Desktop / browser PSS | Latency p50 / p95 / p99 | First device frame |
| --- | --- | --- | --- | --- | --- |
| 3, static | 1% → 1% | 1% → 1% | 152 / 431 → 148 / 426 MB | 55 / 89 / 97 → 58 / 93 / 98 ms | 0.95 → 0.97 s |
| 3, animated (1 off screen) | 172% → 177% | 128% → 77% | 162 / 457 → 157 / 442 MB | 124 / 170 / 219 → 99 / 131 / 161 ms | 0.98 → 0.91 s |
| 8, static | 1% → 1% | 1% → 1% | 171 / 505 → 148 / 474 MB | 58 / 105 / 113 → 63 / 98 / 110 ms | 1.41 → 1.39 s |
| 8, animated (5 off screen) | 147% → 152% | 170% → 97% | 198 / 578 → 156 / 514 MB | 238 / 318 / 371 → 129 / 173 / 189 ms | 1.40 → 1.36 s |

At the end of the eight-device animated runs (about 85 s), each visible device had
delivered about 2750 frames with 25% replaced before display (about 24 shown per
second) before the change, and about 4970 with 34% replaced (about 38 shown per
second) after it. Desktop CPU is dominated by Lavapipe drawing the window and did
not change. Static pages produce no frames, so pausing changes nothing there.

After 20 s of animation, scrolling the eight-device canvas down showed both
tablets at 68 frames, from before the first paint paused them; 3 s later they had
252 and 253, about 61 per second.

### HiDPI and device pixel ratio

A scratch engine test asked 360 × 640 devices with DPR 1, 2 and 3 for frames up to
three times their CSS size:

| Browser setting | Frames | Input and page |
| --- | --- | --- |
| Current launch | 360 × 640 for every DPR and limit | A click at CSS (40, 300) arrived at (40, 300) |
| `Emulation.setDeviceMetricsOverride` with `scale: 2` | None within 2 s | — |
| Helium started with `--force-device-scale-factor=2` | Up to 720 × 1280, following the limit, for every DPR | Clicks arrived at (40, 300); `devicePixelRatio` stayed 1, 2 and 3 |

At the same displayed size (three animated devices, 50% zoom, 1× window), the
forced scale factor raised browser CPU from about 75% to 124%, browser PSS by about
34 MB and p95 latency from 126–150 ms to 178–182 ms. Frames therefore stay at the
CSS size: sharp while zoom × window scale is at most 1, upscaled above it, for
example above 50% zoom on a 2× display. ADR 0012 records the trade-off.

### Long session

The release build of this change ran the three-device workspace on the animation
fixture on its own Xvfb display. A scratch harness typed 20 characters into the
phone, then sampled CPU and PSS of the desktop and its browser tree for 50 s, and
checked that the window was still drawn, 140 times: 117 minutes from 08:18 to
10:16 UTC. Two earlier attempts were cut off after 23 and 4 samples, when the
cloud container was reclaimed while this session was idle; their desktop logs were
empty and the window was drawn at every sample.

| Measure | Result |
| --- | --- |
| Window | Alive and drawn at all 140 samples; 15 processes throughout |
| Desktop PSS | 154–157 MB at every sample outside the overlap below; mean 155.8 MB over the first ten samples, 156.9 MB over the last ten |
| Browser PSS | 445–469 MB during the first ten samples, 467–473 MB up to sample 33, 475–481 MB in samples 61–100, 476–482 MB in samples 101–140 |
| CPU | Median 177.5% desktop, 72.1% browser outside the overlap |
| End | Ctrl+Q exit 0, no panic, no browser process or profile left |

Samples 34–60 overlapped the smoke and review runs below on the same four cores:
desktop CPU fell to 98% and PSS to 112 MB, because shared pages were split with the
other processes. Browser memory rose by about 8 MB after warm-up and did not rise
over the last 40 samples (34 minutes); a session of several hours on a real
desktop remains to be measured.

### Automated checks

| Check | Result |
| --- | --- |
| `bash scripts/check.sh` | Passed: 1 CLI, 8 core, 63 engine and 24 desktop tests; format and strict Clippy; 29 live tests ignored by default |
| Live Helium suite, four threads, beside the long session | Passed 29 of 29 twice (68.8 s, 64.1 s). One more run failed the cleanup check of `live_ime_rejects_scripted_focus_and_selection_after_synthetic_events`; see the next subsection |
| New `live_off_screen_devices_pause_frames_keep_input_and_resume_fresh` | Passed 3 of 3; without the on-screen check in `start_stream` it failed, because showing the device restarted its stream while off screen |
| `scripts/desktop-smoke.sh` with the debug and the release build | Passed all eight runs each, before and after the typing run's readiness fix; no browser process, profile or window left, and the known preview directory after SIGTERM |

### Browser cleanup deadlock under load (fixed later, see the P0 follow-up)

In the failed run, the IME assertions passed, but processes of the stopped browser
still ran 10 s after `LiveSession` was dropped. A renderer had crashed while the
browser was killed, and Helium's crash handler traced all its threads with ptrace
to write a dump. That renderer is the init of a sandbox PID namespace: its last
thread waited in the kernel's `zap_pid_ns_processes` for the traced threads to be
reaped, and the crash handler waited for that thread. The renderer zombie, its
namespace parent and two crash handlers were still there 90 s later. Killing the
tracing crash handler released all four within 5 s. `BrowserProcess::shutdown`
waits 5 s for processes that name the profile but does not kill them, and keeps
the profile while any does; the next start's stale-profile recovery (ADR 0007)
would stop them. Proposed separate change: after that wait, kill the survivors that
still name the private profile and wait once more.

### Limits

- Latency stays above the 50 ms p95 target in every scenario. The frame path
  includes JPEG encoding in the browser, decoding and software rendering on shared
  cores; frame age was not measured separately and is part of these numbers.
- When the whole window is minimized or obscured, GPUI stops painting, so devices
  that were on screen keep streaming and their frames are decoded but not uploaded.
- Not available here: Wayland scale changes, fractional scaling, multi-monitor
  setups, physical GPUs and HiDPI displays. X11 fixes the scale factor at startup,
  so the re-sent limits are a no-op on this display.

## P1.3 keyboard, browser keys and clipboard, 26 September 2026 (cloud container)

Same container and toolchain as P1.2 below. The desktop and its browser ran as
the unprivileged user `broxsertest` with the sandbox enabled. The audit, the
measurements and the decision are in
[ADR 0010](adr/0010-keyboard-identity-and-explicit-paste.md).

### Before the change

A scratch X11 harness, not committed, ran the debug desktop of `4c2d5c1` against a
fixture text area that reports key, input and paste events, typed with xdotool and
set the system clipboard with xclip. This Xvfb ignores keymap changes from
clients, so German layout runs used a second Xvfb started with a private copy of
the XKB data whose default layout is German. The results are the table in
ADR 0010; in addition, with the window given X input focus:

| Action on `4c2d5c1` | Result |
| --- | --- |
| German `/` held while the URL bar is clicked, Shift released first, then `/` twice in the page | The keyup reached the page at the click (371 ms); both later `/` were dropped |
| German Shift+7 released Shift first, then another device and back, then `/` twice | The keyup reached the page 327 ms late, at the device switch; both later `/` were dropped |

The engine then sent each key of ADR 0010's second table to a fresh Helium,
focused in a text area and in the page body, and listed the browser's page
targets through its loopback DevTools endpoint.

### After the change

| Check | Result |
| --- | --- |
| `bash scripts/check.sh` | Passed: 1 CLI, 8 core, 60 engine and 12 desktop tests, 6 of them new; format and strict Clippy; 24 live tests ignored by default |
| Live Helium suite, four test threads | Passed 24 of 24 twice, in 48.6 s and 48.8 s, including the two new tests |
| The browser-key live test without the engine's browser-key check | Failed: Ctrl+W closed the desktop device, and the text typed afterwards never arrived |
| German `/` with Shift released first, three times, across a device switch | All three typed; each keyup 1–12 ms after its key-down |
| German `/` held while the URL bar is clicked, Shift released first, then `/` twice | The keyup reached the page at the click (351 ms); both later `/` typed |
| German dead keys `´` `e`, `^` `a` | `Dead` without text, then `é`; `Dead`, then `â` |
| Ctrl+V with `pasted-from-system` in the system clipboard | Inserted as `insertText`; no `paste` event |
| Ctrl+C in the Guest phone, Ctrl+V in the Admin desktop | The Admin page received the system clipboard's `system-clipboard`, not `secret-guest` |
| Text selected in the Guest phone, middle click in the Admin desktop | No input in the Admin page |
| Ctrl+V held for 1.5 s (auto-repeat on: 660 ms delay, 25 per second) | Pasted once |
| 70,000 characters in the clipboard; no clipboard owner | Nothing pasted; the status bar showed "Nothing pasted: the clipboard text has 70000 characters, more than 65536." and "Nothing pasted: the clipboard holds no text." |
| F2, Insert, F12 | F2 (113) and Insert (45) reached the page; F12 did not |
| Ctrl+R, F5 | One document load each: Broxser's reload of the selected device |
| Ctrl+W, Ctrl+Shift+M, Ctrl+U, Alt+F4 or F12 in the Guest phone, then `ok` | No device closed, the runtime kept running, no document load; `ok` arrived |
| `navigator.clipboard.readText()` in the Admin page after a Guest copy, on a key press | Rejected: "Read permission denied" |
| `scripts/desktop-smoke.sh` | Passed all seven runs; no process, profile or window left, and the known preview directory after SIGTERM |

Limits:

- Xvfb without a window manager never gives the window X input focus, and GPUI
  then reports no focus changes: a key held while the URL bar was clicked was not
  released in the page, on `4c2d5c1` and with the change. The rows above that
  change focus ran after `xdotool windowfocus`, as a window manager would focus
  the window.
- Checked on X11 only, with US and German layouts, synthetic key events and no
  input method installed; not on Wayland, physical keyboards or other layouts.
- The browser keys were measured on Helium 0.18.1.1 only.
- GPUI's 4-second X11 clipboard timeout for an owner that does not answer was not
  reproduced.

## P1.2 restart and close transitions, 25 September 2026 (cloud container)

Same container and toolchain as P1.1 below: Rust 1.98.1, GPUI 0.2.2, Helium
0.18.1.1, Xvfb with Mesa lavapipe and xdotool, no window manager. The desktop and
its browser ran as the unprivileged user `broxsertest` with the sandbox enabled.
The audit and the decision are in
[ADR 0009](adr/0009-serialized-live-runtime-transitions.md).

### Before the change

A scratch X11 harness, not committed, ran the debug desktop of `f947727` against
the fixture, served with `Cache-Control: no-store` so that every document load is
counted. It killed the desktop's own browser by PID so that Restart appeared, sent
the action with xdotool, and recorded every profile directory created, one per
browser launch:

| Action | Runs | Browsers started by the action | Other observations |
| --- | --- | --- | --- |
| Restart once | 1 | 1 | 3 document requests, one per device |
| Restart twice without delay | 4 | 2 in every run, the second 9–113 ms after the clicks | The extra browser was stopped 46 ms after the next one started, by a drop on the UI thread |
| Restart, then Ctrl+Q | 2 | 1, 80–85 ms after the action, while the window was closing | Stopped with the window after 259–273 ms; the desktop exited after 332–336 ms |
| Restart twice, then Ctrl+Q | 2 | 2; the second 155 ms after the action, after the first had been stopped for the close | The desktop exited after 223–225 ms |
| Ctrl+Q, then Restart in the same event batch | 3 | 0 | The button was already hidden |

No stale frame was observed. Restart appears only after the worker has stopped
the browser and cleared its frames, and in the double Restart runs the replaced
browser was stopped before its first frame.

### After the change

| Check | Result |
| --- | --- |
| `bash scripts/check.sh` | Passed: 1 CLI, 8 core, 57 engine and 9 desktop tests, 5 of them new transition tests; format and strict Clippy; 22 live tests ignored by default |
| Same harness, fixed desktop | Restart once: 1 browser and 3 documents. Restart twice: 1 browser and 3 documents in 3 of 3 runs. Restart twice, then Ctrl+Q: no browser, exit after 171–176 ms, 2 of 2. Ctrl+Q, then Restart: no browser, 3 of 3 |
| Restart, then Ctrl+Q | In 2 of 2 runs the restart had started its browser 8–11 ms after the click, before the close was handled; the close then stopped it like a normal close, and the desktop exited after 222–224 ms |
| `scripts/desktop-smoke.sh` | Passed all seven runs with the fixed desktop, including the two new Restart runs: 1 browser started, and none with Ctrl+Q; no process, profile or window left. With the `f947727` desktop, the new "Restart clicked twice" run failed with 2 browsers started |
| Real window after a double Restart | One live runtime (`Live · Chrome/154.0.8037.57 · CDP 1.3`), frames streaming with new counters, no stale notice and no Restart button |
| Live Helium suite, four test threads | Passed 22 of 22 in 50.6 s; the engine is unchanged |

Limits:

- The "Restarting…" state lasts milliseconds, because Restart is offered only
  after the previous runtime has stopped; it was not captured on screen.
- Discarding stale frames and wake-ups is covered by the generation unit tests,
  not by a reproduced window run.
- Not checked: Wayland, physical GPUs, window managers, and a restart of a runtime
  that is still running, which the desktop does not offer.

## PR #7 reload ownership fixes, 25 September 2026 (local host)

Linux x86_64, Omarchy, kernel `7.2.6-arch2-Watanare-T2-4-t2`, Rust 1.98.1,
Helium 0.18.1.1 (`Chrome/154.0.8037.57`), unprivileged user and browser sandbox
enabled. Review of PR head `7507b4a` reproduced two deadline bugs in Helium:

| Before the fixes (load limit 2 s, observed after 3 s) | Result |
| --- | --- |
| Hold Reload, then follow a trusted link whose request is also held | The old reload's deadline canceled the replacement as well: two abandoned requests and a timeout error |
| Hold Reload while the old document repeatedly calls `history.replaceState` | The deadline disappeared: `loading: true`, no error and the request remained open |

Reloads now retain their main-frame loader identity. A distinct cross-document
navigation retires the old deadline; History API updates, subframes and navigation
requests without a start do not. Buffered starts from older commands cannot erase
the latest Reload deadline, and late replies cannot restore a retired deadline or
assign the old error to a replacement. Commit/stop/replacement observed before a
reply is remembered, including when that reply never arrives.

| Final verification | Result |
| --- | --- |
| `bash scripts/check.sh` | Passed: 1 CLI + 8 core + 57 engine + 4 desktop tests, format and strict Clippy; 22 live tests skipped by default |
| Full live Helium suite with four test threads | Passed 22/22 in 55.30 s, including both new regression tests |
| Held Reload with repeated `replaceState` / `pushState` | Stopped after 2025 / 2037 ms; request closed once without retry |
| Held Reload replaced by a trusted link / script navigation | Only the original request canceled; replacement remained loading beyond the old deadline without error; a subsequent explicit Reload got its own deadline |
| Fake CDP ordering and isolation regressions | Passed: queued reload starts with an interleaved page navigation, stale commit, early/late/missing replies, stale error, main-frame stop, request-only/subframe/same-document events and repeated loader notifications |

One default run failed in the unchanged
`guardian_waits_for_helpers_started_after_its_owner_died` test at its pre-kill
process-alive assertion. A full rerun passed without changes to guardian code or
that test. The cause, a race in the test helper, was found later and fixed through
PR 8 (see "Snapshot race in the late-helper test" under P0). The first two
Helium reproducers failed on the reviewed head as shown above. No GUI code changed;
a new window/Wayland/GPU check was not run for these engine fixes.

### Cloud container rerun after merging `main`

In the P1.1 cloud container, `live_reload_deadline_survives_history_api_changes`
failed at `!status.devices[0].loading` in four of five full live-suite runs with
four test threads (three of three after merging `main`, one of two on `9d12f2f`).
It passed six of six runs alone on an otherwise idle machine and failed on the
first traced run beside the rest of the suite. The traced page events showed that
Helium reports each History API update as `Page.frameStartedLoading` for the main
frame. An update sent before the browser handled `Page.stopLoading` arrived after
Broxser had reported the stop and marked the device loading again until its
`Page.frameStoppedLoading` 26 ms later. The final state was correct; the test now
waits until the device reports the stop while not loading. Afterwards `check.sh`
passed and the full live suite passed 22 of 22 in four of four runs (48.6–51.5 s),
the held reload stopping after 2026–2086 ms.

One earlier run also failed `live_link_sync_binds_slow_commit_and_preserves_long_url`,
unchanged since M1, with `browser processes still running` after close. Its sandbox
processes were stuck in the VM kernel (6.18): the init of a nested Chromium PID
namespace was a zombie whose last thread waited in `zap_pid_ns_processes` with
SIGKILL pending, the outer namespace init waited for it, and two crash handlers
stayed alive. As ADR 0007 describes, shutdown killed only the main browser process,
waited five seconds and removed the profile; the stuck processes were not
signaled. Later runs were not affected; the kernel-side cause was not established.

## P1.1 live command and navigation deadlines, 25 September 2026 (cloud container)

Same container and toolchain as P0 below, restarted before this work: Rust 1.98.1,
Helium 0.18.1.1 (`Chrome/154.0.8037.57`), browser tests as the unprivileged user
`broxsertest` with the sandbox enabled. The audit and the decision are in
[ADR 0008](adr/0008-live-command-and-navigation-deadlines.md).

### Before the change

Reproducers on `main` at `035b6fe`. The fake CDP peer answers setup like a browser
and withholds chosen answers.

| Reproducer | Result |
| --- | --- |
| Fake peer: the phone never answers input; 300 key events to it, then one key press to the desktop | Runtime stopped with `too many unanswered CDP commands`; the desktop's key events never arrived |
| Fake peer: the phone's navigation is never answered | 35 s later `loading: true`, `error: None` |
| Fake peer: the phone's first navigation hangs, Go starts a second, then the first fails late with `net::ERR_ABORTED` | The phone showed `Navigation failed: net::ERR_ABORTED; not retried` and `loading: false` while the second navigation ran |
| Helium: the desktop page blocks its main thread for 4 s on its first key; 300 key events to it | The runtime had stopped with the same error when checked 3.0 s later; the phone got no new frames and a click on it never arrived |
| Helium: the fixture holds the phone's reload | 35 s later `loading: true`, `error: None`, the request still open; the other devices kept streaming |

A scratch probe (not committed) measured how Helium answers: `Page.navigate` at
commit (24 ms) and not within 4 s while the fixture held the request;
`Page.reload` 4 ms after the reload started, then no event while it hung;
`Page.stopLoading` within 3 ms, which ended loading and closed the held requests.
A deadline on the reload's answer alone would therefore never fire, which the
first Helium run of the new test showed; the final design follows the reload
until the main frame commits or stops.

### After the change

| Check | Result |
| --- | --- |
| `bash scripts/check.sh` | Passed: 1 CLI, 8 core, 52 engine and 4 desktop tests; strict Clippy for default members and desktop; 20 live tests ignored by default |
| New default tests (fake peer) | Passed; the flood, navigation and superseded cases failed before the change as listed above, the single-click and reload cases were not run before |
| Whole live Helium suite | Passed 20 of 20 in each of four runs: two with two test threads (55.1–55.3 s) and two with four, as CI runs it (42.3–42.7 s) |
| Default engine tests, 25 consecutive runs as the unprivileged user | 25 passed |
| `scripts/desktop-smoke.sh` under Xvfb | Passed all five runs: Ctrl+Q after 160 ms, static close after 507 ms, and SIGKILL, Ctrl+C and SIGTERM without a browser process or profile left (the known preview directory after SIGTERM) |

| Scenario | Measured |
| --- | --- |
| Fake peer: 300 key events to a phone that never answers | The runtime kept running; 32 events reached the peer and the rest were dropped; the desktop's input and a Go for all devices arrived; after the phone answered, its next key press arrived and none of the dropped ones did |
| Fake peer: one unanswered click, command limit 1 s | "Not responding" after 1026 ms; later keys dropped |
| Fake peer: navigation and reload without an end, load limit 0.5 s | Stopped and reported at the deadline, `Page.stopLoading` sent once, nothing sent again; a later Go was sent once |
| Fake peer: superseded navigation fails late | The new navigation's status stayed without error |
| Helium: the fixture holds the phone's reload, load limit 3 s | Stopped and reported 3010–3027 ms after the reload was sent; the held request closed, the phone kept its page, the others kept streaming; four document requests, then a fifth only for the explicit reload |
| Desktop window under Xvfb against a server that accepts connections and never answers | After 30 s the device cards showed "Navigation got no response within 30 seconds; loading stopped…" in the warning color (truncated on the narrow phone card at 50% zoom); the runtime stayed live; SIGTERM afterwards left no browser process or profile |
| Helium: the desktop page blocks its main thread for 4 s; 300 key events | Runtime kept running and reported the desktop as not responding; a phone click arrived 80–244 ms after the flood and phone frames continued; after the page answered, the field held 16 characters, not 150, and the next key press was typed |

Findings and limits:

- Chromium can place devices of one session and site in one renderer process;
  the busy-page test therefore blocks the desktop, which is alone in its session.
  Blocking a phone page can stall the tablet in the same session as well.
- Input sent before the page stopped answering (at most 32 events) still arrives
  when it recovers; dropped releases can leave a key or button held in that page.
- Page-initiated navigations that hang are not stopped, and live mode has no load
  deadline after commit. A browser that stops answering is noticed only while a
  command is outstanding. Capture keeps per-operation limits without a job deadline.
- `xwd` captured the lavapipe-rendered desktop window as black until the pointer
  moved over it, so the window check above moved the pointer before capturing.
- Not checked: Wayland, a physical GPU, other distros and kernels, real slow dev
  servers, and how often long tasks on real applications reach the 15 s limit.

## P0 cleanup after owner death, 25 September 2026 (cloud container)

Ubuntu 24.04.4 container, kernel 6.18, x86_64, 4 vCPU, 15 GB RAM, `pid_max` 32768,
Rust 1.98.1, GPUI 0.2.2, Helium 0.18.1.1 from `scripts/fetch-helium.sh` (binary
SHA-256 `50d69027…40890`). Browser tests ran as the unprivileged user
`broxsertest` with the sandbox enabled. The container lacked `libxkbcommon-dev`
and `libxkbcommon-x11-dev`, so the first baseline `check.sh` failed to link the
desktop tests; after installing the packages CI also installs, baseline `d6e7e97`
passed (8 core, 28 engine and 4 desktop tests, strict Clippy) and the live suite
passed 14 of 14. X11 checks used Xvfb with Mesa lavapipe (`mesa-vulkan-drivers`
25.2.8) and Ubuntu's xdotool; nothing on the host desktop was changed.

### Before the change

A scratch copy of the reproducer, run on `d6e7e97` and not committed, recorded the
state five seconds after the owner died:

| Owner death | State five seconds later |
| --- | --- |
| Capture startup, fake browser: SIGKILL, SIGTERM, `abort()` | Browser running, profile present |
| Capture startup: SIGTERM to the owner's process group | Browser gone (it received the signal), profile present |
| Helium, held capture request: SIGKILL, SIGTERM | 15 of 15 browser processes running, CDP port open, profile present |
| Helium, live frames: SIGKILL, SIGTERM | 15 of 15 running, CDP port open, profile present |

A separate Python experiment (parent SIGKILLed, Helium on `about:blank`) measured
the candidate mechanisms for ADR 0007. With the CDP websocket held by the parent,
all 12 processes stayed alive for ten seconds with the port open.
`PR_SET_PDEATHSIG=SIGKILL` (through `setpriv`) and `--remote-debugging-pipe` both
stopped them within 0.5 s but left the profile. SIGKILL of only the main browser
process ended all 12 in 37–87 ms, including two `helium_crashpad` handlers that run
in their own sessions with parent PID 1.

Profiles were created with mode 0755 (the `tempfile` default under umask 022) and
Chromium kept it: `DevToolsActivePort` and some metadata files were readable by
other local users; `Default/` was 0700. Profiles are now created with mode 0700.

### After the change

| Check | Result |
| --- | --- |
| `bash scripts/check.sh` | Passed: 1 CLI, 8 core, 47 engine (one is the libtest entry point for re-executed roles) and 4 desktop tests; strict Clippy for default members and desktop; 18 live tests ignored by default |
| Default engine tests, 25 consecutive runs as the unprivileged user | 25 passed, repeated after the CI fix below |
| Whole live Helium suite | Passed 18 of 18 in 46 s with two test threads: the 14 earlier tests (slow-page reproducer six runs without failure, renderer crash reported after 40 ms) and 4 new ones. After the CI fix: 18 of 18 in 46.6 s with two threads, and in four of four runs with four threads as CI runs it (37.6–38.7 s each) |
| `scripts/desktop-smoke.sh` under Xvfb | Passed all five runs, before and after the CI fix; below |

Measured from owner death until every process of that instance (browser tree,
detached helpers and guardian) had exited and its profile was gone, over several
runs. Each test first proves the instance is running, then checks that another
instance in the same root keeps running, that the CDP port closed and that no
document request was replayed.

| Scenario | Owner death | Gone after |
| --- | --- | --- |
| Fake browser, capture and live startup | SIGKILL, SIGTERM, `abort()` | 26 ms |
| Fake browser, capture startup | SIGTERM to the process group | 5 ms |
| Fake browser, capture and live teardown | `abort()` before the kill or before profile removal | 45–65 ms |
| Helium, held capture request | SIGKILL, SIGTERM, `abort()` | 62–90 ms (16 processes with the guardian) |
| Helium, live frames | SIGKILL, SIGTERM, `abort()`, SIGTERM to the process group | 38–99 ms |
| Helium, live teardown | `abort()` before the kill or before profile removal | 53–70 ms |
| Real `broxser` CLI during a capture | SIGKILL | 26 ms |
| Helium orphan: owner and guardian killed, browser left running | Next browser start in the same root | 56–60 ms for 15 processes |
| Fake browser whose helper writes into the profile 0.3 s after the browser exits (added with the CI fix) | SIGKILL | 334–357 ms |

After the CI fix, a run with two test threads measured 66–97 ms for the held
request, 43–105 ms for live frames, 63–80 ms for live teardown and 60 ms for the
orphan. With four test threads, as CI runs the suite, the Helium cleanups took
60–240 ms and the orphan 73–83 ms.

| Desktop smoke run | Result |
| --- | --- |
| Live, Ctrl+Q | Exit 0 after 155 ms; 15 browser processes before, 0 after; nothing left |
| Static close during a held request | Exit 0 after 307–505 ms; nothing left |
| Live, SIGKILL | Exit 137; 0 browser processes and profiles when checked 69–71 ms later; window gone |
| Live, SIGINT to its process group (Ctrl+C) | Exit 130; 0 processes and profiles when checked 134 ms later; window gone |
| Static, SIGTERM during a held request | Exit 143; 0 processes and profiles after 68 ms; one preview directory left (below) |

After the CI fix all five runs passed again: Ctrl+Q exited after 104 ms, the
static close after 405 ms, and the SIGKILL, Ctrl+C and SIGTERM runs left no
browser process or profile when checked 69–71 ms after the kill (the same preview
directory remained after SIGTERM).

Guardian cost: one process per browser, not per frame, with one thread and 12.3 MB
VmRSS under the debug desktop binary (2.7 MB anonymous, the rest shared file
pages). A profile with its lease and a ready guardian took 4 ms warm and 26 ms
cold in tests. `ps` shows `broxser-guardian --broxser-browser-guardian`; the kernel
command name is `exe` because the guardian starts through `/proc/self/exe`.

Findings and limits:

- Pre-existing and also on normal close: each browser run leaves Chromium's
  process-singleton directory `org.chromium.Chromium.XXXXXX` (mode 0700, a dead
  socket and a cookie symlink, no page data) in the temporary directory, because
  Broxser stops browsers with SIGKILL. Not changed here; a candidate is pointing
  the browser's `TMPDIR` into its profile, followed by the whole live suite.
- Static preview directories (`broxser-preview-*`) belong to the desktop, not to a
  browser, and still remain after the desktop is killed.
- Profiles left by earlier Broxser versions have no lease and are never removed
  automatically; delete stale `broxser-cdp-*` directories once by hand.
- The container's PID 1 reaps orphans with a delay. Zombies count as exited, as
  `is_running` already did.
- A non-interactive shell starts background jobs with SIGINT ignored. The first
  Ctrl+C smoke attempt therefore left the desktop running while Helium stopped
  itself; the script now enables job control for that run, as a terminal does.
- Not checked: the kill scenarios on Wayland or a physical GPU, other distros and
  kernels, and macOS (unsupported; guardians need Linux 5.3+ pidfds). The System
  Design DOCX was not re-rendered.

### Profile written again after removal (CI run #29)

The first push of this change failed CI run #29. In
`live_next_start_stops_an_orphaned_browser_and_recovers_its_profile`, the next
browser's `shutdown()` returned success, yet its profile existed again when the
test listed the root (`profiles left: ["broxser-cdp-hNdglp"]`); the other 17 live
tests passed, and the pull-request run of the same commit passed. Seven whole-suite
runs here, three of them limited to two CPUs, and 90 shutdowns 0–120 ms after
startup did not reproduce it.

The owner's shutdown, its drop fallback, the guardian and recovery recorded the
browser's processes once, before the kill, and removed the profile as soon as
those had exited. Helium's processes, the crash handlers included, call `mkdir`
for `Default/` and `Crash Reports/*` when they write there (`strace`). A scratch
experiment that SIGKILLed the main browser 0.02–2 s after launch and deleted the
profile at once found it re-created about 200 ms later in 6 of 48 runs
(`Crash Reports/*` or `Default/Network Persistent State`); after waiting for every
process that descended from the browser or named the profile, in 0 of 56. Every
Helium process, including both detached crash handlers, carries the profile path
in its command line. A helper started after the record was never waited for; that
is the likely cause of the CI failure, which was not reproduced here.

All four paths now delete the profile only after the recorded processes have
exited and no running process names the profile, re-scanning until then within
the same five seconds; they never signal the processes found this way. Two new
tests use a fake browser whose helper, once the browser has exited, starts a new
process that writes `Default/Late` 0.3 s later. Before the change, the owner's
shutdown and the guardian (owner SIGKILLed) both left `Default/` behind in 8 of 8
runs; after it, 10 of 10 runs passed, the guardian finishing 334–357 ms after the
owner died.

### Snapshot race in the late-helper test (CI run #39)

`guardian_waits_for_helpers_started_after_its_owner_died` failed CI run #39 for PR 7
and its re-run with "the instance is not running before its owner dies"; the push
run of the same commit passed. The test helper recorded the browser's descendants
and then required every one of them to run. The fake browser's helper polls
`/proc` with `sleep 0.02`, so the record could hold a `sleep` that had exited by
the check; the guardian was not involved. Instrumented runs found one exited
process (empty command line) out of four. The test alone, in four parallel loops
of 150 runs as the unprivileged user, failed 23 of 600 runs on `035b6fe`.

The helper now keeps the recorded processes that still run and still requires the
guardian and the browser among them. A process that exited before the owner died
needs no cleanup, and the checks after the death are unchanged. Under the same
load the test then failed 0 of 600 runs on `035b6fe` and 0 of 1200 on PR 7's head;
`check.sh` passed, and the live Helium suite passed 18 of 18 twice with four test
threads (40.8–43.7 s).

## PR 4 review fixes 25 September 2026

Verified locally with Rust 1.98.1, GPUI 0.2.2 and the pinned Helium 0.18.1.1.
The six review findings now have regression coverage; the original independent
reproducers were rebuilt against the updated engine as an additional check.

| Check | Result |
| --- | --- |
| `bash scripts/check.sh` | Passed: 36 default tests (8 core + 28 engine), 4 desktop tests and strict Clippy for default members and desktop; CI now runs the desktop regression tests too |
| Desktop unit tests | Passed: 4, covering visible selection, all-hidden state, X11-style repeats and shifted ASCII key identity |
| Native desktop build | Passed |
| Whole live Helium suite | Passed: 14 tests, two test threads, including six slow-page runs with no foreign navigations or session extensions |
| Scripted link after ordinary typing | Original reproducer now navigates only the source; one effect request, peer remains on its original page |
| Genuine link with a three-second response | Both intended same-session devices arrive; two document requests |
| Long link | Source requests the intact 2,312-character path; no truncated peer request |
| Hidden input and individual reload | No hidden input callback or hidden reload request |
| Canceled click and interactive/editable descendants | Peer remains unchanged, including cancellation that clears timers and Enter inside an input nested in a link |
| Early extension announcement | Original fake CDP returns a qualification error before page navigation; created/changed and subsequently destroyed targets are covered in capture and live tests |
| Cancellation during stalled websocket upgrade | Original fake CDP returns typed `Cancelled` in 59 ms after acceptance; direct tests require under 500 ms, and capture including teardown under one second |
| Fragmented handshake | Valid headers separated by two gaps longer than 500 ms succeed within the overall deadline; expiry is separately tested |
| Native X11 visual/input check | Passed under Xvfb with Lavapipe: frames visible, hiding the selected device selects the visible alternative, all-hidden state has no input target, hidden sidebar selection is ignored, and a held key does not repeat into a re-shown device |
| Native Wayland visual/input check | Passed on Hyprland and the physical display at 112.5% scale, with `xwayland: false`: frame display, hide/show, visible selection, all-hidden state, key ownership and ignored hidden-row selection |
| `scripts/desktop-smoke.sh` | Passed: live close exit 0 in 718 ms; static close during held request exit 0 in 667 ms; both went from 15 browser processes to 0 with no owned profiles remaining |
| Integration with dependency PRs 1–3 | Scratch merge clean; 36 default tests and 3 live link-sync regression tests passed with tungstenite 0.30 and base64 0.23 |

Link authorization now requires a trusted isolated-world candidate and confirmation
before unload, a per-activation ID, a positive registered main-frame context ID,
and the matching requested URL and committing loader (ADR 0006). The old
recent-keypress heuristic and URL truncation are gone. Redirects to a different
final destination are deliberately not mirrored.

The X11 test used a separate virtual display; signed Xvfb, xdotool and software
Vulkan packages were extracted into a temporary directory without installation or
desktop configuration changes. A fixture logged keyboard callbacks by viewport:
phone received `a`, then no input while hidden; tablet received `b` and a held `h`;
after hiding all devices and re-showing the phone while `h` remained physically
held, the phone received no repeated `h`. After key-up, fresh `d` and `e` reached
the phone, including after an attempted selection of the still-hidden tablet.

After the desktop session was unlocked, the updated native binary was rebuilt from
`d5099fe` and checked on Wayland at 112.5% display scale. The compositor reported a
native Wayland window (`xwayland: false`). Compositor-generated mouse and keyboard
input exercised the same two-device fixture: phone logged `a`, tablet logged `b`
and `bh`, then phone logged only fresh `ad` and `ade` after hide/show and key release.
No `x` or held `h` reached the hidden/re-shown phone. Clicking the hidden tablet's
sidebar label did not redirect the subsequent key. All-hidden state was displayed;
normal window-manager close returned exit 0. Both GitHub CI jobs for `d5099fe`
were successful. This is a visual/input regression check, not a latency benchmark
or broad hardware/keyboard qualification. The System Design DOCX snapshot remains
older than the Markdown ADRs.

Keyboard suppression is conservative until a matching key-up. GPUI 0.2.2 exposes
logical names rather than physical keycodes, so unusual layout symbol changes or
key-ups lost outside the application still need platform qualification. These
limits do not permit forwarding input to a hidden device.

## M1 live frames, 24 September 2026 (cloud container)

Same environment and runtimes as M0 below. Live frames are CDP screencast JPEGs
shown in the GPUI window: frame streaming, not an embedded surface (ADR 0005).

### Automated checks

| Check | Result |
| --- | --- |
| `bash scripts/check.sh` | Passed: 27 default tests (8 core + 19 engine), strict Clippy, desktop check |
| Engine default tests, 25 consecutive runs | 25 passed, after the fake browser scripts moved to `testdata` (below) |
| `cargo clippy --locked -p broxser-desktop --all-targets -- -D warnings` | Passed |
| Live session tests, Helium 0.18.1.1 (`live_session -- --ignored`) | 5 passed; whole live suite 10 passed again after each core-dump change |
| Live session tests, Chromium 141 (explicit) | 5 passed, before and after the core-dump changes |
| `scripts/desktop-smoke.sh` under Xvfb | Passed three times (before and after each core-dump change): live close exit 0 after 207–210 ms, static close during a held request exit 0 after 307–508 ms; 15 browser processes before, 0 after, no temporary directory left |

| Live session test | What it proves |
| --- | --- |
| `live_session_streams_frames_and_cleans_up` | A page that ticks every 100 ms produces new frames on three devices without a capture action; one document request per device; drop removes browser and profile |
| `live_session_input_reaches_the_right_device` | Clicks at CSS (40, 300) on a DPR2 device and (50, 315) on a DPR1 device arrive with those `clientX/Y`; the wheel scrolls only the tablet; typed `ab` reaches only the desktop input |
| `live_session_sync_stays_in_session_without_loops_or_replay` | A link click on the phone navigates the same-session tablet once and not the other session; wheel sync mirrors inside the session; script-driven link clicks never synchronize; a restart loads `/` once and replays nothing |
| `live_session_bounds_frames_and_pauses_hidden_devices` | Unread frames are replaced and still acknowledged (frames keep flowing), at most one pending frame; a hidden device stops streaming and resumes |
| `live_session_reports_crashes_and_browser_exit` | A renderer crash is reported for one device and recovers only after an explicit reload; killing the browser stops the runtime with an error and removes processes and profile |

Headless screencasts delivered DPR2 viewports at CSS resolution (360×640 for a
360×640 DPR2 device) on both Helium and Chromium. Static capture keeps physical
pixels.

### Renderer crash reporting and core dumps

CI runs #10 and #11 (GitHub Ubuntu 24.04 runner) failed only
`live_session_reports_crashes_and_browser_exit`: after `Page.crash` the tablet
stopped producing frames, but `Target.targetCrashed` did not arrive within 10 s
(run #10) or 45 s (run #11). Run #11 listed the browser processes: the crashed
renderer's main thread was in state `I` with wait channel `do_exit`, where a thread
waits while another thread of its process writes a core dump. Locally the crash was
reported in about 60 ms: this container has `core_pattern=core`,
`suid_dumpable=0` and a zero core limit, so no dump is written.

Local reproduction on the VM kernel (6.18), test user, `suid_dumpable=2` and a
`core_pattern` pipe helper that skips the dump when its limit argument is 0 and
otherwise reads it. With `%c` as that argument (like apport) and
`ulimit -c unlimited`, the renderer (crashing thread `Chrome_ChildIOT`) streamed a
23,547,559,936-byte, mostly empty dump for 45.2 s and the test failed as on CI;
with `ulimit -c 0` the helper skipped the dump and the crash was reported after
60 ms.

First fix (`76f5d26`): the engine lowers the soft `RLIMIT_CORE` to 0 before
launching a browser. It passed locally with the `%c` helper, but CI run #12 failed
the same way and printed the runner's pattern:
`|/usr/lib/systemd/systemd-coredump %P %u %g %s %t 9223372036854775808 %h %d`.
systemd passes a fixed unlimited value instead of `%c`, so the limit is ignored;
every browser process had `core_limit=0` and the renderer was again in state `I`,
`do_exit`. A local helper given the same fixed value reproduced it: 15 s timeout,
renderer in the dump path.

Second fix (`9e09830`): the engine also writes 0 to `/proc/self/coredump_filter`,
which is inherited across fork and exec and leaves every memory mapping out of a
dump. Both settings apply to the Broxser process too (`SECURITY.md`). Results with
the fixed-value helper: each dump was 143,360 bytes (headers and register notes),
read in 6–10 ms; the crash was reported after 60–166 ms on Helium and 248 ms on
Chromium 141; all 10 live tests (Helium) and the 5 live session tests (Chromium)
passed. With the `%c` helper the zero limit still made it skip the dump (crash
reported after 72 ms). `browser_starts_without_core_dumps` reads the started
browser's limit and filter and fails without either change (`unlimited`,
`00000033`). The VM's kernel settings were restored afterwards. On the runner, with
its systemd-coredump pattern, CI run #13 passed and reported the crash after 222 ms.

That new test exposed a race in the test helpers: two of three `check.sh` runs
failed, and the logged failure was `Text file busy` when starting a fake browser
script that had been written at test time while another test thread forked. The
scripts are now checked in under `crates/broxser-engine/testdata/fake-browser`.

### X11 window checks

Unprivileged user, Xvfb and Mesa llvmpipe, debug build, `examples/workspace.json`
and a DPR2 variant, fixture `live.html` from `scripts/serve-fixture.sh`.

| Check | Result |
| --- | --- |
| Three live viewports | Shown; two screenshots one second apart differ only in the page's tick counter |
| Typing into the phone page and saving | Label saved on the phone only; tablet showed it after an explicit Ctrl+R reload (same session) |
| Wheel down on the phone | Page scrolled down (sign mapping correct) |
| Sync links and Sync scroll on, click "Page 2" on the phone at 38% | Phone and tablet on `?page=2`, desktop (`admin`) unchanged; scroll followed on the tablet, desktop stayed at 0 px |
| URL bar (Ctrl+L, text, Enter) | All devices on `?page=3` |
| DPR2 phone, clicks at 50% and 75% scale | Link under the pointer followed each time; tablet click at 75% went to the tablet only |
| Browser killed externally | Status `Stopped: read CDP websocket: … Connection reset …`, frames paused, 0 browser processes and profiles left; **Restart runtime** loaded the URL bar URL once and restored sync settings |
| Ctrl+Q and `WM_DELETE_WINDOW` with a live session | Exit after 293 ms and 245 ms, 15 browser processes before, 0 after, no profile left |
| `--static --capture-on-start` | Static previews still captured and closed cleanly |

### Measurements

Debug build under software rendering, three devices, `live.html` updating four
times per second. These are not hardware or release-build results.

| Measure | Result |
| --- | --- |
| Desktop RSS over 60 s | 221.5–227.0 MB, no growth (replaced frames are released) |
| Sum of Helium process RSS | About 1.4 GB (shared pages counted per process) |
| Desktop CPU, decoder unoptimized | About 3.2 cores: 1.5 JPEG decode, 1.2 llvmpipe, 0.4 GPUI main thread |
| Desktop CPU, decoder optimized in dev profile | About 2 cores: 0.25 decode, 1.25 llvmpipe, 0.5 GPUI main thread |
| Live runtime worker thread | About 2% of one core |
| Frames replaced before display (sync check, unoptimized decoder) | Phone 45 of 236, tablet 48 of 234, desktop 0 of 175; the latest frame is always shown |

Input latency and frame smoothness were not measured.

### Manual desktop checklist (not possible in this environment)

Run on the developer's Wayland (Hyprland) session and on an X11 desktop with a
physical GPU, release build, `live.html` and a representative company app:

- [ ] Window opens and draws without extra events; resize and move keep input mapping correct.
- [ ] HiDPI and fractional scaling: frame sharpness, especially DPR>1 devices; click accuracy.
- [ ] Input latency (target p95 below 50 ms) and CPU/GPU use with 3 and 8 devices.
- [ ] Keyboard layouts, shortcuts (Ctrl+L/R/Q and F5 not reaching pages, browser keys dropped), paste from other apps, IME (not supported yet). Xvfb evidence is in the P1.3 section.
- [ ] Touch devices with mouse input; text selection and drag inside pages.
- [ ] Popups, downloads, permissions, file upload, JavaScript dialogs (unsupported, must fail visibly).
- [ ] Close, window-manager close and `scripts/desktop-smoke.sh` on that desktop.
- [ ] Hours-long session memory and multi-monitor scale changes.

## M0 reliability, 24 September 2026 (cloud container)

### Environment

- Ubuntu 24.04.4 container, x86_64, 4 vCPU, 15 GB RAM, no GPU and no desktop session.
- Rust 1.98.1 (`rustc 48a229cea 2026-09-01`), GPUI 0.2.2, same `Cargo.lock`.
- Helium 0.18.1.1 portable, fetched by `scripts/fetch-helium.sh`; archive SHA-256
  `9f4d3523…5042c` matched the manifest. Extracted `helium` binary SHA-256
  `50d69027…40890`. `Browser.getVersion`: `Chrome/154.0.8037.57`, protocol 1.3.
- Comparison runtime, selected explicitly: Playwright's Chromium 141.0.7390.37,
  `/opt/pw-browsers/chromium-1194/chrome-linux/chrome`, SHA-256 `85918a99…d5a1a95`.
- The container runs as root. Chromium refuses to start as root unless the sandbox
  is disabled, which Broxser never does, so every browser test ran as an unprivileged
  user (`broxsertest`) with the namespace sandbox enabled.
- Desktop checks used Xvfb (X11) with Mesa llvmpipe software Vulkan. This proves a
  real X11 window and input path, not a physical GPU, compositor or Wayland session.

### Automated checks

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo test --locked` (default members) | 22 passed: 8 core + 14 engine; 5 live tests ignored by default |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo clippy --locked -p broxser-desktop --all-targets -- -D warnings` | Passed (first full desktop-only run); only the upstream `proc-macro-error2` future-incompat notice |
| `cargo build --locked -p broxser-desktop` | Passed |
| Live engine suite, Helium, `-- --ignored` | 5 passed |
| Live engine suite, Chromium 141 (explicit), reproducer only | Passed, guarded and baseline |

Default engine tests use fake browser scripts and a fake CDP websocket, so they run
without a browser: browser exit before CDP, startup timeout, cancellation during
startup, a websocket handshake longer than the 500 ms poll interval (regression
guard for the fixed handshake issue), unanswered command timeout and a closed
websocket. Each asserts cleanup by process identity (PID plus kernel start time,
browser tree plus processes naming the profile) under a unique profile root.

Live tests, run as the unprivileged user:

```bash
BROXSER_TEST_BROWSER="$PWD/.local/helium/helium" \
  cargo test --locked -p broxser-engine -- --ignored --nocapture
```

| Live test | Helium 0.18.1.1 |
| --- | --- |
| `live_capture_has_expected_pixels_and_isolated_sessions` | Passed: DPR2 600×600, DPR1 300×300; same session shares a cookie and a context, different session is isolated |
| `live_load_timeout_is_reported_and_cleans_up` | Passed: stalled body reports a load timeout for `phone`, no further device, held request closed |
| `live_cancel_during_active_request_stops_browser` | Passed: cancel while the server holds the request returns in under 3 s, browser and profile gone, request closed, no second navigation |
| `live_partial_failure_keeps_earlier_frames_and_is_not_retried` | Passed: `phone.png` kept, `tablet` fails with `net::ERR_EMPTY_RESPONSE`, one navigation per device |
| `live_slow_page_reproducer` | Passed (guarded); see below |

### Slow-page `net::ERR_ABORTED`: cause and fix

`live_slow_page_reproducer` serves every document after 2 s from a test-owned
server on a random port. Three targets (phone DPR2 and tablet in `guest`, desktop
in `admin`) navigate sequentially and in parallel, five iterations each. The
output records loader IDs and navigation types without URLs. Command per mode:

```bash
BROXSER_TEST_BROWSER=<executable> BROXSER_REPRO_ITERATIONS=5 [BROXSER_REPRO_BASELINE=1] \
  cargo test --locked -p broxser-engine live_slow_page_reproducer -- --ignored --nocapture
```

| Runtime and mode (10 runs each) | Failed | Unrequested navigations | Blocker in session contexts | Extra document requests |
| --- | --- | --- | --- | --- |
| Helium, baseline (foundation behavior) | 4 (all `tablet`, sequential) | 4 `reload`, browser-initiated | 10 of 10 runs | 0 |
| Helium, guarded (this change) | 0 | 0 | 0 | 0 |
| Chromium 141, baseline | 0 | 0 | 0 | 0 |
| Chromium 141, guarded | 0 | 0 | 0 | 0 |

Example baseline failure: `navigation failed for tablet: net::ERR_ABORTED
(superseded by a browser-initiated reload navigation that Broxser did not
request); not retried`, own loader `60BEA267…`, reload loader `F655E37B…`.
Baseline parallel runs did not abort but held two of three document requests for
about 4.7 extra seconds. Guarded parallel runs finish the same-session tablet in
about 4 s because Chromium serializes identical in-flight requests of one context.

Root cause: Helium bundles uBlock Origin as component extension
`blockjmkbacgjkknlgpkjjiijinjdanf` and enables it in off-the-record contexts by
default. Each Broxser BrowserContext starts its own instance; after loading filter
lists it calls `tabs.reload` on tabs that made requests earlier. The bundled code
and Helium patches are cited in [ADR 0004](adr/0004-helium-bundled-blocker-in-session-contexts.md).
Scratch exploration showed `--disable-extensions` (3 of 6 runs failed) and
`--disable-component-extensions-with-background-pages` (1 of 6 failed) do not help.
The fix writes `extensions.settings.<id>.incognito = false` into Broxser's private
profile and adds a fail-closed gate for extension pages in session contexts.
There is still no automatic navigation retry.

### Desktop window, refresh retention and close

Run as the unprivileged user under Xvfb, fixture from `scripts/serve-fixture.sh`.
Without a window manager, GPUI drew its first frame only after a configure event
(`xdotool windowsize`); later frames and input worked.

| Check | Result |
| --- | --- |
| Launch and capture | Three Helium previews displayed; status `3 static previews · Chrome/154.0.8037.57 · CDP 1.3` |
| 15 refreshes via the capture button | RSS 220.5 → 220.6 MB, exactly 2 preview generations (184 KB each), 0 profiles and 0 browser processes between captures |
| Ctrl+Q while idle | Exit 0 in about 200 ms, no temporary directories left |
| Ctrl+Q while a server holds the request (2 runs) | Exit 0 after 518–618 ms; 15 browser processes during capture, 0 after; 0 profiles and previews |
| `WM_DELETE_WINDOW` while the request is held (2 runs) | Exit 0 after 180–607 ms; same cleanup result |
| SIGTERM while idle | Process ended; the preview directory remained |
| SIGTERM while a request is held | 15 orphaned browser processes, profile and preview remained |

Before this change, Ctrl+Q during a capture ended the process in 57 ms and left the
browser, profile and preview behind: GPUI 0.2.2 stops its Linux event loop when the
last window is removed, so background jobs never reached cleanup. The window now
stays until the cancelled capture has stopped its browser. Wayland could not be
checked here: GPUI 0.2.2 panics without a `wl_seat`, which headless Weston lacks.

### Other findings

- Chromium keeps its crash database under the default user data directory
  (`~/.config/net.imput.helium/Crash Reports` for Helium) even with
  `--user-data-dir`. Broxser now sets `BREAKPAD_DUMP_LOCATION` inside the private
  profile; checked with a temporary `XDG_CONFIG_HOME` that stayed empty.
- Helium opens the shared NSS database `~/.local/share/pki/nssdb`, where corporate
  CAs and client certificates live. Isolating it is an open product decision.

## Foundation, 24–25 September 2026 (local host)

Developer host: Linux x86_64, Wayland/Hyprland, same toolchain, GPUI and Helium
baseline. Archive SHA-256 verified against the official release metadata.

| Check | Result |
| --- | --- |
| Default workspace tests on Rust 1.98.1 | 12 passed: 8 core + 4 engine |
| Strict Clippy for default members and all their targets | Passed with `-D warnings` |
| Native GPUI build on Rust 1.98.1 | Passed; final application opened as a native Wayland window |
| Native preview and explicit refresh | Three actual Helium images displayed; screenshot in `docs/images/preview.png` |
| Normal window close | Application exited with code 0 |
| Live Helium test | Passed; DPR2 produces 600×600 PNG, DPR1 300×300 |
| Same-session sharing and different-session isolation | Passed with a cookie-backed fixture |
| Public fixture capture through CLI | Passed: 390×844, 768×1024, 1440×900 PNGs, inspected visually |
| CLI repeat/failure behavior | Existing workspace preserved; unique runs; failed navigation leaves prior reports untouched |
| Script syntax and CI YAML structure | Checked locally |

That host first exposed the slow-page `ERR_ABORTED` and a too-short websocket
handshake timeout. The handshake fix (15 s during HTTP upgrade, 500 ms polling
afterwards) is now guarded by `websocket_handshake_may_outlast_the_poll_interval`.
Stopping the initial blank load or precreating all targets did not prevent the
abort; M0 above explains why. An independent review had resolved stale CLI
reports, unbounded GPUI image retention and Ubuntu 24.04 AppArmor setup.

The System Design DOCX was rendered and inspected at that time. It has not been
re-rendered since; see the note at the top of `system-design.md`.

## Remote CI

GitHub Actions (Ubuntu 24.04 runner, sandbox with a path-scoped AppArmor
profile) runs format, default tests, Clippy, a desktop compile and the whole live
engine suite with Helium.

| Run | Commit | Result |
| --- | --- | --- |
| #9 | `9c14532` (M0) | Passed, including the reproducer with 6 runs and no failure |
| #10 | `42bb535` (M1 desktop) | Failed: crash not reported within 10 s; the other 9 live tests passed |
| #11 | `8d1060f` (crash diagnostics) | Failed: crash not reported within 45 s, renderer in the core dump path; the other 9 passed |
| #12 | `76f5d26` (zero core limit) | Failed: systemd-coredump ignores the limit, crash not reported within 15 s; the other 9 passed |
| #13 | `9e09830` (zero `coredump_filter`) | Passed: all 10 live tests, crash reported after 222 ms, reproducer 6 runs with no failure |

## P1.3 native IME — 26 September 2026

ADR 0011 extends PR #10 (`bcdb855`) with a selected-canvas input handler,
target-bound CDP composition, and observed caret geometry. Verified on Linux with
Helium 0.18.1.1 (`Chrome/154.0.8037.57`), GPUI 0.2.2 plus the two documented native
IME patches, Fcitx5 5.1.22, Pinyin addons 5.1.14, libime 1.1.16, private Xvfb,
and Mesa Lavapipe 26.2.2. Browser sandboxing remained enabled.

| Check | Result |
| --- | --- |
| `CARGO_BUILD_JOBS=2 bash scripts/check.sh` | Passed: formatting, strict Clippy, 1 CLI + 8 core + 63 engine + 24 desktop tests (96 total). 26 live tests ignored here and run separately |
| `BROXSER_TEST_BROWSER="$PWD/.local/helium/helium" cargo test --locked -p broxser-engine -- --ignored --test-threads=2 --nocapture` | Passed: 26/26, 140.60 s |
| `cargo build --locked -p broxser-desktop -j 2` | Passed; actual GUI executable used below |
| Real Fcitx5/Pinyin XIM, extended scenario | Passed: inline preedit, `你好` twice, Escape cancellation, two `n` commits with no DOM key-up, then a textarea commit without changing the first input |
| Candidate placement | Visually checked under the input caret, then the textarea caret, at 50% canvas scale in an actual GPUI X11 window |
| Final native event trace | Six composition starts/ends, 28 updates, final input `你好你好nn`, textarea `你好`. No DOM key-down/up events were needed for those commits |
| Close after native IME | Exit 0 and zero browser profiles remaining; private Fcitx/Xvfb processes stopped by the harness |
| Vendored GPUI audit | Original crate SHA-256 verified; only `wayland/client.rs` and `x11/xim_handler.rs` differ from upstream source. License retained; transitive Cargo versions unchanged |
| Python harness | `python3 -m py_compile scripts/ime-smoke.py` passed |

Native command:

```bash
python3 scripts/ime-smoke.py \
  --desktop target/debug/broxser-desktop --browser .local/helium/helium \
  --output /tmp/broxser-ime-handoff-verified --capture-private-root --scenario extended
```

The Arch-hosted harness expects the seven pinned archives listed in its
`PACKAGES` constant in `BROXSER_IME_PACKAGES` (default
`/tmp/broxser-ime-runtime/packages`), plus `bsdtar`, Python, Fcitx5, D-Bus and
ImageMagick. It verifies SHA-256 against the local authenticated `extra.db` before
extracting the temporary runtime. Set `--packages`, `--runtime` and `--database`
when using other retained locations; no packages are installed into the system.
It creates private configuration, starts Fcitx with cloud Pinyin disabled,
enables `UseOnTheSpot=True`, and waits for both the XIM server and active Pinyin
context. Full-display capture is allowed only for the Xvfb server it creates.
The host IME daemon and configuration are untouched.

Logs, native `summary.json`, fixture events and reviewed screenshots are retained
locally in `artifacts/p1-3-ime/` (ignored by Git). The actual private native run was
`/tmp/broxser-ime-handoff-verified`; earlier passing extended evidence remains in
`/tmp/broxser-ime-final-extended2`.

Failures found and repaired during qualification:

- The original XIM context was never focused: Fcitx saw no active context and
  forwarded Latin keys. Sending `SetIcFocus` produced native commits. Fcitx's
  default off-the-spot mode sends no application preedit; the private test
  explicitly enables on-the-spot mode.
- Empty cleanup after a commit created a phantom composition and swallowed the
  next field's first word. Cleanup no longer creates an origin; pointer-down also
  retires the old caret token before new composition can latch it. A queued old
  status snapshot cannot rearm that token while the worker processes the click.
- Review reproduced an old commit migrating after cancellation plus empty
  preedit, inactive-window input, synthetic events preserving stale target tokens,
  and a vertically misplaced caret in tall inputs. Regression coverage now
  exercises these cases; native deletion records a boundary for the next preedit.
- One full live run exposed `Runtime.evaluate` overtaking earlier key dispatch.
  The target check now waits for preceding input responses (no longer within a
  fixed 250 ms bound; see the next subsection). Another run exposed a
  pre-existing link-test predicate accepting the previous `/next` status; it now
  waits for the new source request before checking the unchanged no-sync
  assertion. The complete final suite passed.
- Initial sandbox-only runs could not bind fixture sockets; tests requiring
  localhost/browser/window access were rerun outside that sandbox. Private Xvfb
  initially lacked a usable Vulkan driver; a verified temporary Lavapipe package
  supplied it. Formatting and Clippy findings were fixed before the passing run.

Not qualified: native Wayland IME end-to-end, physical keyboards, other IME
languages/engines, all fractional scales, password/iframe/shadow-root/canvas
editors, surrounding-text replacement, or all cancellation callback orderings.
In particular, after cancellation without a terminal callback, an ambiguous
commit-only input is dropped rather than moved to a new target. Frames and
candidate/DOM updates are asynchronous; a streamed preedit can trail the latest
DOM update. GPUI's existing numeric-fallback warnings and the existing
`proc-macro-error2` future-compatibility warning remain non-failing.

### Slow answers before an IME commit (PR #10 CI, 26 September 2026)

CI on `4f4e54d` failed `live_ime_rejects_scripted_focus_and_selection_after_synthetic_events`:
after the three attacks, the two genuine `✓` commits never produced a value with
both check marks. The 25 other live tests passed on that runner, which was also
writing browser core dumps for the guardian tests. The identity check blocked the
runtime for at most 250 ms, including the wait for the page's answer to earlier
input; on timeout it dropped the IME action and invalidated its target, so the
next commit was dropped as well.

Reproduction in the cloud container (user `broxsertest`, sandbox on):

| Check on `4f4e54d` | Result |
| --- | --- |
| The CI test alone five times, then twelve times beside six busy loops | Passed each time: load alone did not reproduce it |
| Full live suite, four threads, three times | Passed 26 of 26 each time |
| Scratch test with debug logging: the page answers the first commit after 400 ms, a second commit follows | The second commit was dropped after 251 ms with the first still unanswered; the page kept one `✓` |
| New test `live_ime_waits_for_a_slow_page_without_dropping_or_reordering_input` | Failed: both commits dropped, only `x` typed |
| New test `live_input_held_behind_an_ime_check_is_dropped_when_hidden` | Failed as expected without a hold: the commit was dropped at 250 ms and the key typed after it reached the page |

The check now waits without blocking the runtime: the read is sent once earlier
input is answered, its answer sends or drops the action, and later input to that
device waits behind it in order under the ordinary input budget and deadline.
Navigation, a new document, hiding, a crash or an unresponsive page drops the
waiting action and that input (ADR 0011).

| Check with the change | Result |
| --- | --- |
| The two new tests, three runs each | Passed: `✓✓x` with every reported value a prefix; after hide and show nothing held was sent, and the next commit arrived alone |
| The three older IME live tests | Passed |
| `bash scripts/check.sh` | Passed: 1 CLI, 8 core, 63 engine and 24 desktop tests; format and strict Clippy; 28 live tests ignored by default |
| Live Helium suite, four test threads | Passed 28 of 28 twice, in 40.3 s and 40.0 s |
| Same suite beside six busy loops on the four cores | Passed 28 of 28 in 90.9 s |
| `scripts/desktop-smoke.sh` under Xvfb with the rebuilt desktop | Passed all seven runs; no browser process or profile left, and the known preview directory after SIGTERM |

Not rerun: the native Fcitx5 smoke, since Fcitx5 is not installed in this
container. The desktop code did not change.

## Open gates

- The M1 manual desktop checklist above, on Wayland and a physical GPU.
- systemd-coredump still records browser crashes with small dumps that hold
  register state. A browser change that resets the inherited `coredump_filter`
  would bring back large dumps and delayed crash reports; the live crash test and
  its diagnostics would show it.
- Owner death is handled by guardians and profile leases (ADR 0007, P0 section
  above). Still open: static preview directories and Chromium's singleton socket
  directory after a kill, profiles in roots never used again, and qualification of
  the kill scenarios on company desktops, distros and kernels.
- Physical GPU, Wayland compositors, fractional scaling, IME and accessibility
  need a real desktop session; the cloud check above is X11 on software Vulkan.
- Input methods: ADR 0011 qualifies Fcitx5/Pinyin over XIM with a native input
  handler, preedit, commit and caret placement. Native Wayland IME, physical
  keyboards, other language engines and complex editors remain unqualified.
  The browser keys that the engine drops must be measured again on each Helium update.
- Before a team rollout: multiple distro/GPU combinations, SPA readiness,
  popup/download/clipboard behavior, permission policy, persistent storage
  isolation, engine updates and rollback. Performance budgets in System Design are
  proposed targets, not benchmarks.
