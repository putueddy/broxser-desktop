use super::*;
use crate::browser::{self, ProcessIdentity};
use crate::test_support::{Fixture, Reply, profile_root, test_browser};
use broxser_core::{Device, Session};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn keys_map_to_dom_values_and_text() {
    let none = Modifiers::default();
    let enter = KeyInput::from_key("enter", None, none, true).unwrap();
    assert_eq!((enter.key.as_str(), enter.key_code), ("Enter", 13));
    assert_eq!(enter.text.as_deref(), Some("\r"));
    let left = KeyInput::from_key("left", None, none, true).unwrap();
    assert_eq!(
        (left.key.as_str(), left.code.as_str(), left.text),
        ("ArrowLeft", "ArrowLeft", None)
    );
    let shifted = KeyInput::from_key(
        "a",
        Some("A"),
        Modifiers {
            shift: true,
            ..none
        },
        true,
    )
    .unwrap();
    assert_eq!(
        (
            shifted.key.as_str(),
            shifted.code.as_str(),
            shifted.key_code
        ),
        ("A", "KeyA", 65)
    );
    assert_eq!(shifted.text.as_deref(), Some("A"));
    let digit = KeyInput::from_key("7", Some("7"), none, false).unwrap();
    assert_eq!(
        (digit.code.as_str(), digit.key_code, digit.down),
        ("Digit7", 55, false)
    );
    let control = KeyInput::from_key(
        "c",
        None,
        Modifiers {
            control: true,
            ..none
        },
        true,
    )
    .unwrap();
    assert!(control.text.is_none(), "shortcuts must not insert text");
    assert!(KeyInput::from_key("f5", None, none, true).is_none());
    assert!(KeyInput::from_key("\u{7}", None, none, true).is_none());
}

#[test]
fn modifiers_use_cdp_bits() {
    let all = Modifiers {
        alt: true,
        control: true,
        meta: true,
        shift: true,
    };
    assert_eq!(all.cdp(), 15);
    assert_eq!(
        Modifiers {
            shift: true,
            ..Modifiers::default()
        }
        .cdp(),
        8
    );
}

#[test]
fn viewport_mapping_handles_scale_offset_and_bounds() {
    // A 390x844 CSS viewport shown at half size, offset inside the window.
    let image = (100.0, 50.0, 195.0, 422.0);
    assert_eq!(
        to_viewport((100.0, 50.0), image, (390.0, 844.0)),
        Some((0.0, 0.0))
    );
    assert_eq!(
        to_viewport((197.5, 261.0), image, (390.0, 844.0)),
        Some((195.0, 422.0))
    );
    // The device scale factor does not change CSS coordinates.
    let doubled = (0.0, 0.0, 780.0, 1688.0);
    assert_eq!(
        to_viewport((390.0, 844.0), doubled, (390.0, 844.0)),
        Some((195.0, 422.0))
    );
    assert_eq!(to_viewport((99.9, 60.0), image, (390.0, 844.0)), None);
    assert_eq!(to_viewport((295.0, 60.0), image, (390.0, 844.0)), None);
    assert_eq!(
        to_viewport((120.0, 60.0), (0.0, 0.0, 0.0, 10.0), (390.0, 844.0)),
        None
    );
}

// Live tests: BROXSER_TEST_BROWSER=/path/to/helium cargo test -p broxser-engine -- --ignored

