use super::*;

fn started(frame: &str, loader: &str, kind: &str) -> Value {
    json!({"method": "Page.frameStartedNavigating", "sessionId": "S0", "params": {
        "frameId": frame, "loaderId": loader, "navigationType": kind,
        "url": "http://127.0.0.1:4173/next"
    }})
}

fn reload_started(method: &str, session: Option<&str>) -> Vec<Value> {
    if method == "Page.reload" && session == Some("S0") {
        vec![started("T0", "reload", "reload")]
    } else {
        Vec::new()
    }
}

#[test]
fn reload_deadline_ignores_requests_subframes_and_same_document_events() {
    let root = profile_root();
    let peer = FakePeer::start_with_events(root.path(), |_, _| false, reload_started);
    let live = fake_live(
        root.path(),
        Limits {
            load: Duration::from_millis(500),
            ..Limits::default()
        },
    );
    assert!(live.send(Command::Reload { device: 0 }));
    wait_for_requests(&peer, "Page.reload", "S0", 1);
    // A request alone may be canceled. Subframes, History API updates and
    // repeated notifications of the same loader do not replace the reload.
    peer.event(
        json!({"method": "Page.frameRequestedNavigation", "sessionId": "S0", "params": {
            "frameId": "T0", "reason": "anchorClick", "disposition": "currentTab",
            "url": "http://127.0.0.1:4173/next"
        }}),
    );
    peer.event(started("subframe", "child", "differentDocument"));
    peer.event(started("T0", "old-document", "sameDocument"));
    peer.event(started("T0", "old-document", "historySameDocument"));
    peer.event(started("T0", "reload", "differentDocument"));
    peer.event(
        json!({"method": "Page.frameNavigated", "sessionId": "S0", "params": {
            "frame": {"id": "T0", "loaderId": "older-document", "url": "http://127.0.0.1:4173/"}
        }}),
    );
    peer.event(
        json!({"method": "Page.navigatedWithinDocument", "sessionId": "S0", "params": {
            "frameId": "T0", "navigationType": "historyApi", "url": "http://127.0.0.1:4173/#state"
        }}),
    );
    peer.event(
        json!({"method": "Page.frameStoppedLoading", "sessionId": "S0", "params": {
            "frameId": "subframe"
        }}),
    );
    wait_for_requests(&peer, "Page.stopLoading", "S0", 1);
    let status = live.status();
    assert!(running(&status));
    assert!(
        status.devices[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("loading stopped"))
    );
    assert_eq!(peer.count("Page.reload", "S0"), 1, "never retried");
    drop(live);
}

#[test]
fn accepted_navigation_retires_reload_with_early_late_or_missing_reply() {
    for (hold_reply, release_reply) in [(false, false), (true, true), (true, false)] {
        let root = profile_root();
        let peer = FakePeer::start_with_events(
            root.path(),
            move |method, session| hold_reply && method == "Page.reload" && session == Some("S0"),
            reload_started,
        );
        let limits = Limits {
            load: Duration::from_millis(500),
            ..Limits::default()
        };
        let live = fake_live(root.path(), limits);
        assert!(live.send(Command::Reload { device: 0 }));
        wait_for_requests(&peer, "Page.reload", "S0", 1);
        peer.event(started("T0", "replacement", "differentDocument"));
        // A late or missing reply cannot transfer the deadline to the new load.
        if release_reply {
            peer.release();
        }
        thread::sleep(limits.load + Duration::from_millis(200));
        let status = live.status();
        assert!(running(&status));
        assert_eq!(status.devices[0].error, None, "hold_reply={hold_reply}");
        assert_eq!(
            peer.count("Page.stopLoading", "S0"),
            0,
            "hold_reply={hold_reply}"
        );
        drop(live);
    }
}

