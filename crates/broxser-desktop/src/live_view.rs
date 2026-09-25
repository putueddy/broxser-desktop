//! Live device canvas. Frames are CDP screencast JPEGs from a headless browser,
//! decoded off the UI thread into GPUI images: frame streaming, not an embedded
//! browser surface. Pointer and wheel input go to the device under the pointer
//! and keys to the selected device, in CSS pixels of that device's viewport.

use crate::url_input::{UrlEvent, UrlInput};
use crate::{ACCENT, BG, BORDER, FocusUrl, MUTED, Quit, RAISED, Refresh, SURFACE, TEXT, WARN};
use anyhow::{Context as _, Result};
use broxser_core::{Workspace, validate_url};
use broxser_engine::{
    BrowserOptions, Cancellation, Command, Frame, KeyInput, LiveSession, Modifiers, PointerButton,
    PointerEvent, PointerKind, RuntimeState, Status, SyncSettings, to_viewport,
};
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{
    AnyElement, Bounds, Context, Corners, Entity, FocusHandle, KeyDownEvent, KeyUpEvent, Keystroke,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, RenderImage,
    ScrollWheelEvent, SharedString, Subscription, Window, canvas, div, prelude::*, px, rgb,
};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
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
    /// Keys sent to pages, retained so a release never lands on another device.
    held_keys: HashMap<String, (usize, KeyInput)>,
    /// Released keys still physically down. X11 reports repeats with is_held=false.
    suppressed_keys: HashSet<String>,
    url: Entity<UrlInput>,
    /// Display pixels per CSS pixel.
    scale: f32,
    sync: SyncSettings,
    focus: FocusHandle,
    closing: bool,
    notice: Option<String>,
    _url_events: Subscription,
    _focus_out: Subscription,
}

#[derive(Default)]
struct DeviceView {
    image: Option<Arc<RenderImage>>,
    decoding: bool,
    /// Frame bounds from the last paint, used to map pointer positions.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    hidden: bool,
    /// Held buttons as a CDP bitmask, and the last mapped pointer position.
    buttons: u8,
    last_point: Option<(f64, f64)>,
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
        let focus_out = cx.on_focus_out(&focus, window, |view, _, _, _| {
            view.release_keys();
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
            held_keys: HashMap::new(),
            suppressed_keys: HashSet::new(),
            url,
            scale: 0.5,
            sync: SyncSettings::default(),
            focus,
            closing: false,
            notice: None,
            _url_events: url_events,
            _focus_out: focus_out,
        };
        view.start(window, cx);
        view
    }

