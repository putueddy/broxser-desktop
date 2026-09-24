# Validation evidence

Evidence per milestone. It is not production qualification or a claim of Sizzy
parity. Keep failed, skipped and manual-only results visible.

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

## Open gates

- Remote CI has not run for these commits yet.
- If the Broxser process is killed (SIGTERM, SIGKILL, crash), `Drop` does not run:
  the browser keeps running with its loopback CDP port and temporary files remain.
  Candidate fixes: pipe transport, parent-death signal and a stale-profile sweep.
- Physical GPU, Wayland compositors, fractional scaling, IME and accessibility
  need a real desktop session; the cloud check above is X11 on software Vulkan.
- Before a team rollout: multiple distro/GPU combinations, SPA readiness,
  popup/download/clipboard behavior, permission policy, persistent storage
  isolation, engine updates and rollback. Performance budgets in System Design are
  proposed targets, not benchmarks.
