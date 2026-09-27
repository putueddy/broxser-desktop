mod ime;
mod lifecycle;
mod live_view;
mod static_view;
mod url_input;

use anyhow::{Context as _, Result};
use broxser_core::{AppState, Workspace};
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
/// Destructive actions, such as removing a device from the draft.
const DANGER: u32 = 0xe07a7a;

actions!(
    broxser,
    [Quit, Refresh, FocusUrl, TogglePanel, ToggleConsole]
);

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

/// `$XDG_STATE_HOME/broxser/state.json`, or `~/.local/state/broxser/state.json`;
/// `None` when neither variable names an absolute directory. `BROXSER_STATE_FILE`
/// overrides it, so a test or a smoke run keeps its state to itself.
fn state_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("BROXSER_STATE_FILE") {
        let path = PathBuf::from(path);
        return path.is_absolute().then_some(path);
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".local").join("state"))
        })?;
    Some(base.join("broxser").join("state.json"))
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
    // The state file remembers workspace files and the window size, nothing
    // of a run (ADR 0022). An unreadable one is reported and replaced later.
    let state_path = state_path();
    let mut state = match &state_path {
        Some(path) => AppState::load(path).unwrap_or_else(|error| {
            eprintln!("broxser: ignoring {}: {error}", path.display());
            AppState::default()
        }),
        None => AppState::default(),
    };
    let workspace_path = args.workspace.or_else(|| {
        state
            .latest_existing_workspace()
            .map(std::path::Path::to_path_buf)
    });
    let mut workspace = match &workspace_path {
        Some(path) => {
            Workspace::load(path).with_context(|| format!("load workspace {}", path.display()))?
        }
        None => Workspace::demo(),
    };
    if let (Some(path), Some(state_path)) = (&workspace_path, &state_path) {
        state.remember_workspace(path);
        if let Err(error) = state.save(state_path) {
            eprintln!("broxser: could not save {}: {error}", state_path.display());
        }
    }
    let window_size = state
        .window
        .map(|window| size(px(window.width as f32), px(window.height as f32)))
        .unwrap_or_else(|| size(px(1360.), px(860.)));
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
                // Helium would reload the page itself, untracked (ADR 0010).
                KeyBinding::new("f5", Refresh, None),
                KeyBinding::new("ctrl-l", FocusUrl, None),
                // Helium's own Ctrl+Shift+W closes a window; it never reaches
                // a page (ADR 0010), so the panel can take it.
                KeyBinding::new("ctrl-shift-w", TogglePanel, None),
                // Helium's own Ctrl+Shift+J opens DevTools; the engine never
                // forwards it (ADR 0010), so the console panel can take it.
                KeyBinding::new("ctrl-shift-j", ToggleConsole, None),
            ]);
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let bounds = Bounds::centered(None, window_size, cx);
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
                    cx.new(|cx| {
                        LiveView::new(workspace, workspace_path, state_path, browser, window, cx)
                    })
                })
                .expect("open Broxser window");
            }
            cx.activate(true);
        });
    Ok(())
}
