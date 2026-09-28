# ADR 0016 Downloads and file choosers are refused and reported

Status: accepted for P1.6 (third capability), 2026-09-27. Amends ADR 0005.

## Context

Broxser shows device pages; it has no place to save a file and no file to give
a page. A CDP probe against Helium 0.18.1.1 (Chrome 154) and a run of the live
runtime at `6fefab7` found:

- A link to an attachment (`Content-Disposition`), a link with a `download`
  attribute, an `application/octet-stream` answer, a POST answered as an
  attachment, a `data:` or `blob:` URL with a `download` attribute, a redirect
  ending in an attachment, and a script that sets `location` to one all start a
  download. Headless Chromium then cancels it at once: the page's session
  reports the deprecated `Page.downloadWillBegin` and a `downloadProgress` of
  `canceled`, and no file appears in the profile, the working directory or
  `~/Downloads`. The document request is still sent; the server sees it and the
  page stays where it was.
- With `Browser.setDownloadBehavior` `deny` for the browser context and
  `eventsEnabled`, the same happens explicitly and the browser itself reports
  `Browser.downloadWillBegin` (frame, URL, suggested file name) and the
  cancellation. The frame is the device's main frame or a subframe; a download
  from a cross-site frame, which another renderer process runs, is reported only
  at the browser level and only with that frame's ID, which the device session
  saw attached and then detached with reason `swap`.
- A suggested file name is derived by the browser and is untrusted: the probe's
  `filename*` with a right-to-left override, a newline, a bell and an escape
  sequence came back as `_fdp.exe___[31m.txt`.
- `Page.navigate` to a download answers `net::ERR_ABORTED` with
  `isDownload: true` and no `loaderId`. Broxser reported that as a navigation
  error on every device.
- Clicking `<input type="file">` (single, `multiple`, `webkitdirectory`), a
  scripted `click()` with a gesture, and the `showOpenFilePicker`,
  `showSaveFilePicker` and `showDirectoryPicker` APIs open no chooser in the
  headless browser: the input's `cancel` event fires and the picker promises
  reject with `AbortError`. A scripted click without a gesture does nothing.
  With `Page.setInterceptFileChooserDialog` the session reports
  `Page.fileChooserOpened` (frame, mode, node) and the page waits; with
  `cancel: true` the page still gets its `cancel` event.
- Broxser showed nothing for either. A user saw a click do nothing.

## Options

| Option | Assessment |
| --- | --- |
| Keep the headless default | Nothing is saved today, but only by the browser's default; nothing tells the user what the page tried |
| Save downloads to a Broxser directory | A new file lifecycle (where, how long, which name, what the user opens it with); its own capability |
| Deny per context and report | Explicit, one command per session context, uses browser-level events; the document request is still made |
| Give pages a chosen file (`DOM.setFileInputFiles`) | Reads user files into a session; a file picker of Broxser's own; a separate decision |

## Decision

- Every session context sets `Browser.setDownloadBehavior` to `deny` with
  events, in live and capture runs. Broxser saves nothing, whatever the browser
  or platform default.
- The device whose main frame or subframe started a download reports it: a
  count and the latest one's suggested file name and URL, as untrusted page
  text shown on one line with control characters removed and bounded by
  `MAX_DIALOG_CHARS`. Subframes are tracked per device from `Page.frameAttached`
  until removal, across renderer swaps, and cleared with each new document, so
  a cross-site frame's download is the device's too. A frame Broxser does not
  know reports nothing.
- Device sessions auto-attach to iframe renderer targets, and configure
  each attached iframe to do the same for its own descendants. The browser-level
  target and page targets are not auto-attached, so this does not change popup
  handling. New iframe renderers wait during asynchronous setup: enable Page
  events, install chooser cancellation, enable recursive attachment, read the
  existing frame tree, then resume. Ownership follows the DOM parent tree,
  including same-process ancestors between renderer targets; removed subtrees
  and replaced documents retire their sessions and pending setup results.
  Child-session events update frame ownership and chooser counts only; they do
  not enter the device's main-frame input, IME, dialog or navigation-sync paths.
  ADR 0019 also explicitly includes worker target types: pinned Helium can pause
  a worker under `waitForDebuggerOnStart` even when the iframe-only filter hides
  its attachment event. Owned worker sessions are immediately resumed and
  detached after the resume acknowledgement, without Page setup or frame/input
  ownership. They are never treated as page or iframe event sources.
