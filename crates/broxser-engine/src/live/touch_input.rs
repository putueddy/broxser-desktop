use super::*;

fn pointer(kind: PointerKind, x: f64, y: f64) -> Command {
    Command::Pointer {
        device: 0,
        event: PointerEvent {
            kind,
            x,
            y,
            button: PointerButton::Left,
            buttons: u8::from(kind != PointerKind::Up),
            click_count: 1,
            modifiers: Modifiers::default(),
        },
    }
}

fn touch_live(root: &Path, limits: Limits) -> LiveSession {
    let mut config = workspace("http://127.0.0.1:4173/".into());
    config.devices[0].touch = true;
    let live = LiveSession::start_with(config, fake_options(root), limits, || {}).unwrap();
    wait_for(&live, "touch runtime", Duration::from_secs(10), running);
    live
}

fn touches(peer: &FakePeer) -> Vec<Value> {
    peer.received
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, session, _, _)| {
            method == "Input.dispatchTouchEvent" && session.as_deref() == Some("S0")
        })
        .map(|(_, _, _, params)| params.clone())
        .collect()
}

fn types(peer: &FakePeer) -> Vec<String> {
    touches(peer)
        .iter()
        .map(|params| params["type"].as_str().unwrap().to_owned())
        .collect()
}

// The other device's key follows earlier UI commands; the frame ack follows
// the runtime's input flush and CDP drain. This also checks device isolation.
fn settled(live: &LiveSession, peer: &FakePeer) {
    let count = peer.count("Input.dispatchKeyEvent", "S2");
    assert!(live.send(Command::Key {
        device: 2,
        key: key(true)
    }));
    wait_for_requests(peer, "Input.dispatchKeyEvent", "S2", count + 1);
    wait_until_read(peer);
}

#[test]
fn rapid_touch_release_keeps_the_final_position_and_pending_excursion() {
    for release_x in [100.0, 400.0] {
        let root = profile_root();
        let peer = FakePeer::start(root.path(), |_, _| false);
        let live = touch_live(root.path(), Limits::default());
        for command in [
            pointer(PointerKind::Down, 100.0, 200.0),
            pointer(PointerKind::Move, 300.0, 200.0),
            pointer(PointerKind::Up, release_x, 200.0),
        ] {
            assert!(live.send(command));
        }
        wait_for_requests(&peer, "Input.dispatchTouchEvent", "S0", 4);
        settled(&live, &peer);
        let requests = touches(&peer);
        assert_eq!(
            types(&peer),
            ["touchStart", "touchMove", "touchMove", "touchEnd"]
        );
        assert_eq!(requests[1]["touchPoints"], json!([{"x":300.0,"y":200.0}]));
        assert_eq!(
            requests[2]["touchPoints"],
            json!([{"x":release_x.min(359.0),"y":200.0}])
        );
        assert_eq!(requests[3]["touchPoints"], json!([]));
        assert_eq!(live.status().protocol_error, None);
        drop(live);
    }
}

#[test]
fn touch_release_sends_its_last_move_while_an_earlier_move_is_unanswered() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, session| {
        method == "Input.dispatchTouchEvent" && session == Some("S0")
    });
    let live = touch_live(root.path(), Limits::default());
    assert!(live.send(pointer(PointerKind::Down, 100.0, 200.0)));
    assert!(live.send(pointer(PointerKind::Move, 150.0, 200.0)));
    wait_for_requests(&peer, "Input.dispatchTouchEvent", "S0", 2);
    assert!(live.send(pointer(PointerKind::Move, 200.0, 200.0)));
    assert!(live.send(pointer(PointerKind::Up, 300.0, 200.0)));
    wait_for_requests(&peer, "Input.dispatchTouchEvent", "S0", 5);
    assert_eq!(
        types(&peer),
        [
            "touchStart",
            "touchMove",
            "touchMove",
            "touchMove",
            "touchEnd"
        ]
    );
    assert_eq!(
        touches(&peer)[3]["touchPoints"],
        json!([{"x":300.0,"y":200.0}])
    );
    peer.release();
    settled(&live, &peer);
    assert_eq!(
        types(&peer).len(),
        5,
        "no move or release replays after the held reply"
    );
    drop(live);
}

