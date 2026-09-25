# ADR 0004 Keep Helium's bundled blocker out of Broxser session contexts

Status: accepted for M0 reliability, 2026-09-24. Re-qualify with every Helium update.

## Context

Slow pages intermittently failed with `Page.navigate` → `net::ERR_ABORTED` on the
second device, in both the desktop app and the CLI. The automated reproducer
`live_slow_page_reproducer` (engine crate, test-owned HTTP fixture on a random
port, 2-second document delay, three targets with two in one session, DPR2 and
DPR1, sequential and parallel navigation) showed the cause on Helium 0.18.1.1:

- Helium bundles uBlock Origin as a component extension,
  `blockjmkbacgjkknlgpkjjiijinjdanf`. Helium's patches make it follow the normal
  per-extension incognito preference and default that preference to enabled.
- Every CDP `Target.createBrowserContext` context is an off-the-record profile,
  so each Broxser session starts its own blocker instance (a `background_page`
  target appeared inside the session contexts in 10 of 10 unguarded runs).
- While that instance loads its filter lists it holds or records requests. The
  bundled Chromium platform code then calls `vAPI.tabs.reload(tabId)` for every
  tab it saw before it was ready (`unsuspendAllRequests`).
- The reload supersedes an in-flight Broxser navigation. The trace shows the
  failing tablet navigation (loader `60BEA267…`) answered with
  `net::ERR_ABORTED` and a browser-initiated `reload` navigation (loader
  `F655E37B…`) starting on the same frame. The fixture has no navigation script.

Unguarded Helium failed 4 of 5 sequential runs; parallel runs did not abort but
held two of three document requests for about 4.7 extra seconds. Explicitly
selected Chromium 141 had no extension target and no failure in 10 of 10 runs,
guarded or not. `--disable-extensions` (3 of 6 failed) and
`--disable-component-extensions-with-background-pages` (1 of 6 failed) do not
remove the blocker: Helium adds it with the always-loaded session components.

## Decision

- Before the first launch, Broxser writes its own private profile's
  `Default/Preferences` with `extensions.settings.<blocker id>.incognito = false`.
  The blocker then stays out of every Broxser session context. With this, Helium
  passed 10 of 10 runs with no unrequested navigation and no extra document request.
- Captures fail closed if any extension background page or service worker runs
  inside a Broxser session context. This capability gate turns a future Helium
  change into an explicit qualification failure instead of intermittent reloads.
  ADR 0006 adds bounded retention of discovery events that precede the context
  creation response; the context is rechecked before page targets are opened,
  including when the observed extension target has already been destroyed.
- Navigation errors report a superseding navigation and whether the page or the
  browser started it. Broxser never retries a navigation automatically.
- Crash dumps go to the private profile through `BREAKPAD_DUMP_LOCATION`;
  otherwise Chromium writes them to the default Helium configuration directory,
  shared with a personal installation.

## Options rejected

| Option | Reason |
| --- | --- |
| Retry after `ERR_ABORTED` | Replays navigation and hides side effects |
| `Page.stopLoading` or creating all targets first | Earlier experiments failed; neither affects the extension |
| Standard extension switches | Measured ineffective for this component |
| Wait for blocker readiness | Couples to extension internals, adds seconds per context and still reloads |
| Managed policy | Requires system-wide configuration outside the app-owned profile |
| Use Chromium instead | Changes the user's runtime choice; Chromium stays a diagnostic comparison |

## Consequences

Pages inside Broxser sessions render without Helium's content blocker. That is
closer to a stock Chromium visitor but differs from Helium users who keep the
blocker; document it for QA. Helium's other defaults, such as fingerprint noise,
remain and still need evaluation. The blocker still starts in the unused default
context and may fetch filter assets according to Helium's defaults; disabling it
there or Helium services entirely needs a separate, measured decision.

The fix depends on Helium keeping the incognito preference semantics. Each engine
update runs the live suite, including the reproducer in guarded and
`BROXSER_REPRO_BASELINE=1` modes, before promotion (ADR 0003).
