# ADR 0020 Certificate store and keyring: the browser's own, inside the private profile

Status: accepted for P2.1, 2026-09-27. Amends ADR 0005 (limits) and the
security model in `SECURITY.md`; the profile of ADR 0007 now also holds the
browser's home directory.

## Context

P2.1 asks what the browser opens outside its private profile, and how a
corporate CA is handled, before persistent sessions are designed; the security
model recorded that Chromium still opened the user's shared NSS database.
Traced `broxser capture` runs (`strace` on file opens, `mkdir` and `connect`;
Helium 0.18.1.1, the unprivileged user `broxsertest` with the sandbox on, a
local HTTPS fixture whose certificate a test CA signed) found:

- Over HTTP the browser reads from the home directory only fontconfig's caches
  (`~/.cache/fontconfig`), Vulkan layer directories (`~/.local/share/vulkan`,
  `~/.config/vulkan`), `~/.local/share/glib-2.0` and `~/.config/user-dirs.dirs`,
  and writes nothing there. It reads `/etc/chromium/policies/{managed,recommended}`
  and connects to the system bus, nscd and the syslog socket.
- The first HTTPS certificate verification opens the user's NSS database
  read-write: `~/.pki/nssdb/cert9.db` and `key4.db` with `O_CREAT`, `pkcs11.txt`,
  and a lookup of `libnssckbi.so` there. A CA imported into that database with
  `certutil` was trusted by Broxser's browser; the same database holds a
  user's client certificates and private keys, which the browser offers to
  sites that ask. With `XDG_DATA_HOME` set, Chromium 154 uses
  `$XDG_DATA_HOME/pki/nssdb` instead and creates it.
- The `CACertificates` enterprise policy (a JSON file under
  `/etc/chromium/policies/managed`) made the browser trust the test CA with no
  CA in any NSS database; removing the file removed the trust. Helium reads
  that directory.
- A private `HOME` isolated NSS: the CA in the user's database was no longer
  trusted (`net::ERR_CERT_AUTHORITY_INVALID`) while the policy CA still was.
  Without the user's cache directory, fontconfig rebuilt its caches into the
  new home at every start: 10.0 s for the trusted capture against 7.4 s.
  Passing the user's `XDG_CACHE_HOME` kept the caches (30 cache files reused);
  passing `XDG_DATA_HOME` reopened, and created, `~/.local/share/pki/nssdb`.
- Keyring: under `XDG_CURRENT_DESKTOP=GNOME` with a session bus address the
  browser connected to the session bus 11 times, 10 with
  `--password-store=basic`. Chromium's Linux `os_crypt` asks the desktop's
  Secret Service or KWallet for the "Safe Storage" key that also protects a
  personal profile's cookies, creates one when it is missing, and a locked
  keyring prompts the user outside Broxser. No keyring daemon runs here, so
  the item itself was not observed.

## Options

| Option | Assessment |
| --- | --- |
| Keep sharing the user's NSS database | CAs a user added keep working, but so do that user's client certificates and any CA they trust privately, and a profile Broxser deletes depends on state it does not own |
| Private `HOME` inside the profile, the user's cache directory kept | The browser's NSS database lives and dies with the profile; system fonts and their caches stay; fonts and a `fontconfig` configuration under the user's home are no longer seen |
| Copy CAs from the user's database into the private one | Needs NSS tooling or parsing its SQLite files, and copies trust decisions Broxser cannot audit |
| Corporate CA through the `CACertificates` policy | Machine level and administrator owned, documented by Chromium, the same for the user's Helium and for Broxser; nothing is copied |
| Load the system trust store through p11-kit | Distribution-specific module paths; Broxser would trust more than the user's own Helium; not measured |
| Accept certificate errors for chosen hosts | A TLS bypass, which `GOALS.md` rules out |

## Decision

- The browser runs with `HOME` set to a directory inside its profile
  (`<profile>/home`, mode 0700), without `XDG_DATA_HOME` and `XDG_CONFIG_HOME`,
  and with the user's `XDG_CACHE_HOME` (or `~/.cache`) passed on. NSS creates
  its database under that home, and the database goes with the profile.
  Nothing is read from or copied out of the user's database.
- Preserve explicit `XAUTHORITY` unchanged. If it is unset, resolve the caller's
  original nonempty `HOME/.Xauthority` before replacing HOME, so authenticated
  X11 headed launches still work. This retains a path for display authorization;
  no authority file or certificate database is copied into the profile.
- Remove inherited `SSLKEYLOGFILE` on every launch so ambient TLS debugging
  cannot leave handshake secrets outside the deleted profile.
- Every launch carries `--password-store=basic`: profile encryption does not
  request the desktop's Secret Service or KWallet key. Its fixed-key protection
  is not a substitute for private directory permissions and ephemeral cleanup.
- A corporate CA is configured explicitly through the `CACertificates` policy
  in `/etc/chromium/policies/managed`, which applies to Broxser and to the
  user's Helium alike. Broxser appends to `net::ERR_CERT_AUTHORITY_INVALID`
  what it trusts (built-in roots and that policy, not a personal certificate
  store). In live mode the browser's own error page stays in the frame. The last
  confirmed navigation failure survives a same-page Reload, whose CDP reply
  need not contain an error; a successful document commit clears it. A new
  explicit navigation starts a new report rather than carrying the old one.
- Client certificates from the caller's NSS database are not imported into
  Broxser sessions. Client-certificate provisioning and other platform providers
  remain unqualified and are decided with persistent sessions (P2.2).

## Consequences

- A CA trusted only in a user's `~/.pki/nssdb` is no longer trusted: those
  pages fail with `ERR_CERT_AUTHORITY_INVALID` until an administrator installs
  the CA by policy, which the user's own Helium honours too.
- Fonts under the user's home (`~/.fonts`, `~/.local/share/fonts`) and a user
  `fontconfig` configuration are no longer used by pages in Broxser; system
  fonts and their caches are unchanged. The browser still reads, and fontconfig
  may write, the user's cache directory. X11 authorization remains available,
  and other native environment overrides can still name external resources,
  including explicit font configurations. Private HOME isolates the default
  NSS/data/config locations; it is not a filesystem sandbox for the browser
  process or a guarantee that only cache files outside the profile are read.
- In the audited configuration, trusted HTTPS capture took 7.7 s against 7.4 s
  before, and 10.0 s with a private cache directory. This is not a general startup
  performance guarantee.
- Persistent sessions (P2.2) must keep a profile's home and NSS database inside
  Broxser's data directory and decide their own key handling: `basic` means a
  fixed key, adequate only for a 0700 profile deleted at exit.

## Validation

Unit: `environment_keeps_cache_and_native_display_authorization` (the variables set and
removed, the certificate note), `browser_runs_in_a_private_home` (the fake
browser's environment and the home's mode) and the launch argument test
(`--password-store=basic` in both modes). Helium:
`live_certificate_trust_is_the_browsers_own_not_the_users` (an `openssl
s_server` with a self-signed certificate: every device reports
`net::ERR_CERT_AUTHORITY_INVALID` with the note, NSS created `cert9.db` inside
the profile's home, and the profile is gone after the session). The traced runs
before and after the change, with the user database, the policy CA and the
keyring connects, are in `docs/validation.md` (P2.1).
Review regressions exercise explicit/implicit/non-UTF8 display authorization,
cache fallback and the actual child environment. A separate capture subprocess
inherits disposable legacy and XDG NSS stores containing a test CA and an ambient
TLS key-log path; it must reject that CA, leave both caller stores untouched,
write no TLS key log, and remove all private profiles.
