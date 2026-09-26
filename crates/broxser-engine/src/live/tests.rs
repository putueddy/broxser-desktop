use super::*;
use crate::browser::{self, ProcessIdentity};
use crate::test_support::{FakeBrowser, Fixture, Reply, fake_browser, profile_root, test_browser};
use broxser_core::{Device, Session};
use std::collections::HashMap;
use std::net::TcpListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[path = "navigation_deadlines.rs"]
mod navigation_deadlines;

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
    assert_eq!(control.key, "c");
    for (name, key, key_code) in [
        ("f5", "F5", 116),
        ("f12", "F12", 123),
        ("insert", "Insert", 45),
    ] {
        let named = KeyInput::from_key(name, None, none, true).unwrap();
        assert_eq!(
            (
                named.key.as_str(),
                named.code.as_str(),
                named.key_code,
                named.text
            ),
            (key, key, key_code, None)
        );
    }
    assert!(KeyInput::from_key("f13", None, none, true).is_none());
    assert!(KeyInput::from_key("back", None, none, true).is_none());
    assert!(KeyInput::from_key("\u{7}", None, none, true).is_none());
}

/// GPUI names a dead key `dead_acute`, or guesses an ASCII name for its
/// position without a character (`'`), and names the composed character
/// after its keysym (`eacute`). Measured on a German X11 layout (ADR 0010).
#[test]
fn dead_keys_type_nothing_and_composed_characters_are_typed() {
    let none = Modifiers::default();
    for (name, down) in [("dead_acute", true), ("dead_acute", false), ("'", true)] {
        let dead = KeyInput::from_key(name, None, none, down).unwrap();
        assert_eq!(
            (
                dead.key.as_str(),
                dead.code.as_str(),
                dead.key_code,
                dead.text
            ),
            ("Dead", "", 0, None),
            "{name}"
        );
    }
    let composed = KeyInput::from_key("eacute", Some("é"), none, true).unwrap();
    assert_eq!(
        (
            composed.key.as_str(),
            composed.code.as_str(),
            composed.key_code
        ),
        ("é", "", 0)
    );
    assert_eq!(composed.text.as_deref(), Some("é"));
    let control = Modifiers {
        control: true,
        ..none
    };
    let shortcut = KeyInput::from_key("eacute", Some("é"), control, true).unwrap();
    assert_eq!(shortcut.text, None);
    // With a shortcut modifier the ASCII name is the key, as before.
    let guessed = KeyInput::from_key("'", None, control, true).unwrap();
    assert_eq!(guessed.key, "'");
    // A German key typing a non-ASCII character keeps working.
    let umlaut = KeyInput::from_key(";", Some("ö"), none, true).unwrap();
    assert_eq!(
        (umlaut.key.as_str(), umlaut.text.as_deref()),
        ("ö", Some("ö"))
    );
}

#[test]
fn paste_keys_are_recognized_and_pasted_text_is_bounded() {
    let key =
        |name: &str, modifiers: Modifiers| KeyInput::from_key(name, None, modifiers, true).unwrap();
    let none = Modifiers::default();
    let control = Modifiers {
        control: true,
        ..none
    };
    let shift = Modifiers {
        shift: true,
        ..none
    };
    assert!(is_paste_key(&key("v", control)));
    assert!(is_paste_key(&key(
        "v",
        Modifiers {
            shift: true,
            ..control
        }
    )));
    assert!(is_paste_key(&key("insert", shift)));
    for (name, modifiers) in [
        ("v", none),
        (
            "v",
            Modifiers {
                alt: true,
                ..control
            },
        ),
        ("v", Modifiers { meta: true, ..none }),
        ("insert", control),
        ("insert", none),
        ("c", control),
    ] {
        assert!(!is_paste_key(&key(name, modifiers)), "{name} {modifiers:?}");
    }
    assert_eq!(
        paste_text("a\u{0}b\r\nc\td\u{7f}").as_deref(),
        Ok("ab\r\nc\td")
    );
    assert_eq!(paste_text(""), Err(PasteRejected::Empty));
    assert_eq!(paste_text("\u{7}\u{1b}"), Err(PasteRejected::Empty));
    let longest = "x".repeat(MAX_PASTE_CHARS);
    assert_eq!(paste_text(&longest).as_deref(), Ok(longest.as_str()));
    assert_eq!(
        paste_text(&format!("{longest}é")),
        Err(PasteRejected::TooLong(MAX_PASTE_CHARS + 1))
    );
}

