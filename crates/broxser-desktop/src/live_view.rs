//! Live device canvas. Frames are CDP screencast JPEGs from a headless browser,
//! decoded off the UI thread into GPUI images: frame streaming, not an embedded
//! browser surface. Pointer and wheel input go to the device under the pointer
//! and keys to the selected device, in CSS pixels of that device's viewport.

use crate::ime::{ImeBuffer, Origin};
use crate::lifecycle::{AfterStop, CloseRequest, Lifecycle};
use crate::report;
use crate::theme::{
    self, ACCENT, ACCENT_SOFT, BORDER, BORDER_STRONG, CANVAS, CARD, CHROME, DANGER, DANGER_TEXT,
    DIALOG_FILL, DIVIDER, FAINT, HOVER, INFO, INK, Icon, MUTED, TEXT, TEXT_2, Tone, WARN,
    WARN_TEXT,
};
use crate::url_input::{UrlEvent, UrlInput};
use crate::{FocusUrl, Quit, Refresh, ToggleConsole, TogglePanel};
use anyhow::{Context as _, Result};
use broxser_core::{AppState, PRESETS, WindowSize, Workspace, validate_url};
use broxser_engine::{
    BrowserOptions, Cancellation, Command, ConsoleEntry, ConsoleKind, ConsoleLevel, ConsoleScope,
    DeviceStatus, DialogKind, DialogState, DownloadState, Frame, ImeAction, KeyInput, LiveSession,
    MAX_DIALOG_CHARS, MAX_PASTE_CHARS, Modifiers, PasteRejected, PointerButton, PointerEvent,
    PointerKind, PopupState, RuntimeState, Screenshot, Status, SyncSettings, is_paste_key,
    paste_text, to_viewport,
};
use futures::StreamExt as _;
use futures::channel::{mpsc, oneshot};
use gpui::{
    AnyElement, Bounds, Context, Corners, ElementInputHandler, Entity, EntityInputHandler,
    FocusHandle, Focusable, KeyDownEvent, KeyUpEvent, Keystroke, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, RenderImage, ScrollWheelEvent, SharedString,
    Subscription, UTF16Selection, Window, canvas, div, prelude::*, px, rgb, rgba,
};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

/// UI pixels per wheel line when the platform reports lines instead of pixels.
const WHEEL_LINE: f32 = 40.0;
/// Widest dialog or download panel, in UI pixels. It starts at the frame's left edge and
/// the canvas does not scroll sideways, so the panel of a wide device stays
/// readable in a half-width window.
const PANEL_WIDTH: f32 = 360.0;
/// The device list left of the canvas.
const SIDEBAR_WIDTH: f32 = 256.0;
/// The Workspace or Console panel right of the canvas.
const INSPECTOR_WIDTH: f32 = 380.0;
/// Corner radius of a device frame, its image included.
const FRAME_RADIUS: f32 = 6.0;

pub(crate) struct LiveView {
    workspace: Workspace,
    /// The workspace as edited in the panel; the next restart runs it.
    draft: Workspace,
    /// The file the workspace came from, where Save writes; the demo has none.
    workspace_path: Option<PathBuf>,
    /// The application state file that keeps the window size (ADR 0022).
    state_path: Option<PathBuf>,
    /// The panel beside the device list, if one is open.
    panel: Option<SidePanel>,
    /// The last outcome of a workspace panel action, shown in that panel.
    panel_notice: Option<String>,
    /// Only one workspace snapshot may be written at a time.
    saving_workspace: bool,
    /// The console of the selected device as the Console panel last read it,
    /// with the device and revision it belongs to (ADR 0023).
    console: Vec<ConsoleEntry>,
    console_read: Option<(usize, u64)>,
    /// Where Save report writes (ADR 0024); `None` without a home directory.
    reports: Option<PathBuf>,
    /// Resolving XDG paths may read a slow filesystem; never do it on the GUI.
    reports_loading: bool,
    /// The report waiting for its screenshot.
    report: Option<PendingReport>,
    /// A captured report owns a close-time save until both files are written.
    saving_report: bool,
    next_report: u64,
    /// The outcome of the last Save report, and whether it failed.
    report_notice: Option<(String, bool)>,
    browser: Option<PathBuf>,
    session: Option<LiveSession>,
    status: Status,
    devices: Vec<DeviceView>,
    selected: Option<usize>,
    pressed: PressedKeys,
    ignored_ime_keys: HashSet<String>,
    /// Keys sent to pages, retained so a release never lands on another device.
    held_keys: HashMap<String, (usize, KeyInput)>,
    ime: ImeBuffer,
    url: Entity<UrlInput>,
    /// Display pixels per CSS pixel.
    scale: f32,
    /// Window scale factor of the frame limits last sent.
    limit_scale: f32,
    sync: SyncSettings,
    focus: FocusHandle,
    /// Restart and close, one at a time, and the current runtime's generation.
    lifecycle: Lifecycle,
    notice: Option<String>,
    _url_events: Subscription,
    _focus_out: Subscription,
    _window_activation: Subscription,
    _window_bounds: Subscription,
    _key_presses: Subscription,
}

/// Keys down in the window, whoever handled the press, in press order. GPUI
/// reports neither physical keys nor, on X11, repeats.
#[derive(Default)]
struct PressedKeys {
    keys: Vec<PressedKey>,
    /// The latest key-down: only that key auto-repeats.
    latest: Option<String>,
}

struct PressedKey {
    /// As [`logical_key_identity`] names the press.
    identity: String,
    /// Digits, symbols, dead keys and composed characters can be released
    /// under another name; letters and named keys cannot.
    renamable: bool,
    /// Its latest key-down repeated the one before: an auto-repeat.
    repeating: bool,
}

impl PressedKeys {
    fn forget(&mut self, identity: &str) {
        self.keys.retain(|pressed| pressed.identity != identity);
        if self.latest.as_deref() == Some(identity) {
            self.latest = None;
        }
    }
    /// Records a key-down. It repeats the key only if that key had the latest
    /// key-down; otherwise it is a new press, even under a name still down
    /// whose release went unseen.
    fn press(&mut self, identity: String, key: &KeyInput) {
        let repeating = self.latest.as_deref() == Some(identity.as_str());
        match self.keys.iter_mut().find(|down| down.identity == identity) {
            Some(down) => down.repeating = repeating,
            None => self.keys.push(PressedKey {
                identity: identity.clone(),
                renamable: key.code.is_empty() || key.code.starts_with("Digit"),
                repeating,
            }),
        }
        self.latest = Some(identity);
    }

    /// Whether the latest key-down of `identity` repeated its press.
    fn repeating(&self, identity: &str) -> bool {
        self.keys
            .iter()
            .any(|down| down.identity == identity && down.repeating)
    }

    /// Whether the latest key-down in the window was an auto-repeat.
    fn latest_repeats(&self) -> bool {
        self.latest
            .as_deref()
            .is_some_and(|identity| self.repeating(identity))
    }

    /// Removes and returns the press a key-up ends: the key down under the
    /// same name, or else the most recently pressed key that can be released
    /// under another name, such as `/` released as `7` on a German layout
    /// after Shift.
    fn release(&mut self, identity: &str) -> Option<String> {
        let index = self
            .keys
            .iter()
            .position(|down| down.identity == identity)
            .or_else(|| self.keys.iter().rposition(|down| down.renamable))?;
        let released = self.keys.remove(index).identity;
        if self.latest.as_deref() == Some(released.as_str()) {
            self.latest = None;
        }
        Some(released)
    }
}

#[derive(Default)]
struct DeviceView {
    image: Option<Arc<RenderImage>>,
    /// Generation of the frame being decoded, if any.
    decoding: Option<u64>,
    /// Frame bounds from the last paint, used to map pointer positions.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    on_screen: Rc<OnScreen>,
    hidden: bool,
    /// A queued engine snapshot must not restore the caret preceding a click.
    invalidated_ime_target: Option<u64>,
    /// Held buttons as a CDP bitmask, and the last mapped pointer position.
    buttons: u8,
    last_point: Option<(f64, f64)>,
    /// Text field of the open prompt dialog; it lives while that dialog does.
    prompt: Option<PromptField>,
    /// Token of the closed-window report the user dismissed.
    dismissed_popup: Option<u64>,
    /// The device's download count when the user dismissed its report; the
    /// next refused download shows again.
    dismissed_download: Option<u32>,
}

/// A report as it was when the user asked for it; its screenshot follows.
struct PendingReport {
    device: usize,
    token: u64,
    console: Vec<ConsoleEntry>,
    errors: u32,
    warnings: u32,
    url: String,
    browser: Option<(String, String)>,
    saved: SystemTime,
}

/// The panel beside the device list; one at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidePanel {
    Workspace,
    /// The selected device's console (ADR 0023).
    Console,
}

struct PromptField {
    token: u64,
    input: Entity<UrlInput>,
    focus: FocusHandle,
    /// Invalidates a stale canvas IME mark when this field takes focus, by
    /// the auto-focus after it is created or by a later click: the field is
    /// a descendant of the canvas's own focus handle, so neither counts as
    /// leaving it and the usual focus-out handling never sees the change.
    _focus_in: Subscription,
    /// Enter in the field accepts the prompt with its text, Escape cancels it.
    _events: Subscription,
}

/// Where keyboard focus is, as far as the device canvas is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyFocus {
    /// The canvas itself: keys go to the selected page.
    Canvas,
    /// The text field of device `index`'s prompt dialog, inside the canvas.
    Prompt(usize),
    /// The URL bar, or nothing.
    Elsewhere,
}

/// Whether the last paint showed part of a device frame in the scrolled canvas,
/// and what the runtime was last told. The runtime pauses the screencast of a
/// device that is not on screen. Both start true, as the runtime does.
struct OnScreen {
    painted: Cell<bool>,
    sent: Cell<bool>,
    /// A paint has scheduled [`LiveView::sync_on_screen`] for this device.
    pending: Cell<bool>,
}

impl Default for OnScreen {
    fn default() -> Self {
        Self {
            painted: Cell::new(true),
            sent: Cell::new(true),
            pending: Cell::new(false),
        }
    }
}

impl DeviceView {
    fn ime_target(&self, status: &broxser_engine::DeviceStatus) -> Option<u64> {
        let target = status.text_input?.target;
        (!self.hidden && self.invalidated_ime_target != Some(target)).then_some(target)
    }

    fn invalidate_ime_target(&mut self, status: &mut broxser_engine::DeviceStatus) {
        if let Some(caret) = status.text_input.take() {
            self.invalidated_ime_target = Some(caret.target);
        }
    }
}

