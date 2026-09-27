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
use crate::device::{
    Commands, ExtensionObservations, deny_permission_prompts, extension_in_context, setup_target,
};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use broxser_core::{MAX_DEVICES, SyncAction, SyncEvent, SyncRouter, Workspace, validate_url};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
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
/// One navigation and one dialog answer plus the unanswered input of every
/// device, so one stuck device can never exhaust the commands of the others.
const MAX_PENDING: usize = MAX_DEVICES * (MAX_UNANSWERED_INPUT + 2);
/// Device status while its page leaves input unanswered.
pub(crate) const NOT_RESPONDING: &str =
    "The page is not responding to input. New input is dropped, not sent later.";
const MAX_LINK_INTENT_BYTES: usize = 8192;
const LINK_INTENT_WINDOW: Duration = Duration::from_secs(1);
/// Longest dialog message, prompt default or prompt answer, in characters.
pub const MAX_DIALOG_CHARS: usize = 2048;
/// Subframes remembered per device, so that downloads they start are
/// attributed to it (ADR 0016).
const MAX_TRACKED_FRAMES: usize = 256;
/// Bounds active iframe sessions and retired iframe/worker sessions, including
/// stalled cleanup. Each keeps at most one awaited setup or resume command.
const MAX_IFRAME_SESSIONS: usize = 128;
const INCOMPLETE_IFRAME_ACTIVITY: &str = "Iframe activity could not be fully observed. Navigate or reload explicitly to start a fresh document.";
/// Start of the device status for a navigation Broxser started that failed.
const NAVIGATION_FAILED: &str = "Navigation failed: ";
/// Device status while it has an open dialog and Broxser was asked to navigate it.
pub(crate) const DIALOG_OPEN: &str =
    "The page is waiting for an answer to its dialog; navigation was not sent.";
/// Device status after a prompt answer that Broxser did not send; the dialog
/// stays open for another answer.
pub(crate) const PROMPT_REJECTED: &str = "The prompt answer was not sent: it must be at most 2048 characters with no control characters.";
/// Device status while the page shows a dialog of a kind Broxser cannot show.
pub(crate) const UNKNOWN_DIALOG: &str =
    "The page opened a JavaScript dialog Broxser cannot show; reload the device.";
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
mod ime;
use ime::{IME_BINDING, IME_OBSERVER, IME_WORLD, parse_caret_report, valid_ime_action};
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
    /// Windows the page opened that Broxser closed (ADR 0015).
    pub popups: u32,
    /// The latest of them, until the user opens it in the device or the page
    /// opens another.
    pub popup: Option<PopupState>,
    /// Downloads the page started, all refused by the browser (ADR 0016).
    pub downloads: u32,
    /// The latest of them.
    pub download: Option<DownloadState>,
    /// File choosers the page opened. Each was answered as cancelled; pages
    /// get no files (ADR 0016).
    pub file_choosers: u32,
    /// Current main-frame editable caret. Cleared whenever its target is unsafe.
    pub text_input: Option<TextInputState>,
    /// A JavaScript dialog the page is waiting on. The page, its frames and
    /// its input stay blocked until the user answers it (ADR 0014).
    pub dialog: Option<DialogState>,
}

/// A window the page opened and Broxser closed as soon as the browser
/// reported it. `url` is untrusted page text, bounded for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PopupState {
    pub url: String,
    /// The complete URL is an HTTP(S) address within the navigation limit, so
    /// [`Command::OpenPopup`] can load it in the device.
    pub openable: bool,
    /// Engine-assigned identity; opening an older report does nothing.
    pub token: u64,
}

/// A download the page started and the browser refused. Both fields are
/// untrusted page text on one line, bounded for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadState {
    /// The file name the browser derived for it.
    pub filename: String,
    pub url: String,
}

