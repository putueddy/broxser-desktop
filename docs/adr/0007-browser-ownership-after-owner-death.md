# ADR 0007 Browser ownership after the Broxser process dies

Status: accepted for P0, 2026-09-25. Re-verify with every Helium update.

## Context

`BrowserProcess` stops its browser and deletes the private profile on normal
close, errors and cancellation. None of that runs when the Broxser process gets
SIGKILL, SIGTERM (Broxser installs no handler), or crashes. A deterministic
reproducer (a re-executed test binary that owns a browser through the ordinary
engine API and is then killed) showed on HEAD `d6e7e97`, Helium 0.18.1.1:

| Owner death | Five seconds later |
| --- | --- |
| SIGKILL or SIGTERM during a held capture request | 15 of 15 browser processes running, CDP port open, profile present |
| SIGKILL or SIGTERM with live frames streaming | 15 of 15 running, CDP port open, profile present |
| SIGKILL, SIGTERM or `abort()` during startup (fake browser) | Browser running, profile present |
| SIGTERM to the owner's process group | Browser gone, profile present |

Measured mechanisms, parent SIGKILLed while Helium runs:

| Mechanism | Browser processes | Profile |
| --- | --- | --- |
| CDP websocket held by the parent (current) | All 12 alive after 10 s, port open | Left |
| `PR_SET_PDEATHSIG=SIGKILL` (via `setpriv`) | 0 after 0.5 s | Left |
| `--remote-debugging-pipe`, fds held by the parent | 0 after 0.5 s | Left |
| SIGKILL of only the main browser process | All 12, including detached crash handlers, gone in 37–87 ms | Left |

Closing the websocket does not stop the browser. The crash handlers run in their
own sessions, outside the browser's process tree and process group. The profile
directory was also created with mode 0755, so `DevToolsActivePort` and several
metadata files were readable by other local users; Chromium itself uses 0700.

## Options

| Option | Assessment |
| --- | --- |
| Signal handlers in Broxser | Cannot see SIGKILL, OOM kills or crashes; needs `unsafe` (forbidden) |
| `PR_SET_PDEATHSIG` on the browser | Kills the tree, but needs `unsafe` `pre_exec` or an external trampoline, fires when the spawning *thread* exits, and leaves the profile with cookies on disk |
| Pipe CDP transport | Browser exits on EOF, but that is browser behavior rather than a guarantee we own; needs fd passing (`unsafe` or a new crate) and a transport rewrite; profile left. Still a candidate for local CDP hardening |
| Process group or cgroup kill | No ownership proof; crash handlers leave the group; a systemd user scope is not always available and removes no files |
| **Guardian with a liveness pipe** | Owns cleanup after any kind of owner death, keeps identity checks, removes the profile; one small process per browser |

## Decision

- **Private profile.** Created with mode 0700 under the same root as before. A
  lease `broxser-lease.json` inside it records version, boot ID, PID namespace
  and the owner, guardian and browser as PID plus kernel start time. It is
  written atomically (`create_new`, fsync, rename, directory fsync) before the
  guardian starts and rewritten as identities become known. Profile removal
  deletes the lease last, so an interrupted removal stays recognizable.
- **Guardian.** Before the browser starts, the engine re-executes the running
  Broxser executable (`/proc/self/exe --broxser-browser-guardian`). The guardian
  calls `setsid`, so terminal signals to Broxser's process group (Ctrl+C,
  hangup) do not reach it, checks that the lease names its owner and that
  pidfds work (Linux 5.3+), and prints `ready`; otherwise no browser is launched.
  Its stdin is a pipe whose only writer is the owner (close-on-exec, so the
  browser never inherits it); the kernel closes it however the owner ends.
  The owner reports `browser <pid> <start-time>` right after spawning. After its
  own cleanup the owner sends `release` and reaps the guardian.