impl LiveView {
    pub(crate) fn new(
        workspace: Workspace,
        workspace_path: Option<PathBuf>,
        state_path: Option<PathBuf>,
        browser: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let url = cx.new(|cx| UrlInput::address(workspace.url.clone(), cx));
        let url_events = cx.subscribe_in(&url, window, |view, _, event, window, cx| match event {
            UrlEvent::Submit(text) => view.navigate(text.trim(), window, cx),
            UrlEvent::Cancel => window.blur(),
        });
        let focus = cx.focus_handle();
        focus.focus(window);
        let focus_out = cx.on_focus_out(&focus, window, |view, _, window, _| {
            view.release_keys();
            view.cancel_touches();
            view.invalidate_ime();
            window.invalidate_character_coordinates();
        });
        let window_activation = cx.observe_window_activation(window, |view, window, _| {
            if !window.is_window_active() {
                view.release_keys();
                view.cancel_touches();
                view.invalidate_ime();
                window.invalidate_character_coordinates();
            }
        });
        // Moving to a display with another scale changes the physical size of
        // every frame; GPUI reports it as a bounds change.
        let window_bounds = cx.observe_window_bounds(window, |view, window, _| {
            if window.scale_factor() != view.limit_scale {
                view.send_frame_limits(window);
            }
        });
        // Before shortcuts and elements, so every press pairs with its release.
        let pressing = cx.entity().downgrade();
        let key_presses = cx.intercept_keystrokes(move |event, _, cx| {
            let _ = pressing.update(cx, |view, _| view.record_press(&event.keystroke));
        });
        let entity = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            entity
                .update(cx, |view: &mut LiveView, cx| view.request_close(window, cx))
                .unwrap_or(true)
        });
        let mut view = Self {
            devices: workspace
                .devices
                .iter()
                .map(|_| DeviceView::default())
                .collect(),
            draft: workspace.clone(),
            workspace,
            workspace_path,
            state_path,
            panel: None,
            panel_notice: None,
            saving_workspace: false,
            console: Vec::new(),
            console_read: None,
            reports: None,
            reports_loading: true,
            report: None,
            saving_report: false,
            next_report: 0,
            report_notice: None,
            browser,
            session: None,
            status: Status::default(),
            selected: Some(0),
            pressed: PressedKeys::default(),
            ignored_ime_keys: HashSet::new(),
            held_keys: HashMap::new(),
            ime: ImeBuffer::default(),
            url,
            scale: 0.5,
            limit_scale: window.scale_factor(),
            sync: SyncSettings::default(),
            focus,
            lifecycle: Lifecycle::default(),
            notice: None,
            _url_events: url_events,
            _focus_out: focus_out,
            _window_activation: window_activation,
            _window_bounds: window_bounds,
            _key_presses: key_presses,
        };
        view.resolve_reports(window, cx);
        view.start(window, cx);
        view
    }

    fn resolve_reports(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let resolved = save_off_thread(|| report::reports_dir(|name| std::env::var_os(name)));
        cx.spawn_in(window, async move |this, cx| {
            let resolved = resolved.await;
            this.update(cx, |view, cx| {
                view.reports_loading = false;
                match resolved {
                    Ok(path) => {
                        view.reports = path;
                        view.report_notice = None;
                    }
                    Err(error) => {
                        view.report_notice =
                            Some((format!("Reports unavailable: {error:#}"), true));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Starts a browser for the workspace. Opening loads its URL once per device.
    /// Runs only while no session is held: replacing one would drop a running
    /// session, and wait for its browser, on the UI thread.
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        debug_assert!(self.session.is_none() && self.lifecycle.is_live());
        let Some(executable) = self.browser.clone() else {
            self.notice =
                Some("Helium not found. Set BROXSER_HELIUM_BIN or pass --browser.".into());
            return;
        };
        let (sender, mut wakeups) = mpsc::unbounded::<()>();
        let queued = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&queued);
        let options = BrowserOptions {
            executable,
            headless: true,
            profile_root: None,
            cancel: Cancellation::new(),
        };
        let started = LiveSession::start(self.workspace.clone(), options, move || {
            // Coalesce: at most one wake-up waits for the UI thread.
            if !flag.swap(true, Ordering::AcqRel) {
                let _ = sender.unbounded_send(());
            }
        });
        match started {
            Ok(session) => {
                self.session = Some(session);
                self.status = Status::default();
                self.read_console();
                self.forget_report();
                self.notice = None;
                // A new runtime streams every visible device; the next paint
                // pauses those still off screen.
                for device in &self.devices {
                    device.on_screen.sent.set(true);
                }
                cx.notify();
            }
            Err(error) => {
                self.notice = Some(format!("{error:#}"));
                return;
            }
        }
        self.send_frame_limits(window);
        if self.sync != SyncSettings::default() {
            self.send(Command::SetSync(self.sync));
        }
        let hidden: Vec<usize> = (0..self.devices.len())
            .filter(|&index| self.devices[index].hidden)
            .collect();
        for device in hidden {
            self.send(Command::SetVisible {
                device,
                visible: false,
            });
        }
        let generation = self.lifecycle.generation();
        cx.spawn_in(window, async move |this, cx| {
            while wakeups.next().await.is_some() {
                queued.store(false, Ordering::Release);
                // Wake-ups of a runtime that a restart or close stopped end here.
                let current = this.update_in(cx, |view, window, cx| {
                    let current = view.lifecycle.accepts(generation);
                    if current {
                        view.pull(window, cx);
                    }
                    current
                });
                if !matches!(current, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Copies the runtime status and starts decoding new frames, one per device.
    fn pull(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let generation = self.lifecycle.generation();
        let status = session.status();
        let frames: Vec<(usize, Frame)> = (0..self.devices.len())
            .filter(|&index| self.devices[index].decoding.is_none() && !self.devices[index].hidden)
            .filter_map(|index| Some((index, session.take_frame(index)?)))
            .collect();
        let screenshot = self
            .report
            .as_ref()
            .and_then(|report| session.take_screenshot(report.device))
            .filter(|(token, _)| {
                self.report
                    .as_ref()
                    .is_some_and(|report| report.token == *token)
            });
        if let Some((_, result)) = screenshot {
            let report = self.report.take().unwrap();
            match result {
                Ok(screenshot) => self.write_report(report, screenshot, window, cx),
                Err(reason) => {
                    self.report_notice = Some((format!("Not saved: {reason}"), true));
                    cx.notify();
                }
            }
        }
        if matches!(status.runtime, RuntimeState::Stopped { .. }) && self.report.take().is_some() {
            self.report_notice = Some((
                "Not saved: the runtime stopped before returning the screenshot.".into(),
                true,
            ));
            cx.notify();
        }
        if status != self.status {
            let former_caret = self
                .selected
                .and_then(|index| self.status.devices.get(index))
                .and_then(|device| device.text_input);
            let next_caret = self
                .selected
                .and_then(|index| status.devices.get(index))
                .and_then(|device| device.text_input);
            if self.ime_origin_with_status(&status, window) != self.ime_origin(window) {
                self.invalidate_ime();
            }
            if let Some(url) = self
                .selected
                .and_then(|index| status.devices.get(index))
                .map(|device| device.url.clone())
                .filter(|url| !url.is_empty())
            {
                self.url
                    .update(cx, |input, cx| input.show(&url, window, cx));
            }
            self.status = status;
            self.read_console();
            self.sync_prompts(window, cx);
            if former_caret != next_caret {
                window.invalidate_character_coordinates();
            }
            cx.notify();
        }
        for (index, frame) in frames {
            self.devices[index].decoding = Some(generation);
            let decoded = cx.background_executor().spawn(async move { decode(frame) });
            cx.spawn_in(window, async move |this, cx| {
                let image = decoded.await;
                this.update_in(cx, |view, window, cx| {
                    view.show_frame(index, generation, image, window, cx)
                })
                .ok();
            })
            .detach();
        }
    }

    fn show_frame(
        &mut self,
        index: usize,
        generation: u64,
        image: Result<Arc<RenderImage>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(device) = frame_device(&mut self.devices, &self.lifecycle, index, generation)
        else {
            // A frame of a stopped runtime is released unseen and leaves the
            // newer runtime's decode alone, even if Apply removed its index.
            if let Ok(image) = image {
                let _ = window.drop_image(image);
            }
            return;
        };
        match image {
            // Every frame is a new GPUI image; release the previous atlas texture.
            Ok(image) if !device.hidden => {
                if let Some(previous) = device.image.replace(image) {
                    let _ = window.drop_image(previous);
                }
            }
            Ok(image) => {
                let _ = window.drop_image(image);
            }
            Err(error) => self.notice = Some(format!("{error:#}")),
        }
        cx.notify();
        // A newer frame may have arrived while this one was decoding.
        self.pull(window, cx);
    }

    fn send(&mut self, command: Command) -> bool {
        let accepted = self
            .session
            .as_ref()
            .is_some_and(|session| session.send(command));
        if !accepted {
            self.notice = Some("The live runtime is not accepting commands.".into());
        }
        accepted
    }

    /// Gives each open prompt dialog a text field prefilled with the page's
    /// proposal, and drops the field once its dialog is gone or the runtime no
    /// longer runs. Focus moves as [`focus_after_prompt_change`] decides.
    fn sync_prompts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let held = self.session.is_some();
        for index in 0..self.devices.len() {
            let dialog = open_dialog(&self.status, held, index)
                .filter(|dialog| dialog.kind == DialogKind::Prompt);
            let current = self.devices[index].prompt.as_ref().map(|field| field.token);
            if dialog.map(|dialog| dialog.token) == current {
                continue;
            }
            // Only a token actually changing is worth building the default
            // text for: up to MAX_DIALOG_CHARS, redone on every status change
            // otherwise, though the dialog usually just stays open unchanged.
            let prompt = dialog.map(|dialog| (dialog.token, prompt_default(&dialog.default_text)));
            let focus = self.key_focus(window);
            let field =
                prompt.map(|(token, text)| Self::prompt_field(index, token, text, window, cx));
            let next = focus_after_prompt_change(index, self.selected, focus, field.is_some());
            self.devices[index].prompt = field;
            match next {
                Some(KeyFocus::Canvas) => self.focus.focus(window),
                Some(KeyFocus::Prompt(_)) => {
                    if let Some(field) = &self.devices[index].prompt {
                        // As a click into it would: typing replaces the proposal.
                        field
                            .input
                            .update(cx, |input, cx| input.focus_all(window, cx));
                    }
                }
                _ => {}
            }
        }
    }

    /// The text field of prompt `token` of device `index`. Enter answers with
    /// its text and Escape cancels; either returns keys to the canvas.
    fn prompt_field(
        index: usize,
        token: u64,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PromptField {
        let input = cx.new(|cx| UrlInput::new(text, Some(MAX_DIALOG_CHARS), cx));
        let focus = input.focus_handle(cx);
        let focus_in = cx.on_focus_in(&focus, window, |view, _, _| {
            view.cancel_touches();
            view.invalidate_ime();
        });
        let events = cx.subscribe_in(&input, window, move |view, field, event, window, cx| {
            // The auto-repeat of a key pressed elsewhere, such as the Enter
            // that answered the previous prompt, answers nothing; LineEdit::key
            // already cleared select-all as if this key would submit the
            // field, so put it back rather than leave typing append instead
            // of replace.
            if view.pressed.latest_repeats() {
                field.update(cx, |input, cx| input.restore_select_all(cx));
                return;
            }
            view.answer(field_answer(index, token, event), cx);
            // The field stays until the browser reports the dialog closed;
            // shortcuts and key releases must not wait for that.
            view.focus.focus(window);
        });
        PromptField {
            token,
            input,
            focus,
            _focus_in: focus_in,
            _events: events,
        }
    }

    /// Drops every prompt field; focus in one returns to the canvas.
    fn clear_prompts(&mut self, window: &mut Window) {
        if matches!(self.key_focus(window), KeyFocus::Prompt(_)) {
            self.focus.focus(window);
        }
        for device in &mut self.devices {
            device.prompt = None;
        }
    }

    fn key_focus(&self, window: &Window) -> KeyFocus {
        if self.focus.is_focused(window) {
            return KeyFocus::Canvas;
        }
        self.devices
            .iter()
            .position(|device| {
                device
                    .prompt
                    .as_ref()
                    .is_some_and(|field| field.focus.is_focused(window))
            })
            .map_or(KeyFocus::Elsewhere, KeyFocus::Prompt)
    }

    /// A dialog button's answer to the dialog `token` of device `index`. An
    /// accepted prompt takes its field's text exactly as typed.
    fn answer_dialog(&mut self, index: usize, token: u64, accept: bool, cx: &mut Context<Self>) {
        let text = self
            .devices
            .get(index)
            .and_then(|device| device.prompt.as_ref())
            .filter(|field| accept && field.token == token)
            .map(|field| field.input.read(cx).text().to_owned());
        self.answer(
            Command::AnswerDialog {
                device: index,
                token,
                accept,
                text,
            },
            cx,
        );
    }

    /// Sends the user's answer to a dialog; nothing else answers one.
    fn answer(&mut self, command: Command, cx: &mut Context<Self>) {
        self.notice = None;
        self.send(command);
        cx.notify();
    }

    /// The device's open dialog. Only these buttons, or Enter and Escape in the
    /// prompt's field, answer it (ADR 0014).
    fn dialog_panel(
        &self,
        index: usize,
        dialog: &DialogState,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let token = dialog.token;
        let (title, message): (&str, SharedString) = match dialog.kind {
            DialogKind::Alert => ("The page says", dialog.message.clone().into()),
            DialogKind::Confirm => ("The page asks", dialog.message.clone().into()),
            DialogKind::Prompt => ("The page asks for text", dialog.message.clone().into()),
            DialogKind::BeforeUnload => (
                "Leave this page?",
                "The page may have changes you have not saved.".into(),
            ),
        };
        let button = |id: &'static str, label: &'static str, accept: bool, primary: bool| {
            theme::button(
                (id, index),
                if primary {
                    Tone::Primary
                } else {
                    Tone::Secondary
                },
                28.,
            )
            .when(primary, |this| this.px(px(14.)))
            .child(label)
            .on_click(
                cx.listener(move |view, _, _, cx| view.answer_dialog(index, token, accept, cx)),
            )
        };
        let buttons = match dialog.kind {
            DialogKind::Alert => vec![button("dialog-ok", "OK", true, true)],
            DialogKind::Confirm | DialogKind::Prompt => vec![
                button("dialog-cancel", "Cancel", false, false),
                button("dialog-ok", "OK", true, true),
            ],
            DialogKind::BeforeUnload => vec![
                button("dialog-stay", "Stay", false, true),
                button("dialog-leave", "Leave", true, false),
            ],
        };
        let field = self.devices[index]
            .prompt
            .as_ref()
            .filter(|field| field.token == token)
            .map(|field| field.input.clone());
        div()
            .w(px(width))
            .p(px(11.))
            .rounded(px(10.))
            .border_1()
            .border_color(rgb(WARN))
            .bg(rgb(DIALOG_FILL))
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(WARN_TEXT))
                    .child(theme::icon(Icon::Dialog, 13., WARN_TEXT))
                    .child(title),
            )
            .child(
                // Scrolls: a long message, and the cut mark of one the engine
                // shortened, stay readable.
                div()
                    .id(("dialog-message", token))
                    .text_size(px(12.5))
                    .line_height(px(18.))
                    .max_h(px(120.))
                    .overflow_y_scroll()
                    .child(message),
            )
            .when_some(field, |this, field| this.child(div().flex().child(field)))
            // At the panel's left edge the answers stay in the canvas, which
            // does not scroll sideways, whatever the window width.
            .child(div().flex().gap(px(6.)).children(buttons))
            .into_any_element()
    }

    fn navigate(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let url = if text.contains("://") {
            text.to_owned()
        } else {
            format!("http://{text}")
        };
        match validate_url(&url) {
            Ok(()) => {
                self.invalidate_ime();
                self.notice = None;
                self.focus.focus(window);
                self.send(Command::NavigateAll { url });
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
        cx.notify();
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        // A hidden row has its own Show action; selecting its label does not
        // make it a keyboard target while its page is hidden.
        if self.devices.get(index).is_none_or(|device| device.hidden) {
            return;
        }
        if self.selected != Some(index) {
            self.invalidate_ime();
            if !self.release_keys() {
                return;
            }
            if let Some(previous) = self.selected
                && !self.release_buttons(previous)
            {
                return;
            }
        }
        self.selected = Some(index);
        self.read_console();
        self.focus.focus(window);
        if let Some(url) = self
            .status
            .devices
            .get(index)
            .map(|device| device.url.clone())
            .filter(|url| !url.is_empty())
        {
            self.url
                .update(cx, |input, cx| input.show(&url, window, cx));
        }
        cx.notify();
    }

    fn toggle_hidden(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        // As in `pointer`, the row may name a device that Apply removed.
        let Some(visible) = self.devices.get(index).map(|device| device.hidden) else {
            return;
        };
        if !visible && self.selected == Some(index) {
            self.invalidate_ime();
        }
        if !visible && (!self.release_keys_for(index) || !self.release_buttons(index)) {
            return;
        }
        if !self.send(Command::SetVisible {
            device: index,
            visible,
        }) {
            return;
        }
        // A hidden card's prompt field is not drawn; focus in it would leave
        // shortcuts and key releases without a receiver.
        if !visible && self.key_focus(window) == KeyFocus::Prompt(index) {
            self.focus.focus(window);
        }
        let device = &mut self.devices[index];
        device.hidden = !visible;
        device.bounds.set(None);
        if device.hidden
            && let Some(image) = device.image.take()
        {
            let _ = window.drop_image(image);
        }
        let hidden: Vec<bool> = self.devices.iter().map(|device| device.hidden).collect();
        let selected = selected_after_visibility_change(self.selected, &hidden);
        if selected != self.selected {
            self.selected = selected;
            self.read_console();
            if let Some(index) = selected
                && let Some(url) = self
                    .status
                    .devices
                    .get(index)
                    .map(|device| device.url.clone())
                    .filter(|url| !url.is_empty())
            {
                self.url
                    .update(cx, |input, cx| input.show(&url, window, cx));
            }
        }
        cx.notify();
    }

    /// Releases the keys held in the page of device `index`. They stay pressed,
    /// so their repeats go nowhere until the physical release.
    fn release_keys_for(&mut self, index: usize) -> bool {
        let keys: Vec<String> = self
            .held_keys
            .iter()
            .filter_map(|(name, (device, _))| (*device == index).then_some(name.clone()))
            .collect();
        for name in keys {
            if let Some((device, mut key)) = self.held_keys.get(&name).cloned() {
                key.down = false;
                if !self.send(Command::Key { device, key }) {
                    return false;
                }
                self.held_keys.remove(&name);
            }
        }
        true
    }

    fn release_keys(&mut self) -> bool {
        for index in 0..self.devices.len() {
            if !self.release_keys_for(index) {
                return false;
            }
        }
        true
    }

    fn cancel_touches(&mut self) {
        for index in 0..self.devices.len() {
            if self.workspace.devices[index].touch {
                self.release_buttons(index);
            }
        }
    }

    fn release_buttons(&mut self, index: usize) -> bool {
        if self.workspace.devices[index].touch {
            if self.devices[index].buttons & 1 != 0
                && !self.send(Command::CancelTouch { device: index })
            {
                return false;
            }
            self.devices[index].buttons = 0;
            self.devices[index].last_point = None;
            return true;
        }
        let Some((x, y)) = self.devices[index].last_point else {
            self.devices[index].buttons = 0;
            return true;
        };
        for (bit, button) in [
            (1, PointerButton::Left),
            (2, PointerButton::Right),
            (4, PointerButton::Middle),
        ] {
            if self.devices[index].buttons & bit != 0 {
                let buttons = self.devices[index].buttons & !bit;
                if !self.send(Command::Pointer {
                    device: index,
                    event: PointerEvent {
                        kind: PointerKind::Up,
                        x,
                        y,
                        button,
                        buttons,
                        click_count: 1,
                        modifiers: Modifiers::default(),
                    },
                }) {
                    return false;
                }
                self.devices[index].buttons = buttons;
            }
        }
        self.devices[index].last_point = None;
        true
    }

    fn set_sync(&mut self, sync: SyncSettings, cx: &mut Context<Self>) {
        self.sync = sync;
        self.send(Command::SetSync(sync));
        cx.notify();
    }

    fn reload_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(device) = self.selected.filter(|&index| !self.devices[index].hidden) {
            self.invalidate_ime();
            self.send(Command::Reload { device });
            cx.notify();
        }
    }

    fn zoom_by(&mut self, delta: f32, window: &mut Window, cx: &mut Context<Self>) {
        self.scale = (self.scale + delta).clamp(0.25, 1.0);
        window.invalidate_character_coordinates();
        self.send_frame_limits(window);
        cx.notify();
    }

    /// Pauses the screencast of a device whose frame the scrolled canvas no
    /// longer shows, and resumes it once it does. A command the runtime does
    /// not accept is sent after a later paint.
    fn sync_on_screen(&mut self, index: usize) {
        let on_screen = Rc::clone(&self.devices[index].on_screen);
        on_screen.pending.set(false);
        let shown = on_screen.painted.get();
        if shown != on_screen.sent.get()
            && self.session.as_ref().is_some_and(|session| {
                session.send(Command::SetOnScreen {
                    device: index,
                    on_screen: shown,
                })
            })
        {
            on_screen.sent.set(shown);
        }
    }

    /// Asks for frames no larger than they are displayed, in physical pixels.
    fn send_frame_limits(&mut self, window: &Window) {
        self.limit_scale = window.scale_factor();
        let factor = self.scale * self.limit_scale;
        let limits: Vec<Command> = self
            .workspace
            .devices
            .iter()
            .enumerate()
            .map(|(device, config)| Command::SetFrameLimit {
                device,
                width: (config.width as f32 * factor).ceil() as u32,
                height: (config.height as f32 * factor).ceil() as u32,
            })
            .collect();
        for command in limits {
            self.send(command);
        }
    }

    /// Replaces a stopped runtime. Runs only while no other restart or close
    /// runs; otherwise the request is dropped, not queued.
    fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.lifecycle.begin_restart() {
            return;
        }
        self.invalidate_ime();
        // Keys still down stay pressed: their repeats reach no new page.
        self.held_keys.clear();
        for device in &mut self.devices {
            device.buttons = 0;
            device.last_point = None;
            device.bounds.set(None);
            device.invalidated_ime_target = None;
            // Download counts start over with each runtime.
            device.dismissed_download = None;
        }
        let text = self.url.read(cx).text().to_owned();
        if validate_url(&text).is_ok() {
            self.workspace.url = text;
        }
        for device in &mut self.devices {
            if let Some(image) = device.image.take() {
                let _ = window.drop_image(image);
            }
            device.decoding = None;
        }
        // Nothing of the previous runtime is shown while it stops.
        self.status = Status::default();
        self.read_console();
        self.forget_report();
        self.clear_prompts(window);
        self.notice = None;
        // The panel's draft becomes the workspace of the next runtime. Its
        // devices are new: hidden flags and the selection start over.
        if self.draft_changed() {
            self.draft.url = self.workspace.url.clone();
            self.workspace = self.draft.clone();
            self.devices = self
                .workspace
                .devices
                .iter()
                .map(|_| DeviceView::default())
                .collect();
            self.selected = Some(0);
            self.panel_notice = None;
        } else {
            self.draft.url = self.workspace.url.clone();
        }
        match self.session.take() {
            Some(previous) => self.stop_then_continue(previous, window, cx),
            None => self.after_stop(window, cx),
        }
        cx.notify();
    }

    /// Stops `session` off the UI thread, then continues the restart or close
    /// that took it.
    fn stop_then_continue(
        &mut self,
        session: LiveSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stopped = cx.background_executor().spawn(async move { drop(session) });
        cx.spawn_in(window, async move |this, cx| {
            stopped.await;
            this.update_in(cx, |view, window, cx| view.after_stop(window, cx))
                .ok();
        })
        .detach();
    }

    /// Starts the next runtime of a restart, or removes the window if a close
    /// was requested meanwhile: nothing starts after a close.
    fn after_stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.lifecycle.stopped() {
            AfterStop::Start => self.start(window, cx),
            AfterStop::Close => window.remove_window(),
            AfterStop::Wait => {}
        }
        cx.notify();
    }

    /// Whether the panel's draft differs from the running workspace in
    /// anything but the URL, which the URL bar owns.
    fn draft_changed(&self) -> bool {
        self.draft.name != self.workspace.name
            || self.draft.sessions != self.workspace.sessions
            || self.draft.devices != self.workspace.devices
    }

    fn toggle_panel(&mut self, cx: &mut Context<Self>) {
        self.toggle(SidePanel::Workspace, cx);
    }

    /// Opens `panel` in place of another one, or closes it.
    fn toggle(&mut self, panel: SidePanel, cx: &mut Context<Self>) {
        self.show_panel((self.panel != Some(panel)).then_some(panel), cx);
    }

    /// Shows `panel`, or no panel: the panel's tabs and its close button.
    fn show_panel(&mut self, panel: Option<SidePanel>, cx: &mut Context<Self>) {
        self.panel = panel;
        self.read_console();
        cx.notify();
    }

    /// Selects device `index` and shows its console.
    fn open_console(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.select(index, window, cx);
        if self.selected == Some(index) {
            self.panel = Some(SidePanel::Console);
            self.read_console();
            cx.notify();
        }
    }

    /// Reads the selected device's console while the Console panel shows it
    /// and it changed since the last read.
    fn read_console(&mut self) {
        let wanted = self
            .selected
            .filter(|_| self.panel == Some(SidePanel::Console))
            .and_then(|index| Some((index, self.status.devices.get(index)?.console_revision)));
        if wanted.is_none() {
            self.console_read = None;
            self.console.clear();
            return;
        }
        if wanted == self.console_read {
            return;
        }
        let Some((session, (index, _))) = self.session.as_ref().zip(wanted) else {
            return;
        };
        self.console = session.console(index);
        self.console_read = wanted;
    }

    fn clear_console(&mut self, cx: &mut Context<Self>) {
        if let Some(device) = self.selected {
            self.send(Command::ClearConsole { device });
            cx.notify();
        }
    }

    /// Asks the runtime for the selected device's screenshot; the report is
    /// written when it arrives, with the console and page as they are now
    /// (ADR 0024).
    fn save_report(&mut self, cx: &mut Context<Self>) {
        let Some(device) = self.selected else {
            return;
        };
        if self.report.is_some() || self.saving_report || !self.lifecycle.is_live() {
            return;
        }
        if self.reports_loading {
            self.report_notice = Some(("Locating the reports directory…".into(), false));
            cx.notify();
            return;
        }
        if self.reports.is_none() {
            self.report_notice = Some((
                "Not saved: no reports directory; set BROXSER_REPORT_DIR to an absolute path."
                    .into(),
                true,
            ));
            cx.notify();
            return;
        }
        let Some(session) = self.session.as_ref() else {
            self.report_notice = Some(("Not saved: the runtime is not running.".into(), true));
            cx.notify();
            return;
        };
        let Some((status, console)) = session.console_snapshot(device) else {
            return;
        };
        let runtime = session.status().runtime;
        self.next_report += 1;
        let expected_revision = status.page_revision;
        let report = PendingReport {
            device,
            token: self.next_report,
            console,
            errors: status.console_errors,
            warnings: status.console_warnings,
            url: status.url.clone(),
            browser: match &runtime {
                RuntimeState::Running { product, protocol } => {
                    Some((product.clone(), protocol.clone()))
                }
                _ => None,
            },
            saved: SystemTime::now(),
        };
        let token = report.token;
        if self.send(Command::Screenshot {
            device,
            token,
            expected_revision,
        }) {
            self.report = Some(report);
            self.report_notice = Some(("Taking the screenshot…".into(), false));
        } else {
            self.report_notice = Some(("Not saved: the runtime is not running.".into(), true));
        }
        cx.notify();
    }

    /// Writes a report off the UI thread and shows where it went.
    fn write_report(
        &mut self,
        pending: PendingReport,
        screenshot: Screenshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(root), Some(device)) = (
            self.reports.clone(),
            self.workspace.devices.get(pending.device).cloned(),
        ) else {
            return;
        };
        let session = self
            .workspace
            .sessions
            .iter()
            .find(|session| session.id == device.session)
            .map_or_else(|| device.session.clone(), |session| session.name.clone());
        if !self.lifecycle.begin_save() {
            self.report_notice = Some(("Not saved: the window is closing.".into(), true));
            cx.notify();
            return;
        }
        self.saving_report = true;
        self.report_notice = Some(("Saving the report…".into(), false));
        let written = save_off_thread(move || {
            let text = report::report_text(&report::Report {
                device: &device,
                session: &session,
                url: &pending.url,
                browser: pending
                    .browser
                    .as_ref()
                    .map(|(product, protocol)| (product.as_str(), protocol.as_str())),
                console: &pending.console,
                errors: pending.errors,
                warnings: pending.warnings,
                saved: pending.saved,
                screenshot: (screenshot.width, screenshot.height),
            });
            report::write_report(&root, &device.id, pending.saved, &screenshot.png, &text)
        });
        cx.spawn_in(window, async move |this, cx| {
            let written = written.await.and_then(|result| result.map_err(Into::into));
            this.update_in(cx, |view, window, cx| {
                view.saving_report = false;
                view.report_notice = Some(match written {
                    Ok(folder) => (format!("Saved to {}", folder.display()), false),
                    Err(error) => (format!("Not saved: {error}"), true),
                });
                if view.lifecycle.save_finished() {
                    window.remove_window();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Cancels a report still awaiting its screenshot when leaving a runtime.
    fn forget_report(&mut self) {
        if self.report.take().is_some() {
            self.report_notice = Some((
                "Not saved: the screenshot was cancelled before writing.".into(),
                true,
            ));
        }
    }

    /// Adds preset `index` to the draft, in the session of the selected
    /// device or else the first session.
    fn add_preset(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(preset) = PRESETS.get(index) else {
            return;
        };
        let session = self
            .selected
            .and_then(|selected| self.workspace.devices.get(selected))
            .map(|device| device.session.clone())
            .or_else(|| {
                self.draft
                    .sessions
                    .first()
                    .map(|session| session.id.clone())
            });
        let Some(session) = session else {
            return;
        };
        self.panel_notice = match self.draft.add_device_from_preset(preset, &session) {
            Ok(added) => Some(format!(
                "Added {}; Apply restarts the runtime with it.",
                self.draft.devices[added].name
            )),
            Err(error) => Some(error.to_string()),
        };
        cx.notify();
    }

    fn remove_draft_device(&mut self, index: usize, cx: &mut Context<Self>) {
        self.panel_notice = match self.draft.remove_device(index) {
            Ok(removed) => Some(format!(
                "Removed {}; Apply restarts the runtime without it.",
                removed.name
            )),
            Err(error) => Some(error.to_string()),
        };
        cx.notify();
    }

    fn discard_draft(&mut self, cx: &mut Context<Self>) {
        self.draft = self.workspace.clone();
        self.panel_notice = None;
        cx.notify();
    }

    /// Writes the draft to the workspace file it was loaded from. The running
    /// runtime is untouched; Apply is separate and explicit.
    fn save_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving_workspace || self.lifecycle.is_closing() {
            return;
        }
        let Some(path) = self.workspace_path.clone() else {
            self.panel_notice =
                Some("Started without --workspace; there is no file to save to.".into());
            cx.notify();
            return;
        };
        let mut saved = self.draft.clone();
        saved.url = self.url.read(cx).text().to_owned();
        if let Err(error) = saved.validate() {
            self.panel_notice = Some(format!("Not saved: {error}"));
            cx.notify();
            return;
        }
        if !self.lifecycle.begin_save() {
            return;
        }
        self.saving_workspace = true;
        self.panel_notice = Some("Saving workspace…".into());
        let saving = save_off_thread(move || match saved.save(&path) {
            Ok(()) => format!("Saved snapshot to {}.", path.display()),
            Err(error) => format!("Not saved: {error}"),
        });
        cx.spawn_in(window, async move |this, cx| {
            let notice = saving
                .await
                .unwrap_or_else(|error| format!("Not saved: {error:#}"));
            this.update_in(cx, |view, window, cx| {
                view.saving_workspace = false;
                view.panel_notice = Some(notice);
                if view.lifecycle.save_finished() {
                    window.remove_window();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Remembers the window size for the next start (ADR 0022). Nothing else
    /// of a run is written: no page address, cookie or profile.
    fn save_window_size(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.state_path.clone() else {
            return;
        };
        if !self.lifecycle.begin_save() {
            return;
        }
        let size = window.bounds().size;
        let size = WindowSize {
            width: f32::from(size.width).round().max(0.0) as u32,
            height: f32::from(size.height).round().max(0.0) as u32,
        };
        let saving = save_off_thread(move || {
            let mut state = AppState::load(&path).unwrap_or_default();
            state.window = Some(size);
            if let Err(error) = state.save(&path) {
                eprintln!("broxser: could not save {}: {error}", path.display());
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = saving.await {
                eprintln!("broxser: could not save window size: {error:#}");
            }
            this.update_in(cx, |view, window, cx| {
                if view.lifecycle.save_finished() {
                    window.remove_window();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Stops the browser independently of file I/O. GPUI ends the process when
    /// the last window closes, so keep it until both cleanup and saves finish.
    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.invalidate_ime();
        // Stop can cancel a capture; a write already started must finish.
        self.forget_report();
        self.save_window_size(window, cx);
        let request = self.lifecycle.begin_close(self.session.is_some());
        if request == CloseRequest::Now {
            return true;
        }
        self.notice = Some("Closing after the browser stops and saves finish…".into());
        cx.notify();
        if request == CloseRequest::Stop
            && let Some(session) = self.session.take()
        {
            self.clear_prompts(window);
            self.stop_then_continue(session, window, cx);
        }
        false
    }

    fn ime_origin_with_status(&self, status: &Status, window: &Window) -> Option<Origin> {
        if !window.is_window_active() || !self.focus.is_focused(window) || !self.lifecycle.is_live()
        {
            return None;
        }
        let device = self.selected?;
        if self.devices.get(device)?.hidden
            || !matches!(&status.runtime, RuntimeState::Running { .. })
        {
            return None;
        }
        let target = self.devices[device].ime_target(status.devices.get(device)?)?;
        Some(Origin {
            device,
            target,
            generation: self.lifecycle.generation(),
        })
    }

    fn ime_origin(&self, window: &Window) -> Option<Origin> {
        self.ime_origin_with_status(&self.status, window)
    }

    fn dispatch_ime(&self, origin: Origin, action: ImeAction) {
        if self.lifecycle.generation() == origin.generation
            && let Some(session) = &self.session
        {
            let _ = session.send(Command::Ime {
                device: origin.device,
                target: origin.target,
                action,
            });
        }
    }

    fn invalidate_ime(&mut self) {
        if let Some((origin, action)) = self.ime.invalidate() {
            self.dispatch_ime(origin, action);
        }
    }

    fn commit_ime(&mut self, text: &str, window: &Window) {
        if let Some((origin, action)) = self.ime.commit(self.ime_origin(window), text) {
            self.dispatch_ime(origin, action);
        }
    }

    fn map(&self, index: usize, position: Point<Pixels>) -> Option<(f64, f64)> {
        let bounds = self.devices[index].bounds.get()?;
        let device = &self.workspace.devices[index];
        to_viewport(
            (position.x.to_f64(), position.y.to_f64()),
            (
                bounds.origin.x.to_f64(),
                bounds.origin.y.to_f64(),
                bounds.size.width.to_f64(),
                bounds.size.height.to_f64(),
            ),
            (f64::from(device.width), f64::from(device.height)),
        )
    }

    #[expect(clippy::too_many_arguments)]
    fn pointer(
        &mut self,
        index: usize,
        kind: PointerKind,
        position: Point<Pixels>,
        button: Option<MouseButton>,
        click_count: usize,
        modifiers: &gpui::Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // GPUI dispatches input to the listeners of the last drawn frame, so
        // one can still name a device that Apply removed before the redraw.
        if self.devices.get(index).is_none_or(|device| device.hidden) {
            return;
        }
        // A touch canvas has only a left-button finger. Reject other presses
        // before they can cancel composition, select a device or alter its
        // held-button state; the engine also rejects unsupported page input.
        if self.workspace.devices[index].touch
            && kind != PointerKind::Move
            && button != Some(MouseButton::Left)
        {
            return;
        }
        // URL and prompt fields are descendants of the root focus handle,
        // so entering them need not trigger its focus-out subscription.
        if self.workspace.devices[index].touch
            && kind != PointerKind::Down
            && self.key_focus(window) != KeyFocus::Canvas
        {
            self.release_buttons(index);
            return;
        }
        let Some((x, y)) = self.map(index, position) else {
            return;
        };
        let bit = match button {
            Some(MouseButton::Left) => 1,
            Some(MouseButton::Right) => 2,
            Some(MouseButton::Middle) => 4,
            _ => 0,
        };
        if kind == PointerKind::Down {
            if matches!(button, Some(MouseButton::Left | MouseButton::Right)) {
                self.invalidate_ime();
                if let Some(status) = self.status.devices.get_mut(index) {
                    // This click can move editable focus before the next engine
                    // snapshot. Never latch its old target for a new composition.
                    self.devices[index].invalidate_ime_target(status);
                }
            }
            self.select(index, window, cx);
            if self.selected != Some(index) {
                return;
            }
        }
        let buttons = match kind {
            PointerKind::Down => self.devices[index].buttons | bit,
            PointerKind::Up => self.devices[index].buttons & !bit,
            PointerKind::Move => self.devices[index].buttons,
        };
        let button = match (kind, button) {
            (PointerKind::Move, _) if buttons & 1 != 0 => PointerButton::Left,
            (PointerKind::Move, _) => PointerButton::None,
            (_, Some(MouseButton::Left)) => PointerButton::Left,
            (_, Some(MouseButton::Right)) => PointerButton::Right,
            (_, Some(MouseButton::Middle)) => PointerButton::Middle,
            _ => PointerButton::None,
        };
        if self.send(Command::Pointer {
            device: index,
            event: PointerEvent {
                kind,
                x,
                y,
                button,
                buttons,
                click_count: click_count.min(3) as u32,
                modifiers: modifiers_of(modifiers),
            },
        }) {
            self.devices[index].buttons = buttons;
            self.devices[index].last_point = Some((x, y));
        }
    }

    /// A button released outside the frame must not stay pressed in the page.
    fn release_outside(&mut self, index: usize, modifiers: &gpui::Modifiers) {
        // As in `pointer`, the device may be gone since the frame was drawn.
        if self.devices.get(index).is_none_or(|device| device.hidden) {
            return;
        }
        // Outside motion has no mapped viewport point. Ending a touch at its
        // last inside point could click the element the user dragged away from.
        if self.workspace.devices[index].touch {
            self.release_buttons(index);
            return;
        }
        let device = &self.devices[index];
        let Some((x, y)) = device.last_point.filter(|_| device.buttons & 1 != 0) else {
            return;
        };
        let buttons = device.buttons & !1;
        if self.send(Command::Pointer {
            device: index,
            event: PointerEvent {
                kind: PointerKind::Up,
                x,
                y,
                button: PointerButton::Left,
                buttons,
                click_count: 1,
                modifiers: modifiers_of(modifiers),
            },
        }) {
            self.devices[index].buttons = buttons;
        }
    }

    fn wheel(&mut self, index: usize, event: &ScrollWheelEvent) {
        // As in `pointer`, the device may be gone since the frame was drawn.
        if self.devices.get(index).is_none_or(|device| device.hidden) {
            return;
        }
        let Some((x, y)) = self.map(index, event.position) else {
            return;
        };
        let Some(bounds) = self.devices[index].bounds.get() else {
            return;
        };
        let scale = bounds.size.width.to_f64() / f64::from(self.workspace.devices[index].width);
        let delta = event.delta.pixel_delta(px(WHEEL_LINE));
        // GPUI reports how content should move; CDP expects the scroll direction.
        self.send(Command::Wheel {
            device: index,
            x,
            y,
            delta_x: -delta.x.to_f64() / scale,
            delta_y: -delta.y.to_f64() / scale,
        });
    }

    /// Every key-down in the window, before shortcuts and elements see it.
    fn record_press(&mut self, keystroke: &Keystroke) {
        if let Some((key, identity)) = key_input(keystroke, true) {
            // Its release ends the press, unless the IME composition takes
            // the key-down in the canvas; key_down forgets the press then.
            self.ignored_ime_keys.remove(&identity);
            self.pressed.press(identity, &key);
        }
    }

    /// A key-down in the device canvas, or one that a field inside it left.
    fn key_down(
        &mut self,
        keystroke: &Keystroke,
        is_held: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some((key, identity)) = key_input(keystroke, true) else {
            return;
        };
        let composing = self.ime.has_mark()
            && !keystroke.modifiers.control
            && !keystroke.modifiers.alt
            && !keystroke.modifiers.platform;
        let repeat = is_held || self.pressed.repeating(&identity);
        let target = self
            .selected
            .filter(|&index| self.devices.get(index).is_some_and(|device| !device.hidden));
        match canvas_key(
            self.key_focus(window),
            composing,
            &key,
            &identity,
            repeat,
            target,
            &self.held_keys,
        ) {
            CanvasKey::Drop => {}
            CanvasKey::Compose => {
                self.pressed.forget(&identity);
                self.ignored_ime_keys.insert(identity);
                // GPUI/XKB can report the composed character as a key-down after
                // invoking preedit. It belongs to the composition, not raw CDP keys.
                if let Some(text) = keystroke.key_char.as_deref() {
                    self.commit_ime(text, window);
                }
            }
            CanvasKey::Paste => self.paste(cx),
            CanvasKey::Send(device) => {
                if self.send(Command::Key {
                    device,
                    key: key.clone(),
                }) {
                    self.held_keys.insert(identity, (device, key));
                }
            }
        }
    }

    /// A key-up anywhere in the window: it is observed on the window root,
    /// even if focus moved to the URL bar or a prompt field after the press.
    /// Every release ends its press, and a page gets a key-up only for a key
    /// whose key-down it received.
    fn key_up(&mut self, keystroke: &Keystroke) {
        let Some((_, identity)) = key_input(keystroke, false) else {
            return;
        };
        if self.ignored_ime_keys.remove(&identity) {
            return;
        }
        let Some(pressed) = self.pressed.release(&identity) else {
            return;
        };
        if let Some((device, mut key)) = self.held_keys.get(&pressed).cloned()
            && !self.devices[device].hidden
        {
            key.down = false;
            if self.send(Command::Key { device, key }) {
                self.held_keys.remove(&pressed);
            }
        }
    }

    /// Inserts the system clipboard's text into the selected device (ADR 0010).
    /// Pages never read the browser's clipboard, which all sessions share.
    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(device) = self.selected.filter(|&index| !self.devices[index].hidden) else {
            return;
        };
        let text = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        match paste_text(&text) {
            Ok(text) => {
                self.notice = None;
                self.send(Command::InsertText { device, text });
            }
            Err(PasteRejected::Empty) => {
                self.notice = Some("Nothing pasted: the clipboard holds no text.".into());
            }
            Err(PasteRejected::TooLong(length)) => {
                self.notice = Some(format!(
                    "Nothing pasted: the clipboard text has {length} characters, more than {MAX_PASTE_CHARS}."
                ));
            }
        }
        cx.notify();
    }

    fn device_card(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let device = &self.workspace.devices[index];
        let view = &self.devices[index];
        let status = self.status.devices.get(index).cloned().unwrap_or_default();
        let width = device.width as f32 * self.scale;
        let height = device.height as f32 * self.scale;
        // Header, notices and footer are never narrower than this.
        let content = width.max(180.);
        let bounds = Rc::clone(&view.bounds);
        let on_screen = Rc::clone(&view.on_screen);
        let image = view.image.clone();
        let selected = self.selected == Some(index);
        let focus = self.focus.clone();
        let entity = cx.entity();
        let surface = canvas(
            |_, _, _| (),
            move |area, (), window, cx| {
                if bounds.get() != Some(area) {
                    window.invalidate_character_coordinates();
                }
                bounds.set(Some(area));
                if selected {
                    window.handle_input(&focus, ElementInputHandler::new(area, entity.clone()), cx);
                }
                let shown = area.intersects(&window.content_mask().bounds);
                on_screen.painted.set(shown);
                if shown != on_screen.sent.get() && !on_screen.pending.replace(true) {
                    let entity = entity.clone();
                    cx.defer(move |cx| entity.update(cx, |view, _| view.sync_on_screen(index)));
                }
                // GPUI uploads every painted image, including clipped ones.
                if shown && let Some(image) = image {
                    let corners = Corners::all(px(FRAME_RADIUS));
                    let _ = window.paint_image(area, corners, image, 0, false);
                }
            },
        )
        .size_full();
        let session = self
            .workspace
            .sessions
            .iter()
            .position(|session| session.id == device.session);
        let mut meta = format!(
            "{} × {} · {}×",
            device.width, device.height, device.device_scale_factor
        );
        if device.touch {
            meta.push_str(" · touch");
        }
        let state = match (&status.error, status.loading) {
            (Some(error), _) => div()
                .flex()
                .gap(px(6.))
                .text_size(px(11.5))
                .line_height(px(16.))
                .text_color(rgb(WARN_TEXT))
                .child(
                    div()
                        .pt(px(2.))
                        .child(theme::icon(Icon::Warning, 12., WARN)),
                )
                // Wraps up to three lines, so the reason stays readable.
                .child(div().flex_1().min_w_0().line_clamp(3).child(error.clone())),
            (None, true) => div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(11.5))
                .text_color(rgb(INFO))
                .child(theme::icon(Icon::Loading, 12., INFO))
                .child("Loading…"),
            (None, false) if status.url.is_empty() => div()
                .text_size(px(11.5))
                .text_color(rgb(MUTED))
                .child("Starting…"),
            (None, false) => theme::mono(status.url.clone(), 11., TEXT_2).truncate(),
        };
        let header = div()
            .w(px(content))
            .flex()
            .flex_col()
            .gap(px(5.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .child(theme::icon(
                        theme::device_icon(device.width, device.mobile),
                        14.,
                        TEXT_2,
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(14.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(device.name.clone()),
                    )
                    .children(session.map(|session| {
                        div().flex_1().flex().justify_end().child(theme::tag(
                            self.workspace.sessions[session].name.clone(),
                            theme::session_hue(session),
                        ))
                    })),
            )
            .child(theme::mono(meta, 11., MUTED))
            .child(state);
        let frame = div()
            .id(("frame", index))
            .w(px(width))
            .h(px(height))
            .flex_none()
            .bg(rgb(CANVAS))
            .rounded(px(FRAME_RADIUS))
            .shadow(theme::frame_shadow())
            .overflow_hidden()
            .cursor_default()
            .child(surface)
            .on_any_mouse_down(
                cx.listener(move |view, event: &MouseDownEvent, window, cx| {
                    view.pointer(
                        index,
                        PointerKind::Down,
                        event.position,
                        Some(event.button),
                        event.click_count,
                        &event.modifiers,
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .capture_any_mouse_up(cx.listener(move |view, event: &MouseUpEvent, window, cx| {
                view.pointer(
                    index,
                    PointerKind::Up,
                    event.position,
                    Some(event.button),
                    event.click_count,
                    &event.modifiers,
                    window,
                    cx,
                );
            }))
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |view, event: &MouseUpEvent, _, _| {
                    view.release_outside(index, &event.modifiers);
                }),
            )
            .on_mouse_move(
                cx.listener(move |view, event: &MouseMoveEvent, window, cx| {
                    view.pointer(
                        index,
                        PointerKind::Move,
                        event.position,
                        event.pressed_button,
                        0,
                        &event.modifiers,
                        window,
                        cx,
                    );
                }),
            )
            .on_scroll_wheel(cx.listener(move |view, event: &ScrollWheelEvent, _, cx| {
                view.wheel(index, event);
                cx.stop_propagation();
            }));
        let stream = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .text_color(rgb(0xc9ced5))
            .when(status.streaming, |this| {
                this.child(theme::dot(ACCENT, 5.)).child("Streaming")
            })
            .when(!status.streaming, |this| {
                this.child(theme::icon(Icon::Paused, 12., TEXT_2))
                    .child("Paused")
            });
        let frames = theme::mono(
            format!(
                "{} frames · {} replaced",
                status.frames, status.dropped_frames
            ),
            11.,
            MUTED,
        );
        let footer = div()
            .w(px(content))
            .flex()
            .flex_col()
            .gap(px(4.))
            .text_size(px(11.5))
            .text_color(rgb(MUTED))
            .child(if content >= 300. {
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(stream)
                    .child(div().flex_1().flex().justify_end().child(frames))
            } else {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(stream)
                    .child(frames)
            })
            .children(activity_line(&status))
            .when(
                status.console_errors > 0 || status.console_warnings > 0,
                |this| {
                    // Filled in the danger or warning color, so a real-window
                    // check can find it by color alone.
                    this.child(
                        div().flex().pt(px(4.)).child(
                            div()
                                .id(("console-summary", index))
                                .h(px(24.))
                                .pl(px(8.))
                                .pr(px(9.))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .rounded_full()
                                .cursor_pointer()
                                .bg(rgb(if status.console_errors > 0 {
                                    DANGER
                                } else {
                                    WARN
                                }))
                                .text_color(rgb(INK))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(theme::icon(Icon::Console, 12., INK))
                                .child(console_counts(&status))
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.open_console(index, window, cx)
                                })),
                        ),
                    )
                },
            );
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(12.))
            .rounded(px(14.))
            .border_1()
            .border_color(rgb(if selected { ACCENT } else { BORDER }))
            .bg(rgb(CARD))
            .shadow(theme::card_shadow(selected))
            .child(header)
            .when_some(
                open_dialog(&self.status, self.session.is_some(), index).cloned(),
                |this, dialog| {
                    let panel = width.clamp(180., PANEL_WIDTH);
                    this.child(self.dialog_panel(index, &dialog, panel, cx))
                },
            )
            // A viewport narrower than the card's text sits in its middle.
            .child(div().w(px(content)).flex().justify_center().child(frame))
            .child(footer)
            .when_some(
                status
                    .popup
                    .clone()
                    .filter(|popup| view.dismissed_popup != Some(popup.token)),
                |this, popup| {
                    this.child(self.popup_notice(index, &popup, width.clamp(180., PANEL_WIDTH), cx))
                },
            )
            .when_some(
                status
                    .download
                    .clone()
                    .filter(|_| view.dismissed_download != Some(status.downloads)),
                |this, download| {
                    this.child(self.download_notice(
                        index,
                        &download,
                        status.downloads,
                        width.clamp(180., PANEL_WIDTH),
                        cx,
                    ))
                },
            )
            .into_any_element()
    }

    /// The latest download the device's page started, which the browser
    /// refused (ADR 0016). Nothing saves it; Dismiss only hides the report.
    fn download_notice(
        &self,
        index: usize,
        download: &DownloadState,
        count: u32,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let line = |text: &str, fallback: &'static str| -> SharedString {
            if text.is_empty() {
                fallback.into()
            } else {
                text.to_owned().into()
            }
        };
        let filename = line(&download.filename, "(no file name)");
        let url = line(&download.url, "(no address)");
        notice(width, Icon::Download, "Refused a download the page started")
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(12.5))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(filename),
                    )
                    .child(theme::mono(url, 11., MUTED).truncate()),
            )
            .child(
                div().flex().child(
                    theme::button(("download-dismiss", index), Tone::Primary, 28.)
                        .child("Dismiss")
                        .on_click(cx.listener(move |view, _, _, cx| {
                            // Only the report this button was drawn with: after
                            // Apply or Restart the index names another runtime's
                            // device, which starts without one.
                            let shown = view
                                .status
                                .devices
                                .get(index)
                                .is_some_and(|status| status.downloads == count);
                            if let Some(device) = view.devices.get_mut(index).filter(|_| shown) {
                                device.dismissed_download = Some(count);
                            }
                            cx.notify();
                        })),
                ),
            )
            .into_any_element()
    }

    /// The latest window the device's page opened, which Broxser closed
    /// (ADR 0015). Opening it loads its URL in this device; nothing opens it
    /// otherwise. Its answers sit at the left edge like a dialog's.
    fn popup_notice(
        &self,
        index: usize,
        popup: &PopupState,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let token = popup.token;
        let url: SharedString = if popup.url.is_empty() {
            "(no address)".into()
        } else {
            popup.url.clone().into()
        };
        notice(width, Icon::Window, "Closed a window the page opened")
            .child(theme::mono(url, 11., TEXT_2).truncate())
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .when(popup.openable, |this| {
                        this.child(
                            theme::button(("popup-open", index), Tone::Primary, 28.)
                                .child("Open here")
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.notice = None;
                                    view.send(Command::OpenPopup {
                                        device: index,
                                        token,
                                    });
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        theme::button(("popup-dismiss", index), Tone::Secondary, 28.)
                            .child("Dismiss")
                            .on_click(cx.listener(move |view, _, _, cx| {
                                // As for downloads: only the report still shown.
                                let shown = view
                                    .status
                                    .devices
                                    .get(index)
                                    .and_then(|status| status.popup.as_ref())
                                    .is_some_and(|popup| popup.token == token);
                                if let Some(device) = view.devices.get_mut(index).filter(|_| shown)
                                {
                                    device.dismissed_popup = Some(token);
                                }
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    /// Brand, the address with Go, then the selected device's reload, sync,
    /// zoom and the panel buttons at fixed widths from the right edge.
    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sync = self.sync;
        let live = div()
            .h(px(22.))
            .px(px(9.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .rounded_full()
            .bg(rgba(ACCENT_SOFT))
            .text_size(px(11.5))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(rgb(ACCENT))
            .child(theme::dot(ACCENT, 6.))
            .child("Live frames");
        div()
            .h(px(52.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(12.))
            .pl(px(16.))
            .pr(px(14.))
            .bg(rgb(CHROME))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(theme::brand(live))
            .child(theme::divider())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(self.url.clone())
                    .child(
                        theme::button("go", Tone::Primary, 34.)
                            .pl(px(14.))
                            .rounded(px(9.))
                            .text_size(px(13.))
                            .child("Go")
                            .child(theme::icon(Icon::Go, 14., INK))
                            .on_click(cx.listener(|view, _, window, cx| {
                                let text = view.url.read(cx).text().to_owned();
                                view.navigate(&text, window, cx);
                            })),
                    ),
            )
            .child(
                theme::button("reload", Tone::Secondary, 34.)
                    .rounded(px(9.))
                    .text_size(px(13.))
                    .child(theme::icon(Icon::Reload, 14., TEXT))
                    .child("Reload")
                    .on_click(cx.listener(|view, _, _, cx| view.reload_selected(cx))),
            )
            .child(
                theme::segmented()
                    .child(
                        div()
                            .pl(px(8.))
                            .pr(px(6.))
                            .child(theme::caps("SYNC", MUTED)),
                    )
                    .child(
                        theme::segment(
                            "sync-navigation",
                            Some(Icon::Links),
                            "Links",
                            sync.navigation,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.set_sync(
                                SyncSettings {
                                    navigation: !sync.navigation,
                                    ..sync
                                },
                                cx,
                            )
                        })),
                    )
                    .child(
                        theme::segment("sync-scroll", Some(Icon::Scroll), "Scroll", sync.scroll)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.set_sync(
                                    SyncSettings {
                                        scroll: !sync.scroll,
                                        ..sync
                                    },
                                    cx,
                                )
                            })),
                    ),
            )
            .child(theme::stepper(
                format!("{}%", (self.scale * 100.).round() as u32),
                div()
                    .id("zoom-out")
                    .on_click(cx.listener(|view, _, window, cx| view.zoom_by(-0.125, window, cx))),
                div()
                    .id("zoom-in")
                    .on_click(cx.listener(|view, _, window, cx| view.zoom_by(0.125, window, cx))),
            ))
            .child(theme::divider())
            .child(
                theme::toggle(
                    "workspace-panel",
                    Icon::Workspace,
                    "Workspace",
                    self.panel == Some(SidePanel::Workspace),
                    116.,
                )
                .on_click(cx.listener(|view, _, _, cx| view.toggle_panel(cx))),
            )
            .child(
                theme::toggle(
                    "console-panel-toggle",
                    Icon::Console,
                    "Console",
                    self.panel == Some(SidePanel::Console),
                    100.,
                )
                .on_click(cx.listener(|view, _, _, cx| view.toggle(SidePanel::Console, cx))),
            )
    }

    /// The workspace and its devices by session. Rows have fixed heights, so a
    /// window check can reach them by position.
    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sections: Vec<AnyElement> = self
            .workspace
            .sessions
            .iter()
            .enumerate()
            .map(|(session_index, session)| {
                let hue = theme::session_hue(session_index);
                let rows: Vec<AnyElement> = self
                    .workspace
                    .devices
                    .iter()
                    .enumerate()
                    .filter(|(_, device)| device.session == session.id)
                    .map(|(index, device)| {
                        let hidden = self.devices[index].hidden;
                        let selected = self.selected == Some(index);
                        let status = self.status.devices.get(index);
                        let state = status.filter(|_| !hidden).and_then(|status| {
                            if status.error.is_some() {
                                Some(WARN)
                            } else if status.loading {
                                Some(INFO)
                            } else {
                                status.streaming.then_some(ACCENT)
                            }
                        });
                        div()
                            .h(px(34.))
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .pl(px(10.))
                            .pr(px(4.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(if selected {
                                rgb(BORDER_STRONG)
                            } else {
                                rgba(0)
                            })
                            .when(selected, |this| this.bg(rgb(HOVER)))
                            .child(
                                div()
                                    .id(("select", index))
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .flex()
                                    .items_center()
                                    .gap(px(9.))
                                    .cursor_pointer()
                                    .text_color(rgb(if hidden {
                                        MUTED
                                    } else if selected {
                                        TEXT
                                    } else {
                                        0xd7dbe0
                                    }))
                                    .child(theme::icon(
                                        theme::device_icon(device.width, device.mobile),
                                        15.,
                                        if hidden {
                                            FAINT
                                        } else if selected {
                                            0xc9ced5
                                        } else {
                                            0xa4abb5
                                        },
                                    ))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(px(13.))
                                            .font_weight(if selected {
                                                gpui::FontWeight::SEMIBOLD
                                            } else {
                                                gpui::FontWeight::MEDIUM
                                            })
                                            .child(device.name.clone()),
                                    )
                                    .children(state.map(|color| theme::dot(color, 6.)))
                                    .child(div().flex_1().flex().justify_end().child(if hidden {
                                        div()
                                            .text_size(px(11.))
                                            .text_color(rgb(MUTED))
                                            .child("Hidden")
                                    } else {
                                        theme::mono(
                                            format!("{}×{}", device.width, device.height),
                                            11.,
                                            MUTED,
                                        )
                                    }))
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.select(index, window, cx)
                                    })),
                            )
                            .child(
                                theme::icon_button(
                                    ("visibility", index),
                                    if hidden { Icon::Hidden } else { Icon::Visible },
                                    if hidden { 0xa4abb5 } else { MUTED },
                                )
                                .on_click(cx.listener(
                                    move |view, _, window, cx| {
                                        view.toggle_hidden(index, window, cx)
                                    },
                                )),
                            )
                            .into_any_element()
                    })
                    .collect();
                let initial: String = session
                    .name
                    .chars()
                    .next()
                    .map(|first| first.to_uppercase().collect())
                    .unwrap_or_default();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .h(px(24.))
                            .pb(px(6.))
                            .px(px(6.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .size(px(18.))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(5.))
                                    .bg(theme::tint(hue, 0x29))
                                    .text_size(px(10.5))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(hue))
                                    .child(initial),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(13.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(session.name.clone()),
                            )
                            .child(
                                theme::mono(rows.len().to_string(), 11., MUTED)
                                    .flex_1()
                                    .flex()
                                    .justify_end(),
                            ),
                    )
                    .children(rows)
                    .into_any_element()
            })
            .collect();
        div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(CHROME))
            .border_r_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .h(px(80.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap(px(6.))
                    .px(px(16.))
                    .border_b_1()
                    .border_color(rgb(DIVIDER))
                    .child(theme::caps("WORKSPACE", ACCENT))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(18.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(self.workspace.name.clone()),
                    ),
            )
            .child(
                div()
                    .id("device-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(18.))
                    .px(px(10.))
                    .py(px(16.))
                    .child(
                        div()
                            .h(px(20.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px(px(6.))
                            .child(theme::caps("SESSIONS", MUTED))
                            .child(pill("Ephemeral")),
                    )
                    .children(sections),
            )
            .child(
                div().flex_none().px(px(12.)).pt(px(8.)).pb(px(14.)).child(theme::note(
                    Icon::Info,
                    "Frames stream from a headless browser. Sync stays inside one session; typing, forms and clicks are never broadcast.",
                )),
            )
    }

    /// The panel right of the canvas: Workspace or Console, one at a time.
    fn inspector(&self, panel: SidePanel, cx: &mut Context<Self>) -> AnyElement {
        let tab = |id: &'static str, glyph: Icon, label: &'static str, which: SidePanel| {
            let on = panel == which;
            let color = if on { TEXT } else { 0xa4abb5 };
            div()
                .id(id)
                .h(px(28.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(7.))
                .border_1()
                .cursor_pointer()
                .text_size(px(12.5))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(rgb(color))
                .when(on, |this| {
                    this.border_color(rgb(0x2f343b)).bg(rgb(0x1f2328))
                })
                .when(!on, |this| {
                    this.border_color(rgba(0))
                        .hover(|style| style.bg(rgb(HOVER)))
                })
                .child(theme::icon(glyph, 13., color))
                .child(label)
                .on_click(cx.listener(move |view, _, _, cx| view.show_panel(Some(which), cx)))
        };
        div()
            .w(px(INSPECTOR_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(CHROME))
            .border_l_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .h(px(48.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pl(px(12.))
                    .pr(px(10.))
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(
                        div()
                            .flex()
                            .gap(px(2.))
                            .p(px(2.))
                            .rounded(px(9.))
                            .border_1()
                            .border_color(rgb(BORDER))
                            .bg(rgb(CANVAS))
                            .child(tab(
                                "panel-tab-workspace",
                                Icon::Workspace,
                                "Workspace",
                                SidePanel::Workspace,
                            ))
                            .child(tab(
                                "panel-tab-console",
                                Icon::Console,
                                "Console",
                                SidePanel::Console,
                            )),
                    )
                    .child(div().flex_1())
                    .child(
                        theme::icon_button("panel-close", Icon::Close, 0xa4abb5)
                            .on_click(cx.listener(|view, _, _, cx| view.show_panel(None, cx))),
                    ),
            )
            .child(match panel {
                SidePanel::Workspace => self.workspace_panel(cx),
                SidePanel::Console => self.console_panel(cx),
            })
            .into_any_element()
    }

    /// The workspace panel: the draft's devices with Remove, the presets with
    /// Add, and Apply, Save and Discard. Edits touch the draft only; Apply
    /// restarts the runtime with it and Save writes the file (ADR 0022).
    fn workspace_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let changed = self.draft_changed();
        let removable = self.draft.devices.len() > 1;
        // Filled buttons: Add, Apply and Save in the accent color, Remove in
        // the danger color, so a real-window check can find each kind of
        // button by its color alone.
        let save = theme::button(
            "save-draft",
            if !changed && self.workspace_path.is_some() && !self.saving_workspace {
                Tone::Primary
            } else if self.saving_workspace || self.lifecycle.is_closing() {
                Tone::Disabled
            } else {
                Tone::Secondary
            },
            32.,
        )
        .child(theme::icon(
            Icon::Save,
            14.,
            if !changed && self.workspace_path.is_some() && !self.saving_workspace {
                INK
            } else {
                TEXT_2
            },
        ))
        .child(if self.saving_workspace {
            "Saving…"
        } else {
            "Save"
        })
        .when(
            !self.saving_workspace && !self.lifecycle.is_closing(),
            |button| {
                button.on_click(cx.listener(|view, _, window, cx| view.save_draft(window, cx)))
            },
        );
        let devices: Vec<AnyElement> = self
            .draft
            .devices
            .iter()
            .enumerate()
            .map(|(index, device)| {
                div()
                    .h(px(38.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .pl(px(2.))
                    .pr(px(4.))
                    .when(index > 0, |this| {
                        this.border_t_1().border_color(rgb(DIVIDER))
                    })
                    .child(theme::icon(
                        theme::device_icon(device.width, device.mobile),
                        15.,
                        0xa4abb5,
                    ))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(device.name.clone()),
                    )
                    .child(
                        theme::mono(
                            format!(
                                "{}×{}{} · {}",
                                device.width,
                                device.height,
                                scale_suffix(device.device_scale_factor),
                                device.session
                            ),
                            11.,
                            MUTED,
                        )
                        .min_w_0()
                        .truncate(),
                    )
                    .when(removable, |row| {
                        row.child(
                            div().flex_1().flex().justify_end().child(
                                theme::button(("remove-device", index), Tone::Danger, 24.)
                                    .text_size(px(11.5))
                                    .child("Remove")
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        view.remove_draft_device(index, cx)
                                    })),
                            ),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        let tile = |index: usize, preset: &broxser_core::DevicePreset| {
            div()
                .id(("add-preset", index))
                .flex_1()
                .min_w_0()
                .h(px(50.))
                .flex()
                .items_center()
                .gap(px(10.))
                .pl(px(12.))
                .pr(px(10.))
                .rounded(px(10.))
                .border_1()
                .border_color(rgb(BORDER))
                .bg(rgb(CARD))
                .cursor_pointer()
                .hover(|style| style.border_color(rgb(BORDER_STRONG)))
                .child(theme::icon(
                    theme::device_icon(preset.width, preset.mobile),
                    15.,
                    0xa4abb5,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .child(
                            div()
                                .truncate()
                                .text_size(px(12.5))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(preset.name),
                        )
                        .child(
                            theme::mono(
                                format!(
                                    "{}×{}{}",
                                    preset.width,
                                    preset.height,
                                    scale_suffix(preset.device_scale_factor)
                                ),
                                10.5,
                                MUTED,
                            )
                            .truncate(),
                        ),
                )
                .child(
                    div()
                        .size(px(22.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .bg(rgb(ACCENT))
                        .child(theme::icon(Icon::Plus, 12., INK)),
                )
                .on_click(cx.listener(move |view, _, _, cx| view.add_preset(index, cx)))
                .into_any_element()
        };
        let mut tiles = PRESETS
            .iter()
            .enumerate()
            .map(|(index, preset)| tile(index, preset));
        let mut preset_rows: Vec<AnyElement> = Vec::new();
        while let Some(first) = tiles.next() {
            preset_rows.push(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(first)
                    .children(tiles.next())
                    .into_any_element(),
            );
        }
        let file = match &self.workspace_path {
            Some(path) => div()
                .h(px(34.))
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(10.))
                .rounded(px(8.))
                .border_1()
                .border_color(rgb(BORDER))
                .bg(rgb(theme::WELL))
                .child(theme::icon(Icon::File, 14., MUTED))
                .child(
                    theme::mono(path.display().to_string(), 12., 0xd7dbe0)
                        .min_w_0()
                        .truncate(),
                ),
            None => div()
                .text_size(px(12.))
                .line_height(px(18.))
                .text_color(rgb(TEXT_2))
                .child("Demo workspace; started without --workspace, so Save has no file"),
        };
        let actions = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(14.))
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(BORDER_STRONG))
            .bg(rgb(CARD))
            .when(changed, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(7.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(theme::dot(WARN, 7.))
                                .child("Draft not applied"),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(18.))
                                .text_color(rgb(TEXT_2))
                                .child("Apply restarts the runtime with this draft. Until then the running devices do not change."),
                        ),
                )
            })
            .when(!changed, |this| {
                this.child(
                    div()
                        .text_size(px(12.))
                        .line_height(px(18.))
                        .text_color(rgb(TEXT_2))
                        .child("The draft matches the running devices. Save writes it and the address to the file."),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .when(changed, |row| {
                        row.child(
                            theme::button("apply-draft", Tone::Primary, 32.)
                                .child(theme::icon(Icon::Reload, 14., INK))
                                .child("Apply (restart)")
                                .on_click(cx.listener(|view, _, window, cx| view.restart(window, cx))),
                        )
                        .child(
                            theme::button("discard-draft", Tone::Secondary, 32.)
                                .child("Discard")
                                .on_click(cx.listener(|view, _, _, cx| view.discard_draft(cx))),
                        )
                    })
                    .child(div().flex_1().flex().justify_end().child(save)),
            )
            .children(self.panel_notice.clone().map(|notice| {
                div()
                    .text_size(px(12.))
                    .line_height(px(18.))
                    .text_color(rgb(WARN_TEXT))
                    .child(notice)
            }));
        div()
            .id("workspace-panel-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(18.))
            .p(px(16.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(theme::caps("WORKSPACE FILE", MUTED))
                    .child(file),
            )
            // The actions stay above the lists that may scroll: Apply and
            // Discard while the draft differs, Save always.
            .child(actions)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .pb(px(4.))
                            .child(theme::caps("DEVICES IN THE DRAFT", MUTED))
                            .child(theme::mono(self.draft.devices.len().to_string(), 11., MUTED)),
                    )
                    .children(devices),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(theme::caps("ADD A DEVICE", MUTED))
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(18.))
                            .text_color(rgb(TEXT_2))
                            .child("Into the selected device's session. Generic viewport classes; edit the file for exact sizes."),
                    )
                    .children(preset_rows),
            )
            .into_any_element()
    }

    /// The selected device's console (ADR 0023), newest first. It lives in
    /// memory for this runtime; Clear empties it for the device.
    fn console_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let can_save_report = self.reports.is_some()
            && !self.reports_loading
            && self.report.is_none()
            && !self.saving_report
            && self.lifecycle.is_live()
            && matches!(self.status.runtime, RuntimeState::Running { .. });
        let Some((device, status)) = self.selected.and_then(|index| {
            Some((
                self.workspace.devices.get(index)?,
                self.status.devices.get(index)?,
            ))
        }) else {
            return div()
                .flex_1()
                .p(px(16.))
                .text_color(rgb(MUTED))
                .child("Select a device to see its console.")
                .into_any_element();
        };
        let session = self
            .workspace
            .sessions
            .iter()
            .position(|session| session.id == device.session);
        let count = |count: u32, one: &str, many: &str, color: u32, text: u32| {
            div()
                .h(px(22.))
                .px(px(8.))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(5.))
                .rounded_full()
                .bg(theme::tint(color, 0x21))
                .text_size(px(11.5))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(rgb(text))
                .child(theme::dot(color, 5.))
                .child(plural(count, one, many))
        };
        let counts = div()
            .flex_1()
            .flex()
            .justify_end()
            .gap(px(6.))
            .when(status.console_errors > 0, |this| {
                this.child(count(
                    status.console_errors,
                    "error",
                    "errors",
                    DANGER,
                    DANGER_TEXT,
                ))
            })
            .when(status.console_warnings > 0, |this| {
                this.child(count(
                    status.console_warnings,
                    "warning",
                    "warnings",
                    WARN,
                    WARN_TEXT,
                ))
            })
            .when(
                status.console_errors == 0 && status.console_warnings == 0,
                |this| {
                    this.text_size(px(11.5))
                        .text_color(rgb(MUTED))
                        .child(console_counts(status))
                },
            );
        let (report_text, report_failed): (SharedString, bool) =
            match (&self.report_notice, &self.reports) {
                (Some((notice, failed)), _) => (notice.clone().into(), *failed),
                (None, _) if self.reports_loading => {
                    ("Locating the reports directory…".into(), false)
                }
                (None, Some(dir)) => (
                    format!(
                        "Save report writes a screenshot and a redacted report to {}.",
                        dir.display()
                    )
                    .into(),
                    false,
                ),
                (None, None) => (
                    "Save report needs BROXSER_REPORT_DIR or a home directory.".into(),
                    false,
                ),
            };
        let report_label = if self.saving_report {
            "Saving…"
        } else if self.report.is_some() {
            "Taking…"
        } else {
            "Save report"
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .pt(px(14.))
                    .px(px(16.))
                    .pb(px(16.))
                    .border_b_1()
                    .border_color(rgb(DIVIDER))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(theme::icon(
                                theme::device_icon(device.width, device.mobile),
                                15.,
                                TEXT_2,
                            ))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(14.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(device.name.clone()),
                            )
                            .children(session.map(|session| {
                                theme::tag(
                                    self.workspace.sessions[session].name.clone(),
                                    theme::session_hue(session),
                                )
                            }))
                            .child(counts),
                    )
                    // Filled, Save report in the accent and Clear in the
                    // danger color, so a real-window check can find each by
                    // its color alone.
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                theme::button(
                                    "console-save-report",
                                    if can_save_report {
                                        Tone::Primary
                                    } else {
                                        Tone::Disabled
                                    },
                                    32.,
                                )
                                .w(px(124.))
                                .child(theme::icon(
                                    Icon::Report,
                                    14.,
                                    if can_save_report { INK } else { MUTED },
                                ))
                                .child(report_label)
                                .when(can_save_report, |button| {
                                    button.on_click(
                                        cx.listener(|view, _, _, cx| view.save_report(cx)),
                                    )
                                }),
                            )
                            .child(
                                theme::button("console-clear", Tone::Danger, 32.)
                                    .child(theme::icon(Icon::Clear, 14., INK))
                                    .child("Clear")
                                    .on_click(cx.listener(|view, _, _, cx| view.clear_console(cx))),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(18.))
                            .text_color(rgb(if report_failed { WARN_TEXT } else { MUTED }))
                            .child(report_text),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .px(px(16.))
                    .py(px(10.))
                    .border_b_1()
                    .border_color(rgb(DIVIDER))
                    .text_size(px(11.5))
                    .text_color(rgb(MUTED))
                    .child("Newest first. Kept in memory until Clear or Restart."),
            )
            .child(
                div()
                    .id("console-entries")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(self.console.is_empty().then(|| {
                        div()
                            .p(px(16.))
                            .text_color(rgb(MUTED))
                            .child("No console messages.")
                    }))
                    .children(self.console.iter().rev().map(console_row)),
            )
            .into_any_element()
    }

    fn status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (color, label, detail) = match &self.status.runtime {
            _ if self.lifecycle.is_restarting() => (
                INFO,
                "Restarting…",
                Some("stopping the previous browser".to_owned()),
            ),
            RuntimeState::Starting => (INFO, "Starting browser…", None),
            RuntimeState::Running { product, protocol } => {
                (ACCENT, "Live", Some(format!("{product} · CDP {protocol}")))
            }
            RuntimeState::Stopped { error: Some(error) } => {
                (DANGER, "Stopped", Some(error.clone()))
            }
            RuntimeState::Stopped { error: None } => (MUTED, "Stopped", None),
        };
        let failed = matches!(
            self.status.runtime,
            RuntimeState::Stopped { error: Some(_) }
        ) && !self.lifecycle.is_restarting();
        let stopped =
            matches!(self.status.runtime, RuntimeState::Stopped { .. }) || self.session.is_none();
        let restart = stopped && self.lifecycle.is_live() && self.browser.is_some();
        let quiet = self.notice.is_none() && self.status.protocol_error.is_none();
        theme::status_bar()
            .child(theme::state(color, label))
            .children(detail.map(|detail| {
                theme::mono(detail, 11.5, if failed { DANGER_TEXT } else { TEXT_2 })
                    .min_w_0()
                    .truncate()
            }))
            .when(self.selected.is_none(), |bar| {
                bar.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(6.))
                        .text_color(rgb(TEXT_2))
                        .child(theme::icon(Icon::Hidden, 13., TEXT_2))
                        .child("All devices hidden"),
                )
            })
            .children(self.status.protocol_error.clone().map(|error| {
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(WARN_TEXT))
                    .child(format!("Input error: {error}"))
            }))
            .children(self.notice.clone().map(|notice| {
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_color(rgb(WARN_TEXT))
                    .child(theme::icon(Icon::Warning, 13., WARN))
                    .child(div().min_w_0().truncate().child(notice))
            }))
            .child(div().flex_1())
            // Offered only while no restart or close runs (ADR 0009).
            .when(restart, |bar| {
                bar.child(
                    theme::button("restart", Tone::Primary, 24.)
                        .pl(px(8.))
                        .pr(px(10.))
                        .child(theme::icon(Icon::Reload, 12., INK))
                        .child("Restart runtime")
                        .on_click(cx.listener(|view, _, window, cx| view.restart(window, cx))),
                )
            })
            .when(!restart && quiet, |bar| {
                bar.child(
                    div()
                        .flex()
                        .flex_none()
                        .gap(px(16.))
                        .pr(px(11.))
                        .child(theme::shortcut(&["Ctrl", "R"], "Reload selected"))
                        .child(theme::shortcut(&["Ctrl", "Shift", "J"], "Console"))
                        .child(theme::shortcut(&["Ctrl", "Shift", "W"], "Workspace")),
                )
            })
    }
}

/// A report on a card, below its frame: an icon and a title, then what the
/// caller adds.
fn notice(width: f32, glyph: Icon, title: &'static str) -> gpui::Div {
    div()
        .w(px(width))
        .p(px(11.))
        .rounded(px(10.))
        .border_1()
        .border_color(rgb(BORDER_STRONG))
        .bg(rgb(CHROME))
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div()
                .flex()
                .gap(px(7.))
                .text_size(px(12.))
                .line_height(px(16.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(0xd7dbe0))
                .child(div().pt(px(1.5)).child(theme::icon(glyph, 13., 0xd7dbe0)))
                .child(div().flex_1().min_w_0().child(title)),
        )
}

/// A quiet outlined label, such as the sessions' lifetime.
fn pill(label: &'static str) -> gpui::Div {
    div()
        .h(px(20.))
        .px(px(8.))
        .flex()
        .items_center()
        .rounded_full()
        .border_1()
        .border_color(rgb(BORDER_STRONG))
        .text_size(px(11.))
        .text_color(rgb(0xa4abb5))
        .child(label)
}

impl Render for LiveView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cards: Vec<AnyElement> = (0..self.workspace.devices.len())
            .filter(|&index| !self.devices[index].hidden)
            .map(|index| self.device_card(index, cx))
            .collect();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(CANVAS))
            .text_color(rgb(TEXT))
            .font_family(theme::SANS)
            .text_size(px(13.))
            .on_action(cx.listener(|view, _: &Quit, window, cx| {
                if view.request_close(window, cx) {
                    window.remove_window();
                }
            }))
            .on_action(cx.listener(|view, _: &Refresh, _, cx| view.reload_selected(cx)))
            .on_action(cx.listener(|view, _: &FocusUrl, window, cx| {
                view.cancel_touches();
                view.url.update(cx, |input, cx| input.focus_all(window, cx));
            }))
            .on_action(cx.listener(|view, _: &TogglePanel, _, cx| view.toggle_panel(cx)))
            .on_action(
                cx.listener(|view, _: &ToggleConsole, _, cx| view.toggle(SidePanel::Console, cx)),
            )
            .on_key_up(cx.listener(|view, event: &KeyUpEvent, _, _| {
                view.key_up(&event.keystroke);
            }))
            .child(self.toolbar(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.sidebar(cx))
                    .child(
                        div()
                            .id("canvas")
                            .track_focus(&self.focus)
                            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                                view.key_down(&event.keystroke, event.is_held, window, cx);
                                cx.stop_propagation();
                            }))
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_y_scroll()
                            .p(px(24.))
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_start()
                                    .gap(px(20.))
                                    .children(cards),
                            ),
                    )
                    .children(self.panel.map(|panel| self.inspector(panel, cx))),
            )
            .child(self.status_bar(cx))
    }
}

impl EntityInputHandler for LiveView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let (text, actual) = self.ime.text_for_range(range)?;
        *adjusted_range = Some(actual);
        Some(text)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.ime.selected_range(),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.ime.marked_range()
    }

    fn unmark_text(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        if let Some((origin, action)) = self.ime.unmark(self.ime_origin(window)) {
            self.dispatch_ime(origin, action);
        }
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        if !self.ime.accepts_replacement_range(range.as_ref()) {
            return;
        }
        if text.is_empty() {
            if let Some((origin, action)) = self.ime.terminal_delete() {
                self.dispatch_ime(origin, action);
            }
        } else {
            self.commit_ime(text, window);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selection: Option<Range<usize>>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        if !self.ime.accepts_replacement_range(range.as_ref()) {
            return;
        }
        if let Some((origin, action)) = self.ime.preedit(self.ime_origin(window), text, selection) {
            self.dispatch_ime(origin, action);
        }
    }

    fn bounds_for_range(
        &mut self,
        _range: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let origin = self.ime_origin(window)?;
        let caret = self.status.devices.get(origin.device)?.text_input?.caret;
        let device = &self.workspace.devices[origin.device];
        map_caret(
            caret,
            element_bounds,
            f64::from(device.width),
            f64::from(device.height),
        )
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.ime.selected_range().end)
    }
}

/// CSS viewport caret to painted GPUI pixels. The compositor receives only a
/// rectangle inside the visible device frame, even for stale or odd page data.
fn map_caret(
    caret: broxser_engine::CaretRect,
    bounds: Bounds<Pixels>,
    css_width: f64,
    css_height: f64,
) -> Option<Bounds<Pixels>> {
    if !(caret.x.is_finite()
        && caret.y.is_finite()
        && caret.width.is_finite()
        && caret.height.is_finite())
        || css_width <= 0.0
        || css_height <= 0.0
    {
        return None;
    }
    let x0 = caret.x.clamp(0.0, css_width);
    let y0 = caret.y.clamp(0.0, css_height);
    let x1 = (caret.x + caret.width.max(0.0)).clamp(x0, css_width);
    let y1 = (caret.y + caret.height.max(0.0)).clamp(y0, css_height);
    let sx = bounds.size.width.to_f64() / css_width;
    let sy = bounds.size.height.to_f64() / css_height;
    Some(Bounds::from_corners(
        gpui::point(
            px(bounds.origin.x.to_f64() as f32 + (x0 * sx) as f32),
            px(bounds.origin.y.to_f64() as f32 + (y0 * sy) as f32),
        ),
        gpui::point(
            px(bounds.origin.x.to_f64() as f32 + (x1 * sx) as f32),
            px(bounds.origin.y.to_f64() as f32 + (y1 * sy) as f32),
        ),
    ))
}

fn modifiers_of(modifiers: &gpui::Modifiers) -> Modifiers {
    Modifiers {
        alt: modifiers.alt,
        control: modifiers.control,
        meta: modifiers.platform,
        shift: modifiers.shift,
    }
}

/// GPUI does not expose a physical keycode. Normalize the shifted ASCII pairs
/// whose X11 key names can change before key-up; prefer CDP codes elsewhere.
fn logical_key_identity(keystroke: &Keystroke, key: &KeyInput) -> String {
    let shifted_pair = match keystroke.key.as_str() {
        "1" | "!" => Some("Digit1"),
        "2" | "@" => Some("Digit2"),
        "3" | "#" => Some("Digit3"),
        "4" | "$" => Some("Digit4"),
        "5" | "%" => Some("Digit5"),
        "6" | "^" => Some("Digit6"),
        "7" | "&" => Some("Digit7"),
        "8" | "*" => Some("Digit8"),
        "9" | "(" => Some("Digit9"),
        "0" | ")" => Some("Digit0"),
        "-" | "_" => Some("Minus"),
        "=" | "+" => Some("Equal"),
        "[" | "{" => Some("BracketLeft"),
        "]" | "}" => Some("BracketRight"),
        "\\" | "|" => Some("Backslash"),
        ";" | ":" => Some("Semicolon"),
        "'" | "\"" => Some("Quote"),
        "," | "<" => Some("Comma"),
        "." | ">" => Some("Period"),
        "/" | "?" => Some("Slash"),
        "`" | "~" => Some("Backquote"),
        _ => None,
    };
    shifted_pair
        .or_else(|| (!key.code.is_empty()).then_some(key.code.as_str()))
        .unwrap_or(&keystroke.key)
        .to_owned()
}

/// The key event for a GPUI keystroke and the name its press is tracked by.
fn key_input(keystroke: &Keystroke, down: bool) -> Option<(KeyInput, String)> {
    let key = KeyInput::from_key(
        &keystroke.key,
        keystroke.key_char.as_deref(),
        modifiers_of(&keystroke.modifiers),
        down,
    )?;
    let identity = logical_key_identity(keystroke, &key);
    Some((key, identity))
}

/// The device a key-down goes to. A repeat goes only to the page that
/// received the press, never to one selected or shown later.
fn key_down_target(
    selected: Option<usize>,
    visible: impl Fn(usize) -> bool,
    identity: &str,
    repeat: bool,
    held: &HashMap<String, (usize, KeyInput)>,
) -> Option<usize> {
    let device = selected.filter(|&index| visible(index))?;
    match held.get(identity) {
        Some((owner, _)) if *owner != device => None,
        None if repeat => None,
        _ => Some(device),
    }
}

/// What a key-down that reached the canvas does.
#[derive(Debug, PartialEq, Eq)]
enum CanvasKey {
    /// No page gets the key.
    Drop,
    /// The key belongs to the open IME composition.
    Compose,
    /// Pastes the system clipboard's text into the selected page.
    Paste,
    /// The key goes to the page of this device.
    Send(usize),
}

/// The canvas also receives the keys that a field inside it, such as a
/// prompt's text field, leaves unhandled; they belong to no page. A key goes to
/// the selected page (`target`, if shown) only while the canvas itself holds
/// focus, and a repeat only to the page that received its press.
fn canvas_key(
    focus: KeyFocus,
    composing: bool,
    key: &KeyInput,
    identity: &str,
    repeat: bool,
    target: Option<usize>,
    held: &HashMap<String, (usize, KeyInput)>,
) -> CanvasKey {
    if focus != KeyFocus::Canvas {
        CanvasKey::Drop
    } else if composing {
        CanvasKey::Compose
    } else if is_paste_key(key) {
        if repeat {
            CanvasKey::Drop
        } else {
            CanvasKey::Paste
        }
    } else {
        key_down_target(target, |_| true, identity, repeat, held)
            .map_or(CanvasKey::Drop, CanvasKey::Send)
    }
}

/// A prompt's proposed answer as its field's first text. The engine takes an
/// answer only as one line of at most [`MAX_DIALOG_CHARS`] characters without
/// control characters, so line breaks and tabs become spaces.
fn prompt_default(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\n' | '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .take(MAX_DIALOG_CHARS)
        .collect()
}

/// The dialog device `index` waits on, while the held runtime runs. A status
/// from before a stop, restart or close can still name a dialog that nothing
/// can answer any more.
fn open_dialog(status: &Status, held: bool, index: usize) -> Option<&DialogState> {
    let running = held && matches!(status.runtime, RuntimeState::Running { .. });
    status
        .devices
        .get(index)?
        .dialog
        .as_ref()
        .filter(|_| running)
}

/// Where keyboard focus goes when device `index` gets a field for a new prompt
/// (`created`) or loses its field. A new prompt of the selected device takes
/// focus from the canvas or from that device's previous field, never from the
/// URL bar or another device's field. Focus in a field that goes away returns
/// to the canvas, so shortcuts and key releases keep a receiver. `None` leaves
/// focus where it is.
fn focus_after_prompt_change(
    index: usize,
    selected: Option<usize>,
    focus: KeyFocus,
    created: bool,
) -> Option<KeyFocus> {
    let in_own_field = focus == KeyFocus::Prompt(index);
    if created && selected == Some(index) && (focus == KeyFocus::Canvas || in_own_field) {
        Some(KeyFocus::Prompt(index))
    } else if in_own_field {
        Some(KeyFocus::Canvas)
    } else {
        None
    }
}

/// The answer that Enter or Escape in the prompt field of dialog `token` of
/// `device` sends: Enter accepts with the field's text exactly as typed, and
/// Escape cancels, as the Cancel button does.
fn field_answer(device: usize, token: u64, event: &UrlEvent) -> Command {
    let (accept, text) = match event {
        UrlEvent::Submit(text) => (true, Some(text.clone())),
        UrlEvent::Cancel => (false, None),
    };
    Command::AnswerDialog {
        device,
        token,
        accept,
        text,
    }
}

/// Keep the current visible device, otherwise move forward through the sidebar
/// order with wraparound. No visible device means no page input target.
fn selected_after_visibility_change(selected: Option<usize>, hidden: &[bool]) -> Option<usize> {
    if selected.is_some_and(|index| hidden.get(index) == Some(&false)) {
        return selected;
    }
    let start = selected.map_or(0, |index| index.saturating_add(1));
    (start..hidden.len())
        .chain(0..start.min(hidden.len()))
        .find(|&index| !hidden[index])
}

/// Resolve a decode only in its own runtime, before inspecting any device slot:
/// applying a draft can shrink or reorder the list while decoding is in flight.
fn frame_device<'a>(
    devices: &'a mut [DeviceView],
    lifecycle: &Lifecycle,
    index: usize,
    generation: u64,
) -> Option<&'a mut DeviceView> {
    if !lifecycle.accepts(generation) {
        return None;
    }
    let device = devices.get_mut(index)?;
    if device.decoding == Some(generation) {
        device.decoding = None;
    }
    Some(device)
}

/// At most three file operations run: one workspace save, report operation
/// (directory discovery or writing) and close-time state save.
/// Give blocking filesystem calls their own threads so they cannot occupy
/// GPUI's finite background pool and delay frame decoding or browser teardown,
/// even on a single-CPU machine. Spawn failures and panics complete the waiter.
fn save_off_thread<T: Send + 'static>(
    save: impl FnOnce() -> T + Send + 'static,
) -> impl Future<Output = Result<T>> {
    let (done, result) = oneshot::channel();
    let started = std::thread::Builder::new()
        .name("broxser-save".into())
        .spawn(move || {
            let _ = done.send(save());
        });
    async move {
        started.context("start save worker")?;
        result.await.context("save worker stopped before reporting")
    }
}

fn decode(frame: Frame) -> Result<Arc<RenderImage>> {
    let mut pixels = image::load_from_memory_with_format(&frame.jpeg, image::ImageFormat::Jpeg)
        .context("decode live frame")?
        .into_rgba8();
    // GPUI images are BGRA.
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])))
}