#[test]
fn replaced_reload_error_does_not_describe_the_new_navigation() {
    let root = profile_root();
    let peer = FakePeer::start_with_events(
        root.path(),
        |method, _| method == "Page.reload",
        reload_started,
    );
    let limits = Limits {
        load: Duration::from_millis(500),
        ..Limits::default()
    };
    let live = fake_live(root.path(), limits);
    assert!(live.send(Command::Reload { device: 0 }));
    wait_for_requests(&peer, "Page.reload", "S0", 1);
    peer.event(started("T0", "replacement", "differentDocument"));
    peer.event(
        json!({"id": peer.last_request_id("Page.reload", "S0"), "error": {
            "code": -32000, "message": "Reload was superseded"
        }}),
    );
    thread::sleep(limits.load + Duration::from_millis(200));
    let status = live.status();
    assert_eq!(status.devices[0].error, None);
    assert!(status.devices[0].loading);
    assert_eq!(peer.count("Page.stopLoading", "S0"), 0);
    drop(live);
}

#[test]
fn reload_settlement_before_a_late_or_missing_reply_does_not_leave_a_deadline() {
    for (commit, hold_reply) in [(true, false), (true, true), (false, false), (false, true)] {
        let root = profile_root();
        let peer = FakePeer::start_with_events(
            root.path(),
            move |method, _| hold_reply && method == "Page.reload",
            move |method, session| {
                let mut events = reload_started(method, session);
                if !events.is_empty() {
                    events.push(if commit {
                        json!({"method": "Page.frameNavigated", "sessionId": "S0", "params": {
                            "frame": {"id": "T0", "loaderId": "reload", "url": "http://127.0.0.1:4173/"}
                        }})
                    } else {
                        json!({"method": "Page.frameStoppedLoading", "sessionId": "S0", "params": {"frameId": "T0"}})
                    });
                }
                events
            },
        );
        let limits = Limits {
            load: Duration::from_millis(500),
            ..Limits::default()
        };
        let live = fake_live(root.path(), limits);
        assert!(live.send(Command::Reload { device: 0 }));
        wait_for_requests(&peer, "Page.reload", "S0", 1);
        // The document committed, but a subresource may keep loading indefinitely.
        thread::sleep(limits.load + Duration::from_millis(200));
        assert_eq!(
            live.status().devices[0].error,
            None,
            "hold_reply={hold_reply}"
        );
        assert_eq!(
            peer.count("Page.stopLoading", "S0"),
            0,
            "hold_reply={hold_reply}"
        );
        drop(live);
    }
}

