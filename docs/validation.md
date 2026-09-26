# Validation evidence

Evidence per milestone. It is not production qualification or a claim of Sizzy
parity. Keep failed, skipped and manual-only results visible.

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

### Browser cleanup deadlock under load (not changed here)

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
