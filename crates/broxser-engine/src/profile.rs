//! Private browser profiles and their ownership leases (ADR 0007).
//!
//! Every profile directory holds a lease naming the Broxser process that owns
//! it, the guardian that cleans up if that process dies, and the browser. A later
//! start recovers profiles whose owners are provably gone, and nothing else.

use crate::browser::{self, EXIT_TIMEOUT, ProcessIdentity};
use crate::cdp::Cancellation;
use crate::guardian::Guardian;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use tempfile::TempDir;

pub(crate) const PROFILE_PREFIX: &str = "broxser-cdp-";
pub(crate) const LEASE: &str = "broxser-lease.json";
const LEASE_TEMPORARY: &str = "broxser-lease.json.tmp";
const LEASE_VERSION: u32 = 1;
const MAX_LEASE_BYTES: u64 = 4096;

/// Who owns a profile. PIDs and start times identify a process only within one
/// boot and one PID namespace, so both are recorded too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Lease {
    pub version: u32,
    pub boot_id: String,
    pub pid_namespace: String,
    pub owner: ProcessIdentity,
    pub guardian: Option<ProcessIdentity>,
    pub browser: Option<ProcessIdentity>,
}

impl Lease {
    pub(crate) fn for_current_process() -> Result<Self> {
        let host = Host::current()?;
        let owner = browser::procfs::read(std::process::id())
            .ok_or_else(|| anyhow!("read the identity of this process from /proc"))?
            .identity;
        Ok(Self {
            version: LEASE_VERSION,
            boot_id: host.boot_id,
            pid_namespace: host.pid_namespace,
            owner,
            guardian: None,
            browser: None,
        })
    }

