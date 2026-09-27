# ADR 0021 Persistent sessions: one on-disk browser profile per persistent session

Status: proposed for P2.2, 2026-09-27. Not accepted: nothing here is implemented,
and the default stays ephemeral until the gates below pass. Builds on ADR 0002
(portable workspace), ADR 0007 (profile ownership and cleanup) and ADR 0020
(private home and no keyring).

## Context

Every Broxser session is a CDP browser context (`Target.createBrowserContext`)
in the one browser of a workspace: off-the-record, in memory, gone with the
browser, and CDP offers no option to make a context persistent. `GOALS.md`
(P2.2) asks for persistent login with a profile and secret-store design and a
tested migration, and rules out exporting cookies into a JSON file and calling
it a session. A scratch probe (Helium 0.18.1.1, an on-disk profile as
`--user-data-dir` with Broxser's launch flags, the page in the browser's
default context, a fixture that sets three cookies, localStorage,
sessionStorage and IndexedDB) measured what such a profile does:

| What | Measured |
| --- | --- |
| A cookie with an expiry, localStorage, IndexedDB | Kept across a clean close (`Browser.close`) and across a SIGKILL of a browser that had flushed them before |
| Session cookies (no expiry, the `HttpOnly` login cookie among them), sessionStorage | Gone after every restart: Chromium drops them at startup |
| Session cookies with `session.restore_on_startup = 1` in the profile's `Preferences` | Kept across a clean close: Chromium persists session cookies when it would restore the last session |
| SIGKILL 12 s after the cookies were set | Every cookie lost (the cookie store writes on a timer and at shutdown); localStorage and IndexedDB kept; `exit_type` recorded as `Crashed` |
| An off-the-record context in the same browser | Sees none of the profile's cookies, storage or permission decisions |
| `Browser.setDownloadBehavior` and `Browser.setPermission` without `browserContextId` | Apply to the default context: the download is refused and `notifications` is `denied`, while a created context still reports `prompt` |
| Cookies at rest | `Default/Cookies` (SQLite), values `v10` AES-128-CBC under the fixed password of `--password-store=basic` (ADR 0020): decrypted here with a short script; session cookies are on disk too until the next start |
| Profile on disk | 2.6 MB after one run and 5.0 MB after five (GPU caches, History, Web Data, Login Data, Extension State); the previous run's `DevToolsActivePort` stays behind |
| The profile opened by Chromium 141 (a downgrade from 154) | Answered `Browser.getVersion` and no target query within 8 s; Helium 154 used the profile again afterwards |
| Start with a warm profile | 150 ms to the CDP endpoint, the same as a fresh one |

One browser holds one on-disk profile, so N persistent sessions need N browser
processes (about 15 processes each here) where N ephemeral sessions are N
contexts in one.

## Options

| Option | Assessment |
| --- | --- |
| Cookies in the workspace JSON | Rejected by `GOALS.md`: credentials in a shareable file, no storage, nothing for `HttpOnly` cookies |
| `Network.getCookies` and `setCookies` into a Broxser-owned store | Cookies only; localStorage and IndexedDB lost; re-implements what a profile does and adds a secret store to design |
| One on-disk profile per persistent session | The browser's own storage, isolation and deletion; one browser per persistent session; needs a clean shutdown path and a version binding |
| One on-disk profile shared by a workspace's sessions | Sessions would share cookies: the isolation ADR 0002 promises is gone |

## Proposed decision

- A session can be marked persistent in the workspace: a flag, never a
  secret. The workspace JSON stays free of cookies, tokens and passwords, and
  the flag comes with a schema change under ADR 0002's migration rule.
- Each persistent session gets its own browser with a Broxser-owned profile
  directory under the user's data directory (`$XDG_DATA_HOME/broxser`, mode
  0700, one directory per workspace and session), seeded like a temporary
  profile (blocker off, ADR 0004) plus `session.restore_on_startup = 1`, so
  logins that use session cookies survive a restart, and with the private home
  of ADR 0020 inside it. Ephemeral sessions stay contexts in the shared
  browser. The download refusal and permission denials go to the default
  context through the browser-level commands.
- Stopping a persistent browser is graceful: `Browser.close` with a deadline,
  then the kill and cleanup of ADR 0007; the guardian never deletes a
  persistent profile. Until then, a crash or kill can lose cookies set in the
  last flush interval, which the card must say.
- The profile records the browser version that created it. Broxser opens it
  only with the same major version and otherwise offers to forget it: no
  downgrade, no silent reset.
- Secrets at rest are protected by the directory mode and the disk, not by a
  key: `--password-store=basic` (ADR 0020) is a fixed key, and a desktop
  keyring would share the key with the personal Helium profile. Forgetting a
  session deletes its directory, and nothing else holds its data.
- Nothing is persisted by default; persistence and forgetting are explicit
  actions in the workspace UI.

## Gates before acceptance

- Product: which machines may hold persistent sessions (disk encryption as a
  condition?) and whether an application under test may keep a login on a
  shared machine.
- Lifecycle: graceful close proven under the crash and kill scenarios of
  ADR 0007 with no cookie loss beyond the flush interval, and the version
  binding proven against a Helium update and rollback (P3.1).
- Resources: N persistent browsers measured against the P1.4 numbers.
- Migration: the workspace schema change with a tested migration and backup.

## Consequences if accepted

- A persistent session costs a browser process tree and a few megabytes on
  disk per session, and survives Broxser restarts with its cookies, storage
  and permission decisions.
- The security model gains a directory that outlives the session: mode 0700,
  fixed-key encryption, explicit deletion, and no keyring item.
- Session cookies survive only through the restore-on-startup preference and
  a clean close; a page that expires its own login still logs out.

## Validation

None yet: this is a proposal. The probe's measurements are in
`docs/validation.md` (P2.2 audit).
