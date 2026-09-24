# ADR 0002 Portable workspace and session scoped sync

Status: accepted, 2026-09-24.

Store shareable project configuration in versioned JSON. Cookies, tokens, captures
and browser profiles are separate private runtime data. Reject unknown schema
versions. Introduce migrations only when a second real schema exists; back up before
conversion and do not destructively rewrite a file that a newer client owns.

Session IDs define browser storage boundaries. Multiple devices may intentionally
share one BrowserContext; different IDs must be isolated. The first capture runtime
uses ephemeral sessions and never claims persistent login.

Synchronization is opt-in and scoped to the same session by default. Sequence and
origin tracking reject duplicates and replay loops. The pure router is a foundation
contract, not a shipping browser sync feature. Reconnect never automatically replays
typing, clicks, authentication or forms. Future live integration adds bounded queues
and navigation generations rather than making page-side code authoritative.