#[test]
fn orphan_touch_moves_and_releases_are_ignored() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = touch_live(root.path(), Limits::default());
    for kind in [PointerKind::Move, PointerKind::Up] {
        assert!(live.send(pointer(kind, 100.0, 200.0)));
    }
    settled(&live, &peer);
    assert!(touches(&peer).is_empty());
    assert!(live.send(pointer(PointerKind::Down, 100.0, 200.0)));
    assert!(live.send(pointer(PointerKind::Up, 100.0, 200.0)));
    wait_for_requests(&peer, "Input.dispatchTouchEvent", "S0", 2);
    for kind in [PointerKind::Move, PointerKind::Up] {
        assert!(live.send(pointer(kind, 100.0, 200.0)));
    }
    settled(&live, &peer);
    assert_eq!(types(&peer), ["touchStart", "touchEnd"]);
    drop(live);
}

#[test]
fn a_second_touch_press_cancels_the_abandoned_finger() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = touch_live(root.path(), Limits::default());
    assert!(live.send(pointer(PointerKind::Down, 100.0, 200.0)));
    assert!(live.send(pointer(PointerKind::Down, 120.0, 220.0)));
    assert!(live.send(pointer(PointerKind::Up, 120.0, 220.0)));
    wait_for_requests(&peer, "Input.dispatchTouchEvent", "S0", 4);
    assert_eq!(
        types(&peer),
        ["touchStart", "touchCancel", "touchStart", "touchEnd"]
    );
    drop(live);
}

#[test]
fn hiding_cancels_touch_before_ignoring_input_and_show_does_not_replay_release() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = touch_live(root.path(), Limits::default());
    assert!(live.send(pointer(PointerKind::Down, 100.0, 200.0)));
    assert!(live.send(pointer(PointerKind::Move, 200.0, 200.0)));
    assert!(live.send(Command::SetVisible {
        device: 0,
        visible: false
    }));
    assert!(live.send(pointer(PointerKind::Up, 200.0, 200.0)));
    assert!(live.send(Command::SetVisible {
        device: 0,
        visible: true
    }));
    settled(&live, &peer);
    let before = touches(&peer).len();
    assert_eq!(types(&peer).last().unwrap(), "touchCancel");
    assert!(!types(&peer).contains(&"touchEnd".to_owned()));
    let log = peer.received.lock().unwrap();
    let cancel = log
        .iter()
        .position(|(m, s, _, p)| {
            m == "Input.dispatchTouchEvent"
                && s.as_deref() == Some("S0")
                && p["type"] == "touchCancel"
        })
        .unwrap();
    let hidden = log
        .iter()
        .position(|(m, s, _, p)| {
            m == "Input.setIgnoreInputEvents" && s.as_deref() == Some("S0") && p["ignore"] == true
        })
        .unwrap();
    assert!(cancel < hidden);
    drop(log);
    assert!(live.send(pointer(PointerKind::Move, 250.0, 200.0)));
    assert!(live.send(pointer(PointerKind::Up, 250.0, 200.0)));
    settled(&live, &peer);
    assert_eq!(touches(&peer).len(), before);
    assert!(live.send(pointer(PointerKind::Down, 120.0, 220.0)));
    assert!(live.send(pointer(PointerKind::Up, 120.0, 220.0)));
    wait_for_requests(&peer, "Input.dispatchTouchEvent", "S0", before + 2);
    assert_eq!(&types(&peer)[before..], ["touchStart", "touchEnd"]);
    drop(live);
}

#[test]
fn a_dialog_retires_touch_without_queuing_a_release_or_cancel_behind_it() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = touch_live(root.path(), Limits::default());
    assert!(live.send(pointer(PointerKind::Down, 100.0, 200.0)));
    wait_for_requests(&peer, "Input.dispatchTouchEvent", "S0", 1);
    peer.event(dialog_opening("alert", "touch", ""));
    let dialog = phone_dialog(&live);
    for kind in [PointerKind::Move, PointerKind::Up, PointerKind::Down] {
        assert!(live.send(pointer(kind, 200.0, 200.0)));
    }
    settled(&live, &peer);
    assert_eq!(types(&peer), ["touchStart"]);
    answer_phone(&live, dialog.token, true, None);
    peer.event(dialog_closed(true));
    wait_for(&live, "dialog closed", Duration::from_secs(2), |s| {
        s.devices[0].dialog.is_none()
    });
    settled(&live, &peer);
    assert_eq!(
        types(&peer),
        ["touchStart"],
        "closing a dialog replays no old gesture"
    );
    assert!(live.send(pointer(PointerKind::Move, 200.0, 200.0)));
    assert!(live.send(pointer(PointerKind::Up, 200.0, 200.0)));
    assert!(live.send(pointer(PointerKind::Down, 120.0, 220.0)));
    assert!(live.send(pointer(PointerKind::Up, 120.0, 220.0)));
    wait_for_requests(&peer, "Input.dispatchTouchEvent", "S0", 4);
    assert_eq!(
        types(&peer),
        ["touchStart", "touchCancel", "touchStart", "touchEnd"]
    );
    drop(live);
}

