mod live_view;
mod static_view;
mod url_input;

use anyhow::{Context as _, Result};
use broxser_core::Workspace;
use broxser_engine::discover_browser;
use clap::Parser;
use gpui::{
    App, Application, AssetSource, Bounds, KeyBinding, SharedString, TitlebarOptions, WindowBounds,
    WindowOptions, actions, prelude::*, px, size,
};
use live_view::LiveView;
use static_view::StaticView;
use std::borrow::Cow;
use std::path::PathBuf;

const BG: u32 = 0x111514;
const SURFACE: u32 = 0x1b211f;
const RAISED: u32 = 0x252c29;
const BORDER: u32 = 0x35403a;
const TEXT: u32 = 0xe8eee9;
const MUTED: u32 = 0x9aa9a0;
const ACCENT: u32 = 0x7ce29b;
const WARN: u32 = 0xf2b872;

actions!(broxser, [Quit, Refresh, FocusUrl]);

#[derive(Parser)]
#[command(
    name = "broxser-desktop",
    about = "Broxser multi-device workspace: live frames or static previews"
)]
struct Args {
    /// Workspace JSON v1; defaults to the demo workspace.
    #[arg(long)]
    workspace: Option<PathBuf>,
    /// Replaces the workspace URL.
    #[arg(long)]
    url: Option<String>,
    /// Explicit browser executable. Other Chromium builds are diagnostics only.
    #[arg(long)]
    browser: Option<PathBuf>,
    /// Static PNG previews in fresh sessions per capture instead of live frames.
    #[arg(long = "static")]
    static_previews: bool,
    /// With --static: capture once the window opens.
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

fn main() -> Result<()> {
    // Browser guardians are this executable started again (ADR 0007).
    broxser_engine::run_guardian_if_requested();
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
    let (browser, status) = match args.browser.map(Ok).unwrap_or_else(discover_browser) {
        Ok(path) => (Some(path), "Ready to capture static previews.".to_owned()),
        Err(error) => (None, format!("Helium unavailable: {error}")),
    };
    let static_previews = args.static_previews;
    let capture_on_start = args.capture_on_start;
    Application::new()
        .with_assets(FileAssets)
        .run(move |cx: &mut App| {
            cx.bind_keys([
                KeyBinding::new("ctrl-q", Quit, None),
                KeyBinding::new("ctrl-r", Refresh, None),
                KeyBinding::new("ctrl-l", FocusUrl, None),
            ]);
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let bounds = Bounds::centered(None, size(px(1360.), px(860.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("broxser".into()),
                titlebar: Some(TitlebarOptions {
                    title: Some("Broxser".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            if static_previews {
                let window = cx
                    .open_window(options, |window, cx| {
                        window.set_window_title("Broxser");
                        cx.new(|cx| StaticView::new(workspace, browser, status, window, cx))
                    })
                    .expect("open Broxser window");
                if capture_on_start {
                    let _ = window.update(cx, |view, window, cx| view.capture(window, cx));
                }
            } else {
                cx.open_window(options, |window, cx| {
                    window.set_window_title("Broxser");
                    cx.new(|cx| LiveView::new(workspace, browser, window, cx))
                })
                .expect("open Broxser window");
            }
            cx.activate(true);
        });
    Ok(())
}
