# Ownership and upstream components

Broxser is an internal prototype. No public redistribution license for the new
Broxser code has been selected; Cargo packages are not publishable. The company
should choose its intended distribution policy before a public release.

- GPUI 0.2.2: Apache-2.0, as declared by the published crate. Its verified source
  and license are retained in `vendor/gpui-0.2.2`, with the native IME commit
  patch described in `BROXSER-PATCH.md` and ADR 0011.
- Helium: its original code and patches use GPL-3.0; imported Chromium and other
  upstream components retain their respective licenses. The engine is obtained
  separately from the official release, is not vendored, and is not included in Git.
- Rust transitive dependencies retain their own licenses. Cargo.lock is an
  inventory of versions, not a completed license audit or SBOM.
- Sizzy is a functional reference. Its DMG, code, artwork, branding and license
  mechanism are not part of this repository.

Before packaging or distribution, inventory every bundled component, preserve
notices, decide how to satisfy applicable source/distribution obligations, and
review the resulting package with the company's license owner. Process separation
by itself is not a legal conclusion about license obligations.

Sources: [GPUI](https://crates.io/crates/gpui/0.2.2),
[Helium licensing](https://github.com/imputnet/helium#license).
