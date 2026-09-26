//! Live workspace runtime: one owned browser for an open workspace, CDP
//! screencast frames per device, and input and navigation commands from the UI.
//!
//! This is frame streaming into the native UI, not an embedded browser surface.
//! A worker thread owns every blocking call. The UI exchanges only commands, the
//! latest frame per device and a status snapshot, so it never waits on the browser.

use crate::Limits;
use crate::browser::{BrowserOptions, BrowserProcess};
use crate::cdp::{
    Cancellation, Cancelled, Cdp, Event, error_message, parse_response, required_str,
};
use crate::device::{Commands, ExtensionObservations, extension_in_context, setup_target};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use broxser_core::{MAX_DEVICES, SyncAction, SyncEvent, SyncRouter, Workspace, validate_url};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const COMMAND_QUEUE: usize = 512;
const COMMANDS_PER_TURN: usize = 64;
/// Socket read timeout of the live loop; bounds the delay before UI commands run.
const LIVE_POLL: Duration = Duration::from_millis(8);
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
/// Input events, including the coalesced move and wheel in flight, that one
/// page may leave unanswered before Broxser reports it as not responding.
const MAX_UNANSWERED_INPUT: usize = 32;
/// One navigation plus the unanswered input of every device, so one stuck
/// device can never exhaust the commands of the others.
const MAX_PENDING: usize = MAX_DEVICES * (MAX_UNANSWERED_INPUT + 1);
/// Device status while its page leaves input unanswered.
pub(crate) const NOT_RESPONDING: &str =
    "The page is not responding to input. New input is dropped, not sent later.";
const MAX_LINK_INTENT_BYTES: usize = 8192;
const LINK_INTENT_WINDOW: Duration = Duration::from_secs(1);
const LINK_WORLD: &str = "broxser_link_observer";
const LINK_BINDING: &str = "__broxserTrustedLink";
const LINK_OBSERVER: &str = r#"(() => {
  let next = 0;
  let pending = null;
  const report = event => {
    pending = null;
    if (!event.isTrusted || event.defaultPrevented || event.button !== 0 ||
        event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
    const target = event.target;
    const anchor = target instanceof Element && target.closest('a[href],area[href]');
    if (!anchor || anchor.hasAttribute('download') ||
        (anchor.target && anchor.target.toLowerCase() !== '_self')) return;
    const nestedControl = target.closest('input,textarea,select,button,option,label,[role="button"]');
    if (target.isContentEditable || anchor.isContentEditable ||
        (nestedControl && anchor.contains(nestedControl))) return;
    const href = anchor.href;
    if (href.length > 8192 || !/^https?:\/\//i.test(href)) return;
    next = (next + 1) >>> 0;
    pending = {event, href, id: next};
    __broxserTrustedLink(JSON.stringify({phase: 'C', id: next, url: href}));
  };
  addEventListener('pointerdown', event => { if (event.isTrusted) pending = null; }, true);
  addEventListener('keydown', event => { if (event.isTrusted) pending = null; }, true);
  addEventListener('click', report, true);
  addEventListener('beforeunload', event => {
    if (event.isTrusted && pending && !pending.event.defaultPrevented)
      __broxserTrustedLink(JSON.stringify({phase: 'Y', id: pending.id, url: pending.href}));
    pending = null;
  }, true);
})();"#;
const JPEG_QUALITY: u32 = 80;
/// Largest frame edge requested before the UI reports its display size.
const DEFAULT_FRAME_EDGE: u32 = 2048;

/// UI-owned handle to a running live workspace. Dropping it stops the browser and
/// waits for cleanup, so drop it off the UI thread.
pub struct LiveSession {
    commands: SyncSender<Command>,
    shared: Arc<Shared>,
    cancel: Cancellation,
    worker: Option<JoinHandle<()>>,
}

