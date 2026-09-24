//! One-shot capture: a fresh browser, one BrowserContext per session, one target
//! per device and a PNG per device; afterwards the browser and profile are removed.

use crate::Limits;
use crate::browser::{BrowserOptions, BrowserProcess, ProcessIdentity};
use crate::cdp::{Cdp, Event, parse_response, required_str};
use crate::device::{Commands, extension_in_context, setup_target};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use broxser_core::Workspace;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_LOAD_EVENTS: usize = 512;
const MAX_TRACKED_NAVIGATIONS: usize = 32;
const MAX_EXTENSION_IDS: usize = 16;
/// How long to keep reading after a failed navigation for the event that explains it.
const EXPLAIN_GRACE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureReport {
    pub browser_product: String,
    pub protocol_version: String,
    pub frames: Vec<CaptureFrame>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureFrame {
    pub device_id: String,
    pub session_id: String,
    pub path: PathBuf,
    /// Actual PNG pixel dimensions, including device scale factor.
    pub width: u32,
    pub height: u32,
}

/// Capture each configured device as PNG, one device after another. The URL
/// must be accepted by core validation. A failed navigation or incomplete load
/// returns an error and is never retried: navigation can have side effects.
pub fn capture_workspace(
    workspace: &Workspace,
    options: &BrowserOptions,
    output_dir: &Path,
) -> Result<CaptureReport> {
    run(workspace, options, output_dir, Plan::default()).result
}

/// How a capture runs. Production always uses [`Plan::default`]; tests vary it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Plan {
    pub order: Order,
    pub limits: Limits,
    /// Seeds the private profile so Helium's bundled blocker stays out of session
    /// contexts, and fails if any extension page runs there (ADR 0004). Only the
    /// reproducer's baseline mode turns this off.
    pub extension_guard: bool,
}

impl Default for Plan {
    fn default() -> Self {
        Self {
            order: Order::Sequential,
            limits: Limits::default(),
            extension_guard: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Order {
    Sequential,
    /// Starts every navigation before waiting for any; used by the reproducer.
    #[cfg_attr(not(test), expect(dead_code))]
    Parallel,
}

pub(crate) struct Outcome {
    pub result: Result<CaptureReport>,
    /// Read by the reproducer and lifecycle tests; callers need only the result.
    #[cfg_attr(not(test), expect(dead_code))]
    pub diagnostics: Diagnostics,
}

/// Evidence about a run without URLs, cookies or page content.
#[derive(Debug, Default)]
pub(crate) struct Diagnostics {
    pub navigations: Vec<NavigationRecord>,
    /// Extensions seen running inside Broxser browser contexts.
    pub extension_targets: BTreeSet<String>,
    /// (device ID, browser context ID) in workspace order.
    pub contexts: Vec<(String, String)>,
    /// Browser processes observed just before cleanup.
    pub processes: Vec<ProcessIdentity>,
}

#[derive(Debug, Clone)]
#[cfg_attr(not(test), expect(dead_code))]
pub(crate) struct NavigationRecord {
    pub device: String,
    pub loader: Option<String>,
    pub error: Option<String>,
    pub elapsed: Duration,
    /// Main-frame navigations on this target that Broxser did not request.
    pub foreign: Vec<StartedNavigation>,
}

#[derive(Debug, Clone)]
pub(crate) struct StartedNavigation {
    pub loader: String,
    /// CDP `Page.frameStartedNavigating.navigationType`, such as `reload`.
    pub kind: String,
    /// `Page.frameRequestedNavigation.reason` when the page itself asked for it.
    pub page_reason: Option<String>,
}

pub(crate) fn run(
    workspace: &Workspace,
    options: &BrowserOptions,
    output_dir: &Path,
    plan: Plan,
) -> Outcome {
    let mut diagnostics = Diagnostics::default();
    let result = run_with(workspace, options, output_dir, plan, &mut diagnostics);
    Outcome {
        result,
        diagnostics,
    }
}

fn run_with(
    workspace: &Workspace,
    options: &BrowserOptions,
    output_dir: &Path,
    plan: Plan,
    diagnostics: &mut Diagnostics,
) -> Result<CaptureReport> {
    workspace
        .validate()
        .map_err(|error| anyhow!("invalid workspace: {error}"))?;
    fs::create_dir_all(output_dir).with_context(|| format!("create {}", output_dir.display()))?;
    let mut browser = BrowserProcess::start(options, plan.extension_guard)?;
    let captured = match browser.connect(&plan.limits) {
        Ok(cdp) => Capture {
            cdp,
            workspace,
            limits: plan.limits,
            extension_guard: plan.extension_guard,
            diagnostics: &mut *diagnostics,
            contexts: HashSet::new(),
            targets: Vec::new(),
            loaded: HashSet::new(),
        }
        .run(output_dir, plan.order),
        Err(error) => Err(error),
    };
    diagnostics.processes = browser.processes();
    let cleanup = browser.shutdown();
    match (captured, cleanup) {
        (Ok(report), Ok(())) => Ok(report),
        (Ok(_), Err(cleanup)) => {
            Err(cleanup.context("capture finished but browser cleanup failed"))
        }
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("browser cleanup also failed: {cleanup:#}")))
        }
    }
}

