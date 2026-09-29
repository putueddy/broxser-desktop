//! Static capture mode: fresh browser sessions per capture and PNG previews.
//! Kept as the fallback while live frames are qualified on real desktops.

use crate::theme::{
    self, ACCENT, BORDER, BORDER_STRONG, CANVAS, CARD, CHROME, DIVIDER, INFO, INK, Icon, MUTED,
    TEXT, TEXT_2, Tone,
};
use crate::{Quit, Refresh};
use broxser_core::{Device, Workspace};
use broxser_engine::{BrowserOptions, Cancellation, Cancelled, CaptureReport, capture_workspace};
use gpui::{
    Context, Entity, FocusHandle, RetainAllImageCache, Window, div, img, prelude::*, px, rgb,
};
use std::path::PathBuf;
use tempfile::TempDir;

pub(crate) struct StaticView {
    workspace: Workspace,
    browser: Option<PathBuf>,
    report: Option<CaptureReport>,
    image_cache: Entity<RetainAllImageCache>,
    // Keep two generations of PNG paths alive while GPUI loads the current images.
    captures: Vec<TempDir>,
    in_flight: bool,
    /// Cancels the in-flight capture when the window closes.
    cancel: Option<Cancellation>,
    /// GPUI on Linux stops its event loop when the last window is removed, which
    /// would end the process before the engine stops its browser. A close request
    /// during a capture cancels it and removes the window after cleanup instead.
    closing: bool,
    status: String,
    zoom: f32,
    focus: FocusHandle,
}

