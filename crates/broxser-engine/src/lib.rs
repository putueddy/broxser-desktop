//! Browser runtime adapter: an owned Helium (or explicitly selected CDP browser)
//! subprocess with a private temporary profile, controlled over loopback CDP.
//!
//! [`capture_workspace`] is the one-shot capture used by the CLI. Call it on a
//! worker thread; it blocks on browser I/O and stops only its own child.
//! [`LiveSession`] keeps one browser alive for an open workspace and streams
//! frames to the desktop; its worker thread owns all browser I/O.
//!
//! Each browser gets a guardian process that stops it and removes its profile
//! if the Broxser process dies (ADR 0007). Binaries that launch browsers must
//! call [`run_guardian_if_requested`] first in `main`.

mod browser;
mod capture;
mod cdp;
mod device;
mod guardian;
mod live;
mod profile;
#[cfg(test)]
mod test_support;

use std::time::Duration;

pub use browser::{BrowserOptions, discover_browser};
pub use capture::{CaptureFrame, CaptureReport, capture_workspace};
pub use cdp::{Cancellation, Cancelled};
pub use guardian::run_guardian_if_requested;
pub use live::{
    Command, DeviceStatus, Frame, KeyInput, LiveSession, MAX_PASTE_CHARS, Modifiers, PasteRejected,
    PointerButton, PointerEvent, PointerKind, RuntimeState, Status, SyncSettings, is_paste_key,
    paste_text, to_viewport,
};

/// Per-operation deadlines. These are not an SLA for a whole job.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub startup: Duration,
    /// Also the websocket handshake deadline; see [`cdp::Cdp::connect`].
    pub command: Duration,
    pub load: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            startup: Duration::from_secs(15),
            command: Duration::from_secs(15),
            load: Duration::from_secs(30),
        }
    }
}
