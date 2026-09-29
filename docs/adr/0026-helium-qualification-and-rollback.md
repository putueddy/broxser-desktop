# ADR 0026 Qualifying Helium releases by signature, suites and smoke; rehearsed rollback

Status: accepted for P3.1, 2026-09-28 (tooling and procedure); the decisions
listed under "Proposed" wait for the owner. Builds on ADR 0003 (engine update
lifecycle), ADR 0001 (external Helium) and ADR 0025 (release archive).

## Context

- `runtime/helium-linux-x86_64.json` pins Helium by version, URL and SHA-256;
  `scripts/fetch-helium.sh` refuses a download with another digest. Nothing
  recorded where the pinned digest came from or checked that the tarball is
  Helium's.
- Helium signs its binary tarballs (since 0.7.7.2) with one OpenPGP key,
  fingerprint `BE67 7C19 89D3 5EAB 2C5F 26C9 3516 01AD 01D6 378E` (ed25519,
  created 2025-10-11, expires 2028-10-10), published as `pubkey.asc` and in the
  README of `imputnet/helium-linux`; each release carries `<tarball>.asc`. On
  2026-09-28 the pinned 0.18.1.1 tarball verifies against it (signed
  2026-09-23 18:41:31 UTC) and its SHA-256 equals the pin.
- Qualifying another version meant editing the manifest, moving `.local/helium`
  aside and running the suites by hand, with no record. No rollback had been
  tried. The release before the pin is 0.17.2.1 (Chromium 153.0.8010.52).
- Release downloads are reachable from the environments used so far; the GitHub
  API is not reachable from all of them.

## Options

| Option | Assessment |
| --- | --- |
| Keep trusting a digest someone computed | What exists; no evidence the artifact is Helium's |
| GitHub's stored asset digests | Needs API access; states what GitHub stored, not who built it |
| Helium's detached signatures, checked with `gpgv` against a pinned key | Upstream's own mechanism; `gpg` and `gpgv` ship with Debian and Ubuntu (apt uses `gpgv`) |
| Check the signature on every fetch | Makes GnuPG a prerequisite for users; the pinned digest already binds the reviewed artifact |
| Qualify candidates in CI automatically | Needs a trigger, a display runner and owners (ADR 0003); later |

## Decision

- `runtime/helium-signing-key.asc` is Helium's public key, byte-identical to
  `pubkey.asc` in `imputnet/helium-linux` at `0d03416`; `scripts/qualify-helium.py`
  pins its fingerprint. The key file must hold exactly one primary key with that
  fingerprint, and a signature counts only when `gpgv` reports one `VALIDSIG`
  whose primary key is it. GnuPG runs in a temporary home; the user's own
  keyrings are never read or changed. A key rotation is a reviewed change of the
  file and the fingerprint together.
- `propose VERSION` downloads the release's tarball and signature, accepts them
  only as above, computes the SHA-256, unpacks the browser into
  `.local/helium-candidates/VERSION` (beside the pinned `.local/helium`), reads
  `helium --version` and writes a manifest to `artifacts/helium/`. A new pin's
  digest therefore always comes from a signature-verified download.
- `verify MANIFEST` checks a manifest's signature and digest without unpacking;
  CI runs it for the pinned manifest before fetching, so a pin whose tarball is
  not Helium-signed, or whose digest changed, fails CI.
- `run MANIFEST` refuses root (the sandbox stays enabled), prepares or reuses the
  verified candidate, requires the reported Helium and Chromium versions to equal
  the manifest's, then runs the live Helium suite (4 threads) and the X11 desktop
  smoke against it. It writes a JSON record with the logs to
  `artifacts/qualification/` (outside Git): manifest, verified signature, Broxser
  commit, host, each step's result and time, and a verdict of `qualified`, `not
  qualified` or `incomplete`. Without a display the smoke is skipped and the
  verdict is `incomplete`, never `qualified`. `BROXSER_ENGINE_TESTS` and
  `BROXSER_DESKTOP_BIN` may name prebuilt binaries, for example a release
  archive's desktop.
- Promotion is a reviewed commit that replaces the pinned manifest with the
  proposed one and summarizes the record in `docs/validation.md`; a pilot can
  point `BROXSER_HELIUM_BIN` at the candidate first. Rollback is the same: qualify
  the target against the Broxser commit that will run it, then commit its
  manifest. A record is evidence for that pair of Broxser commit and Helium
  version only.
- `fetch-helium.sh` still checks only the digest, so users need no GnuPG. When
  `.local/helium` holds another version it refuses as before and now names that
  version and the `mv` that keeps it, so switching back is a rename.

## Rehearsal (2026-09-28)

- The pinned 0.18.1.1 qualifies against this commit: live 48 of 48, smoke 15
  of 15.
- The previous release 0.17.2.1 does **not**: while a JavaScript alert is open,
  Chromium 153 leaves a `Page.stopScreencast` unanswered. Hiding, scrolling off
  and resizing a device each send one, and in
  `live_dialog_survives_hide_show_scroll_and_zoom` they stopped the whole live
  runtime at the 15-second command limit in all six runs; the test passes on
  0.18.1.1. ADR 0014 requires that a waiting page never stops the runtime. The
  smoke passed; it does not hide a device during a dialog.
- Switching the pin to 0.17.2.1 and back in a clone worked as described above:
  the switch took 10 s including the download, the way back 0.04 s.
- So the rollback target for the next update is the current pin, 0.18.1.1.
  Rolling the engine back alone to 0.17.2.1 would ship a runtime-stopping
  regression; going further back means pairing it with an older Broxser commit
  and qualifying that pair.

## Consequences

- Every new pin carries signature evidence, and CI rechecks the current one.
- Rolling back to an older Chromium reintroduces the vulnerabilities fixed
  since; ADR 0003's "do not roll back blindly" now has a procedure, not an
  exemption. Profiles are ephemeral today; once P2.2 keeps profiles (ADR 0021),
  a rollback must also not open a profile that a newer Helium wrote.
- Trust in the key rests on GitHub serving `imputnet/helium-linux` over HTTPS
  (here through the environment's TLS-inspecting proxy). `gpgv` checks neither
  expiry nor revocation; the key expires 2028-10-10, so expect a rotation before
  then.
- Nothing runs on a schedule: the cadence and owners of ADR 0003 are still
  unassigned.

## Proposed (needs the owner)

1. Confirm the key fingerprint through a second channel (for example
   helium.computer) and name who approves a key rotation.
2. Who runs qualification and when (ADR 0003: weekly review, critical updates
   within 72 hours), and whether a manually triggered CI workflow with a display
   runs `propose` and `run`.
3. Where records are kept beyond `docs/validation.md` (for example with the
   release artifacts of ADR 0025).
4. Who may pin an older Chromium, and for how long.

## Validation

`docs/validation.md` (P3.1 Helium qualification and rollback rehearsal): unit
tests of the tooling, the signature check of the pin, records for 0.18.1.1 and
0.17.2.1, reruns of the failing tests, and the pin switch in a clone.
