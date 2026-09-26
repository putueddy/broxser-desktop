# Security model

This foundation is not yet approved for company-wide browsing. The release gates
in docs/system-design.md include runtime updates, crash cleanup, recovery,
permissions, and a pilot with representative company applications.

The browser subprocess keeps Chromium's sandbox enabled, uses a private temporary
profile (mode 0700), and exposes CDP on a random loopback port. CDP can control every
page in that subprocess. Loopback prevents remote network access, but does not
authenticate other processes running locally. Never forward or expose the port, and
never reuse a personal browser profile. A pipe transport is a planned hardening option.

Broxser seeds only its own new profile: Helium's bundled content blocker is kept
out of session contexts (ADR 0004), and extension pages inside those contexts stop
a capture. Crash dumps are redirected into the private profile; without that,
Chromium writes them next to a personal Helium installation. Before starting a
browser, Broxser sets its own soft core-file limit and `coredump_filter` to zero,
and the browser inherits both. File-based core patterns and apport (for unpackaged
programs) then write nothing. systemd-coredump ignores the limit and still records
the crash, but the dump holds only headers and register state, no memory mappings
with cookies or page content. Chromium still opens the user's shared NSS
certificate database, which may hold corporate CAs and client certificates;
whether to isolate it is an open decision.

Cleanup runs on normal close, errors and cancellation. Every cleanup path deletes
the profile only once no running process names it, so a Helium helper that is
still stopping cannot write profile files back afterwards. If the Broxser process is
killed (SIGKILL, SIGTERM, Ctrl+C) or crashes, a guardian process started before each
browser notices that its pipe from Broxser closed, stops that browser (a pidfd plus
the recorded start time, so a reused PID is never signaled), waits for its helpers
and deletes the profile if its lease still names the same owner (ADR 0007).
Measured in tests: browser, CDP endpoint and profile gone 38–105 ms after the
owner died. The guardian runs in its own session and never signals processes it did
not record or that do not carry that profile's `--user-data-dir` argument.

If the guardian dies together with Broxser, for example when a whole cgroup is
killed or power is lost, the profile stays until the next browser start in the same
profile root. That start only removes profiles it can prove stale: a real directory
owned by the user with a valid lease whose boot ID changed, or whose owner and
guardian are gone in the same PID namespace. It stops a recorded browser only if that
process still runs with the profile's argument. Symlinks, other users' directories,
unleased or unknown-version entries and profiles still named by any process are
left alone. Neither path replays user actions. Live mode keeps one browser per open
workspace, holding the sessions' cookies and storage until the window closes.
Not covered yet: static-mode preview directories (screenshots) after the desktop is
killed, and Chromium's `org.chromium.Chromium.*` socket directory in the temporary
directory, which also remains after a normal close.

Live input is forwarded only to the device the user targets. Sync never broadcasts
typing, form submission, clicks or pointer events, never crosses sessions, and a
restart restores configuration without replaying user actions. One restart or close
runs at a time: a restart starts one browser, and none starts once the window is
closing (ADR 0009). Once a page stops
answering, new input for it is dropped, not queued, so those clicks and keys cannot
reach it seconds later; input already sent (at most 32 events) still arrives if
the page recovers. A navigation Broxser started that gets no response in 30 seconds
is stopped and never retried (ADR 0008).

Sessions share the browser process, and Chromium keeps one clipboard and one
selection buffer for all of them: before ADR 0010, a copy or a mere selection in one
session could be pasted into another with Ctrl+V or a middle click. The engine now
never forwards paste keys or middle-button presses to pages. Paste is explicit:
Ctrl+V, Shift+Insert or Ctrl+Shift+V inserts the system clipboard's plain text, at
most 65,536 characters without control characters other than tab and line breaks,
into the selected visible device only, once per key press. Pages cannot read the
browser clipboard through `navigator.clipboard.readText()` (denied in Helium
0.18.1.1). Keys that Helium turns into browser commands are not forwarded either:
before ADR 0010, Ctrl+W in one device closed another device of the same session,
Ctrl+Shift+M stopped the browser, and other keys opened tabs and DevTools that
Broxser never showed, or reloaded and navigated pages outside its deadlines. That
list is measured per Helium release.

Link sync requires a trusted link report from the device's isolated main-frame
execution context and the matching browser navigation request/loader. The binding
does not expose native capabilities to page scripts. A recent ordinary keypress
cannot authorize scripted links. Full URLs are validated before routing; oversized
destinations are rejected rather than shortened. Hidden devices receive no page
input or synchronized navigation/scroll, even through the engine API (ADR 0006).

Extension observations are retained across context-registration ordering and
target destruction, then checked before targets in a new session are opened.
Websocket handshake progress is cancellable and retains its overall deadline;
partial upgrades and navigation URLs are not replayed.

HTTP and HTTPS are accepted deliberately, including localhost and internal sites.
This is a developer desktop app, not a public URL-fetching service. Do not expose
the CLI as an unauthenticated server: that would create an SSRF boundary the current
design does not address. Browser permissions, redirects and content remain governed
by the browser; input URL validation is not a network allowlist.

Screenshots may contain private data. CLI exports remain in the specified output
directory until the developer deletes them. Desktop previews use an owned temporary
directory. No analytics or application upload endpoint is configured. Website
requests and upstream browser behavior are separate from Broxser application
telemetry. Browser privacy/filtering defaults can change the page being tested.

Report problems through the company's established private security channel. An
actual team/contact must be assigned before rollout; do not attach cookies, tokens,
customer screenshots, or full sensitive URLs to public issues.

Proposed operating targets: triage browser advisories within one business day;
qualify critical updates within 72 hours of a usable upstream release; review other
updates weekly. These are targets needing assigned maintainers, not current service
guarantees. An incompatible upstream release blocks promotion until contract tests
pass. Security response takes precedence over UI feature work.
