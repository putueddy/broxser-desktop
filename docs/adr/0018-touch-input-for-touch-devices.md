# ADR 0018 Touch devices get touch input

Status: accepted for P1.6 (fifth capability), 2026-09-27. Amends ADR 0005.

## Context

A workspace device declares `touch: true` (the phone and tablet of the example
workspace) and its target is set up with touch emulation
(`Emulation.setTouchEmulationEnabled`, one touch point), so pages see
`(pointer: coarse)` and `(hover: none)` and a touch-capable `navigator`. Yet
every pointer event of the canvas reached the page through
`Input.dispatchMouseEvent`. The P1.6 audit (Helium 0.18.1.1) and a live test
at `77a1762` found:

- On a touch device, a click on the card arrived as `pointerdown` with
  `pointerType: mouse`, `mousedown`, `mouseup` and `click`; no `touchstart` or
  `touchend` fired. A page that handles touch (swipe carousels, pull to
  refresh, `touch-action`, `touchstart` listeners) never ran that code, and
  code that branches on `pointerType` took the mouse branch.
- `Input.dispatchTouchEvent` with one touch point gives `pointerdown` with
  `pointerType: touch`, `touchstart`, `touchend`, then the compatibility
  `mousedown`, `mouseup` and `click`; a drag with `touchMove` scrolls the page
  as a swipe. `Input.emulateTouchFromMouseEvent` taps produced nothing.
- A touch device has no hover and no second button; a finger is one point.

## Options

| Option | Assessment |
| --- | --- |
| Keep mouse events everywhere | Touch pages behave as on a desktop with a small screen; QA of touch behavior is impossible |
| Touch events on touch devices, one finger | Pages see what a phone sends; multi-touch gestures stay out of scope |
| A toggle per device | Extra UI for a property the workspace already states |

## Decision

- On a device with `touch: true`, a left press is a `touchStart` with one
  point, a move with the left button held is a `touchMove`, and a release is
  a `touchEnd`, all through `Input.dispatchTouchEvent`, with the keyboard
  modifiers. Moves without the finger down and the right and middle buttons
  send nothing to a touch device. Mouse devices are unchanged.
- Moves are coalesced with one move in flight and at most the newest and
  furthest unsent point retained, so a fast out-and-back drag cannot become a tap.
  A touch release preserves its final position with a `touchMove` before
  `touchEnd` when needed: unlike a mouse release, `touchEnd` has no position.
  Dropping this move could turn a fast swipe into a click at its starting point.
  Release commands count toward the unanswered-input limit; if the final
  movement and end cannot fit, the gesture is canceled instead of becoming a tap.
  Touch input waiting for an IME identity check retains its path in the existing
  bounded input queue; an explicit cancellation removes held pointer commands.
- Each device owns at most one active touch. Moves and releases without an
  accepted press are ignored. Hide, navigation, dialogs and unresponsive
  input abandon that ownership and discard coalesced input. A browser touch
  that remains after a dropped release is canceled before a fresh press;
  its old release is never replayed. Cancellation waits for input to be allowed
  rather than leaving a command behind an open dialog or unresponsive page.
- Leaving the canvas on release, switching devices or losing canvas/window
  focus cancels the touch instead of ending it at the last visible point.
  An abandoned drag must not activate the element where it started.
- Unsupported buttons are filtered before they can cancel IME composition.
  Mouse devices keep their existing input behavior.
- Wheel input stays a mouse wheel on every device: the browser scrolls a
  touch page for it, and a mouse wheel is how the user scrolls the card.
  Opt-in scroll sync still mirrors wheel deltas; native swipe scrolling stays
  in its own device.

## Consequences

- Touch pages run their touch handlers on touch devices, `pointerType` is
  `touch`, and a drag is a swipe that scrolls; taps still produce `click`,
  so link sync (ADR 0006) and dialogs (ADR 0014) work as before.
- Nothing hovers on a touch device: menus that open on hover need a tap, as
  on a phone. There is no right click or long press.
- Pinch, multi-finger gestures and touch pressure are not modeled.

## Validation

Fake CDP: `touch_devices_send_touches_and_nothing_for_hover_or_other_buttons`
(the three touch commands and their points; nothing for hover, right or
middle; a mouse device keeps mouse events). Helium:
`live_touch_devices_get_touches_and_mouse_devices_get_a_mouse` (a tap's event
sequence on a touch phone and a click's on a mouse desktop; a drag on the
phone sends `touchmove` and scrolls the page; it failed before the change with
mouse events on the phone). Desktop: the existing smoke runs click the touch
phone of the example workspace, so dialogs, popups and downloads are reached
through taps. Results are in `docs/validation.md` (P1.6).
