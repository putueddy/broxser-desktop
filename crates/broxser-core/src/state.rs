//! Application state that outlives a run: which workspace files were opened
//! and how big the window was. It never holds a URL typed at runtime (a page's
//! address can carry a token), cookies, credentials or a browser profile.

use crate::{Error, Result, read_config, write_config};
use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

pub const STATE_SCHEMA_VERSION: u32 = 1;
/// How many workspace files the state remembers, most recent first.
pub const MAX_RECENT_WORKSPACES: usize = 10;
const MAX_PATH_BYTES: usize = 4096;
const WINDOW_SIZE: std::ops::RangeInclusive<u32> = 200..=16384;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppState {
    pub schema_version: u32,
    /// Workspace files, the most recently opened first.
    #[serde(default)]
    pub recent_workspaces: Vec<PathBuf>,
    /// The window's last size in logical pixels.
    #[serde(default)]
    pub window: Option<WindowSize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowSize {
    pub width: u32,
    pub height: u32,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            recent_workspaces: Vec::new(),
            window: None,
        }
    }
}

impl AppState {
    /// Loads the state file; a missing file is the default state, anything
    /// unreadable or invalid is an error for the caller to report.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = match read_config(path.as_ref(), "application state") {
            Ok(bytes) => bytes,
            Err(Error::Io(error)) if error.kind() == ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error),
        };
        let state: Self = serde_json::from_slice(&bytes)?;
        state.validate()?;
        Ok(state)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != STATE_SCHEMA_VERSION {
            return Err(Error::UnsupportedStateSchema(self.schema_version));
        }
        if self.recent_workspaces.len() > MAX_RECENT_WORKSPACES {
            return Err(Error::Invalid(format!(
                "application state lists more than {MAX_RECENT_WORKSPACES} recent workspaces"
            )));
        }
        for path in &self.recent_workspaces {
            let text = path.to_string_lossy();
            if !path.is_absolute()
                || path.as_os_str().len() > MAX_PATH_BYTES
                || text.chars().any(char::is_control)
            {
                return Err(Error::Invalid(
                    "recent workspace paths must be absolute, at most 4096 bytes and free of \
                     control characters"
                        .into(),
                ));
            }
        }
        if let Some(window) = self.window
            && !(WINDOW_SIZE.contains(&window.width) && WINDOW_SIZE.contains(&window.height))
        {
            return Err(Error::Invalid(
                "window size must be 200..=16384 logical pixels per axis".into(),
            ));
        }
        Ok(())
    }

    /// Atomically replaces the state file after validation, creating its
    /// directory first.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        self.validate()?;
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        write_config(path, &serde_json::to_vec_pretty(self)?, "application state")
    }

    /// Puts `path` first among the recent workspaces, once, keeping at most
    /// [`MAX_RECENT_WORKSPACES`]. A relative path is resolved against the
    /// current directory; nothing is stored for a path that cannot be.
    pub fn remember_workspace(&mut self, path: &Path) {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            match std::env::current_dir() {
                Ok(current) => current.join(path),
                Err(_) => return,
            }
        };
        self.recent_workspaces.retain(|known| known != &absolute);
        self.recent_workspaces.insert(0, absolute);
        self.recent_workspaces.truncate(MAX_RECENT_WORKSPACES);
    }

    /// The most recently opened workspace file that still exists.
    pub fn latest_existing_workspace(&self) -> Option<&Path> {
        self.recent_workspaces
            .iter()
            .map(PathBuf::as_path)
            .find(|path| path.is_file())
    }
}
