# ADR 0011 Native IME on the device canvas

Status: implemented; native Fcitx5/Pinyin over XIM qualified, 2026-09-26.
Extends ADR 0010; review pending.

## Context

The canvas forwards physical keys but registers no GPUI text input handler.
An input method can consume all those keys and deliver only preedit and committed
text, so forwarding more keys cannot make it work. A candidate popup also needs
the editable caret in window coordinates, which a streamed frame does not expose.

GPUI 0.2.2 additionally converts a one-byte Wayland IME commit into an ordinary
key-down with no corresponding release. The event has no origin field. Inferring
an IME commit from a missing release would conflict with the held-key suppression
contract from ADR 0006 and cannot distinguish a physical press reliably.

## Decision

- Register a native input handler during paint on the selected, visible device's
  canvas only. The URL field and normal physical-key path keep their existing
  behavior. A native commit bypasses pressed-key, repeat, and release tracking.
- Keep only the transient preedit and its UTF-16 selection in the desktop. Do not
  copy surrounding document or password text to the host or the operating system.
  Replacement requests outside that transient composition are unsupported.
- Forward preedit using CDP `Input.imeSetComposition` and commit using
  `Input.insertText`. Updating preedit replaces the previous composition; an empty
  preedit cancels it. A completed commit clears the local mark, so a following
  native cleanup callback cannot erase the committed text. Native finalization
  applies only to the original still-valid composition target.
- Bind a composition to the device, the engine's editable-target token, and the
  desktop runtime generation. Hiding, deselecting, navigating, losing focus,
  restarting, crashing, or closing invalidates it. Old commits must not migrate
  to another device or runtime. Input is never retried after cancellation,
  overload, an unresponsive page, or reconnect.
- Observe editable focus and caret geometry inside a dedicated isolated main-frame
  world. Accept reports only from that device's registered world and validate
  their size, identity, finite coordinates, and viewport bounds. The bridge
  reports geometry and an identity, not page text. Invalidate identities across
  focus, pointer, navigation, and execution-context boundaries.
- Measure input and textarea carets with a styled text mirror inside the renderer;
  use the DOM selection for contenteditable. Convert the resulting CSS rectangle
  through the actual painted frame bounds, including canvas zoom. GPUI then owns
  the final platform/window scaling and candidate popup positioning.
- Apply the same per-device visibility, responsiveness, input budgets, and
  deadlines as ordinary input. Validate composition text and UTF-16 ranges before
  a CDP side effect. A read of the isolated editable identity before each IME
  operation rejects a focus change that has not reached the asynchronous
  observer yet. Preceding input responses settle before that read, because CDP
  input dispatch and Runtime evaluation use different renderer queues. The
  runtime waits for both without blocking other devices. Later input to the same
  device waits behind the operation, in order, and counts against the device's
  input budget. A navigation, a new document, hiding, a crash or an unresponsive
  page drops the waiting operation and that input; neither is sent later. The
  check is not an atomic transaction with arbitrary page scripts.
- Vendor the checksum-verified GPUI 0.2.2 crate with two platform fixes: all Wayland
  IME commits use the text handler, and newly created XIM contexts receive
  `SetIcFocus`, without which Fcitx5 does not compose. Keep its version, license and transitive pins;
  document the patch in `vendor/gpui-0.2.2/BROXSER-PATCH.md` and remove it when an
  upstream release provides a suitable contract.

## Scope and limits

The initial caret observer covers ordinary main-frame text inputs, textareas and
contenteditable. Password fields, iframe editors, shadow-root editors, canvas
editors, and arbitrary surrounding-text replacement are not qualified by this
change. The page remains authoritative: it may reject composition/input events or
move its focus through script. A snapshot and a later CDP action cannot make those
scripted changes atomic.

Native callbacks carry no original-target identifier. After target loss, the
desktop retains a cancelled composition through empty preedit and the following
commit. A new nonempty preedit after a native boundary establishes a new
composition. If an IME sends no terminal/reset callback, the first later
commit-only input remains ambiguous and is discarded. This conservative limit
needs qualification for other IMEs; it must not be turned into a replay into a
new field. Native unmark is finalization, so it does not discard a later fresh
commit. Empty cleanup after a successful commit does not create a new composition.

Candidate windows belong to the native IME. Browser frames, candidate placement,
and input-state reports are asynchronous; hardware, compositor, scaling, language
engine, and complex editor combinations need explicit qualification. This does
not imply full embedded-webview text, accessibility, or mobile virtual-keyboard
support. Copy to the system clipboard remains a separate gap from ADR 0010.
For Fcitx5 XIM, the input method must run before Broxser and
`conf/xim.conf` must enable `UseOnTheSpot=True` to send preedit to the application.
Otherwise Fcitx shows preedit in its own panel and sends only the final commit.
The test harness configures this only in its temporary profile, never the user's.

## Validation

The engine tests must cover composition events and final values, cancel, repeated
identical commits without key-up, target isolation, stale/hidden targets, caret
updates, UTF-16 boundaries, and forged or malformed observer reports. They must
also cover a page that answers preceding input slowly: the first version bounded
the identity check at 250 ms and dropped genuine commits on a loaded CI runner.
Desktop tests must cover composition ownership through focus/lifecycle changes,
UTF-16 range handling, commit without physical-key bookkeeping, and coordinate
mapping.

`examples/fixture/ime.html` records only local test input. `scripts/ime-smoke.py`
runs a real Fcitx5 Pinyin engine over XIM with private configuration and D-Bus,
using owned Helium profiles and the fixture. Synthetic CDP composition tests alone
are not evidence that the native IME path works. Record the executed commands,
passed/failed checks and unverified platform combinations in `docs/validation.md`.

## References

- [CDP Input protocol](https://github.com/ChromeDevTools/devtools-protocol/blob/master/json/browser_protocol.json)
- [Fcitx5 setup](https://fcitx-im.org/wiki/Setup_Fcitx_5)
- GPUI 0.2.2 `src/input.rs`, `examples/input.rs`, and Linux X11/Wayland input paths
  in the retained upstream source.