/// ` @2×` for a device scale factor other than 1, else nothing.
fn scale_suffix(scale: f64) -> String {
    if scale == 1.0 {
        String::new()
    } else {
        format!(" @{scale}×")
    }
}

/// "1 error", "2 errors".
fn plural(count: u32, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// "2 errors · 1 warning", or that there are none.
fn console_counts(status: &DeviceStatus) -> String {
    match (status.console_errors, status.console_warnings) {
        (0, 0) => "No errors or warnings".into(),
        (errors, 0) => plural(errors, "error", "errors"),
        (0, warnings) => plural(warnings, "warning", "warnings"),
        (errors, warnings) => format!(
            "{} · {}",
            plural(errors, "error", "errors"),
            plural(warnings, "warning", "warnings")
        ),
    }
}

/// One console entry as the panel shows it: its level, what reported it,
/// its text and where it came from. Page text is shown, never interpreted.
fn console_row(entry: &ConsoleEntry) -> AnyElement {
    let row = div()
        .flex()
        .gap(px(10.))
        .px(px(16.))
        .border_b_1()
        .border_color(rgb(DIVIDER));
    if entry.kind == ConsoleKind::Navigation {
        return row
            .items_center()
            .py(px(9.))
            .text_size(px(11.5))
            .text_color(rgb(MUTED))
            .child(theme::icon(Icon::Navigated, 14., MUTED))
            .child(div().flex_none().child("Navigated to"))
            .child(
                theme::mono(entry.location.clone(), 11., 0xa4abb5)
                    .min_w_0()
                    .truncate(),
            )
            .into_any_element();
    }
    let (level, glyph, color, label) = match entry.level {
        ConsoleLevel::Error => ("Error", Icon::Error, DANGER, DANGER_TEXT),
        ConsoleLevel::Warning => ("Warning", Icon::Warning, WARN, WARN_TEXT),
        ConsoleLevel::Info => ("Info", Icon::Info, MUTED, 0xa4abb5),
    };
    let mut tags: Vec<String> = Vec::new();
    match entry.kind {
        ConsoleKind::Console | ConsoleKind::Navigation => {}
        ConsoleKind::Exception => tags.push("uncaught".into()),
        ConsoleKind::Network => tags.push("request".into()),
        ConsoleKind::Browser => tags.push("browser".into()),
    }
    match entry.scope {
        ConsoleScope::MainFrame => {}
        ConsoleScope::Subframe => tags.push("frame".into()),
        ConsoleScope::Unknown => tags.push("frame unknown".into()),
    }
    if entry.repeats > 1 {
        tags.push(format!("×{}", entry.repeats));
    }
    row.py(px(10.))
        // Errors and warnings get a faint wash of their color.
        .when(entry.level != ConsoleLevel::Info, |row| {
            row.bg(theme::tint(color, 0x0d))
        })
        .child(div().pt(px(1.)).child(theme::icon(glyph, 14., color)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_size(px(11.5))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(rgb(label))
                                .child(level),
                        )
                        .children(tags.into_iter().map(|tag| {
                            theme::mono(tag, 10.5, TEXT_2)
                                .h(px(18.))
                                .px(px(6.))
                                .flex()
                                .items_center()
                                .rounded(px(5.))
                                .border_1()
                                .border_color(rgb(0x2f343b))
                        })),
                )
                .child(
                    theme::mono(entry.text.clone(), 12., TEXT)
                        .w_full()
                        .line_height(px(18.)),
                )
                .children(
                    (!entry.location.is_empty())
                        .then(|| theme::mono(entry.location.clone(), 11., MUTED).truncate()),
                ),
        )
        .into_any_element()
}

