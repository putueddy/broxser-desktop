# ADR 0021 Persistent sessions: one on-disk browser profile per persistent session

Status: proposed for P2.2, 2026-09-27; accepted on 2026-09-29, on the owner's
delegation, with the product decisions below (see "Decisions, 2026-09-29").
Nothing is implemented yet. Persistent sessions stay unavailable, and every
session ephemeral, until each gate below has evidence; the stages that build
them are separate changes. Builds on ADR 0002 (portable workspace), ADR 0003
(engine updates), ADR 0004 (blocker scope), ADR 0007 (profile ownership and
cleanup), ADR 0019 (temporary discovery) and ADR 0020 (private home and
profile-encryption backend).

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
| Session cookies (no expiry, the `HttpOnly` fixture cookie among them), sessionStorage without restore enabled | Gone in the measured restarts; this is not a qualification of real application login |
| Session cookies with `session.restore_on_startup = 1` in the profile's `Preferences` | Kept across a clean close: Chromium persists session cookies when it would restore the last session |
| SIGKILL 12 s after the cookies were set | Every cookie lost (the cookie store writes on a timer and at shutdown); localStorage and IndexedDB kept; `exit_type` recorded as `Crashed` |
| An off-the-record context in the same browser | Sees none of the profile's cookies, storage or permission decisions |
| `Browser.setDownloadBehavior` and `Browser.setPermission` without `browserContextId` | Apply to the default context: the download is refused and `notifications` is `denied`, while a created context still reports `prompt` |
| Cookies at rest | `Default/Cookies` (SQLite), values `v10` AES-128-CBC under the fixed password of `--password-store=basic` (ADR 0020): decrypted here with a short script; session cookies are on disk too until the next start |
| Profile on disk | 2.6 MB after one run and 5.0 MB after five (GPU caches, History, Web Data, Login Data, Extension State); the previous run's `DevToolsActivePort` stays behind |
| The profile opened by Chromium 141 (a downgrade from 154) | Answered `Browser.getVersion` and no target query within 8 s; Helium 154 used the profile again afterwards |
| Start with a warm profile | About 150 ms to the CDP endpoint in this probe; not full Broxser startup, restored-page readiness or a resource budget |

This proposal chooses a separate browser/default context and application-owned
user-data directory for each persistent session. Under that adapter contract,
N persistent sessions mean N browser process trees (about 15 processes per
browser in this fixture); ephemeral contexts can share another browser. This is
an isolation design choice, not a claim about every Chromium profile topology.

## Options

| Option | Assessment |
| --- | --- |
| Cookies in the workspace JSON | Rejected by `GOALS.md`: credentials in a shareable file and incomplete site storage; `HttpOnly` does not prevent privileged CDP cookie access |
| `Network.getCookies` and `setCookies` into a Broxser-owned store | Cookies only; localStorage and IndexedDB lost; re-implements what a profile does and adds a secret store to design |
| One on-disk profile per persistent session | The browser's own storage, isolation and deletion; one browser per persistent session; needs a clean shutdown path and a version binding |
| One on-disk profile shared by a workspace's sessions | Sessions would share cookies: the isolation ADR 0002 promises is gone |

## Decision

- A session can be marked persistent in the workspace: a flag, never a
  secret. The workspace JSON stays free of cookies, tokens and passwords, and
  the flag comes with a schema change under ADR 0002's migration rule.
  It expresses intent, not authorization to open an existing local login or
  enable durable credential storage without explicit local opt-in.
  Local profile binding and copied/imported workspace behavior need the identity
  gate below; names and session IDs alone must not select a credential directory.
- Each persistent session gets its own browser with a Broxser-owned profile
  directory under the user's data directory (`$XDG_DATA_HOME/broxser`, mode
  0700; `~/.local/share/broxser` when XDG_DATA_HOME is unset or invalid), with
  ADR 0020's private home inside it. Initialize a new profile once; reopening
  must preserve its data and preferences rather than rerun the temporary-profile
  writer. Ephemeral sessions stay contexts in the shared browser. Download
  refusal and permission denials must apply to the default context before any
  application page can run there.
- The existing blocker seed is **not sufficient** for the default context:
  `seed_profile` only writes the extension's `incognito = false`. ADR 0004
  explicitly leaves the blocker running in the unused default context. A
  separate, measured default-context configuration and extension guard must
  prevent the known reload/replay behavior before this proposal is accepted.
  Ordinary extension-disable flags were already found ineffective in ADR 0004.
