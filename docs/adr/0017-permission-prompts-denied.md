# ADR 0017 Permission prompts are denied per session context

Status: accepted for P1.6 (fourth capability), 2026-09-27. Amends ADR 0005.

## Context

Broxser cannot show a permission prompt: the browser runs headless and the
device card has no place for one. A CDP probe against Helium 0.18.1.1 (Chrome
154) requested every permission-gated API from one page target, with and
without a user gesture, and then with `Browser.setPermission` `denied` for the
browser context; a run of the live runtime at `43cb397` asked for the
notification permission on three devices. Findings:

- `Notification.requestPermission()` waits for a prompt nobody can answer.
  Without an explicit setting the headless browser resolves it `denied` after
  1.2–1.8 s in the probe, and in the live runtime one device of three was still
  waiting after 10 s. `IdleDetector.requestPermission()` behaves the same
  (2.6–3.8 s). Meanwhile `navigator.permissions.query()` reports `prompt` for
  `notifications`, `idle-detection`, `camera`, `microphone`, `display-capture`,
  `local-fonts`, `persistent-storage` and `push`, so a page believes a prompt
  is possible and may show "enable notifications" UI that can never succeed.
- With `Browser.setPermission` `denied` for a context, the request resolves
  `denied` in under 20 ms and `query` reports `denied`.
- Everything else already fails or succeeds at once: geolocation and MIDI are
  denied by default, `getUserMedia` fails with `NotFoundError` (no devices
  here), clipboard read, pointer lock and window management throw
  `NotAllowedError`, push subscription throws `AbortError`, and clipboard
  write, wake lock, fullscreen and persistent storage answer immediately.
  `getDisplayMedia()` never resolves, and denying `display-capture` does not
  change that.
- Denying more than needed changes page behavior: `clipboard-write` denied
  breaks copy buttons, `screen-wake-lock` denied rejects wake locks, and
  `local-fonts` denied made `queryLocalFonts()` resolve instead of throwing.
  `Browser.setPermission` takes web permission descriptor names; the
  `PermissionType` names of `Browser.grantPermissions` are rejected.
- CDP reports no event for a permission request, so Broxser cannot count or
  show them.

## Options

| Option | Assessment |
| --- | --- |
| Leave the headless default | Requests wait seconds or forever for a prompt that never comes, and `query` claims a prompt is possible |
| Deny every permission | Instant answers, but copy, wake lock and font access change behavior pages rely on |
| Deny the prompt-type permissions that wait | Instant, truthful answers for what would otherwise hang; nothing else changes |
| Grant on request through a Broxser prompt | Needs a permission event CDP does not send, and a decision about what a QA session may access; its own capability |

## Decision

- Every session context denies `notifications`, `idle-detection`, `camera`
  and `microphone` (`Browser.setPermission`, `denied`, all origins) right
  after the context is created, in live and capture runs. A page that asks
  gets `denied` at once and `permissions.query` says so.
- Camera denial includes both the ordinary `{"name":"camera"}` descriptor and
  `{"name":"camera","panTiltZoom":true}`. Chromium treats camera movement
  controls as a separate permission, so both descriptors must be denied for
  their query states to consistently reflect Broxser's camera policy.
- Camera and microphone are denied on the same ground (a prompt-type
  permission whose request would wait) although no device here could exercise
  them; a machine with a webcam is expected to see `NotAllowedError` at once
  instead of a prompt.
- Nothing else is set: permissions the headless browser already denies or
  grants keep that answer, and the default of a capability that works without
  a prompt is not changed.
- No card element: without a CDP event there is nothing truthful to count.

## Consequences

- Web push, notification and presence-detection flows fail at once with
  `denied`, and pages that check first see `denied`; nobody waits on a prompt.
- Video call and recording pages get `NotAllowedError` at once in Broxser.
- Screen capture (`getDisplayMedia`) still never resolves; it is recorded as a
  limit. Granting a permission for a QA scenario is a separate decision.

## Validation

Fake CDP: `permission_prompts_are_denied_in_every_session_context` (the five
descriptor denials per context, including ordinary camera and PTZ, in order). Helium:
`live_permission_requests_are_denied_at_once` (a page asks without a gesture on
load: every device is answered `denied` within 500 ms and `query` reports
`denied`; before the change two devices waited 1.5–1.8 s and one was not
answered within 10 s, all reporting `prompt`). Results are in
`docs/validation.md` (P1.6).

`live_camera_permission_queries_deny_ptz_across_origins` checks ordinary camera
and PTZ queries on three devices in two session contexts, first on `127.0.0.1`
and then on `localhost`. Both camera descriptors must report `denied`, while
clipboard-write and screen-wake-lock remain `granted`. This query-only regression
uses no physical or simulated camera and fails on the original PTZ policy gap.