#[test]
fn keys_that_helium_turns_into_browser_commands_are_recognized() {
    let key = |name: &str, modifiers: Modifiers| {
        let shortcut = modifiers.control || modifiers.alt || modifiers.meta;
        let typed = (name.chars().count() == 1 && !shortcut).then_some(name);
        KeyInput::from_key(name, typed, modifiers, true).unwrap()
    };
    let none = Modifiers::default();
    let shift = Modifiers {
        shift: true,
        ..none
    };
    let control = Modifiers {
        control: true,
        ..none
    };
    let control_shift = Modifiers {
        shift: true,
        ..control
    };
    let alt = Modifiers { alt: true, ..none };
    for (name, modifiers) in [
        ("w", control),
        ("f4", control),
        ("w", control_shift),
        ("f4", alt),
        ("q", control_shift),
        ("m", control_shift),
        ("t", control),
        ("n", control),
        ("n", control_shift),
        ("t", control_shift),
        ("u", control),
        ("j", control),
        ("o", control_shift),
        ("delete", control_shift),
        ("a", control_shift),
        ("f12", none),
        ("i", control_shift),
        ("j", control_shift),
        ("f5", none),
        ("f5", shift),
        ("f5", control),
        ("r", control),
        ("r", control_shift),
        ("left", alt),
        ("right", alt),
        ("home", alt),
    ] {
        assert!(
            is_browser_key(&key(name, modifiers)),
            "{name} {modifiers:?}"
        );
    }
    for (name, modifiers) in [
        ("a", control),
        ("c", control),
        ("x", control),
        ("z", control),
        ("z", control_shift),
        ("c", control_shift),
        ("s", control),
        ("p", control),
        ("f", control),
        ("h", control),
        ("l", control),
        ("tab", control),
        ("left", control),
        ("w", none),
        ("r", shift),
        ("f4", none),
        ("f1", none),
        ("f2", none),
        ("escape", none),
        ("f12", Modifiers { meta: true, ..none }),
    ] {
        assert!(
            !is_browser_key(&key(name, modifiers)),
            "{name} {modifiers:?}"
        );
    }
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
fn old_confirmation_cannot_confirm_a_later_activation_of_the_same_url() {
    let url = "https://example.test/next";
    assert!(same_link_activation(42, url, 42, url));
    assert!(!same_link_activation(43, url, 42, url));
    assert!(!same_link_activation(
        42,
        url,
        42,
        "https://example.test/other"
    ));
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

#[test]
fn early_extension_event_rejects_live_before_navigation() {
    for method in ["Target.targetCreated", "Target.targetInfoChanged"] {
        let root = profile_root();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        std::fs::write(
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
        let live = LiveSession::start(
            workspace("http://127.0.0.1:4173/".into()),
            BrowserOptions {
                executable: fake_browser(FakeBrowser::LoopbackEndpoint),
                headless: true,
                profile_root: Some(root.path().to_owned()),
                cancel: Cancellation::new(),
            },
            || {},
        )
        .unwrap();
        let started = Instant::now();
        let status = loop {
            let status = live.status();
            if matches!(status.runtime, RuntimeState::Stopped { .. }) {
                break status;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "fake live runtime did not stop"
            );
            thread::sleep(Duration::from_millis(10));
        };
        let RuntimeState::Stopped { error: Some(error) } = status.runtime else {
            panic!("expected runtime error")
        };
        assert!(
            error.contains("browser runtime not qualified"),
            "{method}: {error}"
        );
        assert_eq!(navigations.load(Ordering::SeqCst), 0, "{method}");
        drop(live);
        server.join().unwrap();
        assert_eq!(fs_entries(root.path()), ["fake-cdp-port"]);
    }
}

/// A browser CDP endpoint for fake-browser live tests. It answers setup like a
/// browser with one target per device (device `n` gets target `Tn` and session
/// `Sn`) and holds the reply to every request that `hold` selects until
/// [`FakePeer::release`], like a page or a server that stopped answering. A held
/// navigation then fails with `net::ERR_ABORTED`, as a canceled one does.
struct FakePeer {
    received: Requests,
    released: Arc<AtomicBool>,
    events: std::sync::mpsc::Sender<Value>,
    server: Option<thread::JoinHandle<()>>,
}

/// Method, session and command ID of every request, in arrival order.
type Requests = Arc<Mutex<Vec<(String, Option<String>, u64)>>>;

impl FakePeer {
    fn start(root: &Path, hold: impl Fn(&str, Option<&str>) -> bool + Send + 'static) -> Self {
        Self::start_with_events(root, hold, |_, _| Vec::new())
    }

    fn start_with_events(
        root: &Path,
        hold: impl Fn(&str, Option<&str>) -> bool + Send + 'static,
        before_reply: impl Fn(&str, Option<&str>) -> Vec<Value> + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        std::fs::write(
            root.join("fake-cdp-port"),
            listener.local_addr().unwrap().port().to_string(),
        )
        .unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let released = Arc::new(AtomicBool::new(false));
        let (events, event_receiver) = std::sync::mpsc::channel::<Value>();
        let (log, release) = (Arc::clone(&received), Arc::clone(&released));
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            socket
                .get_ref()
                .set_read_timeout(Some(Duration::from_millis(10)))
                .unwrap();
            let (mut contexts, mut targets, mut loaders) = (0, 0, 0);
            let mut held = Vec::new();
            loop {
                for event in event_receiver.try_iter() {
                    if socket
                        .send(tungstenite::Message::Text(event.to_string().into()))
                        .is_err()
                    {
                        return;
                    }
                }
                if release.load(Ordering::SeqCst) {
                    for reply in held.drain(..) {
                        if socket.send(reply).is_err() {
                            return;
                        }
                    }
                }
                let request: Value = match socket.read() {
                    Ok(tungstenite::Message::Text(text)) => {
                        serde_json::from_str(text.as_str()).unwrap()
                    }
                    Ok(_) => continue,
                    Err(tungstenite::Error::Io(error))
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    Err(_) => return,
                };
                let method = request["method"].as_str().unwrap_or_default().to_owned();
                let session = request["sessionId"].as_str().map(str::to_owned);
                log.lock().unwrap().push((
                    method.clone(),
                    session.clone(),
                    request["id"].as_u64().unwrap(),
                ));
                let result = match method.as_str() {
                    "Browser.getVersion" => json!({"product":"Fake/1.0", "protocolVersion":"1.3"}),
                    "Target.createBrowserContext" => {
                        contexts += 1;
                        json!({"browserContextId": format!("CTX{contexts}")})
                    }
                    "Target.createTarget" => {
                        targets += 1;
                        json!({"targetId": format!("T{}", targets - 1)})
                    }
                    "Target.attachToTarget" => json!({
                        "sessionId": request["params"]["targetId"].as_str().unwrap().replacen('T', "S", 1)
                    }),
                    "Page.navigate" => {
                        loaders += 1;
                        json!({
                            "frameId": session.as_deref().unwrap_or_default().replacen('S', "T", 1),
                            "loaderId": format!("L{loaders}")
                        })
                    }
                    _ => json!({}),
                };
                let reply = |result: Value| {
                    tungstenite::Message::Text(
                        json!({"id": request["id"], "result": result})
                            .to_string()
                            .into(),
                    )
                };
                for event in before_reply(&method, session.as_deref()) {
                    if socket
                        .send(tungstenite::Message::Text(event.to_string().into()))
                        .is_err()
                    {
                        return;
                    }
                }
                if hold(&method, session.as_deref()) && !release.load(Ordering::SeqCst) {
                    let mut result = result;
                    if method == "Page.navigate" {
                        result["errorText"] = json!("net::ERR_ABORTED");
                    }
                    held.push(reply(result));
                } else if socket.send(reply(result)).is_err() {
                    return;
                }
            }
        });
        Self {
            received,
            released,
            events,
            server: Some(server),
        }
    }

    /// Requests received so far for `method` on `session`.
    fn count(&self, method: &str, session: &str) -> usize {
        self.received
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, s, _)| m == method && s.as_deref() == Some(session))
            .count()
    }

    fn last_request_id(&self, method: &str, session: &str) -> u64 {
        self.received
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(m, s, _)| m == method && s.as_deref() == Some(session))
            .unwrap()
            .2
    }

    /// Sends every held reply and stops holding, as a page that answers again.
    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
    }

    fn event(&self, event: Value) {
        self.events.send(event).unwrap();
    }
}

