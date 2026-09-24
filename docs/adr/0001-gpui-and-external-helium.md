# ADR 0001 GPUI shell and external Helium runtime

Status: accepted for the foundation spike, 2026-09-24.

The user requires GPUI and Helium, with Linux first. GPUI is not a web renderer;
the reviewed Helium sources do not provide a public embedding SDK. Maintaining a
large Chromium UI fork before validating this gap would commit the team to major
security and merge work without proving the developer workflow.

Use GPUI for application chrome and a Rust-owned external Helium process over CDP.
The first end-to-end feature is static multi-viewport capture. Keep domain state
independent of both. Do not describe screenshots as an embedded browser or infer
Helium compatibility from a Chromium-only test.

Consequences: a small first implementation, testable process boundary and replaceable
runtime, but no free native web surface, input forwarding, accessibility or browser
chrome. Real interaction must pass a dedicated spike before commitment. If frame
transport cannot meet the documented requirements, compare an upstreamable Helium
embedding adapter with CEF/Content hosting and record a new ADR.

Evidence: [GPUI](https://gpui.rs/), [Helium](https://github.com/imputnet/helium),
[headless](https://developer.chrome.com/docs/automation-and-testing/headless),
[CDP Page](https://chromedevtools.github.io/devtools-protocol/tot/Page/).