impl LiveSession {
    /// Validates the workspace and starts the browser on a worker thread. The
    /// session opens the workspace URL once on every device. `notify` runs on the
    /// worker after a new frame or status change; keep it cheap and non-blocking.
    pub fn start(
        workspace: Workspace,
        options: BrowserOptions,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self> {
        Self::start_with(workspace, options, Limits::default(), notify)
    }

    /// [`LiveSession::start`] with other deadlines; tests shorten them.
    pub(crate) fn start_with(
        workspace: Workspace,
        options: BrowserOptions,
        limits: Limits,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self> {
        workspace
            .validate()
            .map_err(|error| anyhow!("invalid workspace: {error}"))?;
        let (commands, receiver) = sync_channel(COMMAND_QUEUE);
        let shared = Arc::new(Shared {
            frames: Mutex::new(vec![None; workspace.devices.len()]),
            status: Mutex::new(Status {
                devices: vec![DeviceStatus::default(); workspace.devices.len()],
                ..Status::default()
            }),
            notify: Box::new(notify),
        });
        let cancel = options.cancel.clone();
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("broxser-live".into())
            .spawn(move || run_worker(&workspace, &options, limits, &receiver, &worker_shared))
            .context("start live runtime thread")?;
        Ok(Self {
            commands,
            shared,
            cancel,
            worker: Some(worker),
        })
    }

    /// Queues a command. Returns `false` if the runtime stopped or is overloaded;
    /// the command is then dropped, never replayed later.
    pub fn send(&self, command: Command) -> bool {
        self.commands.try_send(command).is_ok()
    }

    /// Takes the newest frame for `device` that the UI has not taken yet.
    pub fn take_frame(&self, device: usize) -> Option<Frame> {
        lock(&self.shared.frames).get_mut(device)?.take()
    }

    pub fn status(&self) -> Status {
        lock(&self.shared.status).clone()
    }

    /// True once the worker has stopped the browser and removed its profile.
    pub fn is_finished(&self) -> bool {
        self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Latest encoded frame for one device. Frames are CDP screencast JPEGs.
#[derive(Clone, Debug)]
pub struct Frame {
    pub jpeg: Vec<u8>,
    /// Per-device counter. Gaps mean newer frames replaced unread ones.
    pub sequence: u64,
    /// CSS viewport size that the image shows.
    pub css_width: f64,
    pub css_height: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub runtime: RuntimeState,
    pub devices: Vec<DeviceStatus>,
    pub sync: SyncSettings,
    /// Latest protocol error from a forwarded input or stream command.
    pub protocol_error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum RuntimeState {
    #[default]
    Starting,
    Running {
        product: String,
        protocol: String,
    },
    /// The browser was stopped and its profile removed. `None` means a requested close.
    Stopped {
        error: Option<String>,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeviceStatus {
    /// Committed main-frame URL; shown to the user, never logged by Broxser.
    pub url: String,
    pub loading: bool,
    pub error: Option<String>,
    pub streaming: bool,
    pub frames: u64,
    /// Frames replaced before the UI took them. Each was still acknowledged.
    pub dropped_frames: u64,
    /// Windows the page opened; they are not displayed yet.
    pub popups: u32,
}

/// Opt-in synchronization inside one session. Pointer and key synchronization
/// stay off; typing, form submission and clicks are never broadcast.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncSettings {
    /// Link navigations that follow a forwarded click or key.
    pub navigation: bool,
    /// Wheel scrolling, mirrored as the same delta at the destination's center.
    pub scroll: bool,
}

#[derive(Clone, Debug)]
pub enum Command {
    /// Explicit URL bar action: navigates every device once.
    NavigateAll {
        url: String,
    },
    /// Explicit reload of one device.
    Reload {
        device: usize,
    },
    Pointer {
        device: usize,
        event: PointerEvent,
    },
    /// Wheel at a CSS point; positive `delta_y` scrolls down, in CSS pixels.
    Wheel {
        device: usize,
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
    },
    Key {
        device: usize,
        key: KeyInput,
    },
    /// Explicit paste: inserts `text`, as [`paste_text`] returns it, into the
    /// focused element of the device, like an input method commit. The page
    /// gets no `paste` event.
    InsertText {
        device: usize,
        text: String,
    },
    /// Hidden devices stop their screencast and keep no frame.
    SetVisible {
        device: usize,
        visible: bool,
    },
    /// Largest frame worth encoding, in physical pixels of the display.
    SetFrameLimit {
        device: usize,
        width: u32,
        height: u32,
    },
    SetSync(SyncSettings),
    /// Crashes a renderer so tests can check crash reporting and recovery.
    #[cfg(test)]
    CrashForTest {
        device: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Move,
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton {
    None,
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub alt: bool,
    pub control: bool,
    pub meta: bool,
    pub shift: bool,
}

impl Modifiers {
    fn cdp(self) -> u8 {
        u8::from(self.alt)
            | u8::from(self.control) << 1
            | u8::from(self.meta) << 2
            | u8::from(self.shift) << 3
    }
}

/// A pointer event at CSS coordinates of the device viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerEvent {
    pub kind: PointerKind,
    pub x: f64,
    pub y: f64,
    pub button: PointerButton,
    /// Buttons held after this event: 1 left, 2 right, 4 middle.
    pub buttons: u8,
    pub click_count: u32,
    pub modifiers: Modifiers,
}

/// A keyboard event for the selected device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInput {
    pub down: bool,
    /// DOM `KeyboardEvent.key`.
    pub key: String,
    /// DOM `KeyboardEvent.code`, empty when unknown.
    pub code: String,
    /// Text inserted by a key press without Ctrl, Alt or Meta.
    pub text: Option<String>,
    pub key_code: u32,
    pub modifiers: Modifiers,
}

impl KeyInput {
    /// Maps a platform key name (as GPUI reports it, such as `enter`, `left` or
    /// `a`) and the character it types. Returns `None` for keys not forwarded.
    pub fn from_key(
        name: &str,
        typed: Option<&str>,
        modifiers: Modifiers,
        down: bool,
    ) -> Option<Self> {
        let named = |key: &str, code: &str, key_code: u32, text: Option<&str>| {
            (
                key.to_owned(),
                code.to_owned(),
                key_code,
                text.map(str::to_owned),
            )
        };
        if let Some(number) = function_key(name) {
            let key = format!("F{number}");
            return Some(Self {
                down,
                code: key.clone(),
                key,
                text: None,
                key_code: 111 + number,
                modifiers,
            });
        }
        let shortcut = modifiers.control || modifiers.alt || modifiers.meta;
        // The one character the key press types, if any.
        let typed = typed
            .filter(|typed| typed.chars().count() == 1 && !typed.chars().any(char::is_control));
        let mut chars = name.chars();
        let single = match (chars.next(), chars.next()) {
            (Some(base), None) => Some(base),
            _ => None,
        };
        let (key, code, key_code, text) = match name {
            "enter" => named("Enter", "Enter", 13, Some("\r")),
            "backspace" => named("Backspace", "Backspace", 8, None),
            "tab" => named("Tab", "Tab", 9, None),
            "escape" => named("Escape", "Escape", 27, None),
            "space" => named(" ", "Space", 32, Some(" ")),
            "left" => named("ArrowLeft", "ArrowLeft", 37, None),
            "up" => named("ArrowUp", "ArrowUp", 38, None),
            "right" => named("ArrowRight", "ArrowRight", 39, None),
            "down" => named("ArrowDown", "ArrowDown", 40, None),
            "delete" => named("Delete", "Delete", 46, None),
            "home" => named("Home", "Home", 36, None),
            "end" => named("End", "End", 35, None),
            "pageup" => named("PageUp", "PageUp", 33, None),
            "pagedown" => named("PageDown", "PageDown", 34, None),
            "insert" => named("Insert", "Insert", 45, None),
            // A dead key starts a composition and types nothing itself.
            _ if name.starts_with("dead_") => named("Dead", "", 0, None),
            _ => match (single, typed) {
                (Some(base), _) if base.is_control() => return None,
                // Without a character, GPUI's name is only a guess at the key's
                // position; without a shortcut modifier this is a dead key.
                (Some(_), None) if !shortcut => named("Dead", "", 0, None),
                (Some(base), typed) => {
                    let typed = typed.map_or_else(|| base.to_string(), str::to_owned);
                    let (code, key_code) = match base {
                        'a'..='z' => (
                            format!("Key{}", base.to_ascii_uppercase()),
                            u32::from(base.to_ascii_uppercase()),
                        ),
                        '0'..='9' => (format!("Digit{base}"), u32::from(base)),
                        _ => (String::new(), 0),
                    };
                    (typed.clone(), code, key_code, Some(typed))
                }
                // A composed character, named after its keysym (`eacute`); its
                // physical key is unknown.
                (None, Some(typed)) => (typed.to_owned(), String::new(), 0, Some(typed.to_owned())),
                (None, None) => return None,
            },
        };
        let text = text.filter(|_| !shortcut);
        Some(Self {
            down,
            key,
            code,
            text,
            key_code,
            modifiers,
        })
    }
}

/// Function keys F1 to F12, as GPUI names them (`f1`).
fn function_key(name: &str) -> Option<u32> {
    name.strip_prefix('f')?
        .parse()
        .ok()
        .filter(|number| (1..=12).contains(number))
}

/// Ctrl+V, Ctrl+Shift+V and Shift+Insert. Pages never receive them: Chromium
/// would paste its own clipboard, which every session of the browser shares.
/// A paste inserts the system clipboard's text instead (ADR 0010).
pub fn is_paste_key(key: &KeyInput) -> bool {
    let Modifiers {
        alt,
        control,
        meta,
        shift,
    } = key.modifiers;
    !alt && !meta
        && ((control && key.code == "KeyV") || (shift && !control && key.code == "Insert"))
}

/// Key presses that Helium 0.18.1.1 turns into browser commands, whether or
/// not the page handles them (ADR 0010): they close a device's tab or its
/// window with every device of the session in it, quit, crash the browser
/// (Ctrl+Shift+M), open tabs, browser pages or DevTools that Broxser neither
/// shows nor tracks, or reload and navigate outside Broxser's commands.
fn is_browser_key(key: &KeyInput) -> bool {
    let Modifiers {
        alt,
        control,
        meta,
        shift,
    } = key.modifiers;
    let code = key.code.as_str();
    if meta {
        return false;
    }
    match (control, alt, shift) {
        _ if code == "F5" => true,
        (false, false, _) => code == "F12",
        (true, false, false) => matches!(
            code,
            "KeyW" | "KeyT" | "KeyN" | "KeyR" | "KeyU" | "KeyJ" | "F4"
        ),
        (true, false, true) => matches!(
            code,
            "KeyW"
                | "KeyT"
                | "KeyN"
                | "KeyQ"
                | "KeyR"
                | "KeyO"
                | "KeyM"
                | "KeyA"
                | "KeyI"
                | "KeyJ"
                | "Delete"
        ),
        (false, true, false) => matches!(code, "F4" | "ArrowLeft" | "ArrowRight" | "Home"),
        _ => false,
    }
}

/// Longest text one paste inserts, in characters.
pub const MAX_PASTE_CHARS: usize = 65_536;

/// Why clipboard text cannot be pasted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteRejected {
    /// Nothing remains without control characters.
    Empty,
    /// The text has this many characters, more than [`MAX_PASTE_CHARS`].
    TooLong(usize),
}

/// Clipboard text as [`Command::InsertText`] inserts it: without control
/// characters other than tab and line breaks, at most [`MAX_PASTE_CHARS`].
pub fn paste_text(text: &str) -> Result<String, PasteRejected> {
    let text: String = text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let length = text.chars().count();
    if length > MAX_PASTE_CHARS {
        return Err(PasteRejected::TooLong(length));
    }
    if text.is_empty() {
        return Err(PasteRejected::Empty);
    }
    Ok(text)
}

/// Maps a point in UI pixels to CSS pixels of a device viewport. `image` is the
/// displayed frame as (left, top, width, height) in the same UI pixels, and
/// `viewport` the CSS size the frame shows. Points outside the image map to `None`.
pub fn to_viewport(
    point: (f64, f64),
    image: (f64, f64, f64, f64),
    viewport: (f64, f64),
) -> Option<(f64, f64)> {
    let (left, top, width, height) = image;
    if width <= 0.0 || height <= 0.0 || viewport.0 <= 0.0 || viewport.1 <= 0.0 {
        return None;
    }
    let x = (point.0 - left) / width;
    let y = (point.1 - top) / height;
    ((0.0..1.0).contains(&x) && (0.0..1.0).contains(&y)).then_some((x * viewport.0, y * viewport.1))
}

struct Shared {
    frames: Mutex<Vec<Option<Frame>>>,
    status: Mutex<Status>,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn update(&self, change: impl FnOnce(&mut Status)) {
        change(&mut lock(&self.status));
        (self.notify)();
    }

    fn device(&self, index: usize, change: impl FnOnce(&mut DeviceStatus)) {
        self.update(|status| {
            if let Some(device) = status.devices.get_mut(index) {
                change(device);
            }
        });
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic elsewhere must not make the UI lose the runtime status.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn run_worker(
    workspace: &Workspace,
    options: &BrowserOptions,
    limits: Limits,
    commands: &Receiver<Command>,
    shared: &Shared,
) {
    let error = match drive(workspace, options, limits, commands, shared) {
        Ok(()) => None,
        Err(error) if error.is::<Cancelled>() => None,
        Err(error) => Some(format!("{error:#}")),
    };
    lock(&shared.frames)
        .iter_mut()
        .for_each(|frame| *frame = None);
    shared.update(|status| {
        status.runtime = RuntimeState::Stopped { error };
        for device in &mut status.devices {
            device.loading = false;
            device.streaming = false;
        }
    });
}

fn drive(
    workspace: &Workspace,
    options: &BrowserOptions,
    limits: Limits,
    commands: &Receiver<Command>,
    shared: &Shared,
) -> Result<()> {
    let mut browser = BrowserProcess::start(options, true)?;
    let result = match browser.connect(&limits) {
        Ok(cdp) => Controller::new(cdp, workspace, shared, limits)
            .and_then(|controller| controller.run(commands)),
        Err(error) => Err(error),
    };
    let cleanup = browser.shutdown();
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(cleanup)) => Err(cleanup.context("browser cleanup failed")),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("browser cleanup also failed: {cleanup:#}")))
        }
    }
}

struct Controller<'a> {
    cdp: Cdp,
    workspace: &'a Workspace,
    shared: &'a Shared,
    limits: Limits,
    contexts: HashSet<String>,
    extensions: ExtensionObservations,
    devices: Vec<LiveDevice>,
    router: SyncRouter,
    sync: SyncSettings,
    pending: HashMap<u64, Pending>,
}

struct LinkIntent {
    url: String,
    id: u64,
    at: Instant,
    generation: u64,
    confirmed: bool,
}

struct LinkCandidate {
    url: String,
    id: u64,
    confirmed: bool,
}

struct LinkNavigation {
    url: String,
    id: u64,
    loader: String,
    confirmed: bool,
}

fn same_link_activation(expected_id: u64, expected_url: &str, id: u64, url: &str) -> bool {
    expected_id == id && expected_url == url
}

struct LiveDevice {
    target_id: String,
    session: String,
    css: (f64, f64),
    visible: bool,
    streaming: bool,
    limit: (u32, u32),
    /// Increments on every committed cross-document navigation.
    generation: u64,
    link_context: Option<i64>,
    link_intent: Option<LinkIntent>,
    requested_link: Option<LinkCandidate>,
    link_navigation: Option<LinkNavigation>,
    /// Sequence of sync events originating here.
    sequence: u64,
    frame_sequence: u64,
    wheel: Option<Wheel>,
    wheel_in_flight: bool,
    pointer_move: Option<PointerEvent>,
    move_in_flight: bool,
    /// Input events sent to the page that it has not answered yet.
    unanswered: usize,
    /// Since when the page has left input unanswered without answering any.
    waiting_since: Option<Instant>,
    /// The page left input unanswered too long or too often. New input is
    /// dropped, never queued, until the page answers again.
    unresponsive: bool,
    /// The navigation Broxser started that has not committed, failed or stopped.
    navigation: Option<Navigation>,
}

/// A navigation Broxser started. It is stopped and reported, never retried,
/// if it has not ended by `deadline`.
struct Navigation {
    /// The navigate or reload command while its answer is outstanding.
    command: Option<u64>,
    /// `Page.navigate` answers once the navigation commits or fails, but
    /// `Page.reload` as soon as it starts. A reload follows its main-frame
    /// loader until commit, stop or replacement by another document navigation.
    reload: bool,
    /// Main-frame loader started by this reload. A later, distinct loader can
    /// replace it without inheriting its deadline.
    reload_loader: Option<String>,
    /// Its document committed, loading stopped or another loader started before the reply.
    /// A later reload start can still identify this command's own loader.
    settled_before_reply: bool,
    deadline: Instant,
}

struct Wheel {
    x: f64,
    y: f64,
    delta_x: f64,
    delta_y: f64,
    generation: u64,
}

/// A command whose answer the live loop waits for without blocking.
enum Pending {
    /// The command of a device's [`Navigation`].
    Navigate {
        device: usize,
    },
    Wheel {
        device: usize,
    },
    Move {
        device: usize,
    },
    /// A pointer button or key event.
    Input {
        device: usize,
    },
}

impl Pending {
    fn device(&self) -> usize {
        match *self {
            Self::Navigate { device }
            | Self::Wheel { device }
            | Self::Move { device }
            | Self::Input { device } => device,
        }
    }
}

impl Commands for Controller<'_> {
    fn command(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        let id = self.cdp.send(method, params, session)?;
        let deadline = Instant::now() + self.limits.command;
        loop {
            if let Some(response) = self.cdp.take_response(id) {
                return parse_response(response, method);
            }
            if !self.cdp.read_until(deadline)? {
                bail!("CDP {method} response timed out");
            }
            self.drain()?;
        }
    }
}

