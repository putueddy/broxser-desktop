# ADR 0006 Trusted link intent and hidden input boundaries

Status: accepted for the M1 spike, 2026-09-25. Amends ADR 0005.

## Problem

A recent key or pointer event does not prove that the user followed a link.
CDP labels a scripted `anchor.click()` as `anchorClick` too. The first live
implementation therefore mirrored scripted navigation after typing, lost slow
link navigations when its two-second timer expired, and could navigate peers to
a truncated URL. Hiding a selected device also left keyboard routing active.

## Decision

Observe trusted link activation in a named isolated JavaScript world. The
observer has a narrow CDP binding for reporting the link's original destination;
it grants no filesystem, shell or other native capability. Accept a report only
from the registered isolated execution context of that device's main frame.
Page scripts, subframes and other execution contexts cannot authorize peer
navigation by calling a similarly named function or emitting synthetic events.

Match the reported destination with the browser's same-tab link navigation
request, then associate that request with its navigation loader. A candidate alone
cannot authorize sync: the isolated world's trusted `beforeunload` callback must
confirm that the same click was not canceled. Candidate and confirmation carry a
per-activation ID, so a delayed confirmation cannot approve another same-URL click.
Editable content and interactive descendants are excluded; Enter on a real link
uses the browser's trusted click activation. The committing loader and URL must
match. Eligibility is decided at the start; a slow HTTP
response does not turn an eligible navigation into a different user action.
Unrelated navigations, document changes and hidden devices clear eligibility.
Every intent is consumed once; synced navigations cannot authorize another sync.

Keep the complete observed URL for status and validate the original string before
routing. A destination beyond the supported URL limit is not synchronized.
Visual clipping is presentation only and must never construct a new URL for a
peer, Go or Restart. Redirects to a different destination are conservatively not
mirrored by this implementation; that behavior needs its own policy and tests
before support is widened. Modified clicks, downloads, new-tab targets and
unsupported subframe navigation are outside this sync contract.

Hide pauses both streaming and page input. The desktop selects a visible
alternative, or has no page-input target when every device is hidden. Selecting
a hidden sidebar row does not silently make it a keyboard target. Held key
releases belong to the device that received their presses, and pending pointer,
wheel and link state must not replay when a device becomes visible again.
The engine also enforces visibility, so this guarantee does not depend solely
on the GUI's selection state. Sync stays within a session and visible devices.

Individual reloads also ignore hidden devices. The explicit URL-bar Go command
still loads every configured device once, including hidden ones: it is a workspace
navigation requested by the user, rather than forwarded or synchronized page input.
Released keys stay suppressed until their matching key-up, including X11 repeats
whose GPUI `is_held` flag is false. GPUI does not expose physical keycodes; unusual
keyboard layouts and lost OS key-up events remain platform qualification limits.
ADR 0010 replaces the name-only match: every press in the window is tracked, a
release under another name ends the latest press whose name can change, and only
the latest key-down counts as repeating.

## Related transport guarantees

Extension discovery may arrive before `Target.createBrowserContext` returns its
ID. Retain bounded observations of extension/context identifiers and recheck them
immediately when a context is registered. Destroying the target later cannot erase
the fact that an extension already ran. This closes the ordering gap in ADR 0004.

Resume partial websocket HTTP upgrades rather than restarting them. Poll
cancellation while retaining a single overall handshake deadline; a stalled peer
must return the typed cancellation error when the user cancels. Navigation URLs
are never retried as part of transport recovery.

## Validation

Regression coverage must include scripted links after typing and unrelated
clicks, invalid execution contexts, real pointer and keyboard link activation,
slow responses, oversized URLs, hidden input, early/destroyed extension targets,
fragmented handshake responses, cancellation and deadline expiry. See
`docs/validation.md` for results on the final source revision.

Protocol references: [Runtime bindings](https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-addBinding),
[isolated document scripts](https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-addScriptToEvaluateOnNewDocument),
[navigation events](https://chromedevtools.github.io/devtools-protocol/tot/Page/#event-frameStartedNavigating).
