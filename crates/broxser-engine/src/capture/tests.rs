use super::*;
use crate::browser::BrowserOptions;
use crate::cdp::{Cancellation, Cancelled, POLL_INTERVAL};
use crate::test_support::{
    FakeBrowser, FakeCdp, Fixture, Reply, assert_cleaned_up, fake_browser, fake_cdp, profile_root,
    test_browser,
};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;

fn options(executable: PathBuf, root: &Path) -> BrowserOptions {
    BrowserOptions {
        executable,
        headless: true,
        profile_root: Some(root.to_owned()),
        cancel: Cancellation::new(),
    }
}

fn fast_limits() -> Limits {
    Limits {
        startup: Duration::from_secs(1),
        command: Duration::from_secs(2),
        load: Duration::from_secs(2),
        ..Limits::default()
    }
}

fn plan(limits: Limits) -> Plan {
    Plan {
        limits,
        ..Plan::default()
    }
}

fn error_text(outcome: &Outcome) -> String {
    format!("{:#}", outcome.result.as_ref().unwrap_err())
}

#[test]
fn failed_browser_start_is_bounded() {
    let root = profile_root();
    let output = tempfile::tempdir().unwrap();
    let outcome = run(
        &Workspace::demo(),
        &options("/bin/false".into(), root.path()),
        output.path(),
        plan(fast_limits()),
    );
    assert!(error_text(&outcome).contains("exited before CDP"));
    assert!(fs::read_dir(root.path()).unwrap().next().is_none());
}

#[test]
fn startup_without_endpoint_times_out_and_cleans_up() {
    let root = profile_root();
    let output = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let outcome = run(
        &Workspace::demo(),
        &options(fake_browser(FakeBrowser::NeverReady), root.path()),
        output.path(),
        plan(fast_limits()),
    );
    assert!(
        error_text(&outcome).contains("did not publish a CDP endpoint"),
        "{}",
        error_text(&outcome)
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_cleaned_up(root.path(), &outcome.diagnostics.processes);
}

#[test]
fn cancellation_during_startup_is_prompt_and_cleans_up() {
    let root = profile_root();
    let output = tempfile::tempdir().unwrap();
    let options = options(fake_browser(FakeBrowser::NeverReady), root.path());
    let cancel = options.cancel.clone();
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        cancel.cancel();
    });
    let started = Instant::now();
    let outcome = run(
        &Workspace::demo(),
        &options,
        output.path(),
        plan(Limits {
            startup: Duration::from_secs(30),
            ..fast_limits()
        }),
    );
    canceller.join().unwrap();
    assert!(outcome.result.as_ref().unwrap_err().is::<Cancelled>());
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_cleaned_up(root.path(), &outcome.diagnostics.processes);
}

#[test]
fn websocket_handshake_may_outlast_the_poll_interval() {
    // Regression guard for the fixed native QA issue: the HTTP upgrade uses the
    // command deadline; only the established websocket polls every 500 ms.
    let root = profile_root();
    let server = fake_cdp(
        root.path(),
        POLL_INTERVAL * 2 + Duration::from_millis(200),
        FakeCdp::VersionOnly,
    );
    let options = options(fake_browser(FakeBrowser::LoopbackEndpoint), root.path());
    let limits = Limits {
        command: Duration::from_secs(5),
        ..fast_limits()
    };
    let mut browser = BrowserProcess::start(&options, true).unwrap();
    let mut cdp = browser.connect(&limits).unwrap();
    let id = cdp.send("Browser.getVersion", json!({}), None).unwrap();
    let deadline = Instant::now() + limits.command;
    let response = loop {
        if let Some(response) = cdp.take_response(id) {
            break response;
        }
        assert!(
            cdp.read_until(deadline).unwrap(),
            "no response before deadline"
        );
    };
    assert_eq!(
        parse_response(response, "Browser.getVersion").unwrap()["product"],
        "Fake/1.0"
    );
    let processes = browser.processes();
    drop(cdp);
    browser.shutdown().unwrap();
    server.join().unwrap();
    assert_cleaned_up(root.path(), &processes);
}

