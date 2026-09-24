//! Validated workspace configuration and browser independent session sync routing.
//!
//! A workspace is untrusted input. Call [`Workspace::validate`] before using a
//! value built in memory; [`Workspace::load`] and [`Workspace::save`] do this for you.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;
use url::Url;

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_DEVICES: usize = 8;
pub const MAX_PHYSICAL_PIXELS: f64 = 24_000_000.0;
const MAX_CONFIG_BYTES: u64 = 1_048_576;
static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported workspace schema version {0}; expected {SCHEMA_VERSION}")]
    UnsupportedSchema(u32),
    #[error("invalid workspace: {0}")]
    Invalid(String),
    #[error("sync event sequence {sequence} is not newer than {previous} for device {device}")]
    StaleSequence {
        device: String,
        sequence: u64,
        previous: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub schema_version: u32,
    pub name: String,
    pub url: String,
    pub sessions: Vec<Session>,
    pub devices: Vec<Device>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub device_scale_factor: f64,
    pub mobile: bool,
    pub touch: bool,
    pub session: String,
}

impl Workspace {
    pub fn demo() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            name: "Demo workspace".into(),
            url: "http://127.0.0.1:4173".into(),
            sessions: vec![
                Session {
                    id: "guest".into(),
                    name: "Guest".into(),
                },
                Session {
                    id: "admin".into(),
                    name: "Admin".into(),
                },
            ],
            devices: vec![
                Device {
                    id: "phone".into(),
                    name: "Phone".into(),
                    width: 390,
                    height: 844,
                    device_scale_factor: 1.0,
                    mobile: true,
                    touch: true,
                    session: "guest".into(),
                },
                Device {
                    id: "tablet".into(),
                    name: "Tablet".into(),
                    width: 768,
                    height: 1024,
                    device_scale_factor: 1.0,
                    mobile: true,
                    touch: true,
                    session: "guest".into(),
                },
                Device {
                    id: "desktop".into(),
                    name: "Desktop".into(),
                    width: 1440,
                    height: 900,
                    device_scale_factor: 1.0,
                    mobile: false,
                    touch: false,
                    session: "admin".into(),
                },
            ],
        }
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        // Cap input before parsing so a malformed config cannot allocate without bound.
        let file = fs::File::open(path)?;
        if file.metadata()?.len() > MAX_CONFIG_BYTES {
            return Err(Error::Invalid("workspace file exceeds 1 MiB".into()));
        }
        let mut bytes = Vec::new();
        file.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(Error::Invalid("workspace file exceeds 1 MiB".into()));
        }
        let workspace: Self = serde_json::from_slice(&bytes)?;
        workspace.validate()?;
        Ok(workspace)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(Error::UnsupportedSchema(self.schema_version));
        }
        validate_name("workspace name", &self.name)?;
        validate_http_url(&self.url)?;
        if self.sessions.is_empty() || self.sessions.len() > 8 {
            return Err(Error::Invalid("workspace needs 1 to 8 sessions".into()));
        }
        if self.devices.is_empty() || self.devices.len() > MAX_DEVICES {
            return Err(Error::Invalid(format!(
                "workspace needs 1 to {MAX_DEVICES} devices"
            )));
        }

        let mut sessions = HashSet::new();
        for session in &self.sessions {
            validate_id("session", &session.id)?;
            validate_name("session name", &session.name)?;
            if !sessions.insert(session.id.as_str()) {
                return Err(Error::Invalid(format!(
                    "duplicate session id: {}",
                    session.id
                )));
            }
        }

        let mut devices = HashSet::new();
        let mut physical_pixels = 0.0;
        for device in &self.devices {
            validate_id("device", &device.id)?;
            validate_name("device name", &device.name)?;
            if !devices.insert(device.id.as_str()) {
                return Err(Error::Invalid(format!(
                    "duplicate device id: {}",
                    device.id
                )));
            }
            if !(200..=4096).contains(&device.width) || !(200..=4096).contains(&device.height) {
                return Err(Error::Invalid(format!(
                    "device {} viewport must be 200..=4096 per axis",
                    device.id
                )));
            }
            if !device.device_scale_factor.is_finite()
                || !(0.5..=4.0).contains(&device.device_scale_factor)
            {
                return Err(Error::Invalid(format!(
                    "device {} scale factor must be finite and within 0.5..=4",
                    device.id
                )));
            }
            if !sessions.contains(device.session.as_str()) {
                return Err(Error::Invalid(format!(
                    "device {} refers to unknown session {}",
                    device.id, device.session
                )));
            }
            physical_pixels += f64::from(device.width)
                * f64::from(device.height)
                * device.device_scale_factor.powi(2);
        }
        if physical_pixels > MAX_PHYSICAL_PIXELS {
            return Err(Error::Invalid(format!(
                "physical pixel budget exceeds {MAX_PHYSICAL_PIXELS}"
            )));
        }
        Ok(())
    }

    /// Atomically replaces a config in the same directory after validation.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        self.validate()?;
        let path = path.as_ref();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let filename = path
            .file_name()
            .ok_or_else(|| Error::Invalid("save path needs a filename".into()))?;
        let data = serde_json::to_vec_pretty(self)?;
        if data.len() as u64 > MAX_CONFIG_BYTES {
            return Err(Error::Invalid("workspace file exceeds 1 MiB".into()));
        }
        for _ in 0..16 {
            let seq = SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let mut temp_name = filename.to_os_string();
            temp_name.push(format!(".tmp-{}-{seq}", std::process::id()));
            let temp_path = parent.join(temp_name);
            let mut file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
            {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            };
            let result = (|| -> std::io::Result<()> {
                file.write_all(&data)?;
                file.sync_all()?;
                fs::rename(&temp_path, path)?;
                fs::File::open(parent)?.sync_all()?;
                Ok(())
            })();
            if result.is_err() {
                let _ = fs::remove_file(&temp_path);
            }
            return result.map_err(Error::Io);
        }
        Err(Error::Invalid(
            "could not reserve temporary workspace file".into(),
        ))
    }
}

