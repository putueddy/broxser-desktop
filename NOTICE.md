# Ownership and upstream components

Broxser is an internal prototype. Its own code is licensed to no one: all rights
are reserved. The repository being public on GitHub grants only what GitHub's
terms allow on GitHub itself, and Cargo packages are not publishable. Choosing a
license stays with the company that holds the rights; if it opens the code,
Apache-2.0 is recommended (ADR 0025, decision 1). Until then, release archives
go only to the company's pilot users.

- GPUI 0.2.2: Apache-2.0, as declared by the published crate. Its verified source
  and license are retained in `vendor/gpui-0.2.2`, with Broxser's patches
  described in `BROXSER-PATCH.md` (ADRs 0011, 0012 and 0025); each changed file
  says that Broxser changed it.
- Helium: its original code and patches use GPL-3.0; imported Chromium and other
  upstream components retain their respective licenses. The engine is obtained
  separately from the official release, is not vendored, and is not included in Git.
  `runtime/helium-signing-key.asc` is Helium's public release signing key, copied
  unchanged from `imputnet/helium-linux` (`pubkey.asc`) to verify its signatures.
- Rust transitive dependencies retain their own licenses. `scripts/sbom.py`
  writes an SPDX SBOM of the crates the Linux binaries link, a list by declared
  license (`THIRD-PARTY.md`) and their license and notice texts
  (`THIRD-PARTY-LICENSES.txt`), all three in the release archive (ADR 0025).
  The technical review of 2026-09-29 (ADR 0025, decision 1; not legal advice)
  found every shipped component redistributable in binary form on those
  terms, once GPUI no longer compiled KDE's blur protocol (LGPL-2.1-or-later),
  which it no longer does. `sbom.py --check` fails CI for a new dependency
  that offers only other licenses.
- Sizzy is a functional reference. Its DMG, code, artwork, branding and license
  mechanism are not part of this repository.

Before packaging or distribution, inventory every bundled component, preserve
notices, decide how to satisfy applicable source/distribution obligations, and
review the resulting package with the company's license owner. Process separation
by itself is not a legal conclusion about license obligations.

Sources: [GPUI](https://crates.io/crates/gpui/0.2.2),
[Helium licensing](https://github.com/imputnet/helium#license).