struct Capture<'a> {
    cdp: Cdp,
    workspace: &'a Workspace,
    limits: Limits,
    extension_guard: bool,
    diagnostics: &'a mut Diagnostics,
    contexts: HashSet<String>,
    targets: Vec<Target>,
    loaded: HashSet<(String, String)>,
}

struct Target {
    /// Also the main frame ID.
    target_id: String,
    session: String,
    navigation: Option<(u64, Instant)>,
    /// Main-frame navigations started since Broxser issued its own.
    started: Vec<StartedNavigation>,
    pending_reason: Option<String>,
    gone: bool,
}

impl Commands for Capture<'_> {
    fn command(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        let id = self.cdp.send(method, params, session)?;
        self.wait(id, method, Instant::now() + self.limits.command)
    }
}

impl Capture<'_> {
    fn run(mut self, output_dir: &Path, order: Order) -> Result<CaptureReport> {
        let workspace = self.workspace;
        let version = self.command("Browser.getVersion", json!({}), None)?;
        let browser_product = required_str(&version, "product")?.to_owned();
        let protocol_version = required_str(&version, "protocolVersion")?.to_owned();
        // Discovery reports extension pages that start inside Broxser contexts.
        self.command("Target.setDiscoverTargets", json!({"discover": true}), None)?;

        let mut contexts = HashMap::new();
        for session in &workspace.sessions {
            let response = self.command("Target.createBrowserContext", json!({}), None)?;
            let context = required_str(&response, "browserContextId")?.to_owned();
            self.contexts.insert(context.clone());
            contexts.insert(session.id.as_str(), context);
        }
        for device in &workspace.devices {
            let context = contexts
                .get(device.session.as_str())
                .ok_or_else(|| {
                    anyhow!(
                        "device {} references unknown session {}",
                        device.id,
                        device.session
                    )
                })?
                .clone();
            let (target_id, session) = setup_target(&mut self, &context, device)?;
            self.diagnostics.contexts.push((device.id.clone(), context));
            self.targets.push(Target {
                target_id,
                session,
                navigation: None,
                started: Vec::new(),
                pending_reason: None,
                gone: false,
            });
        }

        let count = self.targets.len();
        let mut frames = Vec::with_capacity(count);
        match order {
            Order::Sequential => {
                for index in 0..count {
                    self.start_navigation(index)?;
                    self.finish_navigation(index)?;
                    frames.push(self.screenshot(index, output_dir)?);
                }
            }
            Order::Parallel => {
                for index in 0..count {
                    self.start_navigation(index)?;
                }
                for index in 0..count {
                    self.finish_navigation(index)?;
                }
                for index in 0..count {
                    frames.push(self.screenshot(index, output_dir)?);
                }
            }
        }
        Ok(CaptureReport {
            browser_product,
            protocol_version,
            frames,
        })
    }

    fn start_navigation(&mut self, index: usize) -> Result<()> {
        let workspace = self.workspace;
        let target = &mut self.targets[index];
        target.started.clear();
        target.pending_reason = None;
        let id = self.cdp.send(
            "Page.navigate",
            json!({"url": workspace.url}),
            Some(&target.session),
        )?;
        target.navigation = Some((id, Instant::now()));
        Ok(())
    }

    fn finish_navigation(&mut self, index: usize) -> Result<()> {
        let workspace = self.workspace;
        let device = &workspace.devices[index];
        let (id, started_at) = self.targets[index]
            .navigation
            .take()
            .ok_or_else(|| anyhow!("navigation for {} was not started", device.id))?;
        let response = match self.wait(id, "Page.navigate", Instant::now() + self.limits.command) {
            Ok(response) => response,
            Err(error) => {
                self.record(index, None, Some(format!("{error:#}")), started_at);
                return Err(error);
            }
        };
        let loader = response
            .get("loaderId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(error) = response.get("errorText").and_then(Value::as_str) {
            self.settle(index, loader.as_deref());
            let explanation = foreign(&self.targets[index], loader.as_deref())
                .map(|navigation| format!(" (superseded by {})", describe(navigation)))
                .unwrap_or_default();
            self.record(index, loader, Some(error.to_owned()), started_at);
            bail!(
                "navigation failed for {}: {error}{explanation}; not retried",
                device.id
            );
        }
        let loader = loader.ok_or_else(|| anyhow!("CDP Page.navigate missing loaderId"))?;
        let loaded = self.wait_for_load(index, &loader);
        self.record(
            index,
            Some(loader),
            loaded.as_ref().err().map(|error| format!("{error:#}")),
            started_at,
        );
        loaded.with_context(|| format!("loading device {}", device.id))
    }

    fn wait_for_load(&mut self, index: usize, loader: &str) -> Result<()> {
        let deadline = Instant::now() + self.limits.load;
        let key = (self.targets[index].session.clone(), loader.to_owned());
        loop {
            if self.loaded.remove(&key) {
                return Ok(());
            }
            let target = &self.targets[index];
            if target.gone {
                bail!("page target crashed or was detached");
            }
            if let Some(navigation) = foreign(target, Some(loader)) {
                bail!(
                    "navigation superseded by {}; not retried",
                    describe(navigation)
                );
            }
            if !self.pump(deadline)? {
                bail!(
                    "load event did not arrive within {} seconds",
                    self.limits.load.as_secs_f32()
                );
            }
        }
    }

    /// After a failed navigation, reads briefly for the navigation that caused it.
    fn settle(&mut self, index: usize, loader: Option<&str>) {
        let deadline = Instant::now() + EXPLAIN_GRACE;
        while foreign(&self.targets[index], loader).is_none() {
            match self.cdp.read_until(deadline) {
                Ok(true) => {
                    while let Some(event) = self.cdp.pop_event() {
                        let _ = self.observe(event);
                    }
                }
                Ok(false) | Err(_) => break,
            }
        }
    }

    fn record(
        &mut self,
        index: usize,
        loader: Option<String>,
        error: Option<String>,
        started: Instant,
    ) {
        let target = &self.targets[index];
        let foreign = target
            .started
            .iter()
            .filter(|navigation| is_foreign(navigation, loader.as_deref()))
            .cloned()
            .collect();
        self.diagnostics.navigations.push(NavigationRecord {
            device: self.workspace.devices[index].id.clone(),
            loader,
            error,
            elapsed: started.elapsed(),
            foreign,
        });
    }

    fn screenshot(&mut self, index: usize, output_dir: &Path) -> Result<CaptureFrame> {
        let workspace = self.workspace;
        let device = &workspace.devices[index];
        let session = self.targets[index].session.clone();
        let screenshot = self.command(
            "Page.captureScreenshot",
            json!({"format": "png", "fromSurface": true, "captureBeyondViewport": false}),
            Some(&session),
        )?;
        let encoded = required_str(&screenshot, "data")?;
        let png = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .context("decode CDP screenshot")?;
        if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            bail!("browser returned a non-PNG screenshot for {}", device.id);
        }
        let (width, height) = png_dimensions(&png)?;
        let path = output_dir.join(format!("{}.png", safe_filename(&device.id)));
        fs::write(&path, png).with_context(|| format!("write screenshot {}", path.display()))?;
        Ok(CaptureFrame {
            device_id: device.id.clone(),
            session_id: device.session.clone(),
            path,
            width,
            height,
        })
    }

    fn wait(&mut self, id: u64, method: &str, deadline: Instant) -> Result<Value> {
        loop {
            if let Some(response) = self.cdp.take_response(id) {
                return parse_response(response, method);
            }
            if !self
                .pump(deadline)
                .with_context(|| format!("wait for CDP {method}"))?
            {
                bail!("CDP {method} response timed out");
            }
        }
    }

    /// Reads one message, observes buffered events and applies the extension gate.
    fn pump(&mut self, deadline: Instant) -> Result<bool> {
        let read = self.cdp.read_until(deadline)?;
        while let Some(event) = self.cdp.pop_event() {
            self.observe(event)?;
        }
        if self.extension_guard && !self.diagnostics.extension_targets.is_empty() {
            bail!(
                "browser runtime not qualified: extension {} runs inside a Broxser session \
                 context and can reload pages or replay navigations (ADR 0004)",
                self.diagnostics
                    .extension_targets
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        Ok(read)
    }

    fn observe(&mut self, event: Event) -> Result<()> {
        let params = &event.params;
        let text = |field: &str| params.get(field).and_then(Value::as_str);
        match event.method.as_str() {
            "Target.targetCreated" | "Target.targetInfoChanged" => {
                if let Some(extension) = params
                    .get("targetInfo")
                    .and_then(|info| extension_in_context(info, &self.contexts))
                    && self.diagnostics.extension_targets.len() < MAX_EXTENSION_IDS
                {
                    self.diagnostics.extension_targets.insert(extension);
                }
            }
            "Target.targetCrashed" | "Target.detachedFromTarget" => {
                for target in &mut self.targets {
                    if text("targetId") == Some(target.target_id.as_str()) {
                        target.gone = true;
                    }
                }
            }
            _ => {
                let Some(target) = self
                    .targets
                    .iter_mut()
                    .find(|target| event.session.as_deref() == Some(target.session.as_str()))
                else {
                    return Ok(());
                };
                if text("frameId") != Some(target.target_id.as_str()) {
                    return Ok(());
                }
                match event.method.as_str() {
                    "Page.lifecycleEvent" if text("name") == Some("load") => {
                        if let Some(loader) = text("loaderId") {
                            if self.loaded.len() >= MAX_LOAD_EVENTS {
                                bail!("CDP lifecycle event limit exceeded");
                            }
                            self.loaded
                                .insert((target.session.clone(), loader.to_owned()));
                        }
                    }
                    "Page.frameRequestedNavigation" => {
                        target.pending_reason =
                            text("reason").map(|reason| reason.chars().take(64).collect());
                    }
                    "Page.frameStartedNavigating" => {
                        if let (Some(loader), Some(kind)) =
                            (text("loaderId"), text("navigationType"))
                            && target.started.len() < MAX_TRACKED_NAVIGATIONS
                        {
                            target.started.push(StartedNavigation {
                                loader: loader.chars().take(64).collect(),
                                kind: kind.chars().take(64).collect(),
                                page_reason: target.pending_reason.take(),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

fn is_foreign(navigation: &StartedNavigation, ours: Option<&str>) -> bool {
    ours.is_some_and(|ours| navigation.loader != ours)
        && !matches!(
            navigation.kind.as_str(),
            "sameDocument" | "historySameDocument"
        )
}

/// The latest cross-document navigation on the main frame that is not Broxser's.
fn foreign<'a>(target: &'a Target, ours: Option<&str>) -> Option<&'a StartedNavigation> {
    target
        .started
        .iter()
        .rev()
        .find(|navigation| is_foreign(navigation, ours))
}

fn describe(navigation: &StartedNavigation) -> String {
    match &navigation.page_reason {
        Some(reason) => format!("a page-initiated {} navigation ({reason})", navigation.kind),
        None => format!(
            "a browser-initiated {} navigation that Broxser did not request",
            navigation.kind
        ),
    }
}

fn safe_filename(id: &str) -> String {
    let mut name = String::with_capacity(id.len());
    for byte in id.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' => name.push(byte as char),
            _ => name.push('_'),
        }
    }
    if name.is_empty() || name == "." || name == ".." {
        "device".to_owned()
    } else {
        name
    }
}

fn png_dimensions(png: &[u8]) -> Result<(u32, u32)> {
    if png.len() < 24 || &png[12..16] != b"IHDR" {
        bail!("screenshot PNG is missing IHDR");
    }
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    if width == 0 || height == 0 {
        bail!("screenshot PNG has zero dimensions");
    }
    Ok((width, height))
}

#[cfg(test)]
mod tests;