#[test]
fn stalled_websocket_upgrade_cancels_and_cleans_up() {
    let root = profile_root();
    let output = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    fs::write(
        root.path().join("fake-cdp-port"),
        listener.local_addr().unwrap().port().to_string(),
    )
    .unwrap();
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        accepted_tx.send(()).unwrap();
        thread::sleep(Duration::from_secs(2));
        drop(stream);
    });
    let options = options(fake_browser(FakeBrowser::LoopbackEndpoint), root.path());
    let cancel = options.cancel.clone();
    let worker = thread::spawn(move || {
        run(
            &Workspace::demo(),
            &options,
            output.path(),
            plan(Limits {
                command: Duration::from_secs(15),
                ..fast_limits()
            }),
        )
    });
    accepted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let cancelled_at = Instant::now();
    cancel.cancel();
    let outcome = worker.join().unwrap();
    assert!(
        outcome.result.as_ref().unwrap_err().is::<Cancelled>(),
        "{}",
        error_text(&outcome)
    );
    // Includes browser-process teardown and profile removal after the CDP
    // transport has returned its typed cancellation.
    assert!(
        cancelled_at.elapsed() < Duration::from_secs(1),
        "cancellation and cleanup took {:?}",
        cancelled_at.elapsed()
    );
    assert_cleaned_up(root.path(), &outcome.diagnostics.processes);
    server.join().unwrap();
}

#[test]
fn early_extension_event_rejects_capture_before_navigation() {
    for method in ["Target.targetCreated", "Target.targetInfoChanged"] {
        let root = profile_root();
        let output = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        fs::write(
            root.path().join("fake-cdp-port"),
            listener.local_addr().unwrap().port().to_string(),
        )
        .unwrap();
        let navigations = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&navigations);
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            while let Ok(tungstenite::Message::Text(text)) = socket.read() {
                let request: Value = serde_json::from_str(text.as_str()).unwrap();
                let command = request["method"].as_str().unwrap();
                if command == "Page.navigate" {
                    count.fetch_add(1, Ordering::SeqCst);
                }
                if command == "Target.createBrowserContext" {
                    for event in [
                        json!({"method": method, "params": {"targetInfo": {
                            "type":"background_page", "targetId":"EXT1", "browserContextId":"CTX1",
                            "url":"chrome-extension://blockjmkbacgjkknlgpkjjiijinjdanf/background.html"
                        }}}),
                        json!({"method":"Target.targetDestroyed", "params":{"targetId":"EXT1"}}),
                    ] {
                        socket
                            .send(tungstenite::Message::Text(event.to_string().into()))
                            .unwrap();
                    }
                }
                let result = match command {
                    "Browser.getVersion" => json!({"product":"Fake/1.0", "protocolVersion":"1.3"}),
                    "Target.createBrowserContext" => json!({"browserContextId":"CTX1"}),
                    _ => json!({}),
                };
                let reply = json!({"id":request["id"],"result":result});
                if socket
                    .send(tungstenite::Message::Text(reply.to_string().into()))
                    .is_err()
                {
                    break;
                }
            }
        });
        let outcome = run(
            &Workspace::demo(),
            &options(fake_browser(FakeBrowser::LoopbackEndpoint), root.path()),
            output.path(),
            plan(fast_limits()),
        );
        assert!(
            error_text(&outcome).contains("browser runtime not qualified"),
            "{method}: {}",
            error_text(&outcome)
        );
        assert_eq!(navigations.load(Ordering::SeqCst), 0, "{method}");
        server.join().unwrap();
        assert_cleaned_up(root.path(), &outcome.diagnostics.processes);
    }
}

