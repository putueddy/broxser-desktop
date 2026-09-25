# Validation evidence

Evidence per milestone. It is not production qualification or a claim of Sizzy
parity. Keep failed, skipped and manual-only results visible.

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
- [ ] Keyboard layouts, shortcuts (Ctrl+L/R/Q not reaching pages), IME (not supported yet).
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

## Open gates

- The M1 manual desktop checklist above, on Wayland and a physical GPU.
- systemd-coredump still records browser crashes with small dumps that hold
  register state. A browser change that resets the inherited `coredump_filter`
  would bring back large dumps and delayed crash reports; the live crash test and
  its diagnostics would show it.
- If the Broxser process is killed (SIGTERM, SIGKILL, crash), `Drop` does not run:
  the browser keeps running with its loopback CDP port and temporary files remain.
  Candidate fixes: pipe transport, parent-death signal and a stale-profile sweep.
- Physical GPU, Wayland compositors, fractional scaling, IME and accessibility
  need a real desktop session; the cloud check above is X11 on software Vulkan.
- Before a team rollout: multiple distro/GPU combinations, SPA readiness,
  popup/download/clipboard behavior, permission policy, persistent storage
  isolation, engine updates and rollback. Performance budgets in System Design are
  proposed targets, not benchmarks.
