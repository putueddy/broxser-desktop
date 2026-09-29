# ADR 0027 Graphite & Signal: one visual system for the desktop shell, with bundled Geist fonts

Status: accepted, 2026-09-29, at the owner's request for a modern look. It
changes how the desktop looks and where its panels sit; it changes no engine
behavior, no workspace or state format and no guarantee of ADRs 0005–0026.

## Context

The shell grew control by control: a green-tinted dark theme with nine color
constants, the system sans-serif, text-only buttons, and one panel at a time
opening between the device list and the canvas, which pushed every card 340 px
to the right. Device errors were cut to one line, a closed popup's report was
as wide as its device with its buttons on the right (outside the canvas, which
does not scroll sideways, for a wide device), and counts read "1 window(s)
closed". The design canvas "Broxser Revamp" (claude.ai artifact) drew the
replacement for every screen and state of the live and static modes.

Constraints:

- `scripts/desktop-smoke.sh` finds controls by solid fill colors and window
  regions; each kind of control needs its own fill.
- GPUI 0.2.2 on Linux renders with whatever fonts fontconfig offers unless the
  application registers its own; SVG icons are tinted alpha masks loaded
  through the application's asset source.
- A device frame is a streamed image, not a page (ADR 0005); nothing in the new
  look may suggest otherwise.

## Decision

- **Palette** (`crates/broxser-desktop/src/theme.rs`): graphite neutrals
  (canvas `#0B0C0E`, chrome `#111316`, cards `#15181C`), one lime accent
  `#C6F26B` taken from the Broxser fixtures, info `#7CB4FF`, warn `#F2B35E`,
  danger `#FF7B72`. Caption text `#8A929C` keeps 5.9:1 on chrome. Status colors
  differ in lightness as well as hue, and every state also carries a word or
  an icon. Sessions get a tag color by their order in the workspace.
- **One solid fill per kind of action**: lime confirms (Go, Apply, Save when
  the draft is unchanged, Open here, Dismiss of a download, Save report,
  Restart runtime, the preset pluses), coral removes or clears (Remove, Clear,
  the console badge with errors), raised `#171A1E` does the rest. Filled
  buttons do not change color on hover. Streaming dots are 5 px, too small to
  pass for a filled control.
- **Type**: Geist and Geist Mono (SIL Open Font License 1.1), five static faces
  compiled into the desktop and registered at startup; addresses, sizes,
  counts and console text are monospaced. Without them GPUI falls back to the
  system faces.
- **Icons**: 30 stroke icons drawn for Broxser, embedded as SVG and served by
  the asset source before any file path.
- **Layout**: a 52 px toolbar (brand and mode, address with Go, Reload, Sync
  links and scroll as one segmented control, a zoom stepper, and the Workspace
  and Console buttons at fixed widths), a 256 px device list with fixed row
  heights, the canvas, the Workspace or Console panel at 380 px right of the
  canvas with tabs and a close button, and a 32 px status bar across the
  window. Opening a panel no longer moves the cards.
- **Cards**: the name with its session tag, size, scale and touch; the
  address or the state, where errors wrap up to three lines; the frame with
  rounded corners (the image is painted with the same radius); streaming state
  and counts; the page's closed windows and refused downloads as one
  pluralized line. Every card report (dialog, popup, download) is at most
  360 px wide with its buttons at its left edge.
- The dot grid and the accent choice drawn on the design canvas are not
  implemented: repainting a grid of dots with every frame costs CPU on the
  software renderers the smoke runs on (ADR 0012).

## Options

| Option | Assessment |
| --- | --- |
| Restyle colors only | Leaves the moving cards, cut errors and the popup report's reach unsolved |
| Follow the desktop's GTK or KDE theme | GPUI has no theme bridge on Linux; per-desktop results could not be tested |
| System fonts | No dependency, but metrics and weights differ per distro; the fixed geometry the smoke relies on would drift |
| Bundle Geist | About 680 KB in the binary and one more license to carry; consistent metrics |
| Panel left of the canvas (as before) | Cards jump 340 px whenever a panel opens |
| Panel right of the canvas | Cards stay put; matches the list-left, inspector-right convention |

## Consequences

- `scripts/desktop-smoke.sh` uses the new colors and regions; its window
  checks passed on Xvfb with Lavapipe for this change (`docs/validation.md`).
- `scripts/sbom.py` lists the fonts as `geist-font` (OFL-1.1) contained in
  `broxser-desktop` and refuses a font file whose SHA-256 differs from
  `crates/broxser-desktop/fonts/fonts.json`; `scripts/package.sh` ships the
  license as `licenses/geist-OFL.txt`.
- Updating the fonts means copying files from a reviewed upstream commit and
  updating `fonts.json`, `fonts/README.md` and `NOTICE.md`.
- The window still needs a check on a physical GPU, Wayland and fractional
  scales before release; the palette has no light variant yet.