#[test]
fn unanswered_command_times_out_and_cleans_up() {
    let root = profile_root();
    let output = tempfile::tempdir().unwrap();
    let server = fake_cdp(root.path(), Duration::ZERO, FakeCdp::VersionOnly);
    let started = Instant::now();
    let outcome = run(
        &Workspace::demo(),
        &options(fake_browser(FakeBrowser::LoopbackEndpoint), root.path()),
        output.path(),
        plan(Limits {
            command: Duration::from_secs(1),
            ..fast_limits()
        }),
    );
    let error = error_text(&outcome);
    assert!(
        error.contains("Target.setDiscoverTargets response timed out"),
        "{error}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    server.join().unwrap();
    assert_cleaned_up(root.path(), &outcome.diagnostics.processes);
}

#[test]
fn closed_websocket_fails_fast_and_cleans_up() {
    let root = profile_root();
    let output = tempfile::tempdir().unwrap();
    let server = fake_cdp(root.path(), Duration::ZERO, FakeCdp::CloseOnFirstCommand);
    let started = Instant::now();
    let outcome = run(
        &Workspace::demo(),
        &options(fake_browser(FakeBrowser::LoopbackEndpoint), root.path()),
        output.path(),
        plan(Limits {
            command: Duration::from_secs(10),
            ..fast_limits()
        }),
    );
    let error = error_text(&outcome);
    assert!(
        error.contains("CDP websocket closed") || error.contains("read CDP websocket"),
        "{error}"
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    server.join().unwrap();
    assert_cleaned_up(root.path(), &outcome.diagnostics.processes);
}

#[test]
fn foreign_navigation_detection_ignores_own_and_same_document() {
    let target = Target {
        target_id: "main".into(),
        session: "s".into(),
        navigation: None,
        started: vec![
            StartedNavigation {
                loader: "ours".into(),
                kind: "differentDocument".into(),
                page_reason: None,
            },
            StartedNavigation {
                loader: "spa".into(),
                kind: "sameDocument".into(),
                page_reason: Some("scriptInitiated".into()),
            },
        ],
        pending_reason: None,
        gone: false,
    };
    assert!(foreign(&target, Some("ours")).is_none());
    assert!(
        foreign(&target, None).is_none(),
        "unknown loader explains nothing"
    );
    let mut target = target;
    target.started.push(StartedNavigation {
        loader: "blocker".into(),
        kind: "reload".into(),
        page_reason: None,
    });
    let found = foreign(&target, Some("ours")).unwrap();
    assert_eq!(
        describe(found),
        "a browser-initiated reload navigation that Broxser did not request"
    );
    target.started.push(StartedNavigation {
        loader: "page".into(),
        kind: "differentDocument".into(),
        page_reason: Some("metaTagRefresh".into()),
    });
    assert_eq!(
        describe(foreign(&target, Some("ours")).unwrap()),
        "a page-initiated differentDocument navigation (metaTagRefresh)"
    );
}

fn html(color: &str) -> String {
    format!(
        "<!doctype html><html><head><meta name=viewport content='width=device-width, initial-scale=1'></head><body style='margin:0;background:{color};min-height:100vh'></body></html>"
    )
}

/// Three 300x300 devices (DPR2, DPR1, DPR1); the first two share a session.
fn small_workspace(url: String) -> Workspace {
    let mut workspace = Workspace::demo();
    workspace.url = url;
    for device in &mut workspace.devices {
        device.width = 300;
        device.height = 300;
        device.device_scale_factor = 1.0;
        device.mobile = false;
        device.touch = false;
    }
    workspace.devices[0].device_scale_factor = 2.0;
    workspace
}

// Live tests: BROXSER_TEST_BROWSER=/path/to/helium (or an explicit Chromium for
// comparison) cargo test -p broxser-engine -- --ignored --nocapture

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_capture_has_expected_pixels_and_isolated_sessions() {
    let next_cookie = AtomicUsize::new(1);
    let fixture = Fixture::start(move |request, _| {
        let cookie = request
            .session_cookie
            .as_deref()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or_else(|| next_cookie.fetch_add(1, Ordering::SeqCst));
        let color = if cookie % 2 == 1 {
            "#ff0000"
        } else {
            "#00ff00"
        };
        Reply::Html {
            body: html(color),
            delay: Duration::ZERO,
            cookie: Some(cookie.to_string()),
        }
    });
    let workspace = small_workspace(fixture.url("/"));
    let root = profile_root();
    let output = tempfile::tempdir().unwrap();
    let outcome = run(
        &workspace,
        &options(test_browser(), root.path()),
        output.path(),
        Plan::default(),
    );
    let report = outcome.result.as_ref().unwrap();
    assert!(!report.browser_product.is_empty());
    assert!(!report.protocol_version.is_empty());
    assert_eq!(report.frames.len(), 3);
    let colors: Vec<_> = report
        .frames
        .iter()
        .zip(&workspace.devices)
        .map(|(frame, device)| {
            let png = fs::read(&frame.path).unwrap();
            let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
            decoder.set_transformations(png::Transformations::EXPAND);
            let mut reader = decoder.read_info().unwrap();
            let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
            let info = reader.next_frame(&mut pixels).unwrap();
            let expected = (300.0 * device.device_scale_factor) as u32;
            assert_eq!((info.width, info.height), (expected, expected));
            assert_eq!((frame.width, frame.height), (expected, expected));
            pixels[..3].to_vec()
        })
        .collect();
    assert_eq!(
        colors[0], colors[1],
        "devices sharing a session should see the same cookie"
    );
    assert_ne!(
        colors[0], colors[2],
        "separate sessions should have isolated cookies"
    );
    let contexts = &outcome.diagnostics.contexts;
    assert_eq!(contexts[0].1, contexts[1].1, "same session, same context");
    assert_ne!(
        contexts[0].1, contexts[2].1,
        "different session, new context"
    );
    assert!(outcome.diagnostics.extension_targets.is_empty());
    assert_cleaned_up(root.path(), &outcome.diagnostics.processes);
}

fn document_requests(fixture: &Fixture) -> usize {
    fixture
        .requests()
        .iter()
        .filter(|request| request.path == "/")
        .count()
}

fn env_number(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn executable_digest(path: &Path) -> String {
    std::process::Command::new("sha256sum")
        .arg(path)
        .output()
        .ok()
        .and_then(|output| {
            String::from_utf8(output.stdout)
                .ok()?
                .split_whitespace()
                .next()
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unavailable".into())
}

/// M0 reproducer for the slow-page `net::ERR_ABORTED` report: every document
/// response is delayed, three targets (two in one session, DPR2 and DPR1) load
/// sequentially and in parallel. Evidence is printed without URLs or content.
///
/// BROXSER_REPRO_ITERATIONS (default 3) and BROXSER_REPRO_DELAY_MS (default 2000)
/// tune the run. BROXSER_REPRO_BASELINE=1 disables the extension guard to
/// reproduce the foundation behavior; it reports instead of asserting.
#[test]
#[ignore = "requires an installed CDP browser"]
fn live_slow_page_reproducer() {
    let iterations = env_number("BROXSER_REPRO_ITERATIONS", 3);
    let delay = Duration::from_millis(env_number("BROXSER_REPRO_DELAY_MS", 2000));
    let baseline = std::env::var_os("BROXSER_REPRO_BASELINE").is_some();
    let executable = test_browser();
    let fixture = Fixture::start(move |_, _| {
        Reply::Html {
        body: "<!doctype html><title>slow</title><body style='margin:0;background:#2a6'>slow fixture without scripts</body>".into(),
        delay,
        cookie: None,
    }
    });
    let mut workspace = Workspace::demo();
    workspace.url = fixture.url("/");
    workspace.devices[0].device_scale_factor = 2.0;
    println!(
        "reproducer executable={} sha256={} delay_ms={} guard={}",
        executable.display(),
        executable_digest(&executable),
        delay.as_millis(),
        if baseline { "off (baseline)" } else { "on" }
    );
    let mut failures = 0;
    let mut foreign_navigations = 0;
    let mut extension_runs = 0;
    let mut extra_documents = 0;
    for order in [Order::Sequential, Order::Parallel] {
        for iteration in 1..=iterations {
            let root = profile_root();
            let output = tempfile::tempdir().unwrap();
            let documents_before = document_requests(&fixture);
            let outcome = run(
                &workspace,
                &options(executable.clone(), root.path()),
                output.path(),
                Plan {
                    order,
                    extension_guard: !baseline,
                    ..Plan::default()
                },
            );
            let documents = document_requests(&fixture) - documents_before;
            let diagnostics = &outcome.diagnostics;
            println!(
                "order={order:?} run={iteration} result={} documents={documents} extensions_in_session_contexts=[{}]",
                match &outcome.result {
                    Ok(report) => format!(
                        "ok product={} protocol={}",
                        report.browser_product, report.protocol_version
                    ),
                    Err(error) => format!("error: {error:#}"),
                },
                diagnostics
                    .extension_targets
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(",")
            );
            for navigation in &diagnostics.navigations {
                println!(
                    "  device={} loader={} elapsed_ms={} error={} foreign=[{}]",
                    navigation.device,
                    navigation.loader.as_deref().unwrap_or("-"),
                    navigation.elapsed.as_millis(),
                    navigation.error.as_deref().unwrap_or("-"),
                    navigation
                        .foreign
                        .iter()
                        .map(|foreign| format!(
                            "{} {} {}",
                            foreign.kind,
                            foreign.loader,
                            foreign
                                .page_reason
                                .as_deref()
                                .unwrap_or("browser-initiated")
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                );
                foreign_navigations += navigation.foreign.len();
            }
            failures += usize::from(outcome.result.is_err());
            extension_runs += usize::from(!diagnostics.extension_targets.is_empty());
            extra_documents += documents.saturating_sub(workspace.devices.len());
            assert_cleaned_up(root.path(), &diagnostics.processes);
        }
    }
    println!(
        "summary runs={} failed={failures} foreign_navigations={foreign_navigations} runs_with_extensions_in_session_contexts={extension_runs} extra_document_requests={extra_documents}",
        iterations * 2
    );
    if !baseline {
        assert_eq!(failures, 0, "slow-page captures failed");
        assert_eq!(foreign_navigations, 0, "unrequested navigations observed");
        assert_eq!(extension_runs, 0, "extension pages ran in session contexts");
        assert_eq!(extra_documents, 0, "a navigation was replayed");
    }
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_load_timeout_is_reported_and_cleans_up() {
    let fixture = Fixture::start(|_, _| Reply::Stall);
    let workspace = small_workspace(fixture.url("/stall"));
    let root = profile_root();
    let output = tempfile::tempdir().unwrap();
    let outcome = run(
        &workspace,
        &options(test_browser(), root.path()),
        output.path(),
        plan(Limits {
            load: Duration::from_secs(2),
            ..Limits::default()
        }),
    );
    let error = error_text(&outcome);
    assert!(
        error.contains("loading device phone") && error.contains("load event did not arrive"),
        "{error}"
    );
    assert_eq!(
        outcome.diagnostics.navigations.len(),
        1,
        "no further devices"
    );
    assert_cleaned_up(root.path(), &outcome.diagnostics.processes);
    assert!(
        fixture.wait_for(Duration::from_secs(5), |fixture| fixture.abandoned() == 1),
        "the stalled response was not torn down with the browser"
    );
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_cancel_during_active_request_stops_browser() {
    let fixture = Fixture::start(|_, _| Reply::Hang);
    let workspace = small_workspace(fixture.url("/hang"));
    let root = profile_root();
    let output = tempfile::tempdir().unwrap();
    let options = options(test_browser(), root.path());
    let cancel = options.cancel.clone();
    let output_path = output.path().to_owned();
    let worker = thread::spawn(move || run(&workspace, &options, &output_path, Plan::default()));
    assert!(
        fixture.wait_for(Duration::from_secs(20), |fixture| !fixture
            .requests()
            .is_empty()),
        "the document request never arrived"
    );
    let cancelled_at = Instant::now();
    cancel.cancel();
    let outcome = worker.join().unwrap();
    assert!(
        cancelled_at.elapsed() < Duration::from_secs(3),
        "cancellation took {:?}",
        cancelled_at.elapsed()
    );
    assert!(outcome.result.as_ref().unwrap_err().is::<Cancelled>());
    assert_cleaned_up(root.path(), &outcome.diagnostics.processes);
    assert!(
        fixture.wait_for(Duration::from_secs(5), |fixture| fixture.abandoned() == 1),
        "the active request was not closed"
    );
    assert_eq!(
        fixture.requests().len(),
        1,
        "cancellation must not navigate again"
    );
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_partial_failure_keeps_earlier_frames_and_is_not_retried() {
    let served = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&served);
    // The first document succeeds; every later connection closes without a response.
    let fixture = Fixture::start(move |_, sequence| {
        if sequence == 0 {
            counter.fetch_add(1, Ordering::SeqCst);
            Reply::Html {
                body: html("#3355ff"),
                delay: Duration::ZERO,
                cookie: None,
            }
        } else {
            Reply::Drop
        }
    });
    let workspace = small_workspace(fixture.url("/"));
    let root = profile_root();
    let output = tempfile::tempdir().unwrap();
    let outcome = run(
        &workspace,
        &options(test_browser(), root.path()),
        output.path(),
        Plan::default(),
    );
    let error = error_text(&outcome);
    assert!(
        error.contains("navigation failed for tablet") && error.contains("not retried"),
        "{error}"
    );
    assert!(output.path().join("phone.png").is_file());
    assert!(!output.path().join("tablet.png").exists());
    assert!(!output.path().join("desktop.png").exists());
    let navigated: Vec<_> = outcome
        .diagnostics
        .navigations
        .iter()
        .map(|navigation| navigation.device.as_str())
        .collect();
    assert_eq!(
        navigated,
        ["phone", "tablet"],
        "one navigation per device, no retry"
    );
    assert_eq!(served.load(Ordering::SeqCst), 1);
    assert_cleaned_up(root.path(), &outcome.diagnostics.processes);
}
