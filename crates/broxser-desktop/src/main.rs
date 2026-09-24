use anyhow::{Context as _, Result};
use broxser_core::{Device, Workspace};
use broxser_engine::{
    BrowserOptions, Cancellation, Cancelled, CaptureReport, capture_workspace, discover_browser,
};
use clap::Parser;
use gpui::{
    App, Application, AssetSource, Bounds, Context, Entity, FocusHandle, KeyBinding,
    RetainAllImageCache, SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions,
    actions, div, img, prelude::*, px, rgb, size,
};
use std::borrow::Cow;
use std::path::PathBuf;
use tempfile::TempDir;

const BG: u32 = 0x111514;
const SURFACE: u32 = 0x1b211f;
const RAISED: u32 = 0x252c29;
const BORDER: u32 = 0x35403a;
const TEXT: u32 = 0xe8eee9;
const MUTED: u32 = 0x9aa9a0;
const ACCENT: u32 = 0x7ce29b;

actions!(broxser, [Quit, Refresh]);

#[derive(Parser)]
#[command(
    name = "broxser-desktop",
    about = "Broxser static multi-device previews"
)]
struct Args {
    #[arg(long)]
    workspace: Option<PathBuf>,
    #[arg(long)]
    url: Option<String>,
    #[arg(long)]
    browser: Option<PathBuf>,
    #[arg(long)]
    capture_on_start: bool,
}

struct FileAssets;
impl AssetSource for FileAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(Some(
            std::fs::read(path)
                .with_context(|| format!("read image {path}"))?
                .into(),
        ))
    }
    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(std::fs::read_dir(path)?
            .filter_map(|entry| Some(entry.ok()?.path().to_string_lossy().into_owned().into()))
            .collect())
    }
}

struct Desktop {
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

impl Desktop {
    fn capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            .map(|session| {
                let devices = self
                    .workspace
                    .devices
                    .iter()
                    .filter(|device| device.session == session.id);
                let count = devices.clone().count();
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .mb_6()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_sm()
                            .child(session.name.clone())
                            .child(count.to_string()),
                    )
                    .children(devices.map(|device| {
                        div()
                            .pl_3()
                            .py_1()
                            .border_l_2()
                            .border_color(rgb(ACCENT))
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .child(device.name.clone())
                    }))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
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
            .child(
                div()
                    .mb_5()
                    .text_xs()
                    .text_color(rgb(ACCENT))
                    .child("WORKSPACE"),
            )
            .child(
                div()
                    .mb_8()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(self.workspace.name.clone()),
            )
            .child(
                div()
                    .mb_4()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("SESSIONS · EPHEMERAL"),
            )
            .children(sections)
            .child(
                div()
                    .mt_auto()
                    .pt_4()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("Broxser · preview workspace"),
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
                .items_center()
                .justify_center()
                .bg(rgb(BG))
                .text_sm()
                .text_color(rgb(MUTED))
                .child(if self.in_flight {
                    "Capturing…"
                } else {
                    "No preview yet"
                })
                .into_any_element()
        };
        div()
            .w(px(width + 24.))
            .flex_none()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(RAISED))
            .p_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .mb_3()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(device.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(format!("{} × {} viewport", device.width, device.height)),
                    ),
            )
            .child(div().overflow_hidden().rounded_sm().child(image))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .mt_3()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(format!(
                        "{} · {}×",
                        device.session, device.device_scale_factor
                    ))
                    .child(if device.mobile { "Mobile" } else { "Desktop" }),
            )
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        // Closing the window stops an in-flight capture; the engine then kills
        // its browser and removes the profile before the job reports back.
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
    }
}

