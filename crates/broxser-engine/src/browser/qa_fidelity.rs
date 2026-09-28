//! Discovery runs through normal ownership, with no page work or CLI probe.

use super::*;
use crate::cdp::Cancelled;
use crate::test_support::{HeldProcess, assert_cleaned_up, profile_root};
use serde_json::Value;
use std::io::ErrorKind;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use tungstenite::Message;

const NATIVE: &str = "Mozilla/5.0 (X11; Linux aarch64) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/154.0.8037.57 Safari/537.36";

fn options(root: &Path) -> BrowserOptions {
    BrowserOptions {
        executable: Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/fake-browser/qa-fidelity"),
        headless: true,
        profile_root: Some(root.to_owned()),
        cancel: Cancellation::new(),
    }
}

fn limits() -> Limits {
    Limits {
        startup: Duration::from_secs(2),
        command: Duration::from_secs(2),
        ..Limits::default()
    }
}

fn launches(root: &Path) -> Vec<Value> {
    fs::read_to_string(root.join("qa-launches.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[derive(Clone)]
enum Reply {
    Version(Value),
    VersionAfterBarrier(Value, Arc<AtomicBool>),
    Error,
    Hold,
}

struct Discovery {
    profile: PathBuf,
    processes: Vec<ProcessIdentity>,
}

struct Peer {
    stop: Arc<AtomicBool>,
    commands: Arc<Mutex<Vec<(usize, String)>>>,
    discovered: mpsc::Receiver<Discovery>,
    server: Option<thread::JoinHandle<()>>,
}

impl Peer {
    fn new(root: &Path, reply: Reply) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        fs::write(
            root.join("fake-cdp-port"),
            listener.local_addr().unwrap().port().to_string(),
        )
        .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let commands = Arc::new(Mutex::new(Vec::new()));
        let (observed, discovered) = mpsc::channel();
        let root = root.to_owned();
        let stopped = Arc::clone(&stop);
        let recorded = Arc::clone(&commands);
        let server = thread::spawn(move || {
            let mut connection = 0;
            while !stopped.load(Ordering::SeqCst) {
                let (stream, _) = match listener.accept() {
                    Ok(stream) => stream,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("accept: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_millis(100)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut socket = tungstenite::accept(stream).unwrap();
                connection += 1;
                while !stopped.load(Ordering::SeqCst) {
                    let message = match socket.read() {
                        Ok(Message::Text(message)) => message,
                        Ok(_) => continue,
                        Err(tungstenite::Error::Io(error))
                            if matches!(
                                error.kind(),
                                ErrorKind::TimedOut | ErrorKind::WouldBlock
                            ) =>
                        {
                            continue;
                        }
                        Err(_) => break,
                    };
                    let request: Value = serde_json::from_str(message.as_str()).unwrap();
                    let method = request["method"].as_str().unwrap();
                    recorded
                        .lock()
                        .unwrap()
                        .push((connection, method.to_owned()));
                    assert_eq!(
                        method, "Browser.getVersion",
                        "workspace work during discovery"
                    );
                    assert_eq!(connection, 1, "replacement recursively performed discovery");
                    assert_eq!(request["params"], json!({}));
                    assert!(request.get("sessionId").is_none());
                    let launch = launches(&root).pop().unwrap();
                    let profile = PathBuf::from(launch["profile"].as_str().unwrap());
                    let lease: Value = serde_json::from_slice(
                        &fs::read(profile.join("broxser-lease.json")).unwrap(),
                    )
                    .unwrap();
                    let processes = ["browser", "guardian"]
                        .map(|name| {
                            serde_json::from_value::<ProcessIdentity>(lease[name].clone()).unwrap()
                        })
                        .to_vec();
                    fs::write(
                        root.join("previous-owner.json"),
                        json!({"profile": profile, "processes": processes}).to_string(),
                    )
                    .unwrap();
                    observed.send(Discovery { profile, processes }).unwrap();
                    let response = match &reply {
                        Reply::Version(version) => json!({"id": request["id"], "result": version}),
                        Reply::VersionAfterBarrier(version, barrier) => {
                            while !barrier.load(Ordering::SeqCst) {
                                if stopped.load(Ordering::SeqCst) {
                                    return;
                                }
                                thread::sleep(Duration::from_millis(5));
                            }
                            json!({"id": request["id"], "result": version})
                        }
                        Reply::Error => {
                            json!({"id": request["id"], "error": {"code": -32000, "message": "discovery rejected"}})
                        }
                        Reply::Hold => continue,
                    };
                    if socket
                        .send(Message::Text(response.to_string().into()))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        });
        Self {
            stop,
            commands,
            discovered,
            server: Some(server),
        }
    }

    fn discovery(&self) -> Discovery {
        self.discovered
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.server.take().unwrap().join().unwrap();
    }
}

#[test]
fn normalizer_preserves_the_native_platform_and_full_version() {
    assert_eq!(
        headed_user_agent(Some(NATIVE)),
        Some(NATIVE.replace("HeadlessChrome/", "Chrome/"))
    );
    let alternate =
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64)  HeadlessChrome/141.0.7390.37 Extra/1";
    assert_eq!(
        headed_user_agent(Some(alternate)),
        Some(alternate.replace("HeadlessChrome/", "Chrome/"))
    );
    assert_eq!(
        headed_user_agent(Some("aHeadlessChrome/154.0.0.0 HeadlessChrome/154.0.0.0")),
        Some("aHeadlessChrome/154.0.0.0 Chrome/154.0.0.0".to_owned())
    );
    for invalid in [
        "",
        "Chrome/154.0.0.0",
        "aHeadlessChrome/154.0.0.0",
        "HeadlessChrome/154",
        "HeadlessChrome/154..0.0",
        "HeadlessChrome/154.0.0.x",
        "HeadlessChrome/154.0.0.0;",
        "HeadlessChrome/154.0.0.0 HeadlessChrome/141.0.0.0",
        "HeadlessChrome/154.0.0.0\r\nInjected: value",
        "HeadlessChrome/154.0.0.0\0",
        "HeadlessChrome/154.0.0.0 λ",
    ] {
        assert_eq!(headed_user_agent(Some(invalid)), None, "{invalid:?}");
    }
    assert_eq!(headed_user_agent(None), None);
    assert_eq!(
        headed_user_agent(Some(&format!(
            "{} HeadlessChrome/154.0.0.0",
            "x".repeat(4096)
        ))),
        None
    );
}

#[test]
fn native_discovery_replaces_once_after_the_first_runtime_is_gone() {
    let root = profile_root();
    let peer = Peer::new(root.path(), Reply::Version(json!({"userAgent": NATIVE})));
    let started = Instant::now();
    let (browser, cdp) =
        BrowserProcess::start_connected(&options(root.path()), true, &limits()).unwrap();
    // The fixture's --version path blocks for 60 s. A CLI probe would delay this
    // by at least the former 3 s timeout, even though the real launch succeeds.
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "an executable version probe delayed startup"
    );
    let first = peer.discovery();
    let records = launches(root.path());
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["previous_profile_present"], false);
    assert_eq!(records[1]["previous_profile_present"], false);
    assert_eq!(records[1]["previous_running"], json!([]));
    assert_ne!(records[0]["profile"], records[1]["profile"]);
    assert!(!first.profile.exists());
    assert!(first.processes.iter().all(|identity| !is_running(identity)));
    let first_args = records[0]["args"].as_array().unwrap();
    assert!(
        !first_args
            .iter()
            .any(|arg| arg.as_str().unwrap().starts_with("--user-agent="))
    );
    let second_args = records[1]["args"].as_array().unwrap();
    assert!(second_args.contains(&json!(format!(
        "--user-agent={}",
        NATIVE.replace("HeadlessChrome/", "Chrome/")
    ))));
    assert_eq!(
        *peer.commands.lock().unwrap(),
        [(1, "Browser.getVersion".to_owned())]
    );
    let processes = startup_processes(&browser);
    drop(cdp);
    browser.shutdown().unwrap();
    assert_cleaned_up(root.path(), &processes);
}

#[test]
fn absent_invalid_or_ordinary_user_agent_keeps_the_first_runtime() {
    for version in [
        json!({}),
        json!({"userAgent": 42}),
        json!({"userAgent": "Chrome/154.0.0.0"}),
        json!({"userAgent": "HeadlessChrome/bad"}),
    ] {
        let root = profile_root();
        let peer = Peer::new(root.path(), Reply::Version(version));
        let (browser, cdp) =
            BrowserProcess::start_connected(&options(root.path()), true, &limits()).unwrap();
        let first = peer.discovery();
        assert_eq!(launches(root.path()).len(), 1);
        assert_eq!(browser.profile.as_ref().unwrap().path(), first.profile);
        assert_eq!(
            *peer.commands.lock().unwrap(),
            [(1, "Browser.getVersion".to_owned())]
        );
        drop(cdp);
        browser.shutdown().unwrap();
        assert_cleaned_up(root.path(), &first.processes);
    }
}

#[test]
fn discovery_error_or_timeout_cleans_up_without_replacement() {
    for reply in [Reply::Error, Reply::Hold] {
        let root = profile_root();
        let peer = Peer::new(root.path(), reply.clone());
        let mut bounds = limits();
        if matches!(reply, Reply::Hold) {
            bounds.command = Duration::from_millis(100);
        }
        let result = BrowserProcess::start_connected(&options(root.path()), true, &bounds);
        let error = result.err().expect("discovery must fail");
        let expected = match reply {
            Reply::Error => "discovery rejected",
            _ => "timed out during browser discovery",
        };
        assert!(format!("{error:#}").contains(expected), "{error:#}");
        let first = peer.discovery();
        assert_eq!(launches(root.path()).len(), 1);
        assert_cleaned_up(root.path(), &first.processes);
        assert_eq!(
            error.downcast_ref::<StartupDiagnostics>().unwrap().0,
            first.processes
        );
    }
}

#[test]
fn discovery_cancellation_is_typed_and_never_launches_replacement() {
    let root = profile_root();
    let peer = Peer::new(root.path(), Reply::Hold);
    let options = options(root.path());
    let cancel = options.cancel.clone();
    let started = thread::spawn(move || BrowserProcess::start_connected(&options, true, &limits()));
    let first = peer.discovery();
    let cancelled = Instant::now();
    cancel.cancel();
    let error = started.join().unwrap().err().unwrap();
    assert!(error.is::<Cancelled>(), "{error:#}");
    assert!(cancelled.elapsed() < Duration::from_secs(1));
    assert_eq!(launches(root.path()).len(), 1);
    assert_cleaned_up(root.path(), &first.processes);
}

#[test]
fn cleanup_failure_preserves_observers_and_prevents_replacement() {
    let root = profile_root();
    let barrier = Arc::new(AtomicBool::new(false));
    let peer = Peer::new(
        root.path(),
        Reply::VersionAfterBarrier(json!({"userAgent": NATIVE}), Arc::clone(&barrier)),
    );
    let options = options(root.path());
    let started = thread::spawn(move || {
        let mut bounds = limits();
        bounds.command = Duration::from_secs(2);
        BrowserProcess::start_connected(&options, true, &bounds)
    });
    let first = peer.discovery();
    // A wait-only profile observer makes shutdown fail. That failure must not
    // start a second browser, and the observer must never be signaled.
    let observer = HeldProcess::argument(&first.profile);
    barrier.store(true, Ordering::SeqCst);
    let error = started.join().unwrap().err().unwrap();
    assert!(
        format!("{error:#}").contains("discovery browser cleanup failed"),
        "{error:#}"
    );
    observer.assert_running();
    assert_eq!(launches(root.path()).len(), 1);
    drop(observer);
    assert_cleaned_up(root.path(), &first.processes);
}

#[test]
fn cancellation_during_successful_cleanup_prevents_replacement() {
    let root = profile_root();
    let barrier = Arc::new(AtomicBool::new(false));
    let peer = Peer::new(
        root.path(),
        Reply::VersionAfterBarrier(json!({"userAgent": NATIVE}), Arc::clone(&barrier)),
    );
    let options = options(root.path());
    let cancel = options.cancel.clone();
    let started = thread::spawn(move || {
        let mut bounds = limits();
        bounds.command = Duration::from_secs(2);
        BrowserProcess::start_connected(&options, true, &bounds)
    });
    let first = peer.discovery();
    let observer = HeldProcess::argument(&first.profile);
    barrier.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(2);
    while is_running(&first.processes[0]) {
        assert!(Instant::now() < deadline, "discovery did not reach cleanup");
        thread::sleep(Duration::from_millis(5));
    }
    cancel.cancel();
    drop(observer);
    let error = started.join().unwrap().err().unwrap();
    assert!(error.is::<Cancelled>(), "{error:#}");
    assert_eq!(launches(root.path()).len(), 1);
    assert_cleaned_up(root.path(), &first.processes);
}

#[test]
fn already_cancelled_start_does_not_spawn_or_create_a_profile() {
    let root = profile_root();
    let options = options(root.path());
    options.cancel.cancel();
    let error = BrowserProcess::start_connected(&options, true, &limits())
        .err()
        .unwrap();
    assert!(error.is::<Cancelled>());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn discovery_owner() {
    let Some(root) = std::env::var_os("BROXSER_TEST_DISCOVERY_OWNER") else {
        return;
    };
    let mut bounds = limits();
    bounds.command = Duration::from_secs(60);
    let _ = BrowserProcess::start_connected(&options(Path::new(&root)), true, &bounds);
    panic!("the parent should stop the discovery owner");
}

#[test]
fn guardian_cleans_discovery_when_its_owner_dies() {
    let root = profile_root();
    let peer = Peer::new(root.path(), Reply::Hold);
    let mut owner = Command::new("/proc/self/exe")
        .args([
            "browser::qa_fidelity::discovery_owner",
            "--exact",
            "--nocapture",
            "--quiet",
            "--test-threads=1",
        ])
        .env("BROXSER_TEST_DISCOVERY_OWNER", root.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let first = peer.discovery();
    owner.kill().unwrap();
    owner.wait().unwrap();
    assert_cleaned_up(root.path(), &first.processes);
    assert_eq!(launches(root.path()).len(), 1);
}
