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
    selected: usize,
    url: Entity<UrlInput>,
    /// Display pixels per CSS pixel.
    scale: f32,
    sync: SyncSettings,
    focus: FocusHandle,
    closing: bool,
    notice: Option<String>,
    _url_events: Subscription,
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
            selected: 0,
            url,
            scale: 0.5,
            sync: SyncSettings::default(),
            focus,
            closing: false,
            notice: None,
            _url_events: url_events,
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
            if let Some(url) = status
                .devices
                .get(self.selected)
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

    fn send(&mut self, command: Command) {
        let accepted = self
            .session
            .as_ref()
            .is_some_and(|session| session.send(command));
        if !accepted {
            self.notice = Some("The live runtime is not accepting commands.".into());
        }
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
        self.selected = index;
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
        let device = &mut self.devices[index];
        device.hidden = !device.hidden;
        if device.hidden
            && let Some(image) = device.image.take()
        {
            let _ = window.drop_image(image);
        }
        let visible = !device.hidden;
        self.send(Command::SetVisible {
            device: index,
            visible,
        });
        cx.notify();
    }

    fn set_sync(&mut self, sync: SyncSettings, cx: &mut Context<Self>) {
        self.sync = sync;
        self.send(Command::SetSync(sync));
        cx.notify();
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
        let Some((x, y)) = self.map(index, position) else {
            return;
        };
        let bit = match button {
            Some(MouseButton::Left) => 1,
            Some(MouseButton::Right) => 2,
            Some(MouseButton::Middle) => 4,
            _ => 0,
        };
        let device = &mut self.devices[index];
        match kind {
            PointerKind::Down => device.buttons |= bit,
            PointerKind::Up => device.buttons &= !bit,
            PointerKind::Move => {}
        }
        device.last_point = Some((x, y));
        let buttons = device.buttons;
        let button = match (kind, button) {
            (PointerKind::Move, _) if buttons & 1 != 0 => PointerButton::Left,
            (PointerKind::Move, _) => PointerButton::None,
            (_, Some(MouseButton::Left)) => PointerButton::Left,
            (_, Some(MouseButton::Right)) => PointerButton::Right,
            (_, Some(MouseButton::Middle)) => PointerButton::Middle,
            _ => PointerButton::None,
        };
        if kind == PointerKind::Down {
            self.select(index, window, cx);
        }
        self.send(Command::Pointer {
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
        });
    }

    /// A button released outside the frame must not stay pressed in the page.
    fn release_outside(&mut self, index: usize, modifiers: &gpui::Modifiers) {
        let device = &mut self.devices[index];
        let Some((x, y)) = device.last_point.filter(|_| device.buttons & 1 != 0) else {
            return;
        };
        device.buttons &= !1;
        let buttons = device.buttons;
        self.send(Command::Pointer {
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
        });
    }

    fn wheel(&mut self, index: usize, event: &ScrollWheelEvent) {
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

    fn key(&mut self, keystroke: &Keystroke, down: bool) {
        if let Some(key) = KeyInput::from_key(
            &keystroke.key,
            keystroke.key_char.as_deref(),
            modifiers_of(&keystroke.modifiers),
            down,
        ) {
            let device = self.selected;
            self.send(Command::Key { device, key });
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
            .border_color(rgb(if index == self.selected {
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
                button("reload", "Reload").on_click(cx.listener(|view, _, _, cx| {
                    let device = view.selected;
                    view.send(Command::Reload { device });
                    cx.notify();
                })),
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
                            .border_color(rgb(if index == self.selected {
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
            .on_action(cx.listener(|view, _: &Refresh, _, cx| {
                let device = view.selected;
                view.send(Command::Reload { device });
                cx.notify();
            }))
            .on_action(cx.listener(|view, _: &FocusUrl, window, cx| {
                view.url.update(cx, |input, cx| input.focus_all(window, cx));
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
                                            view.key(&event.keystroke, true);
                                            cx.stop_propagation();
                                        },
                                    ))
                                    .on_key_up(cx.listener(|view, event: &KeyUpEvent, _, _| {
                                        view.key(&event.keystroke, false);
                                    }))
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
