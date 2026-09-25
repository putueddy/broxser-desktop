use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use broxser_core::Workspace;
use broxser_engine::{BrowserOptions, Cancellation, capture_workspace, discover_browser};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "broxser",
    version,
    about = "Local responsive browser workspace"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write an editable v1 workspace. Refuses to overwrite an existing file.
    Init {
        #[arg(default_value = "workspace.json")]
        path: PathBuf,
    },
    /// Validate a workspace without starting a browser.
    Validate {
        #[arg(default_value = "examples/workspace.json")]
        workspace: PathBuf,
    },
    /// Discover Helium and print the configured executable, without launching it.
    Doctor,
    /// Capture all viewports in isolated ephemeral browser sessions.
    Capture {
        #[arg(long, default_value = "examples/workspace.json")]
        workspace: PathBuf,
        #[arg(long)]
        url: Option<String>,
        /// Explicit executable. Other Chromium browsers are diagnostics only.
        #[arg(long)]
        browser: Option<PathBuf>,
        #[arg(long, default_value = "artifacts/captures")]
        output: PathBuf,
        /// Show the browser windows while capturing.
        #[arg(long)]
        headed: bool,
    },
}

fn main() -> Result<()> {
    // Browser guardians are this executable started again (ADR 0007).
    broxser_engine::run_guardian_if_requested();
    match Cli::parse().command {
        Command::Init { path } => {
            let workspace = Workspace::demo();
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .with_context(|| {
                    format!(
                        "Cannot create {} (existing files are preserved)",
                        path.display()
                    )
                })?;
            serde_json::to_writer_pretty(file, &workspace)?;
            println!("Created {}", path.display());
        }
        Command::Validate { workspace } => {
            let workspace = Workspace::load(workspace)?;
            println!(
                "Valid v{} workspace: {} ({} devices, {} sessions)",
                workspace.schema_version,
                workspace.name,
                workspace.devices.len(),
                workspace.sessions.len()
            );
        }
        Command::Doctor => {
            let executable = discover_browser()?;
            println!("Browser executable: {}", executable.display());
            println!("Discovery only. Run capture to verify headless/CDP compatibility.");
            println!("Linux desktop requires a Vulkan-capable GPU and Wayland or X11.");
        }
        Command::Capture {
            workspace,
            url,
            browser,
            output,
            headed,
        } => {
            let mut workspace = Workspace::load(workspace)?;
            if let Some(url) = url {
                workspace.url = url;
            }
            workspace.validate()?;
            let executable = browser.map(Ok).unwrap_or_else(discover_browser)?;
            if output.is_file() {
                bail!("Capture output must be a directory");
            }
            let options = BrowserOptions {
                executable,
                headless: !headed,
                profile_root: None,
                cancel: Cancellation::new(),
            };
            // Every run has a distinct directory, so a failed refresh cannot
            // leave an old report looking like the result of a new capture.
            fs::create_dir_all(&output)?;
            let run_id = format!(
                "{}-{}",
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
                std::process::id()
            );
            let run_dir = output.join(run_id);
            fs::create_dir(&run_dir)?;
            let report = capture_workspace(&workspace, &options, &run_dir)?;
            let json = serde_json::to_string_pretty(&report)?;
            fs::write(run_dir.join("report.json"), &json)?;
            println!("{json}");
        }
    }
    Ok(())
}
