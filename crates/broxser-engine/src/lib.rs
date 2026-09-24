//! Browser runtime adapter: an owned Helium (or explicitly selected CDP browser)
//! subprocess with a private temporary profile, controlled over loopback CDP.
//!
//! [`capture_workspace`] is the one-shot capture used by the CLI. Call it on a
//! worker thread; it blocks on browser I/O and stops only its own child.

mod browser;
mod capture;
mod cdp;
mod device;
#[cfg(test)]
mod test_support;

use std::time::Duration;

pub use browser::{BrowserOptions, discover_browser};
pub use capture::{CaptureFrame, CaptureReport, capture_workspace};
pub use cdp::{Cancellation, Cancelled};

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
