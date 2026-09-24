# Foundation validation

Local verification, 24–25 September 2026. This is evidence for the initial
integration spike, not production qualification or a claim of Sizzy parity.

## Environment

- Linux x86_64 development host, Wayland/Hyprland.
- Rust 1.98.1, selected by the user; workspace minimum Rust 1.98.
- GPUI exactly 0.2.2 with its default Wayland/X11 features.
- Helium official portable Linux 0.18.1.1, Chromium 154.0.8037.57.
- Download SHA-256 verified against the official release metadata and recorded in
  `runtime/helium-linux-x86_64.json`.
- `Browser.getVersion` reports `Chrome/154.0.8037.57`, protocol `1.3`, even when the
  launched executable is Helium. Browser product alone is not brand attestation.

## Automated checks

| Check | Result |
| --- | --- |
| Default workspace tests on Rust 1.98.1 | 12 passed: 8 core + 4 engine; live test deliberately ignored by default |
| Strict Clippy for default members and all their targets | Passed with `-D warnings` |
| Native GPUI build on Rust 1.98.1 | Passed; final application opened as a native Wayland window |
| Native preview and explicit refresh | Three actual Helium images displayed; screenshot in `docs/images/preview.png` |
| Normal window close | Application exited with code 0 |
| Live Helium test on Rust 1.98.1 | Passed; one viewport DPR2 produces 600×600 PNG, DPR1 produces 300×300 |
| Same-session cookie sharing | Passed: two device targets observe the same cookie-backed fixture |
| Different-session cookie isolation | Passed: separate context observes a different cookie-backed fixture |
| Public fixture capture through CLI | Passed: 390×844, 768×1024, 1440×900 PNGs, inspected visually |
| CLI repeat/failure behavior | Existing workspace preserved; unique capture runs; failed navigation leaves prior reports untouched |
| Owned browser cleanup | No new Broxser profile browser processes after CLI success and navigation failure |
| Script syntax and CI YAML structure | Checked locally |

Commands used the same sources/lockfile with `CARGO_TARGET_DIR` set to a temporary
build cache for core/engine/CLI while GPUI compiled in the workspace. Equivalent
developer commands are in README.md. The live test serves its own local HTTP fixture
on an ephemeral port and launches a fresh sandboxed Helium profile.

The original three-view fixture used DPR1. The final live integration additionally
tests DPR2 and asserts report dimensions against the actual PNG header. Capturing
pixels and reading a cookie fixture do not validate every storage API, partitioning
rule, extension, authentication flow, privacy feature or rendering engine.

## Known runtime issue

A fixture delaying each response by two seconds intermittently triggers
`Page.navigate: net::ERR_ABORTED` on the second device. It occurred through both
the native app and CLI. CDP tracing showed a phone-target `reload` event while the
tablet navigation was in flight; the fixture contained no navigation JavaScript.
This observation does not establish the root cause. Stopping the initial blank
load or precreating all targets did not prevent it; those experiments were reverted.
The fast deterministic integration test and several slow runs succeeded, but the
slow-page case is not qualified as reliable. The app reports failure and requires
an explicit refresh; workspace URLs are never automatically retried.

Native QA also exposed a too-short websocket handshake timeout. That was fixed by
using the 15-second command timeout during HTTP upgrade, then switching to the
500-ms polling timeout only after websocket establishment. Native capture succeeded
after that change. Do not conflate this fixed handshake issue with the unresolved
navigation cancellation above.

## Review and document checks

An independent source review found and resolved stale CLI reports across runs,
unbounded GPUI image retention, and Ubuntu 24.04 AppArmor setup for the downloaded
browser. The desktop now uses a per-capture image cache with explicit release;
only two PNG generations remain on disk while images may still load.

The System Design DOCX uses the retained System Design template. All seven rendered
pages were visually inspected, and the retained reference remains unchanged.
The source-of-truth v1 fields are in `broxser-core` and `examples/workspace.json`.

## Remaining release qualification

Remote CI has not yet run for this initial repository. Its Ubuntu AppArmor profile
is scoped to the exact Helium executable; the browser sandbox remains enabled.
Do not interpret successful local checks as a tested Ubuntu package.

Active-close automated instrumentation did not establish reliable process/profile
ownership, so it is not counted as a passed cleanup test. Normal close was observed;
the close-during-capture and parent-crash cases still need dedicated qualification.
Desktop-only strict Clippy was interrupted during dependency checks; default-member
strict Clippy and the actual desktop build passed. An upstream `proc-macro-error2`
future-compatibility warning remains for a later dependency update.

Before a team rollout: exercise multiple distro/GPU combinations, X11 and Wayland,
fractional scaling, IME/accessibility, SPA readiness, popup/download/clipboard
behavior, permission policy, parent crashes/SIGTERM, sustained refresh memory,
global cancellation, persistent storage isolation, engine updates and rollback.
Performance budgets in System Design are proposed targets, not benchmarks.