/// An open JavaScript dialog, as the page requested it. `message` and
/// `default_text` are untrusted page text, bounded and shown only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogState {
    pub kind: DialogKind,
    pub message: String,
    /// The prompt's proposed answer, bounded so that it can be sent back
    /// unchanged: one line (line breaks and tabs become spaces), no other
    /// control characters, at most [`MAX_DIALOG_CHARS`], cut without a marker.
    /// Empty for other kinds.
    pub default_text: String,
    /// Engine-assigned identity: an answer to a dialog that already closed
    /// cannot answer the next one.
    pub token: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogKind {
    Alert,
    Confirm,
    Prompt,
    /// The page asks whether to leave; accepting continues the navigation.
    BeforeUnload,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaretRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextInputState {
    /// Engine-assigned token; old tokens cannot type into a new focus or anchor.
    pub target: u64,
    pub caret: CaretRect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImeAction {
    /// UTF-16 code-unit offsets within `text`.
    Preedit {
        text: String,
        selection: std::ops::Range<usize>,
    },
    Commit {
        text: String,
    },
    Cancel,
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
    /// Abandons a touch whose native pointer left its canvas or lost focus.
    /// This never ends the touch as a tap or waits behind an IME identity read.
    CancelTouch {
        device: usize,
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
    Ime {
        device: usize,
        target: u64,
        action: ImeAction,
    },
    /// The user's explicit answer to the device's open dialog. `text` is the
    /// prompt's answer when accepted. Nothing else ever answers a dialog.
    AnswerDialog {
        device: usize,
        token: u64,
        accept: bool,
        text: Option<String>,
    },
    /// Explicitly loads the reported popup `token` in its device, replacing
    /// the device's page. Only the URL the engine recorded is loaded.
    OpenPopup {
        device: usize,
        token: u64,
    },
    /// Hidden devices stop their screencast and keep no frame.
    SetVisible {
        device: usize,
        visible: bool,
    },
    /// A device whose frame lies outside the visible canvas pauses its
    /// screencast. Unlike hiding, input, sync and IME stay as they are.
    SetOnScreen {
        device: usize,
        on_screen: bool,
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

impl Command {
    /// The device this page input goes to; other commands have none.
    fn input_device(&self) -> Option<usize> {
        match *self {
            Self::Pointer { device, .. }
            | Self::Wheel { device, .. }
            | Self::Key { device, .. }
            | Self::InsertText { device, .. }
            | Self::Ime { device, .. } => Some(device),
            _ => None,
        }
    }
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
            // No browser is left to show a dialog or take its answer.
            device.dialog = None;
            clear_dialog_error(device);
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
    let (browser, cdp) = BrowserProcess::start_connected(options, true, &limits)?;
    let result = Controller::new(cdp, workspace, shared, limits)
        .and_then(|controller| controller.run(commands));
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
    next_ime_target: u64,
    next_dialog: u64,
    next_popup: u64,
    iframe_sessions: HashMap<String, IframeSession>,
    iframe_pending: HashMap<u64, IframeCommand>,
    iframe_cleanup: HashMap<String, IframeCleanup>,
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

/// Ownership of the single finger Broxser sent to CDP. Chromium keeps its
/// active touch across hiding, dialogs and document replacement. An abandoned
/// finger must be canceled before a fresh press, never ended as a stale click.
#[derive(Clone, Copy, Default)]
enum TouchGesture {
    #[default]
    Idle,
    Active {
        x: f64,
        y: f64,
    },
    NeedsCancel,
}

struct LiveDevice {
    context: String,
    target_id: String,
    session: String,
    css: (f64, f64),
    /// A touch device: pointer input reaches the page as touches (ADR 0018).
    touch: bool,
    touch_gesture: TouchGesture,
    visible: bool,
    /// The UI shows part of the frame; otherwise the screencast pauses.
    on_screen: bool,
    streaming: bool,
    limit: (u32, u32),
    /// Increments on every committed cross-document navigation.
    generation: u64,
    link_context: Option<i64>,
    ime_context: Option<i64>,
    ime_anchor: Option<u64>,
    ime_blocked_anchor: Option<u64>,
    ime_target: Option<u64>,
    composing: bool,
    /// The IME action whose editable identity check is outstanding.
    ime_check: Option<ImeCheck>,
    /// Input that arrived after that action, in order. It is sent once the
    /// check has sent or dropped the action; a navigation, a new document,
    /// hiding, a crash or overload drops it instead.
    held_input: VecDeque<Command>,
    link_intent: Option<LinkIntent>,
    requested_link: Option<LinkCandidate>,
    link_navigation: Option<LinkNavigation>,
    /// Sequence of sync events originating here.
    sequence: u64,
    frame_sequence: u64,
    wheel: Option<Wheel>,
    wheel_in_flight: bool,
    pointer_move: Option<PointerEvent>,
    /// Furthest unsent touch point from the last sent point. Together with
    /// pointer_move (the newest point), this keeps a coalesced out-and-back
    /// drag from turning into a tap, with no unbounded path history.
    touch_excursion: Option<PointerEvent>,
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
    /// The open JavaScript dialog. Input and navigation wait for the user's
    /// answer; nothing answers it for them (ADR 0014).
    dialog: Option<OpenDialog>,
    /// URL of the page's latest `window.open`, reported just before its
    /// target appears.
    window_open: Option<String>,
    /// The closed popup the user may load here: its token and complete URL.
    popup: Option<(u64, String)>,
    /// Subframes of the current document, including frames that another
    /// renderer process now runs, so their downloads are attributed here.
    frames: HashMap<String, OwnedFrame>,
    frame_revision: u64,
    iframe_activity_incomplete: bool,
}

struct OwnedFrame {
    parent: String,
    loader: Option<String>,
}

struct IframeSession {
    device: usize,
    frame: String,
    parent_session: String,
    waiting: bool,
    deadline: Instant,
}

/// A retired session is never observed again. Workers enter this release-only
/// path without page setup or frame ownership. A busy target may leave its
/// resume unanswered; retain only this bounded cleanup record until it answers
/// or the browser reports destruction. Detach ancestors only after children.
struct IframeCleanup {
    device: usize,
    frame: String,
    parent_session: String,
    resume: Option<u64>,
    detaching: bool,
}

#[derive(Clone, Copy)]
enum IframeSetup {
    Enable,
    Intercept,
    AutoAttach,
    FrameTree,
    Resume,
}

struct IframeCommand {
    session: String,
    stage: IframeSetup,
    deadline: Instant,
    revision: u64,
}

/// Auto-attach is local to an owned page/iframe and excludes pages (popups).
/// Helium pauses related workers with waitForDebuggerOnStart even if a filter
/// excludes them; include them so their sessions can be resumed and released.
/// Only iframe targets get recursive observation and page setup.
fn iframe_auto_attach() -> Value {
    json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true,
    "filter": [
        {"type": "iframe", "exclude": false},
        {"type": "worker", "exclude": false},
        {"type": "shared_worker", "exclude": false},
        {"type": "service_worker", "exclude": false},
        {"exclude": true}
    ]})
}

#[derive(Clone, Copy)]
struct OpenDialog {
    token: u64,
    kind: DialogKind,
    /// The user's answer, once sent and unless the browser refuses it.
    /// `Page.handleJavaScriptDialog` answers whichever dialog the page shows,
    /// so a second answer could reach the next dialog before this one's
    /// closing is read.
    answer: Option<bool>,
    /// A `beforeunload` question about the navigation Broxser follows rather
    /// than one the page started.
    for_navigation: bool,
}

/// Page text for display: control characters other than line breaks and tabs
/// are dropped, and text beyond [`MAX_DIALOG_CHARS`] is cut and marked.
fn dialog_text(text: &str) -> String {
    shown_text(text, |c| matches!(c, '\n' | '\t'))
}

/// Page text for display on one line: every control character is dropped,
/// and text beyond [`MAX_DIALOG_CHARS`] is cut and marked.
fn display_line(text: &str) -> String {
    shown_text(text, |_| false)
}

fn shown_text(text: &str, keep_control: fn(char) -> bool) -> String {
    let mut kept = text.chars().filter(|&c| !c.is_control() || keep_control(c));
    let mut shown: String = kept.by_ref().take(MAX_DIALOG_CHARS).collect();
    if kept.next().is_some() {
        shown.push('…');
    }
    shown
}

/// A prompt's default as an answer the user can send unchanged: line breaks
/// and tabs become spaces, other control characters are dropped, and it is
/// cut at [`MAX_DIALOG_CHARS`] without a marker.
fn prompt_text(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\n' | '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .take(MAX_DIALOG_CHARS)
        .collect()
}

/// Clears a device error that describes a dialog; it ends with the dialog.
fn clear_dialog_error(status: &mut DeviceStatus) {
    if status
        .error
        .as_deref()
        .is_some_and(|error| [DIALOG_OPEN, PROMPT_REJECTED, UNKNOWN_DIALOG].contains(&error))
    {
        status.error = None;
    }
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
    /// The page asked for a main-frame navigation of its own in the current
    /// tab since this one was sent; a `beforeunload` question that follows is
    /// about the page's.
    page_requested: bool,
    deadline: Instant,
}

struct Wheel {
    x: f64,
    y: f64,
    delta_x: f64,
    delta_y: f64,
    generation: u64,
}

/// An IME action waiting for a read of the current editable identity in the
/// isolated world. A script can move focus before its next animation-frame
/// report arrives; the read rejects that. It waits without blocking the
/// runtime, under the deadlines of ordinary input (ADR 0011).
struct ImeCheck {
    target: u64,
    action: ImeAction,
    /// The read, sent once the page has answered earlier input.
    read: Option<u64>,
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
    /// The identity read of a device's [`ImeCheck`].
    ImeRead {
        device: usize,
    },
    /// The user's answer to dialog `token` of a device. The browser process
    /// answers it at once; it is no page input and has no deadline.
    DialogAnswer {
        device: usize,
        token: u64,
    },
}

impl Pending {
    fn device(&self) -> usize {
        match *self {
            Self::Navigate { device }
            | Self::Wheel { device }
            | Self::Move { device }
            | Self::Input { device }
            | Self::ImeRead { device }
            | Self::DialogAnswer { device, .. } => device,
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
            next_ime_target: 0,
            next_dialog: 0,
            next_popup: 0,
            iframe_sessions: HashMap::new(),
            iframe_pending: HashMap::new(),
            iframe_cleanup: HashMap::new(),
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
            for index in 0..self.devices.len() {
                self.release_input(index)?;
                self.read_ime_target(index)?;
            }
            self.flush_input()?;
            if self.cdp.read_until(Instant::now() + LIVE_POLL)? {
                self.drain()?;
            }
            self.expire(Instant::now())?;
            for (index, device) in self.devices.iter().enumerate() {
                if device.iframe_activity_incomplete
                    && lock(&self.shared.status).devices[index].error.is_none()
                {
                    self.shared.device(index, |status| {
                        if status.error.is_none() {
                            status.error = Some(INCOMPLETE_IFRAME_ACTIVITY.into());
                        }
                    });
                }
            }
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
            // Broxser saves no download, whatever the browser's default, and
            // reports each refused one on its device (ADR 0016).
            self.command(
                "Browser.setDownloadBehavior",
                json!({"behavior": "deny", "browserContextId": context, "eventsEnabled": true}),
                None,
            )?;
            deny_permission_prompts(self, &context)?;
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
            // A file chooser is reported, then cancelled for the page as the
            // headless browser does without interception. No file is given.
            self.command(
                "Page.setInterceptFileChooserDialog",
                json!({"enabled": true, "cancel": true}),
                Some(&session),
            )?;
            let physical = |css: u32| {
                ((f64::from(css) * device.device_scale_factor).ceil() as u32)
                    .min(DEFAULT_FRAME_EDGE)
            };
            self.devices.push(LiveDevice {
                context,
                target_id,
                session,
                css: (f64::from(device.width), f64::from(device.height)),
                touch: device.touch,
                touch_gesture: TouchGesture::Idle,
                visible: true,
                on_screen: true,
                streaming: false,
                limit: (physical(device.width), physical(device.height)),
                generation: 0,
                link_context: None,
                ime_context: None,
                ime_anchor: None,
                ime_blocked_anchor: None,
                ime_target: None,
                composing: false,
                ime_check: None,
                held_input: VecDeque::new(),
                link_intent: None,
                requested_link: None,
                link_navigation: None,
                sequence: 0,
                frame_sequence: 0,
                wheel: None,
                wheel_in_flight: false,
                pointer_move: None,
                touch_excursion: None,
                move_in_flight: false,
                unanswered: 0,
                waiting_since: None,
                unresponsive: false,
                navigation: None,
                dialog: None,
                window_open: None,
                popup: None,
                frames: HashMap::new(),
                frame_revision: 0,
                iframe_activity_incomplete: false,
            });
            self.command(
                "Target.setAutoAttach",
                iframe_auto_attach(),
                Some(&self.devices.last().unwrap().session.clone()),
            )?;
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
            self.command(
                "Runtime.addBinding",
                json!({"name": IME_BINDING, "executionContextName": IME_WORLD}),
                Some(&self.devices.last().unwrap().session.clone()),
            )?;
            self.command(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({"source": IME_OBSERVER, "worldName": IME_WORLD, "runImmediately": true}),
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
            self.navigate(index, &workspace.url, true)?;
        }
        Ok(())
    }

    fn handle(&mut self, command: Command) -> Result<()> {
        // Ignored touch buttons must not wait behind an IME action or cancel
        // the current composition when the held input is released.
        if let Command::Pointer { device, event } = &command
            && self.devices.get(*device).is_some_and(|state| state.touch)
            && !is_touch(*event)
        {
            return Ok(());
        }
        if let Some(index) = command
            .input_device()
            .filter(|&index| index < self.devices.len())
        {
            self.release_input(index)?;
            if self.devices[index].ime_check.is_some() {
                self.hold_input(index, command);
                return Ok(());
            }
        }
        self.dispatch(command)
    }

    fn dispatch(&mut self, command: Command) -> Result<()> {
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
                    self.navigate(index, &url, true)?;
                }
            }
            Command::Reload { device } if device < count && self.devices[device].visible => {
                self.start_navigation(device, "Page.reload", json!({}))?;
            }
            Command::Pointer { device, event } if device < count => self.pointer(device, event)?,
            Command::CancelTouch { device } if device < count && self.devices[device].touch => {
                self.devices[device]
                    .held_input
                    .retain(|command| !matches!(command, Command::Pointer { .. }));
                self.cancel_touch(device)?;
            }
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
            Command::Ime {
                device,
                target,
                action,
            } if device < count => {
                self.ime(device, target, action)?;
            }
            Command::AnswerDialog {
                device,
                token,
                accept,
                text,
            } if device < count => {
                self.answer_dialog(device, token, accept, text)?;
            }
            Command::OpenPopup { device, token } if device < count => {
                if let Some((current, url)) = self.devices[device].popup.clone()
                    && current == token
                {
                    self.devices[device].popup = None;
                    self.shared.device(device, |status| status.popup = None);
                    self.navigate(device, &url, true)?;
                }
            }
            Command::SetVisible { device, visible } if device < count => {
                if visible {
                    self.devices[device].visible = true;
                    self.cdp.send_detached(
                        "Input.setIgnoreInputEvents",
                        json!({"ignore": false}),
                        Some(&self.devices[device].session.clone()),
                    )?;
                    self.start_stream(device)?;
                    self.refresh_ime(device)?;
                } else {
                    self.cancel_touch(device)?;
                    self.clear_ime(device, true)?;
                    self.devices[device].visible = false;
                    let state = &mut self.devices[device];
                    state.pointer_move = None;
                    state.wheel = None;
                    state.held_input.clear();
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
            Command::SetOnScreen { device, on_screen } if device < count => {
                self.devices[device].on_screen = on_screen;
                if on_screen {
                    self.start_stream(device)?;
                } else {
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

    /// Navigates device `index` for Go, workspace open or link sync. A sent
    /// navigation supersedes the link the device follows, and so does an
    /// explicit one that an open dialog refuses (ADR 0013); a refused synced
    /// link leaves the device's own link to commit and sync.
    fn navigate(&mut self, index: usize, url: &str, explicit: bool) -> Result<()> {
        let sent = self.start_navigation(index, "Page.navigate", json!({"url": url}))?;
        if sent || explicit {
            let device = &mut self.devices[index];
            device.wheel = None;
            device.pointer_move = None;
            device.link_intent = None;
            device.requested_link = None;
            device.link_navigation = None;
        }
        Ok(())
    }

    /// Sends a navigation of `index` and replaces the one it may still follow:
    /// the browser cancels that navigation, and its late answer must not
    /// describe this one. Returns whether it was sent; a device with an open
    /// dialog keeps its state.
    fn start_navigation(&mut self, index: usize, method: &str, params: Value) -> Result<bool> {
        if self.devices[index].dialog.is_some() {
            // Navigating would answer the dialog for the user: Chromium
            // cancels an alert, confirm or prompt when a navigation starts.
            self.shared
                .device(index, |device| device.error = Some(DIALOG_OPEN.to_owned()));
            return Ok(false);
        }
        self.cancel_touch(index)?;
        self.clear_ime(index, true)?;
        // Input held for the current document never reaches the next one.
        self.devices[index].held_input.clear();
        self.forget_navigation(index);
        let session = self.devices[index].session.clone();
        let id = self.cdp.send(method, params, Some(&session))?;
        self.track(id, Pending::Navigate { device: index })?;
        self.devices[index].navigation = Some(Navigation {
            command: Some(id),
            reload: method == "Page.reload",
            reload_loader: None,
            settled_before_reply: false,
            page_requested: false,
            deadline: Instant::now() + self.limits.load,
        });
        let unresponsive = self.devices[index].unresponsive;
        let incomplete = self.devices[index].iframe_activity_incomplete;
        self.shared.device(index, |device| {
            device.loading = true;
            device.error = if incomplete {
                Some(INCOMPLETE_IFRAME_ACTIVITY.into())
            } else {
                unresponsive.then(|| NOT_RESPONDING.to_owned())
            };
        });
        Ok(true)
    }

    fn track(&mut self, id: u64, pending: Pending) -> Result<()> {
        if self.pending.len() >= MAX_PENDING {
            bail!("too many unanswered live commands");
        }
        if !matches!(
            pending,
            Pending::Navigate { .. } | Pending::DialogAnswer { .. }
        ) {
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

    /// Stops waiting for the input that device `index` has not answered and
    /// for the reply to its dialog answer; late answers are discarded. Nothing
    /// is sent again.
    fn forget_input(&mut self, index: usize) {
        self.invalidate_touch(index);
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
        device.held_input.clear();
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
        // Chromium holds pointer input while a dialog is open and delivers it
        // when the dialog closes, out of context; keys it drops (ADR 0014).
        if !device.visible || device.unresponsive || device.dialog.is_some() {
            return false;
        }
        if device.unanswered >= MAX_UNANSWERED_INPUT {
            self.set_unresponsive(index);
            return false;
        }
        true
    }

    /// Reports that the page of device `index` leaves input unanswered, and
    /// drops its coalesced move and wheel and the input held behind an IME
    /// action. An error already shown, such as a failed navigation, explains
    /// more and stays. An open dialog is no sign of a page that stopped
    /// responding; it pauses this check instead (ADR 0014).
    fn set_unresponsive(&mut self, index: usize) {
        self.invalidate_touch(index);
        self.invalidate_ime(index);
        let device = &mut self.devices[index];
        device.unresponsive = true;
        device.pointer_move = None;
        device.wheel = None;
        device.held_input.clear();
        self.shared.device(index, |status| {
            status
                .error
                .get_or_insert_with(|| NOT_RESPONDING.to_owned());
        });
    }

    /// Reports a window that the page of device `index` opened and Broxser
    /// closed. Its URL is the one of the page's latest `window.open`.
    fn popup_closed(&mut self, index: usize) -> Result<()> {
        self.next_popup = self
            .next_popup
            .checked_add(1)
            .ok_or_else(|| anyhow!("popup token exhausted"))?;
        let token = self.next_popup;
        let url = self.devices[index].window_open.take().unwrap_or_default();
        // The complete URL, never a shortened one, is what the user may load.
        let openable = url.len() <= MAX_LINK_INTENT_BYTES && validate_url(&url).is_ok();
        self.devices[index].popup = openable.then(|| (token, url.clone()));
        let state = PopupState {
            url: dialog_text(&url),
            openable,
            token,
        };
        self.shared.device(index, |device| {
            device.popups = device.popups.saturating_add(1);
            device.popup = Some(state);
        });
        Ok(())
    }

    /// Sends the user's answer to the open dialog of device `index`, if `token`
    /// still names it and it has no answer yet. The dialog stays until the
    /// browser reports it closed. A prompt answer that is too long or holds
    /// control characters is not sent, and the dialog waits for another, as
    /// it does after an answer that the browser refuses.
    fn answer_dialog(
        &mut self,
        index: usize,
        token: u64,
        accept: bool,
        text: Option<String>,
    ) -> Result<()> {
        let Some(dialog) = self.devices[index]
            .dialog
            .filter(|dialog| dialog.token == token && dialog.answer.is_none())
        else {
            return Ok(());
        };
        let mut params = json!({"accept": accept});
        if accept
            && dialog.kind == DialogKind::Prompt
            && let Some(text) = text
        {
            if text.chars().count() > MAX_DIALOG_CHARS || text.chars().any(char::is_control) {
                self.shared.device(index, |device| {
                    device.error = Some(PROMPT_REJECTED.to_owned());
                });
                return Ok(());
            }
            params["promptText"] = json!(text);
        }
        let session = self.devices[index].session.clone();
        // The browser process answers at once; `Page.javascriptDialogClosed`
        // is the confirmation.
        let id = self
            .cdp
            .send("Page.handleJavaScriptDialog", params, Some(&session))?;
        self.track(
            id,
            Pending::DialogAnswer {
                device: index,
                token,
            },
        )?;
        if let Some(dialog) = &mut self.devices[index].dialog {
            dialog.answer = Some(accept);
        }
        self.shared.device(index, |device| {
            if device.error.as_deref() == Some(PROMPT_REJECTED) {
                device.error = None;
            }
        });
        Ok(())
    }

    /// A dialog of device `index` closed, through the user's answer or the
    /// page itself. Input is accepted again and the paused deadlines resume.
    fn dialog_closed(&mut self, index: usize, accepted: bool) -> Result<()> {
        let dialog = self.devices[index].dialog.take();
        // Staying ends the navigation that Broxser asked about: its
        // net::ERR_ABORTED is the user's choice, not a failure. A question
        // about the page's own navigation leaves Broxser's navigation running.
        let stayed = dialog.is_some_and(|dialog| {
            dialog.kind == DialogKind::BeforeUnload && dialog.for_navigation && !accepted
        });
        if dialog.is_some() {
            let now = Instant::now();
            let device = &mut self.devices[index];
            device.waiting_since = (device.unanswered > 0).then_some(now);
            if let Some(navigation) = &mut device.navigation {
                navigation.deadline = now + self.limits.load;
            }
        }
        if stayed {
            self.forget_navigation(index);
        }
        self.shared.device(index, |status| {
            status.dialog = None;
            clear_dialog_error(status);
            if stayed {
                status.loading = false;
            }
        });
        if dialog.is_some() {
            // The dialog dropped the caret; the observer reports it again.
            self.refresh_ime(index)?;
        }
        Ok(())
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
        let expired: Vec<String> = self
            .iframe_pending
            .values()
            .filter(|pending| now >= pending.deadline)
            .map(|pending| pending.session.clone())
            .collect();
        for session in expired {
            self.fail_iframe_setup(&session)?;
        }
        self.drain_iframe_cleanup()?;
        if let Some((sent, method)) = self.cdp.oldest_detached()
            && now >= sent + self.limits.command
        {
            bail!(
                "the browser did not answer {method} within {} seconds",
                self.limits.command.as_secs_f32()
            );
        }
        for index in 0..self.devices.len() {
            if self.devices[index].dialog.is_some() {
                // The page waits for the user, not for the network or a
                // script; its deadlines resume when the dialog closes.
                continue;
            }
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
        if event.button == PointerButton::Middle
            || (self.devices[index].touch && !is_touch(event))
            || !self.input_allowed(index)
        {
            return Ok(());
        }
        if self.devices[index].touch {
            match event.kind {
                PointerKind::Down => {
                    // A second press starts a new gesture, including after a
                    // lost release. Canceling cannot synthesize a click.
                    self.cancel_touch(index)?;
                    if !self.input_allowed(index) {
                        return Ok(());
                    }
                }
                PointerKind::Move | PointerKind::Up => {
                    if !matches!(
                        self.devices[index].touch_gesture,
                        TouchGesture::Active { .. }
                    ) {
                        return Ok(());
                    }
                }
            }
        }
        if event.kind == PointerKind::Down {
            self.devices[index].ime_blocked_anchor = self.devices[index].ime_anchor;
            self.clear_ime(index, true)?;
            if !self.input_allowed(index) {
                return Ok(());
            }
        }
        let device = &mut self.devices[index];
        if event.kind == PointerKind::Move {
            // Coalesce moves: only the newest one waits while another is in flight.
            if let TouchGesture::Active { x, y } = device.touch_gesture {
                let distance = |next| {
                    let (next_x, next_y) = pointer_position(next, device.css);
                    (next_x - x).powi(2) + (next_y - y).powi(2)
                };
                if device
                    .touch_excursion
                    .is_none_or(|queued| distance(event) >= distance(queued))
                {
                    device.touch_excursion = Some(event);
                }
            }
            device.pointer_move = Some(event);
            return Ok(());
        }
        if device.touch && event.kind == PointerKind::Up {
            return self.end_touch(index, event);
        }
        device.pointer_move = None;
        if device.touch {
            let (x, y) = pointer_position(event, device.css);
            device.touch_gesture = TouchGesture::Active { x, y };
        }
        let session = device.session.clone();
        let (method, params) = pointer_params(event, device.css, device.touch);
        let id = self.cdp.send(method, params, Some(&session))?;
        self.track(id, Pending::Input { device: index })
    }

    /// Drops local ownership immediately. Cancellation may only be sent while
    /// input is allowed; otherwise a fresh Down resets the browser first.
    fn invalidate_touch(&mut self, index: usize) {
        let device = &mut self.devices[index];
        if matches!(device.touch_gesture, TouchGesture::Active { .. }) {
            device.touch_gesture = TouchGesture::NeedsCancel;
        }
        if device.touch {
            device.pointer_move = None;
            device.touch_excursion = None;
        }
    }

    fn cancel_touch(&mut self, index: usize) -> Result<()> {
        self.invalidate_touch(index);
        if !matches!(self.devices[index].touch_gesture, TouchGesture::NeedsCancel)
            || !self.input_allowed(index)
        {
            return Ok(());
        }
        let session = self.devices[index].session.clone();
        let id = self.cdp.send(
            "Input.dispatchTouchEvent",
            json!({"type": "touchCancel", "touchPoints": []}),
            Some(&session),
        )?;
        self.devices[index].touch_gesture = TouchGesture::Idle;
        self.track(id, Pending::Input { device: index })
    }

    fn end_touch(&mut self, index: usize, event: PointerEvent) -> Result<()> {
        let device = &mut self.devices[index];
        let TouchGesture::Active { x, y } = device.touch_gesture else {
            return Ok(());
        };
        let mut position = (x, y);
        let mut moves = Vec::with_capacity(2);
        // Preserve a coalesced excursion too: Down(A), Move(B), Move(A), Up(A)
        // must not lose its drag just because the finger returned to its start.
        let excursion = device.touch_excursion.take().or(device.pointer_move.take());
        device.pointer_move = None;
        for next in [
            excursion,
            Some(PointerEvent {
                kind: PointerKind::Move,
                ..event
            }),
        ]
        .into_iter()
        .flatten()
        {
            let next_position = pointer_position(next, device.css);
            if next_position != position {
                moves.push(next);
                position = next_position;
            }
        }
        // touchEnd has no coordinates: its last touchMove determines whether
        // the release taps or swipes. Send the final position in order even
        // while an earlier coalesced move waits for its reply. Never hold Up.
        if device.unanswered + moves.len() + 1 > MAX_UNANSWERED_INPUT {
            self.cancel_touch(index)?;
            self.set_unresponsive(index);
            return Ok(());
        }
        let (session, css) = (device.session.clone(), device.css);
        for next in moves {
            let (method, params) = pointer_params(next, css, true);
            let id = self.cdp.send(method, params, Some(&session))?;
            self.track(id, Pending::Input { device: index })?;
        }
        let (method, params) = pointer_params(event, css, true);
        let id = self.cdp.send(method, params, Some(&session))?;
        self.devices[index].touch_gesture = TouchGesture::Idle;
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

    /// Starts the identity check of an IME action. Input to the device that
    /// follows waits until the check has sent or dropped the action.
    fn ime(&mut self, index: usize, target: u64, action: ImeAction) -> Result<()> {
        if self.devices[index].ime_target != Some(target)
            || !valid_ime_action(&action)
            || !self.input_allowed(index)
        {
            return Ok(());
        }
        self.devices[index].ime_check = Some(ImeCheck {
            target,
            action,
            read: None,
        });
        self.read_ime_target(index)
    }

    /// Reads the current editable identity in its trusted isolated context for
    /// the waiting IME action of device `index`. Input dispatch and
    /// Runtime.evaluate use different renderer queues, so the read waits until
    /// the page has answered earlier input; otherwise it could overtake a key
    /// or click that moves focus. It returns only an identity, never page text
    /// or a password.
    fn read_ime_target(&mut self, index: usize) -> Result<()> {
        let device = &self.devices[index];
        let Some(check) = &device.ime_check else {
            return Ok(());
        };
        if check.read.is_some() || device.unanswered > 0 {
            return Ok(());
        }
        let (Some(context), Some(_)) = (device.ime_context, device.ime_anchor) else {
            self.devices[index].ime_check = None;
            return Ok(());
        };
        let session = device.session.clone();
        let id = self.cdp.send(
            "Runtime.evaluate",
            json!({
                "expression": "globalThis.__broxserImeCurrent?.() ?? 0",
                "contextId": context,
                "returnByValue": true,
                "silent": true,
            }),
            Some(&session),
        )?;
        self.track(id, Pending::ImeRead { device: index })?;
        if let Some(check) = &mut self.devices[index].ime_check {
            check.read = Some(id);
        }
        Ok(())
    }

    /// Sends the waiting IME action of device `index` if read `id` found its
    /// target still current; otherwise the action is dropped.
    fn ime_target_read(&mut self, index: usize, id: u64, response: Value) -> Result<()> {
        let device = &mut self.devices[index];
        if device.ime_check.as_ref().and_then(|check| check.read) != Some(id) {
            return Ok(());
        }
        let Some(check) = device.ime_check.take() else {
            return Ok(());
        };
        let anchor = parse_response(response, "Runtime.evaluate")
            .ok()
            .and_then(|result| result.pointer("/result/value").and_then(Value::as_u64));
        if anchor.is_none() || anchor != device.ime_anchor {
            self.invalidate_ime(index);
            return Ok(());
        }
        if device.ime_target != Some(check.target) || !self.input_allowed(index) {
            return Ok(());
        }
        let session = self.devices[index].session.clone();
        let (method, params) = match check.action {
            ImeAction::Preedit { text, selection } => {
                self.devices[index].composing = true;
                (
                    "Input.imeSetComposition",
                    json!({
                        "text": text,
                        "selectionStart": selection.start,
                        "selectionEnd": selection.end,
                    }),
                )
            }
            ImeAction::Commit { text } => {
                self.devices[index].composing = false;
                ("Input.insertText", json!({"text": text}))
            }
            ImeAction::Cancel => {
                self.devices[index].composing = false;
                (
                    "Input.imeSetComposition",
                    json!({
                        "text": "", "selectionStart": 0, "selectionEnd": 0,
                    }),
                )
            }
        };
        let id = self.cdp.send(method, params, Some(&session))?;
        self.track(id, Pending::Input { device: index })
    }

    /// Holds input to device `index` behind its waiting IME action. As for
    /// sent input, only the newest mouse move and the summed wheel wait. Touch
    /// moves retain their path until pointer() can coalesce with its gesture's
    /// position; the same input bound drops a stalled IME's held path.
    fn hold_input(&mut self, index: usize, command: Command) {
        let device = &mut self.devices[index];
        match (device.held_input.back_mut(), &command) {
            (Some(Command::Pointer { event: held, .. }), Command::Pointer { event, .. })
                if !device.touch
                    && held.kind == PointerKind::Move
                    && event.kind == PointerKind::Move =>
            {
                *held = *event;
                return;
            }
            (
                Some(Command::Wheel {
                    x: held_x,
                    y: held_y,
                    delta_x: held_delta_x,
                    delta_y: held_delta_y,
                    ..
                }),
                Command::Wheel {
                    x,
                    y,
                    delta_x,
                    delta_y,
                    ..
                },
            ) => {
                (*held_x, *held_y) = (*x, *y);
                *held_delta_x += delta_x;
                *held_delta_y += delta_y;
                return;
            }
            _ => {}
        }
        if device.unanswered + device.held_input.len() >= MAX_UNANSWERED_INPUT {
            self.set_unresponsive(index);
            return;
        }
        device.held_input.push_back(command);
    }

    /// Sends the input held behind a decided IME action, in order, until
    /// another IME action waits for its own check.
    fn release_input(&mut self, index: usize) -> Result<()> {
        while self.devices[index].ime_check.is_none()
            && let Some(command) = self.devices[index].held_input.pop_front()
        {
            self.dispatch(command)?;
        }
        Ok(())
    }

    fn invalidate_ime(&mut self, index: usize) {
        let device = &mut self.devices[index];
        let had_target = device.ime_target.is_some();
        device.ime_anchor = None;
        device.ime_target = None;
        // A waiting action is dropped; a late answer to its read is ignored.
        device.ime_check = None;
        device.composing = false;
        if had_target {
            self.shared.device(index, |status| status.text_input = None);
        }
    }

    /// Asks the IME observer of visible device `index` to report its caret
    /// again. The page's renderer answers `Runtime.evaluate` and holds it
    /// while a dialog is open, a script runs or a navigation waits for its
    /// server, so it has no answer deadline: ADR 0008 keeps those for commands
    /// that the browser process answers. A late report is checked as usual.
    fn refresh_ime(&mut self, index: usize) -> Result<()> {
        let device = &self.devices[index];
        if let Some(context) = device.ime_context
            && device.visible
            && device.dialog.is_none()
        {
            let session = device.session.clone();
            self.cdp.send_ignored(
                "Runtime.evaluate",
                json!({"expression":"globalThis.__broxserImeRefresh?.()", "contextId":context, "silent":true}),
                Some(&session),
            )?;
        }
        Ok(())
    }

    fn clear_ime(&mut self, index: usize, cancel: bool) -> Result<()> {
        if cancel && self.devices[index].composing && self.input_allowed(index) {
            let session = self.devices[index].session.clone();
            let id = self.cdp.send(
                "Input.imeSetComposition",
                json!({"text": "", "selectionStart": 0, "selectionEnd": 0}),
                Some(&session),
            )?;
            self.track(id, Pending::Input { device: index })?;
        }
        self.invalidate_ime(index);
        Ok(())
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
                let event = if device.touch {
                    let excursion = device.touch_excursion.take().unwrap_or(event);
                    if pointer_position(excursion, device.css)
                        != pointer_position(event, device.css)
                    {
                        // The furthest point arrives first; the newest still
                        // coalesces while it is in flight, or ends the gesture.
                        device.pointer_move = Some(event);
                        device.touch_excursion = Some(event);
                    }
                    excursion
                } else {
                    event
                };
                let (method, params) = pointer_params(event, device.css, device.touch);
                if device.touch {
                    let (x, y) = pointer_position(event, device.css);
                    device.touch_gesture = TouchGesture::Active { x, y };
                }
                let session = device.session.clone();
                let id = self.cdp.send(method, params, Some(&session))?;
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
        if !device.visible || !device.on_screen || device.streaming {
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
        // A link activated before navigation sync changed never synchronizes.
        if settings.navigation != self.sync.navigation {
            for device in &mut self.devices {
                device.link_intent = None;
                device.requested_link = None;
                device.link_navigation = None;
            }
        }
        self.sync = settings;
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
        self.drain_iframe_setup()?;
        self.drain_iframe_cleanup()?;
        let ready: Vec<u64> = self.pending.keys().copied().collect();
        for id in ready {
            let Some(response) = self.cdp.take_response(id) else {
                continue;
            };
            match self.pending.remove(&id) {
                Some(Pending::Navigate { device }) => {
                    let mut error = match parse_response(response, "Page.navigate") {
                        // The address is a download. The browser refused it and
                        // the device reports it instead of an error (ADR 0016).
                        Ok(result)
                            if result.get("isDownload").and_then(Value::as_bool) == Some(true) =>
                        {
                            self.shared.device(device, |status| status.loading = false);
                            None
                        }
                        Ok(result) => {
                            result
                                .get("errorText")
                                .and_then(Value::as_str)
                                .map(|error| {
                                    let note = crate::browser::certificate_note(error);
                                    format!("{NAVIGATION_FAILED}{error}{note}; not retried")
                                })
                        }
                        Err(error) => Some(format!("{error:#}")),
                    };
                    if self.devices[device].dialog.is_some_and(|dialog| {
                        dialog.kind == DialogKind::BeforeUnload
                            && dialog.for_navigation
                            && dialog.answer == Some(false)
                    }) && error
                        .as_deref()
                        .is_some_and(|error| error.contains("ERR_ABORTED"))
                    {
                        // The page asked whether to leave for this navigation and
                        // the user stayed; the browser can report that before the
                        // dialog's closing.
                        error = None;
                        self.shared.device(device, |status| status.loading = false);
                    }
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
                Some(Pending::ImeRead { device }) => {
                    self.answered(device);
                    self.ime_target_read(device, id, response)?;
                }
                Some(Pending::DialogAnswer { device, token }) => {
                    // A dialog that closed meanwhile is no error. One still
                    // open was not answered and waits for another answer.
                    if response.get("error").is_some()
                        && let Some(dialog) = &mut self.devices[device].dialog
                        && dialog.token == token
                    {
                        dialog.answer = None;
                        if let Some(error) = error_message(&response) {
                            self.shared
                                .update(|status| status.protocol_error = Some(error));
                        }
                    }
                }
                None => {}
            }
        }
        Ok(())
    }

    fn session_owner(&self, session: Option<&str>) -> Option<(usize, bool)> {
        let session = session?;
        self.devices
            .iter()
            .position(|device| device.session == session)
            .map(|index| (index, false))
            .or_else(|| {
                self.iframe_sessions
                    .get(session)
                    .map(|iframe| (iframe.device, true))
            })
    }

    fn owns_frame(&self, index: usize, frame: &str) -> bool {
        self.devices[index].target_id == frame || self.devices[index].frames.contains_key(frame)
    }

    fn frame_in_session(&self, index: usize, frame: &str, session: &str) -> bool {
        let root = self
            .iframe_sessions
            .get(session)
            .map(|iframe| iframe.frame.as_str())
            .unwrap_or(self.devices[index].target_id.as_str());
        let mut next = frame;
        for _ in 0..=MAX_TRACKED_FRAMES {
            if next == root {
                return true;
            }
            let Some(parent) = self.devices[index].frames.get(next) else {
                return false;
            };
            next = &parent.parent;
        }
        false
    }

    fn remember_frame(&mut self, index: usize, frame: &str, parent: &str) -> Result<()> {
        if frame.is_empty() || frame.len() > 128 || parent.len() > 128 || frame == parent {
            bail!("invalid CDP iframe identity");
        }
        if !self.owns_frame(index, parent) {
            return Ok(());
        }
        if self
            .devices
            .iter()
            .enumerate()
            .any(|(other, _)| other != index && self.owns_frame(other, frame))
        {
            bail!("CDP iframe belongs to another device");
        }
        if frame == self.devices[index].target_id {
            bail!("CDP subframe has the device identity");
        }
        let mut ancestor = parent;
        for _ in 0..=MAX_TRACKED_FRAMES {
            if ancestor == frame {
                bail!("CDP frame parent cycle");
            }
            let Some(owner) = self.devices[index].frames.get(ancestor) else {
                break;
            };
            ancestor = &owner.parent;
        }
        if let Some(owner) = self.devices[index].frames.get_mut(frame)
            && owner.parent != parent
        {
            owner.parent = parent.to_owned();
            self.devices[index].frame_revision += 1;
        }
        if !self.devices[index].frames.contains_key(frame) {
            if self.devices[index].frames.len() >= MAX_TRACKED_FRAMES {
                self.mark_iframe_activity_incomplete(index);
                return Ok(());
            }
            self.devices[index].frames.insert(
                frame.to_owned(),
                OwnedFrame {
                    parent: parent.to_owned(),
                    loader: None,
                },
            );
            self.devices[index].frame_revision += 1;
        }
        Ok(())
    }

    /// Forget an entire document, or a removed frame and its descendants.
    /// Retire child sessions before accepting any more events from them.
    fn remove_frame_subtree(&mut self, index: usize, root: Option<&str>) -> Result<()> {
        let mut removed: HashSet<String> = match root {
            Some(root) => [root.to_owned()].into(),
            None => self.devices[index].frames.keys().cloned().collect(),
        };
        loop {
            let before = removed.len();
            for (frame, owner) in &self.devices[index].frames {
                if removed.contains(&owner.parent) {
                    removed.insert(frame.clone());
                }
            }
            if before == removed.len() {
                break;
            }
        }
        self.devices[index]
            .frames
            .retain(|frame, _| !removed.contains(frame));
        self.devices[index].frame_revision += 1;
        let sessions: Vec<String> = self
            .iframe_sessions
            .iter()
            .filter(|(_, iframe)| iframe.device == index && removed.contains(&iframe.frame))
            .map(|(session, _)| session.clone())
            .collect();
        for session in sessions {
            self.retire_iframe(&session)?;
        }
        Ok(())
    }

    fn clear_frame_children(&mut self, index: usize, frame: &str) -> Result<()> {
        let children: Vec<String> = self.devices[index]
            .frames
            .iter()
            .filter(|(_, owner)| owner.parent == frame)
            .map(|(child, _)| child.clone())
            .collect();
        for child in children {
            self.remove_frame_subtree(index, Some(&child))?;
        }
        // Also invalidates a frame-tree snapshot even when there were no children.
        self.devices[index].frame_revision += 1;
        Ok(())
    }

    fn retire_iframe(&mut self, session: &str) -> Result<()> {
        let mut retired: HashSet<String> = [session.to_owned()].into();
        loop {
            let before = retired.len();
            for (child, iframe) in &self.iframe_sessions {
                if retired.contains(&iframe.parent_session) {
                    retired.insert(child.clone());
                }
            }
            if before == retired.len() {
                break;
            }
        }
        // Transfer an already-sent resume to cleanup; never resend it just
        // because its response was late. Other setup responses become stale.
        let mut resumes = HashMap::new();
        let commands: Vec<u64> = self
            .iframe_pending
            .iter()
            .filter(|(_, pending)| retired.contains(&pending.session))
            .map(|(&id, _)| id)
            .collect();
        for id in commands {
            let pending = self.iframe_pending.remove(&id).unwrap();
            if matches!(pending.stage, IframeSetup::Resume) {
                resumes.insert(pending.session, id);
            } else {
                self.cdp.abandon(id);
            }
        }
        for session in retired {
            if let Some(iframe) = self.iframe_sessions.remove(&session) {
                let resume = match resumes.remove(&session) {
                    Some(id) => Some(id),
                    None if iframe.waiting => Some(self.cdp.send(
                        "Runtime.runIfWaitingForDebugger",
                        json!({}),
                        Some(&session),
                    )?),
                    None => None,
                };
                self.iframe_cleanup.insert(
                    session,
                    IframeCleanup {
                        device: iframe.device,
                        frame: iframe.frame,
                        parent_session: iframe.parent_session,
                        resume,
                        detaching: false,
                    },
                );
            }
        }
        Ok(())
    }

    fn mark_iframe_activity_incomplete(&mut self, index: usize) {
        self.devices[index].iframe_activity_incomplete = true;
        self.shared.device(index, |device| {
            device.error = Some(INCOMPLETE_IFRAME_ACTIVITY.into())
        });
    }

    fn fail_iframe_setup(&mut self, session: &str) -> Result<()> {
        if let Some(iframe) = self.iframe_sessions.get(session) {
            let index = iframe.device;
            self.mark_iframe_activity_incomplete(index);
        }
        self.retire_iframe(session)
    }

    fn drain_iframe_cleanup(&mut self) -> Result<()> {
        let sessions: Vec<String> = self.iframe_cleanup.keys().cloned().collect();
        for session in sessions {
            let Some(id) = self.iframe_cleanup[&session].resume else {
                continue;
            };
            if let Some(response) = self.cdp.take_response(id)
                && error_message(&response).is_none()
            {
                self.iframe_cleanup.get_mut(&session).unwrap().resume = None;
            }
            // A failed resume does not prove the renderer is released. Keep
            // this bounded record until target destruction, without retrying
            // resume or detaching a potentially held renderer.
        }
        loop {
            let ready = self
                .iframe_cleanup
                .iter()
                .find(|(session, cleanup)| {
                    !cleanup.detaching
                        && cleanup.resume.is_none()
                        && !self
                            .iframe_cleanup
                            .values()
                            .any(|child| child.parent_session == **session)
                })
                .map(|(session, _)| session.clone());
            let Some(session) = ready else {
                break;
            };
            let cleanup = self.iframe_cleanup.get_mut(&session).unwrap();
            cleanup.detaching = true;
            self.cdp.send_ignored(
                "Target.detachFromTarget",
                json!({"sessionId": session}),
                Some(&cleanup.parent_session),
            )?;
        }
        Ok(())
    }

    fn forget_destroyed_iframe_cleanup(&mut self, session: Option<&str>, frame: Option<&str>) {
        let mut gone: HashSet<String> = self
            .iframe_cleanup
            .iter()
            .filter(|(id, cleanup)| match session {
                Some(session) => session == id.as_str(),
                None => frame == Some(cleanup.frame.as_str()),
            })
            .map(|(session, _)| session.clone())
            .collect();
        loop {
            let before = gone.len();
            for (session, cleanup) in &self.iframe_cleanup {
                if gone.contains(&cleanup.parent_session) {
                    gone.insert(session.clone());
                }
            }
            if before == gone.len() {
                break;
            }
        }
        for session in gone {
            if let Some(cleanup) = self.iframe_cleanup.remove(&session)
                && let Some(id) = cleanup.resume
            {
                self.cdp.abandon(id);
            }
        }
    }

    fn attach_related_target(&mut self, event: &Event) -> Result<()> {
        let Some(parent_session) = event.session.as_deref() else {
            return Ok(());
        };
        let owner = self.session_owner(Some(parent_session));
        let Some(index) = owner.map(|(index, _)| index).or_else(|| {
            self.iframe_cleanup
                .get(parent_session)
                .map(|cleanup| cleanup.device)
        }) else {
            return Ok(());
        };
        let release_only = owner.is_none();
        let Some(info) = event.params.get("targetInfo") else {
            return Ok(());
        };
        let Some(session) = event.params.get("sessionId").and_then(Value::as_str) else {
            return Ok(());
        };
        let Some(frame) = info.get("targetId").and_then(Value::as_str) else {
            return Ok(());
        };
        let kind = info.get("type").and_then(Value::as_str);
        let worker = matches!(kind, Some("worker" | "shared_worker" | "service_worker"));
        if (!worker && kind != Some("iframe"))
            || info
                .get("browserContextId")
                .and_then(Value::as_str)
                .is_some_and(|context| context != self.devices[index].context)
        {
            return Ok(());
        }
        // A worker's context must explicitly match its known parent's context.
        // It must never alias a page/iframe debugger session or frame target.
        if worker
            && (info.get("browserContextId").and_then(Value::as_str)
                != Some(self.devices[index].context.as_str())
                || self
                    .devices
                    .iter()
                    .any(|device| device.session == session || device.target_id == frame)
                || self
                    .iframe_sessions
                    .values()
                    .any(|iframe| iframe.frame == frame))
        {
            return Ok(());
        }
        if self.iframe_sessions.contains_key(session) || self.iframe_cleanup.contains_key(session) {
            return Ok(());
        }
        if session.is_empty()
            || session.len() > 128
            || frame.is_empty()
            || frame.len() > 128
            || (worker
                && (session.chars().any(char::is_control) || frame.chars().any(char::is_control)))
            || self.iframe_sessions.len() + self.iframe_cleanup.len() >= MAX_IFRAME_SESSIONS
        {
            bail!("CDP target session limit exceeded or invalid session identity");
        }
        let parent_session = event.session.as_ref().unwrap().clone();
        if release_only || worker {
            let resume =
                self.cdp
                    .send("Runtime.runIfWaitingForDebugger", json!({}), Some(session))?;
            self.iframe_cleanup.insert(
                session.to_owned(),
                IframeCleanup {
                    device: index,
                    frame: frame.to_owned(),
                    parent_session,
                    resume: Some(resume),
                    detaching: false,
                },
            );
            return Ok(());
        }
        let parent = self
            .iframe_sessions
            .get(&parent_session)
            .map(|iframe| iframe.frame.clone())
            .unwrap_or(self.devices[index].target_id.clone());
        if !self.owns_frame(index, frame) {
            let dom_parent = info
                .get("parentFrameId")
                .and_then(Value::as_str)
                .filter(|parent| self.owns_frame(index, parent))
                .unwrap_or(&parent);
            self.remember_frame(index, frame, dom_parent)?;
        }
        // A new session can replace the renderer for an existing frame. Retire
        // its old debugger sessions, but keep the frame identity across swaps.
        let previous: Vec<String> = self
            .iframe_sessions
            .iter()
            .filter(|(_, iframe)| iframe.frame == frame)
            .map(|(session, _)| session.clone())
            .collect();
        for old in previous {
            self.retire_iframe(&old)?;
        }
        self.iframe_sessions.insert(
            session.to_owned(),
            IframeSession {
                device: index,
                frame: frame.to_owned(),
                parent_session,
                waiting: event
                    .params
                    .get("waitingForDebugger")
                    .and_then(Value::as_bool)
                    == Some(true),
                deadline: Instant::now() + self.limits.command,
            },
        );
        self.send_iframe_setup(session, IframeSetup::Enable)
    }

    fn send_iframe_setup(&mut self, session: &str, stage: IframeSetup) -> Result<()> {
        let (method, params) = match stage {
            IframeSetup::Enable => ("Page.enable", json!({})),
            IframeSetup::Intercept => (
                "Page.setInterceptFileChooserDialog",
                json!({"enabled": true, "cancel": true}),
            ),
            IframeSetup::AutoAttach => ("Target.setAutoAttach", iframe_auto_attach()),
            IframeSetup::FrameTree => ("Page.getFrameTree", json!({})),
            IframeSetup::Resume => ("Runtime.runIfWaitingForDebugger", json!({})),
        };
        let index = self.iframe_sessions[session].device;
        let id = self.cdp.send(method, params, Some(session))?;
        self.iframe_pending.insert(
            id,
            IframeCommand {
                session: session.to_owned(),
                stage,
                deadline: self.iframe_sessions[session].deadline,
                revision: self.devices[index].frame_revision,
            },
        );
        Ok(())
    }

    fn drain_iframe_setup(&mut self) -> Result<()> {
        let ready: Vec<u64> = self.iframe_pending.keys().copied().collect();
        for id in ready {
            let Some(response) = self.cdp.take_response(id) else {
                continue;
            };
            let pending = self.iframe_pending.remove(&id).unwrap();
            let Some(iframe) = self.iframe_sessions.get(&pending.session) else {
                continue;
            };
            let (index, frame) = (iframe.device, iframe.frame.clone());
            if error_message(&response).is_some() {
                self.fail_iframe_setup(&pending.session)?;
                continue;
            }
            let next = match pending.stage {
                IframeSetup::Enable => IframeSetup::Intercept,
                IframeSetup::Intercept => IframeSetup::AutoAttach,
                IframeSetup::AutoAttach => IframeSetup::FrameTree,
                IframeSetup::FrameTree => {
                    // A remove/navigation event after the request makes its old
                    // tree obsolete; it must never resurrect old ownership.
                    let stale = self.devices[index].frame_revision != pending.revision;
                    if !stale
                        && let Some(tree) = response.pointer("/result/frameTree")
                        && tree.pointer("/frame/id").and_then(Value::as_str) == Some(&frame)
                    {
                        self.remember_frame_tree(index, tree)?;
                    }
                    if stale {
                        // Repeat only this read-only inventory after a concurrent
                        // lifecycle change, within the original setup deadline.
                        IframeSetup::FrameTree
                    } else {
                        IframeSetup::Resume
                    }
                }
                IframeSetup::Resume => {
                    self.iframe_sessions
                        .get_mut(&pending.session)
                        .unwrap()
                        .waiting = false;
                    continue;
                }
            };
            self.send_iframe_setup(&pending.session, next)?;
        }
        Ok(())
    }

    fn remember_frame_tree(&mut self, index: usize, tree: &Value) -> Result<()> {
        let mut trees = vec![tree];
        let mut seen = 0;
        while let Some(tree) = trees.pop() {
            seen += 1;
            if seen > MAX_TRACKED_FRAMES + 1 {
                self.mark_iframe_activity_incomplete(index);
                return Ok(());
            }
            if let Some(frame) = tree.pointer("/frame/id").and_then(Value::as_str)
                && let Some(loader) = tree.pointer("/frame/loaderId").and_then(Value::as_str)
            {
                if loader.len() > 128 {
                    bail!("invalid CDP frame loader identity");
                }
                if let Some(owner) = self.devices[index].frames.get_mut(frame) {
                    owner.loader = Some(loader.to_owned());
                }
            }
            if let Some(parent) = tree.pointer("/frame/id").and_then(Value::as_str)
                && let Some(children) = tree.get("childFrames").and_then(Value::as_array)
            {
                if trees.len() + children.len() > MAX_TRACKED_FRAMES + 1 {
                    self.mark_iframe_activity_incomplete(index);
                    return Ok(());
                }
                for child in children {
                    if let Some(frame) = child.pointer("/frame/id").and_then(Value::as_str) {
                        self.remember_frame(index, frame, parent)?;
                        trees.push(child);
                    }
                }
            }
        }
        Ok(())
    }

    fn chooser_session(&self, index: usize, frame: &str) -> &str {
        let mut next = frame;
        for _ in 0..=MAX_TRACKED_FRAMES {
            if let Some((session, _)) = self
                .iframe_sessions
                .iter()
                .find(|(_, iframe)| iframe.device == index && iframe.frame == next)
            {
                return session;
            }
            let Some(owner) = self.devices[index].frames.get(next) else {
                break;
            };
            next = &owner.parent;
        }
        &self.devices[index].session
    }

    fn observe_frame_lifecycle(&mut self, index: usize, event: &Event) -> Result<bool> {
        let text = |field: &str| event.params.get(field).and_then(Value::as_str);
        let session = event.session.as_deref().unwrap();
        match event.method.as_str() {
            "Page.frameAttached" => {
                if let (Some(frame), Some(parent)) = (text("frameId"), text("parentFrameId"))
                    && self.frame_in_session(index, parent, session)
                {
                    self.remember_frame(index, frame, parent)?;
                }
            }
            "Page.frameDetached" => {
                if text("reason") == Some("remove")
                    && let Some(frame) = text("frameId")
                    && self.frame_in_session(index, frame, session)
                {
                    self.remove_frame_subtree(index, Some(frame))?;
                }
                // "swap" retains the ownership of this frame.
            }
            "Page.frameNavigated" => {
                let Some(frame) = event.params.get("frame") else {
                    return Ok(true);
                };
                let Some(id) = frame.get("id").and_then(Value::as_str) else {
                    return Ok(true);
                };
                if id == self.devices[index].target_id {
                    return Ok(false);
                }
                if let Some(parent) = frame.get("parentId").and_then(Value::as_str)
                    && self.frame_in_session(index, parent, session)
                {
                    self.remember_frame(index, id, parent)?;
                }
                if self.frame_in_session(index, id, session)
                    && let Some(loader) = frame.get("loaderId").and_then(Value::as_str)
                {
                    if loader.len() > 128 {
                        bail!("invalid CDP frame loader identity");
                    }
                    let changed = self.devices[index]
                        .frames
                        .get(id)
                        .and_then(|frame| frame.loader.as_deref())
                        .is_some_and(|old| old != loader);
                    if changed {
                        self.clear_frame_children(index, id)?;
                    }
                    if let Some(frame) = self.devices[index].frames.get_mut(id) {
                        frame.loader = Some(loader.to_owned());
                    }
                }
            }
            "Page.fileChooserOpened" => {
                if let Some(frame) = text("frameId")
                    && self.frame_in_session(index, frame, session)
                    && self.chooser_session(index, frame) == session
                {
                    self.shared.device(index, |device| {
                        device.file_choosers = device.file_choosers.saturating_add(1);
                    });
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
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
                // A page with an opener in a Broxser session is a window a
                // page opened. Nothing shows it, so it would run unseen with
                // the session's cookies and could navigate its opener. It is
                // closed at once and reported (ADR 0015); Broxser's own
                // targets have no opener.
                if event.method == "Target.targetCreated"
                    && info.get("type").and_then(Value::as_str) == Some("page")
                    && let Some(opener) = info.get("openerId").and_then(Value::as_str)
                    && let Some(target) = info.get("targetId").and_then(Value::as_str)
                    && info
                        .get("browserContextId")
                        .and_then(Value::as_str)
                        .is_some_and(|context| self.contexts.contains(context))
                {
                    self.cdp.send_detached(
                        "Target.closeTarget",
                        json!({"targetId": target}),
                        None,
                    )?;
                    if let Some(index) = self
                        .devices
                        .iter()
                        .position(|device| device.target_id == opener)
                    {
                        self.popup_closed(index)?;
                    }
                }
                return Ok(());
            }
            "Target.attachedToTarget" => {
                self.attach_related_target(&event)?;
                return Ok(());
            }
            "Target.targetCrashed" | "Target.targetDestroyed" | "Target.detachedFromTarget" => {
                let sessions: Vec<String> = self
                    .iframe_sessions
                    .iter()
                    .filter(|(session, iframe)| match text("sessionId") {
                        Some(detached) => detached == session.as_str(),
                        None => text("targetId") == Some(iframe.frame.as_str()),
                    })
                    .map(|(session, _)| session.clone())
                    .collect();
                for session in sessions {
                    if let Some(iframe) = self.iframe_sessions.get(&session) {
                        let (index, frame) = (iframe.device, iframe.frame.clone());
                        if event.method != "Target.detachedFromTarget" {
                            self.remove_frame_subtree(index, Some(&frame))?;
                        } else {
                            self.clear_frame_children(index, &frame)?;
                        }
                    }
                    self.retire_iframe(&session)?;
                }
                self.forget_destroyed_iframe_cleanup(text("sessionId"), text("targetId"));
                if let Some(index) = self.devices.iter().position(|device| {
                    Some(device.target_id.as_str()) == text("targetId")
                        || Some(device.session.as_str()) == text("sessionId")
                }) {
                    let crashed = event.method == "Target.targetCrashed";
                    // The renderer or the session that owed these answers is
                    // gone; the error below describes the device instead.
                    self.remove_frame_subtree(index, None)?;
                    self.forget_input(index);
                    self.forget_navigation(index);
                    self.invalidate_ime(index);
                    self.devices[index].streaming = false;
                    self.devices[index].dialog = None;
                    self.shared.device(index, |device| {
                        device.loading = false;
                        device.streaming = false;
                        device.dialog = None;
                        device.error = Some(if crashed {
                            "The page crashed. Reload the device to start a new renderer.".into()
                        } else {
                            "The page target was detached.".into()
                        });
                    });
                }
                return Ok(());
            }
            // Every session context denies downloads, so the browser refuses
            // this one; the device whose frame started it reports it (ADR 0016).
            "Browser.downloadWillBegin" => {
                if let Some(frame) = text("frameId")
                    && let Some(index) = self.devices.iter().position(|device| {
                        device.target_id == frame || device.frames.contains_key(frame)
                    })
                {
                    let download = DownloadState {
                        filename: display_line(text("suggestedFilename").unwrap_or_default()),
                        url: display_line(text("url").unwrap_or_default()),
                    };
                    self.shared.device(index, |device| {
                        device.downloads = device.downloads.saturating_add(1);
                        device.download = Some(download);
                    });
                }
                return Ok(());
            }
            _ => {}
        }
        let Some((index, iframe)) = self.session_owner(event.session.as_deref()) else {
            return Ok(());
        };
        if self.observe_frame_lifecycle(index, &event)? || iframe {
            // Iframe sessions never feed top-level navigation, input, sync,
            // dialogs or screencast state.
            return Ok(());
        }
        match event.method.as_str() {
            "Runtime.executionContextCreated" => {
                if let Some(context) = params.get("context")
                    && context.pointer("/auxData/frameId").and_then(Value::as_str)
                        == Some(self.devices[index].target_id.as_str())
                    && context.pointer("/auxData/type").and_then(Value::as_str) == Some("isolated")
                {
                    let id = context
                        .get("id")
                        .and_then(Value::as_i64)
                        .filter(|id| *id > 0);
                    match context.get("name").and_then(Value::as_str) {
                        Some(LINK_WORLD) => self.devices[index].link_context = id,
                        Some(IME_WORLD) => {
                            self.invalidate_ime(index);
                            self.devices[index].ime_context = id;
                            self.devices[index].ime_blocked_anchor = None;
                        }
                        _ => {}
                    }
                }
            }
            "Runtime.executionContextDestroyed" => {
                if self.devices[index].ime_context.is_some_and(|id| {
                    params.get("executionContextId").and_then(Value::as_i64) == Some(id)
                }) {
                    self.devices[index].ime_context = None;
                    self.invalidate_ime(index);
                }
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
                self.devices[index].ime_context = None;
                self.invalidate_ime(index);
            }
            "Runtime.bindingCalled"
                if text("name") == Some(IME_BINDING)
                    && self.devices[index].ime_context.is_some_and(|id| {
                        params.get("executionContextId").and_then(Value::as_i64) == Some(id)
                    })
                    && self.devices[index].visible =>
            {
                match text("payload")
                    .and_then(|payload| parse_caret_report(payload, self.devices[index].css))
                {
                    None => self.clear_ime(index, true)?,
                    Some(None) => self.clear_ime(index, true)?,
                    Some(Some((anchor, caret))) => {
                        if self.devices[index]
                            .ime_blocked_anchor
                            .is_some_and(|blocked| anchor <= blocked)
                        {
                            return Ok(());
                        }
                        self.devices[index].ime_blocked_anchor = None;
                        if self.devices[index].ime_anchor != Some(anchor) {
                            self.clear_ime(index, true)?;
                            self.next_ime_target = self
                                .next_ime_target
                                .checked_add(1)
                                .ok_or_else(|| anyhow!("IME target token exhausted"))?;
                            self.devices[index].ime_anchor = Some(anchor);
                            self.devices[index].ime_target = Some(self.next_ime_target);
                        }
                        let target = self.devices[index].ime_target.unwrap();
                        self.shared.device(index, |status| {
                            status.text_input = Some(TextInputState { target, caret });
                        });
                    }
                }
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
                self.clear_ime(index, true)?;
                let sync_navigation = self.sync.navigation;
                // A new tab, a new window or a download leaves the page as it is.
                let current_tab = text("disposition").is_none_or(|value| value == "currentTab");
                if current_tab {
                    self.invalidate_touch(index);
                }
                let device = &mut self.devices[index];
                if current_tab && let Some(navigation) = &mut device.navigation {
                    navigation.page_requested = true;
                }
                let url = text("url");
                device.requested_link = device.link_intent.as_ref().and_then(|intent| {
                    (sync_navigation
                        && device.visible
                        && text("reason") == Some("anchorClick")
                        && current_tab
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
                self.invalidate_touch(index);
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
                let window = self.limits.link_follow;
                let device = &mut self.devices[index];
                device.requested_link = None;
                device.link_navigation = None;
                // A hash link, or a router's History API or Navigation API
                // change, reaching the URL of the latest trusted link activation
                // follows it (ADR 0013). A change to another URL, such as a
                // router saving state on the current entry first, keeps it.
                let followed = device
                    .link_intent
                    .as_ref()
                    .is_some_and(|intent| intent.url == url)
                    && device.link_intent.take().is_some_and(|intent| {
                        intent.generation == device.generation && intent.at.elapsed() <= window
                    });
                self.shared.device(index, |device| device.url = url.clone());
                if self.sync.navigation && followed && self.devices[index].visible {
                    self.sync_navigation(index, url)?;
                }
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
                let field = |name: &str| frame.get(name).and_then(Value::as_str);
                // CDP reports the fragment apart from `url`.
                let url = format!(
                    "{}{}",
                    field("url").unwrap_or_default(),
                    field("urlFragment").unwrap_or_default()
                );
                // A new document answers input again; the old one's is moot,
                // and so is a dialog of the old one. Its frames went with it.
                self.forget_input(index);
                self.invalidate_ime(index);
                self.devices[index].dialog = None;
                self.remove_frame_subtree(index, None)?;
                self.devices[index].iframe_activity_incomplete = false;
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
                // The link's own loader committed, possibly after server
                // redirects. Peers load the link and follow their own
                // redirects; a redirect target, which can carry a code or a
                // token, never reaches them (ADR 0013). An error page is not
                // a destination.
                let link = device.link_navigation.take().filter(|link| {
                    link.confirmed
                        && field("loaderId") == Some(link.loader.as_str())
                        && frame.get("unreachableUrl").is_none()
                });
                device.link_intent = None;
                device.requested_link = None;
                if device.visible && !device.streaming {
                    // A crashed renderer is replaced on navigation; resume its stream.
                    self.start_stream(index)?;
                }
                // The browser's error page for a navigation Broxser started
                // shows the failure; its report stays until a document commits.
                let error_page = frame.get("unreachableUrl").is_some();
                self.shared.device(index, |device| {
                    device.url = url;
                    let failed = device
                        .error
                        .as_deref()
                        .is_some_and(|error| error.starts_with(NAVIGATION_FAILED));
                    if !(error_page && failed) {
                        device.error = None;
                    }
                    device.dialog = None;
                });
                if self.sync.navigation
                    && self.devices[index].visible
                    && let Some(link) = link
                {
                    self.sync_navigation(index, link.url)?;
                }
            }
            "Page.javascriptDialogOpening" => {
                // Input sent during the dialog would be replayed after its
                // answer. Retire the finger locally and cancel only before a
                // fresh press once the page can receive input again.
                self.invalidate_touch(index);
                let kind = match text("type") {
                    Some("alert") => DialogKind::Alert,
                    Some("confirm") => DialogKind::Confirm,
                    Some("prompt") => DialogKind::Prompt,
                    Some("beforeunload") => DialogKind::BeforeUnload,
                    _ => {
                        // Broxser has no answer to offer. It does not block
                        // input or navigation; Reload cancels the dialog.
                        self.shared.device(index, |device| {
                            device.error = Some(UNKNOWN_DIALOG.to_owned());
                        });
                        return Ok(());
                    }
                };
                self.next_dialog = self
                    .next_dialog
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("dialog token exhausted"))?;
                let token = self.next_dialog;
                let state = DialogState {
                    kind,
                    message: dialog_text(text("message").unwrap_or_default()),
                    default_text: if kind == DialogKind::Prompt {
                        prompt_text(text("defaultPrompt").unwrap_or_default())
                    } else {
                        String::new()
                    },
                    token,
                };
                // Helium reports the page's own navigation request (a link, a
                // script, location.reload()) before that navigation asks, and a
                // history navigation of the page cancels Broxser's navigation
                // before it asks. A page navigation reported neither way, or
                // requested just before Broxser's was sent, is taken for Broxser's.
                let for_navigation = kind == DialogKind::BeforeUnload
                    && self.devices[index]
                        .navigation
                        .as_ref()
                        .is_some_and(|navigation| !navigation.page_requested);
                let device = &mut self.devices[index];
                device.dialog = Some(OpenDialog {
                    token,
                    kind,
                    answer: None,
                    for_navigation,
                });
                // The page stops until the user answers: input held for it
                // would arrive out of context.
                device.pointer_move = None;
                device.wheel = None;
                device.held_input.clear();
                device.waiting_since = None;
                // So would a composition cancel, which the page would hold as
                // well; it deletes the selection if no composition is left by
                // then. The composition and the caret are dropped unsent.
                self.invalidate_ime(index);
                self.shared
                    .device(index, |device| device.dialog = Some(state));
            }
            "Page.javascriptDialogClosed" => {
                let accepted = params.get("result").and_then(Value::as_bool) == Some(true);
                self.dialog_closed(index, accepted)?;
            }
            "Page.windowOpen" => {
                self.devices[index].window_open = text("url").map(str::to_owned);
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
                self.navigate(target, url, false)?;
            }
        }
        Ok(())
    }
}

/// Whether a pointer event is the finger of a touch device: the left button
/// pressed, released, or held while moving.
fn is_touch(event: PointerEvent) -> bool {
    match event.kind {
        PointerKind::Move => event.buttons & 1 != 0,
        PointerKind::Down | PointerKind::Up => event.button == PointerButton::Left,
    }
}

/// The CDP input method and parameters for a pointer event: a single touch
/// point on a touch device, a mouse event otherwise.
fn pointer_params(event: PointerEvent, css: (f64, f64), touch: bool) -> (&'static str, Value) {
    if !touch {
        return ("Input.dispatchMouseEvent", mouse_params(event, css));
    }
    let (x, y) = pointer_position(event, css);
    let point = json!({"x": x, "y": y});
    let (kind, points) = match event.kind {
        PointerKind::Down => ("touchStart", vec![point]),
        PointerKind::Move => ("touchMove", vec![point]),
        PointerKind::Up => ("touchEnd", Vec::new()),
    };
    (
        "Input.dispatchTouchEvent",
        json!({"type": kind, "touchPoints": points, "modifiers": event.modifiers.cdp()}),
    )
}

fn pointer_position(event: PointerEvent, css: (f64, f64)) -> (f64, f64) {
    (
        event.x.clamp(0.0, css.0 - 1.0),
        event.y.clamp(0.0, css.1 - 1.0),
    )
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
