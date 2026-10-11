# ADR 0025 Linux distribution: a reproducible unsigned archive with an SBOM first

Status: first part accepted for P3.1, 2026-09-27 (archive, SBOM, checksums);
the open decisions were taken on 2026-09-29, on the owner's delegation (see
"Decisions, 2026-09-29"). The license texts and the fourth GPUI patch of
decision 1 and the release workflow of decisions 2 and 3 are implemented; the
installer of decision 4 is a separate change. Builds on ADR 0003 (engine update ownership), ADR 0001 (external
Helium) and `NOTICE.md`.

## Context

`GOALS.md` (P3.1) asks for packaging and signing, checksums, an SBOM and
notices, qualifiable Helium updates, a rehearsed rollback, and a primary and
backup owner. An audit of the repository on 2026-09-27 found:

- No release build, package, release workflow or signature exists. CI runs
  format, tests, strict Clippy and the live Helium suite on every push; nothing
  builds `--release`. Dependabot proposes Cargo and Actions updates weekly.
- Helium is pinned by `runtime/helium-linux-x86_64.json` (version, URL,
  SHA-256, review date) and `scripts/fetch-helium.sh` downloads and verifies it
  into `.local/helium`; it is never vendored or bundled. The pinned 0.18.1.1 is
  the newest `imputnet/helium-linux` tag on 2026-09-27.
- `NOTICE.md` says no distribution license is chosen for Broxser's own code and
  that `Cargo.lock` is an inventory, not an SBOM. ADR 0003 is a proposed policy
  whose owners are unassigned.
- The shipped binaries link 525 crates.io crates on x86_64 Linux (per-binary
  `cargo tree`); `cargo metadata` resolves 549, because it unifies features
  across the workspace and its tests. Declared licenses are MIT or Apache-2.0
  for most; others are Unicode-3.0, BSD, ISC, Zlib, CC0 and Unlicense options,
  one MPL-2.0 crate used unmodified (`option-ext`) and one offering
  Apache-2.0 or GPL-2.0-only (`self_cell`). GPUI 0.2.2 is vendored under
  Apache-2.0 with three patches.
- A release build of the desktop and CLI takes 2 m 49 s here; the binaries are
  23.6 MB and 2.2 MB with debug information stripped.

## Options

| Option | Assessment |
| --- | --- |
| Tar archive of the binaries and notices | Works on any glibc distribution; the base every other format wraps |
| AppImage, `.deb`/`.rpm`, Flatpak | Each needs a decision on target distributions, update channel and sandboxing (Flatpak's sandbox conflicts with launching Helium's own) |
| Bundle Helium in the package | GPL-3.0 source obligations and a 288 MB browser in every release; its security updates would need a Broxser release |
| Keep fetching Helium by its pinned checksum | Already built; engine updates stay independent (ADR 0003) |
| SBOM from `Cargo.lock` | Lists every platform's crates and no licenses |
| A new dependency such as `cargo-cyclonedx` | Another tool to pin and trust |
| SPDX from `cargo tree` and `cargo metadata` with Python's standard library | Exactly the shipped graph, no new dependency |
| Signing now | Needs an identity and key custody that only the owner can decide |

## Decision (first part)

- `scripts/package.sh` builds the release binaries and writes
  `broxser-<version>-linux-x86_64.tar.xz`: `bin/broxser-desktop`,
  `bin/broxser`, `README.md`, `NOTICE.md`, `SECURITY.md`,
  `THIRD-PARTY-LICENSES.txt` (the GPUI license alone until decision 1),
  `sbom.spdx.json`, `THIRD-PARTY.md`, the Helium manifest with
  `scripts/fetch-helium.sh`, `COMMIT`, and a `SHA256SUMS` of every file; next to
  it `<archive>.sha256`. File order, owners, modes and times come from the
  commit, so two builds of one commit's binaries give the same archive. It
  refuses to run with uncommitted tracked changes. Helium is not included.
- `scripts/sbom.py` writes an SPDX 2.3 document of the shipped graph: every
  crate with its version, declared license, crates.io download location,
  `pkg:cargo` purl and the SHA-256 pinned in `Cargo.lock`; the vendored GPUI;
  Broxser's crates without a license (none is chosen); Helium as a runtime
  dependency of the engine with its pinned URL and SHA-256, not contained.
  `THIRD-PARTY.md` lists the components by declared license for the license
  owner's review. `--check` validates the document and runs in `check.sh` and
  CI, so a dependency update that breaks it fails early.
