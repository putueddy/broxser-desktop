//! Related workers must be released without acquiring iframe/page privileges.

use super::*;

fn worker_attached(parent: &str, session: &str, target: &str, kind: &str) -> Value {
    iframe_event(
        parent,
        "Target.attachedToTarget",
        json!({
            "sessionId": session, "waitingForDebugger": true,
            "targetInfo": {"targetId": target, "type": kind, "browserContextId": "CTX1"}
        }),
    )
}

fn worker_requests(peer: &FakePeer, session: &str) -> Vec<String> {
    peer.received
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, owner, _, _)| owner.as_deref() == Some(session))
        .map(|(method, _, _, _)| method.clone())
        .collect()
}

#[test]
fn related_worker_filter_preserves_iframe_pausing_and_excludes_other_targets() {
    assert_eq!(
        iframe_auto_attach(),
        json!({
            "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true,
            "filter": [
                {"type": "iframe", "exclude": false},
                {"type": "worker", "exclude": false},
                {"type": "shared_worker", "exclude": false},
                {"type": "service_worker", "exclude": false},
                {"exclude": true}
            ]
        })
    );
}

#[test]
fn worker_types_resume_once_before_detach_without_page_setup_or_event_privileges() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, session| {
        method == "Runtime.runIfWaitingForDebugger" && session.is_some_and(|s| s.starts_with('W'))
    });
    let live = fake_live(root.path(), Limits::default());
    for (index, kind) in ["worker", "shared_worker", "service_worker"]
        .into_iter()
        .enumerate()
    {
        let session = format!("W{index}");
        let attached = worker_attached("S0", &session, &format!("WT{index}"), kind);
        peer.event(attached.clone());
        wait_for_requests(&peer, "Runtime.runIfWaitingForDebugger", &session, 1);
        peer.event(attached.clone());
        peer.event(iframe_event(
            &session,
            "Page.fileChooserOpened",
            json!({"frameId": "T0"}),
        ));
        peer.event(iframe_event(
            &session,
            "Page.frameAttached",
            json!({"frameId": "UNOWNED", "parentFrameId": "T0"}),
        ));
        wait_until_read(&peer);
        assert_eq!(peer.count("Target.detachFromTarget", "S0"), index);
        let id = peer.last_request_id("Runtime.runIfWaitingForDebugger", &session);
        peer.event(json!({"id": id, "result": {}}));
        wait_for_requests(&peer, "Target.detachFromTarget", "S0", index + 1);
        assert_eq!(
            peer.last_params("Target.detachFromTarget", "S0"),
            json!({"sessionId": session})
        );
        // Duplicate attachment and late ACK never issue another resume/detach.
        peer.event(attached);
        peer.event(json!({"id": id, "result": {}}));
        wait_until_read(&peer);
        assert_eq!(
            worker_requests(&peer, &session),
            ["Runtime.runIfWaitingForDebugger"]
        );
        assert_eq!(peer.count("Target.detachFromTarget", "S0"), index + 1);
    }
    assert_eq!(live.status().devices[0].file_choosers, 0);
    peer.event(frame_download("UNOWNED"));
    wait_until_read(&peer);
    assert_eq!(live.status().devices[0].downloads, 0);
    assert!(running(&live.status()));
    drop(live);
}

#[test]
fn unknown_foreign_or_aliasing_worker_attachments_have_no_side_effects() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = fake_live(root.path(), Limits::default());
    let mut events = vec![
        worker_attached("UNKNOWN", "W-unknown", "WT-unknown", "worker"),
        worker_attached("S0", "S0", "WT-session-alias", "worker"),
        worker_attached("S0", "W-target-alias", "T0", "worker"),
        worker_attached("S0", "W-page", "WT-page", "page"),
    ];
    let mut no_parent = worker_attached("S0", "W-no-parent", "WT-no-parent", "worker");
    no_parent.as_object_mut().unwrap().remove("sessionId");
    events.push(no_parent);
    let mut foreign = worker_attached("S0", "W-foreign", "WT-foreign", "worker");
    foreign["params"]["targetInfo"]["browserContextId"] = json!("CTX2");
    events.push(foreign);
    let mut no_context = worker_attached("S0", "W-no-context", "WT-no-context", "worker");
    no_context["params"]["targetInfo"]
        .as_object_mut()
        .unwrap()
        .remove("browserContextId");
    events.push(no_context);
    let mut no_target = worker_attached("S0", "W-no-target", "WT-no-target", "worker");
    no_target["params"]["targetInfo"]
        .as_object_mut()
        .unwrap()
        .remove("targetId");
    events.push(no_target);
    let mut no_session = worker_attached("S0", "W-no-session", "WT-no-session", "worker");
    no_session["params"]
        .as_object_mut()
        .unwrap()
        .remove("sessionId");
    events.push(no_session);
    for event in events {
        peer.event(event);
    }
    wait_until_read(&peer);
    assert_eq!(peer.count("Runtime.runIfWaitingForDebugger", "S0"), 0);
    assert_eq!(peer.count("Target.detachFromTarget", "S0"), 0);
    assert!(
        !peer
            .received
            .lock()
            .unwrap()
            .iter()
            .any(|(_, session, _, _)| {
                session
                    .as_deref()
                    .is_some_and(|session| session.starts_with('W'))
            })
    );
    assert!(running(&live.status()));
    drop(live);
}