    /// Replaces the lease atomically and durably: after a power loss the
    /// profile holds either the previous or the new complete lease.
    pub(crate) fn write(&self, profile: &Path) -> Result<()> {
        let temporary = profile.join(LEASE_TEMPORARY);
        match fs::remove_file(&temporary) {
            Err(error) if error.kind() != ErrorKind::NotFound => {
                return Err(error).context("remove an interrupted lease write");
            }
            _ => {}
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .context("create profile lease")?;
        file.write_all(&serde_json::to_vec(self)?)
            .context("write profile lease")?;
        file.sync_all().context("sync profile lease")?;
        fs::rename(&temporary, profile.join(LEASE)).context("commit profile lease")?;
        File::open(profile)
            .and_then(|directory| directory.sync_all())
            .context("sync profile directory")
    }

    /// Reads a lease. Profile contents are untrusted: the lease must be a small
    /// regular file, not a symlink, with a known version and plausible fields.
    pub(crate) fn read(profile: &Path) -> Result<Self> {
        let path = profile.join(LEASE);
        if !fs::symlink_metadata(&path)
            .context("read profile lease")?
            .is_file()
        {
            bail!("profile lease is not a regular file");
        }
        let mut bytes = Vec::new();
        File::open(&path)?
            .take(MAX_LEASE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_LEASE_BYTES {
            bail!("profile lease is too large");
        }
        let lease: Self = serde_json::from_slice(&bytes).context("parse profile lease")?;
        let identities = [
            Some(&lease.owner),
            lease.guardian.as_ref(),
            lease.browser.as_ref(),
        ];
        if lease.version != LEASE_VERSION
            || !plausible(&lease.boot_id, |c| c.is_ascii_hexdigit() || c == '-')
            || !plausible(&lease.pid_namespace, |c| c.is_ascii_graphic())
            || identities
                .into_iter()
                .flatten()
                .any(|identity| identity.pid == 0 || identity.pid > i32::MAX as u32)
        {
            bail!("profile lease has an unsupported version or invalid fields");
        }
        Ok(lease)
    }
}

fn plausible(text: &str, allowed: impl Fn(char) -> bool) -> bool {
    !text.is_empty() && text.len() <= 64 && text.chars().all(allowed)
}

/// The boot and PID namespace this process runs in.
pub(crate) struct Host {
    pub boot_id: String,
    pub pid_namespace: String,
}

impl Host {
    pub(crate) fn current() -> Result<Self> {
        let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .context("read the kernel boot ID")?
            .trim()
            .to_owned();
        let pid_namespace = fs::read_link("/proc/self/ns/pid")
            .context("read the PID namespace")?
            .to_string_lossy()
            .into_owned();
        Ok(Self {
            boot_id,
            pid_namespace,
        })
    }
}

/// A private profile directory with its lease and guardian. Dropping it removes
/// the directory, then releases the guardian. If this process dies first, the
/// guardian stops the browser and removes the directory instead.
pub(crate) struct OwnedProfile {
    directory: TempDir,
    lease: Lease,
    guardian: Option<Guardian>,
    removed: bool,
}

impl OwnedProfile {
    /// Recovers stale profiles in `root` (the system temporary directory when
    /// `None`), creates a new mode-0700 profile there and starts its guardian.
    pub(crate) fn create(root: Option<&Path>, cancel: &Cancellation) -> Result<Self> {
        let root = root.map(Path::to_owned).unwrap_or_else(env::temp_dir);
        recover_stale(&root);
        let directory = tempfile::Builder::new()
            .prefix(PROFILE_PREFIX)
            .permissions(Permissions::from_mode(0o700))
            .tempdir_in(&root)
            .context("create private browser profile")?;
        let mut lease = Lease::for_current_process()?;
        lease.write(directory.path())?;
        let guardian = Guardian::start(directory.path(), &lease.owner, cancel)?;
        lease.guardian = Some(guardian.identity());
        let profile = Self {
            directory,
            lease,
            guardian: Some(guardian),
            removed: false,
        };
        profile.lease.write(profile.path())?;
        Ok(profile)
    }

    pub(crate) fn path(&self) -> &Path {
        self.directory.path()
    }

    pub(crate) fn guardian(&self) -> Option<ProcessIdentity> {
        self.lease.guardian
    }

    /// Hands the new browser to the guardian, then records it in the lease.
    pub(crate) fn watch(&mut self, browser: ProcessIdentity) -> Result<()> {
        if let Some(guardian) = &mut self.guardian {
            guardian.watch(&browser)?;
        }
        self.lease.browser = Some(browser);
        self.lease.write(self.path())
    }

    /// Removes the profile once the browser has stopped, then releases the
    /// guardian. An error means profile files may remain.
    pub(crate) fn close(mut self) -> Result<()> {
        let removal = self.remove();
        self.release();
        removal
    }

    fn remove(&mut self) -> Result<()> {
        if std::mem::replace(&mut self.removed, true) {
            return Ok(());
        }
        let path = self.directory.path();
        remove_profile(path)
            .with_context(|| format!("remove browser profile {}", path.display()))?;
        // Removed: `TempDir` must not delete whatever takes this path next.
        self.directory.disable_cleanup(true);
        Ok(())
    }

    fn release(&mut self) {
        if let Some(guardian) = self.guardian.take() {
            guardian.release();
        }
    }
}

impl Drop for OwnedProfile {
    fn drop(&mut self) {
        let _ = self.remove();
        self.release();
        // `TempDir` then retries a removal that failed.
    }
}

/// Deletes a profile directory without following symlinks. The lease goes
/// last, so an interrupted removal can still be recognized and finished.
pub(crate) fn remove_profile(profile: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(profile)? {
        let entry = entry?;
        if entry.file_name() == LEASE {
            continue;
        }
        // `file_type` does not follow symlinks; `remove_dir_all` never does.
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    match fs::remove_file(profile.join(LEASE)) {
        Err(error) if error.kind() != ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    fs::remove_dir(profile)
}

/// Removes the profile a guardian protects if its lease still names the same
/// owner. Without a lease it removes only an empty directory: its owner was
/// interrupted after deleting everything else.
pub(crate) fn remove_guarded(profile: &Path, owner: &ProcessIdentity) -> Result<()> {
    match fs::symlink_metadata(profile) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("inspect the guarded profile"),
        Ok(metadata) if !metadata.is_dir() => bail!("the guarded profile is not a directory"),
        Ok(_) => {}
    }
    match fs::symlink_metadata(profile.join(LEASE)) {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return fs::remove_dir(profile).context("remove the emptied guarded profile");
        }
        _ => {}
    }
    if Lease::read(profile)?.owner != *owner {
        bail!("the guarded profile is leased to another owner");
    }
    remove_profile(profile).context("remove the guarded profile")
}

enum Verdict {
    /// The owner or its guardian still runs.
    Live,
    /// Ownership cannot be established; never touched.
    Uncertain,
    Stale(Lease),
}

fn verdict(profile: &Path, host: &Host, uid: u32) -> Verdict {
    let Ok(metadata) = fs::symlink_metadata(profile) else {
        return Verdict::Uncertain;
    };
    if !metadata.is_dir() || metadata.uid() != uid {
        return Verdict::Uncertain;
    }
    let Ok(lease) = Lease::read(profile) else {
        return Verdict::Uncertain;
    };
    if lease.boot_id != host.boot_id {
        // No process of an earlier boot is still running.
        return Verdict::Stale(lease);
    }
    if lease.pid_namespace != host.pid_namespace {
        return Verdict::Uncertain;
    }
    // A zombie, or a reused PID with another start time, is not the recorded process.
    if browser::is_running(&lease.owner)
        || lease
            .guardian
            .is_some_and(|guardian| browser::is_running(&guardian))
    {
        return Verdict::Live;
    }
    Verdict::Stale(lease)
}

/// Removes profiles under `root` whose owner and guardian are provably gone,
/// after stopping a recorded browser that still runs on that profile. Entries
/// that are not such profiles, or that some process still names, are kept.
pub(crate) fn recover_stale(root: &Path) {
    let Ok(host) = Host::current() else {
        return;
    };
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let uid = rustix::process::geteuid().as_raw();
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .as_encoded_bytes()
            .starts_with(PROFILE_PREFIX.as_bytes())
        {
            continue;
        }
        let profile = entry.path();
        if let Verdict::Stale(lease) = verdict(&profile, &host, uid) {
            recover(&profile, &lease, &host);
        }
    }
}

fn recover(profile: &Path, lease: &Lease, host: &Host) {
    // Only a browser of this boot that still runs on this very profile is stopped.
    if lease.boot_id == host.boot_id
        && let Some(orphan) = lease.browser
        && browser::launched_with(profile).contains(&orphan)
    {
        let mut processes = browser::descendants(&orphan);
        processes.extend(browser::referencing(profile));
        let _ = browser::terminate(&orphan);
        browser::wait_for_exit(&processes, EXIT_TIMEOUT);
    }
    if browser::referencing(profile).is_empty() {
        let _ = remove_profile(profile);
    }
}

#[cfg(test)]
mod tests;
