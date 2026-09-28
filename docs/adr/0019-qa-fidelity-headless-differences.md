# ADR 0019 QA fidelity: what the headless browser shows pages, measured and aligned

Status: accepted for P1.7, 2026-09-27. Amends ADR 0004 and ADR 0005.

## Context

Broxser runs Helium headless with Broxser's device setup. P1.7 asks whether
a page in Broxser sees the browser a user would run, before results are called
representative. A scratch probe loaded one fingerprinting page, with Broxser's
setup for a touch phone and a mouse desktop, in headless Helium 0.18.1.1
(Broxser's configuration), headed Helium on an X display, and Chromium 141
headless and headed, several runs each, and recorded what the page saw and
what the server received. Differences between Broxser's configuration and a
headed Helium, on this machine (no GPU, Xvfb):

| What | Headless Helium (Broxser before) | Headed Helium | Cause |
| --- | --- | --- | --- |
| `navigator.userAgent`, `User-Agent` header | `HeadlessChrome/154.0.0.0` | `Chrome/154.0.0.0` | Headless mode marks its user agent; pages and bot walls read it |
| Client hints (`Sec-CH-UA`, `userAgentData`) | Same brands (`Chromium`, `Google Chrome`, greased) | Same | Helium reports the Chrome brand |
| `(hover)` and `(pointer)` media on a mouse device | `hover: none`, `pointer: none` | `hover: hover`, `pointer: fine` | Headless Chromium detects no pointing device; touch emulation still gives touch devices `coarse` and `none` |
| `screen.width × height` on a desktop device | 800 × 600 behind a 1440 × 900 viewport | The X screen | Device metrics without a screen size keep headless Chromium's default screen; mobile emulation sizes the screen to the viewport |
| WebGL renderer | ANGLE on SwiftShader (software) | No WebGL at all on this display; a user's GPU on theirs | No GPU here |
| Canvas and audio fingerprints | Change every session | Change every session | Helium adds per-session noise; Chromium 141 is stable across runs |
| `navigator.hardwareConcurrency` | 2 in one run of five, 4 otherwise | 4 | Helium noise or scheduling; Chromium reports 4 every time |
| `navigator.deviceMemory` | 16 | 16 | Helium reports the machine; Chromium caps at 8 |
| Storage quota | 10 GB | 10 GB | Helium; Chromium about 1 GB |
| `Accept-Language`, time zone, locale, fonts, permissions, `webdriver` (false), plugins, `chrome` object | Same | Same | — |
| Requests to `/ads/` and `/tracker/` paths on the fixture | Made | Made | The bundled blocker is kept out of session contexts (ADR 0004), so pages in Broxser see no content blocking, unlike a user's Helium whose normal profile blocks listed third parties; local paths are not on any list either way |

`Emulation.setEmulatedMedia` with `hover` and `pointer` features had no
effect; `Emulation.setUserAgentOverride` with only a user agent string drops
every client hint. Omitting optional brands and versions from its metadata
does preserve the browser's defaults, as the
[Chromium regression test](https://chromium.googlesource.com/chromium/src/+/ada95e1c23f32870621efd684130ecfea582b087/third_party/blink/web_tests/http/tests/inspector-protocol/emulation/emulation-user-agent-metadata-override.js)
and the review's Helium probe confirm. However, a target-level override changes
the page and its dedicated worker while a service worker and its requests keep
the headless user agent. The browser-wide launch flags preserve consistency:
`--blink-settings` with the Blink pointer and hover settings restores a mouse
for the whole browser (touch emulation still wins on touch devices), and
`--user-agent` with the headed string keeps the client-hint brands, leaving
only the platform-specific high-entropy hints (`Sec-CH-UA-Arch`,
`Sec-CH-UA-Full-Version-List`) blank. The browser's version for that string
comes from `Browser.getVersion` in a blank, fully owned discovery browser.

## Options

| Option | Assessment |
| --- | --- |
| Document the differences only | Honest, but a page that hides hover UI or blocks headless browsers still misleads QA |
| Align what can be aligned at launch and setup | Two flags and two metric fields; the rest is documented |
| Emulate a full profile (GPU strings, no noise) | Pretends to be a machine Broxser is not; QA of Helium-specific noise would then be wrong |

## Decision

- Headless launches get `--blink-settings` for a fine pointer with hover and
  `--user-agent` with the same browser's headed user agent, built from the
  native user agent returned by `Browser.getVersion`, replacing only its
  `HeadlessChrome/` product token. The version, platform and other bytes come
  from that browser rather than a synthesized Linux string or CLI output.
- Live and capture share one connected-start path. Start a private, guarded
  browser on `about:blank` and query its version through bounded, cancellable
  CDP. No workspace context, target or navigation is created during discovery.
  If its user agent needs normalization, disconnect and require successful
  cleanup of the browser, helpers, profile and guardian before starting one
  replacement with the browser-wide override. Recheck cancellation immediately
  before spawning. The replacement is never probed or restarted recursively.
  An absent, unsupported or already-headed user agent retains the first
  browser. A CDP, timeout, cancellation or cleanup error stops startup;
  it never authorizes another launch or a navigation retry.
- Device setup gives non-mobile devices a screen the size of their viewport
  (`screenWidth`, `screenHeight`), in live and capture runs.
- Keep iframe setup paused as ADR 0016 requires, but explicitly include worker
  target types in auto-attach. The pinned browser can otherwise pause excluded
  workers without giving Broxser a session to resume. Validated owned workers
  are resumed once and detached only after acknowledgement using the existing
  bounded cleanup path; no iframe/Page setup or page-input ownership is granted.
- Everything else stays as the browser has it and is documented: SwiftShader
  WebGL where the machine has no GPU, Helium's per-session canvas and audio
  noise and its memory and storage reports, and no content blocking in
  session contexts (ADR 0004). Results are representative of a Helium user on
  a similar machine with the blocker off, not of a Chrome user, and pixel
  comparisons of canvas output are not stable across sessions.

## Consequences

- Pages see `Chrome/<major>` with consistent brands, hover-capable mouse
  devices and a screen that fits the viewport; touch devices are unchanged.
- The user agent retains the native platform and version. The qualified runtime
  is Linux Helium; this does not add support for another operating system.
- Two high-entropy client hints are blank; a site that requires them sees a
  browser that withheld them, as many do.
- Headless startup normally launches two browsers sequentially with separate
  private profiles: discovery and the aligned runtime. Discovery closes before
  the second launch and never loads the workspace. Each endpoint wait and CDP
  command keeps its existing per-operation deadline; there is no new global
  startup SLA. Closing the app can cancel either phase. No `--version` child,
  stdout reader thread or unbounded output allocation remains.

## Validation

Unit/fake CDP: native UA normalization, sequential ownership, discovery failures,
cancellation and owner death in `browser/qa_fidelity.rs`, plus the launch
argument test. Helium: `live_pages_see_the_browser_a_user_would_run` (no
headless marker, brands present, hover and fine pointer with a viewport-sized
screen on the mouse devices, coarse and no hover on the touch phone; it failed
before the change with `HeadlessChrome`, `hover: none` and an 800 × 600 screen).
Additional live and capture tests verify page/worker/network UA consistency and
that discovery never requests the workspace URL. The X11 smoke accounts for
the two sequential launches while still accepting only one restart transition.
The probe's measurements and review corrections are in `docs/validation.md` (P1.7).