impl Drop for FakePeer {
    fn drop(&mut self) {
        // The server ends when the live session closes its websocket.
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

fn fake_options(root: &Path) -> BrowserOptions {
    BrowserOptions {
        executable: fake_browser(FakeBrowser::LoopbackEndpoint),
        headless: true,
        profile_root: Some(root.to_owned()),
        cancel: Cancellation::new(),
    }
}

fn wait_for(
    live: &LiveSession,
    what: &str,
    timeout: Duration,
    condition: impl Fn(&Status) -> bool,
) -> Status {
    let deadline = Instant::now() + timeout;
    loop {
        let status = live.status();
        if condition(&status) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {status:#?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn key(down: bool) -> KeyInput {
    KeyInput::from_key("a", Some("a"), Modifiers::default(), down).unwrap()
}

/// Waits until `peer` has received `count` requests for `method` on `session`.
fn wait_for_requests(peer: &FakePeer, method: &str, session: &str, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while peer.count(method, session) < count {
        assert!(
            Instant::now() < deadline,
            "{method} on {session}: {} of {count} requests",
            peer.count(method, session)
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn fake_live(root: &Path, limits: Limits) -> LiveSession {
    let live = LiveSession::start_with(
        workspace("http://127.0.0.1:4173/".into()),
        fake_options(root),
        limits,
        || {},
    )
    .unwrap();
    wait_for(&live, "the runtime", Duration::from_secs(10), running);
    live
}

fn send_keys(live: &LiveSession, device: usize, presses: usize) {
    for _ in 0..presses {
        for down in [true, false] {
            assert!(live.send(Command::Key {
                device,
                key: key(down)
            }));
        }
    }
}

#[test]
fn unanswered_input_on_one_device_leaves_the_others_running() {
    let root = profile_root();
    // The phone's page stops answering input, like a renderer busy in a script.
    let peer = FakePeer::start(root.path(), |method, session| {
        method.starts_with("Input.dispatch") && session == Some("S0")
    });
    let live = fake_live(root.path(), Limits::default());
    send_keys(&live, 0, 150);
    send_keys(&live, 2, 1);
    wait_for_requests(&peer, "Input.dispatchKeyEvent", "S2", 2);
    let status = wait_for(
        &live,
        "the phone not responding",
        Duration::from_secs(5),
        |status| status.devices[0].error.as_deref() == Some(NOT_RESPONDING),
    );
    assert!(running(&status), "{status:#?}");
    assert_eq!(status.devices[2].error, None);
    // Input beyond the limit was dropped, not queued.
    assert_eq!(
        peer.count("Input.dispatchKeyEvent", "S0"),
        MAX_UNANSWERED_INPUT
    );

    // Explicit navigation still reaches every device once, the stuck one included.
    assert!(live.send(Command::NavigateAll {
        url: "http://127.0.0.1:4173/next".into()
    }));
    for session in ["S0", "S1", "S2"] {
        wait_for_requests(&peer, "Page.navigate", session, 2);
    }
    send_keys(&live, 0, 5);
    send_keys(&live, 1, 1);
    wait_for_requests(&peer, "Input.dispatchKeyEvent", "S1", 2);
    let status = live.status();
    assert_eq!(
        status.devices[0].error.as_deref(),
        Some(NOT_RESPONDING),
        "navigating does not prove that the page answers"
    );
    assert_eq!(
        peer.count("Input.dispatchKeyEvent", "S0"),
        MAX_UNANSWERED_INPUT
    );

    // Once the page answers, input flows again; what was dropped is never sent.
    peer.release();
    wait_for(
        &live,
        "the phone answering",
        Duration::from_secs(5),
        |status| status.devices[0].error.is_none(),
    );
    send_keys(&live, 0, 1);
    wait_for_requests(
        &peer,
        "Input.dispatchKeyEvent",
        "S0",
        MAX_UNANSWERED_INPUT + 2,
    );
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        peer.count("Input.dispatchKeyEvent", "S0"),
        MAX_UNANSWERED_INPUT + 2
    );
    assert!(running(&live.status()));
    drop(live);
}

#[test]
fn input_left_unanswered_reports_not_responding_after_the_command_limit() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, session| {
        method.starts_with("Input.dispatch") && session == Some("S0")
    });
    let limits = Limits {
        command: Duration::from_secs(1),
        ..Limits::default()
    };
    let live = fake_live(root.path(), limits);
    // One click, far below the limit, that the page never answers.
    let sent = Instant::now();
    for (kind, buttons) in [(PointerKind::Down, 1), (PointerKind::Up, 0)] {
        assert!(live.send(Command::Pointer {
            device: 0,
            event: PointerEvent {
                kind,
                x: 10.0,
                y: 10.0,
                button: PointerButton::Left,
                buttons,
                click_count: 1,
                modifiers: Modifiers::default(),
            },
        }));
    }
    wait_for_requests(&peer, "Input.dispatchMouseEvent", "S0", 2);
    assert_eq!(live.status().devices[0].error, None);
    let status = wait_for(
        &live,
        "the phone not responding",
        Duration::from_secs(5),
        |status| status.devices[0].error.as_deref() == Some(NOT_RESPONDING),
    );
    let elapsed = sent.elapsed();
    assert!(elapsed >= limits.command, "reported after {elapsed:?}");
    println!("unanswered click reported after {} ms", elapsed.as_millis());
    assert!(
        status.devices[1..]
            .iter()
            .all(|device| device.error.is_none())
    );
    send_keys(&live, 0, 3);
    thread::sleep(Duration::from_millis(200));
    assert_eq!(peer.count("Input.dispatchKeyEvent", "S0"), 0, "dropped");
    drop(live);
}

#[test]
fn navigation_without_reply_is_stopped_at_its_deadline_and_not_retried() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, session| {
        method == "Page.navigate" && session == Some("S0")
    });
    let limits = Limits {
        load: Duration::from_millis(500),
        ..Limits::default()
    };
    let started = Instant::now();
    let live = fake_live(root.path(), limits);
    let status = wait_for(
        &live,
        "a timeout status",
        Duration::from_secs(5),
        |status| status.devices[0].error.is_some(),
    );
    assert!(started.elapsed() >= limits.load);
    let error = status.devices[0].error.as_deref().unwrap();
    assert!(
        error.contains("no response within 0.5 seconds") && error.contains("not retried"),
        "{error}"
    );
    assert!(!status.devices[0].loading);
    assert!(
        status.devices[1..]
            .iter()
            .all(|device| device.error.is_none())
    );
    wait_for_requests(&peer, "Page.stopLoading", "S0", 1);
    thread::sleep(Duration::from_millis(700));
    assert_eq!(peer.count("Page.navigate", "S0"), 1, "not retried");
    assert_eq!(peer.count("Page.stopLoading", "S0"), 1);
    assert_eq!(peer.count("Page.stopLoading", "S1"), 0);

    // Go is a new explicit navigation: sent once, and stopped again at its deadline.
    assert!(live.send(Command::NavigateAll {
        url: "http://127.0.0.1:4173/next".into()
    }));
    wait_for_requests(&peer, "Page.stopLoading", "S0", 2);
    assert_eq!(peer.count("Page.navigate", "S0"), 2);

    // A reload answers as soon as it starts; it must still commit or stop in time.
    assert!(live.send(Command::Reload { device: 1 }));
    wait_for_requests(&peer, "Page.stopLoading", "S1", 1);
    let status = live.status();
    assert!(
        status.devices[1]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("loading stopped")),
        "{:?}",
        status.devices[1]
    );
    assert_eq!(peer.count("Page.reload", "S1"), 1);
    assert!(running(&status));
    drop(live);
}

#[test]
fn superseded_navigation_answer_does_not_describe_the_new_one() {
    let root = profile_root();
    // Only the phone's first navigation hangs; it fails once released.
    let navigations = AtomicUsize::new(0);
    let peer = FakePeer::start(root.path(), move |method, session| {
        method == "Page.navigate"
            && session == Some("S0")
            && navigations.fetch_add(1, Ordering::SeqCst) == 0
    });
    let live = fake_live(root.path(), Limits::default());
    wait_for_requests(&peer, "Page.navigate", "S0", 1);
    assert!(live.send(Command::NavigateAll {
        url: "http://127.0.0.1:4173/next".into()
    }));
    wait_for_requests(&peer, "Page.navigate", "S0", 2);
    // The browser cancels the first navigation when the second starts.
    peer.release();
    thread::sleep(Duration::from_millis(500));
    let status = live.status();
    assert_eq!(status.devices[0].error, None, "{:?}", status.devices[0]);
    assert!(status.devices[0].loading);
    drop(live);
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

/// Reports what reaches a text area: keys, pastes, input and mouse buttons.
const KEYS_PAGE: &str = r#"<!doctype html><html><head><meta name=viewport content="width=device-width,initial-scale=1">
<style>html,body{margin:0}#area{position:absolute;left:20px;top:200px;width:300px;height:80px}</style></head><body>
<textarea id=area></textarea>
<script>
const report = (kind, data) => fetch('/event?' + new URLSearchParams({kind, w: innerWidth, ...data}));
addEventListener('mousedown', e => report('down', {button: e.button, buttons: e.buttons}), true);
area.addEventListener('keydown', e => report('keydown', {key: e.key, code: e.code}));
area.addEventListener('paste', e => report('paste', {data: e.clipboardData.getData('text')}));
area.addEventListener('input', e => report('input', {v: area.value, type: e.inputType}));
</script></body></html>"#;

fn fixture() -> Fixture {
    Fixture::start(|request, _| {
        let path = request.path.split('?').next().unwrap_or("/");
        let body = match path {
            "/event" => String::new(),
            "/script-key" => PAGE.replace("AUTO", "document.getElementById('field').addEventListener('keydown', () => setTimeout(() => document.getElementById('link').click(), 100));"),
            "/script-pointer" => PAGE.replace("AUTO", "document.body.addEventListener('click', e => { if (!e.target.closest('a')) setTimeout(() => document.getElementById('link').click(), 100); });"),
            "/forged" => PAGE.replace("AUTO", "if (innerWidth === 360) setTimeout(() => { window.__broxserTrustedLink?.(location.origin + '/next'); document.getElementById('link').click(); }, 300);"),
            "/prevent" => PAGE.replace("AUTO", "document.getElementById('link').addEventListener('click', e => { e.preventDefault(); const a = document.createElement('a'); a.href = '/prevent-b'; a.click(); });"),
            "/prevent-same" => PAGE.replace("AUTO", "document.getElementById('link').addEventListener('click', e => { if (e.isTrusted) { e.preventDefault(); document.getElementById('link').click(); } });"),
            "/prevent-clear" => PAGE.replace("AUTO", "document.getElementById('link').addEventListener('click', e => { if (e.isTrusted) { e.preventDefault(); for(let i=0;i<10000;i++) clearTimeout(i); document.getElementById('link').click(); } });"),
            "/nested-input" => "<a href=/next><input id=i style='width:160px;height:40px'></a><script>i.focus(); i.addEventListener('keydown', e => { if(e.key==='Enter') document.querySelector('a').click(); });</script>".into(),
            "/editable-link" => "<a id=a contenteditable=true href=/next style='display:block;width:160px;height:40px'>edit</a><a id=hidden href=/next style='display:none'>hidden</a><script>a.addEventListener('click',e=>{if(e.isTrusted)hidden.click()});</script>".into(),
            "/parent-editable" => "<div contenteditable=true><a id=a href=/next style='display:block;width:160px;height:40px'>edit</a></div><a id=hidden href=/next style='display:none'>hidden</a><script>a.addEventListener('click',e=>{if(e.isTrusted)hidden.click()});</script>".into(),
            "/nested-button" => "<a href=/next style='display:block;width:160px;height:40px'><button id=b style='width:160px;height:40px'>button</button></a><script>b.addEventListener('click',e=>{if(e.isTrusted)document.querySelector('a').click()});</script>".into(),
            "/keyboard" => PAGE.replace("AUTO", "document.getElementById('link').href = '/keyboard-dest'; document.getElementById('link').focus();"),
            "/keys" => KEYS_PAGE.into(),
            "/slowstart" => PAGE.replace("AUTO", "document.getElementById('link').href = '/slow';"),
            "/supersede" => PAGE.replace("AUTO", "document.getElementById('link').href = '/slow'; document.getElementById('link').addEventListener('click', () => setTimeout(() => location.href = '/supersede-dest', 100));"),
            "/longstart" => PAGE.replace("AUTO", &format!("document.getElementById('link').href = '/long?token={}';", "x".repeat(2300))),
            // A page that follows its own link without any user gesture.
            "/auto" => PAGE.replace(
                "AUTO",
                "setTimeout(() => { const a = document.getElementById('link'); a.href = '/after-auto'; a.click(); }, 300);",
            ),
            _ => PAGE.replace("AUTO", ""),
        };
        Reply::Html {
            body,
            delay: if path == "/slow" {
                Duration::from_secs(3)
            } else {
                Duration::ZERO
            },
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
        Self::start_with(workspace, Limits::default())
    }

    fn start_with(workspace: Workspace, limits: Limits) -> Self {
        let root = profile_root();
        let notified = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&notified);
        let session = LiveSession::start_with(
            workspace,
            BrowserOptions {
                executable: test_browser(),
                headless: true,
                profile_root: Some(root.path().to_owned()),
                cancel: Cancellation::new(),
            },
            limits,
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
/// kept memory out of core dumps, a renderer on the CI runner stayed in the
/// kernel's core dump path (state `I`, wait channel `do_exit`) for more than 45
/// seconds.
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
            "pid={} state={state} wchan={} core_limit={core} coredump_filter={} {kind}\n",
            process.pid,
            file("wchan"),
            file("coredump_filter")
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
                .map(|(key, value)| (key.to_owned(), query_value(value)))
                .collect::<HashMap<_, _>>()
        })
        .filter(|event| event.get("kind").map(String::as_str) == Some(kind))
        .collect()
}

/// Decodes a `URLSearchParams` value.
fn query_value(value: &str) -> String {
    let mut bytes = Vec::new();
    let mut input = value.bytes();
    while let Some(byte) = input.next() {
        match byte {
            b'+' => bytes.push(b' '),
            b'%' => {
                let hex: String = input.by_ref().take(2).map(char::from).collect();
                bytes.push(u8::from_str_radix(&hex, 16).unwrap());
            }
            _ => bytes.push(byte),
        }
    }
    String::from_utf8(bytes).unwrap()
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

fn has_input(fixture: &Fixture, width: &str, value: &str) -> bool {
    events(fixture, "input")
        .iter()
        .any(|event| event["w"] == width && event["v"] == value)
}

/// Before ADR 0010 a copy in the Guest phone was pasted into the Admin
/// desktop by Ctrl+V, and a selection there by a middle click.
#[test]
#[ignore = "requires an installed CDP browser"]
fn live_paste_is_explicit_and_never_reads_the_shared_browser_clipboard() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/keys")));
    live.wait("pages", Duration::from_secs(30), |status| {
        loaded(status, &fixture, "/keys")
    });
    let none = Modifiers::default();
    let control = Modifiers {
        control: true,
        ..none
    };
    let press = |device: usize, name: &str, typed: Option<&str>, modifiers: Modifiers| {
        for down in [true, false] {
            let key = KeyInput::from_key(name, typed, modifiers, down).unwrap();
            live.send(Command::Key { device, key });
        }
    };
    let pointer = |device: usize, kind, button, buttons| {
        live.send(Command::Pointer {
            device,
            event: PointerEvent {
                kind,
                x: 60.0,
                y: 230.0,
                button,
                buttons,
                click_count: 1,
                modifiers: none,
            },
        });
    };
    click(&live, 0, 40.0, 210.0);
    click(&live, 2, 40.0, 210.0);
    assert!(fixture.wait_for(
        Duration::from_secs(5),
        |fixture| events(fixture, "down").len() == 2
    ));

    // Copied and still selected in the Guest phone: the browser's clipboard
    // and selection buffer, which all sessions of the browser share.
    for name in ["s", "e", "c", "r", "e", "t"] {
        press(0, name, Some(name), none);
    }
    press(0, "a", None, control);
    press(0, "c", None, control);
    assert!(fixture.wait_for(Duration::from_secs(5), |fixture| {
        has_input(fixture, "360", "secret")
    }));

    // Paste keys and middle clicks in the Admin desktop reach no page.
    press(2, "v", None, control);
    press(
        2,
        "v",
        None,
        Modifiers {
            shift: true,
            ..control
        },
    );
    press(
        2,
        "insert",
        None,
        Modifiers {
            shift: true,
            ..none
        },
    );
    pointer(2, PointerKind::Down, PointerButton::Middle, 4);
    pointer(2, PointerKind::Up, PointerButton::Middle, 0);
    // A left click with the middle button still held shows no middle button.
    pointer(2, PointerKind::Down, PointerButton::Left, 5);
    pointer(2, PointerKind::Up, PointerButton::Left, 4);
    // An explicit paste inserts exactly its text into the focused element.
    let pasted = "pasted é\n2";
    live.send(Command::InsertText {
        device: 2,
        text: pasted.into(),
    });
    assert!(
        fixture.wait_for(Duration::from_secs(5), |fixture| has_input(
            fixture, "1000", pasted
        )),
        "inserted text did not reach the desktop"
    );
    thread::sleep(Duration::from_millis(400));
    let desktop = |kind: &str| -> Vec<HashMap<String, String>> {
        events(&fixture, kind)
            .into_iter()
            .filter(|event| event["w"] == "1000")
            .collect()
    };
    assert_eq!(desktop("keydown"), [], "paste keys must not reach the page");
    assert_eq!(events(&fixture, "paste"), [], "no page reads a clipboard");
    let inputs = desktop("input");
    assert!(
        inputs.iter().all(|event| event["v"] == pasted),
        "only the inserted text arrives: {inputs:?}"
    );
    let downs: Vec<(String, String)> = desktop("down")
        .into_iter()
        .map(|event| (event["button"].clone(), event["buttons"].clone()))
        .collect();
    assert_eq!(
        downs,
        [("0".into(), "1".into()), ("0".into(), "1".into())],
        "middle presses must not reach the page"
    );

    // Composed characters and function keys reach the page.
    press(2, "eacute", Some("é"), none);
    press(2, "f2", None, none);
    assert!(fixture.wait_for(Duration::from_secs(5), |fixture| {
        has_input(fixture, "1000", "pasted é\n2é")
    }));
    assert!(fixture.wait_for(Duration::from_secs(5), |_| {
        let keys: Vec<String> = desktop("keydown")
            .into_iter()
            .map(|event| event["key"].clone())
            .collect();
        keys == ["é", "F2"]
    }));

    // Nothing is inserted into a hidden device, or later when it is shown,
    // and text that a paste would not produce is not inserted.
    live.send(Command::SetVisible {
        device: 0,
        visible: false,
    });
    live.wait("phone hidden", Duration::from_secs(5), |status| {
        !status.devices[0].streaming
    });
    live.send(Command::InsertText {
        device: 0,
        text: "hidden".into(),
    });
    live.send(Command::InsertText {
        device: 2,
        text: "bell\u{7}".into(),
    });
    live.send(Command::SetVisible {
        device: 0,
        visible: true,
    });
    live.wait("phone visible", Duration::from_secs(5), |status| {
        status.devices[0].streaming
    });
    thread::sleep(Duration::from_millis(500));
    let inputs = events(&fixture, "input");
    assert!(
        inputs
            .iter()
            .all(|event| !event["v"].contains("hidden") && !event["v"].contains("bell")),
        "{inputs:?}"
    );
    assert_eq!(count(&fixture, "/keys"), 3, "input must not reload pages");
    live.close();
}

/// Page targets of the test's own browser, from its loopback DevTools endpoint.
fn page_targets(root: &Path) -> Vec<String> {
    use std::io::{Read, Write};
    let port = std::fs::read_dir(root)
        .unwrap()
        .find_map(|entry| {
            std::fs::read_to_string(entry.ok()?.path().join("DevToolsActivePort")).ok()
        })
        .and_then(|contents| contents.lines().next()?.parse::<u16>().ok())
        .expect("DevToolsActivePort");
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET /json/list HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    // The endpoint may keep the connection open after the response.
    let mut response = Vec::new();
    let mut buffer = [0; 16384];
    let body = loop {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0, "DevTools endpoint closed the connection early");
        response.extend_from_slice(&buffer[..read]);
        let text = String::from_utf8_lossy(&response);
        if let Some((head, body)) = text.split_once("\r\n\r\n")
            && let Some(length) = head.lines().find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")?
                    .trim()
                    .parse::<usize>()
                    .ok()
            })
            && body.len() >= length
        {
            break body[..length].to_owned();
        }
    };
    let targets: Vec<Value> = serde_json::from_str(&body).unwrap();
    let mut pages: Vec<String> = targets
        .iter()
        .filter(|target| target["type"] == "page")
        .map(|target| target["url"].as_str().unwrap_or_default().to_owned())
        .collect();
    pages.sort();
    pages
}

