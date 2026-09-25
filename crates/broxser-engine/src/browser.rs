//! Owned browser subprocess: private temporary profile, loopback CDP endpoint and
//! cleanup that waits for every browser process before deleting the profile. If
//! the Broxser process itself dies, the profile's guardian does both (ADR 0007).

use crate::Limits;
use crate::cdp::{Cancellation, Cdp};
use crate::profile::OwnedProfile;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::env;
use std::ffi::OsString;
use std::fs;
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
    /// Removed after the browser stops. `None` once `shutdown` has handled it.
    profile: Option<OwnedProfile>,
    cancel: Cancellation,
}

impl BrowserProcess {
    /// Launches the browser. `seed` writes Broxser's profile preferences first;
    /// only the reproducer's baseline mode launches an unseeded profile. The
    /// profile's guardian is ready before the browser starts.
    pub(crate) fn start(options: &BrowserOptions, seed: bool) -> Result<Self> {
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
        let child = Command::new(&options.executable)
            .args(launch_args(profile.path(), options.headless))
            // Chromium keeps its crash database under the default user data
            // directory (for Helium ~/.config/net.imput.helium), shared with a
            // personal installation. Keep crash data in the private profile.
            .env(
                "BREAKPAD_DUMP_LOCATION",
                profile.path().join("Crash Reports"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("launch browser {}", options.executable.display()))?;
        let identity = self::identity(child.id());
        // From here on, dropping `browser` stops the child before the profile goes.
        let mut browser = Self {
            child,
            profile: Some(profile),
            cancel: options.cancel.clone(),
        };
        let identity = identity.ok_or_else(|| anyhow!("read the browser's identity from /proc"))?;
        if let Some(profile) = &mut browser.profile {
            profile.watch(identity)?;
        }
        Ok(browser)
    }

    /// The browser, its descendants and any detached helper whose command line
    /// names the private profile (Chromium's crash handler).
    pub(crate) fn processes(&self) -> Vec<ProcessIdentity> {
        let mut processes = process_tree(self.child.id());
        if let Some(profile) = &self.profile {
            for process in referencing(profile.path()) {
                if !processes.contains(&process) {
                    processes.push(process);
                }
            }
        }
        processes
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
        let processes = self.processes();
        let _ = self.child.kill();
        self.child.wait().context("wait for browser exit")?;
        let survivors = wait_for_exit(&processes, EXIT_TIMEOUT);
        #[cfg(test)]
        crate::test_support::abort_point("before-remove");
        let removal = self.profile.take().map_or(Ok(()), OwnedProfile::close);
        if survivors > 0 {
            bail!("{survivors} browser processes did not exit after the browser was stopped");
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
        let mut processes = process_tree(self.child.id());
        processes.extend(referencing(profile.path()));
        let _ = self.child.kill();
        let _ = self.child.wait();
        wait_for_exit(&processes, EXIT_TIMEOUT);
        // Removes the profile, then releases the guardian.
        drop(profile);
    }
}

fn launch_args(profile: &Path, headless: bool) -> Vec<OsString> {
    let mut user_data_dir = OsString::from("--user-data-dir=");
    user_data_dir.push(profile);
    let mut args: Vec<OsString> = vec![
        "--remote-debugging-address=127.0.0.1".into(),
        "--remote-debugging-port=0".into(),
        user_data_dir,
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "about:blank".into(),
    ];
    if headless {
        args.push("--headless=new".into());
    }
    args
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
    /// rewrite its title and join arguments with spaces, so a space ends one too.
    pub(crate) fn with_argument(argument: &[u8]) -> Vec<ProcessIdentity> {
        matching(|cmdline| has_argument(cmdline, argument))
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

    pub(super) fn has_argument(cmdline: &[u8], argument: &[u8]) -> bool {
        let boundary = |byte: Option<&u8>| matches!(byte, None | Some(0 | b' '));
        !argument.is_empty()
            && cmdline
                .windows(argument.len())
                .enumerate()
                .any(|(at, window)| {
                    window == argument
                        && (at == 0 || boundary(cmdline.get(at - 1)))
                        && boundary(cmdline.get(at + argument.len()))
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
        let args: Vec<String> = launch_args(profile, true)
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();
        assert!(args.contains(&"--remote-debugging-address=127.0.0.1".to_owned()));
        assert!(args.contains(&"--user-data-dir=/tmp/broxser-cdp-test".to_owned()));
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
        for other in [
            &b"helium\0--user-data-dir=/tmp/broxser-cdp-ab\0"[..],
            b"ls\0/tmp/broxser-cdp-a\0",
            b"x--user-data-dir=/tmp/broxser-cdp-a\0",
        ] {
            assert!(!procfs::has_argument(other, argument), "{other:?}");
        }
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
