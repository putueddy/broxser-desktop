# Validation evidence

Evidence per milestone. It is not production qualification or a claim of Sizzy
parity. Keep failed, skipped and manual-only results visible.

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
- Owner death is handled by guardians and profile leases (ADR 0007, P0 section
  above). Still open: static preview directories and Chromium's singleton socket
  directory after a kill, profiles in roots never used again, and qualification of
  the kill scenarios on company desktops, distros and kernels.
- Physical GPU, Wayland compositors, fractional scaling, IME and accessibility
  need a real desktop session; the cloud check above is X11 on software Vulkan.
- Before a team rollout: multiple distro/GPU combinations, SPA readiness,
  popup/download/clipboard behavior, permission policy, persistent storage
  isolation, engine updates and rollback. Performance budgets in System Design are
  proposed targets, not benchmarks.