- The archive is **unsigned**. Its integrity rests on the `.sha256` obtained
  through a trusted channel until signing is decided.

## Decisions, 2026-09-29

The owner delegated these decisions. The repository is public, which settles
some of them: GitHub releases would be public once published, and keyless
signing of a public repository reveals nothing that is not already public.

1. **Broxser's own code stays unlicensed: all rights reserved.** The repository
   being public grants no license beyond what GitHub's terms allow on GitHub
   itself. Choosing a license is an irrevocable grant for every version
   published under it, and `NOTICE.md` leaves it to the company that holds the
   rights, so it is not taken here. If the company opens the code, Apache-2.0 is
   the recommended license: it is GPUI's, it has a patent grant, and every
   license in the shipped graph allows it. Until then, archives go to the
   company's pilot users, not to the public (decision 3).
   The third-party review of 2026-09-29 (`docs/validation.md`) found that every
   shipped component allows redistribution in binary form on the terms below,
   after one change to GPUI. The archive did not meet them then: it listed
   licenses but carried only GPUI's text. No archive is given to anyone before
   it does, so that change came before the release workflow (implemented
   below).
   - Each archive carries every license and notice file of every shipped
     crate, including those of code a crate bundles (the Unicode data license
     in `regex-syntax`, the Wayland protocol authors' notice in
     `wayland-protocols`, fiat-crypto's license in `ring`), and, for the 32
     crates that ship none, the standard text of the license they declare with
     their authors. No shipped crate, and not GPUI, has an Apache-2.0 `NOTICE`
     file.
   - Where a crate offers a choice, Broxser takes the permissive one:
     `self_cell` under Apache-2.0, never GPL-2.0-only; `ring` is Apache-2.0 and
     ISC together.
   - `option-ext` 0.2.0 (MPL-2.0) is used unmodified. The archive says so and
     where its source form is available: crates.io, with the SHA-256 in the SBOM
     (MPL-2.0 section 3.2).
   - GPUI's patched files carry a notice at the top that Broxser changed them
     (Apache-2.0 section 4(b)), beside `BROXSER-PATCH.md`. Its X11 clipboard,
     adapted from arboard, keeps that project's notice.
   - `wayland-protocols-plasma` declares MIT, but GPUI compiles one protocol
     from it, KDE's blur, whose description is LGPL-2.1-or-later. GPUI uses it
     only to unset the blur of windows that do not ask for one, and Broxser's
     window never does (it keeps GPUI's opaque default). Rather than rely on a
     reading of LGPL-2.1 for generated protocol code, the vendored GPUI drops
     that protocol and the crate: a fourth patch, with no change for Broxser.
   - FreeType, bundled inside `freetype-sys`, is not shipped: the build links
     the system library, and the desktop calls none of it.
   This is a technical review, not legal advice.

   Implemented on 2026-09-29. `sbom.py --licenses` writes
   `THIRD-PARTY-LICENSES.txt` into the archive: the license and notice files of
   every shipped third-party component, grouped by identical text; the SPDX
   standard text (`scripts/licenses`) for each crate that ships none, the MIT
   one naming the crate's authors; and notes for `self_cell`'s election,
   `option-ext`'s source form and GPUI's changes. `sbom.py --check`, in
   `check.sh` and CI, fails when a shipped crate offers no license but ones
   outside the permissive set and MPL-2.0, when MPL-2.0 code is not unmodified
   from crates.io, or when a crate without license files declares a license
   `scripts/licenses` has no text for. The vendored GPUI no longer uses KDE's
   blur protocol, and its changed files say so; the shipped graph drops to 524
   crates.io crates, 31 of them without license files.
2. **Signing is keyless build provenance.** The release workflow attests each
   archive with GitHub artifact attestations, signed through Sigstore's public
   instance with the workflow's short-lived identity, so there is no long-lived
   private key to guard. With the GitHub CLI, anyone verifies an archive with
   `gh attestation verify <archive> --repo putueddy/broxser-desktop`; the
   attestation names this repository, the release workflow, the tag and the
   commit. The attestation is in Sigstore's public transparency log from the
   moment the draft is built. The `.sha256` remains a quick integrity check, not
   an authenticity check.
3. **Releases are drafts on this repository's GitHub releases.** A workflow on
   tags `v<workspace version>` builds with `scripts/package.sh` on Ubuntu 24.04
   and attaches the archive, its `.sha256`, the SBOM and the attestation to a
   **draft** release. A draft is visible only to people with write access to the
   repository; the owner downloads it for the pilot, or publishes it once
   decision 1 allows public distribution. Nothing is published automatically.
   The archive is never uploaded as a workflow artifact, which any signed-in
   GitHub user could download from this public repository.

   Decisions 2 and 3 implemented on 2026-09-29 in
   `.github/workflows/release.yml`. On a tag `v*` the release job refuses a
   commit that `main` does not contain, then runs `scripts/release-assets.sh`:
   it refuses a tag other than `v<workspace version>`, builds the archive twice
   with `scripts/package.sh` and requires one SHA-256, checks the `.sha256`, the
   `SHA256SUMS` of the unpacked files (and that it lists exactly those files)
   and `COMMIT`, and writes the SBOM and the release notes beside the archive.
   `actions/attest` (v4.2.2, pinned by commit) then attests the archive with
   SLSA build provenance, and `gh release create --draft --verify-tag`
   attaches the archive, its `.sha256`, the SBOM and the attestation's Sigstore
   bundle. Only that job may write contents, request an OIDC token and store
   attestations. Pull requests that change the release path run a dry-run job
   with read-only permissions: the same build and checks, no attestation and no
   upload. The workflow does not check that CI passed; the owner tags a commit
   of `main` whose CI is green. The release notes give the verification
   command with the signer workflow, the tag and the commit:
   `gh attestation verify <archive> --repo putueddy/broxser-desktop
   --signer-workflow putueddy/broxser-desktop/.github/workflows/release.yml
   --source-ref refs/tags/v<version> --source-digest <commit>
   --deny-self-hosted-runners`.
4. **The user-facing format is the archive with an installer.** The archive
   gains `install.sh`, which installs it for the current user without root:
   the files under `~/.local/opt/broxser/<version>`, `broxser` and
   `broxser-desktop` linked from `~/.local/bin`, a desktop entry in
   `~/.local/share/applications` without Broxser artwork, and the pinned Helium
   prepared beside the binaries by the bundled `fetch-helium.sh`. Broxser then
   finds that Helium itself: after `BROXSER_HELIUM_BIN` and before `PATH`. Not
   now: an AppImage (Helium would still be a separate download), Flatpak (its
   sandbox conflicts with Helium's own) and distribution packages (they need
   target distributions and a package repository; revisit for fleet
   deployment).
5. **Owners.** The primary owner of engine updates and releases is the
   repository owner (@putueddy), on 2026-09-29 the repository's only
   collaborator and so the one person who can merge pins and publish releases.
   A backup must be a person the company names, which this repository cannot
   do. The weekly qualification (ADR 0026) and the release workflow run on
   GitHub rather than on the owner's machine, so a backup needs only write
   access to take over both.
6. Done in [ADR 0026](0026-helium-qualification-and-rollback.md) (2026-09-28):
   `scripts/qualify-helium.py` qualifies a signature-verified candidate beside
   the pinned engine with the live suite and the smoke and writes a record;
   rollback is the same run for the previous manifest, rehearsed once.

## Consequences

- A pilot can install Broxser from one archive whose contents, dependencies
  and licenses are listed and checksummed; Helium stays a separate verified
  download, so a browser security update needs no Broxser release.
- The binaries themselves are reproducible only with the same toolchain,
  dependencies and build path: Rust embeds source paths.
- The archive depends on the system's X11/Wayland, Vulkan and font libraries;
  they are not bundled.
- Without signatures an attacker who can replace both the archive and its
  `.sha256` goes unnoticed; the provenance attestation of decision 2 closes this
  gap for drafts of the release workflow. Archives built by hand with
  `scripts/package.sh` stay unsigned.

## Validation

`docs/validation.md` (P3.1): two archive builds with the same SHA-256, the
archive verified and unpacked by the unprivileged user, its `SHA256SUMS`
checked, the packaged fetch script preparing Helium, the packaged CLI's
`doctor` and `validate`, and the full desktop smoke run against the packaged
desktop and browser.