#[test]
fn failed_worker_resume_is_not_retried_or_detached_on_a_late_success() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, session| {
        method == "Runtime.runIfWaitingForDebugger" && session == Some("W-failed")
    });
    let live = fake_live(root.path(), Limits::default());
    let attached = worker_attached("S0", "W-failed", "WT-failed", "worker");
    peer.event(attached.clone());
    wait_for_requests(&peer, "Runtime.runIfWaitingForDebugger", "W-failed", 1);
    let id = peer.last_request_id("Runtime.runIfWaitingForDebugger", "W-failed");
    peer.event(json!({"id": id, "error": {"code": -32000, "message": "worker still held"}}));
    wait_until_read(&peer);
    peer.event(attached);
    peer.event(json!({"id": id, "result": {}}));
    wait_until_read(&peer);
    assert_eq!(
        worker_requests(&peer, "W-failed"),
        ["Runtime.runIfWaitingForDebugger"]
    );
    assert_eq!(peer.count("Target.detachFromTarget", "S0"), 0);
    assert!(running(&live.status()));
    drop(live);
}

#[test]
fn retired_iframe_parent_releases_workers_before_detach_and_forgets_late_children() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, session| {
        method == "Runtime.runIfWaitingForDebugger" && matches!(session, Some("I1" | "W-child"))
    });
    let live = fake_live(root.path(), Limits::default());
    peer.event(iframe_attached("S0", "I1", "F1"));
    wait_for_requests(&peer, "Runtime.runIfWaitingForDebugger", "I1", 1);
    peer.event(iframe_event(
        "S0",
        "Page.frameDetached",
        json!({"frameId": "F1", "reason": "remove"}),
    ));
    peer.event(worker_attached("I1", "W-child", "WT-child", "worker"));
    wait_for_requests(&peer, "Runtime.runIfWaitingForDebugger", "W-child", 1);
    assert_eq!(
        worker_requests(&peer, "W-child"),
        ["Runtime.runIfWaitingForDebugger"]
    );
    peer.event(
        json!({"id": peer.last_request_id("Runtime.runIfWaitingForDebugger", "I1"), "result": {}}),
    );
    wait_until_read(&peer);
    assert_eq!(peer.count("Target.detachFromTarget", "S0"), 0);
    peer.event(json!({"id": peer.last_request_id("Runtime.runIfWaitingForDebugger", "W-child"), "result": {}}));
    wait_for_requests(&peer, "Target.detachFromTarget", "I1", 1);
    assert_eq!(peer.count("Target.detachFromTarget", "S0"), 0);
    peer.event(iframe_event(
        "I1",
        "Target.detachedFromTarget",
        json!({"sessionId": "W-child"}),
    ));
    wait_for_requests(&peer, "Target.detachFromTarget", "S0", 1);
    peer.event(iframe_event(
        "S0",
        "Target.detachedFromTarget",
        json!({"sessionId": "I1"}),
    ));
    peer.event(worker_attached("I1", "W-late", "WT-late", "worker"));
    wait_until_read(&peer);
    assert!(worker_requests(&peer, "W-late").is_empty());
    assert_eq!(peer.count("Runtime.runIfWaitingForDebugger", "I1"), 1);
    drop(live);
}

#[test]
fn worker_cleanup_and_iframe_setup_share_the_session_cap() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, session| {
        (method == "Runtime.runIfWaitingForDebugger"
            && session.is_some_and(|session| session.starts_with('W')))
            || (method == "Page.enable" && session == Some("I-held"))
    });
    let live = fake_live(root.path(), Limits::default());
    peer.event(iframe_attached("S0", "I-held", "F-held"));
    wait_for_requests(&peer, "Page.enable", "I-held", 1);
    for index in 0..MAX_IFRAME_SESSIONS - 1 {
        let session = format!("W{index}");
        peer.event(worker_attached(
            "S0",
            &session,
            &format!("WT{index}"),
            "worker",
        ));
        wait_for_requests(&peer, "Runtime.runIfWaitingForDebugger", &session, 1);
    }
    peer.event(worker_attached("S0", "W-overflow", "WT-overflow", "worker"));
    let status = wait_for(
        &live,
        "bounded worker cleanup",
        Duration::from_secs(3),
        |status| matches!(status.runtime, RuntimeState::Stopped { .. }),
    );
    let RuntimeState::Stopped { error: Some(error) } = status.runtime else {
        panic!("missing limit error");
    };
    assert!(
        error.contains("CDP target session limit exceeded"),
        "{error}"
    );
    assert!(worker_requests(&peer, "W-overflow").is_empty());
    drop(live);
}

#[test]
fn malformed_worker_identity_is_rejected_before_resume() {
    let oversized = "x".repeat(129);
    for (session, target) in [
        ("", "WT"),
        ("W", ""),
        ("W\n", "WT"),
        ("W", "WT\0"),
        (oversized.as_str(), "WT"),
        ("W", oversized.as_str()),
    ] {
        let root = profile_root();
        let peer = FakePeer::start(root.path(), |_, _| false);
        let live = fake_live(root.path(), Limits::default());
        peer.event(worker_attached("S0", session, target, "worker"));
        let status = wait_for(
            &live,
            "invalid worker identity",
            Duration::from_secs(3),
            |status| matches!(status.runtime, RuntimeState::Stopped { .. }),
        );
        let RuntimeState::Stopped { error: Some(error) } = status.runtime else {
            panic!("missing identity error");
        };
        assert!(error.contains("invalid session identity"), "{error}");
        assert!(
            worker_requests(&peer, session)
                .iter()
                .all(|method| method != "Runtime.runIfWaitingForDebugger")
        );
        drop(live);
    }
}
