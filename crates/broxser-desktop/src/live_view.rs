//! Live device canvas. Frames are CDP screencast JPEGs from a headless browser,
//! decoded off the UI thread into GPUI images: frame streaming, not an embedded
//! browser surface. Pointer and wheel input go to the device under the pointer
//! and keys to the selected device, in CSS pixels of that device's viewport.

use crate::ime::{ImeBuffer, Origin};
use crate::lifecycle::{AfterStop, CloseRequest, Lifecycle};
use crate::url_input::{UrlEvent, UrlInput};
use crate::{ACCENT, BG, BORDER, FocusUrl, MUTED, Quit, RAISED, Refresh, SURFACE, TEXT, WARN};
use anyhow::{Context as _, Result};
use broxser_core::{Workspace, validate_url};
use broxser_engine::{
    BrowserOptions, Cancellation, Command, DialogKind, DialogState, Frame, ImeAction, KeyInput,
    LiveSession, MAX_PASTE_CHARS, Modifiers, PasteRejected, PointerButton, PointerEvent,
    PointerKind, RuntimeState, Status, SyncSettings, is_paste_key, paste_text, to_viewport,
};
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{
    AnyElement, Bounds, Context, Corners, ElementInputHandler, Entity, EntityInputHandler,
    FocusHandle, KeyDownEvent, KeyUpEvent, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, RenderImage, ScrollWheelEvent, SharedString, Subscription,
    UTF16Selection, Window, canvas, div, prelude::*, px, rgb,
};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// UI pixels per wheel line when the platform reports lines instead of pixels.
const WHEEL_LINE: f32 = 40.0;

pub(crate) struct LiveView {
    workspace: Workspace,
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
}