impl Render for Desktop {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cards = self
            .workspace
            .devices
            .iter()
            .map(|device| self.device_card(device).into_any_element())
            .collect::<Vec<_>>();
        div().size_full().flex().flex_col().bg(rgb(BG)).text_color(rgb(TEXT)).font_family("sans-serif")
            .track_focus(&self.focus)
            .on_action(cx.listener(|view, _: &Quit, window, cx| {
                if view.request_close(cx) {
                    window.remove_window();
                }
            }))
            .on_action(cx.listener(|view, _: &Refresh, window, cx| view.capture(window, cx)))
            .child(div().h(px(58.)).flex_none().flex().items_center().justify_between()
                .px_5().border_b_1().border_color(rgb(BORDER))
                .child(div().flex().items_center().gap_3()
                    .child(div().size(px(11.)).rounded_full().bg(rgb(ACCENT)))
                    .child(div().text_lg().font_weight(gpui::FontWeight::BOLD).child("Broxser"))
                    .child(div().text_xs().text_color(rgb(MUTED)).child("STATIC PREVIEWS")))
                .child(div().text_xs().text_color(rgb(MUTED)).child("Ctrl+R refresh · Ctrl+Q quit")))
            .child(div().flex().flex_1().min_h_0()
                .child(self.sidebar())
                .child(div().flex_1().min_w_0().flex().flex_col()
                    .child(div().flex().items_center().justify_between().gap_4().px_6().py_4()
                        .border_b_1().border_color(rgb(BORDER))
                        .child(div().flex_1().min_w_0().flex().flex_col().gap_1()
                            .child(div().text_xs().text_color(rgb(MUTED)).child("TARGET URL · FROM WORKSPACE"))
                            .child(div().text_sm().text_ellipsis().child(self.workspace.url.clone())))
                        .child(div().id("capture").cursor_pointer().rounded_md().bg(rgb(ACCENT))
                            .px_4().py_2().text_sm().font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(BG)).child(if self.in_flight { "Capturing…" } else { "Capture previews" })
                            .on_click(cx.listener(|view, _, window, cx| view.capture(window, cx)))))
                    .child(div().flex().items_center().justify_between().px_6().py_4()
                        .child(div().flex().flex_col().gap_1()
                            .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child("Device canvas"))
                            .child(div().text_xs().text_color(rgb(MUTED))
                                .child("Screenshots are static; every capture starts fresh browser sessions.")))
                        .child(div().flex().items_center().gap_2()
                            .child(div().id("zoom-out").cursor_pointer().rounded_md().bg(rgb(RAISED))
                                .px_3().py_1().child("−").on_click(cx.listener(|view, _, _, cx| view.zoom_by(-0.1, cx))))
                            .child(div().w(px(44.)).text_center().text_sm().child(format!("{}%", (self.zoom * 100.).round() as u32)))
                            .child(div().id("zoom-in").cursor_pointer().rounded_md().bg(rgb(RAISED))
                                .px_3().py_1().child("+").on_click(cx.listener(|view, _, _, cx| view.zoom_by(0.1, cx))))))
                    .child(div().id("canvas").flex_1().min_h_0().overflow_y_scroll().px_6().pb_6()
                        .child(div().flex().flex_wrap().items_start().gap_5().children(cards)))
                    .child(div().min_h(px(44.)).flex_none().flex().items_center().px_6()
                        .border_t_1().border_color(rgb(BORDER)).text_xs().text_color(rgb(MUTED))
                        .child(self.status.clone()))))
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut workspace = if let Some(path) = args.workspace {
        Workspace::load(&path).with_context(|| format!("load workspace {}", path.display()))?
    } else {
        Workspace::demo()
    };
    if let Some(url) = args.url {
        workspace.url = url;
    }
    workspace.validate()?;
    let (browser, status) = match args.browser {
        Some(path) => (Some(path), "Ready to capture static previews.".to_owned()),
        None => match discover_browser() {
            Ok(path) => (Some(path), "Ready to capture static previews.".to_owned()),
            Err(error) => (None, format!("Helium unavailable: {error}")),
        },
    };
    let start = args.capture_on_start;
    Application::new()
        .with_assets(FileAssets)
        .run(move |cx: &mut App| {
            cx.bind_keys([
                KeyBinding::new("ctrl-q", Quit, None),
                KeyBinding::new("ctrl-r", Refresh, None),
            ]);
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let bounds = Bounds::centered(None, size(px(1360.), px(860.)), cx);
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        app_id: Some("broxser".into()),
                        titlebar: Some(TitlebarOptions {
                            title: Some("Broxser".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    |window, cx| {
                        cx.new(|cx| {
                            let focus = cx.focus_handle();
                            focus.focus(window);
                            window.set_window_title("Broxser");
                            let view = cx.entity().downgrade();
                            window.on_window_should_close(cx, move |_, cx| {
                                view.update(cx, |view: &mut Desktop, cx| view.request_close(cx))
                                    .unwrap_or(true)
                            });
                            Desktop {
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
                        })
                    },
                )
                .expect("open Broxser window");
            if start {
                let _ = window.update(cx, |view, window, cx| view.capture(window, cx));
            }
            cx.activate(true);
        });
    Ok(())
}