#[test]
fn document_replacement_crash_detach_and_page_navigation_retire_touch_ownership() {
    for event in [
        phone(
            "Page.frameNavigated",
            json!({"frame":{"id":"T0","loaderId":"replacement","url":"http://127.0.0.1/new"}}),
        ),
        json!({"method":"Target.targetCrashed","params":{"targetId":"T0"}}),
        json!({"method":"Target.detachedFromTarget","params":{"targetId":"T0","sessionId":"S0"}}),
        phone(
            "Page.frameRequestedNavigation",
            json!({"frameId":"T0","url":"http://127.0.0.1/new","reason":"scriptInitiated","disposition":"currentTab"}),
        ),
        phone(
            "Page.frameStartedNavigating",
            json!({"frameId":"T0","loaderId":"new","navigationType":"differentDocument"}),
        ),
    ] {
        let root = profile_root();
        let peer = FakePeer::start(root.path(), |_, _| false);
        let live = touch_live(root.path(), Limits::default());
        assert!(live.send(pointer(PointerKind::Down, 100.0, 200.0)));
        wait_for_requests(&peer, "Input.dispatchTouchEvent", "S0", 1);
        peer.event(event.clone());
        wait_until_read(&peer);
        assert!(live.send(pointer(PointerKind::Move, 200.0, 200.0)));
        assert!(live.send(pointer(PointerKind::Up, 200.0, 200.0)));
        settled(&live, &peer);
        assert_eq!(types(&peer), ["touchStart"], "{event}");
        assert!(live.send(pointer(PointerKind::Down, 120.0, 220.0)));
        assert!(live.send(pointer(PointerKind::Up, 120.0, 220.0)));
        wait_for_requests(&peer, "Input.dispatchTouchEvent", "S0", 4);
        assert_eq!(
            types(&peer),
            ["touchStart", "touchCancel", "touchStart", "touchEnd"],
            "{event}"
        );
        drop(live);
    }
}

#[test]
fn explicit_navigation_cancels_touch_before_loading_and_discards_old_release() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = touch_live(root.path(), Limits::default());
    assert!(live.send(pointer(PointerKind::Down, 100.0, 200.0)));
    assert!(live.send(Command::Reload { device: 0 }));
    assert!(live.send(pointer(PointerKind::Up, 200.0, 200.0)));
    settled(&live, &peer);
    assert_eq!(types(&peer), ["touchStart", "touchCancel"]);
    let log = peer.received.lock().unwrap();
    let cancel = log
        .iter()
        .position(|(m, s, _, p)| {
            m == "Input.dispatchTouchEvent"
                && s.as_deref() == Some("S0")
                && p["type"] == "touchCancel"
        })
        .unwrap();
    let reload = log
        .iter()
        .position(|(m, s, _, _)| m == "Page.reload" && s.as_deref() == Some("S0"))
        .unwrap();
    assert!(cancel < reload);
    drop(log);
    drop(live);
}

#[test]
fn touch_release_at_the_input_limit_cancels_or_resets_before_a_fresh_press() {
    for full in [false, true] {
        let root = profile_root();
        let peer = FakePeer::start(root.path(), |method, session| {
            method.starts_with("Input.") && session == Some("S0")
        });
        let live = touch_live(root.path(), Limits::default());
        assert!(live.send(pointer(PointerKind::Down, 100.0, 200.0)));
        send_keys(&live, 0, 15);
        wait_for_requests(&peer, "Input.dispatchKeyEvent", "S0", 30);
        if full {
            assert!(live.send(Command::Key {
                device: 0,
                key: key(true)
            }));
            wait_for_requests(&peer, "Input.dispatchKeyEvent", "S0", 31);
        }
        assert!(live.send(pointer(PointerKind::Move, 300.0, 200.0)));
        assert!(live.send(pointer(PointerKind::Up, 300.0, 200.0)));
        wait_for(&live, "touch input limit", Duration::from_secs(2), |s| {
            s.devices[0].error.as_deref() == Some(NOT_RESPONDING)
        });
        settled(&live, &peer);
        assert_eq!(
            types(&peer),
            if full {
                vec!["touchStart"]
            } else {
                vec!["touchStart", "touchCancel"]
            }
        );
        assert_eq!(
            peer.count("Input.dispatchKeyEvent", "S0") + touches(&peer).len(),
            MAX_UNANSWERED_INPUT
        );
        peer.release();
        wait_for(&live, "input answered", Duration::from_secs(2), |s| {
            s.devices[0].error.is_none()
        });
        settled(&live, &peer);
        assert!(live.send(pointer(PointerKind::Up, 300.0, 200.0)));
        assert!(live.send(pointer(PointerKind::Move, 300.0, 200.0)));
        settled(&live, &peer);
        let before = touches(&peer).len();
        assert!(live.send(pointer(PointerKind::Down, 120.0, 220.0)));
        assert!(live.send(pointer(PointerKind::Up, 120.0, 220.0)));
        wait_for_requests(&peer, "Input.dispatchTouchEvent", "S0", 4);
        assert_eq!(
            &types(&peer)[before..],
            if full {
                vec!["touchCancel", "touchStart", "touchEnd"]
            } else {
                vec!["touchStart", "touchEnd"]
            }
        );
        drop(live);
    }
}