struct PromptField {
    token: u64,
    input: Entity<UrlInput>,
    /// Enter in the field accepts the prompt with its text.
    _submit: Subscription,
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
        browser: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let url = cx.new(|cx| UrlInput::new(workspace.url.clone(), cx));
        let url_events = cx.subscribe_in(&url, window, |view, _, event, window, cx| match event {
            UrlEvent::Submit(text) => view.navigate(text, window, cx),
        });
        let focus = cx.focus_handle();
        focus.focus(window);
        let focus_out = cx.on_focus_out(&focus, window, |view, _, window, _| {
            view.release_keys();
            view.invalidate_ime();
            window.invalidate_character_coordinates();
        });
        let window_activation = cx.observe_window_activation(window, |view, window, _| {
            if !window.is_window_active() {
                view.release_keys();
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
            workspace,
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
        view.start(window, cx);
        view
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
            self.sync_prompts(cx);
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
        let device = &mut self.devices[index];
        if device.decoding == Some(generation) {
            device.decoding = None;
        }
        if !self.lifecycle.accepts(generation) {
            // A frame of a stopped runtime is released unseen and leaves the
            // newer runtime's decode alone.
            if let Ok(image) = image {
                let _ = window.drop_image(image);
            }
            return;
        }
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
    /// proposal, and drops the field once its dialog is gone.
    fn sync_prompts(&mut self, cx: &mut Context<Self>) {
        for index in 0..self.devices.len() {
            let prompt = self.status.devices.get(index).and_then(|status| {
                status
                    .dialog
                    .as_ref()
                    .filter(|dialog| dialog.kind == DialogKind::Prompt)
            });
            let current = self.devices[index].prompt.as_ref().map(|field| field.token);
            match prompt {
                Some(dialog) if current != Some(dialog.token) => {
                    let token = dialog.token;
                    let default_text = dialog.default_text.clone();
                    let input = cx.new(|cx| UrlInput::new(default_text, cx));
                    let submit = cx.subscribe(&input, move |view, _, event: &UrlEvent, cx| {
                        let UrlEvent::Submit(text) = event;
                        view.answer_dialog(index, token, true, Some(text.clone()), cx);
                    });
                    self.devices[index].prompt = Some(PromptField {
                        token,
                        input,
                        _submit: submit,
                    });
                }
                Some(_) => {}
                None => self.devices[index].prompt = None,
            }
        }
    }

    /// The user's answer to the dialog `token` of device `index`. An accepted
    /// prompt takes `text`, or the field's text when `text` is `None`.
    fn answer_dialog(
        &mut self,
        index: usize,
        token: u64,
        accept: bool,
        text: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let text = match text {
            Some(text) => Some(text),
            None if accept => self
                .devices
                .get(index)
                .and_then(|device| device.prompt.as_ref())
                .filter(|field| field.token == token)
                .map(|field| field.input.read(cx).text().to_owned()),
            None => None,
        };
        self.notice = None;
        self.send(Command::AnswerDialog {
            device: index,
            token,
            accept,
            text,
        });
        cx.notify();
    }

    /// The device's open dialog. Only these buttons, or Enter in the prompt's
    /// field, answer it (ADR 0014).
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
            div()
                .id((id, index))
                .cursor_pointer()
                .rounded_md()
                .px_3()
                .py_1()
                .text_xs()
                .bg(rgb(if primary { ACCENT } else { RAISED }))
                .text_color(rgb(if primary { BG } else { TEXT }))
                .child(label)
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.answer_dialog(index, token, accept, None, cx)
                }))
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
            .mb_3()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(WARN))
            .bg(rgb(SURFACE))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(WARN))
                    .child(title),
            )
            .child(
                div()
                    .text_sm()
                    .max_h(px(120.))
                    .overflow_hidden()
                    .child(message),
            )
            .when_some(field, |this, field| this.child(field))
            .child(div().flex().gap_2().justify_end().children(buttons))
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
        let visible = self.devices[index].hidden;
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

    fn release_buttons(&mut self, index: usize) -> bool {
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
        self.notice = None;
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
        }
        cx.notify();
    }

    /// Returns true if the window may close now. Otherwise stops the runtime, or
    /// lets a running restart finish stopping the previous one, and removes the
    /// window once the browser and profile are gone: GPUI ends the process as
    /// soon as the last window closes.
    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.invalidate_ime();
        let request = self.lifecycle.begin_close(self.session.is_some());
        if request == CloseRequest::Now {
            return true;
        }
        self.notice = Some("Closing after the live browser stops…".into());
        cx.notify();
        if request == CloseRequest::Stop
            && let Some(session) = self.session.take()
        {
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
        if self.devices[index].hidden {
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
        if self.devices[index].hidden {
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
        if self.devices[index].hidden {
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
            self.pressed.press(identity, &key);
        }
    }

    /// A key-down in the device canvas.
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
        if self.ime.has_mark()
            && !keystroke.modifiers.control
            && !keystroke.modifiers.alt
            && !keystroke.modifiers.platform
        {
            self.pressed.forget(&identity);
            self.ignored_ime_keys.insert(identity);
            // GPUI/XKB can report the composed character as a key-down after
            // invoking preedit. It belongs to the composition, not raw CDP keys.
            if let Some(text) = keystroke.key_char.as_deref() {
                self.commit_ime(text, window);
            }
            return;
        }
        self.ignored_ime_keys.remove(&identity);
        let repeat = is_held || self.pressed.repeating(&identity);
        if is_paste_key(&key) {
            if !repeat {
                self.paste(cx);
            }
            return;
        }
        let Some(device) = key_down_target(
            self.selected,
            |index| self.devices.get(index).is_some_and(|device| !device.hidden),
            &identity,
            repeat,
            &self.held_keys,
        ) else {
            return;
        };
        if self.send(Command::Key {
            device,
            key: key.clone(),
        }) {
            self.held_keys.insert(identity, (device, key));
        }
    }

    /// A key-up anywhere in the window: it is observed on the window root,
    /// even if focus moved to the URL bar after the press.
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
                    let _ = window.paint_image(area, Corners::default(), image, 0, false);
                }
            },
        )
        .size_full();
        let state: SharedString = match (&status.error, status.loading) {
            (Some(error), _) => error.clone().into(),
            (None, true) => "Loading…".into(),
            (None, false) if status.url.is_empty() => "Starting…".into(),
            (None, false) => status.url.clone().into(),
        };
        div()
            .flex_none()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(rgb(if Some(index) == self.selected {
                ACCENT
            } else {
                BORDER
            }))
            .bg(rgb(RAISED))
            .p_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .mb_3()
                    .w(px(width.max(180.)))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(device.name.clone()),
                    )
                    .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                        "{} × {} · {}× · {}",
                        device.width, device.height, device.device_scale_factor, device.session
                    )))
                    .child(
                        div()
                            .text_xs()
                            .text_ellipsis()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_color(rgb(if status.error.is_some() { WARN } else { MUTED }))
                            .child(state),
                    ),
            )
            .when_some(status.dialog.clone(), |this, dialog| {
                this.child(self.dialog_panel(index, &dialog, width.max(180.), cx))
            })
            .child(
                div()
                    .id(("frame", index))
                    .w(px(width))
                    .h(px(height))
                    .bg(rgb(BG))
                    .overflow_hidden()
                    .rounded_sm()
                    .cursor_default()
                    .child(surface)
                    .on_any_mouse_down(cx.listener(
                        move |view, event: &MouseDownEvent, window, cx| {
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
                        },
                    ))
                    .capture_any_mouse_up(cx.listener(
                        move |view, event: &MouseUpEvent, window, cx| {
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
                        },
                    ))
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
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .mt_3()
                    .w(px(width.max(180.)))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(format!(
                        "{} frames · {} replaced",
                        status.frames, status.dropped_frames
                    ))
                    .child(if status.popups > 0 {
                        format!("{} popup(s) not shown", status.popups)
                    } else if status.streaming {
                        "Streaming".into()
                    } else {
                        "Paused".into()
                    }),
            )
            .into_any_element()
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sync = self.sync;
        let toggle = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .cursor_pointer()
                .rounded_md()
                .border_1()
                .border_color(rgb(if on { ACCENT } else { BORDER }))
                .bg(rgb(if on { SURFACE } else { RAISED }))
                .text_color(rgb(if on { ACCENT } else { TEXT }))
                .px_3()
                .py_1()
                .text_xs()
                .child(label)
        };
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .cursor_pointer()
                .rounded_md()
                .bg(rgb(RAISED))
                .px_3()
                .py_1()
                .text_sm()
                .child(label)
        };
        div()
            .h(px(58.))
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .px_5()
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(div().size(px(11.)).rounded_full().bg(rgb(ACCENT)))
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Broxser"),
            )
            .child(div().text_xs().text_color(rgb(MUTED)).child("LIVE FRAMES"))
            .child(self.url.clone())
            .child(
                div()
                    .id("go")
                    .cursor_pointer()
                    .rounded_md()
                    .bg(rgb(ACCENT))
                    .text_color(rgb(BG))
                    .px_3()
                    .py_1()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Go")
                    .on_click(cx.listener(|view, _, window, cx| {
                        let text = view.url.read(cx).text().to_owned();
                        view.navigate(&text, window, cx);
                    })),
            )
            .child(
                button("reload", "Reload")
                    .on_click(cx.listener(|view, _, _, cx| view.reload_selected(cx))),
            )
            .child(
                toggle("sync-navigation", "Sync links", sync.navigation).on_click(cx.listener(
                    move |view, _, _, cx| {
                        view.set_sync(
                            SyncSettings {
                                navigation: !sync.navigation,
                                ..sync
                            },
                            cx,
                        )
                    },
                )),
            )
            .child(
                toggle("sync-scroll", "Sync scroll", sync.scroll).on_click(cx.listener(
                    move |view, _, _, cx| {
                        view.set_sync(
                            SyncSettings {
                                scroll: !sync.scroll,
                                ..sync
                            },
                            cx,
                        )
                    },
                )),
            )
            .child(
                button("zoom-out", "−")
                    .on_click(cx.listener(|view, _, window, cx| view.zoom_by(-0.125, window, cx))),
            )
            .child(
                div()
                    .w(px(44.))
                    .text_center()
                    .text_sm()
                    .child(format!("{}%", (self.scale * 100.).round() as u32)),
            )
            .child(
                button("zoom-in", "+")
                    .on_click(cx.listener(|view, _, window, cx| view.zoom_by(0.125, window, cx))),
            )
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sections: Vec<AnyElement> = self
            .workspace
            .sessions
            .iter()
            .map(|session| {
                let rows: Vec<AnyElement> = self
                    .workspace
                    .devices
                    .iter()
                    .enumerate()
                    .filter(|(_, device)| device.session == session.id)
                    .map(|(index, device)| {
                        let hidden = self.devices[index].hidden;
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .pl_3()
                            .py_1()
                            .border_l_2()
                            .border_color(rgb(if Some(index) == self.selected {
                                ACCENT
                            } else {
                                BORDER
                            }))
                            .text_sm()
                            .child(
                                div()
                                    .id(("select", index))
                                    .cursor_pointer()
                                    .text_color(rgb(if hidden { MUTED } else { TEXT }))
                                    .child(device.name.clone())
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.select(index, window, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .id(("visibility", index))
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child(if hidden { "Show" } else { "Hide" })
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.toggle_hidden(index, window, cx)
                                    })),
                            )
                            .into_any_element()
                    })
                    .collect();
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .mb_6()
                    .child(div().text_sm().child(session.name.clone()))
                    .children(rows)
                    .into_any_element()
            })
            .collect();
        div()
            .w(px(230.))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(SURFACE))
            .border_r_1()
            .border_color(rgb(BORDER))
            .p_5()
            .child(div().mb_5().text_xs().text_color(rgb(ACCENT)).child("WORKSPACE"))
            .child(div().mb_8().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child(self.workspace.name.clone()))
            .child(div().mb_4().text_xs().text_color(rgb(MUTED)).child("SESSIONS · EPHEMERAL"))
            .children(sections)
            .child(
                div()
                    .mt_auto()
                    .pt_4()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("Frames stream from a headless browser. Sync stays inside one session; typing, forms and clicks are never broadcast."),
            )
    }

    fn status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let runtime = match &self.status.runtime {
            _ if self.lifecycle.is_restarting() => {
                "Restarting… stopping the previous browser".to_owned()
            }
            RuntimeState::Starting => "Starting browser…".to_owned(),
            RuntimeState::Running { product, protocol } => {
                format!("Live · {product} · CDP {protocol}")
            }
            RuntimeState::Stopped { error: Some(error) } => format!("Stopped: {error}"),
            RuntimeState::Stopped { error: None } => "Stopped".to_owned(),
        };
        let runtime = if self.selected.is_none() {
            format!("{runtime} · All devices hidden")
        } else {
            runtime
        };
        let stopped =
            matches!(self.status.runtime, RuntimeState::Stopped { .. }) || self.session.is_none();
        div()
            .min_h(px(44.))
            .flex_none()
            .flex()
            .items_center()
            .gap_4()
            .px_6()
            .border_t_1()
            .border_color(rgb(BORDER))
            .text_xs()
            .text_color(rgb(MUTED))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(runtime),
            )
            .children(self.status.protocol_error.clone().map(|error| {
                div()
                    .text_color(rgb(WARN))
                    .child(format!("Input error: {error}"))
            }))
            .children(
                self.notice
                    .clone()
                    .map(|notice| div().text_color(rgb(WARN)).child(notice)),
            )
            // Offered only while no restart or close runs (ADR 0009).
            .when(
                stopped && self.lifecycle.is_live() && self.browser.is_some(),
                |bar| {
                    bar.child(
                        div()
                            .id("restart")
                            .cursor_pointer()
                            .rounded_md()
                            .bg(rgb(ACCENT))
                            .text_color(rgb(BG))
                            .px_3()
                            .py_1()
                            .child("Restart runtime")
                            .on_click(cx.listener(|view, _, window, cx| view.restart(window, cx))),
                    )
                },
            )
    }
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
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .font_family("sans-serif")
            .on_action(cx.listener(|view, _: &Quit, window, cx| {
                if view.request_close(window, cx) {
                    window.remove_window();
                }
            }))
            .on_action(cx.listener(|view, _: &Refresh, _, cx| view.reload_selected(cx)))
            .on_action(cx.listener(|view, _: &FocusUrl, window, cx| {
                view.url.update(cx, |input, cx| input.focus_all(window, cx));
            }))
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
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .id("canvas")
                                    .track_focus(&self.focus)
                                    .on_key_down(cx.listener(
                                        |view, event: &KeyDownEvent, window, cx| {
                                            view.key_down(
                                                &event.keystroke,
                                                event.is_held,
                                                window,
                                                cx,
                                            );
                                            cx.stop_propagation();
                                        },
                                    ))
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .p_6()
                                    .child(
                                        div()
                                            .flex()
                                            .flex_wrap()
                                            .items_start()
                                            .gap_5()
                                            .children(cards),
                                    ),
                            )
                            .child(self.status_bar(cx)),
                    ),
            )
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

#[cfg(test)]
mod tests {
    use super::{
        DeviceView, PressedKeys, key_down_target, key_input, map_caret,
        selected_after_visibility_change,
    };
    use crate::ime::{ImeBuffer, Origin};
    use broxser_engine::{CaretRect, DeviceStatus, ImeAction, TextInputState};
    use gpui::{Bounds, Keystroke, point, px, size};
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
}