const PAGE: &str = r#"<!doctype html><html><head><meta name=viewport content="width=device-width,initial-scale=1">
<style>html,body{margin:0}body{height:4000px;background:linear-gradient(#fff,#bbb);font:16px sans-serif}
#tick{position:fixed;top:0;right:0;padding:4px;background:#000;color:#fff}
#link{position:absolute;left:20px;top:100px;width:160px;height:40px;background:#08f;color:#fff}
#field{position:absolute;left:20px;top:200px;width:160px;height:30px}</style></head><body>
<div id=tick>0</div><a id=link href="/next">next</a><input id=field>
<script>
const report = (kind, data) => fetch('/event?' + new URLSearchParams({kind, w: innerWidth, page: location.pathname, ...data}));
let n = 0; setInterval(() => { document.getElementById('tick').textContent = ++n; }, 100);
addEventListener('mousedown', e => report('down', {x: e.clientX, y: e.clientY, dpr: devicePixelRatio}), true);
let last = 0; addEventListener('scroll', () => { const y = Math.round(scrollY); if (y !== last) { last = y; report('scroll', {y}); } });
document.getElementById('field').addEventListener('input', e => report('input', {v: e.target.value}));
AUTO
</script></body></html>"#;

fn fixture() -> Fixture {
    Fixture::start(|request, _| {
        let path = request.path.split('?').next().unwrap_or("/");
        let body = match path {
            "/event" => String::new(),
            // A page that follows its own link without any user gesture.
            "/auto" => PAGE.replace(
                "AUTO",
                "setTimeout(() => { const a = document.getElementById('link'); a.href = '/after-auto'; a.click(); }, 300);",
            ),
            _ => PAGE.replace("AUTO", ""),
        };
        Reply::Html {
            body,
            delay: Duration::ZERO,
            cookie: None,
        }
    })
}

/// Phone (DPR2) and tablet share `guest`; desktop is alone in `admin`.
fn workspace(url: String) -> Workspace {
    let device = |id: &str, width, height, scale, session: &str| Device {
        id: id.into(),
        name: id.into(),
        width,
        height,
        device_scale_factor: scale,
        mobile: false,
        touch: false,
        session: session.into(),
    };
    Workspace {
        schema_version: broxser_core::SCHEMA_VERSION,
        name: "Live test".into(),
        url,
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
            device("phone", 360, 640, 2.0, "guest"),
            device("tablet", 600, 800, 1.0, "guest"),
            device("desktop", 1000, 700, 1.0, "admin"),
        ],
    }
}

struct Live {
    session: Option<LiveSession>,
    root: tempfile::TempDir,
    notified: Arc<AtomicUsize>,
}

impl Live {
    fn start(workspace: Workspace) -> Self {
        let root = profile_root();
        let notified = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&notified);
        let session = LiveSession::start(
            workspace,
            BrowserOptions {
                executable: test_browser(),
                headless: true,
                profile_root: Some(root.path().to_owned()),
                cancel: Cancellation::new(),
            },
            move || {
                counter.fetch_add(1, Ordering::SeqCst);
            },
        )
        .unwrap();
        Self {
            session: Some(session),
            root,
            notified,
        }
    }

    fn session(&self) -> &LiveSession {
        self.session.as_ref().unwrap()
    }

    fn send(&self, command: Command) {
        assert!(
            self.session().send(command),
            "command queue rejected a command"
        );
    }

    fn wait(&self, what: &str, timeout: Duration, condition: impl Fn(&Status) -> bool) -> Status {
        let deadline = Instant::now() + timeout;
        loop {
            let status = self.session().status();
            if condition(&status) {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {status:#?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Stops the session and proves that the browser and profile are gone.
    fn close(mut self) {
        let processes = browser::referencing(self.root.path());
        assert!(!processes.is_empty());
        drop(self.session.take());
        assert_gone(self.root.path(), &processes);
    }
}

/// Kernel crash handling and the state of each browser process. Before Broxser
/// disabled core dumps, a renderer on the CI runner stayed in the kernel's core
/// dump path (state `I`, wait channel `do_exit`) for more than 45 seconds.
fn crash_diagnostics(root: &Path) -> String {
    let read = |path: &str| {
        std::fs::read(path)
            .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned())
            .unwrap_or_default()
    };
    let mut text = format!(
        "core_pattern={:?} suid_dumpable={}\n",
        read("/proc/sys/kernel/core_pattern"),
        read("/proc/sys/fs/suid_dumpable")
    );
    for process in browser::referencing(root) {
        let file = |name: &str| read(&format!("/proc/{}/{name}", process.pid));
        let stat = file("stat");
        let state = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.trim().chars().next())
            .unwrap_or('?');
        let limits = file("limits");
        let core = limits
            .lines()
            .find(|line| line.starts_with("Max core file size"))
            .and_then(|line| line.split_whitespace().nth(4))
            .unwrap_or("?");
        // Chromium rewrites its process title and joins arguments with spaces.
        let cmdline = file("cmdline");
        let kind = cmdline
            .split(['\0', ' '])
            .find(|arg| arg.starts_with("--type="))
            .unwrap_or("browser");
        text += &format!(
            "pid={} state={state} wchan={} core_limit={core} {kind}\n",
            process.pid,
            file("wchan")
        );
    }
    text
}

fn assert_gone(root: &Path, processes: &[ProcessIdentity]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while processes.iter().any(browser::is_running) {
        assert!(Instant::now() < deadline, "browser processes still running");
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(browser::referencing(root), []);
    let profiles = fs_entries(root);
    assert!(profiles.is_empty(), "profiles left: {profiles:?}");
}

fn fs_entries(root: &Path) -> Vec<String> {
    std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

fn running(status: &Status) -> bool {
    matches!(status.runtime, RuntimeState::Running { .. })
}

fn loaded(status: &Status, fixture: &Fixture, path: &str) -> bool {
    running(status)
        && status
            .devices
            .iter()
            .all(|device| device.url == fixture.url(path) && !device.loading && device.frames > 0)
}

fn count(fixture: &Fixture, path: &str) -> usize {
    fixture
        .requests()
        .iter()
        .filter(|request| request.path == path)
        .count()
}

/// Page reports sent to `/event`, as query maps.
fn events(fixture: &Fixture, kind: &str) -> Vec<HashMap<String, String>> {
    fixture
        .requests()
        .iter()
        .filter_map(|request| request.path.strip_prefix("/event?"))
        .map(|query| {
            query
                .split('&')
                .filter_map(|pair| pair.split_once('='))
                .map(|(key, value)| (key.to_owned(), value.replace("%2F", "/")))
                .collect::<HashMap<_, _>>()
        })
        .filter(|event| event.get("kind").map(String::as_str) == Some(kind))
        .collect()
}

fn click(live: &Live, device: usize, x: f64, y: f64) {
    for (kind, buttons) in [(PointerKind::Down, 1), (PointerKind::Up, 0)] {
        live.send(Command::Pointer {
            device,
            event: PointerEvent {
                kind,
                x,
                y,
                button: PointerButton::Left,
                buttons,
                click_count: 1,
                modifiers: Modifiers::default(),
            },
        });
    }
}

fn jpeg_size(data: &[u8]) -> Option<(u32, u32)> {
    if !data.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut at = 2;
    while at + 9 < data.len() {
        if data[at] != 0xFF {
            at += 1;
            continue;
        }
        let marker = data[at + 1];
        if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            let height = u16::from_be_bytes([data[at + 5], data[at + 6]]);
            let width = u16::from_be_bytes([data[at + 7], data[at + 8]]);
            return Some((width.into(), height.into()));
        }
        at += 2 + usize::from(u16::from_be_bytes([data[at + 2], data[at + 3]]));
    }
    None
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_session_streams_frames_and_cleans_up() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/")));
    let status = live.wait(
        "first page on every device",
        Duration::from_secs(30),
        |status| loaded(status, &fixture, "/"),
    );
    assert!(
        status
            .devices
            .iter()
            .all(|device| device.streaming && device.error.is_none())
    );
    // The page ticks every 100 ms, so frames keep arriving without any capture action.
    let before: Vec<u64> = status.devices.iter().map(|device| device.frames).collect();
    let status = live.wait(
        "page updates as new frames",
        Duration::from_secs(10),
        |status| {
            status
                .devices
                .iter()
                .zip(&before)
                .all(|(device, before)| device.frames >= before + 3)
        },
    );
    // Headless screencasts deliver emulated DPR>1 viewports at CSS resolution;
    // only static capture keeps physical pixels (the phone PNG is 720x1280).
    for (index, size) in [(360, 640), (600, 800), (1000, 700)]
        .into_iter()
        .enumerate()
    {
        let frame = live.session().take_frame(index).expect("latest frame");
        assert_eq!(
            (frame.css_width, frame.css_height),
            (f64::from(size.0), f64::from(size.1))
        );
        assert_eq!(jpeg_size(&frame.jpeg), Some(size), "device {index}");
        assert!(frame.sequence <= status.devices[index].frames + 1);
    }
    assert_eq!(
        count(&fixture, "/"),
        3,
        "one document request per device, no replay"
    );
    assert!(live.notified.load(Ordering::SeqCst) > 0);
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_session_input_reaches_the_right_device() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/")));
    live.wait("pages", Duration::from_secs(30), |status| {
        loaded(status, &fixture, "/")
    });
    // Away from the link so the clicks do not navigate.
    click(&live, 0, 40.0, 300.0);
    click(&live, 2, 50.0, 315.0);
    assert!(fixture.wait_for(
        Duration::from_secs(5),
        |fixture| events(fixture, "down").len() == 2
    ));
    let downs = events(&fixture, "down");
    let phone = downs
        .iter()
        .find(|event| event["w"] == "360")
        .expect("phone click");
    assert_eq!(
        (
            phone["x"].as_str(),
            phone["y"].as_str(),
            phone["dpr"].as_str()
        ),
        ("40", "300", "2")
    );
    let desktop = downs
        .iter()
        .find(|event| event["w"] == "1000")
        .expect("desktop click");
    assert_eq!(
        (
            desktop["x"].as_str(),
            desktop["y"].as_str(),
            desktop["dpr"].as_str()
        ),
        ("50", "315", "1")
    );

    live.send(Command::Wheel {
        device: 1,
        x: 300.0,
        y: 400.0,
        delta_x: 0.0,
        delta_y: 500.0,
    });
    assert!(fixture.wait_for(Duration::from_secs(5), |fixture| {
        !events(fixture, "scroll").is_empty()
    }));
    thread::sleep(Duration::from_millis(500));
    let scrolls = events(&fixture, "scroll");
    assert!(
        scrolls.iter().all(|event| event["w"] == "600"),
        "only the tablet scrolls: {scrolls:?}"
    );
    assert!(
        scrolls
            .iter()
            .any(|event| event["y"].parse::<u32>().unwrap() > 0)
    );

    click(&live, 2, 30.0, 210.0);
    for name in ["a", "b"] {
        for down in [true, false] {
            let key = KeyInput::from_key(name, Some(name), Modifiers::default(), down).unwrap();
            live.send(Command::Key { device: 2, key });
        }
    }
    assert!(
        fixture.wait_for(Duration::from_secs(5), |fixture| events(fixture, "input")
            .iter()
            .any(|event| event["v"] == "ab" && event["w"] == "1000")),
        "typed text did not reach the desktop input"
    );
    assert_eq!(count(&fixture, "/"), 3, "input must not navigate");
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_session_sync_stays_in_session_without_loops_or_replay() {
    let fixture = fixture();
    let url = fixture.url("/");
    let live = Live::start(workspace(url.clone()));
    live.wait("pages", Duration::from_secs(30), |status| {
        loaded(status, &fixture, "/")
    });
    live.send(Command::SetSync(SyncSettings {
        navigation: true,
        scroll: true,
    }));
    live.wait("sync on", Duration::from_secs(5), |status| {
        status.sync.navigation
    });

    // A real link click on the phone follows to the tablet (same session) only.
    click(&live, 0, 100.0, 120.0);
    let next = fixture.url("/next");
    live.wait(
        "tablet follows the phone",
        Duration::from_secs(10),
        |status| {
            status.devices[0].url == next
                && status.devices[1].url == next
                && !status.devices[1].loading
        },
    );
    thread::sleep(Duration::from_millis(1500));
    let status = live.session().status();
    assert_eq!(status.devices[2].url, url, "sync must not cross sessions");
    assert_eq!(count(&fixture, "/next"), 2, "one navigation each, no loop");

    live.send(Command::Wheel {
        device: 0,
        x: 100.0,
        y: 300.0,
        delta_x: 0.0,
        delta_y: 400.0,
    });
    assert!(fixture.wait_for(Duration::from_secs(5), |fixture| {
        let scrolls = events(fixture, "scroll");
        ["360", "600"]
            .iter()
            .all(|width| scrolls.iter().any(|event| event["w"] == *width))
    }));
    thread::sleep(Duration::from_millis(500));
    assert!(
        events(&fixture, "scroll")
            .iter()
            .all(|event| event["w"] != "1000")
    );

    // Script-driven link clicks have no user gesture and never synchronize.
    live.send(Command::NavigateAll {
        url: fixture.url("/auto"),
    });
    let after = fixture.url("/after-auto");
    live.wait(
        "pages follow their own script",
        Duration::from_secs(10),
        |status| {
            status
                .devices
                .iter()
                .all(|device| device.url == after && !device.loading)
        },
    );
    thread::sleep(Duration::from_millis(1500));
    assert_eq!(count(&fixture, "/auto"), 3);
    assert_eq!(count(&fixture, "/after-auto"), 3, "no synchronized replays");
    live.close();

    // Restarting restores configuration, not earlier clicks or navigations.
    let restarted = Live::start(workspace(url));
    restarted.wait("restart", Duration::from_secs(30), |status| {
        loaded(status, &fixture, "/")
    });
    thread::sleep(Duration::from_millis(1000));
    assert_eq!(count(&fixture, "/"), 6);
    assert_eq!(count(&fixture, "/next"), 2);
    assert_eq!(count(&fixture, "/after-auto"), 3);
    restarted.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_session_bounds_frames_and_pauses_hidden_devices() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/")));
    live.wait("pages", Duration::from_secs(30), |status| {
        loaded(status, &fixture, "/")
    });

    // Frames the UI does not take are replaced, never queued, and still acknowledged.
    let _ = live.session().take_frame(1);
    let start = live.session().status().devices[1].clone();
    thread::sleep(Duration::from_millis(1500));
    let status = live.wait("frames keep flowing", Duration::from_secs(5), |status| {
        status.devices[1].frames >= start.frames + 5
            && status.devices[1].dropped_frames > start.dropped_frames
    });
    let latest = live.session().take_frame(1).expect("one pending frame");
    assert!(latest.sequence >= status.devices[1].frames - 1);
    assert!(
        live.session().take_frame(1).is_none(),
        "at most one pending frame"
    );

    live.send(Command::SetVisible {
        device: 0,
        visible: false,
    });
    let hidden = live.wait("phone paused", Duration::from_secs(5), |status| {
        !status.devices[0].streaming
    });
    thread::sleep(Duration::from_millis(800));
    let paused = live.session().status().devices[0].frames;
    assert!(
        paused <= hidden.devices[0].frames + 2,
        "in-flight frames only"
    );
    thread::sleep(Duration::from_millis(800));
    assert_eq!(
        live.session().status().devices[0].frames,
        paused,
        "hidden devices stop streaming"
    );
    assert!(live.session().take_frame(0).is_none());
    live.send(Command::SetVisible {
        device: 0,
        visible: true,
    });
    live.wait("phone resumes", Duration::from_secs(5), |status| {
        status.devices[0].frames > paused + 2
    });
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_session_reports_crashes_and_browser_exit() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/")));
    live.wait("pages", Duration::from_secs(30), |status| {
        loaded(status, &fixture, "/")
    });

    live.send(Command::CrashForTest { device: 1 });
    let crashed_at = Instant::now();
    let status = loop {
        let status = live.session().status();
        if status.devices[1]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("crashed"))
        {
            break status;
        }
        if crashed_at.elapsed() > Duration::from_secs(15) {
            panic!(
                "renderer crash not reported within 15 s: {status:#?}\n{}",
                crash_diagnostics(live.root.path())
            );
        }
        thread::sleep(Duration::from_millis(20));
    };
    println!(
        "renderer crash reported after {} ms",
        crashed_at.elapsed().as_millis()
    );
    assert!(status.devices[0].error.is_none() && status.devices[2].error.is_none());
    live.send(Command::Reload { device: 1 });
    let frames = status.devices[1].frames;
    live.wait(
        "explicit reload recovers",
        Duration::from_secs(15),
        |status| status.devices[1].error.is_none() && status.devices[1].frames > frames + 2,
    );

    // Kill the browser from outside: the runtime stops with an error and cleans up.
    let processes = browser::referencing(live.root.path());
    let main = processes
        .iter()
        .find(|process| {
            std::fs::read(format!("/proc/{}/cmdline", process.pid)).is_ok_and(|cmdline| {
                let text = String::from_utf8_lossy(&cmdline);
                !text.contains("--type=") && !text.contains("crashpad")
            })
        })
        .expect("main browser process");
    let killed = std::process::Command::new("kill")
        .args(["-KILL", &main.pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let status = live.wait("runtime stops", Duration::from_secs(10), |status| {
        matches!(status.runtime, RuntimeState::Stopped { error: Some(_) })
    });
    assert!(
        status
            .devices
            .iter()
            .all(|device| !device.streaming && !device.loading)
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !live.session().is_finished() {
        assert!(Instant::now() < deadline, "worker did not exit");
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !live.session().send(Command::Reload { device: 0 }),
        "stopped runtime accepts nothing"
    );
    let Live { session, root, .. } = live;
    drop(session);
    assert_gone(root.path(), &processes);
}