    /// Starts a browser for the workspace. Opening loads its URL once per device.
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        cx.spawn_in(window, async move |this, cx| {
            while wakeups.next().await.is_some() {
                queued.store(false, Ordering::Release);
                if this
                    .update_in(cx, |view, window, cx| view.pull(window, cx))
                    .is_err()
                {
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
        let status = session.status();
        let frames: Vec<(usize, Frame)> = (0..self.devices.len())
            .filter(|&index| !self.devices[index].decoding && !self.devices[index].hidden)
            .filter_map(|index| Some((index, session.take_frame(index)?)))
            .collect();
        if status != self.status {
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
            cx.notify();
        }
        for (index, frame) in frames {
            self.devices[index].decoding = true;
            let decoded = cx.background_executor().spawn(async move { decode(frame) });
            cx.spawn_in(window, async move |this, cx| {
                let image = decoded.await;
                this.update_in(cx, |view, window, cx| {
                    view.show_frame(index, image, window, cx)
                })
                .ok();
            })
            .detach();
        }
    }

    fn show_frame(
        &mut self,
        index: usize,
        image: Result<Arc<RenderImage>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let device = &mut self.devices[index];
        device.decoding = false;
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

    fn navigate(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let url = if text.contains("://") {
            text.to_owned()
        } else {
            format!("http://{text}")
        };
        match validate_url(&url) {
            Ok(()) => {
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
                self.suppressed_keys.insert(name);
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
            self.send(Command::Reload { device });
            cx.notify();
        }
    }

    fn zoom_by(&mut self, delta: f32, window: &mut Window, cx: &mut Context<Self>) {
        self.scale = (self.scale + delta).clamp(0.25, 1.0);
        self.send_frame_limits(window);
        cx.notify();
    }

    /// Asks for frames no larger than they are displayed, in physical pixels.
    fn send_frame_limits(&mut self, window: &Window) {
        let factor = self.scale * window.scale_factor();
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

    fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.suppressed_keys.extend(self.held_keys.keys().cloned());
        self.held_keys.clear();
        for device in &mut self.devices {
            device.buttons = 0;
            device.last_point = None;
            device.bounds.set(None);
        }
        let text = self.url.read(cx).text().to_owned();
        if validate_url(&text).is_ok() {
            self.workspace.url = text;
        }
        for device in &mut self.devices {
            if let Some(image) = device.image.take() {
                let _ = window.drop_image(image);
            }
            device.decoding = false;
        }
        match self.session.take() {
            Some(previous) => {
                // Stop the previous browser off the UI thread, then start fresh.
                let stopped = cx
                    .background_executor()
                    .spawn(async move { drop(previous) });
                cx.spawn_in(window, async move |this, cx| {
                    stopped.await;
                    this.update_in(cx, |view, window, cx| {
                        view.start(window, cx);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            None => self.start(window, cx),
        }
        cx.notify();
    }

    /// Returns true if the window may close now. Otherwise stops the runtime and
    /// removes the window after its browser and profile are gone: GPUI ends the
    /// process as soon as the last window closes.
    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.closing {
            return false;
        }
        let Some(session) = self.session.take() else {
            return true;
        };
        self.closing = true;
        self.notice = Some("Closing after the live browser stops…".into());
        cx.notify();
        let stopped = cx.background_executor().spawn(async move { drop(session) });
        cx.spawn_in(window, async move |_, cx| {
            stopped.await;
            let _ = cx.update(|window, _| window.remove_window());
        })
        .detach();
        false
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

    fn key(&mut self, keystroke: &Keystroke, down: bool, is_held: bool) {
        let Some(key) = KeyInput::from_key(
            &keystroke.key,
            keystroke.key_char.as_deref(),
            modifiers_of(&keystroke.modifiers),
            down,
        ) else {
            return;
        };
        let identity = logical_key_identity(keystroke, &key);
        if !down {
            // Key-up is observed on the window root, even if focus moved to
            // the URL bar after the release was sent to the old page.
            if self.suppressed_keys.remove(&identity) {
                return;
            }
            if let Some((device, mut key)) = self.held_keys.get(&identity).cloned()
                && !self.devices[device].hidden
            {
                key.down = false;
                if self.send(Command::Key { device, key }) {
                    self.held_keys.remove(&identity);
                }
            }
            return;
        }
        let Some(device) = key_down_target(
            self.selected,
            |index| self.devices.get(index).is_some_and(|device| !device.hidden),
            &identity,
            is_held,
            &self.held_keys,
            &self.suppressed_keys,
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

    fn device_card(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let device = &self.workspace.devices[index];
        let view = &self.devices[index];
        let status = self.status.devices.get(index).cloned().unwrap_or_default();
        let width = device.width as f32 * self.scale;
        let height = device.height as f32 * self.scale;
        let bounds = Rc::clone(&view.bounds);
        let image = view.image.clone();
        let surface = canvas(
            |_, _, _| (),
            move |area, (), window, _| {
                bounds.set(Some(area));
                if let Some(image) = image {
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
            .when(stopped && !self.closing && self.browser.is_some(), |bar| {
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
            })
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
                view.key(&event.keystroke, false, false);
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
                                        |view, event: &KeyDownEvent, _, cx| {
                                            view.key(&event.keystroke, true, event.is_held);
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

fn key_down_target(
    selected: Option<usize>,
    visible: impl Fn(usize) -> bool,
    identity: &str,
    is_held: bool,
    held: &HashMap<String, (usize, KeyInput)>,
    suppressed: &HashSet<String>,
) -> Option<usize> {
    let device = selected.filter(|&index| visible(index))?;
    if suppressed.contains(identity) {
        return None;
    }
    match held.get(identity) {
        Some((owner, _)) if *owner != device => None,
        None if is_held => None,
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
    use super::{key_down_target, logical_key_identity, selected_after_visibility_change};
    use broxser_engine::{KeyInput, Modifiers};
    use gpui::Keystroke;
    use std::collections::{HashMap, HashSet};

    fn identity(name: &str) -> String {
        let stroke = Keystroke {
            key: name.into(),
            ..Keystroke::default()
        };
        let input = KeyInput::from_key(name, None, Modifiers::default(), true).unwrap();
        logical_key_identity(&stroke, &input)
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
        let name = identity("a");
        let input = KeyInput::from_key("a", None, Modifiers::default(), true).unwrap();
        let mut held = HashMap::from([(name.clone(), (0, input))]);
        let mut suppressed = HashSet::new();
        assert_eq!(
            key_down_target(Some(0), |_| true, &name, false, &held, &suppressed),
            Some(0)
        );

        // Hiding device 0 sends keyUp to it, then device 1 becomes selected.
        held.remove(&name);
        suppressed.insert(name.clone());
        assert_eq!(
            key_down_target(Some(1), |_| true, &name, false, &held, &suppressed),
            None
        );
        assert_eq!(
            key_down_target(Some(1), |_| true, &name, true, &held, &suppressed),
            None
        );
        // The physical keyUp clears suppression; a fresh press may route.
        suppressed.remove(&name);
        assert_eq!(
            key_down_target(Some(1), |_| true, &name, false, &held, &suppressed),
            Some(1)
        );
        assert_eq!(
            key_down_target(None, |_| true, &name, false, &held, &suppressed),
            None
        );
        assert_eq!(
            key_down_target(Some(0), |_| false, &name, false, &held, &suppressed),
            None
        );
    }

    #[test]
    fn common_shifted_punctuation_matches_the_same_release() {
        for (plain, shifted) in [("1", "!"), ("/", "?"), ("[", "{"), ("'", "\"")] {
            assert_eq!(identity(plain), identity(shifted));
        }
    }
}