- **Cleanup by the guardian** on EOF without `release`: SIGKILL the reported
  browser through a pidfd whose target is re-checked against the recorded start
  time, plus processes carrying the exact `--user-data-dir=<profile>` argument
  (this covers a browser spawned in the microseconds before it was reported);
  wait up to five seconds for the browser tree and until no running process
  names the profile; then delete the profile if its lease still names the same
  owner. Other processes are never signaled; `pkill`, executable names and
  wildcards are not used.
- **Release before removal.** Every cleanup path (the owner's shutdown, its
  drop fallback, the guardian and recovery) waits for the processes it recorded
  and then re-scans until no running process names the profile, before deleting
  it. Helium helpers write profile files while they stop and create missing
  directories, and a helper started after the record would otherwise put the
  profile back after its removal (CI run #29). Every Helium process observed
  carries the profile path in its command line, so each re-scan also finds
  processes the helpers start. Waiting never signals: processes that merely name
  the profile are not killed.
- **Recovery on the next start.** Before creating a profile, the engine checks
  `broxser-cdp-*` entries in the same root. An entry is stale only when it is a
  real directory owned by the user, with a valid version-1 lease, and either
  the boot ID changed or, in the same boot and PID namespace, neither the owner
  nor the guardian is running (a zombie or a reused PID with another start time
  is not the recorded process). A recorded browser that is still running and
  still carries that profile argument is stopped first. Symlinks, other users'
  entries, entries without a valid lease, leases from another PID namespace or
  a future version, and profiles any process still names are left alone.
  Deletion never follows symlinks.
- Recovery restores configuration only. It never replays clicks, typing, forms,
  authentication, payments or navigations; opening a workspace and Restart stay
  explicit user actions that load the URL once.
- Every binary that launches browsers calls
  `broxser_engine::run_guardian_if_requested()` first in `main`. A binary that
  does not cannot start browsers: the guardian never reports `ready` and the
  launch fails with an error naming the call.

## Consequences and limits

- Covered: SIGKILL, SIGTERM and crashes of the owner during startup, held
  requests, live frames and teardown, in live and capture mode. See
  `docs/validation.md` for measured cleanup times against the five-second target.
- If the guardian dies together with Broxser (cgroup or systemd scope stop,
  targeted kills of both, power loss), the profile stays until the next Broxser
  start in the same root. Profiles in a root that is never used again are not
  recovered; `/tmp` on tmpfs is cleared at boot.
- If only the guardian dies, the run continues and normal cleanup still works;
  should Broxser then die too, the next start in that root recovers the profile.
- A process in uninterruptible sleep can outlast the wait; the guardian still
  deletes the profile and exits with a failure status.
- The browser's helpers stopping with its main process is measured behavior,
  not a documented upstream guarantee; the live tests re-check it per update.
- Static-mode preview directories (desktop screenshots) are outside this
  mechanism and remain after the desktop is killed. Chromium's process-singleton
  directory `org.chromium.Chromium.*` in the temporary directory remains after
  every SIGKILL of a browser, including normal close; it holds a dead socket and
  no page data.

## Validation

Regression tests cover the owner roles above with SIGKILL, SIGTERM, `abort()`
and a process-group SIGTERM, a second instance in the same root that must keep
running, guardian protocol edge cases (release, EOF, reused PID, unreported
browser, foreign lease) and stale-profile recovery (dead, zombie and reused
owners, previous boot, live guardian, symlinks, malformed or future leases,
other namespaces, profiles still in use, a whole killed tree and an orphaned
Helium). A fake browser whose helper starts a new process that writes into the
profile after the browser is gone checks the release rule for the owner's
shutdown and for the guardian. A CLI test kills the real `broxser` binary during
a capture, and the X11 smoke script kills the real desktop with SIGKILL, Ctrl+C
and SIGTERM.

Measured on 25 September 2026 (`docs/validation.md`): after owner death the
Helium instance, its CDP endpoint and its profile were gone in 38–99 ms (43–105
ms with the release rule, 60–240 ms with four parallel tests); the fake-browser
and CLI cases took 5–65 ms; an orphaned Helium was stopped and its profile
removed 56–60 ms after the next start in the same root. A guardian costs
one thread and about 12 MB RSS, mostly shared pages, and was ready in 4–26 ms.