#[test]
fn queued_reload_starts_do_not_erase_the_latest_deadline() {
    for interleaved in [false, true] {
        let root = profile_root();
        let peer = FakePeer::start(root.path(), |method, _| method == "Page.reload");
        let live = fake_live(
            root.path(),
            Limits {
                load: Duration::from_millis(500),
                ..Limits::default()
            },
        );
        for _ in 0..2 {
            assert!(live.send(Command::Reload { device: 0 }));
        }
        wait_for_requests(&peer, "Page.reload", "S0", 2);
        // Earlier browser events may still be buffered when the UI sends Reload.
        peer.event(started("T0", "old-reload", "reload"));
        if interleaved {
            peer.event(started("T0", "page-navigation", "differentDocument"));
        }
        peer.event(started("T0", "new-reload", "reload"));
        peer.release();
        wait_for_requests(&peer, "Page.stopLoading", "S0", 1);
        assert_eq!(peer.count("Page.reload", "S0"), 2, "never retried");
        assert_eq!(peer.count("Page.stopLoading", "S0"), 1);
        drop(live);
    }
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_reload_deadline_survives_history_api_changes() {
    for method in ["replaceState", "pushState"] {
        let documents = AtomicUsize::new(0);
        let fixture = Fixture::start(move |request, _| {
            if request.path == "/" && documents.fetch_add(1, Ordering::SeqCst) == 3 {
                return Reply::Hang;
            }
            Reply::Html {
                body: PAGE.replace("AUTO", &format!(
                    "if (innerWidth === 360) setInterval(() => {{ history.{method}({{}}, '', location.pathname + '#t' + Date.now()); report('history', {{}}); }}, 100);"
                )),
                delay: Duration::ZERO,
                cookie: None,
            }
        });
        let limits = Limits {
            load: Duration::from_secs(2),
            ..Limits::default()
        };
        let live = Live::start_with(workspace(fixture.url("/")), limits);
        live.wait("pages", Duration::from_secs(30), |status| {
            running(status)
                && status
                    .devices
                    .iter()
                    .all(|device| device.frames > 0 && !device.loading)
        });
        let reloaded = Instant::now();
        live.send(Command::Reload { device: 0 });
        assert!(fixture.wait_for(Duration::from_secs(5), |fixture| count(fixture, "/") == 4));
        let histories = events(&fixture, "history").len();
        assert!(fixture.wait_for(
            Duration::from_secs(1),
            |fixture| events(fixture, "history").len() > histories + 1
        ));
        // Helium reports each History API update as `Page.frameStartedLoading`.
        // One sent before the browser handled `Page.stopLoading` can briefly
        // mark the device loading again until its `Page.frameStoppedLoading`.
        let status = live.wait(
            "reload deadline despite History API",
            Duration::from_secs(5),
            |status| {
                !status.devices[0].loading
                    && status.devices[0]
                        .error
                        .as_deref()
                        .is_some_and(|error| error.contains("loading stopped"))
            },
        );
        assert!(reloaded.elapsed() >= limits.load);
        assert!(
            status.devices[1..]
                .iter()
                .all(|device| device.error.is_none())
        );
        assert!(fixture.wait_for(Duration::from_secs(5), |fixture| fixture.abandoned() == 1));
        assert_eq!(count(&fixture, "/"), 4, "never retried");
        println!(
            "{method}: held reload stopped after {} ms",
            reloaded.elapsed().as_millis()
        );
        live.close();
    }
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_page_navigation_does_not_inherit_reload_deadline() {
    for scripted in [false, true] {
        let documents = AtomicUsize::new(0);
        let fixture = Fixture::start(move |request, _| {
            if request.path == "/next"
                || (request.path == "/" && documents.fetch_add(1, Ordering::SeqCst) >= 3)
            {
                return Reply::Hang;
            }
            Reply::Html {
                body: PAGE.replace("AUTO", if scripted {
                    "document.getElementById('link').addEventListener('click', e => { e.preventDefault(); location.href = '/next'; });"
                } else { "" }),
                delay: Duration::ZERO,
                cookie: None,
            }
        });
        let limits = Limits {
            load: Duration::from_secs(2),
            ..Limits::default()
        };
        let live = Live::start_with(workspace(fixture.url("/")), limits);
        live.wait("pages", Duration::from_secs(30), |status| {
            loaded(status, &fixture, "/")
        });
        live.send(Command::Reload { device: 0 });
        assert!(fixture.wait_for(Duration::from_secs(5), |fixture| count(fixture, "/") == 4));
        click(&live, 0, 40.0, 120.0);
        assert!(
            fixture.wait_for(Duration::from_secs(5), |fixture| count(fixture, "/next")
                == 1)
        );
        assert!(fixture.wait_for(Duration::from_secs(5), |fixture| fixture.abandoned() == 1));
        // Wait beyond the original deadline: the replacement stays browser-owned.
        thread::sleep(limits.load + Duration::from_millis(300));
        let status = live.session().status();
        assert!(running(&status));
        assert_eq!(
            fixture.abandoned(),
            1,
            "scripted={scripted}: only the superseded reload may be canceled"
        );
        assert_eq!(status.devices[0].error, None, "scripted={scripted}");
        assert!(status.devices[0].loading);
        assert_eq!(count(&fixture, "/next"), 1, "never retried");
        // A new explicit reload still receives a fresh deadline.
        live.send(Command::Reload { device: 0 });
        live.wait(
            "new explicit reload deadline",
            Duration::from_secs(5),
            |status| {
                status.devices[0]
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("loading stopped"))
            },
        );
        live.close();
    }
}