impl<'a> Controller<'a> {
    fn new(cdp: Cdp, workspace: &'a Workspace, shared: &'a Shared, limits: Limits) -> Result<Self> {
        Ok(Self {
            cdp,
            workspace,
            shared,
            limits,
            contexts: HashSet::new(),
            extensions: ExtensionObservations::default(),
            devices: Vec::new(),
            router: SyncRouter::new(workspace).map_err(|error| anyhow!("{error}"))?,
            sync: SyncSettings::default(),
            pending: HashMap::new(),
        })
    }

    fn run(mut self, commands: &Receiver<Command>) -> Result<()> {
        self.setup()?;
        loop {
            for _ in 0..COMMANDS_PER_TURN {
                match commands.try_recv() {
                    Ok(command) => self.handle(command)?,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return Ok(()),
                }
            }
            self.flush_input()?;
            if self.cdp.read_until(Instant::now() + LIVE_POLL)? {
                self.drain()?;
            }
            self.expire(Instant::now())?;
            if let Some(error) = self.cdp.take_detached_error() {
                self.shared
                    .update(|status| status.protocol_error = Some(error));
            }
        }
    }

    fn setup(&mut self) -> Result<()> {
        let workspace = self.workspace;
        let version = self.command("Browser.getVersion", json!({}), None)?;
        let product = required_str(&version, "product")?.to_owned();
        let protocol = required_str(&version, "protocolVersion")?.to_owned();
        self.command("Target.setDiscoverTargets", json!({"discover": true}), None)?;
        let mut contexts = HashMap::new();
        for session in &workspace.sessions {
            let response = self.command("Target.createBrowserContext", json!({}), None)?;
            let context = required_str(&response, "browserContextId")?.to_owned();
            self.contexts.insert(context.clone());
            if let Some(extension) = self.extensions.in_context(&context) {
                bail!(
                    "browser runtime not qualified: extension {extension} runs inside a Broxser session context (ADR 0004)"
                );
            }
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
            let (target_id, session) = setup_target(self, &context, device)?;
            // Every device keeps focus semantics so typing works on any of them.
            self.command(
                "Emulation.setFocusEmulationEnabled",
                json!({"enabled": true}),
                Some(&session),
            )?;
            let physical = |css: u32| {
                ((f64::from(css) * device.device_scale_factor).ceil() as u32)
                    .min(DEFAULT_FRAME_EDGE)
            };
            self.devices.push(LiveDevice {
                target_id,
                session,
                css: (f64::from(device.width), f64::from(device.height)),
                visible: true,
                streaming: false,
                limit: (physical(device.width), physical(device.height)),
                generation: 0,
                link_context: None,
                link_intent: None,
                requested_link: None,
                link_navigation: None,
                sequence: 0,
                frame_sequence: 0,
                wheel: None,
                wheel_in_flight: false,
                pointer_move: None,
                move_in_flight: false,
                unanswered: 0,
                waiting_since: None,
                unresponsive: false,
                navigation: None,
            });
            self.command(
                "Runtime.enable",
                json!({}),
                Some(&self.devices.last().unwrap().session.clone()),
            )?;
            self.command(
                "Runtime.addBinding",
                json!({"name": LINK_BINDING, "executionContextName": LINK_WORLD}),
                Some(&self.devices.last().unwrap().session.clone()),
            )?;
            self.command(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({"source": LINK_OBSERVER, "worldName": LINK_WORLD, "runImmediately": true}),
                Some(&self.devices.last().unwrap().session.clone()),
            )?;
        }
        self.cdp.set_poll_interval(LIVE_POLL)?;
        self.shared
            .update(|status| status.runtime = RuntimeState::Running { product, protocol });
        for index in 0..self.devices.len() {
            self.start_stream(index)?;
        }
        // Opening the workspace loads its URL once. Restarting is a new explicit action.
        for index in 0..self.devices.len() {
            self.navigate(index, &workspace.url)?;
        }
        Ok(())
    }

