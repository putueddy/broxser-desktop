//! Owned browser subprocess: private temporary profile, loopback CDP endpoint and
//! cleanup that waits for every browser process before deleting the profile. If
//! the Broxser process itself dies, the profile's guardian does both (ADR 0007).

use crate::Limits;
use crate::cdp::{Cancellation, Cdp, parse_response};
use crate::profile::OwnedProfile;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// How long browser processes may take to exit after the main process is killed.
pub(crate) const EXIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Helium bundles uBlock Origin as a component extension with this ID and makes
/// it follow the per-extension incognito preference, which defaults to enabled.
/// Every CDP BrowserContext is an off-the-record profile, so each Broxser session
/// would start its own blocker. While loading filter lists it records tabs that
/// made requests and later calls `tabs.reload` on them, replaying navigations and
/// aborting in-flight ones with `net::ERR_ABORTED`. See ADR 0004.
pub(crate) const HELIUM_UBLOCK_ID: &str = "blockjmkbacgjkknlgpkjjiijinjdanf";

#[derive(Debug, Clone)]
pub struct BrowserOptions {
    /// Explicit browser executable. Only an explicit path can select Chromium
    /// or another CDP browser for diagnostics.
    pub executable: PathBuf,
    pub headless: bool,
    /// Parent directory of the private temporary profile. `None` uses the system
    /// temporary directory. Tests pass a unique root so they only observe their
    /// own browser processes and files.
    pub profile_root: Option<PathBuf>,
    /// Stops startup and waits from another thread.
    pub cancel: Cancellation,
}

impl Default for BrowserOptions {
    fn default() -> Self {
        Self {
            executable: discover_browser().unwrap_or_default(),
            headless: true,
            profile_root: None,
            cancel: Cancellation::default(),
        }
    }
}

/// Discover Helium via BROXSER_HELIUM_BIN, then helium/helium-browser on PATH.
pub fn discover_browser() -> Result<PathBuf> {
    if let Some(path) = env::var_os("BROXSER_HELIUM_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        bail!(
            "BROXSER_HELIUM_BIN does not name a browser file: {}",
            path.display()
        );
    }
    for name in ["helium", "helium-browser"] {
        if let Some(paths) = env::var_os("PATH") {
            for directory in env::split_paths(&paths) {
                let path = directory.join(name);
                if path.is_file() {
                    return Ok(path);
                }
            }
        }
    }
    bail!("Helium not found; set BROXSER_HELIUM_BIN or BrowserOptions.executable")
}

pub(crate) struct BrowserProcess {
    child: Child,
    /// Captured once at spawn. Never infer ownership from a possibly reused PID.
    /// `None` only while a failed identity capture unwinds startup.
    identity: Option<ProcessIdentity>,
    /// Removed after the browser stops. `None` once `shutdown` has handled it.
    profile: Option<OwnedProfile>,
    cancel: Cancellation,
}

impl BrowserProcess {
    /// Launches the browser. `seed` writes Broxser's profile preferences first;
    /// only the reproducer's baseline mode launches an unseeded profile. The
    /// profile's guardian is ready before the browser starts.
    pub(crate) fn start(options: &BrowserOptions, seed: bool) -> Result<Self> {
        Self::start_with_user_agent(options, seed, None)
    }