impl StaticView {
    pub(crate) fn new(
        workspace: Workspace,
        browser: Option<PathBuf>,
        status: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        let view = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            view.update(cx, |view: &mut StaticView, cx| view.request_close(cx))
                .unwrap_or(true)
        });
        Self {
            workspace,
            browser,
            report: None,
            image_cache: RetainAllImageCache::new(cx),
            captures: Vec::new(),
            in_flight: false,
            cancel: None,
            closing: false,
            status,
            zoom: 1.0,
            focus,
        }
    }

    pub(crate) fn capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.in_flight {
            return;
        }
        let Some(executable) = self.browser.clone() else {
            self.status = "Helium not found. Set BROXSER_HELIUM_BIN or pass --browser.".into();
            cx.notify();
            return;
        };
        self.report = None;
        self.image_cache
            .update(cx, |cache, cx| cache.clear(window, cx));
        self.image_cache = RetainAllImageCache::new(cx);
        self.in_flight = true;
        let cancel = Cancellation::new();
        self.cancel = Some(cancel.clone());
        self.status = "Capturing devices in fresh browser sessions…".into();
        cx.notify();
        let workspace = self.workspace.clone();
        let job = cx.background_executor().spawn(async move {
            let directory = tempfile::Builder::new()
                .prefix("broxser-preview-")
                .tempdir()?;
            let options = BrowserOptions {
                executable,
                headless: true,
                profile_root: None,
                cancel,
            };
            let report = capture_workspace(&workspace, &options, directory.path())?;
            Ok::<_, anyhow::Error>((directory, report))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            let _ = this.update_in(cx, |view, window, cx| {
                view.in_flight = false;
                view.cancel = None;
                if view.closing {
                    // The engine has stopped the browser; any preview is dropped here.
                    window.remove_window();
                    return;
                }
                match result {
                    Ok((directory, report)) => {
                        view.status = format!(
                            "{} static previews · {} · CDP {}",
                            report.frames.len(),
                            report.browser_product,
                            report.protocol_version
                        );
                        view.captures.push(directory);
                        if view.captures.len() > 2 {
                            view.captures.remove(0);
                        }
                        view.report = Some(report);
                    }
                    Err(error) if error.is::<Cancelled>() => {
                        view.status = "Capture cancelled.".into()
                    }
                    Err(error) => view.status = format!("Capture failed: {error:#}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Returns true if the window can close now. Otherwise cancels the capture and
    /// removes the window once the engine has cleaned up.
    fn request_close(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(cancel) = &self.cancel else {
            return true;
        };
        cancel.cancel();
        self.closing = true;
        self.status = "Closing after the capture browser stops…".into();
        cx.notify();
        false
    }

    fn zoom_by(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.zoom = (self.zoom + delta).clamp(0.6, 1.6);
        cx.notify();
    }

    fn sidebar(&self) -> impl IntoElement {
        let sections = self
            .workspace
            .sessions
            .iter()
            .enumerate()
            .map(|(session_index, session)| {
                let hue = theme::session_hue(session_index);
                let devices: Vec<_> = self
                    .workspace
                    .devices
                    .iter()
                    .filter(|device| device.session == session.id)
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
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(session.name.clone()),
                            )
                            .child(
                                theme::mono(devices.len().to_string(), 11., MUTED)
                                    .flex_1()
                                    .flex()
                                    .justify_end(),
                            ),
                    )
                    .children(devices.into_iter().map(|device| {
                        div()
                            .h(px(34.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .gap(px(9.))
                            .text_color(rgb(0xd7dbe0))
                            .child(theme::icon(
                                theme::device_icon(device.width, device.mobile),
                                15.,
                                0xa4abb5,
                            ))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(device.name.clone()),
                            )
                            .child(
                                theme::mono(
                                    format!("{}×{}", device.width, device.height),
                                    11.,
                                    MUTED,
                                )
                                .flex_1()
                                .flex()
                                .justify_end(),
                            )
                    }))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        div()
            .w(px(256.))
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
                    .flex()
                    .flex_col()
                    .gap(px(18.))
                    .px(px(10.))
                    .py(px(16.))
                    .child(
                        div()
                            .h(px(20.))
                            .flex()
                            .items_center()
                            .justify_between()
                            .px(px(6.))
                            .child(theme::caps("SESSIONS", MUTED))
                            .child(
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
                                    .child("Fresh per capture"),
                            ),
                    )
                    .children(sections),
            )
    }

    fn device_card(&self, device: &Device) -> impl IntoElement {
        let width = (device.width as f32 * 0.5 * self.zoom).clamp(200.0, 760.0);
        let height = width * device.height as f32 / device.width as f32;
        let frame = self.report.as_ref().and_then(|report| {
            report
                .frames
                .iter()
                .find(|frame| frame.device_id == device.id && frame.session_id == device.session)
        });
        let image = if let Some(frame) = frame {
            img(frame.path.clone())
                .image_cache(&self.image_cache)
                .w(px(width))
                .h(px(height))
                .into_any_element()
        } else {
            div()
                .w(px(width))
                .h(px(height))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .bg(rgb(CANVAS))
                .text_color(rgb(MUTED))
                .child(theme::icon(
                    if self.in_flight {
                        Icon::Loading
                    } else {
                        Icon::Static
                    },
                    18.,
                    MUTED,
                ))
                .child(if self.in_flight {
                    "Capturing…"
                } else {
                    "No preview yet"
                })
                .into_any_element()
        };
        let session = self
            .workspace
            .sessions
            .iter()
            .position(|session| session.id == device.session);
        div()
            .w(px(width + 26.))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(12.))
            .rounded(px(14.))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(CARD))
            .shadow(theme::card_shadow(false))
            .child(
                div()
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
                    .child(theme::mono(
                        format!(
                            "{} × {} · {}× · {}",
                            device.width,
                            device.height,
                            device.device_scale_factor,
                            if device.mobile { "mobile" } else { "desktop" }
                        ),
                        11.,
                        MUTED,
                    )),
            )
            .child(
                div()
                    .overflow_hidden()
                    .rounded(px(8.))
                    .shadow(theme::frame_shadow())
                    .child(image),
            )
    }
}

impl Drop for StaticView {
    fn drop(&mut self) {
        // Closing the window stops an in-flight capture; the engine then kills
        // its browser and removes the profile before the job reports back.
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
    }
}

impl Render for StaticView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cards = self
            .workspace
            .devices
            .iter()
            .map(|device| self.device_card(device).into_any_element())
            .collect::<Vec<_>>();
        let chip = div()
            .h(px(22.))
            .px(px(9.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .rounded_full()
            .border_1()
            .border_color(rgb(0x2f343b))
            .bg(rgb(0x1f2328))
            .text_size(px(11.5))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(rgb(0xc9ced5))
            .child(theme::icon(Icon::Static, 12., 0xc9ced5))
            .child("Static previews");
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(CANVAS))
            .text_color(rgb(TEXT))
            .font_family(theme::SANS)
            .text_size(px(13.))
            .track_focus(&self.focus)
            .on_action(cx.listener(|view, _: &Quit, window, cx| {
                if view.request_close(cx) {
                    window.remove_window();
                }
            }))
            .on_action(cx.listener(|view, _: &Refresh, window, cx| view.capture(window, cx)))
            .child(
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
                    .child(theme::brand(chip))
                    .child(theme::divider())
                    // The target comes from the workspace; this mode has no URL bar.
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(px(34.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .pl(px(11.))
                            .pr(px(12.))
                            .rounded(px(9.))
                            .border_1()
                            .border_dashed()
                            .border_color(rgb(0x2f343b))
                            .bg(rgb(theme::WELL))
                            .child(theme::icon(Icon::Globe, 14., MUTED))
                            .child(
                                theme::mono(self.workspace.url.clone(), 12.5, TEXT)
                                    .min_w_0()
                                    .truncate(),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .justify_end()
                                    .whitespace_nowrap()
                                    .text_size(px(11.5))
                                    .text_color(rgb(MUTED))
                                    .child("Target from the workspace"),
                            ),
                    )
                    .child(theme::stepper(
                        format!("{}%", (self.zoom * 100.).round() as u32),
                        div()
                            .id("zoom-out")
                            .on_click(cx.listener(|view, _, _, cx| view.zoom_by(-0.1, cx))),
                        div()
                            .id("zoom-in")
                            .on_click(cx.listener(|view, _, _, cx| view.zoom_by(0.1, cx))),
                    ))
                    .child(theme::divider())
                    .child(
                        theme::button(
                            "capture",
                            if self.in_flight {
                                Tone::Disabled
                            } else {
                                Tone::Primary
                            },
                            34.,
                        )
                        .rounded(px(9.))
                        .text_size(px(13.))
                        .child(theme::icon(
                            Icon::Report,
                            15.,
                            if self.in_flight { MUTED } else { INK },
                        ))
                        .child(if self.in_flight {
                            "Capturing…"
                        } else {
                            "Capture previews"
                        })
                        .on_click(cx.listener(|view, _, window, cx| view.capture(window, cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.sidebar())
                    .child(
                        div()
                            .id("canvas")
                            .flex_1()
                            .min_w_0()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap(px(20.))
                            .pt(px(22.))
                            .px(px(24.))
                            .pb(px(24.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(3.))
                                    .child(
                                        div()
                                            .text_size(px(16.))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child("Device canvas"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.5))
                                            .text_color(rgb(TEXT_2))
                                            .child("Screenshots are static; every capture starts fresh browser sessions."),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_start()
                                    .gap(px(20.))
                                    .children(cards),
                            ),
                    ),
            )
            .child(
                theme::status_bar()
                    .child(theme::state(
                        if self.in_flight { INFO } else { MUTED },
                        self.status.clone(),
                    ))
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .gap(px(16.))
                            .pr(px(11.))
                            .child(theme::shortcut(&["Ctrl", "R"], "Capture previews"))
                            .child(theme::shortcut(&["Ctrl", "Q"], "Quit")),
                    ),
            )
    }
}