    fn handle(&mut self, command: Command) -> Result<()> {
        let count = self.devices.len();
        match command {
            Command::NavigateAll { url } => {
                if let Err(error) = validate_url(&url) {
                    for index in 0..count {
                        self.shared
                            .device(index, |device| device.error = Some(error.to_string()));
                    }
                    return Ok(());
                }
                for index in 0..count {
                    self.navigate(index, &url)?;
                }
            }
            Command::Reload { device } if device < count && self.devices[device].visible => {
                self.start_navigation(device, "Page.reload", json!({}))?;
            }
            Command::Pointer { device, event } if device < count => self.pointer(device, event)?,
            Command::Wheel {
                device,
                x,
                y,
                delta_x,
                delta_y,
            } if device < count => self.wheel(device, x, y, delta_x, delta_y),
            Command::Key { device, key } if device < count => self.key(device, &key)?,
            Command::InsertText { device, text } if device < count => {
                self.insert_text(device, &text)?;
            }
            Command::SetVisible { device, visible } if device < count => {
                self.devices[device].visible = visible;
                if visible {
                    self.cdp.send_detached(
                        "Input.setIgnoreInputEvents",
                        json!({"ignore": false}),
                        Some(&self.devices[device].session.clone()),
                    )?;
                    self.start_stream(device)?;
                } else {
                    let state = &mut self.devices[device];
                    state.pointer_move = None;
                    state.wheel = None;
                    state.link_intent = None;
                    state.requested_link = None;
                    state.link_navigation = None;
                    self.cdp.send_detached(
                        "Input.setIgnoreInputEvents",
                        json!({"ignore": true}),
                        Some(&state.session.clone()),
                    )?;
                    self.stop_stream(device)?;
                }
            }
            Command::SetFrameLimit {
                device,
                width,
                height,
            } if device < count => {
                let limit = (
                    width.clamp(64, DEFAULT_FRAME_EDGE * 2),
                    height.clamp(64, DEFAULT_FRAME_EDGE * 2),
                );
                if self.devices[device].limit != limit {
                    self.devices[device].limit = limit;
                    if self.devices[device].streaming {
                        self.stop_stream(device)?;
                        self.start_stream(device)?;
                    }
                }
            }
            Command::SetSync(settings) => self.set_sync(settings)?,
            #[cfg(test)]
            Command::CrashForTest { device } if device < count => {
                // The crashing renderer may never answer.
                let session = self.devices[device].session.clone();
                self.cdp
                    .send_ignored("Page.crash", json!({}), Some(&session))?;
            }
            _ => {}
        }
        Ok(())
    }

    fn navigate(&mut self, index: usize, url: &str) -> Result<()> {
        let device = &mut self.devices[index];
        device.wheel = None;
        device.pointer_move = None;
        device.link_intent = None;
        device.requested_link = None;
        device.link_navigation = None;
        self.start_navigation(index, "Page.navigate", json!({"url": url}))
    }

    /// Sends a navigation of `index` and replaces the one it may still follow:
    /// the browser cancels that navigation, and its late answer must not
    /// describe this one.
    fn start_navigation(&mut self, index: usize, method: &str, params: Value) -> Result<()> {
        self.forget_navigation(index);
        let session = self.devices[index].session.clone();
        let id = self.cdp.send(method, params, Some(&session))?;
        self.track(id, Pending::Navigate { device: index })?;
        self.devices[index].navigation = Some(Navigation {
            command: Some(id),
            reload: method == "Page.reload",
            reload_loader: None,
            settled_before_reply: false,
            deadline: Instant::now() + self.limits.load,
        });
        let unresponsive = self.devices[index].unresponsive;
        self.shared.device(index, |device| {
            device.loading = true;
            device.error = unresponsive.then(|| NOT_RESPONDING.to_owned());
        });
        Ok(())
    }

