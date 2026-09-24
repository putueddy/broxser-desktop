# Security model

This foundation is not yet approved for company-wide browsing. The release gates
in docs/system-design.md include runtime updates, crash cleanup, recovery,
permissions, and a pilot with representative company applications.

The browser subprocess keeps Chromium's sandbox enabled, uses a private temporary
profile, and exposes CDP on a random loopback port. CDP can control every page in
that subprocess. Loopback prevents remote network access, but does not authenticate
other processes running locally. Never forward or expose the port, and never reuse
a personal browser profile. A pipe transport is a planned hardening option.

Broxser seeds only its own new profile: Helium's bundled content blocker is kept
out of session contexts (ADR 0004), and extension pages inside those contexts stop
a capture. Crash dumps are redirected into the private profile; without that,
Chromium writes them next to a personal Helium installation. Chromium still opens
the user's shared NSS certificate database, which may hold corporate CAs and client
certificates; whether to isolate it is an open decision.

Cleanup runs on normal close, errors and cancellation. If the Broxser process is
killed or crashes, the browser keeps running with its loopback CDP port and the
temporary profile remains until removed; this gate is open.

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