/// What the page tried that Broxser closed or refused (ADR 0015, ADR 0016),
/// or nothing.
fn activity_line(status: &DeviceStatus) -> Option<String> {
    let parts: Vec<String> = [
        (status.popups, "window closed", "windows closed"),
        (status.downloads, "download refused", "downloads refused"),
        (
            status.file_choosers,
            "file chooser cancelled",
            "file choosers cancelled",
        ),
    ]
    .into_iter()
    .filter(|(count, _, _)| *count > 0)
    .map(|(count, one, many)| plural(count, one, many))
    .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::{
        CanvasKey, DeviceView, KeyFocus, PressedKeys, activity_line, canvas_key, console_counts,
        field_answer, focus_after_prompt_change, frame_device, key_down_target, key_input,
        map_caret, open_dialog, prompt_default, selected_after_visibility_change,
    };
    use crate::ime::{ImeBuffer, Origin};
    use crate::lifecycle::{AfterStop, Lifecycle};
    use crate::url_input::{KeyOutcome, LineEdit, UrlEvent};
    use broxser_engine::{
        CaretRect, Command, DeviceStatus, DialogKind, DialogState, ImeAction, MAX_DIALOG_CHARS,
        RuntimeState, Status, TextInputState,
    };
    use gpui::{Bounds, Keystroke, Modifiers, point, px, size};
    use std::collections::HashMap;

    /// A keystroke as GPUI reports it on X11: key name and typed character.
    fn stroke(key: &str, key_char: Option<&str>) -> Keystroke {
        Keystroke {
            key: key.into(),
            key_char: key_char.map(Into::into),
            ..Keystroke::default()
        }
    }

    fn identity(name: &str) -> String {
        key_input(&stroke(name, Some(name)), true).unwrap().1
    }

    /// A keystroke without text, such as a shortcut, as GPUI reports it on X11.
    fn chord(modifiers: Modifiers, key: &str) -> Keystroke {
        Keystroke {
            key: key.into(),
            modifiers,
            ..Keystroke::default()
        }
    }

    const NO_MODIFIERS: Modifiers = Modifiers {
        control: false,
        alt: false,
        shift: false,
        platform: false,
        function: false,
    };
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..NO_MODIFIERS
    };
    const CONTROL: Modifiers = Modifiers {
        control: true,
        ..NO_MODIFIERS
    };

    fn answer_of(command: Command) -> (usize, u64, bool, Option<String>) {
        match command {
            Command::AnswerDialog {
                device,
                token,
                accept,
                text,
            } => (device, token, accept, text),
            other => panic!("not a dialog answer: {other:?}"),
        }
    }

    #[test]
    fn decoded_frames_cannot_touch_devices_replaced_by_apply() {
        let mut lifecycle = Lifecycle::default();
        let old = lifecycle.generation();
        let mut devices: Vec<_> = (0..3)
            .map(|_| DeviceView {
                decoding: Some(old),
                ..DeviceView::default()
            })
            .collect();
        assert!(lifecycle.begin_restart());
        // Apply removes a device, leaving callbacks for all three old slots.
        devices.remove(0);
        for device in &mut devices {
            *device = DeviceView::default();
        }
        assert!(frame_device(&mut devices, &lifecycle, 2, old).is_none());
        assert_eq!(lifecycle.stopped(), AfterStop::Start);
        let current = lifecycle.generation();
        devices[0].decoding = Some(current);
        assert!(frame_device(&mut devices, &lifecycle, 2, old).is_none());
        assert!(frame_device(&mut devices, &lifecycle, 0, old).is_none());
        assert_eq!(devices[0].decoding, Some(current));
        // Missing indices are harmless even when tagged with this generation.
        assert!(frame_device(&mut devices, &lifecycle, 2, current).is_none());
        assert!(frame_device(&mut devices, &lifecycle, 0, current).is_some());
        assert_eq!(devices[0].decoding, None);
    }

    #[test]
    fn a_queued_old_snapshot_cannot_rearm_ime_after_pointer_down() {
        let snapshot = |target| DeviceStatus {
            text_input: Some(TextInputState {
                target,
                caret: CaretRect {
                    x: 10.,
                    y: 20.,
                    width: 1.,
                    height: 14.,
                },
            }),
            ..DeviceStatus::default()
        };
        let mut device = DeviceView::default();
        let mut status = snapshot(4);
        device.invalidate_ime_target(&mut status);
        let origin = |device: &DeviceView, status: &DeviceStatus| {
            device.ime_target(status).map(|target| Origin {
                device: 0,
                target,
                generation: 1,
            })
        };
        let mut composition = ImeBuffer::default();
        // A wake-up already queued before the pointer command restores the old
        // shared status. It must not latch a composition to that retired target.
        status = snapshot(4);
        assert_eq!(
            composition.preedit(origin(&device, &status), "n", None),
            None
        );
        // The worker processes the pointer and publishes its new target. The
        // full native preedit recovers without losing the word's initial keys.
        status = snapshot(7);
        let current = origin(&device, &status).unwrap();
        assert!(composition.preedit(Some(current), "ni hao", None).is_some());
        assert_eq!(
            composition.commit(Some(current), "你好"),
            Some((
                current,
                ImeAction::Commit {
                    text: "你好".into()
                }
            ))
        );
    }

    #[test]
    fn caret_maps_css_to_painted_frame_and_clips_outside_viewport() {
        let frame = Bounds {
            origin: point(px(100.), px(50.)),
            size: size(px(200.), px(400.)),
        };
        let caret = CaretRect {
            x: 25.,
            y: 50.,
            width: 2.,
            height: 10.,
        };
        let mapped = map_caret(caret, frame, 100., 200.).unwrap();
        assert_eq!(mapped.origin, point(px(150.), px(150.)));
        assert_eq!(mapped.size, size(px(4.), px(20.)));
        let clipped = map_caret(
            CaretRect {
                x: 150.,
                y: -20.,
                width: 3.,
                height: 25.,
            },
            frame,
            100.,
            200.,
        )
        .unwrap();
        assert_eq!(clipped.origin, point(px(300.), px(50.)));
        assert_eq!(clipped.size, size(px(0.), px(10.)));
        assert!(
            map_caret(
                CaretRect {
                    x: f64::NAN,
                    ..caret
                },
                frame,
                100.,
                200.
            )
            .is_none()
        );
    }

    fn press(pressed: &mut PressedKeys, key: &str, key_char: Option<&str>) -> String {
        let (input, identity) = key_input(&stroke(key, key_char), true).unwrap();
        pressed.press(identity.clone(), &input);
        identity
    }

    fn release(pressed: &mut PressedKeys, key: &str, key_char: Option<&str>) -> Option<String> {
        let (_, identity) = key_input(&stroke(key, key_char), false).unwrap();
        pressed.release(&identity)
    }

    #[test]
    fn hiding_selected_device_moves_to_next_visible_with_wraparound() {
        assert_eq!(
            selected_after_visibility_change(Some(0), &[true, false, false]),
            Some(1)
        );
        assert_eq!(
            selected_after_visibility_change(Some(1), &[false, true, true]),
            Some(0)
        );
        assert_eq!(
            selected_after_visibility_change(Some(1), &[false, false, true]),
            Some(1)
        );
    }

    #[test]
    fn hiding_every_device_clears_selection_until_one_is_shown() {
        assert_eq!(
            selected_after_visibility_change(Some(0), &[true, true]),
            None
        );
        assert_eq!(selected_after_visibility_change(None, &[true, true]), None);
        assert_eq!(
            selected_after_visibility_change(None, &[true, false]),
            Some(1)
        );
    }

    #[test]
    fn a_released_key_cannot_repeat_into_the_next_device_even_without_is_held() {
        let mut pressed = PressedKeys::default();
        let name = press(&mut pressed, "a", Some("a"));
        let (input, _) = key_input(&stroke("a", Some("a")), true).unwrap();
        let mut held = HashMap::new();
        assert!(!pressed.repeating(&name));
        assert_eq!(
            key_down_target(Some(0), |_| true, &name, false, &held),
            Some(0)
        );
        held.insert(name.clone(), (0, input));
        // X11 reports a repeat as another key-down with is_held=false.
        press(&mut pressed, "a", Some("a"));
        assert!(pressed.repeating(&name));
        assert_eq!(
            key_down_target(Some(0), |_| true, &name, true, &held),
            Some(0)
        );

        // Hiding device 0 sends keyUp to it, then device 1 becomes selected.
        held.remove(&name);
        press(&mut pressed, "a", Some("a"));
        assert!(pressed.repeating(&name));
        assert_eq!(key_down_target(Some(1), |_| true, &name, true, &held), None);
        // The physical keyUp ends the press; a fresh press may route.
        assert_eq!(release(&mut pressed, "a", Some("a")), Some(name.clone()));
        press(&mut pressed, "a", Some("a"));
        assert!(!pressed.repeating(&name));
        assert_eq!(
            key_down_target(Some(1), |_| true, &name, false, &held),
            Some(1)
        );
        assert_eq!(key_down_target(None, |_| true, &name, false, &held), None);
        assert_eq!(
            key_down_target(Some(0), |_| false, &name, false, &held),
            None
        );
    }

    #[test]
    fn only_the_latest_key_down_repeats() {
        let mut pressed = PressedKeys::default();
        let a = press(&mut pressed, "a", Some("a"));
        let b = press(&mut pressed, "b", Some("b"));
        press(&mut pressed, "b", Some("b"));
        assert!(pressed.repeating(&b));
        // "a" is still down, yet a key-down of it after "b" is a new press.
        press(&mut pressed, "a", Some("a"));
        assert!(!pressed.repeating(&a));
        // German "/" held until it repeats, Shift released first: the repeats
        // are named "7", and so is the release. The next "/" is a new press.
        let slash = press(&mut pressed, "/", Some("/"));
        press(&mut pressed, "/", Some("/"));
        let seven = press(&mut pressed, "7", Some("7"));
        assert_eq!(release(&mut pressed, "7", Some("7")), Some(seven));
        press(&mut pressed, "/", Some("/"));
        assert!(!pressed.repeating(&slash));
        assert_eq!(release(&mut pressed, "7", Some("7")), Some(slash));
    }

    #[test]
    fn a_release_under_another_name_ends_the_press_it_belongs_to() {
        let mut pressed = PressedKeys::default();
        // German Shift+7 types "/"; with Shift up first the release is "7".
        let slash = press(&mut pressed, "/", Some("/"));
        assert_eq!(release(&mut pressed, "7", Some("7")), Some(slash));
        // A composed character is released as its base key.
        let e_acute = press(&mut pressed, "eacute", Some("é"));
        assert_eq!(release(&mut pressed, "e", Some("e")), Some(e_acute));
        // AltGr+Q types "@"; with AltGr up first the release is "q". The
        // letter pressed later keeps its name, so the release is not its.
        let at = press(&mut pressed, "@", Some("@"));
        let a = press(&mut pressed, "a", Some("a"));
        assert_eq!(release(&mut pressed, "q", Some("q")), Some(at));
        // A dead key has no character and is released under its own name.
        let dead = press(&mut pressed, "=", None);
        assert_eq!(release(&mut pressed, "=", None), Some(dead));
        // Nothing renamable is down: a release from elsewhere ends nothing.
        let enter = press(&mut pressed, "enter", None);
        assert_eq!(release(&mut pressed, "7", Some("7")), None);
        assert_eq!(release(&mut pressed, "a", Some("a")), Some(a));
        assert_eq!(release(&mut pressed, "enter", None), Some(enter));
        assert!(pressed.keys.is_empty());
    }

    #[test]
    fn a_release_prefers_its_own_name_then_the_latest_renamable_press() {
        let mut pressed = PressedKeys::default();
        let slash = press(&mut pressed, "/", Some("/"));
        let paren = press(&mut pressed, "(", Some("("));
        assert_eq!(release(&mut pressed, "/", Some("/")), Some(slash.clone()));
        let slash = press(&mut pressed, "/", Some("/"));
        assert_eq!(release(&mut pressed, "7", Some("7")), Some(slash));
        assert_eq!(release(&mut pressed, "8", Some("8")), Some(paren));
    }

    #[test]
    fn common_shifted_punctuation_matches_the_same_release() {
        for (plain, shifted) in [("1", "!"), ("/", "?"), ("[", "{"), ("'", "\"")] {
            assert_eq!(identity(plain), identity(shifted));
        }
    }

    #[test]
    fn keys_a_prompt_field_leaves_unhandled_reach_no_page() {
        let mut field = LineEdit::new("Ada".into(), Some(MAX_DIALOG_CHARS));
        let held = HashMap::new();
        for keystroke in [
            stroke("tab", None),
            chord(SHIFT, "insert"),
            chord(CONTROL, "x"),
            stroke("up", None),
        ] {
            // The field does not handle them, so GPUI hands them on to the
            // canvas that contains it.
            assert_eq!(
                field.key(&keystroke, || Some("clipboard".into())),
                KeyOutcome::Unhandled
            );
            let (key, identity) = key_input(&keystroke, true).unwrap();
            // With device 1's prompt field focused, no page gets a key, a
            // paste or text: not the selected device 0, nor device 1 when it
            // is the selected one, and not as part of a composition either.
            for target in [Some(0), Some(1)] {
                for composing in [false, true] {
                    assert_eq!(
                        canvas_key(
                            KeyFocus::Prompt(1),
                            composing,
                            &key,
                            &identity,
                            false,
                            target,
                            &held
                        ),
                        CanvasKey::Drop,
                        "{}",
                        keystroke.key
                    );
                }
            }
            // Only while the canvas itself holds focus are they the page's.
            let page = if keystroke.key == "insert" {
                CanvasKey::Paste
            } else {
                CanvasKey::Send(0)
            };
            assert_eq!(
                canvas_key(
                    KeyFocus::Canvas,
                    false,
                    &key,
                    &identity,
                    false,
                    Some(0),
                    &held
                ),
                page
            );
        }
        assert_eq!(field.text(), "Ada");
    }

    #[test]
    fn a_prompt_default_becomes_one_bounded_line() {
        let proposal = format!("two\nlines\tand a bell\u{7}\r\n{}", "x".repeat(3000));
        let text = prompt_default(&proposal);
        assert!(text.starts_with("two lines and a bell x"), "{text:.30}");
        assert_eq!(text.chars().count(), MAX_DIALOG_CHARS);
        assert!(!text.chars().any(char::is_control));
        // The prompt field starts with it and has no room for typing or paste.
        let mut field = LineEdit::new(text.clone(), Some(MAX_DIALOG_CHARS));
        assert_eq!(
            field.key(&stroke("y", Some("y")), || None),
            KeyOutcome::Edited
        );
        field.key(&chord(CONTROL, "v"), || Some("pasted".into()));
        assert_eq!(field.text(), text);
    }

    #[test]
    fn enter_answers_with_the_exact_field_text_and_escape_cancels() {
        let mut field = LineEdit::new(prompt_default("  padded  "), Some(MAX_DIALOG_CHARS));
        let KeyOutcome::Event(enter) = field.key(&stroke("enter", None), || None) else {
            panic!("Enter belongs to the field");
        };
        assert_eq!(
            answer_of(field_answer(1, 7, &enter)),
            (1, 7, true, Some("  padded  ".into()))
        );
        let KeyOutcome::Event(escape) = field.key(&stroke("escape", None), || None) else {
            panic!("Escape belongs to the field");
        };
        assert_eq!(answer_of(field_answer(1, 7, &escape)), (1, 7, false, None));
    }

    #[test]
    fn an_ignored_repeat_restores_select_all_so_typing_still_replaces() {
        let (key, identity) = key_input(&stroke("enter", None), true).unwrap();
        let mut pressed = PressedKeys::default();
        // The held Enter that answered the previous prompt.
        pressed.press(identity.clone(), &key);
        assert!(!pressed.latest_repeats());
        // The new prompt's field, as a fresh auto-focus or click leaves it.
        let mut field = LineEdit::new(prompt_default("old proposal"), Some(MAX_DIALOG_CHARS));
        field.restore_select_all();
        // The still-held key repeats into it. LineEdit::key runs before any
        // guard can reject the repeat, and clears select-all as if this
        // Enter would submit the field.
        pressed.press(identity.clone(), &key);
        assert!(pressed.latest_repeats());
        assert_eq!(
            field.key(&stroke("enter", None), || None),
            KeyOutcome::Event(UrlEvent::Submit("old proposal".into()))
        );
        // A caller that checks latest_repeats(), as the prompt field's
        // subscriber does, ignores this Enter and must restore select-all;
        // otherwise the next key would append to the stale proposal instead
        // of replacing it.
        assert!(pressed.latest_repeats());
        field.restore_select_all();
        assert_eq!(
            field.key(&stroke("x", Some("x")), || None),
            KeyOutcome::Edited
        );
        assert_eq!(field.text(), "x");
    }

    #[test]
    fn a_new_prompt_takes_focus_only_from_the_canvas_for_the_selected_device() {
        let created =
            |index, selected, focus| focus_after_prompt_change(index, selected, focus, true);
        assert_eq!(
            created(0, Some(0), KeyFocus::Canvas),
            Some(KeyFocus::Prompt(0))
        );
        // A prompt on another device, or with every device hidden, leaves
        // focus in the canvas.
        assert_eq!(created(1, Some(0), KeyFocus::Canvas), None);
        assert_eq!(created(0, None, KeyFocus::Canvas), None);
        // The URL bar, or nothing focused, keeps focus; so does another
        // device's prompt field the user is typing in.
        assert_eq!(created(0, Some(0), KeyFocus::Elsewhere), None);
        assert_eq!(created(0, Some(0), KeyFocus::Prompt(1)), None);
        // The next prompt of a device follows focus out of its previous
        // field, which goes; on a device that is not selected, focus returns
        // to the canvas instead.
        assert_eq!(
            created(0, Some(0), KeyFocus::Prompt(0)),
            Some(KeyFocus::Prompt(0))
        );
        assert_eq!(
            created(1, Some(0), KeyFocus::Prompt(1)),
            Some(KeyFocus::Canvas)
        );
        // A focused field that goes away hands focus back to the canvas;
        // dropping a field without focus moves nothing.
        let dropped = |focus| focus_after_prompt_change(0, Some(0), focus, false);
        assert_eq!(dropped(KeyFocus::Prompt(0)), Some(KeyFocus::Canvas));
        assert_eq!(dropped(KeyFocus::Canvas), None);
        assert_eq!(dropped(KeyFocus::Elsewhere), None);
        assert_eq!(dropped(KeyFocus::Prompt(1)), None);
    }

    #[test]
    fn a_dialog_is_shown_only_while_the_held_runtime_runs() {
        let dialog = DialogState {
            kind: DialogKind::Prompt,
            message: "Name?".into(),
            default_text: "Ada".into(),
            token: 3,
        };
        let mut status = Status {
            runtime: RuntimeState::Running {
                product: "Helium".into(),
                protocol: "1.3".into(),
            },
            devices: vec![
                DeviceStatus::default(),
                DeviceStatus {
                    dialog: Some(dialog.clone()),
                    ..DeviceStatus::default()
                },
            ],
            ..Status::default()
        };
        assert_eq!(open_dialog(&status, true, 1), Some(&dialog));
        assert_eq!(open_dialog(&status, true, 0), None);
        assert_eq!(open_dialog(&status, true, 2), None);
        // A runtime taken for a restart or close answers nothing any more.
        assert_eq!(open_dialog(&status, false, 1), None);
        // Nor does a stopped runtime whose last status still names the dialog.
        status.runtime = RuntimeState::Stopped {
            error: Some("The browser exited".into()),
        };
        assert_eq!(open_dialog(&status, true, 1), None);
        status.runtime = RuntimeState::Starting;
        assert_eq!(open_dialog(&status, true, 1), None);
    }

    #[test]
    fn the_enter_after_a_prompt_answer_reaches_the_page() {
        let (key, identity) = key_input(&stroke("enter", None), true).unwrap();
        let held = HashMap::new();
        // Enter moves focus to the canvas at once and the field goes when the
        // dialog closes; answered with a button instead, the field can go
        // while it still holds focus.
        for field_focused_when_dropped in [false, true] {
            let mut pressed = PressedKeys::default();
            // Enter in the prompt field of device 0: recorded before any
            // element sees it, answered by the field, sent to no page.
            pressed.press(identity.clone(), &key);
            assert!(!pressed.latest_repeats());
            assert_eq!(
                canvas_key(
                    KeyFocus::Prompt(0),
                    false,
                    &key,
                    &identity,
                    false,
                    Some(0),
                    &held
                ),
                CanvasKey::Drop
            );
            let before = if field_focused_when_dropped {
                KeyFocus::Prompt(0)
            } else {
                KeyFocus::Canvas
            };
            // Either way the canvas holds focus once the field is gone, so the
            // release still reaches the window root.
            let focus = focus_after_prompt_change(0, Some(0), before, false).unwrap_or(before);
            assert_eq!(focus, KeyFocus::Canvas);
            // Repeats of the held Enter reach no page and answer no prompt.
            pressed.press(identity.clone(), &key);
            assert!(pressed.latest_repeats());
            assert_eq!(
                canvas_key(focus, false, &key, &identity, true, Some(0), &held),
                CanvasKey::Drop
            );
            // The release ends the press, so it no longer repeats either.
            assert_eq!(pressed.release(&identity), Some(identity.clone()));
            assert!(!pressed.latest_repeats());
            // The next Enter is a new press for the selected page.
            pressed.press(identity.clone(), &key);
            let repeat = pressed.repeating(&identity);
            assert!(!repeat && !pressed.latest_repeats());
            assert_eq!(
                canvas_key(focus, false, &key, &identity, repeat, Some(0), &held),
                CanvasKey::Send(0)
            );
        }
    }

    #[test]
    fn activity_line_names_only_what_happened() {
        let mut status = DeviceStatus {
            streaming: true,
            ..DeviceStatus::default()
        };
        assert_eq!(activity_line(&status), None);
        status.popups = 2;
        status.downloads = 1;
        status.file_choosers = 3;
        assert_eq!(
            activity_line(&status).as_deref(),
            Some("2 windows closed · 1 download refused · 3 file choosers cancelled")
        );
        status.popups = 1;
        status.downloads = 0;
        status.file_choosers = 1;
        assert_eq!(
            activity_line(&status).as_deref(),
            Some("1 window closed · 1 file chooser cancelled")
        );
    }

    #[test]
    fn console_counts_name_errors_and_warnings() {
        let counts = |console_errors, console_warnings| {
            console_counts(&DeviceStatus {
                console_errors,
                console_warnings,
                ..DeviceStatus::default()
            })
        };
        assert_eq!(counts(0, 0), "No errors or warnings");
        assert_eq!(counts(1, 0), "1 error");
        assert_eq!(counts(0, 1), "1 warning");
        assert_eq!(counts(2, 3), "2 errors · 3 warnings");
        assert_eq!(counts(1, 1), "1 error · 1 warning");
    }
}
