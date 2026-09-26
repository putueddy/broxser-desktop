# ADR 0014 JavaScript dialogs are answered by the user only

Status: accepted for P1.6 (first capability), 2026-09-26. Amends ADR 0005 and ADR 0008.

## Context

P1.6 covers browser interactions Broxser does not support yet: dialogs, popups,
permissions, downloads and uploads, touch, drag and drop, accessibility. Each is a
decision of its own; this ADR takes JavaScript dialogs, the one that blocks the
page. A CDP probe against Helium 0.18.1.1 (Chrome 154) and a run of `main` at
`6495c2b` with the live fixture found:

- `alert`, `confirm` and `prompt` stop the page. `Page.javascriptDialogOpening`
  names the kind, the message and the prompt's default. While the dialog is open
  the input that opened it stays unanswered, further pointer input and
  `Input.insertText` are held and delivered when the dialog closes, key events
  are answered and dropped, `Runtime.evaluate` blocks, and the screencast sends
  no frames. Broxser showed "The page opened a JavaScript dialog, which Broxser
  cannot show yet" and a frozen frame, and offered nothing else.
- Nothing dismisses a dialog for the user: none of the key event variants, mouse
  events, or eight seconds of waiting closed one. `Page.navigate` does: it cancels
  the dialog (`confirm` returns false, `prompt` null) and navigates. Go, Reload,
  workspace open and link sync therefore answered dialogs silently.
- The held pointer input counts toward the not-responding limit of ADR 0008, so
  a page waiting for its user was reported as not responding after 15 s.
- `beforeunload`: once the user has interacted with a page that asks before
  leaving, a link click or a Broxser navigation opens a `beforeunload` dialog.
  `Page.navigate` then waits for the answer. Accepting continues the navigation
  (the answer carries the loader); declining keeps the page and the navigate
  answers `net::ERR_ABORTED`, sometimes before `Page.javascriptDialogClosed`.
  `Page.stopLoading`, which Broxser sends at its 30 s navigation deadline, aborted
  the navigation and left the dialog open. On `main`, Go on such a page ended in
  "Navigation got no response within 30 seconds; loading stopped, not retried"
  with the page still frozen behind the question.

## Options

| Option | Assessment |
| --- | --- |
| Dismiss dialogs automatically (cancel) | Answers for the user; `confirm`/`prompt` results are wrong and unsaved-changes questions are decided by Broxser |
| Accept dialogs automatically | Same, with the opposite wrong answer, and leaves pages without asking |
| Let Go and Reload cancel the dialog, as Chromium does | An implicit answer hidden in an unrelated action |
| Show the dialog and wait for the user | The page's question reaches the person testing it; every outcome is theirs |

## Decision

- The engine reports each dialog in the device status: kind (`alert`, `confirm`,
  `prompt`, `beforeunload`), message and prompt default as untrusted text (control
  characters dropped, at most 2048 characters, a cut marked), and a token. The
  desktop shows it on the device card with the page's message and only the answers
  that dialog has: OK; Cancel and OK; Cancel and OK with a text field prefilled with
  the default (Enter accepts); Stay and Leave.
- `Command::AnswerDialog` carries the token. It answers only the dialog that is
  still open under that token; a late click for a dialog that already closed
  answers nothing. Prompt text is bounded like the message. The engine sends
  `Page.handleJavaScriptDialog` and treats `Page.javascriptDialogClosed` as the
  confirmation; the dialog stays shown until the browser reports it closed.
- While a dialog is open, page input for that device (pointer, wheel, keys, text,
  IME) is dropped, never held, as ADR 0008 does for a page that does not answer.
  Held IME input and an open composition are dropped when the dialog opens.
- Broxser does not navigate a device with an open dialog. Go, Reload, a synced
  link and workspace open leave that device where it is and report "The page is
  waiting for an answer to its dialog; navigation was not sent"; other devices
  navigate. The report clears when the dialog closes.
- The not-responding and navigation deadlines pause while a dialog is open and
  resume when it closes. A `beforeunload` question that the user declines ends
  the navigation Broxser started without an error and without a retry; accepting
  continues it. A new document, a crash or a detached target clears a dialog.

## Consequences

- A dialog blocks its page until the user answers, as in a browser, and its frame
  stays at the last image the page drew; frames resume after the answer.
- Chromium delivers pointer input held during the dialog when it closes. Broxser
  drops such input instead of sending it, so nothing arrives late; the click that
  opened the dialog is the page's own.
- Pages that ask before leaving now ask in Broxser too; a Go on eight dirty
  devices asks eight questions, one per card.
- Popups, downloads, uploads, permissions, touch and accessibility keep their
  current behavior and are decided separately.

## Validation

Fake CDP: `dialog_blocks_input_and_navigation_until_the_user_answers` (state,
bounding, dropped input, refused Go, stale token, answer parameters, close,
prompt text). Helium: `live_dialogs_wait_for_an_explicit_answer` (alert, confirm,
prompt through the real page: frozen frames, typing and Go refused, stale token,
each answer's outcome, input afterwards) and
`live_beforeunload_dialog_needs_an_explicit_leave_or_stay` (Go and a link on a
dirty page; Stay without an error past the load limit; Leave). The desktop panel
was checked in a real X11 window; see `docs/validation.md` (P1.6).