/// Before ADR 0010 these keys closed devices (Alt+F4 in the Guest tablet
/// also closed the Guest phone), crashed the browser (Ctrl+Shift+M), opened
/// tabs, browser pages and DevTools that Broxser never showed, and reloaded or
/// navigated pages outside Broxser's commands (Helium 0.18.1.1).
#[test]
#[ignore = "requires an installed CDP browser"]
fn live_browser_command_keys_never_reach_the_browser() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/second")));
    live.wait("pages", Duration::from_secs(30), |status| {
        loaded(status, &fixture, "/second")
    });
    // History to go back to.
    live.send(Command::NavigateAll {
        url: fixture.url("/keys"),
    });
    live.wait("keys pages", Duration::from_secs(30), |status| {
        loaded(status, &fixture, "/keys")
    });
    let pages = page_targets(live.root.path());
    click(&live, 1, 40.0, 210.0);
    click(&live, 2, 40.0, 210.0);
    assert!(fixture.wait_for(
        Duration::from_secs(5),
        |fixture| events(fixture, "down").len() == 2
    ));
    let none = Modifiers::default();
    let shift = Modifiers {
        shift: true,
        ..none
    };
    let control = Modifiers {
        control: true,
        ..none
    };
    let control_shift = Modifiers {
        shift: true,
        ..control
    };
    let alt = Modifiers { alt: true, ..none };
    let press = |device: usize, name: &str, typed: Option<&str>, modifiers: Modifiers| {
        for down in [true, false] {
            let key = KeyInput::from_key(name, typed, modifiers, down).unwrap();
            live.send(Command::Key { device, key });
        }
    };
    press(1, "f4", None, alt);
    for (name, modifiers) in [
        ("w", control),
        ("f4", control),
        ("w", control_shift),
        ("f4", alt),
        ("t", control),
        ("n", control),
        ("n", control_shift),
        ("t", control_shift),
        ("u", control),
        ("j", control),
        ("o", control_shift),
        ("delete", control_shift),
        ("a", control_shift),
        ("m", control_shift),
        ("f12", none),
        ("i", control_shift),
        ("j", control_shift),
        ("f5", none),
        ("f5", shift),
        ("f5", control),
        ("r", control),
        ("r", control_shift),
        ("left", alt),
        ("right", alt),
        ("home", alt),
        ("q", control_shift),
    ] {
        press(2, name, None, modifiers);
    }
    press(2, "o", Some("o"), none);
    press(2, "k", Some("k"), none);
    assert!(
        fixture.wait_for(Duration::from_secs(5), |fixture| has_input(
            fixture, "1000", "ok"
        )),
        "typing after the browser keys did not arrive"
    );
    thread::sleep(Duration::from_millis(1500));
    let status = live.session().status();
    assert!(running(&status), "{status:#?}");
    for device in &status.devices {
        assert_eq!(
            (device.url.as_str(), device.error.as_deref()),
            (fixture.url("/keys").as_str(), None)
        );
    }
    let keys: Vec<String> = events(&fixture, "keydown")
        .iter()
        .map(|event| event["key"].clone())
        .collect();
    assert_eq!(keys, ["o", "k"], "browser keys must not reach pages");
    assert_eq!(count(&fixture, "/keys"), 3, "no reload and no view-source");
    assert_eq!(
        count(
            &fixture,
            "/.well-known/appspecific/com.chrome.devtools.json"
        ),
        0,
        "no DevTools"
    );
    assert_eq!(page_targets(live.root.path()), pages, "no hidden tabs");
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
                && !status.devices[0].loading
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
    assert!(
        fixture.wait_for(Duration::from_secs(5), |fixture| {
            let scrolls = events(fixture, "scroll");
            ["360", "600"]
                .iter()
                .all(|width| scrolls.iter().any(|event| event["w"] == *width))
        }),
        "scrolls={:?} status={:?}",
        events(&fixture, "scroll"),
        live.session().status()
    );
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
fn live_link_sync_requires_a_trusted_link_activation() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/script-key")));
    live.wait("script-key pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/script-key")
    });
    live.send(Command::SetSync(SyncSettings {
        navigation: true,
        scroll: false,
    }));
    live.wait("sync on", Duration::from_secs(5), |s| s.sync.navigation);
    click(&live, 0, 50.0, 210.0);
    live.send(Command::Key {
        device: 0,
        key: KeyInput::from_key("a", Some("a"), Modifiers::default(), true).unwrap(),
    });
    live.wait(
        "script click navigates source",
        Duration::from_secs(10),
        |s| s.devices[0].url == fixture.url("/next"),
    );
    thread::sleep(Duration::from_millis(500));
    assert_eq!(
        live.session().status().devices[1].url,
        fixture.url("/script-key")
    );
    assert_eq!(count(&fixture, "/next"), 1);

    live.send(Command::NavigateAll {
        url: fixture.url("/script-pointer"),
    });
    live.wait("script-pointer pages", Duration::from_secs(10), |s| {
        loaded(s, &fixture, "/script-pointer")
    });
    click(&live, 0, 50.0, 300.0);
    live.wait(
        "nonlink click's script navigates source",
        Duration::from_secs(10),
        |s| s.devices[0].url == fixture.url("/next"),
    );
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        live.session().status().devices[1].url,
        fixture.url("/script-pointer")
    );
    assert_eq!(count(&fixture, "/next"), 2);

    live.send(Command::NavigateAll {
        url: fixture.url("/forged"),
    });
    live.wait(
        "forged page only navigates source",
        Duration::from_secs(10),
        |s| s.devices[0].url == fixture.url("/next") && s.devices[1].url == fixture.url("/forged"),
    );
    thread::sleep(Duration::from_millis(500));
    assert_eq!(
        count(&fixture, "/next"),
        3,
        "main-world binding call must not authorize sync"
    );

    live.send(Command::NavigateAll {
        url: fixture.url("/prevent"),
    });
    live.wait("prevent pages", Duration::from_secs(10), |s| {
        loaded(s, &fixture, "/prevent")
    });
    click(&live, 0, 100.0, 120.0);
    live.wait(
        "canceled link led to synthetic link",
        Duration::from_secs(10),
        |s| s.devices[0].url == fixture.url("/prevent-b"),
    );
    thread::sleep(Duration::from_millis(500));
    assert_eq!(
        live.session().status().devices[1].url,
        fixture.url("/prevent")
    );
    assert_eq!(count(&fixture, "/prevent-b"), 1);

    live.send(Command::NavigateAll {
        url: fixture.url("/prevent-same"),
    });
    live.wait("same-href prevent pages", Duration::from_secs(10), |s| {
        loaded(s, &fixture, "/prevent-same")
    });
    click(&live, 0, 100.0, 120.0);
    live.wait(
        "canceled link followed synthetically",
        Duration::from_secs(10),
        |s| s.devices[0].url == fixture.url("/next"),
    );
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        live.session().status().devices[1].url,
        fixture.url("/prevent-same")
    );
    assert_eq!(count(&fixture, "/next"), 4);

    live.send(Command::NavigateAll {
        url: fixture.url("/keyboard"),
    });
    live.wait("keyboard pages", Duration::from_secs(10), |s| {
        loaded(s, &fixture, "/keyboard")
    });
    for down in [true, false] {
        live.send(Command::Key {
            device: 0,
            key: KeyInput::from_key("enter", None, Modifiers::default(), down).unwrap(),
        });
    }
    live.wait(
        "Enter on focused anchor synchronizes",
        Duration::from_secs(10),
        |s| {
            s.devices[0].url == fixture.url("/keyboard-dest")
                && s.devices[1].url == fixture.url("/keyboard-dest")
        },
    );
    assert_eq!(count(&fixture, "/keyboard-dest"), 2);
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_link_sync_binds_slow_commit_and_preserves_long_url() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/slowstart")));
    live.wait("slow pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/slowstart")
    });
    live.send(Command::SetSync(SyncSettings {
        navigation: true,
        scroll: false,
    }));
    live.wait("sync on", Duration::from_secs(5), |s| s.sync.navigation);
    click(&live, 0, 100.0, 120.0);
    live.wait("slow response synchronized", Duration::from_secs(15), |s| {
        s.devices[0].url == fixture.url("/slow")
            && s.devices[1].url == fixture.url("/slow")
            && !s.devices[1].loading
    });
    assert_eq!(count(&fixture, "/slow"), 2);

    live.send(Command::NavigateAll {
        url: fixture.url("/supersede"),
    });
    live.wait("supersede pages", Duration::from_secs(10), |s| {
        loaded(s, &fixture, "/supersede")
    });
    click(&live, 0, 100.0, 120.0);
    live.wait(
        "later script navigation supersedes slow link",
        Duration::from_secs(10),
        |s| s.devices[0].url == fixture.url("/supersede-dest"),
    );
    thread::sleep(Duration::from_millis(3500));
    assert_eq!(
        live.session().status().devices[1].url,
        fixture.url("/supersede")
    );
    assert_eq!(count(&fixture, "/supersede-dest"), 1);

    live.send(Command::NavigateAll {
        url: fixture.url("/longstart"),
    });
    live.wait("long pages", Duration::from_secs(10), |s| {
        loaded(s, &fixture, "/longstart")
    });
    click(&live, 0, 100.0, 120.0);
    live.wait("full long URL on source", Duration::from_secs(10), |s| {
        s.devices[0].url.contains("/long?token=")
    });
    thread::sleep(Duration::from_millis(500));
    let status = live.session().status();
    assert!(
        status.devices[0].url.len() > 2300,
        "observed URL was truncated"
    );
    assert_eq!(status.devices[1].url, fixture.url("/longstart"));
    assert_eq!(
        fixture
            .requests()
            .iter()
            .filter(|r| r.path.starts_with("/long?token="))
            .count(),
        1,
        "overlong URL must never be truncated into a peer request"
    );
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_link_sync_rejects_canceled_and_nested_activations() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/")));
    live.wait("pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/")
    });
    live.send(Command::SetSync(SyncSettings {
        navigation: true,
        scroll: false,
    }));
    live.wait("sync on", Duration::from_secs(5), |s| s.sync.navigation);
    for path in [
        "/prevent-clear",
        "/nested-input",
        "/editable-link",
        "/parent-editable",
        "/nested-button",
    ] {
        live.send(Command::NavigateAll {
            url: fixture.url(path),
        });
        live.wait("adversarial pages", Duration::from_secs(10), |s| {
            loaded(s, &fixture, path)
        });
        let before = count(&fixture, "/next");
        if path == "/nested-input" {
            for down in [true, false] {
                live.send(Command::Key {
                    device: 0,
                    key: KeyInput::from_key("enter", None, Modifiers::default(), down).unwrap(),
                });
            }
        } else {
            let y = if path == "/prevent-clear" {
                120.0
            } else {
                20.0
            };
            click(&live, 0, 50.0, y);
        }
        live.wait(
            "source follows synthetic link",
            Duration::from_secs(10),
            |s| s.devices[0].url == fixture.url("/next"),
        );
        thread::sleep(Duration::from_millis(350));
        assert_eq!(
            live.session().status().devices[1].url,
            fixture.url(path),
            "{path} synchronized"
        );
        if path != "/nested-button" {
            assert_eq!(
                count(&fixture, "/next"),
                before + 1,
                "{path} sent an extra document request"
            );
        }
    }
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn hidden_devices_reject_input_and_sync_without_replay() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/")));
    live.wait("pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/")
    });
    live.send(Command::SetSync(SyncSettings {
        navigation: true,
        scroll: true,
    }));
    live.wait("sync on", Duration::from_secs(5), |s| {
        s.sync.navigation && s.sync.scroll
    });
    click(&live, 0, 40.0, 210.0);
    live.send(Command::SetVisible {
        device: 0,
        visible: false,
    });
    live.wait("phone hidden", Duration::from_secs(5), |s| {
        !s.devices[0].streaming
    });
    let input_before = events(&fixture, "input").len();
    let scroll_before = events(&fixture, "scroll").len();
    live.send(Command::Key {
        device: 0,
        key: KeyInput::from_key("z", Some("z"), Modifiers::default(), true).unwrap(),
    });
    click(&live, 0, 100.0, 120.0);
    live.send(Command::Wheel {
        device: 0,
        x: 100.0,
        y: 300.0,
        delta_x: 0.0,
        delta_y: 400.0,
    });
    live.send(Command::Reload { device: 0 });
    thread::sleep(Duration::from_millis(400));
    assert_eq!(events(&fixture, "input").len(), input_before);
    assert_eq!(events(&fixture, "scroll").len(), scroll_before);
    assert_eq!(count(&fixture, "/next"), 0);
    assert_eq!(
        count(&fixture, "/"),
        3,
        "hidden reload must not request a document"
    );
    live.send(Command::SetVisible {
        device: 0,
        visible: true,
    });
    live.wait("phone visible", Duration::from_secs(5), |s| {
        s.devices[0].streaming
    });
    thread::sleep(Duration::from_millis(400));
    assert_eq!(events(&fixture, "input").len(), input_before);
    assert_eq!(count(&fixture, "/next"), 0);

    live.send(Command::SetVisible {
        device: 1,
        visible: false,
    });
    live.wait("tablet hidden", Duration::from_secs(5), |s| {
        !s.devices[1].streaming
    });
    click(&live, 0, 100.0, 120.0);
    live.wait("phone navigates", Duration::from_secs(10), |s| {
        s.devices[0].url == fixture.url("/next")
    });
    assert_eq!(live.session().status().devices[1].url, fixture.url("/"));
    live.send(Command::SetVisible {
        device: 1,
        visible: true,
    });
    thread::sleep(Duration::from_millis(500));
    assert_eq!(
        live.session().status().devices[1].url,
        fixture.url("/"),
        "showing a hidden peer must not replay navigation"
    );
    live.close();
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

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_navigation_without_response_is_stopped_and_not_retried() {
    // The first three document requests load; the phone's reload then hangs.
    let documents = AtomicUsize::new(0);
    let fixture = Fixture::start(move |request, _| {
        if request.path == "/" && documents.fetch_add(1, Ordering::SeqCst) == 3 {
            return Reply::Hang;
        }
        Reply::Html {
            body: PAGE.replace("AUTO", ""),
            delay: Duration::ZERO,
            cookie: None,
        }
    });
    let limits = Limits {
        load: Duration::from_secs(3),
        ..Limits::default()
    };
    let live = Live::start_with(workspace(fixture.url("/")), limits);
    live.wait("pages", Duration::from_secs(30), |status| {
        loaded(status, &fixture, "/")
    });
    // The deadline starts when Broxser sends the reload.
    let reloaded = Instant::now();
    live.send(Command::Reload { device: 0 });
    assert!(fixture.wait_for(Duration::from_secs(5), |fixture| count(fixture, "/") == 4));
    let others: Vec<u64> = live.session().status().devices[1..]
        .iter()
        .map(|device| device.frames)
        .collect();
    let status = live.wait("a timeout status", Duration::from_secs(10), |status| {
        status.devices[0].error.is_some()
    });
    let elapsed = reloaded.elapsed();
    let error = status.devices[0].error.clone().unwrap();
    assert!(error.contains("loading stopped, not retried"), "{error}");
    assert!(elapsed >= limits.load, "reported after {elapsed:?}");
    // Stopping closed the held request; the phone keeps its page.
    assert!(
        fixture.wait_for(Duration::from_secs(5), |fixture| fixture.abandoned() == 1),
        "the held request stayed open"
    );
    let status = live.wait(
        "the phone stops loading",
        Duration::from_secs(5),
        |status| !status.devices[0].loading,
    );
    assert_eq!(status.devices[0].url, fixture.url("/"));
    assert!(
        status.devices[1..]
            .iter()
            .all(|device| device.error.is_none())
    );
    live.wait(
        "the others keep streaming",
        Duration::from_secs(5),
        |status| {
            status.devices[1..]
                .iter()
                .zip(&others)
                .all(|(device, before)| device.frames > before + 2)
        },
    );
    println!(
        "phone reload stopped after {} ms; held request closed",
        elapsed.as_millis()
    );
    thread::sleep(Duration::from_millis(500));
    assert_eq!(count(&fixture, "/"), 4, "not retried");

    // Only an explicit action loads the page again, once.
    live.send(Command::Reload { device: 0 });
    live.wait("the explicit reload", Duration::from_secs(15), |status| {
        status.devices[0].error.is_none() && !status.devices[0].loading
    });
    assert_eq!(count(&fixture, "/"), 5);
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_busy_page_does_not_stop_other_devices() {
    // The first key press blocks the page's main thread for four seconds.
    let fixture = Fixture::start(|request, _| Reply::Html {
        body: if request.path.starts_with("/event") {
            String::new()
        } else {
            PAGE.replace(
                "AUTO",
                "const field = document.getElementById('field'); field.focus(); let busy = true;
                 field.addEventListener('keydown', () => { if (!busy) return; busy = false;
                   const end = Date.now() + 4000; while (Date.now() < end) {} });",
            )
        },
        delay: Duration::ZERO,
        cookie: None,
    });
    let live = Live::start(workspace(fixture.url("/")));
    live.wait("pages", Duration::from_secs(30), |status| {
        loaded(status, &fixture, "/")
    });
    // The desktop is alone in its session, so its renderer is its own.
    let typed = |presses: usize| {
        for _ in 0..presses {
            for down in [true, false] {
                live.send(Command::Key {
                    device: 2,
                    key: KeyInput::from_key("a", Some("a"), Modifiers::default(), down).unwrap(),
                });
            }
        }
    };
    typed(150);
    let busy = Instant::now();
    let phone = live.session().status().devices[0].frames;
    let status = live.wait(
        "the desktop not responding",
        Duration::from_secs(3),
        |status| status.devices[2].error.as_deref() == Some(NOT_RESPONDING),
    );
    assert!(running(&status), "{status:#?}");
    click(&live, 0, 40.0, 300.0);
    assert!(
        fixture.wait_for(Duration::from_secs(3), |fixture| {
            events(fixture, "down")
                .iter()
                .any(|event| event["w"] == "360")
        }),
        "the phone stopped taking input"
    );
    let status = live.wait("phone frames", Duration::from_secs(3), |status| {
        status.devices[0].frames > phone + 2
    });
    assert!(
        busy.elapsed() < Duration::from_secs(4),
        "the busy loop ended first"
    );
    println!(
        "while the desktop was busy: phone frames +{}, phone click delivered after {} ms",
        status.devices[0].frames - phone,
        busy.elapsed().as_millis()
    );
    // Once the page answers again, the events sent before the limit arrive and
    // the rest were dropped: 16 presses typed, not 150.
    live.wait("the desktop answering", Duration::from_secs(15), |status| {
        status.devices[2].error.is_none()
    });
    let value = |fixture: &Fixture| {
        events(fixture, "input")
            .iter()
            .filter(|event| event["w"] == "1000")
            .map(|event| event["v"].len())
            .max()
    };
    assert!(
        fixture.wait_for(Duration::from_secs(5), |fixture| value(fixture)
            == Some(MAX_UNANSWERED_INPUT / 2))
    );
    thread::sleep(Duration::from_millis(500));
    assert_eq!(value(&fixture), Some(MAX_UNANSWERED_INPUT / 2));
    typed(1);
    assert!(
        fixture.wait_for(Duration::from_secs(5), |fixture| value(fixture)
            == Some(MAX_UNANSWERED_INPUT / 2 + 1)),
        "input did not resume"
    );
    assert!(running(&live.session().status()));
    live.close();
}