#[test]
fn touch_timeout_discards_moves_and_release_until_a_new_press_resets_the_finger() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, session| {
        method == "Input.dispatchTouchEvent" && session == Some("S0")
    });
    let live = touch_live(
        root.path(),
        Limits {
            command: Duration::from_millis(200),
            ..Limits::default()
        },
    );
    assert!(live.send(pointer(PointerKind::Down, 100.0, 200.0)));
    wait_for(&live, "touch timeout", Duration::from_secs(2), |s| {
        s.devices[0].error.as_deref() == Some(NOT_RESPONDING)
    });
    assert!(live.send(pointer(PointerKind::Up, 300.0, 200.0)));
    peer.release();
    wait_for(&live, "touch response", Duration::from_secs(2), |s| {
        s.devices[0].error.is_none()
    });
    settled(&live, &peer);
    assert_eq!(types(&peer), ["touchStart"]);
    assert!(live.send(pointer(PointerKind::Move, 300.0, 200.0)));
    assert!(live.send(pointer(PointerKind::Up, 300.0, 200.0)));
    assert!(live.send(pointer(PointerKind::Down, 120.0, 220.0)));
    assert!(live.send(pointer(PointerKind::Up, 120.0, 220.0)));
    wait_for_requests(&peer, "Input.dispatchTouchEvent", "S0", 4);
    assert_eq!(
        types(&peer),
        ["touchStart", "touchCancel", "touchStart", "touchEnd"]
    );
    drop(live);
}

#[test]
fn ignored_touch_buttons_keep_the_native_ime_target_and_composition() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = touch_live(root.path(), Limits::default());
    peer.event(phone(
        "Runtime.executionContextCreated",
        json!({"context":{"id":42,"name":IME_WORLD,"auxData":{"frameId":"T0","type":"isolated"}}}),
    ));
    peer.event(phone("Runtime.bindingCalled", json!({"name":IME_BINDING,"executionContextId":42,"payload":"{\"active\":true,\"anchor\":1,\"x\":20,\"y\":40,\"width\":1,\"height\":18}"})));
    let input = wait_for(&live, "IME target", Duration::from_secs(2), |s| {
        s.devices[0].text_input.is_some()
    })
    .devices[0]
        .text_input
        .unwrap();
    assert!(live.send(Command::Ime {
        device: 0,
        target: input.target,
        action: ImeAction::Preedit {
            text: "ka".into(),
            selection: 2..2
        }
    }));
    wait_for_requests(&peer, "Input.imeSetComposition", "S0", 1);
    for button in [PointerButton::Right, PointerButton::Middle] {
        for kind in [PointerKind::Down, PointerKind::Up] {
            let Command::Pointer { device, mut event } = pointer(kind, 100.0, 200.0) else {
                unreachable!()
            };
            event.button = button;
            assert!(live.send(Command::Pointer { device, event }));
        }
    }
    settled(&live, &peer);
    assert_eq!(live.status().devices[0].text_input, Some(input));
    assert_eq!(peer.count("Input.imeSetComposition", "S0"), 1);
    assert!(touches(&peer).is_empty());
    assert!(live.send(Command::Ime {
        device: 0,
        target: input.target,
        action: ImeAction::Commit { text: "か".into() }
    }));
    wait_for_requests(&peer, "Input.insertText", "S0", 1);
    drop(live);
}

