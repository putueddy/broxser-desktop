//! Owner-death reproducers and guardian protocol tests. An owner-role process is
//! this test binary re-executed to play Broxser: it owns a browser through the
//! ordinary engine API and is then killed, crashes, or dies inside its own
//! teardown. The test process keeps the fixtures and another instance in the
//! same profile root, and measures what remains of the dead owner's instance.

use super::*;
use crate::browser::{BrowserOptions, BrowserProcess};
use crate::capture::{self, Plan};
use crate::live::LiveSession;
use crate::profile::{LEASE, PROFILE_PREFIX};
use crate::test_support::{
    ABORT_AT, FakeBrowser, Fixture, Reply, assert_cleaned_up, fake_browser, profile_root,
    role_command, test_browser,
};
use broxser_core::Workspace;
use rustix::process::{Pid, Signal, kill_process, kill_process_group};
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, ChildStdin, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, TryRecvError};

const SCENARIO: &str = "BROXSER_TEST_SCENARIO";
const EXECUTABLE: &str = "BROXSER_TEST_EXECUTABLE";
const ROOT: &str = "BROXSER_TEST_ROOT";
const URL: &str = "BROXSER_TEST_URL";

/// Plays Broxser: runs a capture or a live session with the configured browser,
/// prints live frame counts and follows stdin (`close` stops the browser
/// normally, `crash` aborts) until it is killed.
pub(crate) fn owner_role() -> ! {
    let root = PathBuf::from(env::var_os(ROOT).expect("owner profile root"));
    let options = BrowserOptions {
        executable: PathBuf::from(env::var_os(EXECUTABLE).expect("owner browser")),
        headless: true,
        profile_root: Some(root.clone()),
        cancel: Cancellation::new(),
    };
    let cancel = options.cancel.clone();
    let mut workspace = Workspace::demo();
    if let Ok(url) = env::var(URL) {
        workspace.url = url;
    }
    let (sender, commands) = mpsc::channel();
    thread::spawn(move || {
        for line in io::stdin().lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let mut live = None;
    let mut capture = None;
    match env::var(SCENARIO).as_deref() {
        Ok("capture") => {
            let output = root.join(format!("output-{}", process::id()));
            capture = Some(thread::spawn(move || {
                capture::run(&workspace, &options, &output, Plan::default())
                    .result
                    .is_ok()
            }));
        }
        Ok("live") => {
            live = Some(LiveSession::start(workspace, options, || {}).expect("start live session"));
        }
        scenario => panic!("unknown owner scenario {scenario:?}"),
    }
    let mut stdout = io::stdout();
    loop {
        if let Some(live) = &live {
            let frames: Vec<String> = live
                .status()
                .devices
                .iter()
                .map(|device| device.frames.to_string())
                .collect();
            let _ = writeln!(stdout, "owner: frames {}", frames.join(" "));
            let _ = stdout.flush();
        }
        match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(command) if command == "crash" => process::abort(),
            Ok(command) if command == "close" => {
                cancel.cancel();
                drop(live.take());
                if let Some(capture) = capture.take() {
                    let _ = capture.join();
                }
                let _ = writeln!(stdout, "owner: closed");
                let _ = stdout.flush();
            }
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => thread::sleep(Duration::from_millis(100)),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Death {
    Kill,
    Term,
    /// `abort()` inside the owner.
    Crash,
    /// SIGTERM to the owner's process group, as Ctrl+C in its terminal would.
    GroupTerm,
}

/// An owner-role process started by the test; killed and reaped on drop.
struct Owner {
    child: Child,
    lines: Receiver<String>,
    stdin: ChildStdin,
}

impl Owner {
    fn start(
        scenario: &str,
        executable: &Path,
        root: &Path,
        url: Option<&str>,
        abort_at: Option<&str>,
    ) -> Self {
        let mut command = role_command("owner");
        command
            .env(SCENARIO, scenario)
            .env(EXECUTABLE, executable)
            .env(ROOT, root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Its own process group, like a program started from a shell.
            .process_group(0);
        if let Some(url) = url {
            command.env(URL, url);
        }
        if let Some(point) = abort_at {
            command.env(ABORT_AT, point);
        }
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stdin = child.stdin.take().unwrap();
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            lines,
            stdin,
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn send(&mut self, command: &str) {
        writeln!(self.stdin, "{command}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// The most recent frame counts the owner reported, one per device.
    fn frames(&self) -> Vec<u64> {
        let mut latest = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let line = match self.lines.try_recv() {
                Ok(line) => line,
                Err(TryRecvError::Empty) if latest.is_some() => break,
                Err(TryRecvError::Empty) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    self.lines.recv_timeout(left).expect("owner frame report")
                }
                Err(TryRecvError::Disconnected) => panic!("the owner stopped reporting"),
            };
            if let Some(frames) = line.strip_prefix("owner: frames ") {
                latest = Some(frames.split(' ').map(|n| n.parse().unwrap()).collect());
            }
        }
        latest.unwrap()
    }

    /// Waits until live frames arrive on every device and keep arriving.
    fn streaming(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        let first = loop {
            let frames = self.frames();
            if frames.iter().all(|&count| count > 0) {
                break frames;
            }
            assert!(Instant::now() < deadline, "no frames: {frames:?}");
            thread::sleep(Duration::from_millis(100));
        };
        loop {
            let frames = self.frames();
            if frames.iter().zip(&first).all(|(now, then)| now > then) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "frames stopped: {first:?} {frames:?}"
            );
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Waits until the owner's lease names a running guardian and browser.
    fn armed(&self, root: &Path) -> Armed {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(armed) = leased_to(root, self.pid()) {
                return armed;
            }
            assert!(
                Instant::now() < deadline,
                "the owner never started a browser"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn die(&mut self, death: Death) -> ExitStatus {
        let pid = Pid::from_raw(self.pid() as i32).unwrap();
        match death {
            Death::Kill => kill_process(pid, Signal::KILL).unwrap(),
            Death::Term => kill_process(pid, Signal::TERM).unwrap(),
            Death::GroupTerm => kill_process_group(pid, Signal::TERM).unwrap(),
            Death::Crash => self.send("crash"),
        }
        self.wait()
    }

    fn wait(&mut self) -> ExitStatus {
        let status = self.child.wait().unwrap();
        assert!(!status.success(), "the owner exited normally: {status}");
        status
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// An owner's profile whose lease names a running guardian and browser.
struct Armed {
    profile: PathBuf,
    lease: Lease,
}

impl Armed {
    /// Every process of the instance: its guardian, the browser tree and the
    /// detached helpers naming the profile. All of them run when this returns.
    fn processes(&self) -> Vec<ProcessIdentity> {
        let mut processes = vec![self.lease.guardian.unwrap()];
        for process in browser::descendants(&self.lease.browser.unwrap())
            .into_iter()
            .chain(browser::referencing(&self.profile))
        {
            if !processes.contains(&process) {
                processes.push(process);
            }
        }
        assert!(
            processes.len() >= 2 && processes.iter().all(browser::is_running),
            "the instance is not running before its owner dies"
        );
        processes
    }

    fn cdp_port(&self) -> u16 {
        let endpoint = fs::read_to_string(self.profile.join("DevToolsActivePort")).unwrap();
        endpoint.lines().next().unwrap().parse().unwrap()
    }
}

fn leased_to(root: &Path, owner: u32) -> Option<Armed> {
    fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(PROFILE_PREFIX)
        })
        .find_map(|entry| {
            let lease = Lease::read(&entry.path()).ok()?;
            (lease.owner.pid == owner
                && lease.guardian.is_some_and(|g| browser::is_running(&g))
                && lease.browser.is_some_and(|b| browser::is_running(&b)))
            .then(|| Armed {
                profile: entry.path(),
                lease,
            })
        })
}

/// Waits until every process has exited, zombies included, and the profile is
/// gone. Returns the milliseconds that took after `since`.
fn assert_gone(what: &str, processes: &[ProcessIdentity], profile: &Path, since: Instant) -> u128 {
    let deadline = since + EXIT_TIMEOUT;
    loop {
        let running = processes
            .iter()
            .filter(|process| browser::is_running(process))
            .count();
        if running == 0 && fs::symlink_metadata(profile).is_err() {
            return since.elapsed().as_millis();
        }
        assert!(
            Instant::now() < deadline,
            "{what}: {running} of {} processes running, profile exists: {}, after {:?}",
            processes.len(),
            profile.exists(),
            since.elapsed()
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn options(executable: PathBuf, root: &Path) -> BrowserOptions {
    BrowserOptions {
        executable,
        headless: true,
        profile_root: Some(root.to_owned()),
        cancel: Cancellation::new(),
    }
}

/// Another running instance, owned by the test process, in the same root.
struct Bystander {
    browser: BrowserProcess,
    processes: Vec<ProcessIdentity>,
    profile: PathBuf,
}

impl Bystander {
    fn start(executable: PathBuf, root: &Path) -> Self {
        let browser = BrowserProcess::start(&options(executable, root), true).unwrap();
        let mut processes = browser.processes();
        processes.extend(browser.guardian());
        let profile = leased_to(root, process::id()).unwrap().profile;
        Self {
            browser,
            processes,
            profile,
        }
    }

    fn assert_untouched(&self) {
        assert!(
            self.processes.iter().all(browser::is_running),
            "another instance lost a process"
        );
        assert_eq!(
            Lease::read(&self.profile).unwrap().owner.pid,
            process::id(),
            "another instance lost its profile"
        );
    }

    fn close(self, root: &Path) {
        self.assert_untouched();
        self.browser.shutdown().unwrap();
        assert_cleaned_up(root, &self.processes);
    }
}

#[test]
fn owner_death_stops_only_its_browser_and_removes_its_profile() {
    let browser = fake_browser(FakeBrowser::NeverReady);
    for (scenario, death) in [
        ("capture", Death::Kill),
        ("capture", Death::Term),
        ("capture", Death::Crash),
        ("capture", Death::GroupTerm),
        ("live", Death::Kill),
        ("live", Death::Crash),
    ] {
        let root = profile_root();
        let mut owner = Owner::start(scenario, &browser, root.path(), None, None);
        let armed = owner.armed(root.path());
        let bystander = Bystander::start(browser.clone(), root.path());
        let processes = armed.processes();
        let died = Instant::now();
        let status = owner.die(death);
        let elapsed = assert_gone(scenario, &processes, &armed.profile, died);
        println!("{scenario} startup, owner {death:?} ({status}): cleaned up after {elapsed} ms");
        bystander.close(root.path());
    }
}

/// Kills the guardian, then the owner, as a cgroup kill or power loss would take
/// both, so neither cleans up. The browser survives as an orphan.
fn kill_owner_and_guardian(owner: &mut Owner, armed: &Armed) {
    let guardian = armed.lease.guardian.unwrap();
    assert!(browser::terminate(&guardian).unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    while browser::is_running(&guardian) {
        assert!(Instant::now() < deadline, "the guardian survived SIGKILL");
        thread::sleep(Duration::from_millis(5));
    }
    owner.die(Death::Kill);
}

#[test]
fn next_start_recovers_the_profile_of_a_killed_process_tree() {
    let browser = fake_browser(FakeBrowser::NeverReady);
    let root = profile_root();
    let mut owner = Owner::start("capture", &browser, root.path(), None, None);
    let armed = owner.armed(root.path());
    let processes = armed.processes();
    kill_owner_and_guardian(&mut owner, &armed);
    assert!(browser::terminate(&armed.lease.browser.unwrap()).unwrap());
    assert_eq!(browser::wait_for_exit(&processes, EXIT_TIMEOUT), 0);
    assert!(
        armed.profile.join(LEASE).is_file(),
        "something cleaned up after the whole tree was killed"
    );
    let next = BrowserProcess::start(&options(browser.clone(), root.path()), true).unwrap();
    assert!(
        !armed.profile.exists(),
        "the next start kept a stale profile"
    );
    let mut next_processes = next.processes();
    next_processes.extend(next.guardian());
    next.shutdown().unwrap();
    assert_cleaned_up(root.path(), &next_processes);
}

/// Outlasts the late writer of [`FakeBrowser::LateHelper`] and checks that the
/// removed profile was not written again.
fn assert_stays_removed(profile: &Path) {
    thread::sleep(Duration::from_millis(600));
    assert!(
        fs::symlink_metadata(profile).is_err(),
        "the profile was written again after its removal: {:?}",
        fs::read_dir(profile).map(|entries| entries
            .filter_map(|entry| Some(entry.ok()?.file_name()))
            .collect::<Vec<_>>())
    );
}

/// Waits until the helper of [`FakeBrowser::LateHelper`] runs. The fake browser
/// names the profile itself until it execs, so only the helper's own name counts.
fn wait_for_helper(profile: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !browser::referencing(profile).iter().any(|process| {
        fs::read(format!("/proc/{}/cmdline", process.pid)).is_ok_and(|cmdline| {
            cmdline
                .split(|&byte| byte == 0)
                .any(|arg| arg == b"late-helper")
        })
    }) {
        assert!(
            Instant::now() < deadline,
            "the fake browser's helper never started"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn shutdown_waits_for_helpers_started_after_its_snapshot() {
    let root = profile_root();
    let browser = BrowserProcess::start(
        &options(fake_browser(FakeBrowser::LateHelper), root.path()),
        true,
    )
    .unwrap();
    let profile = leased_to(root.path(), process::id()).unwrap().profile;
    wait_for_helper(&profile);
    let mut processes = browser.processes();
    processes.extend(browser.guardian());
    browser.shutdown().unwrap();
    assert_stays_removed(&profile);
    assert_cleaned_up(root.path(), &processes);
}

#[test]
fn guardian_waits_for_helpers_started_after_its_owner_died() {
    let root = profile_root();
    let browser = fake_browser(FakeBrowser::LateHelper);
    let mut owner = Owner::start("capture", &browser, root.path(), None, None);
    let armed = owner.armed(root.path());
    wait_for_helper(&armed.profile);
    let processes = armed.processes();
    let died = Instant::now();
    owner.die(Death::Kill);
    let elapsed = assert_gone("late helper", &processes, &armed.profile, died);
    println!("owner SIGKILL with a late helper: cleaned up after {elapsed} ms");
    assert_stays_removed(&armed.profile);
    assert_cleaned_up(root.path(), &processes);
}

#[test]
fn owner_crash_during_teardown_is_finished_by_its_guardian() {
    let browser = fake_browser(FakeBrowser::NeverReady);
    for scenario in ["capture", "live"] {
        for point in ["before-kill", "before-remove"] {
            let root = profile_root();
            let mut owner = Owner::start(scenario, &browser, root.path(), None, Some(point));
            let armed = owner.armed(root.path());
            let processes = armed.processes();
            let closing = Instant::now();
            owner.send("close");
            let status = owner.wait();
            assert_eq!(status.signal(), Some(Signal::ABORT.as_raw()), "{status}");
            let elapsed = assert_gone(point, &processes, &armed.profile, closing);
            println!("{scenario} teardown, abort {point}: cleaned up after {elapsed} ms");
            assert_cleaned_up(root.path(), &processes);
        }
    }
}

/// A profile leased to the test process, its guardian and a stand-in process.
struct Guarded {
    _root: tempfile::TempDir,
    profile: PathBuf,
    guardian: Option<Guardian>,
    stand_in: Child,
}

impl Guarded {
    fn start(stand_in: impl FnOnce(&Path) -> Command) -> Self {
        let root = profile_root();
        let profile = root.path().join(format!("{PROFILE_PREFIX}guarded"));
        fs::create_dir(&profile).unwrap();
        fs::write(profile.join("Local State"), "{}").unwrap();
        let lease = Lease::for_current_process().unwrap();
        lease.write(&profile).unwrap();
        let guardian = Guardian::start(&profile, &lease.owner, &Cancellation::new()).unwrap();
        let stand_in = stand_in(&profile).spawn().unwrap();
        Self {
            _root: root,
            profile,
            guardian: Some(guardian),
            stand_in,
        }
    }

    fn stand_in(&self) -> ProcessIdentity {
        browser::identity(self.stand_in.id()).unwrap()
    }

    fn watch(&mut self, browser: ProcessIdentity) {
        self.guardian.as_mut().unwrap().watch(&browser).unwrap();
    }

    /// Closes the pipe as a dying owner would; returns the guardian's status.
    fn abandon(&mut self) -> ExitStatus {
        let mut guardian = self.guardian.take().unwrap().abandon();
        guardian.wait().unwrap()
    }
}

impl Drop for Guarded {
    fn drop(&mut self) {
        let _ = self.stand_in.kill();
        let _ = self.stand_in.wait();
        if let Some(guardian) = self.guardian.take() {
            guardian.release();
        }
    }
}

fn sleeper(_profile: &Path) -> Command {
    let mut command = Command::new("sleep");
    command.arg("60");
    command
}

/// Blocks in a shell builtin, so it has no child processes, and carries the
/// browser's `--user-data-dir` argument.
fn unreported_browser(profile: &Path) -> Command {
    let mut argument = OsString::from("--user-data-dir=");
    argument.push(profile);
    let mut command = Command::new("sh");
    command
        .args(["-c", "read line", "sh"])
        .arg(argument)
        .stdin(Stdio::piped());
    command
}

#[test]
fn guardian_release_leaves_the_owners_processes_and_profile() {
    let mut guarded = Guarded::start(sleeper);
    let stand_in = guarded.stand_in();
    guarded.watch(stand_in);
    let guardian = guarded.guardian.as_ref().unwrap().identity();
    guarded.guardian.take().unwrap().release();
    assert!(
        !browser::is_running(&guardian),
        "a released guardian keeps running"
    );
    assert!(browser::is_running(&stand_in));
    assert!(guarded.profile.join(LEASE).is_file());
}

#[test]
fn guardian_stops_the_reported_browser_when_its_owner_disappears() {
    let mut guarded = Guarded::start(sleeper);
    let stand_in = guarded.stand_in();
    guarded.watch(stand_in);
    let started = Instant::now();
    let status = guarded.abandon();
    assert!(status.success(), "{status}");
    assert!(!browser::is_running(&stand_in));
    assert!(!guarded.profile.exists());
    println!(
        "guardian cleanup after EOF: {} ms",
        started.elapsed().as_millis()
    );
}

#[test]
fn guardian_never_signals_a_reused_pid() {
    let mut guarded = Guarded::start(sleeper);
    let stand_in = guarded.stand_in();
    // The reported PID now belongs to a process that started at another time.
    guarded.watch(ProcessIdentity {
        start_time: stand_in.start_time + 1,
        ..stand_in
    });
    let status = guarded.abandon();
    assert!(status.success(), "{status}");
    assert!(browser::is_running(&stand_in), "a reused PID was signaled");
    assert!(!guarded.profile.exists());
}

#[test]
fn guardian_stops_a_browser_started_on_its_profile_before_it_was_reported() {
    let mut guarded = Guarded::start(unreported_browser);
    let stand_in = guarded.stand_in();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !browser::launched_with(&guarded.profile).contains(&stand_in) {
        assert!(Instant::now() < deadline, "the stand-in never started");
        thread::sleep(Duration::from_millis(5));
    }
    let status = guarded.abandon();
    assert!(status.success(), "{status}");
    assert!(!browser::is_running(&stand_in));
    assert!(!guarded.profile.exists());
}

#[test]
fn guardian_keeps_a_profile_leased_to_another_owner() {
    let mut guarded = Guarded::start(sleeper);
    let stand_in = guarded.stand_in();
    guarded.watch(stand_in);
    // The directory is no longer the one this guardian was started for.
    let mut lease = Lease::read(&guarded.profile).unwrap();
    lease.owner.start_time += 1;
    lease.write(&guarded.profile).unwrap();
    let status = guarded.abandon();
    assert_eq!(status.code(), Some(1), "{status}");
    assert!(
        !browser::is_running(&stand_in),
        "the reported browser keeps running"
    );
    assert!(guarded.profile.join(LEASE).is_file());
}

#[test]
fn guardian_refuses_a_profile_it_does_not_own() {
    let root = profile_root();
    let profile = root.path().join(format!("{PROFILE_PREFIX}foreign"));
    fs::create_dir(&profile).unwrap();
    let mut lease = Lease::for_current_process().unwrap();
    let owner = lease.owner;
    lease.owner.start_time += 1;
    lease.write(&profile).unwrap();
    let error = Guardian::start(&profile, &owner, &Cancellation::new())
        .err()
        .unwrap();
    assert!(
        format!("{error:#}").contains("refused to start")
            && format!("{error:#}").contains("another owner"),
        "{error:#}"
    );
    assert!(profile.join(LEASE).is_file());
}

#[test]
fn guardian_commands_are_parsed_strictly() {
    assert!(matches!(parse(b"release\n"), Some(Message::Release)));
    assert!(matches!(
        parse(b"browser 42 987654\n"),
        Some(Message::Browser(ProcessIdentity {
            pid: 42,
            start_time: 987654
        }))
    ));
    for rejected in [
        &b"release"[..],
        b"browser 42 987654",
        b"browser 0 1\n",
        b"browser 4294967295 1\n",
        b"browser -1 1\n",
        b"browser 42\n",
        b"browser 42 1 2\n",
        b"\xff\n",
    ] {
        assert!(
            parse(rejected).is_none(),
            "{:?}",
            String::from_utf8_lossy(rejected)
        );
    }
}

fn ticking_fixture() -> Fixture {
    Fixture::start(|request, _| match request.path.as_str() {
        "/hang" => Reply::Hang,
        _ => Reply::Html {
            body: "<!doctype html><title>tick</title><div id=t>0</div>\
                   <script>let n=0;setInterval(()=>t.textContent=++n,100)</script>"
                .into(),
            delay: Duration::ZERO,
            cookie: None,
        },
    })
}

fn cdp_closed(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).is_err()
}

// Live tests: BROXSER_TEST_BROWSER=/path/to/helium cargo test -p broxser-engine -- --ignored

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_owner_death_during_a_held_capture_request() {
    let fixture = ticking_fixture();
    for death in [Death::Kill, Death::Term, Death::Crash] {
        let root = profile_root();
        let requests = fixture.requests().len();
        let abandoned = fixture.abandoned();
        let mut owner = Owner::start(
            "capture",
            &test_browser(),
            root.path(),
            Some(&fixture.url("/hang")),
            None,
        );
        assert!(
            fixture.wait_for(Duration::from_secs(30), |f| f.requests().len() > requests),
            "the held request never arrived"
        );
        let armed = owner.armed(root.path());
        let port = armed.cdp_port();
        let bystander = Bystander::start(test_browser(), root.path());
        let processes = armed.processes();
        let died = Instant::now();
        let status = owner.die(death);
        let elapsed = assert_gone("held request", &processes, &armed.profile, died);
        assert!(cdp_closed(port), "the dead owner's CDP endpoint is open");
        println!(
            "held capture request, owner {death:?} ({status}): {} processes, CDP and profile gone after {elapsed} ms",
            processes.len()
        );
        assert!(
            fixture.wait_for(Duration::from_secs(5), |f| f.abandoned() > abandoned),
            "the held request was not closed"
        );
        assert_eq!(
            fixture.requests().len(),
            requests + 1,
            "a request was replayed"
        );
        bystander.close(root.path());
    }
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_owner_death_while_frames_stream() {
    let fixture = ticking_fixture();
    let documents = |fixture: &Fixture| fixture.requests().iter().filter(|r| r.path == "/").count();
    for death in [Death::Kill, Death::Term, Death::Crash, Death::GroupTerm] {
        let root = profile_root();
        let before = documents(&fixture);
        let mut owner = Owner::start(
            "live",
            &test_browser(),
            root.path(),
            Some(&fixture.url("/")),
            None,
        );
        owner.streaming();
        let armed = owner.armed(root.path());
        let port = armed.cdp_port();
        let mut workspace = Workspace::demo();
        workspace.url = fixture.url("/");
        let bystander =
            LiveSession::start(workspace, options(test_browser(), root.path()), || {}).unwrap();
        let bystander_frames = |live: &LiveSession| live.status().devices[0].frames;
        let deadline = Instant::now() + Duration::from_secs(30);
        while bystander_frames(&bystander) == 0 {
            assert!(
                Instant::now() < deadline,
                "the other instance never streamed"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let processes = armed.processes();
        let died = Instant::now();
        let status = owner.die(death);
        let elapsed = assert_gone("live frames", &processes, &armed.profile, died);
        assert!(cdp_closed(port), "the dead owner's CDP endpoint is open");
        println!(
            "live frames, owner {death:?} ({status}): {} processes, CDP and profile gone after {elapsed} ms",
            processes.len()
        );
        // The other instance keeps streaming, and nothing was loaded again.
        let frames = bystander_frames(&bystander);
        let deadline = Instant::now() + Duration::from_secs(10);
        while bystander_frames(&bystander) < frames + 3 {
            assert!(
                Instant::now() < deadline,
                "the other instance stopped streaming"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            documents(&fixture),
            before + 6,
            "one load per device and instance"
        );
        drop(bystander);
        assert!(
            fs::read_dir(root.path()).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(PROFILE_PREFIX)),
            "profiles left in the root"
        );
    }
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_owner_crash_during_live_teardown() {
    let fixture = ticking_fixture();
    for point in ["before-kill", "before-remove"] {
        let root = profile_root();
        let mut owner = Owner::start(
            "live",
            &test_browser(),
            root.path(),
            Some(&fixture.url("/")),
            Some(point),
        );
        owner.streaming();
        let armed = owner.armed(root.path());
        let port = armed.cdp_port();
        let processes = armed.processes();
        let closing = Instant::now();
        owner.send("close");
        let status = owner.wait();
        assert_eq!(status.signal(), Some(Signal::ABORT.as_raw()), "{status}");
        let elapsed = assert_gone(point, &processes, &armed.profile, closing);
        assert!(cdp_closed(port), "the dead owner's CDP endpoint is open");
        println!(
            "live teardown, abort {point}: {} processes, CDP and profile gone after {elapsed} ms",
            processes.len()
        );
        assert_cleaned_up(root.path(), &processes);
    }
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_next_start_stops_an_orphaned_browser_and_recovers_its_profile() {
    let fixture = ticking_fixture();
    let root = profile_root();
    let mut owner = Owner::start(
        "live",
        &test_browser(),
        root.path(),
        Some(&fixture.url("/")),
        None,
    );
    owner.streaming();
    let armed = owner.armed(root.path());
    let port = armed.cdp_port();
    let processes = armed.processes();
    kill_owner_and_guardian(&mut owner, &armed);
    // The orphan keeps its CDP endpoint and profile: nothing is left to stop it.
    thread::sleep(Duration::from_millis(500));
    let orphaned: Vec<_> = processes
        .iter()
        .filter(|process| browser::is_running(process))
        .copied()
        .collect();
    assert!(orphaned.len() > 1, "the browser did not outlive its owner");
    assert!(!cdp_closed(port) && armed.profile.join(LEASE).is_file());
    let recovering = Instant::now();
    let next = BrowserProcess::start(&options(test_browser(), root.path()), true).unwrap();
    let elapsed = assert_gone("orphan", &orphaned, &armed.profile, recovering);
    assert!(cdp_closed(port), "the orphan's CDP endpoint is open");
    println!(
        "next start: {} orphaned processes, CDP and profile gone after {elapsed} ms",
        orphaned.len()
    );
    let mut next_processes = next.processes();
    next_processes.extend(next.guardian());
    next.shutdown().unwrap();
    assert_cleaned_up(root.path(), &next_processes);
}
