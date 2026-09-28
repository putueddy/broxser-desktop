use super::*;

const CERTIFICATE_ERROR: &str = "net::ERR_CERT_AUTHORITY_INVALID";
const FAILED_URL: &str = "https://127.0.0.1:4173/";

fn committed(loader: &str, failed: bool) -> Value {
    let mut frame = json!({"id": "T0", "loaderId": loader, "url": FAILED_URL});
    if failed {
        frame["url"] = json!("chrome-error://chromewebdata/");
        frame["unreachableUrl"] = json!(FAILED_URL);
    }
    phone("Page.frameNavigated", json!({"frame": frame}))
}

fn certificate_peer(root: &Path) -> FakePeer {
    FakePeer::start_with_events(
        root,
        |method, session| {
            session == Some("S0") && matches!(method, "Page.navigate" | "Page.reload")
        },
        |method, session| {
            if method == "Page.reload" && session == Some("S0") {
                vec![phone(
                    "Page.frameStartedNavigating",
                    json!({"frameId": "T0", "loaderId": "reload", "navigationType": "reload", "url": FAILED_URL}),
                )]
            } else {
                Vec::new()
            }
        },
    )
}

fn confirmed_failure(peer: &FakePeer, live: &LiveSession, commit_before_reply: bool) -> String {
    wait_for_requests(peer, "Page.navigate", "S0", 1);
    if commit_before_reply {
        peer.event(committed("failed", true));
        wait_until_read(peer);
    }
    peer.event(failed_navigate_reply(peer, CERTIFICATE_ERROR));
    let status = wait_for(
        live,
        "the certificate report",
        Duration::from_secs(5),
        |s| {
            s.devices[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains(CERTIFICATE_ERROR))
        },
    );
    if !commit_before_reply {
        peer.event(committed("failed", true));
        wait_until_read(peer);
    }
    let error = status.devices[0].error.clone().unwrap();
    assert!(error.contains("CACertificates policy"));
    assert_eq!(
        live.status().devices[0].error.as_deref(),
        Some(error.as_str())
    );
    error
}

#[test]
fn certificate_reload_keeps_last_confirmed_failure_until_a_document_commits() {
    for commit_before_reply in [false, true] {
        let root = profile_root();
        let peer = certificate_peer(root.path());
        let live = fake_live(root.path(), Limits::default());
        let error = confirmed_failure(&peer, &live, commit_before_reply);
        assert!(live.send(Command::Reload { device: 0 }));
        wait_for_requests(&peer, "Page.reload", "S0", 1);
        wait_until_read(&peer);
        let status = live.status();
        assert!(status.devices[0].loading);
        assert_eq!(
            status.devices[0].error.as_deref(),
            Some(error.as_str()),
            "Reload must retain the last confirmed failure while loading"
        );
        let reload_reply = |peer: &FakePeer| json!({"id": peer.last_request_id("Page.reload", "S0"), "result": {}});
        if !commit_before_reply {
            peer.event(reload_reply(&peer));
            wait_until_read(&peer);
        }
        peer.event(committed("reload", true));
        wait_until_read(&peer);
        if commit_before_reply {
            peer.event(reload_reply(&peer));
        }
        peer.event(phone("Page.frameStoppedLoading", json!({"frameId": "T0"})));
        wait_until_read(&peer);
        let status = live.status();
        assert!(!status.devices[0].loading);
        assert_eq!(status.devices[0].error.as_deref(), Some(error.as_str()));

        // A later reload that really commits a document clears the old report,
        // whether the commit arrives before or after its empty acknowledgement.
        assert!(live.send(Command::Reload { device: 0 }));
        wait_for_requests(&peer, "Page.reload", "S0", 2);
        if !commit_before_reply {
            peer.event(reload_reply(&peer));
            wait_until_read(&peer);
        }
        peer.event(committed("reload", false));
        wait_until_read(&peer);
        assert_eq!(live.status().devices[0].error, None);
        if commit_before_reply {
            peer.event(reload_reply(&peer));
            wait_until_read(&peer);
            assert_eq!(live.status().devices[0].error, None);
        }
        assert_eq!(peer.count("Page.reload", "S0"), 2, "only explicit reloads");
        assert_eq!(peer.count("Page.navigate", "S0"), 1, "no automatic retry");
        assert_eq!(peer.count("Page.stopLoading", "S0"), 0);
        drop(live);
    }
}

#[test]
fn explicit_new_navigation_clears_and_replaces_the_old_certificate_report() {
    let root = profile_root();
    let peer = certificate_peer(root.path());
    let live = fake_live(root.path(), Limits::default());
    confirmed_failure(&peer, &live, false);
    assert!(live.send(Command::NavigateAll {
        url: "http://127.0.0.1:4173/next".into(),
    }));
    wait_for_requests(&peer, "Page.navigate", "S0", 2);
    wait_until_read(&peer);
    assert_eq!(live.status().devices[0].error, None);
    peer.event(failed_navigate_reply(&peer, "net::ERR_CONNECTION_REFUSED"));
    let status = wait_for(&live, "the new failure", Duration::from_secs(5), |s| {
        s.devices[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("ERR_CONNECTION_REFUSED"))
    });
    assert!(
        !status.devices[0]
            .error
            .as_deref()
            .unwrap()
            .contains(CERTIFICATE_ERROR)
    );
    assert_eq!(
        peer.count("Page.navigate", "S0"),
        2,
        "only explicit navigation"
    );
    assert_eq!(peer.count("Page.reload", "S0"), 0);
    drop(live);
}
