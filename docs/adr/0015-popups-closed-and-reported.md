# ADR 0015 Windows that pages open are closed and reported

Status: accepted for P1.6 (second capability), 2026-09-26. Amends ADR 0005.

## Context

Broxser shows device pages only; it has no view for a window a page opens.
A CDP probe against Helium 0.18.1.1 (Chrome 154) and a run of the live runtime
at `06e0f5d` found:

- `window.open` with a user gesture, a `target=_blank` link and a named window
  with features each report `Page.windowOpen` (URL, name, features, gesture) on
  the opener's session, then `Target.targetCreated` for a page with `openerId`
  set to the device, in the device's browser context. The new target's URL is
  still empty then; it loads the address a few milliseconds later. Without a
  gesture Chromium blocks the window: `window.open` returns `null` and no target
  appears.
- Broxser counted these windows ("N popup(s) not shown") and left them running.
  They loaded, ran their scripts with the session's cookies and kept running
  after their opener navigated away. A test window that set
  `window.opener.location` 300 ms after it started moved the phone to
  `/hijacked`, invisibly; three windows kept reporting from the background.
- Closing the target as soon as `Target.targetCreated` arrives
  (`Target.closeTarget`) ended every window within about 20 ms. Its first
  document request was still made (10 of 10) and its script started in 1 of 10
  probes; the opener then saw `closed === true`.
- Browser-level `Target.setAutoAttach` with `waitForDebuggerOnStart`, enabled
  after setup, holds new windows before they run: closing and then resuming a
  held window let no script run (0 of 20) and no request for windows with an
  opener (0 of 15), though `noopener` windows still made their request. Detaching
  a held window instead of resuming it left the opener's page hung, and the
  approach adds a session for every existing and new page target.
  Page-level auto-attach does not attach popups.

## Options

| Option | Assessment |
| --- | --- |
| Keep windows running unseen | They act with the session's cookies where nobody sees them, can move their opener, and outlive it |
| Show windows as temporary devices | A new view with input, frames and lifecycle; its own capability |
| Close at `targetCreated` | Small and uses events Broxser already reads; the first request is made and, rarely, the first script starts |
| Hold with browser-level auto-attach, then close | Nothing runs, but it needs session bookkeeping for every page target and exact ordering to avoid hanging the opener |

## Decision

- Every page target with an opener in a Broxser session context is closed as
  soon as the browser reports it. Broxser's own device targets have no opener;
  windows in other contexts are not Broxser's and are left alone.
- The opener device reports it: a count of closed windows and the latest one's
  address from `Page.windowOpen`, as untrusted text bounded for display, with a
  token. A window whose opener is not a device target, such as a frame of
  another process, is closed without a report.
- The card shows "Closed a window the page opened", the address, Dismiss and,
  for an HTTP(S) address within the navigation limit of ADR 0006, "Open here".
  `Command::OpenPopup` with the token loads the recorded address in that device
  once, replacing its page, as an explicit navigation (no sync, no opener). A
  newer report replaces an older one; an older token opens nothing. Dismiss only
  hides the report in the window.
- Browser-level auto-attach is recorded as the stronger option should a window's
  first request or script matter; it is not adopted now.

## Consequences

- Flows that need their window, such as sign-in or payment popups, do not
  complete in Broxser; the card says a window was closed and where it led.
- A page learns its window closed at once (`closed` becomes true). Its first
  request reaches the server, as the page's own navigation does; Broxser does
  not repeat it unless the user chooses "Open here".
- Windows no longer accumulate renderers or outlive their pages.

## Validation

Fake CDP: `popups_are_closed_at_once_and_reported_for_their_device` (close
request, report, tokens, non-HTTP and oversized addresses, Broxser's own and
foreign targets, a frame opener, per-device reports, Open here once). Helium:
`live_popups_are_closed_before_they_act_and_open_only_on_request` (three ways to
open a window; no hijack, no background reports; Open here loads the page in the
phone alone). Desktop: the smoke run "popup closed and opened on the card" in a
real X11 window. Results are in `docs/validation.md` (P1.6).
