# ADR 0023 Device console: page errors and messages per device, bounded and kept in memory

Status: accepted for P2.3 (first part), 2026-09-27. Builds on ADR 0005 (live
frames over CDP), ADR 0014 (page text is shown, never acted on) and the iframe
sessions of ADR 0016. Bug-report export, redaction of exported text and its
retention are the second part of P2.3 and separate.

## Context

`GOALS.md` (P2.3) asks for console and error aggregation per device and
session. Before this change a page's errors reached nobody: every device
session already ran `Runtime.enable` for Broxser's link and IME observers, so
the browser sent each `console` call and uncaught exception to the live
worker, which dropped them; the browser's own stdout and stderr go to
`/dev/null`. Failed requests were not reported at all.

A CDP probe on Helium 0.18.1.1 with a Broxser-like device (context, page
target, recursive iframe auto-attach) measured what the browser reports:

- `Runtime.consoleAPICalled` for every `console` call with its type (`log`,
  `warning`, `error`, `assert`, `table`, `count`, `startGroup`, `endGroup` and
  more), its arguments as remote objects (previews for objects and arrays)
  and the calling script's address and line. A 100 000-character string
  arrives whole, as a 100 KB event.
- `Runtime.exceptionThrown` for uncaught errors ("Uncaught") and unhandled
  rejections ("Uncaught (in promise)").
- Same-origin frames report on the page session with their own execution
  context. A cross-site iframe runs in another process: its messages arrive
  only on its own auto-attached session, and only once `Runtime` is enabled
  there. Dedicated workers are not attached, so their messages never arrive.
- Failed requests (404 image and script, a 500 `fetch`, a refused port, the
  favicon) arrive only through `Log.entryAdded` with source `network`, which
  needs `Log.enable`; enabling it late replays the entries so far, and
  disabling and enabling `Runtime` replays every console message.
- A page logging 100 000 messages in a loop sent 41.7 MB of events. In a live
  runtime of three devices the other devices' frames dropped from 60 per
  second to 1–5 for about four seconds while the worker read them, and the
  runtime recovered without error. That cost exists whether or not Broxser
  keeps the messages.

## Options

| Option | Assessment |
| --- | --- |
| Keep ignoring the events | The browser still sends them; developers see nothing |
| Write page messages to Broxser's log or stderr | Page text would outlive the session in journals; never |
| Keep every message | Unbounded memory for a page that logs in a loop |
| A bounded console per device, in memory | Enough to see what failed where; nothing written anywhere |
| Enable Runtime and Log on out-of-process iframes too | One more step in the existing iframe setup, before the frame runs |
| Attach workers and service workers | New target types with their own lifecycle; later, if needed |
| Honour `console.clear()` | A page could hide its own errors; Broxser's Clear is the user's |

## Decision

- Each device has a **console** in the live runtime: the newest 200 entries,
  each with a level (error, warning, info), a kind (console call, uncaught
  error, failed request, other browser message, navigation), one line of text
  of at most 1000 characters including any truncation mark, a location of at
  most 300 characters including line/column, a frame scope and a repeat count.
  Consecutive identical entries count as one. Error and warning counts keep
  counting when old entries leave. Everything is page text: bounded, put on
  one line without control or bidirectional formatting characters (Unicode
  line/paragraph separators become spaces; script joiners are preserved),
  shown, and never logged, written to disk, sent anywhere or acted on.
- Sources: `Runtime.consoleAPICalled` (formatted like DevTools on one line:
  `%s`, `%d`, `%i`, `%f`, `%o`, `%O`, `%c` substitutions and object and array
  previews), `Runtime.exceptionThrown` and `Log.entryAdded` (verbose entries
  left out) of the page session and of the page's out-of-process iframe
  sessions, whose setup now enables `Runtime` and `Log` before the frame may
  run. Known Runtime contexts distinguish the main frame from same-process
  subframes; iframe sessions identify subframes directly. Page-session Log
  events have no reliable frame identity, so their scope is explicitly unknown
  (the panel shows "frame unknown"). Broxser's own isolated worlds never appear.
  A new main frame document adds a navigation entry. `console.clear()`, `groupEnd` and
  profiling calls add nothing.
- **Locations** keep scheme, host, port and path of hierarchical addresses
  and drop user information, query and fragment, which can carry codes and
  tokens; `data:`, `blob:` and `about:` addresses keep only their scheme.
- **Retention**: the console lives in memory for the runtime. Clear empties
  one device locally, including after the browser has stopped, without a CDP
  command or page action. Log mutation and count/revision publication are
  serialized against incoming messages. Restart starts every console empty;
  closing Broxser drops them.
  Remote objects delivered by Runtime are formatted immediately; Broxser never
  dereferences their object IDs. Their `console` object group is released in
  bounded batches per active debugger session so evicted messages cannot leave
  inspector references keeping arbitrary page objects alive. This housekeeping
  does not clear the browser's own console history or counters.
- The desktop shows a filled count on a device card when the device has
  errors or warnings ("2 errors · 1 warning · Console"). Clicking it, the
  toolbar toggle "Console" or `Ctrl+Shift+J` (Helium's DevTools key, which
  the engine never forwards to a page; ADR 0010) opens the **Console panel**
  for the selected device,
  newest first, with Clear. It shares the panel slot with the Workspace
  panel. Status snapshots carry only counts and a revision; the panel reads
  the entries when the revision changes or Hide selects another device, even
  when hiding an already-paused stream produces no status change.

## Consequences

- Cross-device QA sees at once which device's page threw, logged an error or
  failed a request, including cross-site iframes, without a second browser.
- Worker and service worker messages, network details beyond the browser's
  message, and anything before the runtime started are not shown. Message
  text is the page's own and may contain what the page logged, tokens
  included; locations never carry query strings.
- A page that floods the console still slows the runtime while its events
  arrive (existing cost, measured above); keeping entries adds no events.
- Exporting a report (screenshot, device and browser details, console) with
  redaction and a retention rule is the next part of P2.3.

## Validation

Unit: `console_calls_become_bounded_one_line_entries`,
`exceptions_and_log_entries_name_what_failed_and_where`,
`locations_keep_only_scheme_host_and_path` and
`repeats_collapse_and_old_entries_leave_while_counts_stay` in the engine,
`console_counts_name_errors_and_warnings` in the desktop. Fake CDP:
`device_consoles_are_bounded_attributed_and_cleared`. Helium:
`live_console_keeps_each_devices_errors`. Real window: the desktop smoke run
"console panel counts and clears one device". Details in
`docs/validation.md` (P2.3).