fn validate_id(kind: &str, id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(Error::Invalid(format!(
            "{kind} id must be a 1..=64 character ASCII slug"
        )));
    }
    Ok(())
}

fn validate_name(kind: &str, name: &str) -> Result<()> {
    if name.trim().is_empty() || name.len() > 100 || name.chars().any(char::is_control) {
        return Err(Error::Invalid(format!(
            "{kind} must be nonempty, <=100 bytes, and contain no control characters"
        )));
    }
    Ok(())
}

/// Validates a URL that Broxser may open: absolute HTTP or HTTPS with a host and
/// without userinfo or control characters. This is input hygiene, not an allowlist.
pub fn validate_url(value: &str) -> Result<()> {
    validate_http_url(value)
}

fn validate_http_url(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 2048 || value.chars().any(char::is_control) {
        return Err(Error::Invalid(
            "URL is empty, too long, or contains control characters".into(),
        ));
    }
    let parsed = Url::parse(value)
        .map_err(|_| Error::Invalid("URL must be absolute HTTP or HTTPS".into()))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(Error::Invalid(
            "URL must be HTTP or HTTPS with a host and no userinfo".into(),
        ));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub enum SyncAction {
    Navigate { url: String },
    Scroll { x: i32, y: i32 },
    Pointer { x: i32, y: i32 },
    Key { key: String },
}

#[derive(Clone, Debug)]
pub struct SyncEvent {
    pub origin_device: String,
    pub origin_session: String,
    pub sequence: u64,
    pub action: SyncAction,
    /// Set by the router on a delivery. A replayed event has no outgoing routes.
    pub replayed: bool,
}

#[derive(Clone, Debug)]
pub struct SyncDelivery {
    pub destination_device: String,
    pub event: SyncEvent,
}

/// Routes only explicitly enabled source/destination pairs in one session.
pub struct SyncRouter {
    sessions_by_device: HashMap<String, String>,
    routes: HashSet<(String, String)>,
    last_sequence: HashMap<String, u64>,
    pointer_enabled: bool,
    key_enabled: bool,
}

impl SyncRouter {
    pub fn new(workspace: &Workspace) -> Result<Self> {
        workspace.validate()?;
        Ok(Self {
            sessions_by_device: workspace
                .devices
                .iter()
                .map(|d| (d.id.clone(), d.session.clone()))
                .collect(),
            routes: HashSet::new(),
            last_sequence: HashMap::new(),
            pointer_enabled: false,
            key_enabled: false,
        })
    }

    pub fn enable_route(&mut self, from: &str, to: &str) -> Result<()> {
        let source = self
            .sessions_by_device
            .get(from)
            .ok_or_else(|| Error::Invalid(format!("unknown source device: {from}")))?;
        let target = self
            .sessions_by_device
            .get(to)
            .ok_or_else(|| Error::Invalid(format!("unknown destination device: {to}")))?;
        if from == to || source != target {
            return Err(Error::Invalid(
                "sync route must connect distinct devices in the same session".into(),
            ));
        }
        self.routes.insert((from.into(), to.into()));
        Ok(())
    }

    pub fn disable_route(&mut self, from: &str, to: &str) {
        self.routes.remove(&(from.into(), to.into()));
    }

    pub fn set_pointer_enabled(&mut self, enabled: bool) {
        self.pointer_enabled = enabled;
    }
    pub fn set_key_enabled(&mut self, enabled: bool) {
        self.key_enabled = enabled;
    }

    pub fn route(&mut self, event: &SyncEvent) -> Result<Vec<SyncDelivery>> {
        if event.replayed {
            return Ok(Vec::new());
        }
        let session = self
            .sessions_by_device
            .get(&event.origin_device)
            .ok_or_else(|| {
                Error::Invalid(format!("unknown origin device: {}", event.origin_device))
            })?;
        if session != &event.origin_session {
            return Err(Error::Invalid(
                "sync event origin session does not match its device".into(),
            ));
        }
        if let Some(&previous) = self.last_sequence.get(&event.origin_device)
            && event.sequence <= previous
        {
            return Err(Error::StaleSequence {
                device: event.origin_device.clone(),
                sequence: event.sequence,
                previous,
            });
        }
        match &event.action {
            SyncAction::Navigate { url } => validate_http_url(url)?,
            SyncAction::Key { key }
                if key.is_empty() || key.len() > 64 || key.chars().any(char::is_control) =>
            {
                return Err(Error::Invalid(
                    "sync key must be 1..=64 bytes without controls".into(),
                ));
            }
            _ => {}
        }
        self.last_sequence
            .insert(event.origin_device.clone(), event.sequence);
        if matches!(event.action, SyncAction::Pointer { .. }) && !self.pointer_enabled
            || matches!(event.action, SyncAction::Key { .. }) && !self.key_enabled
        {
            return Ok(Vec::new());
        }
        Ok(self
            .routes
            .iter()
            .filter(|(from, to)| {
                from == &event.origin_device && self.sessions_by_device.get(to) == Some(session)
            })
            .map(|(_, to)| {
                let mut replay = event.clone();
                replay.replayed = true;
                SyncDelivery {
                    destination_device: to.clone(),
                    event: replay,
                }
            })
            .collect())
    }
}
