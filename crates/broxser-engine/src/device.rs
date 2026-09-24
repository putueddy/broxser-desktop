//! Device target setup shared by one-shot capture and live sessions.

use crate::cdp::required_str;
use anyhow::Result;
use broxser_core::Device;
use serde_json::{Value, json};

/// Blocking CDP command execution. Implementors pump and observe events while
/// they wait, so setup code does not need to know about their event handling.
pub(crate) trait Commands {
    fn command(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value>;
}

/// Creates the page target for `device` inside `context`, attaches a flat CDP
/// session and applies viewport, scale and touch emulation. Returns the target
/// ID (also the main frame ID) and the session ID.
pub(crate) fn setup_target(
    commands: &mut impl Commands,
    context: &str,
    device: &Device,
) -> Result<(String, String)> {
    let response = commands.command(
        "Target.createTarget",
        json!({"url": "about:blank", "browserContextId": context}),
        None,
    )?;
    let target = required_str(&response, "targetId")?.to_owned();
    let response = commands.command(
        "Target.attachToTarget",
        json!({"targetId": target, "flatten": true}),
        None,
    )?;
    let session = required_str(&response, "sessionId")?.to_owned();
    commands.command("Page.enable", json!({}), Some(&session))?;
    commands.command(
        "Page.setLifecycleEventsEnabled",
        json!({"enabled": true}),
        Some(&session),
    )?;
    commands.command(
        "Emulation.setDeviceMetricsOverride",
        json!({
            "width": device.width,
            "height": device.height,
            "deviceScaleFactor": device.device_scale_factor,
            "mobile": device.mobile,
        }),
        Some(&session),
    )?;
    let touch = if device.touch {
        json!({"enabled": true, "maxTouchPoints": 1})
    } else {
        json!({"enabled": false})
    };
    commands.command("Emulation.setTouchEmulationEnabled", touch, Some(&session))?;
    Ok((target, session))
}

/// Returns the extension ID when `info` (a CDP TargetInfo) is an extension
/// background context inside one of `contexts`.
pub(crate) fn extension_in_context(
    info: &Value,
    contexts: &std::collections::HashSet<String>,
) -> Option<String> {
    let kind = info.get("type")?.as_str()?;
    if !matches!(kind, "background_page" | "service_worker" | "shared_worker") {
        return None;
    }
    if !contexts.contains(info.get("browserContextId")?.as_str()?) {
        return None;
    }
    let host = info
        .get("url")?
        .as_str()?
        .strip_prefix("chrome-extension://")?
        .split('/')
        .next()?;
    // Extension IDs are 32 characters a-p; keep only that alphabet from untrusted input.
    let id: String = host
        .chars()
        .filter(|c| c.is_ascii_lowercase())
        .take(32)
        .collect();
    (!id.is_empty()).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn extension_targets_are_only_reported_inside_owned_contexts() {
        let ours: HashSet<String> = ["CTX1".to_owned()].into();
        let info = |kind: &str, context: &str, url: &str| json!({"type": kind, "browserContextId": context, "url": url});
        assert_eq!(
            extension_in_context(
                &info(
                    "background_page",
                    "CTX1",
                    "chrome-extension://blockjmkbacgjkknlgpkjjiijinjdanf/background.html"
                ),
                &ours
            )
            .as_deref(),
            Some("blockjmkbacgjkknlgpkjjiijinjdanf")
        );
        assert!(
            extension_in_context(
                &info(
                    "background_page",
                    "DEFAULT",
                    "chrome-extension://abc/background.html"
                ),
                &ours
            )
            .is_none()
        );
        assert!(
            extension_in_context(&info("page", "CTX1", "chrome-extension://abc/x"), &ours)
                .is_none()
        );
        assert!(
            extension_in_context(
                &info("service_worker", "CTX1", "https://example.test/sw.js"),
                &ours
            )
            .is_none()
        );
        assert_eq!(
            extension_in_context(
                &info("service_worker", "CTX1", "chrome-extension://ab\"<c/sw.js"),
                &ours
            )
            .as_deref(),
            Some("abc")
        );
    }
}
