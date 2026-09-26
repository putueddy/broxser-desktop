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
  `prompt`, `beforeunload`), the page's message as untrusted text (control
  characters other than line breaks and tabs dropped, at most 2048 characters,
  a cut marked with `…`), and a token. A prompt's default is bounded as an
  answer Broxser can send back unchanged instead: one line (line breaks and
  tabs become a space), the same character limit, cut without a mark. The
  desktop shows the dialog on the device card with the page's message and only
  the answers that dialog has: OK; Cancel and OK; Cancel and OK with a text
  field prefilled with the default (Enter accepts the field's text unchanged,
  Escape cancels like the Cancel button); Stay and Leave.
- `Command::AnswerDialog` carries the token. It answers only the dialog that is
  still open under that token and has no answer of its own yet: a late click
  for a dialog that already closed, or a second answer once one was already
  sent for it, answers nothing — `Page.handleJavaScriptDialog` answers whichever
  dialog the page is currently showing, so a second send could reach the next
  one. A prompt answer over 2048 characters or holding a control character is
  not sent either; the dialog stays open, waiting for another answer, and the
  device reports why. The engine sends `Page.handleJavaScriptDialog` and treats
  `Page.javascriptDialogClosed` as the confirmation; the dialog stays shown
  until the browser reports it closed. If the browser's reply to
  `Page.handleJavaScriptDialog` is itself an error while that dialog is still
  open, the answer is taken back so the dialog can be answered again, the same
  as an answer that was never sent; the browser's message is shown as a
  protocol error, not folded into the dialog's own report.
- A dialog type Broxser does not recognize, or a missing one, is not shown or
  tracked: the device reports that Broxser cannot show it, without blocking
  that device's input or navigation. The report clears once the browser
  reports the dialog closed (Reload, which cancels any open dialog, is one way
  to reach that).
- While a dialog is open, page input for that device (pointer, wheel, keys, text,
  IME) is dropped, never held, as ADR 0008 does once a page is found not
  responding. A dialog is not itself treated as a page that stopped responding,
  though: ADR 0008's not-responding check pauses instead of tripping while one
  is open. Opening the dialog invalidates the device's IME state (composition
  and caret) locally, without sending a composition cancel to the page, so
  nothing of Broxser's is left waiting behind the dialog.
- Broxser does not navigate a device with an open dialog. Go, Reload, a synced
  link and workspace open leave that device where it is and report "The page is
  waiting for an answer to its dialog; navigation was not sent"; other devices
  navigate. The report clears when the dialog closes. A refused Go or workspace
  open still retires that device's pending link, exactly as a sent one would
  (ADR 0013); a refused synced link is different and leaves the device's
  link-sync tracking untouched, so a link the page commits later still syncs.
- The not-responding and navigation deadlines pause while a dialog is open and
  resume when it closes. A `beforeunload` dialog belongs to the Broxser
  navigation it interrupts — a tracked Go or Reload with no page-initiated
  main-frame navigation request in the same tab since it was sent (a link or
  script that opens a new tab or window does not count) — never to one the
  page started itself. Declining it ends that Broxser navigation without an error
  and without a retry; accepting continues it. One raised by the page's own
  link while a Broxser navigation is still pending leaves that navigation
  tracked, running under its own deadline. A new document, a crash or a
  detached target clears a dialog and its token, and so does stopping the
  runtime.

## Consequences

- A dialog blocks its page until the user answers, as in a browser, and its frame
  stays at the last image the page drew; frames resume after the answer.
- Chromium delivers pointer input held during the dialog when it closes. Broxser
  drops such input instead of sending it, so nothing arrives late; the click that
  opened the dialog is the page's own.
- Pages that ask before leaving now ask in Broxser too; a Go on eight dirty
  devices asks eight questions, one per card.
- A dialog on a hidden device stays open until the device is shown; hiding a
  device never answers its dialog. The `beforeunload` panel always shows
  Broxser's own copy ("Leave this page?" / "The page may have changes you have
  not saved."), not page-supplied text, unlike alert, confirm and prompt.
- The panel is shown only while the runtime is running, at most 360 px wide and
  anchored at the frame's left edge with its buttons on the left, so it stays
  reachable in a narrow window; its message box scrolls past 120 px. A new
  prompt takes keyboard focus only from the canvas or that device's previous
  field, never from the URL bar or another device's field, and returns focus to
  the canvas once answered or the field otherwise goes away; a held or
  auto-repeating Enter or Escape cannot answer a prompt it did not open, and
  does not spoil the field's select-all-to-replace state either, even though
  the field itself clears it on every Enter or Escape. Focus moving into a
  prompt field, by auto-focus or a later click, also invalidates a stale
  canvas IME mark; nothing reaches the page, since the engine already cleared
  that device's IME state when the dialog opened.
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

Review fixes added fake CDP tests `dialog_text_and_prompt_defaults_are_bounded`,
`showing_a_device_during_its_dialog_waits_for_nothing_the_page_answers`,
`prompt_default_can_be_sent_back_and_rejected_answers_are_reported`,
`a_dialog_takes_one_answer_and_never_the_previous_dialogs`,
`a_dialog_answer_the_browser_refuses_can_be_sent_again`,
`staying_ends_the_broxser_navigation_that_asked_in_either_reply_order`,
`staying_ends_a_reload_that_asked`,
`leaving_continues_the_navigation_under_a_fresh_deadline`,
`staying_on_the_pages_own_link_keeps_the_broxser_navigation`,
`staying_ends_the_broxser_navigation_after_a_page_request_outside_its_tab`,
`a_dialog_drops_the_composition_without_a_cancel_or_a_not_responding_report`,
`a_dialog_broxser_cannot_show_is_reported_and_blocks_nothing`,
`a_refused_go_retires_the_devices_link_and_a_refused_synced_link_keeps_it`,
`a_stopped_runtime_leaves_no_dialog_shown`,
`a_dialog_refuses_reload_and_synced_links_and_drops_pointer_input` and
`a_new_document_a_crash_or_a_detach_ends_the_dialog_and_its_token`; and Helium
test `live_dialog_survives_hide_show_scroll_and_zoom`, alongside a revised
`live_dialogs_wait_for_an_explicit_answer` that now waits for settled frames and
for the previous dialog to close. The engine fixes, including the dialog-answer
retry and the Go/synced-link link-retiring split above, are verified by the
fake CDP suite and the live Helium suite; see `docs/validation.md` (P1.6,
"Review fixes") for the runs. The desktop changes (this ADR's Consequences,
including the select-all restore and the IME invalidation on prompt focus) are
covered by unit tests only so far; the real X11 window check has not yet been
repeated for the revised panel, and has never covered composing with an IME
then a prompt taking focus.