#[test]
fn rapid_out_and_back_touch_keeps_its_furthest_coalesced_point() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, session| {
        method == "Input.dispatchTouchEvent" && session == Some("S0")
    });
    let live = touch_live(root.path(), Limits::default());
    for command in [
        pointer(PointerKind::Down, 100.0, 200.0),
        pointer(PointerKind::Move, 300.0, 200.0),
        pointer(PointerKind::Move, 200.0, 200.0),
        pointer(PointerKind::Move, 100.0, 200.0),
        pointer(PointerKind::Up, 100.0, 200.0),
    ] {
        assert!(live.send(command));
    }
    wait_for_requests(&peer, "Input.dispatchTouchEvent", "S0", 4);
    let requests = touches(&peer);
    assert_eq!(
        types(&peer),
        ["touchStart", "touchMove", "touchMove", "touchEnd"]
    );
    assert_eq!(requests[1]["touchPoints"], json!([{"x":300.0,"y":200.0}]));
    assert_eq!(requests[2]["touchPoints"], json!([{"x":100.0,"y":200.0}]));
    peer.release();
    settled(&live, &peer);
    assert_eq!(touches(&peer).len(), 4);
    drop(live);
}

#[test]
fn explicit_touch_cancel_drops_moves_and_never_synthesizes_a_release() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = touch_live(root.path(), Limits::default());
    assert!(live.send(pointer(PointerKind::Down, 100.0, 200.0)));
    assert!(live.send(pointer(PointerKind::Move, 300.0, 200.0)));
    assert!(live.send(Command::CancelTouch { device: 0 }));
    assert!(live.send(pointer(PointerKind::Up, 100.0, 200.0)));
    settled(&live, &peer);
    assert_eq!(types(&peer).last().unwrap(), "touchCancel");
    assert!(!types(&peer).contains(&"touchEnd".to_owned()));
    let count = touches(&peer).len();
    assert!(live.send(Command::CancelTouch { device: 0 }));
    assert!(live.send(Command::CancelTouch { device: 2 }));
    assert!(live.send(Command::CancelTouch { device: usize::MAX }));
    settled(&live, &peer);
    assert_eq!(
        touches(&peer).len(),
        count,
        "cancellation is idempotent and touches no mouse"
    );
    assert!(live.send(pointer(PointerKind::Down, 120.0, 220.0)));
    assert!(live.send(pointer(PointerKind::Up, 120.0, 220.0)));
    wait_for_requests(&peer, "Input.dispatchTouchEvent", "S0", count + 2);
    assert_eq!(&types(&peer)[count..], ["touchStart", "touchEnd"]);
    drop(live);
}

#[test]
fn touch_held_behind_ime_keeps_its_excursion_and_cancel_preserves_other_input() {
    for canceled in [false, true] {
        let root = profile_root();
        let holding = Arc::new(AtomicBool::new(false));
        let hold = Arc::clone(&holding);
        let peer = FakePeer::start(root.path(), move |method, session| {
            method == "Runtime.evaluate" && session == Some("S0") && hold.load(Ordering::SeqCst)
        });
        let live = touch_live(root.path(), Limits::default());
        peer.event(phone(
        "Runtime.executionContextCreated",
        json!({"context":{"id":42,"name":IME_WORLD,"auxData":{"frameId":"T0","type":"isolated"}}}),
    ));
        peer.event(phone("Runtime.bindingCalled", json!({"name":IME_BINDING,"executionContextId":42,"payload":"{\"active\":true,\"anchor\":1,\"x\":20,\"y\":40,\"width\":1,\"height\":18}"})));
        let input = wait_for(&live, "IME target", Duration::from_secs(2), |s| {
            s.devices[0].text_input.is_some()
        })
        .devices[0]
            .text_input
            .unwrap();
        settled(&live, &peer);
        holding.store(true, Ordering::SeqCst);
        let reads = peer.count("Runtime.evaluate", "S0");
        assert!(live.send(Command::Ime {
            device: 0,
            target: input.target,
            action: ImeAction::Preedit {
                text: "ka".into(),
                selection: 2..2
            }
        }));
        wait_for_requests(&peer, "Runtime.evaluate", "S0", reads + 1);
        for command in [
            pointer(PointerKind::Down, 100.0, 200.0),
            pointer(PointerKind::Move, 300.0, 200.0),
            pointer(PointerKind::Move, 100.0, 200.0),
            pointer(PointerKind::Up, 100.0, 200.0),
            Command::Key {
                device: 0,
                key: key(true),
            },
        ] {
            assert!(live.send(command));
        }
        if canceled {
            assert!(live.send(Command::CancelTouch { device: 0 }));
        }
        settled(&live, &peer);
        assert!(touches(&peer).is_empty());
        assert_eq!(peer.count("Input.dispatchKeyEvent", "S0"), 0);
        peer.release();
        wait_for_requests(&peer, "Input.imeSetComposition", "S0", 1);
        wait_for_requests(&peer, "Input.dispatchKeyEvent", "S0", 1);
        settled(&live, &peer);
        if canceled {
            assert!(
                touches(&peer).is_empty(),
                "the held press was canceled, not replayed"
            );
        } else {
            let requests = touches(&peer);
            assert_eq!(
                types(&peer),
                ["touchStart", "touchMove", "touchMove", "touchEnd"]
            );
            assert_eq!(requests[1]["touchPoints"], json!([{"x":300.0,"y":200.0}]));
            assert_eq!(requests[2]["touchPoints"], json!([{"x":100.0,"y":200.0}]));
        }
        assert_eq!(live.status().protocol_error, None);
        drop(live);
    }
}
// Each snapshot travels in one fetch. End/cancel reports check gestures that
// must not click; a positive tap waits for its actual click report, since the
// browser can synthesize it after the end-report timer has already fired.
const REGRESSION_PAGE: &str = r#"<!doctype html><meta name=viewport content="width=device-width,initial-scale=1">
<style>body{margin:0}#t{position:absolute;left:20px;top:100px;width:200px;height:200px;background:#36c;touch-action:none}</style>
<div id=t></div><script>
let log = [], gesture = 0, asked = false;
const report = (kind = 'touch-regression', delay = 50) => {
  const recorded = log, n = gesture;
  setTimeout(() => fetch('/event?' + new URLSearchParams({kind,w:innerWidth,path:location.pathname,n,events:JSON.stringify(recorded)})), delay);
};
for (const type of ['pointerdown','pointermove','pointerup','pointercancel','touchstart','touchmove','touchend','touchcancel','click']) {
  t.addEventListener(type, e => {
    if (type === 'pointerdown') {
      gesture++; log = [];
      fetch('/event?' + new URLSearchParams({kind:'touch-regression-press',w:innerWidth,path:location.pathname,n:gesture}));
    }
    const point = e.changedTouches ? e.changedTouches[0] : e;
    log.push({type, x:Math.round(point.clientX), y:Math.round(point.clientY)});
    if (type === 'touchend' || type === 'touchcancel') report();
    if (type === 'click') report('touch-regression-click', 0);
    if (type === 'touchstart' && location.pathname === '/dialog' && !asked) { asked = true; alert('touch'); }
  });
}
fetch('/event?' + new URLSearchParams({kind:'touch-regression-ready',w:innerWidth,path:location.pathname}));
</script>"#;