- `session.restore_on_startup = 1` is a candidate requiring qualification,
  not an approved unconditional seed. It retained session cookies in the audit,
  but it controls session/tab restoration; behavior depends on launch arguments
  and browser mode. Prove that the exact launcher loads no saved tab, URL,
  form/authentication flow or other stale web action before Broxser's explicit
  navigation and safety setup. If cookie retention cannot meet the no-replay
  contract, defer it or explicitly scope support to the state that can be safely
  retained. Do not turn session cookies into expiry cookies to evade this gate.
- UA/version discovery always uses a separate temporary profile (ADR 0019),
  never the persistent profile. Complete ownership and compatibility checks
  before opening persistent state. A prior `DevToolsActivePort` is not an
  endpoint for the new launch: require fresh endpoint evidence belonging to
  the newly owned browser before connecting.
- Stopping a persistent browser is graceful: `Browser.close` with a deadline,
  then bounded termination of owned processes under ADR 0007. Normal shutdown,
  failed startup, fallback Drop, guardian and stale recovery must all retain
  persistent profile data. The existing ephemeral cleanup cannot be reused
  unchanged: it deletes through all those paths. Only an explicit Forget action
  may delete a validated owned profile, after its processes have stopped and
  exclusive ownership has been established. A crash/kill can lose recently
  written cookies; the loss window has not been bounded by this audit.
- Record the creator and the **last writer's full qualified runtime identity**
  (browser product/build and version, including the Chromium version), not only
  a major number. Compatibility must follow tested upgrade/migration rules;
  same-major equality does not authorize an older patch/minor build or a
  different product to write the original profile. Unsupported combinations
  leave it unopened and offer explicit forgetting/re-authentication or a
  separately qualified migration. Qualify on consistent, protected copies of
  stopped profiles, never by experimenting on the original. An incompatible
  profile must not keep a vulnerable browser installed: follow ADR 0003's
  security updates, with ephemeral use or an unavailable persistent session
  until compatibility is resolved.
- Secrets at rest are protected by the directory mode and the disk, not by a
  key: `--password-store=basic` (ADR 0020) is a fixed key, and a desktop
  keyring would require a separate Broxser secret-store design instead of
  silently borrowing the personal Helium entry. This no-keyring design is
  accepted for durable credentials by decision 2 below; ADR 0020 only
  qualified the fixed-key choice for ephemeral profiles. Broxser would maintain
  no separate exported cookie store. Forget deletes the owned profile directory;
  it does not promise secure erasure or removal from captures, exports, backups,
  OS storage or the remote application. Migration copies are sensitive data too
  and require an explicit retention/deletion policy.
- Nothing is persisted by default; persistence and forgetting are explicit
  actions in the workspace UI.

## Decisions, 2026-09-29

The owner delegated the product gate. These decisions settle it; the other
gates stay engineering acceptance criteria.

1. **Where logins may be kept.** Persistent sessions are for a developer's own
   account on their own machine, and they let an application under test keep
   its login there. Broxser cannot tell whether a disk is encrypted or an
   account shared, so it refuses nothing on a guess. Instead, the explicit
   local opt-in states what is kept (the browser profile: cookies, site storage,
   history and caches), where (`$XDG_DATA_HOME/broxser`, mode 0700), and how it
   is protected: by the file permissions and the disk only, because the key is
   fixed and anyone who can read those files can read the cookies. It
   recommends full-disk encryption and advises against shared accounts.
2. **Fixed-key storage is accepted for this use.** `--password-store=basic`
   stays; there is no keyring item. A Broxser-owned secret store remains
   possible later and would be its own decision. Broxser makes no backups of a
   persistent profile. The one copy it makes is the protected copy before a
   newer Helium first opens the profile (decision 5): kept beside the profile
   under the same protection, and deleted once the newer Helium has closed the
   profile cleanly. Forget deletes the profile directory and any such copy,
   without promising secure erasure; backups the user makes are outside
   Broxser's control and are named in the opt-in.
3. **The binding is local and explicit.** Opting in records a binding in
   Broxser's application state on that machine, never in the workspace file. A
   copied, imported or moved workspace, or another workspace with the same
   names and session IDs, never reaches an existing profile: it starts empty,
   unless the user binds it to a profile chosen from the list of decision 4.
