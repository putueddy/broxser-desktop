# ADR 0025 Linux distribution: a reproducible unsigned archive with an SBOM first

Status: first part accepted for P3.1, 2026-09-27 (archive, SBOM, checksums);
the decisions listed under "Proposed" wait for the owner. Builds on ADR 0003
(engine update ownership), ADR 0001 (external Helium) and `NOTICE.md`.

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
  `bin/broxser`, `README.md`, `NOTICE.md`, `SECURITY.md`, the GPUI license,
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

## Proposed (needs the owner)

1. A distribution license for Broxser's own code (`NOTICE.md`), and the license
   owner's review of `THIRD-PARTY.md`, including the MPL-2.0 and the dual
   GPL/Apache crate.
2. A signing identity and method: for example a detached signature with a key
   the company holds, or keyless signing from CI; plus where public keys live.
3. The release channel: where archives are published (an internal artifact
   store or GitHub releases of a private repository) and a release workflow on
   tags that builds with `scripts/package.sh`.
4. The user-facing format on top of the archive (AppImage or distribution
   packages), a desktop entry and the Helium discovery it needs
   (`BROXSER_HELIUM_BIN` or `PATH` today).
5. Primary and backup owners for engine updates and releases (ADR 0003).
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
  `.sha256` goes unnoticed; this is the first gap the owner's decisions close.

## Validation

`docs/validation.md` (P3.1): two archive builds with the same SHA-256, the
archive verified and unpacked by the unprivileged user, its `SHA256SUMS`
checked, the packaged fetch script preparing Helium, the packaged CLI's
`doctor` and `validate`, and the full desktop smoke run against the packaged
desktop and browser.
