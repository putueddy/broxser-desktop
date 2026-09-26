# GPUI 0.2.2: native IME integration and atlas fix

This directory contains the published `gpui` 0.2.2 crate, unpacked from
`https://static.crates.io/crates/gpui/gpui-0.2.2.crate`.

Archive SHA-256 (the original Cargo.lock checksum):
`979b45cfa6ec723b6f42330915a1b3769b930d02b2d505f9697f8ca602bee707`.

The archive was verified before extraction. Its Apache-2.0 license, resources,
upstream notices, and `.cargo_vcs_info.json` are retained. The published archive,
rather than the VCS metadata's dirty tree, is the reproducible upstream source.

Broxser changes `src/platform/linux/wayland/client.rs`: every Wayland
`CommitString`, including a single ASCII byte, goes to the native text input
handler as `ImeInput::InsertText`. Upstream turns a single byte into a synthetic
key-down without a key-up to support Zed's modal key bindings. That event has no
origin flag, so Broxser cannot distinguish it from a physical key, safely suppress
held keys across devices, and also accept repeated IME commits.

`src/platform/linux/x11/xim_handler.rs` sends `SetIcFocus` after an XIM input
context is created. Creation alone does not focus it. With Fcitx5 on an isolated
X server, the unpatched handler leaves no active context and the page receives
literal Pinyin keys rather than native preedit and commit callbacks.

`src/platform/blade/blade_atlas.rs` forgets the pending initialization and uploads
of a texture that `remove` destroys. A texture created since the last flush, for
example by the draw that key dispatch runs without presenting, can lose its last
tile before the next frame flushes the atlas. Upstream then unwraps the empty
slot and panics in `BladeRenderer::draw`, or initializes and uploads into a later
texture that reuses the index. Broxser replaces every device frame with a new
image, so typing while pages animate reached this within about a hundred key
presses in a release build (P1.4 in `docs/validation.md`).

No other upstream behavior is intentionally changed. `Cargo.toml` patches the
same exact version to this source; transitive versions remain locked. The vendor
directory is excluded from the Broxser workspace's formatting and lint targets.

Requalification: compare this directory with the checksum-verified archive,
repeat the real IME, normal-keyboard and typing-during-animation checks in
`docs/validation.md`, and drop the override once an upstream release supplies a
suitable text-commit path and atlas fix. See ADR 0011 for the canvas contract and
remaining platform limits, and ADR 0012 for the atlas fix.
