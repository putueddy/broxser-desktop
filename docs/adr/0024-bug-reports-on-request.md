# ADR 0024 Bug reports: a device's screenshot and a redacted report, written only on request

Status: accepted for P2.3 (second part), 2026-09-27. Builds on ADR 0023 (the
device console), ADR 0013 (addresses can carry codes and tokens) and ADR 0019
(what the headless browser represents).

## Context

`GOALS.md` (P2.3) asks for screenshots and an export useful for bug reports,
with sensitive data redacted and a clear retention. Before this change the live
desktop exported nothing: a developer who saw a bug on one device used a system
screenshot tool on the scaled canvas and copied console text by hand. The CLI
capture (`broxser capture`) writes one PNG per device and a JSON report of
browser and frame sizes into a run directory, without console output or page
address. Live frames are screencast JPEGs sized to the display (ADR 0012), so at
50 % a 390 × 844 phone arrives at about 195 × 422 pixels.

Constraints found while auditing:

- `Page.captureScreenshot`, which capture already uses, returns a PNG of the
  device's viewport at its device scale on the running page without events in
  it; a JavaScript dialog freezes the page (ADR 0014).
- GPUI's file dialogs need the desktop portal, missing here (ADR 0022).
- Console text is whatever the page printed (ADR 0023); addresses in it and
  the page's own address can carry codes and tokens (ADR 0013).

## Options

| Option | Assessment |
| --- | --- |
| Save the displayed frame | Scaled to the window, JPEG; a poor bug report |
| A fresh `Page.captureScreenshot` on request | Exact viewport at the device scale, PNG, already used by capture |
| Clipboard only | Nothing on disk, but one item at a time and nothing to attach later |
| Files in a reports folder | Screenshot and text together, attachable, and the user sees where they are |
| Next to the workspace file | Often inside a repository; screenshots could be committed |
| Redact every secret | Not possible for free text; claiming it would mislead |
| Redact what has a recognizable shape, say what remains | Honest and useful |

## Decision

- **Save report** in the Console panel asks the live runtime for a screenshot of
  the selected device (`Command::Screenshot`, `Page.captureScreenshot` as PNG of
  the viewport). The runtime answers with a checked PNG or a reason: the device
  is hidden, the page's dialog is open, another screenshot is in flight, the
  reply is no PNG, or no reply came within the command limit. It is no page
  input, so it never makes a page "not responding".
- The desktop then writes a new folder `<date>-<time>-<device id>` with
  `screenshot.png` and `report.md` into the reports directory:
  `BROXSER_REPORT_DIR` if set (absolute), else `Broxser` in the XDG download
  directory (`user-dirs.dirs`), else `~/Downloads/Broxser`. Folders are mode
  0700 and files 0600; an existing name gets a number. Writing happens off the
  UI thread, and the panel shows the folder or the error.
- `report.md` names the device (size, scale, mobile, touch), its session, the
  page, browser product and protocol, Broxser's version, the time in UTC and the
  screenshot size, then the device's console oldest first, as it was when the
  user clicked. The page's address keeps only scheme, host and path. Console
  text loses the query, fragment and user information of HTTP(S) addresses,
  JWT-shaped tokens and bearer credentials (`broxser-core::redact_text`); the
  report says that anything else the page printed remains and asks for review.
  Console lines are indented, so a tracker shows them as code.
- **Retention**: nothing is written without the click. Broxser never reads a
  report back, sends it anywhere or deletes it; the files are the user's until
  the user deletes them. Nothing of a report goes to Broxser's output or logs.
- In the Console panel, Save report is filled in the accent color and Clear in
  the danger color.

## Consequences

- One click gives a bug report with an exact screenshot and the console of the
  device that showed the problem; several devices need several reports.
- The screenshot shows whatever the page showed, including personal data.
- Redaction is best effort by design; the panel still shows console text as
  logged (ADR 0023), only the exported text is redacted.
- A page frozen by its dialog cannot be reported until the dialog is answered.

## Validation

Unit: `addresses_keep_only_scheme_host_and_path` and
`text_loses_address_secrets_and_tokens_only` in `broxser-core`;
`a_report_names_the_device_and_keeps_secrets_out`,
`reports_go_to_new_private_folders_in_the_reports_directory`,
`the_reports_directory_follows_the_override_then_the_download_directory` and
`times_are_utc_calendar_dates` in the desktop. Fake CDP:
`screenshots_are_taken_on_request_checked_and_bounded`. Helium:
`live_screenshots_show_each_viewport_at_its_scale`. Real window: the desktop
smoke run "console panel saves a redacted report". Details in
`docs/validation.md` (P2.3, second part).