- The whole iframe setup uses one command deadline. A setup error or timeout
  marks that device's current document as having incomplete iframe activity
  reports; other devices keep running. A fresh main document clears that state;
  the report offers an explicit navigation or reload, with no automatic retry.
  Retired debugger-held sessions wait asynchronously for their resume response
  before detachment, so a temporarily busy renderer can recover without being
  left paused. Late replies cannot restore retired frame ownership.
- Frame ownership remains bounded to 256 subframes per device; reaching that
  limit reports incomplete activity for that device instead of stopping peers.
  Active and retiring iframe sessions and pending worker cleanup share a runtime
  limit of 128. Exhausting
  that global safety budget stops the runtime with an explicit error and browser
  cleanup, rather than accumulating untracked debugger-held renderers.
- Broxser's own navigation (Go, Reload, a synced link) to an address that is a
  download ends without a navigation error; the download report tells what
  happened, and the device stays on its page. A capture run reports the
  address as a download in its navigation error.
- Every device intercepts file choosers with `cancel`: the page gets the
  `cancel` event as before and no file, and the device counts the chooser. No
  file of the user's is read. The same interception is installed on attached
  iframe renderer sessions and their reports are attributed to the owning device.
- The card's activity line says "N download(s) refused" and "N file chooser(s)
  cancelled"; a refused download also shows "Refused a download the page
  started" with the file name and address and a Dismiss that only hides it.
  Dismissal belongs to the current runtime and is cleared when it is replaced,
  since the download count starts over. The panel is at most 360 UI pixels wide
  with Dismiss at the left edge, keeping it reachable on wide devices in the
  vertically scrolling canvas.

## Consequences

- Export, report and attachment flows do not produce files in Broxser; the card
  says which file the page offered and from where. Upload flows cannot choose a
  file; the page sees a cancelled chooser.
- A refused download's document request still reaches the server, as the
  page's own navigation would; Broxser does not repeat it.
- The chooser count is the only trace of a picker the page opened, since the
  headless browser opens none.

## Validation

Fake CDP: `downloads_and_file_choosers_are_refused_and_reported_for_their_device`
(the deny and intercept commands, reports for a main frame, a swapped subframe,
a removed and an unknown frame, the bounded one-line texts, chooser counts, and
Go to a download without an error). Helium:
`live_downloads_and_file_choosers_are_refused_and_reported` (an attachment link,
a `download` attribute link, a cross-site frame's link, a file input, Go to a
download, and no file in the profile or `~/Downloads`). Desktop: the smoke run
"download refused on the card" in a real X11 window. Results are in
`docs/validation.md` (P1.6).

The review fixes add fake-CDP coverage for recursive sessions, DOM ancestry,
renderer swaps, stale inventories and replies, retirement order, setup errors
and deadlines, and per-device frame limits. The Helium tests
`live_nested_cross_site_file_choosers_and_downloads_belong_to_their_device` and
`live_busy_iframe_setup_degrades_one_device_and_resumes_after_renderer_recovers`
exercise nested renderer targets, separate-device controls, busy-renderer
recovery, cancellation and no replay. The desktop smoke
`download_desktop_restart_run` checks Dismiss at 100% on a 1440 px device in a
1360 px window, including the first report after runtime restart.

The recursive attachment follows the CDP
[`Target.setAutoAttach` contract](https://github.com/ChromeDevTools/devtools-protocol/blob/master/pdl/domains/Target.pdl): attachment applies to directly related targets,
and newly paused targets are resumed with `Runtime.runIfWaitingForDebugger`.
The iframe filter and lifecycle behavior are qualified against the pinned Helium
runtime, rather than inferred from protocol availability alone.