    fn track(&mut self, id: u64, pending: Pending) -> Result<()> {
        if self.pending.len() >= MAX_PENDING {
            bail!("too many unanswered live commands");
        }
        if !matches!(pending, Pending::Navigate { .. }) {
            let device = &mut self.devices[pending.device()];
            device.unanswered += 1;
            device.waiting_since.get_or_insert_with(Instant::now);
        }
        self.pending.insert(id, pending);
        Ok(())
    }

    /// Stops following the navigation that Broxser started on device `index`;
    /// a late answer to it is discarded. Nothing is sent again.
    fn forget_navigation(&mut self, index: usize) {
        if let Some(Navigation {
            command: Some(id), ..
        }) = self.devices[index].navigation.take()
        {
            self.pending.remove(&id);
            self.cdp.abandon(id);
        }
    }

    /// Stops waiting for the input that device `index` has not answered; late
    /// answers are discarded. Nothing is sent again.
    fn forget_input(&mut self, index: usize) {
        let forgotten: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, pending)| {
                pending.device() == index && !matches!(pending, Pending::Navigate { .. })
            })
            .map(|(id, _)| *id)
            .collect();
        for id in forgotten {
            self.pending.remove(&id);
            self.cdp.abandon(id);
        }
        let device = &mut self.devices[index];
        device.unanswered = 0;
        device.waiting_since = None;
        device.move_in_flight = false;
        device.wheel_in_flight = false;
        device.unresponsive = false;
    }

    /// The main frame of device `index` committed or stopped loading. That
    /// ends an acknowledged reload; a navigate command ends with its answer.
    /// A History API URL change is not evidence that a pending reload ended.
    fn navigation_settled(&mut self, index: usize) {
        let device = &mut self.devices[index];
        if device
            .navigation
            .as_ref()
            .is_some_and(|navigation| navigation.command.is_none())
        {
            device.navigation = None;
        }
    }

    /// Whether new input may go to device `index` now. Input that may not is
    /// dropped, never queued or sent later.
    fn input_allowed(&mut self, index: usize) -> bool {
        let device = &self.devices[index];
        if !device.visible || device.unresponsive {
            return false;
        }
        if device.unanswered >= MAX_UNANSWERED_INPUT {
            self.set_unresponsive(index);
            return false;
        }
        true
    }

    /// Reports that the page of device `index` leaves input unanswered, and
    /// drops its coalesced move and wheel. An error already shown, such as an
    /// open dialog or a crash, explains more and stays.
    fn set_unresponsive(&mut self, index: usize) {
        let device = &mut self.devices[index];
        device.unresponsive = true;
        device.pointer_move = None;
        device.wheel = None;
        self.shared.device(index, |status| {
            status
                .error
                .get_or_insert_with(|| NOT_RESPONDING.to_owned());
        });
    }

    /// The page of device `index` answered an input event, so it responds.
    fn answered(&mut self, index: usize) {
        let device = &mut self.devices[index];
        device.unanswered -= 1;
        device.waiting_since = (device.unanswered > 0).then(Instant::now);
        if std::mem::take(&mut device.unresponsive) {
            self.shared.device(index, |status| {
                if status.error.as_deref() == Some(NOT_RESPONDING) {
                    status.error = None;
                }
            });
        }
    }

    /// Applies deadlines. The browser must answer detached commands within the
    /// command limit, or the runtime stops. A navigation that has not ended
    /// within the load limit is stopped and reported. A page that leaves input
    /// unanswered for the command limit is reported as not responding. None of
    /// them is sent again.
    fn expire(&mut self, now: Instant) -> Result<()> {
        if let Some((sent, method)) = self.cdp.oldest_detached()
            && now >= sent + self.limits.command
        {
            bail!(
                "the browser did not answer {method} within {} seconds",
                self.limits.command.as_secs_f32()
            );
        }
        for index in 0..self.devices.len() {
            if self.devices[index]
                .navigation
                .as_ref()
                .is_some_and(|navigation| now >= navigation.deadline)
            {
                let settled_before_reply =
                    self.devices[index]
                        .navigation
                        .as_ref()
                        .is_some_and(|navigation| {
                            navigation.command.is_some() && navigation.settled_before_reply
                        });
                self.forget_navigation(index);
                if !settled_before_reply {
                    let session = self.devices[index].session.clone();
                    // Like the Stop button: the page keeps its current document.
                    self.cdp
                        .send_ignored("Page.stopLoading", json!({}), Some(&session))?;
                    let error = format!(
                        "Navigation got no response within {} seconds; loading stopped, not retried",
                        self.limits.load.as_secs_f32()
                    );
                    self.shared.device(index, |status| {
                        status.loading = false;
                        status.error = Some(error);
                    });
                }
            }
            let device = &self.devices[index];
            if !device.unresponsive
                && device
                    .waiting_since
                    .is_some_and(|since| now >= since + self.limits.command)
            {
                self.set_unresponsive(index);
            }
        }
        Ok(())
    }

    fn pointer(&mut self, index: usize, event: PointerEvent) -> Result<()> {
        // A middle click would paste Chromium's selection buffer, which every
        // session of the browser shares (ADR 0010).
        if event.button == PointerButton::Middle || !self.input_allowed(index) {
            return Ok(());
        }
        let device = &mut self.devices[index];
        if event.kind == PointerKind::Move {
            // Coalesce moves: only the newest one waits while another is in flight.
            device.pointer_move = Some(event);
            return Ok(());
        }
        device.pointer_move = None;
        let session = device.session.clone();
        let params = mouse_params(event, device.css);
        let id = self
            .cdp
            .send("Input.dispatchMouseEvent", params, Some(&session))?;
        self.track(id, Pending::Input { device: index })
    }

    fn wheel(&mut self, index: usize, x: f64, y: f64, delta_x: f64, delta_y: f64) {
        if !self.input_allowed(index) {
            return;
        }
        self.queue_wheel(index, x, y, delta_x, delta_y);
        if !self.sync.scroll {
            return;
        }
        let device = &mut self.devices[index];
        device.sequence += 1;
        let origin = &self.workspace.devices[index];
        let event = SyncEvent {
            origin_device: origin.id.clone(),
            origin_session: origin.session.clone(),
            sequence: device.sequence,
            action: SyncAction::Scroll {
                x: delta_x.round() as i32,
                y: delta_y.round() as i32,
            },
            replayed: false,
        };
        let Ok(deliveries) = self.router.route(&event) else {
            return;
        };
        for delivery in deliveries {
            if let (Some(target), SyncAction::Scroll { x, y }) = (
                self.index_of(&delivery.destination_device),
                &delivery.event.action,
            ) {
                if !self.devices[target].visible {
                    continue;
                }
                let (width, height) = self.devices[target].css;
                self.queue_wheel(
                    target,
                    width / 2.0,
                    height / 2.0,
                    f64::from(*x),
                    f64::from(*y),
                );
            }
        }
    }

    fn queue_wheel(&mut self, index: usize, x: f64, y: f64, delta_x: f64, delta_y: f64) {
        let device = &mut self.devices[index];
        let generation = device.generation;
        match &mut device.wheel {
            Some(wheel) if wheel.generation == generation => {
                wheel.x = x;
                wheel.y = y;
                wheel.delta_x += delta_x;
                wheel.delta_y += delta_y;
            }
            slot => {
                *slot = Some(Wheel {
                    x,
                    y,
                    delta_x,
                    delta_y,
                    generation,
                })
            }
        }
    }

    fn key(&mut self, index: usize, key: &KeyInput) -> Result<()> {
        if is_paste_key(key) || is_browser_key(key) || !self.input_allowed(index) {
            return Ok(());
        }
        if key.key.is_empty()
            || key.key.chars().count() > 32
            || key.code.len() > 32
            || key.text.as_ref().is_some_and(|text| {
                text.chars().count() > 4 || (text != "\r" && text.chars().any(char::is_control))
            })
        {
            return Ok(());
        }
        let kind = match (key.down, &key.text) {
            (false, _) => "keyUp",
            (true, Some(_)) => "keyDown",
            (true, None) => "rawKeyDown",
        };
        let mut params = json!({
            "type": kind,
            "key": key.key,
            "code": key.code,
            "windowsVirtualKeyCode": key.key_code,
            "nativeVirtualKeyCode": key.key_code,
            "modifiers": key.modifiers.cdp(),
        });
        if key.down
            && let Some(text) = &key.text
        {
            params["text"] = json!(text);
            params["unmodifiedText"] = json!(text);
        }
        let session = self.devices[index].session.clone();
        let id = self
            .cdp
            .send("Input.dispatchKeyEvent", params, Some(&session))?;
        self.track(id, Pending::Input { device: index })
    }

    /// Inserts pasted text into the focused element of device `index`. Only
    /// text that [`paste_text`] would return unchanged is inserted.
    fn insert_text(&mut self, index: usize, text: &str) -> Result<()> {
        if paste_text(text).as_deref() != Ok(text) || !self.input_allowed(index) {
            return Ok(());
        }
        let session = self.devices[index].session.clone();
        let id = self
            .cdp
            .send("Input.insertText", json!({"text": text}), Some(&session))?;
        self.track(id, Pending::Input { device: index })
    }

    /// Sends coalesced pointer moves and wheel deltas, one in flight per device.
    fn flush_input(&mut self) -> Result<()> {
        for index in 0..self.devices.len() {
            let device = &self.devices[index];
            if device.pointer_move.is_none() && device.wheel.is_none() {
                continue;
            }
            if !self.input_allowed(index) {
                // Hidden or not responding: queued input is dropped, never sent later.
                let device = &mut self.devices[index];
                device.pointer_move = None;
                device.wheel = None;
                continue;
            }
            let device = &mut self.devices[index];
            if !device.move_in_flight
                && let Some(event) = device.pointer_move.take()
            {
                let params = mouse_params(event, device.css);
                let session = device.session.clone();
                let id = self
                    .cdp
                    .send("Input.dispatchMouseEvent", params, Some(&session))?;
                self.devices[index].move_in_flight = true;
                self.track(id, Pending::Move { device: index })?;
            }
            // Sending the move may have reached the unanswered input limit.
            if self.devices[index].wheel_in_flight || !self.input_allowed(index) {
                continue;
            }
            let device = &mut self.devices[index];
            let Some(wheel) = device.wheel.take() else {
                continue;
            };
            // A scroll queued before a navigation belongs to the old page.
            if wheel.generation != device.generation {
                continue;
            }
            let session = device.session.clone();
            let id = self.cdp.send(
                "Input.dispatchMouseEvent",
                json!({
                    "type": "mouseWheel",
                    "x": wheel.x.clamp(0.0, device.css.0 - 1.0),
                    "y": wheel.y.clamp(0.0, device.css.1 - 1.0),
                    "deltaX": wheel.delta_x,
                    "deltaY": wheel.delta_y,
                }),
                Some(&session),
            )?;
            self.devices[index].wheel_in_flight = true;
            self.track(id, Pending::Wheel { device: index })?;
        }
        Ok(())
    }

    fn start_stream(&mut self, index: usize) -> Result<()> {
        let device = &mut self.devices[index];
        if !device.visible || device.streaming {
            return Ok(());
        }
        device.streaming = true;
        let (max_width, max_height) = device.limit;
        let session = device.session.clone();
        self.cdp.send_detached(
            "Page.startScreencast",
            json!({"format": "jpeg", "quality": JPEG_QUALITY, "maxWidth": max_width, "maxHeight": max_height, "everyNthFrame": 1}),
            Some(&session),
        )?;
        self.shared.device(index, |device| device.streaming = true);
        Ok(())
    }

    fn stop_stream(&mut self, index: usize) -> Result<()> {
        let device = &mut self.devices[index];
        if !device.streaming {
            return Ok(());
        }
        device.streaming = false;
        let session = device.session.clone();
        self.cdp
            .send_detached("Page.stopScreencast", json!({}), Some(&session))?;
        lock(&self.shared.frames)[index] = None;
        self.shared.device(index, |device| device.streaming = false);
        Ok(())
    }

    fn set_sync(&mut self, settings: SyncSettings) -> Result<()> {
        let workspace = self.workspace;
        let enabled = settings.navigation || settings.scroll;
        for from in &workspace.devices {
            for to in &workspace.devices {
                if from.id == to.id || from.session != to.session {
                    continue;
                }
                if enabled {
                    self.router
                        .enable_route(&from.id, &to.id)
                        .map_err(|error| anyhow!("{error}"))?;
                } else {
                    self.router.disable_route(&from.id, &to.id);
                }
            }
        }
        self.sync = settings;
        if !settings.navigation {
            for device in &mut self.devices {
                device.link_intent = None;
                device.requested_link = None;
                device.link_navigation = None;
            }
        }
        self.shared.update(|status| status.sync = settings);
        Ok(())
    }

    fn index_of(&self, device_id: &str) -> Option<usize> {
        self.workspace
            .devices
            .iter()
            .position(|device| device.id == device_id)
    }

    /// Observes buffered events and resolves responses for pending commands.
    fn drain(&mut self) -> Result<()> {
        while let Some(event) = self.cdp.pop_event() {
            self.observe(event)?;
        }
        let ready: Vec<u64> = self.pending.keys().copied().collect();
        for id in ready {
            let Some(response) = self.cdp.take_response(id) else {
                continue;
            };
            match self.pending.remove(&id) {
                Some(Pending::Navigate { device }) => {
                    let error = match parse_response(response, "Page.navigate") {
                        Ok(result) => result
                            .get("errorText")
                            .and_then(Value::as_str)
                            .map(|error| format!("Navigation failed: {error}; not retried")),
                        Err(error) => Some(format!("{error:#}")),
                    };
                    let navigation = &mut self.devices[device].navigation;
                    let settled_before_reply = navigation
                        .as_ref()
                        .is_some_and(|started| started.reload && started.settled_before_reply);
                    match navigation {
                        Some(_) if settled_before_reply => *navigation = None,
                        Some(started) if started.reload && error.is_none() => {
                            started.command = None;
                        }
                        _ => *navigation = None,
                    }
                    if let Some(error) = error.filter(|_| !settled_before_reply) {
                        self.shared.device(device, |status| {
                            status.error = Some(error);
                            status.loading = false;
                        });
                    }
                }
                Some(Pending::Wheel { device }) => {
                    self.devices[device].wheel_in_flight = false;
                    self.answered(device);
                }
                Some(Pending::Move { device }) => {
                    self.devices[device].move_in_flight = false;
                    self.answered(device);
                }
                Some(Pending::Input { device }) => {
                    if let Some(error) = error_message(&response) {
                        self.shared
                            .update(|status| status.protocol_error = Some(error));
                    }
                    self.answered(device);
                }
                None => {}
            }
        }
        Ok(())
    }

    fn observe(&mut self, event: Event) -> Result<()> {
        let params = &event.params;
        let text = |field: &str| params.get(field).and_then(Value::as_str);
        match event.method.as_str() {
            "Target.targetCreated" | "Target.targetInfoChanged" => {
                let Some(info) = params.get("targetInfo") else {
                    return Ok(());
                };
                self.extensions.observe(info)?;
                if let Some(extension) = extension_in_context(info, &self.contexts) {
                    bail!(
                        "browser runtime not qualified: extension {extension} runs inside a Broxser \
                         session context and can reload pages or replay navigations (ADR 0004)"
                    );
                }
                if event.method == "Target.targetCreated"
                    && info.get("type").and_then(Value::as_str) == Some("page")
                    && let Some(opener) = info.get("openerId").and_then(Value::as_str)
                    && let Some(index) = self
                        .devices
                        .iter()
                        .position(|device| device.target_id == opener)
                {
                    self.shared.device(index, |device| device.popups += 1);
                }
                return Ok(());
            }
            "Target.targetCrashed" | "Target.detachedFromTarget" => {
                if let Some(index) = self
                    .devices
                    .iter()
                    .position(|device| Some(device.target_id.as_str()) == text("targetId"))
                {
                    let crashed = event.method == "Target.targetCrashed";
                    // The renderer or the session that owed these answers is
                    // gone; the error below describes the device instead.
                    self.forget_input(index);
                    self.forget_navigation(index);
                    self.devices[index].streaming = false;
                    self.shared.device(index, |device| {
                        device.loading = false;
                        device.streaming = false;
                        device.error = Some(if crashed {
                            "The page crashed. Reload the device to start a new renderer.".into()
                        } else {
                            "The page target was detached.".into()
                        });
                    });
                }
                return Ok(());
            }
            _ => {}
        }
        let Some(index) = self
            .devices
            .iter()
            .position(|device| event.session.as_deref() == Some(device.session.as_str()))
        else {
            return Ok(());
        };
        match event.method.as_str() {
            "Runtime.executionContextCreated" => {
                if let Some(context) = params.get("context")
                    && context.get("name").and_then(Value::as_str) == Some(LINK_WORLD)
                    && context.pointer("/auxData/frameId").and_then(Value::as_str)
                        == Some(self.devices[index].target_id.as_str())
                    && context.pointer("/auxData/type").and_then(Value::as_str) == Some("isolated")
                {
                    self.devices[index].link_context = context
                        .get("id")
                        .and_then(Value::as_i64)
                        .filter(|id| *id > 0);
                }
            }
            "Runtime.executionContextDestroyed" => {
                if self.devices[index].link_context.is_some_and(|id| {
                    params.get("executionContextId").and_then(Value::as_i64) == Some(id)
                }) {
                    self.devices[index].link_context = None;
                    self.devices[index].link_intent = None;
                }
            }
            "Runtime.executionContextsCleared" => {
                self.devices[index].link_context = None;
                self.devices[index].link_intent = None;
            }
            "Runtime.bindingCalled"
                if text("name") == Some(LINK_BINDING)
                    && self.devices[index].link_context.is_some_and(|id| {
                        params.get("executionContextId").and_then(Value::as_i64) == Some(id)
                    })
                    && self.devices[index].visible =>
            {
                let device = &mut self.devices[index];
                if let Some(payload) = text("payload")
                    && payload.len() <= MAX_LINK_INTENT_BYTES + 128
                    && let Ok(message) = serde_json::from_str::<Value>(payload)
                    && let (Some(kind), Some(id), Some(url)) = (
                        message.get("phase").and_then(Value::as_str),
                        message.get("id").and_then(Value::as_u64),
                        message.get("url").and_then(Value::as_str),
                    )
                    && id <= u32::MAX as u64
                    && url.len() <= MAX_LINK_INTENT_BYTES
                    && validate_url(url).is_ok()
                {
                    match kind {
                        "C" => {
                            device.link_intent = Some(LinkIntent {
                                url: url.to_owned(),
                                id,
                                at: Instant::now(),
                                generation: device.generation,
                                confirmed: false,
                            });
                        }
                        "Y" => {
                            if let Some(intent) = &mut device.link_intent
                                && same_link_activation(intent.id, &intent.url, id, url)
                                && intent.generation == device.generation
                                && intent.at.elapsed() <= LINK_INTENT_WINDOW
                            {
                                intent.confirmed = true;
                            }
                            if let Some(requested) = &mut device.requested_link
                                && same_link_activation(requested.id, &requested.url, id, url)
                            {
                                requested.confirmed = true;
                            }
                            if let Some(navigation) = &mut device.link_navigation
                                && same_link_activation(navigation.id, &navigation.url, id, url)
                            {
                                navigation.confirmed = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
            "Page.screencastFrame" => self.frame(index, params)?,
            "Page.frameStartedLoading" | "Page.frameStoppedLoading"
                if text("frameId") == Some(self.devices[index].target_id.as_str()) =>
            {
                let loading = event.method == "Page.frameStartedLoading";
                if !loading {
                    let navigation = &mut self.devices[index].navigation;
                    let reload_started = navigation
                        .as_ref()
                        .is_some_and(|started| started.reload && started.reload_loader.is_some());
                    if reload_started
                        && let Some(started) = navigation
                        && started.command.is_some()
                    {
                        started.settled_before_reply = true;
                    }
                    if reload_started {
                        self.navigation_settled(index);
                    }
                }
                self.shared.device(index, |device| device.loading = loading);
            }
            "Page.frameRequestedNavigation"
                if text("frameId") == Some(self.devices[index].target_id.as_str()) =>
            {
                let sync_navigation = self.sync.navigation;
                let device = &mut self.devices[index];
                let url = text("url");
                device.requested_link = device.link_intent.as_ref().and_then(|intent| {
                    (sync_navigation
                        && device.visible
                        && text("reason") == Some("anchorClick")
                        && text("disposition").is_none_or(|value| value == "currentTab")
                        && url == Some(intent.url.as_str())
                        && intent.generation == device.generation
                        && intent.at.elapsed() <= LINK_INTENT_WINDOW)
                        .then(|| LinkCandidate {
                            url: intent.url.clone(),
                            id: intent.id,
                            confirmed: intent.confirmed,
                        })
                });
                device.link_intent = None;
                device.link_navigation = None;
            }
            "Page.frameStartedNavigating"
                if text("frameId") == Some(self.devices[index].target_id.as_str()) =>
            {
                if let (Some(loader), Some(kind)) = (
                    text("loaderId").filter(|loader| !loader.is_empty()),
                    text("navigationType"),
                ) {
                    let replaced_reload = self.devices[index]
                        .navigation
                        .as_mut()
                        .filter(|navigation| navigation.reload)
                        .is_some_and(|navigation| match kind {
                            "sameDocument" | "historySameDocument" => false,
                            "reload" | "reloadBypassingCache" => {
                                if navigation.command.is_some() {
                                    // A prior Reload can start after a newer
                                    // command was sent. The last reload start
                                    // before this command's reply is its loader.
                                    if navigation.reload_loader.as_deref() != Some(loader) {
                                        navigation.settled_before_reply = false;
                                    }
                                    navigation.reload_loader = Some(loader.to_owned());
                                    false
                                } else if let Some(started) = &navigation.reload_loader {
                                    started != loader
                                } else {
                                    navigation.reload_loader = Some(loader.to_owned());
                                    false
                                }
                            }
                            "differentDocument"
                            | "historyDifferentDocument"
                            | "restore"
                            | "restoreWithPost" => navigation
                                .reload_loader
                                .as_deref()
                                .is_some_and(|started| started != loader),
                            _ => false,
                        });
                    if replaced_reload {
                        if let Some(navigation) = &mut self.devices[index].navigation
                            && navigation.command.is_some()
                        {
                            // Another queued Reload can still start before
                            // this command's reply and take ownership back.
                            navigation.settled_before_reply = true;
                        } else {
                            // The acknowledged reload has been replaced.
                            self.forget_navigation(index);
                        }
                    }
                }
                let device = &mut self.devices[index];
                device.link_navigation =
                    match (device.requested_link.take(), text("url"), text("loaderId")) {
                        (Some(expected), Some(url), Some(loader))
                            if expected.url == url && !loader.is_empty() =>
                        {
                            Some(LinkNavigation {
                                url: expected.url,
                                id: expected.id,
                                loader: loader.to_owned(),
                                confirmed: expected.confirmed,
                            })
                        }
                        _ => None,
                    };
                device.link_intent = None;
            }
            "Page.navigatedWithinDocument"
                if text("frameId") == Some(self.devices[index].target_id.as_str()) =>
            {
                let url = text("url").unwrap_or_default().to_owned();
                self.devices[index].link_intent = None;
                self.devices[index].requested_link = None;
                self.devices[index].link_navigation = None;
                self.shared.device(index, |device| device.url = url);
            }
            "Page.frameNavigated" => {
                let Some(frame) = params.get("frame") else {
                    return Ok(());
                };
                if frame.get("parentId").is_some()
                    || frame.get("id").and_then(Value::as_str)
                        != Some(self.devices[index].target_id.as_str())
                {
                    return Ok(());
                }
                let url = frame
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                // A new document answers input again; the old one's is moot.
                self.forget_input(index);
                let matches_reload =
                    self.devices[index]
                        .navigation
                        .as_ref()
                        .is_some_and(|started| {
                            started.reload
                                && frame.get("loaderId").and_then(Value::as_str).is_some_and(
                                    |loader| started.reload_loader.as_deref() == Some(loader),
                                )
                        });
                if matches_reload {
                    if let Some(started) = &mut self.devices[index].navigation
                        && started.command.is_some()
                    {
                        started.settled_before_reply = true;
                    }
                    self.navigation_settled(index);
                }
                let device = &mut self.devices[index];
                device.generation += 1;
                device.wheel = None;
                let link = device.link_navigation.take().is_some_and(|link| {
                    link.confirmed
                        && frame.get("loaderId").and_then(Value::as_str)
                            == Some(link.loader.as_str())
                        && url == link.url
                });
                device.link_intent = None;
                device.requested_link = None;
                if device.visible && !device.streaming {
                    // A crashed renderer is replaced on navigation; resume its stream.
                    self.start_stream(index)?;
                }
                self.shared.device(index, |device| {
                    device.url = url.clone();
                    device.error = None;
                });
                if self.sync.navigation && link && self.devices[index].visible {
                    self.sync_navigation(index, url)?;
                }
            }
            "Page.javascriptDialogOpening" => {
                self.shared.device(index, |device| {
                    device.error = Some(
                        "The page opened a JavaScript dialog, which Broxser cannot show yet."
                            .into(),
                    )
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn frame(&mut self, index: usize, params: &Value) -> Result<()> {
        // Acknowledge first, including frames that are dropped below, so the
        // browser keeps producing frames for this target.
        if let Some(frame) = params.get("sessionId").and_then(Value::as_u64) {
            let session = self.devices[index].session.clone();
            self.cdp.send_detached(
                "Page.screencastFrameAck",
                json!({"sessionId": frame}),
                Some(&session),
            )?;
        }
        let device = &mut self.devices[index];
        if !device.streaming {
            return Ok(());
        }
        let Some(data) = params.get("data").and_then(Value::as_str) else {
            return Ok(());
        };
        if data.len() > MAX_FRAME_BYTES / 3 * 4 + 4 {
            bail!("screencast frame exceeds {MAX_FRAME_BYTES} bytes");
        }
        let jpeg = base64::engine::general_purpose::STANDARD
            .decode(data)
            .context("decode screencast frame")?;
        let metadata = params.get("metadata");
        let size = |field: &str, fallback: f64| {
            metadata
                .and_then(|metadata| metadata.get(field))
                .and_then(Value::as_f64)
                .filter(|value| value.is_finite() && *value > 0.0)
                .unwrap_or(fallback)
        };
        device.frame_sequence += 1;
        let frame = Frame {
            jpeg,
            sequence: device.frame_sequence,
            css_width: size("deviceWidth", device.css.0),
            css_height: size("deviceHeight", device.css.1),
        };
        let replaced = lock(&self.shared.frames)[index].replace(frame).is_some();
        self.shared.device(index, |device| {
            device.frames += 1;
            device.dropped_frames += u64::from(replaced);
        });
        Ok(())
    }

    fn sync_navigation(&mut self, origin: usize, url: String) -> Result<()> {
        let device = &mut self.devices[origin];
        device.sequence += 1;
        let source = &self.workspace.devices[origin];
        let event = SyncEvent {
            origin_device: source.id.clone(),
            origin_session: source.session.clone(),
            sequence: device.sequence,
            action: SyncAction::Navigate { url },
            replayed: false,
        };
        // Non-HTTP destinations and stale events are not synchronized.
        let Ok(deliveries) = self.router.route(&event) else {
            return Ok(());
        };
        for delivery in deliveries {
            if let (Some(target), SyncAction::Navigate { url }) = (
                self.index_of(&delivery.destination_device),
                &delivery.event.action,
            ) && self.devices[target].visible
            {
                self.navigate(target, url)?;
            }
        }
        Ok(())
    }
}

fn mouse_params(event: PointerEvent, css: (f64, f64)) -> Value {
    json!({
        "type": match event.kind {
            PointerKind::Move => "mouseMoved",
            PointerKind::Down => "mousePressed",
            PointerKind::Up => "mouseReleased",
        },
        "x": event.x.clamp(0.0, css.0 - 1.0),
        "y": event.y.clamp(0.0, css.1 - 1.0),
        "button": match event.button {
            PointerButton::None => "none",
            PointerButton::Left => "left",
            PointerButton::Middle => "middle",
            PointerButton::Right => "right",
        },
        // Middle presses never reach pages (ADR 0010), so neither does its bit.
        "buttons": event.buttons & !4,
        "clickCount": event.click_count,
        "modifiers": event.modifiers.cdp(),
    })
}

#[cfg(test)]
mod tests;