fn regression_fixture() -> Fixture {
    Fixture::start(|path, _| Reply::Html {
        body: if path.path.starts_with("/event?") {
            String::new()
        } else {
            REGRESSION_PAGE.into()
        },
        delay: Duration::ZERO,
        cookie: None,
    })
}

fn regression_reports(fixture: &Fixture) -> Vec<Value> {
    let mut reports: Vec<_> = events(fixture, "touch-regression")
        .into_iter()
        .filter(|event| event["w"] == "360")
        .collect();
    reports.sort_by_key(|event| {
        let phase = match event["path"].as_str() {
            "/touch" => 0,
            "/dialog" => 1,
            _ => 2,
        };
        (phase, event["n"].parse::<u32>().unwrap())
    });
    reports
        .iter()
        .map(|event| serde_json::from_str::<Value>(&event["events"]).unwrap())
        .collect()
}

fn wait_regression_report(fixture: &Fixture, path: &str, gesture: u32) -> Value {
    wait_gesture_report(fixture, path, gesture, "touch-regression")
}

fn wait_tap_report(fixture: &Fixture, path: &str, gesture: u32) -> Value {
    wait_gesture_report(fixture, path, gesture, "touch-regression-click")
}

fn wait_gesture_report(fixture: &Fixture, path: &str, gesture: u32, kind: &str) -> Value {
    let report = |f: &Fixture| {
        events(f, kind)
            .into_iter()
            .find(|event| {
                event["w"] == "360" && event["path"] == path && event["n"] == gesture.to_string()
            })
            .map(|event| serde_json::from_str::<Value>(&event["events"]).unwrap())
    };
    assert!(
        fixture.wait_for(Duration::from_secs(10), |f| report(f).is_some()),
        "missing {kind} {path} gesture {gesture}: {:?}",
        regression_reports(fixture)
    );
    report(fixture).unwrap()
}

fn wait_regression_press(fixture: &Fixture, path: &str, gesture: u32) {
    assert!(
        fixture.wait_for(Duration::from_secs(10), |f| {
            events(f, "touch-regression-press").iter().any(|event| {
                event["w"] == "360" && event["path"] == path && event["n"] == gesture.to_string()
            })
        }),
        "missing {path} press {gesture}: {:?}",
        regression_reports(fixture)
    );
}