4. **Retention.** A profile lives until the user forgets it. Profiles whose
   binding is gone are listed for forgetting; nothing is deleted automatically,
   and the list shows when each profile was last used.
5. **Versions.** A profile last written by a newer Helium, or by another
   product, is not opened: the session is unavailable until Helium is updated
   or the user forgets the profile. A profile from an older Helium that was
   qualified for Broxser opens after the protected copy of decision 2: that is
   the upgrade Chromium supports, and the qualification of the newer Helium
   (ADR 0026) checks it on a copy first. Security updates are never held back
   for a profile (ADR 0003).
6. **What survives a restart** is only what qualification shows. Cookies with
   an expiry, localStorage and IndexedDB are the audit's candidates. Session
   cookies survive only if `session.restore_on_startup` passes the no-replay
   gate; otherwise the opt-in says that logins kept in session cookies end with
   the browser.

The gates below are built in stages, each a separate change with its own
evidence: (1) the profile store, ownership, retention on every cleanup path,
graceful close and Forget; (2) the default context's policies, the blocker and
the no-replay qualification; (3) the runtime identity, the upgrade copy and an
upgrade check in the Helium qualification; (4) the workspace flag with its
migration, the opt-in, Forget and busy state in the UI; (5) resource and
durability measurements. The UI offers persistence only after all five have
evidence.

## Gates before persistent sessions are available

- Product: settled by the decisions of 2026-09-29 above.
- Identity/ownership: stable local profile binding, no implicit credential reuse
  from an imported/copied workspace, two workspaces with identical names/session
  IDs still isolated, and one writer per profile. Refuse busy, foreign or invalid
  bindings; untrusted workspace paths, symlinks or lease files must never grant
  permission to open or delete another profile. Show busy state without breaking
  another owner's locks.
- Lifecycle: graceful close and data retention across every owner/Drop/guardian/
  recovery path, including failed startup and interrupted Forget. Prove process
  cleanup, fresh endpoint ownership and no background use after close. Measure
  durability/loss under crash, kill and interrupted writes before claiming a
  bounded loss interval. Version binding, protected-copy migration, update and
  rollback must satisfy ADR 0003 and be qualified together in P3.1.
- Navigation/default context: reproducible first-start, clean-restart and crash-
  restart traces showing the blocker is controlled, policies are installed before
  application execution, and no old tab/navigation/action is restored or replayed.
  Cookie survival alone does not pass this gate.
- Resources: multiple persistent browsers plus any ephemeral/discovery browser
  measured against P1.4; qualify process, memory, latency and disk growth. The
  small fixture's profile sizes are not an upper bound.
- Migration/evidence: test workspace schema migration and protected backups;
  retain a reproducible scratch harness or equivalent contract tests with exact
  runtime identity, configuration and sanitized results before acceptance. The
  original uncommitted probe is an audit note, not a release qualification suite.

## Consequences

- A persistent session costs a browser process tree, with disk usage driven by
  the site's storage, history and caches. Only the cookie/storage and policy
  behavior established by qualification may be promised across restarts.
- The security model gains a directory that outlives the session: mode 0700,
  fixed-key encryption, explicit deletion, and no keyring item.
- The audit observed session-cookie retention with the restore preference and
  clean close; shipping that behavior still depends on the no-replay gate. A
  site can expire or revoke its login independently, and crash durability is
  not established by a successful clean close.

## Validation

No persistent-session implementation or acceptance validation yet. The original
probe's reported measurements and the documentation review are in
`docs/validation.md` (P2.2 audit).

The restore concern follows Chromium's
[startup restoration test](https://raw.githubusercontent.com/chromium/chromium/main/chrome/browser/policy/test/restore_on_startup_policy_browsertest.cc),
which also treats launch arguments as relevant. It is not a measurement that
the pinned Helium launcher restores pages. CDP's
[Storage.getCookies](https://raw.githubusercontent.com/ChromeDevTools/devtools-protocol/master/pdl/domains/Storage.pdl)
returns browser cookies, whose
[Network.Cookie fields](https://raw.githubusercontent.com/ChromeDevTools/devtools-protocol/master/pdl/domains/Network.pdl)
include `httpOnly`; rejecting a cookie-export design must rest on secrecy and
incomplete session coverage, not on a page-JavaScript access restriction.