    fn start_with_user_agent(
        options: &BrowserOptions,
        seed: bool,
        user_agent: Option<&str>,
    ) -> Result<Self> {
        if options.executable.as_os_str().is_empty() {
            bail!("browser executable is empty; install Helium or set BROXSER_HELIUM_BIN");
        }
        if !options.executable.is_file() {
            bail!(
                "browser executable does not exist: {}",
                options.executable.display()
            );
        }
        options.cancel.check()?;
        #[cfg(unix)]
        restrict_core_dumps()?;
        let profile = OwnedProfile::create(options.profile_root.as_deref(), &options.cancel)?;
        if seed {
            seed_profile(profile.path())?;
        }
        let home = create_private_home(profile.path())?;
        let mut command = Command::new(&options.executable);
        command
            .args(launch_args(profile.path(), options.headless, user_agent))
            // Chromium keeps its crash database under the default user data
            // directory (for Helium ~/.config/net.imput.helium), shared with a
            // personal installation. Keep crash data in the private profile.
            .env(
                "BREAKPAD_DUMP_LOCATION",
                profile.path().join("Crash Reports"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (name, value) in environment(&home, |name| env::var_os(name)) {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        // Profile creation includes guardian startup. Cancellation during that
        // work must prevent the browser spawn as well.
        options.cancel.check()?;
        let child = command
            .spawn()
            .with_context(|| format!("launch browser {}", options.executable.display()))?;
        let identity = self::identity(child.id());
        // From here on, dropping `browser` stops the child before the profile goes.
        let mut browser = Self {
            child,
            identity,
            profile: Some(profile),
            cancel: options.cancel.clone(),
        };
        let identity = identity.ok_or_else(|| anyhow!("read the browser's identity from /proc"))?;
        if let Some(profile) = &mut browser.profile {
            profile.watch(identity)?;
        }
        Ok(browser)
    }

    /// Discovers the native UA only through an owned browser, before workspace
    /// contexts or navigation exist. A normalized UA needs one replacement
    /// runtime: the discovery runtime must finish checked cleanup first.
    pub(crate) fn start_connected(
        options: &BrowserOptions,
        seed: bool,
        limits: &Limits,
    ) -> Result<(Self, Cdp)> {
        let mut browser = Self::start(options, seed)?;
        let discovered = (|| {
            let mut cdp = browser.connect(limits)?;
            let user_agent = if options.headless {
                let id = cdp.send("Browser.getVersion", json!({}), None)?;
                let deadline = Instant::now() + limits.command;
                let version = loop {
                    options.cancel.check()?;
                    if let Some(response) = cdp.take_response(id) {
                        break parse_response(response, "Browser.getVersion")?;
                    }
                    if !cdp.read_until(deadline)? {
                        bail!("CDP Browser.getVersion timed out during browser discovery");
                    }
                };
                headed_user_agent(version.get("userAgent").and_then(serde_json::Value::as_str))
            } else {
                None
            };
            options.cancel.check()?;
            Ok((cdp, user_agent))
        })();
        let (cdp, user_agent) = match discovered {
            Ok(discovered) => discovered,
            Err(error) => return Err(startup_failed(browser, error)),
        };
        let Some(user_agent) = user_agent else {
            return Ok((browser, cdp));
        };
        drop(cdp);
        let processes = startup_processes(&browser);
        browser
            .shutdown()
            .context("discovery browser cleanup failed")
            .with_context(|| StartupDiagnostics(processes.clone()))?;
        options
            .cancel
            .check()
            .with_context(|| StartupDiagnostics(processes.clone()))?;
        let mut browser = Self::start_with_user_agent(options, seed, Some(&user_agent))
            .context(StartupDiagnostics(processes))?;
        match browser.connect(limits) {
            Ok(cdp) => Ok((browser, cdp)),
            Err(error) => Err(startup_failed(browser, error)),
        }
    }

    /// Processes useful for diagnostics, including unrelated profile observers.
    /// This broad list must never be used as proof of signal ownership.
    pub(crate) fn processes(&self) -> Vec<ProcessIdentity> {
        let mut processes = self.owned_processes();
        if let Some(profile) = &self.profile {
            for process in referencing(profile.path()) {
                if !processes.contains(&process) {
                    processes.push(process);
                }
            }
        }
        processes
    }

    /// Only the recorded browser's verified tree is proven to belong to us.
    fn owned_processes(&self) -> Vec<ProcessIdentity> {
        self.identity.as_ref().map_or_else(Vec::new, descendants)
    }

    /// The guardian that cleans up if this process dies; it exits on release.
    pub(crate) fn guardian(&self) -> Option<ProcessIdentity> {
        self.profile.as_ref().and_then(OwnedProfile::guardian)
    }

    /// Waits for `DevToolsActivePort` and opens the loopback websocket.
    pub(crate) fn connect(&mut self, limits: &Limits) -> Result<Cdp> {
        let (port, path) = self.wait_for_endpoint(limits.startup)?;
        Cdp::connect(port, &path, limits.command, self.cancel.clone())
    }

    fn wait_for_endpoint(&mut self, timeout: Duration) -> Result<(u16, String)> {
        let deadline = Instant::now() + timeout;
        let active_port = self
            .profile
            .as_ref()
            .ok_or_else(|| anyhow!("browser profile already removed"))?
            .path()
            .join("DevToolsActivePort");
        loop {
            self.cancel.check()?;
            // Chromium may be between creating and writing the file; both
            // lines must be present before the content is validated.
            if let Ok(contents) = fs::read_to_string(&active_port)
                && contents.lines().count() >= 2
            {
                return parse_endpoint(&contents);
            }
            if let Some(status) = self.child.try_wait().context("poll browser startup")? {
                bail!("browser exited before CDP was ready ({status})");
            }
            if Instant::now() >= deadline {
                bail!(
                    "browser did not publish a CDP endpoint within {} seconds",
                    timeout.as_secs_f32()
                );
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Kills the browser, waits until its process tree has exited, removes the
    /// profile and releases its guardian. An error means a browser process or
    /// profile file may remain.
    pub(crate) fn shutdown(mut self) -> Result<()> {
        #[cfg(test)]
        crate::test_support::abort_point("before-kill");
        let processes = self.owned_processes();
        let _ = self.child.kill();
        self.child.wait().context("wait for browser exit")?;
        let survivors = match &self.profile {
            Some(profile) => release_or_stop(&processes, profile.path(), EXIT_TIMEOUT),
            None => wait_for_exit(&processes, EXIT_TIMEOUT),
        };
        #[cfg(test)]
        crate::test_support::abort_point("before-remove");
        let removal = self.profile.take().map_or(Ok(()), OwnedProfile::close);
        if survivors > 0 {
            bail!(
                "{survivors} processes of the browser or naming its profile did not exit after \
                 the browser was stopped"
            );
        }
        removal
    }
}

impl Drop for BrowserProcess {
    fn drop(&mut self) {
        // Fallback for early returns and panics; `shutdown` is the checked path.
        // After `shutdown` the reaped PID may belong to another process.
        let Some(profile) = self.profile.take() else {
            return;
        };
        let processes = self.owned_processes();
        let _ = self.child.kill();
        let _ = self.child.wait();
        release_or_stop(&processes, profile.path(), EXIT_TIMEOUT);
        // Removes the profile, then releases the guardian.
        drop(profile);
    }
}

/// Headless Chromium detects no pointing device, so every page would see
/// `(hover: none)` and `(pointer: none)`. These Blink settings restore a mouse
/// (fine pointer, hover); touch emulation still overrides them on touch
/// devices (ADR 0019).
const POINTER_SETTINGS: &str = "--blink-settings=availablePointerTypes=4,primaryPointerType=4,\
availableHoverTypes=2,primaryHoverType=2";

fn launch_args(profile: &Path, headless: bool, user_agent: Option<&str>) -> Vec<OsString> {
    let mut user_data_dir = OsString::from("--user-data-dir=");
    user_data_dir.push(profile);
    let mut args: Vec<OsString> = vec![
        "--remote-debugging-address=127.0.0.1".into(),
        "--remote-debugging-port=0".into(),
        user_data_dir,
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        // A temporary profile needs no key from the user's keyring. Without
        // this, Chromium asks the desktop's Secret Service or KWallet for the
        // key that also protects a personal profile's cookies, or creates one
        // there that outlives the profile (ADR 0020).
        "--password-store=basic".into(),
        "about:blank".into(),
    ];
    if headless {
        args.push("--headless=new".into());
        args.push(POINTER_SETTINGS.into());
        if let Some(user_agent) = user_agent {
            args.push(format!("--user-agent={user_agent}").into());
        }
    }
    args
}

/// The browser's home directory inside the private profile (ADR 0020).
pub(crate) const PRIVATE_HOME: &str = "home";

/// Creates the browser's private home directory, mode 0700 like the profile.
fn create_private_home(profile: &Path) -> Result<PathBuf> {
    let home = profile.join(PRIVATE_HOME);
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .context("create the browser's private home directory")?;
    Ok(home)
}

/// The browser's environment (ADR 0020): a variable to set, or `None` to remove
/// it. `HOME` is a directory inside the private profile, so NSS creates the
/// browser's certificate and key database there and never opens the user's
/// `~/.pki/nssdb`, whose CAs and client certificates would otherwise be trusted
/// and offered in Broxser's sessions. `XDG_DATA_HOME` and `XDG_CONFIG_HOME` go
/// too: Chromium keeps that database under the data directory when one is set.
/// The user's cache directory stays, so fontconfig reuses its caches of the
/// system fonts instead of rebuilding them at every browser start. X11 still
/// needs the caller's display authorization: preserve an explicit `XAUTHORITY`
/// unchanged, or name the original home's `.Xauthority` before replacing HOME.
/// This passes only its path; no authorization or certificate files are copied.
/// Disable inherited TLS key logging, which could leave handshake secrets in
/// an external file after the temporary profile has been removed.
fn environment(
    home: &Path,
    inherited: impl Fn(&str) -> Option<OsString>,
) -> [(&'static str, Option<OsString>); 6] {
    let original_home = inherited("HOME");
    let absolute = |value: &OsString| Path::new(value).is_absolute();
    let cache = inherited("XDG_CACHE_HOME").filter(absolute).or_else(|| {
        original_home
            .clone()
            .filter(absolute)
            .map(|home| Path::new(&home).join(".cache").into_os_string())
    });
    let xauthority = inherited("XAUTHORITY").or_else(|| {
        original_home
            .filter(|home| !home.is_empty())
            .map(|home| Path::new(&home).join(".Xauthority").into_os_string())
    });
    [
        ("HOME", Some(home.as_os_str().to_owned())),
        ("XDG_DATA_HOME", None),
        ("XDG_CONFIG_HOME", None),
        ("XDG_CACHE_HOME", cache),
        ("XAUTHORITY", xauthority),
        ("SSLKEYLOGFILE", None),
    ]
}

/// Added to a certificate error: a page that a personal browser opens can fail
/// here, because Broxser's browser trusts its built-in roots and the machine's
/// `CACertificates` policy, not a user's certificate database (ADR 0020).
pub(crate) fn certificate_note(error: &str) -> &'static str {
    if error == "net::ERR_CERT_AUTHORITY_INVALID" {
        " (Broxser trusts the browser's built-in roots and the CACertificates policy, not a \
         personal certificate store)"
    } else {
        ""
    }
}

/// Capture diagnostics retain observed identities even when startup fails
/// before it can return a runtime. An anyhow context preserves the original
/// error's type (including cancellation).
#[derive(Debug)]
pub(crate) struct StartupDiagnostics(pub(crate) Vec<ProcessIdentity>);

impl std::fmt::Display for StartupDiagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("owned browser startup failed")
    }
}

fn startup_processes(browser: &BrowserProcess) -> Vec<ProcessIdentity> {
    let mut processes = browser.processes();
    processes.extend(browser.guardian());
    processes
}

fn startup_failed(browser: BrowserProcess, error: anyhow::Error) -> anyhow::Error {
    let processes = startup_processes(&browser);
    let error = match browser.shutdown() {
        Ok(()) => error,
        Err(cleanup) => error.context(format!("browser cleanup also failed: {cleanup:#}")),
    };
    error.context(StartupDiagnostics(processes))
}

/// Replace only the native headless product token, retaining the browser's
/// actual platform, full version and every other byte. Invalid or absent UAs
/// leave the first runtime in use; never synthesize a version or platform.
fn headed_user_agent(native: Option<&str>) -> Option<String> {
    const MAX_USER_AGENT: usize = 4096;
    let native = native?;
    if native.len() > MAX_USER_AGENT || native.bytes().any(|byte| !(b' '..=b'~').contains(&byte)) {
        return None;
    }
    let mut headless = native
        .split(' ')
        .filter(|token| token.starts_with("HeadlessChrome/"));
    let token = headless.next()?;
    if headless.next().is_some() {
        return None;
    }
    let version = token.strip_prefix("HeadlessChrome/")?;
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() != 4
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    let offset = native
        .match_indices(token)
        .find(|(offset, _)| *offset == 0 || native.as_bytes()[offset - 1] == b' ')?
        .0;
    let mut headed = native.to_owned();
    headed.replace_range(offset..offset + token.len(), &format!("Chrome/{version}"));
    Some(headed)
}

/// Keeps browser memory out of kernel core dumps. Renderer memory holds cookies
/// and page content that a crash collector such as systemd-coredump or apport
/// would store outside the private profile, and streaming a renderer's 20+ GB,
/// mostly empty dump held the dying renderer, and `Target.targetCrashed`, for
/// more than 45 seconds. A zero soft `RLIMIT_CORE` suppresses dumps where it is
/// honored; systemd's default `core_pattern` passes a fixed unlimited value
/// instead, so a zero `coredump_filter` also leaves every memory mapping out of
/// the dump. The browser inherits both. std has no safe per-child hook, so they
/// apply to the Broxser process itself. Crashpad reports still go to the profile.
#[cfg(unix)]
fn restrict_core_dumps() -> Result<()> {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    let limit = getrlimit(Resource::Core);
    setrlimit(
        Resource::Core,
        Rlimit {
            current: Some(0),
            maximum: limit.maximum,
        },
    )
    .context("disable core dumps before launching the browser")?;
    #[cfg(target_os = "linux")]
    fs::write("/proc/self/coredump_filter", "0")
        .context("keep memory out of core dumps before launching the browser")?;
    Ok(())
}

/// Preferences written into the new private profile before the first launch.
/// Unknown extension IDs are harmless for browsers that do not bundle them.
fn seed_profile(profile: &Path) -> Result<()> {
    let directory = profile.join("Default");
    fs::create_dir(&directory).context("create private profile directory")?;
    let preferences = json!({
        "extensions": {"settings": {HELIUM_UBLOCK_ID: {"incognito": false}}}
    });
    fs::write(directory.join("Preferences"), preferences.to_string())
        .context("write private profile preferences")
}

pub(crate) fn parse_endpoint(contents: &str) -> Result<(u16, String)> {
    let mut lines = contents.lines();
    let port: u16 = lines
        .next()
        .ok_or_else(|| anyhow!("CDP endpoint missing port"))?
        .parse()
        .context("CDP endpoint invalid port")?;
    if port == 0 {
        bail!("CDP endpoint port must be nonzero");
    }
    let path = lines
        .next()
        .ok_or_else(|| anyhow!("CDP endpoint missing websocket path"))?;
    let suffix = path
        .strip_prefix("/devtools/browser/")
        .ok_or_else(|| anyhow!("CDP endpoint has unexpected websocket path"))?;
    if suffix.is_empty()
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        bail!("CDP endpoint has unsafe websocket path");
    }
    if lines.next().is_some() {
        bail!("CDP endpoint has unexpected extra lines");
    }
    Ok((port, path.to_owned()))
}

/// A process identified by PID and kernel start time, so a reused PID is never
/// mistaken for a browser process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProcessIdentity {
    pub pid: u32,
    pub start_time: u64,
}

/// The identity of the process that holds `pid` now, zombies included.
#[cfg(target_os = "linux")]
pub(crate) fn identity(pid: u32) -> Option<ProcessIdentity> {
    procfs::read(pid).map(|process| process.identity)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn identity(_pid: u32) -> Option<ProcessIdentity> {
    None
}

/// `root` and its current descendants, or nothing if `root` has exited or its
/// PID now belongs to another process.
pub(crate) fn descendants(root: &ProcessIdentity) -> Vec<ProcessIdentity> {
    let tree = process_tree(root.pid);
    if tree.first() == Some(root) {
        tree
    } else {
        Vec::new()
    }
}

/// Sends SIGKILL to `process` if it is still running. The pidfd pins whichever
/// process holds the PID before its start time is compared, so a reused PID is
/// never signaled. Returns whether a signal was sent.
#[cfg(target_os = "linux")]
pub(crate) fn terminate(process: &ProcessIdentity) -> std::io::Result<bool> {
    use rustix::io::Errno;
    use rustix::process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal};
    let Some(pid) = i32::try_from(process.pid).ok().and_then(Pid::from_raw) else {
        return Ok(false);
    };
    let pidfd = match pidfd_open(pid, PidfdFlags::empty()) {
        Ok(pidfd) => pidfd,
        Err(Errno::SRCH) => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !is_running(process) {
        return Ok(false);
    }
    match pidfd_send_signal(&pidfd, Signal::KILL) {
        Ok(()) => Ok(true),
        Err(Errno::SRCH) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn terminate(_process: &ProcessIdentity) -> std::io::Result<bool> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// Fails unless process file descriptors work (Linux 5.3 or newer), which
/// guardians need to signal only the processes they recorded.
#[cfg(target_os = "linux")]
pub(crate) fn check_pidfd() -> Result<()> {
    use rustix::process::{PidfdFlags, getpid, pidfd_open};
    pidfd_open(getpid(), PidfdFlags::empty())
        .map(drop)
        .context("process file descriptors are unavailable; Linux 5.3 or newer is required")
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn check_pidfd() -> Result<()> {
    bail!("browser crash cleanup is implemented for Linux only (ADR 0007)")
}

/// Running processes started with `--user-data-dir=<profile>`, the argument
/// Broxser gives each browser it launches on that private profile.
#[cfg(target_os = "linux")]
pub(crate) fn launched_with(profile: &Path) -> Vec<ProcessIdentity> {
    let mut argument = b"--user-data-dir=".to_vec();
    argument.extend_from_slice(profile.as_os_str().as_encoded_bytes());
    procfs::with_argument(&argument)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn launched_with(_profile: &Path) -> Vec<ProcessIdentity> {
    Vec::new()
}

/// Running crash handlers of a browser on `profile`: Chromium starts them
/// with `--database=<profile>/Crash Reports`.
#[cfg(target_os = "linux")]
pub(crate) fn crash_handlers_of(profile: &Path) -> Vec<ProcessIdentity> {
    let mut prefix = b"--database=".to_vec();
    prefix.extend_from_slice(profile.as_os_str().as_encoded_bytes());
    prefix.push(b'/');
    procfs::with_argument_prefix(&prefix)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn crash_handlers_of(_profile: &Path) -> Vec<ProcessIdentity> {
    Vec::new()
}

/// The browser and its current descendants. Chromium's sandboxed zygotes,
/// renderers, GPU and utility processes are children or grandchildren of the
/// browser; its crash handler detaches and is found through the profile path.
#[cfg(target_os = "linux")]
pub(crate) fn process_tree(root: u32) -> Vec<ProcessIdentity> {
    let processes = procfs::all();
    let mut tree: Vec<ProcessIdentity> = processes
        .iter()
        .filter(|process| process.identity.pid == root)
        .map(|process| process.identity)
        .collect();
    let mut index = 0;
    while index < tree.len() {
        let parent = tree[index].pid;
        tree.extend(
            processes
                .iter()
                .filter(|process| process.parent == parent && !process.zombie)
                .map(|process| process.identity),
        );
        index += 1;
    }
    tree
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn process_tree(_root: u32) -> Vec<ProcessIdentity> {
    Vec::new()
}

#[cfg(target_os = "linux")]
pub(crate) fn referencing(path: &Path) -> Vec<ProcessIdentity> {
    procfs::referencing(path)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn referencing(_path: &Path) -> Vec<ProcessIdentity> {
    Vec::new()
}

/// Waits until none of `processes` runs and no running process names `profile`
/// any more; only then may the profile be removed. Every Helium process carries
/// the profile in its command line, including detached helpers and helpers the
/// browser started after `processes` was recorded, which otherwise re-create
/// profile directories while they shut down. Once none is left, nothing of that
/// browser can start another. Returns how many still ran at the deadline.
pub(crate) fn wait_for_release(
    processes: &[ProcessIdentity],
    profile: &Path,
    timeout: Duration,
) -> usize {
    let deadline = Instant::now() + timeout;
    let mut waiting = processes.to_vec();
    loop {
        let running = wait_for_exit(&waiting, deadline.saturating_duration_since(Instant::now()));
        let naming = referencing(profile);
        if running == 0 && naming.is_empty() {
            return 0;
        }
        waiting.retain(is_running);
        for process in naming {
            if !waiting.contains(&process) {
                waiting.push(process);
            }
        }
        if Instant::now() >= deadline {
            return waiting.len();
        }
    }
}

/// [`wait_for_release`], then, if something of the browser still runs, kills
/// by process identity each proven-owned process that runs and each process
/// launched on `profile` or keeping its crash reports there, and waits once
/// more. A crash handler that traces a renderer caught by the kill otherwise
/// keeps itself, that renderer and its sandbox namespace alive indefinitely
/// (validation, P1.4); a stopped handler releases them. Processes that merely
/// mention the profile are waited for, never signalled. Returns how many
/// still run. `owned` must contain only identity-verified browser descendants;
/// broad profile references are wait-only observations, never ownership proof.
pub(crate) fn release_or_stop(
    owned: &[ProcessIdentity],
    profile: &Path,
    timeout: Duration,
) -> usize {
    if wait_for_release(owned, profile, timeout) == 0 {
        return 0;
    }
    let mut stopped: Vec<ProcessIdentity> = owned.iter().copied().filter(is_running).collect();
    for process in launched_with(profile)
        .into_iter()
        .chain(crash_handlers_of(profile))
    {
        if !stopped.contains(&process) {
            stopped.push(process);
        }
    }
    for process in &stopped {
        let _ = terminate(process);
    }
    wait_for_release(&stopped, profile, timeout)
}

/// Waits until none of `processes` is running. Returns how many still run.
pub(crate) fn wait_for_exit(processes: &[ProcessIdentity], timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;
    loop {
        let running = processes
            .iter()
            .filter(|process| is_running(process))
            .count();
        if running == 0 || Instant::now() >= deadline {
            return running;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn is_running(process: &ProcessIdentity) -> bool {
    procfs::read(process.pid).is_some_and(|found| found.identity == *process && !found.zombie)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn is_running(_process: &ProcessIdentity) -> bool {
    false
}

#[cfg(target_os = "linux")]
pub(crate) mod procfs {
    use super::ProcessIdentity;
    use std::fs;
    use std::path::Path;

    pub(crate) struct ProcessEntry {
        pub identity: ProcessIdentity,
        pub parent: u32,
        pub zombie: bool,
    }

    pub(crate) fn read(pid: u32) -> Option<ProcessEntry> {
        parse_stat(pid, &fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
    }

    pub(crate) fn all() -> Vec<ProcessEntry> {
        let Ok(entries) = fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
            .filter_map(read)
            .collect()
    }

    /// Running processes whose command line mentions `path`.
    pub(crate) fn referencing(path: &Path) -> Vec<ProcessIdentity> {
        let needle = path.as_os_str().as_encoded_bytes();
        matching(|cmdline| cmdline.windows(needle.len()).any(|window| window == needle))
    }

    /// Running processes with `argument` as one whole argument. Chromium may
    /// replace argv with one process title whose arguments are joined by spaces.
    pub(crate) fn with_argument(argument: &[u8]) -> Vec<ProcessIdentity> {
        matching(|cmdline| has_argument(cmdline, argument))
    }

    /// Running processes with an argument that starts with `prefix`.
    pub(crate) fn with_argument_prefix(prefix: &[u8]) -> Vec<ProcessIdentity> {
        matching(|cmdline| has_argument_prefix(cmdline, prefix))
    }

    fn matching(condition: impl Fn(&[u8]) -> bool) -> Vec<ProcessIdentity> {
        all()
            .into_iter()
            .filter(|process| !process.zombie)
            .filter(|process| {
                fs::read(format!("/proc/{}/cmdline", process.identity.pid))
                    .is_ok_and(|cmdline| condition(&cmdline))
            })
            .map(|process| process.identity)
            .collect()
    }

    /// Crashpad keeps real NUL-separated argv, including paths with spaces.
    /// Text embedded in another argument is never a crash database marker.
    pub(super) fn has_argument_prefix(cmdline: &[u8], prefix: &[u8]) -> bool {
        !prefix.is_empty()
            && cmdline
                .split(|&byte| byte == 0)
                .any(|arg| arg.starts_with(prefix))
    }

    pub(super) fn has_argument(cmdline: &[u8], argument: &[u8]) -> bool {
        if argument.is_empty() {
            return false;
        }
        let mut args = cmdline
            .split(|&byte| byte == 0)
            .filter(|arg| !arg.is_empty());
        let Some(first) = args.next() else {
            return false;
        };
        if first == argument {
            return true;
        }
        if let Some(second) = args.next() {
            // Ordinary argv: spaces inside a shell script or any other argument
            // are contents, never an argument boundary.
            return second == argument || args.any(|arg| arg == argument);
        }
        // Chromium's rewritten argv[0] is one nonempty string (possibly followed
        // by NUL padding). Only this representation admits space boundaries.
        first
            .windows(argument.len())
            .enumerate()
            .any(|(at, window)| {
                window == argument
                    && (at == 0 || first[at - 1] == b' ')
                    && matches!(first.get(at + argument.len()), None | Some(b' '))
            })
    }

    /// Parses `/proc/<pid>/stat`: the command name may contain spaces or
    /// parentheses, so fields are counted after its last `)`.
    pub(crate) fn parse_stat(pid: u32, stat: &str) -> Option<ProcessEntry> {
        let fields: Vec<&str> = stat
            .get(stat.rfind(')')? + 1..)?
            .split_whitespace()
            .collect();
        Some(ProcessEntry {
            identity: ProcessIdentity {
                pid,
                start_time: fields.get(19)?.parse().ok()?,
            },
            parent: fields.get(1)?.parse().ok()?,
            zombie: matches!(*fields.first()?, "Z" | "X"),
        })
    }
}

#[cfg(test)]
#[path = "browser/qa_fidelity.rs"]
mod qa_fidelity;

#[cfg(test)]
#[path = "browser/private_home.rs"]
mod private_home;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_rejects_remote_or_injected_paths() {
        assert_eq!(
            parse_endpoint("12345\n/devtools/browser/abc-123\n").unwrap(),
            (12345, "/devtools/browser/abc-123".to_owned())
        );
        for bad in [
            "0\n/devtools/browser/abc",
            "555\n/devtools/page/abc",
            "555\n/devtools/browser/a?x=1",
            "555\n/devtools/browser/../x",
            "555\n/devtools/browser/abc\nextra",
        ] {
            assert!(parse_endpoint(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn launch_keeps_sandbox_loopback_and_private_state() {
        let profile = Path::new("/tmp/broxser-cdp-test");
        let args: Vec<String> = launch_args(profile, true, Some("UA"))
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();
        assert!(args.contains(&"--remote-debugging-address=127.0.0.1".to_owned()));
        assert!(args.contains(&"--user-data-dir=/tmp/broxser-cdp-test".to_owned()));
        assert!(args.contains(&"--user-agent=UA".to_owned()));
        assert!(args.contains(&POINTER_SETTINGS.to_owned()));
        assert!(args.contains(&"--password-store=basic".to_owned()));
        let headed: Vec<String> = launch_args(profile, false, None)
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();
        assert!(!headed.iter().any(|arg| arg.starts_with("--headless")
            || arg.starts_with("--user-agent")
            || arg.starts_with("--blink-settings")));
        assert!(headed.contains(&"--password-store=basic".to_owned()));
        assert!(
            !args
                .iter()
                .any(|arg| arg.contains("no-sandbox") || arg.contains("remote-allow-origins")),
            "{args:?}"
        );
        let root = tempfile::tempdir().unwrap();
        seed_profile(root.path()).unwrap();
        let preferences: serde_json::Value =
            serde_json::from_slice(&fs::read(root.path().join("Default/Preferences")).unwrap())
                .unwrap();
        assert_eq!(
            preferences["extensions"]["settings"][HELIUM_UBLOCK_ID]["incognito"],
            false
        );
    }

    #[test]
    fn environment_keeps_cache_and_native_display_authorization() {
        let home = Path::new("/tmp/broxser-cdp-test/home");
        let from = |variables: Vec<(&'static str, &'static str)>| {
            move |name: &str| {
                variables
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            }
        };
        assert_eq!(
            environment(
                home,
                from(vec![
                    ("HOME", "/home/u"),
                    ("XDG_DATA_HOME", "/home/u/.local/share"),
                    ("XDG_CONFIG_HOME", "/home/u/.config"),
                    ("SSLKEYLOGFILE", "/home/u/tls-secrets"),
                ])
            ),
            [
                ("HOME", Some("/tmp/broxser-cdp-test/home".into())),
                ("XDG_DATA_HOME", None),
                ("XDG_CONFIG_HOME", None),
                ("XDG_CACHE_HOME", Some("/home/u/.cache".into())),
                ("XAUTHORITY", Some("/home/u/.Xauthority".into())),
                ("SSLKEYLOGFILE", None),
            ]
        );
        let explicit = environment(
            home,
            from(vec![
                ("HOME", "/home/u"),
                ("XDG_CACHE_HOME", "/var/cache/u"),
            ]),
        );
        assert_eq!(explicit[3], ("XDG_CACHE_HOME", Some("/var/cache/u".into())));
        assert_eq!(
            environment(home, from(vec![("HOME", "relative")]))[3],
            ("XDG_CACHE_HOME", None)
        );
        assert_eq!(environment(home, from(vec![]))[3], ("XDG_CACHE_HOME", None));
        assert!(certificate_note("net::ERR_CERT_AUTHORITY_INVALID").contains("CACertificates"));
        assert_eq!(certificate_note("net::ERR_CONNECTION_REFUSED"), "");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn browser_runs_in_a_private_home() {
        use crate::test_support::{FakeBrowser, fake_browser, profile_root};
        use std::io::Read;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::PermissionsExt;
        let root = profile_root();
        let browser = BrowserProcess::start(
            &BrowserOptions {
                executable: fake_browser(FakeBrowser::NeverReady),
                headless: true,
                profile_root: Some(root.path().to_owned()),
                cancel: Cancellation::new(),
            },
            true,
        )
        .unwrap();
        let home = browser.profile.as_ref().unwrap().path().join(PRIVATE_HOME);
        // Around exec, procfs may briefly show the inherited parent environment
        // or no environment. Wait for this launch's private HOME, not merely
        // any HOME substring; wrong environment propagation still fails below.
        let mut expected_home = OsString::from("HOME=");
        expected_home.push(&home);
        let deadline = Instant::now() + Duration::from_secs(5);
        let environ = loop {
            // Take each snapshot with one read: the fake browser's shell execs
            // sleep, and the reads after that exec return nothing, so a
            // multi-read file read can end after HOME without later variables.
            let mut environ = vec![0; 1 << 20];
            let length = fs::File::open(format!("/proc/{}/environ", browser.child.id()))
                .and_then(|mut file| file.read(&mut environ))
                .unwrap();
            assert!(length < environ.len(), "environment larger than one read");
            environ.truncate(length);
            if environ
                .split(|byte| *byte == 0)
                .any(|entry| entry == expected_home.as_bytes())
                || Instant::now() >= deadline
            {
                break environ;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let variable = |name: &str| {
            environ
                .split(|byte| *byte == 0)
                .filter_map(|entry| {
                    let at = entry.iter().position(|byte| *byte == b'=')?;
                    Some((&entry[..at], &entry[at + 1..]))
                })
                .find(|(key, _)| *key == name.as_bytes())
                .map(|(_, value)| std::ffi::OsStr::from_bytes(value).to_owned())
        };
        assert_eq!(variable("HOME"), Some(home.clone().into_os_string()));
        assert_eq!(
            fs::metadata(&home).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(!home.join(".Xauthority").exists());
        assert_eq!(variable("XDG_DATA_HOME"), None);
        assert_eq!(variable("XDG_CONFIG_HOME"), None);
        assert_eq!(variable("SSLKEYLOGFILE"), None);
        assert_eq!(
            variable("XDG_CACHE_HOME"),
            env::var_os("XDG_CACHE_HOME")
                .filter(|cache| Path::new(cache).is_absolute())
                .or_else(|| env::var_os("HOME")
                    .filter(|home| Path::new(home).is_absolute())
                    .map(|home| PathBuf::from(home).join(".cache").into_os_string()))
        );
        assert_eq!(
            variable("XAUTHORITY"),
            env::var_os("XAUTHORITY").or_else(|| env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".Xauthority").into_os_string()))
        );
        assert!(
            variable("BREAKPAD_DUMP_LOCATION")
                .is_some_and(|location| Path::new(&location).starts_with(root.path()))
        );
        browser.shutdown().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn browser_starts_without_core_dumps() {
        use crate::test_support::{FakeBrowser, fake_browser, profile_root};
        use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
        // Enable core dumps with the kernel's default filter first (where the
        // hard limit allows it), so the checks do not pass merely because this
        // process already has zero values.
        let limit = getrlimit(Resource::Core);
        setrlimit(
            Resource::Core,
            Rlimit {
                current: limit.maximum,
                ..limit
            },
        )
        .unwrap();
        fs::write("/proc/self/coredump_filter", "0x33").unwrap();
        let root = profile_root();
        let browser = BrowserProcess::start(
            &BrowserOptions {
                executable: fake_browser(FakeBrowser::NeverReady),
                headless: true,
                profile_root: Some(root.path().to_owned()),
                cancel: Cancellation::new(),
            },
            true,
        )
        .unwrap();
        let limits = fs::read_to_string(format!("/proc/{}/limits", browser.child.id())).unwrap();
        let core = limits
            .lines()
            .find(|line| line.starts_with("Max core file size"))
            .unwrap();
        // Columns: name (4 words), soft limit, hard limit, unit.
        assert_eq!(core.split_whitespace().nth(4), Some("0"), "{core}");
        let filter =
            fs::read_to_string(format!("/proc/{}/coredump_filter", browser.child.id())).unwrap();
        assert_eq!(filter.trim(), "00000000");
        browser.shutdown().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_identity_survives_odd_names_and_detects_exit() {
        let entry = procfs::parse_stat(
            42,
            "42 (a) b (c)) S 7 42 42 0 -1 4194560 1 0 0 0 0 0 0 0 20 0 1 0 987654 0 0",
        )
        .unwrap();
        assert_eq!(entry.parent, 7);
        assert_eq!(entry.identity.start_time, 987654);
        assert!(!entry.zombie);
        assert!(procfs::parse_stat(1, "garbage").is_none());

        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let tree = process_tree(child.id());
        assert_eq!(tree.len(), 1);
        assert!(is_running(&tree[0]));
        let reused = ProcessIdentity {
            start_time: tree[0].start_time + 1,
            ..tree[0]
        };
        assert!(!is_running(&reused), "PID reuse must not match");
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(wait_for_exit(&tree, Duration::from_secs(2)), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn launch_argument_matches_only_a_whole_argument() {
        let argument = b"--user-data-dir=/tmp/broxser-cdp-a";
        assert!(procfs::has_argument(
            b"helium\0--user-data-dir=/tmp/broxser-cdp-a\0about:blank\0",
            argument
        ));
        // Chromium's rewritten process title joins arguments with spaces.
        assert!(procfs::has_argument(
            b"helium --type=zygote --user-data-dir=/tmp/broxser-cdp-a",
            argument
        ));
        assert!(procfs::has_argument(
            b"helium --type=zygote --user-data-dir=/tmp/broxser-cdp-a\0\0\0",
            argument
        ));
        for other in [
            &b"helium\0--user-data-dir=/tmp/broxser-cdp-ab\0"[..],
            b"ls\0/tmp/broxser-cdp-a\0",
            b"x--user-data-dir=/tmp/broxser-cdp-a\0",
            b"sh\0-c\0read line; : --user-data-dir=/tmp/broxser-cdp-a\0sh\0",
        ] {
            assert!(!procfs::has_argument(other, argument), "{other:?}");
        }
    }

    /// Crash handlers are found by the profile's crash database argument; a
    /// process that only mentions the profile is never one of them.
    #[cfg(target_os = "linux")]
    #[test]
    fn crash_database_argument_matches_only_the_profiles_own() {
        let prefix = b"--database=/tmp/broxser-cdp-a/";
        assert!(procfs::has_argument_prefix(
            b"helium_crashpad_handler\0--monitor-self\0--database=/tmp/broxser-cdp-a/Crash Reports\0",
            prefix
        ));
        for other in [
            &b"helium_crashpad_handler\0--database=/tmp/broxser-cdp-ab/Crash Reports\0"[..],
            b"ls\0/tmp/broxser-cdp-a/Crash Reports\0",
            b"x\0x--database=/tmp/broxser-cdp-a/Crash Reports\0",
            b"cat\0notes--database=/tmp/broxser-cdp-a/\0",
            b"sh\0-c\0read line; : --database=/tmp/broxser-cdp-a/Crash Reports\0sh\0",
            b"helium_crashpad_handler --database=/tmp/broxser-cdp-a/Crash Reports",
        ] {
            assert!(!procfs::has_argument_prefix(other, prefix), "{other:?}");
        }
    }

    /// Observers present before the snapshot and arriving after the browser
    /// exits must block release without becoming browser ownership evidence.
    #[cfg(target_os = "linux")]
    #[test]
    fn cleanup_preserves_initial_and_late_profile_observers() {
        use crate::test_support::{FakeBrowser, HeldProcess, fake_browser, profile_root};
        for shutdown in [true, false] {
            let root = profile_root();
            let options = BrowserOptions {
                executable: fake_browser(FakeBrowser::NeverReady),
                headless: true,
                profile_root: Some(root.path().to_owned()),
                cancel: Cancellation::new(),
            };
            let browser = BrowserProcess::start(&options, true).unwrap();
            let sibling = BrowserProcess::start(&options, true).unwrap();
            let profile = browser.profile.as_ref().unwrap().path().to_owned();
            let sibling_profile = sibling.profile.as_ref().unwrap().path().to_owned();
            let initial = HeldProcess::argument(&profile);
            let embedded = HeldProcess::embedded(&format!(
                "--user-data-dir={} --database={}/Crash Reports",
                profile.display(),
                profile.display()
            ));
            let root_identity = identity(browser.child.id()).unwrap();
            let late_profile = profile.clone();
            let late = thread::spawn(move || {
                let deadline = Instant::now() + EXIT_TIMEOUT;
                while is_running(&root_identity) {
                    assert!(Instant::now() < deadline, "the browser did not stop");
                    thread::sleep(Duration::from_millis(5));
                }
                HeldProcess::argument(late_profile)
            });
            let result = if shutdown {
                browser.shutdown()
            } else {
                drop(browser);
                Ok(())
            };
            let late = late.join().unwrap();
            initial.assert_running();
            embedded.assert_running();
            late.assert_running();
            if shutdown {
                assert!(
                    result.is_err(),
                    "a live observer must report incomplete release"
                );
            }
            assert!(!profile.exists());
            assert!(sibling_profile.exists());
            assert!(sibling.processes().iter().any(is_running));
            sibling.shutdown().unwrap();
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn owned_snapshot_rejects_a_mismatched_or_reaped_browser_identity() {
        use crate::test_support::{FakeBrowser, fake_browser, profile_root};
        let root = profile_root();
        let mut browser = BrowserProcess::start(
            &BrowserOptions {
                executable: fake_browser(FakeBrowser::NeverReady),
                headless: true,
                profile_root: Some(root.path().to_owned()),
                cancel: Cancellation::new(),
            },
            true,
        )
        .unwrap();
        let recorded = browser.identity.unwrap();
        assert_eq!(browser.owned_processes(), [recorded]);
        browser.identity = Some(ProcessIdentity {
            start_time: recorded.start_time + 1,
            ..recorded
        });
        assert!(
            browser.owned_processes().is_empty(),
            "a reused PID supplied ownership"
        );
        browser.identity = Some(recorded);
        browser.child.kill().unwrap();
        browser.child.wait().unwrap();
        assert!(
            browser.owned_processes().is_empty(),
            "a reaped child supplied ownership"
        );
        browser.shutdown().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cleanup_scan_rejects_embedded_marker_text() {
        use crate::test_support::{HeldProcess, profile_root};
        let root = profile_root();
        let profile = root.path().join("private-profile");
        let embedded = HeldProcess::embedded(&format!(
            "--user-data-dir={} --database={}/Crash Reports",
            profile.display(),
            profile.display()
        ));
        assert!(referencing(&profile).contains(&embedded.identity));
        assert!(!launched_with(&profile).contains(&embedded.identity));
        assert!(!crash_handlers_of(&profile).contains(&embedded.identity));
        assert_eq!(release_or_stop(&[], &profile, Duration::from_millis(30)), 1);
        embedded.assert_running();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn terminate_signals_only_the_recorded_process() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let recorded = identity(child.id()).unwrap();
        let reused = ProcessIdentity {
            start_time: recorded.start_time + 1,
            ..recorded
        };
        assert!(!terminate(&reused).unwrap());
        assert_eq!(descendants(&reused), []);
        assert!(is_running(&recorded), "another start time was signaled");
        assert_eq!(descendants(&recorded), [recorded]);
        assert!(terminate(&recorded).unwrap());
        child.wait().unwrap();
        assert!(
            !terminate(&recorded).unwrap(),
            "a reaped process was signaled"
        );
        assert_eq!(descendants(&recorded), []);
    }
}