fn fresh_tap(report: &Value, x: u32, y: u32) -> bool {
    let events = report.as_array().unwrap();
    events
        .iter()
        .any(|event| event["type"] == "touchstart" && event["x"] == x && event["y"] == y)
        && events
            .iter()
            .any(|event| event["type"] == "touchend" && event["x"] == x && event["y"] == y)
        && events
            .iter()
            .any(|event| event["type"] == "click" && event["x"] == x && event["y"] == y)
}

fn live_touch_regression(fixture: &Fixture, path: &str) -> Live {
    let mut config = workspace(fixture.url(path));
    config.devices[0].touch = true;
    let live = Live::start(config);
    live.wait("touch regression page", Duration::from_secs(30), |s| {
        running(s)
            && s.devices[0].frames > 0
            && events(fixture, "touch-regression-ready")
                .iter()
                .any(|e| e["w"] == "360" && e["path"] == path)
    });
    live
}

// Temporary CI diagnostic: only attaches to this test's app-owned browser.
// Keep the original gesture commands and assertions, and collect the browser's
// own input trace even when an assertion panics. No event is retried.
fn touch_trace_request(probe: &mut Cdp, method: &str, params: Value) -> Value {
    let id = probe.send(method, params, None).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(response) = probe.take_response(id) {
            return parse_response(response, method).unwrap();
        }
        assert!(
            probe.read_until(deadline).unwrap(),
            "trace {method} timed out"
        );
    }
}

fn start_touch_trace(live: &Live) -> Cdp {
    let profiles: Vec<_> = fs_entries(live.root.path())
        .into_iter()
        .filter(|name| name.starts_with("broxser-cdp-"))
        .collect();
    assert_eq!(profiles.len(), 1);
    let endpoint_path = live
        .root
        .path()
        .join(&profiles[0])
        .join("DevToolsActivePort");
    let endpoint = std::fs::read_to_string(endpoint_path).unwrap();
    let (port, path) = browser::parse_endpoint(&endpoint).unwrap();
    let mut probe = Cdp::connect(port, &path, Duration::from_secs(5), Cancellation::new()).unwrap();
    touch_trace_request(
        &mut probe,
        "Tracing.start",
        json!({"categories":"input", "options":"record-until-full", "transferMode":"ReportEvents"}),
    );
    probe
}

fn finish_touch_trace(probe: &mut Cdp) {
    touch_trace_request(probe, "Tracing.end", json!({}));
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut recent = VecDeque::new();
    let mut suppressed = 0;
    let mut flings = 0;
    loop {
        while let Some(event) = probe.pop_event() {
            if event.method == "Tracing.tracingComplete" {
                eprintln!(
                    "TOUCH TRACE: fling records={flings}, suppressed tap events={suppressed}"
                );
                for entry in recent {
                    eprintln!("TOUCH TRACE: {entry}");
                }
                return;
            }
            if event.method != "Tracing.dataCollected" {
                continue;
            }
            for entry in event.params["value"].as_array().unwrap() {
                let name = entry["name"].as_str().unwrap_or_default();
                if name == "FilterTapSuppression" {
                    suppressed += 1;
                }
                if name == "FlingController::HandlingGestureFling" {
                    flings += 1;
                }
                if matches!(
                    name,
                    "FilterTapSuppression"
                        | "FlingController::HandlingGestureFling"
                        | "GestureProvider::OnTouchEvent"
                        | "NoActiveFling"
                        | "FilteredForFling"
                        | "FilteredForTouchAction"
                ) {
                    if recent.len() == 250 {
                        recent.pop_front();
                    }
                    recent.push_back(entry.clone());
                }
            }
        }
        assert!(
            probe.read_until(deadline).unwrap(),
            "touch trace did not finish"
        );
    }
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_rapid_swipe_and_coalesced_out_and_back_do_not_click() {
    let fixture = regression_fixture();
    let live = live_touch_regression(&fixture, "/touch");
    let mut trace = start_touch_trace(&live);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for round in 0..12 {
            eprintln!("TOUCH TRACE: original sequence round {round}");
            for (index, release_x) in [300.0, 100.0].into_iter().enumerate() {
                for command in [
                    pointer(PointerKind::Down, 100.0, 200.0),
                    pointer(PointerKind::Move, 300.0, 200.0),
                    pointer(PointerKind::Move, release_x, 200.0),
                    pointer(PointerKind::Up, release_x, 200.0),
                ] {
                    live.send(command);
                }
                let report =
                    wait_regression_report(&fixture, "/touch", round * 3 + index as u32 + 1);
                let report = report.as_array().unwrap();
                // Chromium may coalesce the DOM touchmoves too. The fake CDP test
                // checks the excursion dispatch; here the final point and absence of
                // click prove that its gesture recognition retained the drag.
                assert!(
                    report
                        .iter()
                        .any(|e| e["type"] == "touchend" && e["x"] == release_x),
                    "{report:?}"
                );
                assert!(
                    !report.iter().any(|e| e["type"] == "click"),
                    "swipe generated click: {report:?}"
                );
            }
            // A late click from the preceding swipe must not satisfy this fresh tap.
            live.send(pointer(PointerKind::Down, 120.0, 220.0));
            live.send(pointer(PointerKind::Up, 120.0, 220.0));
            let report = wait_tap_report(&fixture, "/touch", round * 3 + 3);
            assert!(fresh_tap(&report, 120, 220), "fresh tap: {report}");
            assert_eq!(live.session().status().protocol_error, None);
        }
    }));
    finish_touch_trace(&mut trace);
    drop(trace);
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_abandoned_touch_is_canceled_after_hide_dialog_and_navigation() {
    let fixture = regression_fixture();
    let live = live_touch_regression(&fixture, "/touch");
    live.send(pointer(PointerKind::Down, 100.0, 200.0));
    // Hide once the page has the press. A press still on its way when the
    // page stops taking input can be dropped together with its cancel, which
    // leaves the page consistent but nothing to cancel.
    wait_regression_press(&fixture, "/touch", 1);
    live.send(Command::SetVisible {
        device: 0,
        visible: false,
    });
    live.send(pointer(PointerKind::Up, 100.0, 200.0));
    live.send(Command::SetVisible {
        device: 0,
        visible: true,
    });
    let cancel = wait_regression_report(&fixture, "/touch", 1);
    assert!(
        cancel
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "touchcancel"),
        "hide: {cancel}"
    );
    assert!(
        !cancel
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "click"),
        "hide: {cancel}"
    );
    live.send(pointer(PointerKind::Down, 120.0, 220.0));
    live.send(pointer(PointerKind::Up, 120.0, 220.0));
    let tap = wait_tap_report(&fixture, "/touch", 2);
    assert!(fresh_tap(&tap, 120, 220), "after hide: {tap}");

    live.send(Command::NavigateAll {
        url: fixture.url("/dialog"),
    });
    live.wait("dialog page", Duration::from_secs(30), |s| {
        !s.devices[0].loading
            && s.devices[0].url.ends_with("/dialog")
            && events(&fixture, "touch-regression-ready")
                .iter()
                .any(|e| e["w"] == "360" && e["path"] == "/dialog")
    });
    live.send(pointer(PointerKind::Down, 100.0, 200.0));
    let dialog = live
        .wait("touch dialog", Duration::from_secs(10), |s| {
            s.devices[0].dialog.is_some()
        })
        .devices[0]
        .dialog
        .clone()
        .unwrap();
    live.send(pointer(PointerKind::Up, 100.0, 200.0));
    live.send(Command::AnswerDialog {
        device: 0,
        token: dialog.token,
        accept: true,
        text: None,
    });
    live.wait("answered touch dialog", Duration::from_secs(10), |s| {
        s.devices[0].dialog.is_none()
    });
    live.send(pointer(PointerKind::Down, 120.0, 220.0));
    live.send(pointer(PointerKind::Up, 120.0, 220.0));
    let cancel = wait_regression_report(&fixture, "/dialog", 1);
    assert!(
        cancel
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "touchcancel"),
        "dialog: {cancel}"
    );
    assert!(
        !cancel
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "click"),
        "dialog: {cancel}"
    );
    let tap = wait_tap_report(&fixture, "/dialog", 2);
    assert!(fresh_tap(&tap, 120, 220), "after dialog: {tap}");

    live.send(pointer(PointerKind::Down, 100.0, 200.0));
    live.send(Command::NavigateAll {
        url: fixture.url("/replacement"),
    });
    live.send(pointer(PointerKind::Up, 100.0, 200.0));
    live.wait("replacement page", Duration::from_secs(30), |s| {
        !s.devices[0].loading
            && s.devices[0].url.ends_with("/replacement")
            && events(&fixture, "touch-regression-ready")
                .iter()
                .any(|e| e["w"] == "360" && e["path"] == "/replacement")
    });
    live.send(pointer(PointerKind::Down, 120.0, 220.0));
    live.send(pointer(PointerKind::Up, 120.0, 220.0));
    let tap = wait_tap_report(&fixture, "/replacement", 1);
    assert!(fresh_tap(&tap, 120, 220), "after navigation: {tap}");
    assert_eq!(live.session().status().protocol_error, None);
    live.close();
}
