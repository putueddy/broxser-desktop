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

#[path = "touch_input.rs"]
mod touch_input;

#[path = "qa_fidelity.rs"]
mod qa_fidelity;

#[path = "worker_targets.rs"]
mod worker_targets;

#[path = "private_home.rs"]
mod private_home;

#[path = "certificate_errors.rs"]
mod certificate_errors;

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

/// Method, session, command ID and parameters of every request, in arrival order.
type Requests = Arc<Mutex<Vec<(String, Option<String>, u64, Value)>>>;

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
                    request["params"].clone(),
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
                        let mut result = json!({
                            "frameId": session.as_deref().unwrap_or_default().replacen('S', "T", 1),
                            "loaderId": format!("L{loaders}")
                        });
                        // Helium's answer when the address is a download.
                        if request["params"]["url"]
                            .as_str()
                            .is_some_and(|url| url.contains("/download"))
                        {
                            result["errorText"] = json!("net::ERR_ABORTED");
                            result["isDownload"] = json!(true);
                        }
                        result
                    }
                    "Runtime.evaluate" => json!({"result":{"type":"number","value":1}}),
                    // A complete 2 × 3 PNG for every device but the tablet,
                    // whose reply is no PNG.
                    "Page.captureScreenshot" if session.as_deref() == Some("S1") => {
                        json!({"data": "bm90IGEgcG5n"})
                    }
                    "Page.captureScreenshot" => {
                        json!({"data": base64::engine::general_purpose::STANDARD.encode(test_screenshot_png())})
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

    /// Parameters of every `method` sent to the browser itself, not a session.
    fn browser_requests(&self, method: &str) -> Vec<Value> {
        self.received
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, s, _, _)| m == method && s.is_none())
            .map(|(_, _, _, params)| params.clone())
            .collect()
    }

    /// Requests received so far for `method` on `session`.
    fn count(&self, method: &str, session: &str) -> usize {
        self.received
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, s, _, _)| m == method && s.as_deref() == Some(session))
            .count()
    }

    fn last_request_id(&self, method: &str, session: &str) -> u64 {
        self.received
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(m, s, _, _)| m == method && s.as_deref() == Some(session))
            .unwrap()
            .2
    }

    /// Parameters of the latest `method` sent to `session`.
    fn last_params(&self, method: &str, session: &str) -> Value {
        self.received
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(m, s, _, _)| m == method && s.as_deref() == Some(session))
            .map(|(_, _, _, params)| params.clone())
            .unwrap_or(Value::Null)
    }

    /// The `url` parameter of every `Page.navigate` sent to `session`.
    fn navigations(&self, session: &str) -> Vec<String> {
        self.received
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, s, _, _)| m == "Page.navigate" && s.as_deref() == Some(session))
            .map(|(_, _, _, params)| params["url"].as_str().unwrap_or_default().to_owned())
            .collect()
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

/// Waits until the runtime has handled the events sent so far and the replies,
/// not held, to requests `peer` received: the peer sends them before this
/// phone frame, and the runtime handles messages in order.
fn wait_until_read(peer: &FakePeer) {
    let acks = peer.count("Page.screencastFrameAck", "S0");
    peer.event(phone("Page.screencastFrame", json!({"sessionId": 1})));
    wait_for_requests(peer, "Page.screencastFrameAck", "S0", acks + 1);
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

#[test]
fn ime_bindings_require_the_main_frame_isolated_context_and_live_token() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = fake_live(root.path(), Limits::default());
    let report = |anchor| {
        json!({"method":"Runtime.bindingCalled","sessionId":"S0","params":{
            "name":IME_BINDING,"executionContextId":42,
            "payload":format!("{{\"active\":true,\"anchor\":{anchor},\"x\":20,\"y\":40,\"width\":1,\"height\":18}}")
        }})
    };
    peer.event(report(1));
    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        live.status().devices[0].text_input,
        None,
        "unregistered binding"
    );
    peer.event(
        json!({"method":"Runtime.executionContextCreated","sessionId":"S0","params":{"context":{
            "id":42,"name":IME_WORLD,"auxData":{"frameId":"not-main","type":"isolated"}
        }}}),
    );
    peer.event(report(1));
    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        live.status().devices[0].text_input,
        None,
        "subframe context"
    );
    peer.event(
        json!({"method":"Runtime.executionContextCreated","sessionId":"S0","params":{"context":{
            "id":42,"name":IME_WORLD,"auxData":{"frameId":"T0","type":"isolated"}
        }}}),
    );
    peer.event(json!({"method":"Runtime.bindingCalled","sessionId":"S0","params":{
        "name":IME_BINDING,"executionContextId":42,"payload":"{\"active\":true,\"anchor\":1,\"x\":1e400,\"y\":40,\"width\":1,\"height\":18}"
    }}));
    assert_eq!(
        live.status().devices[0].text_input,
        None,
        "malformed geometry"
    );
    peer.event(report(1));
    let first = wait_for(&live, "valid IME caret", Duration::from_secs(2), |s| {
        s.devices[0].text_input.is_some()
    })
    .devices[0]
        .text_input
        .unwrap();
    assert!(live.send(Command::Ime {
        device: 0,
        target: first.target,
        action: ImeAction::Commit { text: "a".into() }
    }));
    wait_for_requests(&peer, "Input.insertText", "S0", 1);
    assert_eq!(peer.count("Input.insertText", "S1"), 0);

    peer.event(report(2));
    let second = wait_for(&live, "new anchor", Duration::from_secs(2), |s| {
        s.devices[0]
            .text_input
            .is_some_and(|state| state.target != first.target)
    })
    .devices[0]
        .text_input
        .unwrap();
    assert!(live.send(Command::Ime {
        device: 0,
        target: first.target,
        action: ImeAction::Commit {
            text: "stale".into()
        }
    }));
    assert!(live.send(Command::Ime {
        device: 0,
        target: second.target,
        action: ImeAction::Commit {
            text: "changed".into()
        }
    }));
    wait_for(
        &live,
        "changed anchor rejected by immediate read",
        Duration::from_secs(2),
        |s| s.devices[0].text_input.is_none(),
    );
    assert_eq!(peer.count("Input.insertText", "S0"), 1);

    peer.event(report(1));
    wait_for(&live, "caret restored", Duration::from_secs(2), |s| {
        s.devices[0].text_input.is_some()
    });
    assert!(live.send(Command::SetVisible {
        device: 0,
        visible: false
    }));
    wait_for(&live, "hidden caret cleared", Duration::from_secs(2), |s| {
        s.devices[0].text_input.is_none()
    });
    assert_eq!(peer.count("Input.insertText", "S0"), 1);
    drop(live);
}

/// An event of the phone: session `S0`, main frame `T0`.
fn phone(method: &str, params: Value) -> Value {
    json!({"method": method, "sessionId": "S0", "params": params})
}

/// An event of the tablet, which shares the phone's session: session `S1`,
/// main frame `T1`.
fn tablet(method: &str, params: Value) -> Value {
    json!({"method": method, "sessionId": "S1", "params": params})
}

/// A report of the phone's link observer, registered by [`link_observer`].
fn link_report(phase: &str, id: u64, url: &str) -> Value {
    phone(
        "Runtime.bindingCalled",
        json!({
            "name": LINK_BINDING,
            "executionContextId": 7,
            "payload": json!({"phase": phase, "id": id, "url": url}).to_string(),
        }),
    )
}

/// Registers the phone's link observer world and turns navigation sync on.
fn link_observer(peer: &FakePeer, live: &LiveSession) {
    peer.event(phone(
        "Runtime.executionContextCreated",
        json!({"context": {"id": 7, "name": LINK_WORLD, "auxData": {"frameId": "T0", "type": "isolated"}}}),
    ));
    assert!(live.send(Command::SetSync(SyncSettings {
        navigation: true,
        scroll: false,
    })));
    wait_for(live, "sync on", Duration::from_secs(2), |s| {
        s.sync.navigation
    });
}

/// A trusted click on a link to `url` that starts `loader`, in the order in
/// which Helium reports it (P1.5 in `docs/validation.md`).
fn follow_link(peer: &FakePeer, id: u64, url: &str, loader: &str) {
    peer.event(link_report("C", id, url));
    peer.event(phone(
        "Page.frameRequestedNavigation",
        json!({"frameId": "T0", "reason": "anchorClick", "disposition": "currentTab", "url": url}),
    ));
    peer.event(link_report("Y", id, url));
    peer.event(phone(
        "Page.frameStartedNavigating",
        json!({"frameId": "T0", "loaderId": loader, "navigationType": "differentDocument", "url": url}),
    ));
}

fn commit(loader: &str, url: &str, extra: Value) -> Value {
    let mut frame = json!({"id": "T0", "loaderId": loader, "url": url});
    if let (Some(frame), Some(extra)) = (frame.as_object_mut(), extra.as_object()) {
        frame.extend(extra.clone());
    }
    phone("Page.frameNavigated", json!({"frame": frame}))
}

#[test]
fn redirected_link_commit_synchronizes_the_link_and_keeps_fragments() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = fake_live(root.path(), Limits::default());
    link_observer(&peer, &live);
    let origin = "http://127.0.0.1:4173";

    // The server redirects the link's loader to another URL with a fragment.
    follow_link(&peer, 1, &format!("{origin}/moved"), "L9");
    peer.event(commit(
        "L9",
        &format!("{origin}/landed"),
        json!({"urlFragment": "#part"}),
    ));
    wait_for(&live, "the full URL", Duration::from_secs(2), |s| {
        s.devices[0].url == format!("{origin}/landed#part")
    });
    wait_for_requests(&peer, "Page.navigate", "S1", 2);
    assert_eq!(
        peer.navigations("S1")[1],
        format!("{origin}/moved"),
        "the tablet follows the link, not the phone's redirect target"
    );

    // `frame.url` of a link to another document's fragment lacks the fragment.
    follow_link(&peer, 2, &format!("{origin}/next#part"), "L10");
    peer.event(commit(
        "L10",
        &format!("{origin}/next"),
        json!({"urlFragment": "#part"}),
    ));
    wait_for_requests(&peer, "Page.navigate", "S1", 3);
    assert_eq!(peer.navigations("S1")[2], format!("{origin}/next#part"));

    // An error page for the link's loader and a document of another loader
    // are not destinations of the link.
    follow_link(&peer, 3, &format!("{origin}/down"), "L11");
    peer.event(commit(
        "L11",
        "chrome-error://chromewebdata/",
        json!({"unreachableUrl": format!("{origin}/down")}),
    ));
    follow_link(&peer, 4, &format!("{origin}/slow"), "L12");
    peer.event(commit("L13", &format!("{origin}/elsewhere"), json!({})));
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        peer.navigations("S1").len(),
        3,
        "{:?}",
        peer.navigations("S1")
    );
    assert_eq!(peer.navigations("S2").len(), 1, "sync left its session");
    drop(live);
}

#[test]
fn same_document_link_navigation_follows_only_a_live_activation() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = fake_live(
        root.path(),
        Limits {
            link_follow: Duration::from_millis(500),
            ..Limits::default()
        },
    );
    link_observer(&peer, &live);
    let origin = "http://127.0.0.1:4173";
    let within = |frame: &str, path: &str, kind: &str| {
        phone(
            "Page.navigatedWithinDocument",
            json!({"frameId": frame, "url": format!("{origin}{path}"), "navigationType": kind}),
        )
    };
    let activate =
        |id: u64, path: &str| peer.event(link_report("C", id, &format!("{origin}{path}")));

    // A router saves state on the current entry, then pushes the link's URL.
    activate(1, "/spa");
    peer.event(within("T0", "/", "historyApi"));
    peer.event(within("T0", "/spa", "historyApi"));
    wait_for_requests(&peer, "Page.navigate", "S1", 2);
    assert_eq!(peer.navigations("S1")[1], format!("{origin}/spa"));
    wait_for(&live, "the phone's route", Duration::from_secs(2), |s| {
        s.devices[0].url == format!("{origin}/spa")
    });

    // An activation counts once.
    peer.event(within("T0", "/spa", "historyApi"));
    // A subframe's URL change is not the page's.
    activate(2, "/frame");
    peer.event(within("F1", "/frame", "historyApi"));
    // A subframe's observer is not the main frame's.
    peer.event(phone(
        "Runtime.executionContextCreated",
        json!({"context": {"id": 8, "name": LINK_WORLD, "auxData": {"frameId": "F1", "type": "isolated"}}}),
    ));
    peer.event(phone(
        "Runtime.bindingCalled",
        json!({"name": LINK_BINDING, "executionContextId": 8,
            "payload": json!({"phase": "C", "id": 3, "url": format!("{origin}/sub")}).to_string()}),
    ));
    peer.event(within("T0", "/sub", "historyApi"));
    // A newer activation replaces an older one.
    activate(4, "/a");
    activate(5, "/b");
    peer.event(within("T0", "/a", "historyApi"));
    // An activation does not outlive its document.
    activate(6, "/c");
    peer.event(commit("L20", &format!("{origin}/other"), json!({})));
    peer.event(within("T0", "/c", "historyApi"));
    // It expires.
    activate(7, "/d");
    thread::sleep(Duration::from_millis(700));
    peer.event(within("T0", "/d", "historyApi"));
    // Hiding the device ends it.
    activate(8, "/e");
    thread::sleep(Duration::from_millis(50));
    assert!(live.send(Command::SetVisible {
        device: 0,
        visible: false,
    }));
    wait_for_requests(&peer, "Page.stopScreencast", "S0", 1);
    peer.event(within("T0", "/e", "historyApi"));
    assert!(live.send(Command::SetVisible {
        device: 0,
        visible: true,
    }));
    wait_for_requests(&peer, "Page.startScreencast", "S0", 2);
    // So does switching sync on after it.
    assert!(live.send(Command::SetSync(SyncSettings::default())));
    wait_for(&live, "sync off", Duration::from_secs(2), |s| {
        !s.sync.navigation
    });
    activate(9, "/f");
    thread::sleep(Duration::from_millis(50));
    assert!(live.send(Command::SetSync(SyncSettings {
        navigation: true,
        scroll: false,
    })));
    wait_for(&live, "sync on", Duration::from_secs(2), |s| {
        s.sync.navigation
    });
    peer.event(within("T0", "/f", "historyApi"));
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        peer.navigations("S1").len(),
        2,
        "{:?}",
        peer.navigations("S1")
    );

    // A hash link: the fragment navigation follows a fresh activation.
    activate(10, "/other#part");
    peer.event(within("T0", "/other#part", "fragment"));
    wait_for_requests(&peer, "Page.navigate", "S1", 3);
    assert_eq!(peer.navigations("S1")[2], format!("{origin}/other#part"));
    assert_eq!(peer.navigations("S2").len(), 1, "sync left its session");
    drop(live);
}

#[test]
fn dialog_blocks_input_and_navigation_until_the_user_answers() {
    let root = profile_root();
    // The phone's page never answers pointer input, like the click that
    // opened its dialog and the input Chromium holds behind the dialog.
    let peer = FakePeer::start(root.path(), |method, session| {
        method == "Input.dispatchMouseEvent" && session == Some("S0")
    });
    let live = fake_live(
        root.path(),
        Limits {
            command: Duration::from_secs(1),
            ..Limits::default()
        },
    );
    let opening = |kind: &str, message: &str, default: &str| {
        phone(
            "Page.javascriptDialogOpening",
            json!({"url": "http://127.0.0.1:4173/", "message": message, "type": kind,
                "hasBrowserHandler": true, "defaultPrompt": default}),
        )
    };
    let closed = |accepted: bool| {
        phone(
            "Page.javascriptDialogClosed",
            json!({"result": accepted, "userInput": ""}),
        )
    };
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
    let long = "m".repeat(MAX_DIALOG_CHARS + 10);
    peer.event(opening("confirm", &format!("Continue?\u{7}\n{long}"), ""));
    let dialog = wait_for(&live, "the dialog", Duration::from_secs(2), |s| {
        s.devices[0].dialog.is_some()
    })
    .devices[0]
        .dialog
        .clone()
        .unwrap();
    assert_eq!(dialog.kind, DialogKind::Confirm);
    assert!(
        dialog.message.starts_with("Continue?\nmmm"),
        "{}",
        dialog.message
    );
    assert_eq!(dialog.message.chars().count(), MAX_DIALOG_CHARS + 1);
    assert!(dialog.message.ends_with('…'));
    assert_eq!(dialog.default_text, "");

    // The unanswered click is the dialog's, not a page that stopped responding.
    thread::sleep(Duration::from_millis(1400));
    assert_eq!(live.status().devices[0].error, None);
    // Keys and text are dropped, not held for the page behind the dialog.
    send_keys(&live, 0, 2);
    assert!(live.send(Command::InsertText {
        device: 0,
        text: "x".into(),
    }));
    thread::sleep(Duration::from_millis(200));
    assert_eq!(peer.count("Input.dispatchKeyEvent", "S0"), 0);
    assert_eq!(peer.count("Input.insertText", "S0"), 0);
    // Go reaches the other devices; navigating this one would cancel the dialog.
    assert!(live.send(Command::NavigateAll {
        url: "http://127.0.0.1:4173/next".into(),
    }));
    wait_for_requests(&peer, "Page.navigate", "S1", 2);
    wait_for_requests(&peer, "Page.navigate", "S2", 2);
    let status = wait_for(&live, "the refusal", Duration::from_secs(2), |s| {
        s.devices[0].error.is_some()
    });
    assert_eq!(status.devices[0].error.as_deref(), Some(DIALOG_OPEN));
    assert_eq!(peer.count("Page.navigate", "S0"), 1);
    assert!(status.devices[0].dialog.is_some());

    // Only the current dialog's token answers it.
    assert!(live.send(Command::AnswerDialog {
        device: 0,
        token: dialog.token + 1,
        accept: true,
        text: None,
    }));
    thread::sleep(Duration::from_millis(200));
    assert_eq!(peer.count("Page.handleJavaScriptDialog", "S0"), 0);
    assert!(live.send(Command::AnswerDialog {
        device: 0,
        token: dialog.token,
        accept: false,
        text: Some("ignored for a confirm".into()),
    }));
    wait_for_requests(&peer, "Page.handleJavaScriptDialog", "S0", 1);
    assert_eq!(
        peer.last_params("Page.handleJavaScriptDialog", "S0"),
        json!({"accept": false})
    );
    // The browser's report closes it, and the refusal with it.
    peer.event(closed(false));
    wait_for(&live, "the dialog to close", Duration::from_secs(2), |s| {
        s.devices[0].dialog.is_none() && s.devices[0].error.is_none()
    });
    peer.release();
    send_keys(&live, 0, 1);
    wait_for_requests(&peer, "Input.dispatchKeyEvent", "S0", 2);

    // A prompt carries its default and takes the user's text only when accepted.
    peer.event(opening("prompt", "Your name?", "guest"));
    let prompt = wait_for(&live, "the prompt", Duration::from_secs(2), |s| {
        s.devices[0].dialog.is_some()
    })
    .devices[0]
        .dialog
        .clone()
        .unwrap();
    assert_eq!(prompt.kind, DialogKind::Prompt);
    assert_eq!(prompt.default_text, "guest");
    assert_ne!(prompt.token, dialog.token);
    assert!(live.send(Command::AnswerDialog {
        device: 0,
        token: prompt.token,
        accept: true,
        text: Some("Broxser".into()),
    }));
    wait_for_requests(&peer, "Page.handleJavaScriptDialog", "S0", 2);
    assert_eq!(
        peer.last_params("Page.handleJavaScriptDialog", "S0"),
        json!({"accept": true, "promptText": "Broxser"})
    );
    peer.event(closed(true));
    wait_for(&live, "the prompt to close", Duration::from_secs(2), |s| {
        s.devices[0].dialog.is_none()
    });
    assert_eq!(
        peer.count("Page.navigate", "S0"),
        1,
        "nothing navigated for the user"
    );
    drop(live);
}

/// A JavaScript dialog of `kind` opening on the phone.
fn dialog_opening(kind: &str, message: &str, default: &str) -> Value {
    phone(
        "Page.javascriptDialogOpening",
        json!({"url": "http://127.0.0.1:4173/", "message": message, "type": kind,
            "hasBrowserHandler": true, "defaultPrompt": default}),
    )
}

fn dialog_closed(accepted: bool) -> Value {
    phone(
        "Page.javascriptDialogClosed",
        json!({"result": accepted, "userInput": ""}),
    )
}

/// Waits for a dialog on the phone, which has none open before.
fn phone_dialog(live: &LiveSession) -> DialogState {
    wait_for(live, "the dialog", Duration::from_secs(2), |s| {
        s.devices[0].dialog.is_some()
    })
    .devices[0]
        .dialog
        .clone()
        .unwrap()
}

fn answer_phone(live: &LiveSession, token: u64, accept: bool, text: Option<&str>) {
    assert!(live.send(Command::AnswerDialog {
        device: 0,
        token,
        accept,
        text: text.map(str::to_owned),
    }));
}

/// A fake peer that holds every phone navigation after the workspace's
/// first, like a server that has not answered yet.
fn peer_holding_phone_navigations(root: &Path) -> FakePeer {
    let navigations = AtomicUsize::new(0);
    FakePeer::start(root, move |method, session| {
        method == "Page.navigate"
            && session == Some("S0")
            && navigations.fetch_add(1, Ordering::SeqCst) > 0
    })
}

/// The answer to the phone's latest `Page.navigate`, failed with `error`.
fn failed_navigate_reply(peer: &FakePeer, error: &str) -> Value {
    json!({"id": peer.last_request_id("Page.navigate", "S0"), "result": {
        "frameId": "T0", "loaderId": "held", "errorText": error
    }})
}

#[test]
fn dialog_text_and_prompt_defaults_are_bounded() {
    let full = "x".repeat(MAX_DIALOG_CHARS);
    assert_eq!(dialog_text(&full), full);
    assert_eq!(dialog_text(&format!("{full}y")), format!("{full}…"));
    assert_eq!(dialog_text("a\u{7}\nb\tc\r"), "a\nb\tc");
    // A prompt's default is an answer: one line, cut without a marker.
    assert_eq!(prompt_text(&format!("{full}y")), full);
    assert_eq!(prompt_text("a\u{7}\nb\tc\r\n"), "a b c ");
}

#[test]
fn showing_a_device_during_its_dialog_waits_for_nothing_the_page_answers() {
    let root = profile_root();
    // The renderer answers Runtime.evaluate; while a dialog is open it does not.
    let peer = FakePeer::start(root.path(), |method, session| {
        method == "Runtime.evaluate" && session == Some("S0")
    });
    let limits = Limits {
        command: Duration::from_secs(1),
        ..Limits::default()
    };
    let live = fake_live(root.path(), limits);
    peer.event(phone(
        "Runtime.executionContextCreated",
        json!({"context": {"id": 42, "name": IME_WORLD, "auxData": {"frameId": "T0", "type": "isolated"}}}),
    ));
    assert!(live.send(Command::SetVisible {
        device: 0,
        visible: false
    }));
    wait_for(&live, "the phone hidden", Duration::from_secs(2), |s| {
        !s.devices[0].streaming
    });
    peer.event(dialog_opening("alert", "hello", ""));
    let alert = phone_dialog(&live);
    assert!(live.send(Command::SetVisible {
        device: 0,
        visible: true
    }));
    wait_for(&live, "the phone shown", Duration::from_secs(2), |s| {
        s.devices[0].streaming
    });
    // The browser answers the stream and input commands; nothing waits for
    // the page past the command limit.
    thread::sleep(limits.command + Duration::from_millis(500));
    let status = live.status();
    assert!(running(&status), "{:?}", status.runtime);
    assert_eq!(
        status.devices[0].dialog.as_ref().map(|d| d.token),
        Some(alert.token)
    );
    assert_eq!(peer.count("Runtime.evaluate", "S0"), 0);
    // The closed dialog's caret is asked for again, without a deadline:
    // another dialog could hold that request too.
    answer_phone(&live, alert.token, true, None);
    wait_for_requests(&peer, "Page.handleJavaScriptDialog", "S0", 1);
    peer.event(dialog_closed(true));
    wait_for_requests(&peer, "Runtime.evaluate", "S0", 1);
    let refresh = peer.last_params("Runtime.evaluate", "S0");
    assert_eq!(refresh["contextId"], 42);
    assert!(
        refresh["expression"]
            .as_str()
            .is_some_and(|expression| expression.contains("__broxserImeRefresh")),
        "{refresh}"
    );
    thread::sleep(limits.command + Duration::from_millis(500));
    assert!(running(&live.status()));
    drop(live);
}

#[test]
fn prompt_default_can_be_sent_back_and_rejected_answers_are_reported() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = fake_live(root.path(), Limits::default());
    let long = "d".repeat(3000);
    peer.event(dialog_opening(
        "prompt",
        "Your name?",
        &format!("first\nsecond\tthird\u{7}{long}"),
    ));
    let prompt = phone_dialog(&live);
    // The page's proposal is shown as an answer Broxser can send unchanged.
    let expected: String = format!("first second third{long}")
        .chars()
        .take(MAX_DIALOG_CHARS)
        .collect();
    assert_eq!(prompt.default_text, expected);
    // A longer answer is not sent: the dialog stays and the device says why.
    answer_phone(
        &live,
        prompt.token,
        true,
        Some(&"a".repeat(MAX_DIALOG_CHARS + 1)),
    );
    let status = wait_for(&live, "the rejection", Duration::from_secs(2), |s| {
        s.devices[0].error.is_some()
    });
    assert_eq!(status.devices[0].error.as_deref(), Some(PROMPT_REJECTED));
    assert!(PROMPT_REJECTED.contains(&MAX_DIALOG_CHARS.to_string()));
    assert_eq!(
        status.devices[0].dialog.as_ref().map(|d| d.token),
        Some(prompt.token)
    );
    // The unchanged default is sent as it is; the sent answer clears the report.
    answer_phone(&live, prompt.token, true, Some(&prompt.default_text));
    wait_for_requests(&peer, "Page.handleJavaScriptDialog", "S0", 1);
    assert_eq!(
        peer.last_params("Page.handleJavaScriptDialog", "S0"),
        json!({"accept": true, "promptText": expected})
    );
    wait_for(&live, "the report cleared", Duration::from_secs(2), |s| {
        s.devices[0].error.is_none()
    });
    peer.event(dialog_closed(true));
    wait_for(&live, "the prompt to close", Duration::from_secs(2), |s| {
        s.devices[0].dialog.is_none()
    });

    // Control characters are refused too, and the closing clears the report.
    peer.event(dialog_opening("prompt", "Again?", ""));
    let prompt = phone_dialog(&live);
    assert_eq!(prompt.default_text, "");
    answer_phone(&live, prompt.token, true, Some("two\nlines"));
    wait_for(&live, "the rejection", Duration::from_secs(2), |s| {
        s.devices[0].error.as_deref() == Some(PROMPT_REJECTED)
    });
    peer.event(dialog_closed(false));
    let status = wait_for(&live, "the prompt to close", Duration::from_secs(2), |s| {
        s.devices[0].dialog.is_none()
    });
    assert_eq!(status.devices[0].error, None);
    assert_eq!(peer.count("Page.handleJavaScriptDialog", "S0"), 1);
    drop(live);
}

#[test]
fn a_dialog_takes_one_answer_and_never_the_previous_dialogs() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = fake_live(root.path(), Limits::default());
    peer.event(dialog_opening("confirm", "Continue?", ""));
    let first = phone_dialog(&live);
    // `Page.handleJavaScriptDialog` answers whatever dialog is showing; until
    // the closing is read, a second answer could reach the page's next one.
    answer_phone(&live, first.token, false, None);
    answer_phone(&live, first.token, true, None);
    wait_for_requests(&peer, "Page.handleJavaScriptDialog", "S0", 1);
    thread::sleep(Duration::from_millis(200));
    assert_eq!(peer.count("Page.handleJavaScriptDialog", "S0"), 1);
    assert_eq!(
        peer.last_params("Page.handleJavaScriptDialog", "S0"),
        json!({"accept": false})
    );
    peer.event(dialog_closed(false));
    wait_for(&live, "the dialog to close", Duration::from_secs(2), |s| {
        s.devices[0].dialog.is_none()
    });
    peer.event(dialog_opening("confirm", "Really?", ""));
    let second = phone_dialog(&live);
    assert_ne!(second.token, first.token);
    answer_phone(&live, first.token, true, None);
    thread::sleep(Duration::from_millis(200));
    assert_eq!(peer.count("Page.handleJavaScriptDialog", "S0"), 1);
    answer_phone(&live, second.token, true, None);
    wait_for_requests(&peer, "Page.handleJavaScriptDialog", "S0", 2);
    assert_eq!(
        peer.last_params("Page.handleJavaScriptDialog", "S0"),
        json!({"accept": true})
    );
    drop(live);
}

#[test]
fn a_dialog_answer_the_browser_refuses_can_be_sent_again() {
    let root = profile_root();
    // The test gives the browser's replies to the answers.
    let peer = FakePeer::start(root.path(), |method, _| {
        method == "Page.handleJavaScriptDialog"
    });
    let live = fake_live(root.path(), Limits::default());
    peer.event(dialog_opening("confirm", "Continue?", ""));
    let dialog = phone_dialog(&live);
    answer_phone(&live, dialog.token, true, None);
    wait_for_requests(&peer, "Page.handleJavaScriptDialog", "S0", 1);
    // An error while the dialog stays open takes the answer back.
    peer.event(json!({
        "id": peer.last_request_id("Page.handleJavaScriptDialog", "S0"),
        "error": {"code": -32000, "message": "Could not handle the dialog"}
    }));
    let status = wait_for(&live, "the error", Duration::from_secs(2), |s| {
        s.protocol_error.is_some()
    });
    assert_eq!(
        status.protocol_error.as_deref(),
        Some("Could not handle the dialog")
    );
    assert_eq!(
        status.devices[0].dialog.as_ref().map(|d| d.token),
        Some(dialog.token)
    );
    answer_phone(&live, dialog.token, false, None);
    wait_for_requests(&peer, "Page.handleJavaScriptDialog", "S0", 2);
    assert_eq!(
        peer.last_params("Page.handleJavaScriptDialog", "S0"),
        json!({"accept": false})
    );
    // A successful answer is the dialog's only one, also before the browser
    // reports it closed.
    peer.event(json!({
        "id": peer.last_request_id("Page.handleJavaScriptDialog", "S0"),
        "result": {}
    }));
    wait_until_read(&peer);
    answer_phone(&live, dialog.token, true, None);
    // Commands run in order: once the tablet's key arrives, the answer was handled.
    send_keys(&live, 1, 1);
    wait_for_requests(&peer, "Input.dispatchKeyEvent", "S1", 2);
    assert_eq!(peer.count("Page.handleJavaScriptDialog", "S0"), 2);
    drop(live);
}

#[test]
fn staying_ends_the_broxser_navigation_that_asked_in_either_reply_order() {
    for reply_first in [true, false] {
        let root = profile_root();
        let peer = peer_holding_phone_navigations(root.path());
        let limits = Limits {
            load: Duration::from_millis(600),
            ..Limits::default()
        };
        let live = fake_live(root.path(), limits);
        assert!(live.send(Command::NavigateAll {
            url: "http://127.0.0.1:4173/next".into(),
        }));
        wait_for_requests(&peer, "Page.navigate", "S0", 2);
        peer.event(dialog_opening("beforeunload", "", ""));
        let question = phone_dialog(&live);
        // The question pauses the navigation's deadline.
        thread::sleep(limits.load + Duration::from_millis(200));
        assert_eq!(peer.count("Page.stopLoading", "S0"), 0);
        answer_phone(&live, question.token, false, None);
        wait_for_requests(&peer, "Page.handleJavaScriptDialog", "S0", 1);
        // Helium answers the navigation with net::ERR_ABORTED, before or
        // after it reports the closing.
        let aborted = failed_navigate_reply(&peer, "net::ERR_ABORTED");
        if reply_first {
            peer.event(aborted.clone());
        }
        peer.event(dialog_closed(false));
        if !reply_first {
            peer.event(aborted);
        }
        wait_for(&live, "stayed", Duration::from_secs(2), |s| {
            s.devices[0].dialog.is_none() && !s.devices[0].loading
        });
        // No failure, then or at the deadline: the navigation is over.
        thread::sleep(limits.load + Duration::from_millis(200));
        let status = live.status();
        assert!(running(&status));
        assert_eq!(status.devices[0].error, None, "reply_first={reply_first}");
        assert!(!status.devices[0].loading, "reply_first={reply_first}");
        assert_eq!(
            peer.count("Page.stopLoading", "S0"),
            0,
            "reply_first={reply_first}"
        );
        drop(live);
    }
}

#[test]
fn staying_ends_a_reload_that_asked() {
    let root = profile_root();
    // Helium answers Page.reload at once, after the reload's loader started.
    let peer = FakePeer::start_with_events(
        root.path(),
        |_, _| false,
        |method, session| {
            if method == "Page.reload" && session == Some("S0") {
                vec![phone(
                    "Page.frameStartedNavigating",
                    json!({"frameId": "T0", "loaderId": "reload", "navigationType": "reload",
                        "url": "http://127.0.0.1:4173/"}),
                )]
            } else {
                Vec::new()
            }
        },
    );
    let limits = Limits {
        load: Duration::from_millis(600),
        ..Limits::default()
    };
    let live = fake_live(root.path(), limits);
    assert!(live.send(Command::Reload { device: 0 }));
    wait_for_requests(&peer, "Page.reload", "S0", 1);
    peer.event(dialog_opening("beforeunload", "", ""));
    let question = phone_dialog(&live);
    answer_phone(&live, question.token, false, None);
    wait_for_requests(&peer, "Page.handleJavaScriptDialog", "S0", 1);
    // Helium also reports that the reload's loader stopped, which ends the
    // reload by itself; the answer alone must end it too.
    peer.event(dialog_closed(false));
    wait_for(&live, "stayed", Duration::from_secs(2), |s| {
        s.devices[0].dialog.is_none() && !s.devices[0].loading
    });
    thread::sleep(limits.load + Duration::from_millis(200));
    let status = live.status();
    assert_eq!(status.devices[0].error, None);
    assert!(!status.devices[0].loading);
    assert_eq!(peer.count("Page.stopLoading", "S0"), 0);
    assert_eq!(peer.count("Page.reload", "S0"), 1, "never retried");
    drop(live);
}

#[test]
fn leaving_continues_the_navigation_under_a_fresh_deadline() {
    let root = profile_root();
    let peer = peer_holding_phone_navigations(root.path());
    let limits = Limits {
        load: Duration::from_millis(800),
        ..Limits::default()
    };
    let live = fake_live(root.path(), limits);
    assert!(live.send(Command::NavigateAll {
        url: "http://127.0.0.1:4173/next".into(),
    }));
    wait_for_requests(&peer, "Page.navigate", "S0", 2);
    peer.event(dialog_opening("beforeunload", "", ""));
    let question = phone_dialog(&live);
    thread::sleep(limits.load + Duration::from_millis(200));
    answer_phone(&live, question.token, true, None);
    wait_for_requests(&peer, "Page.handleJavaScriptDialog", "S0", 1);
    peer.event(dialog_closed(true));
    let status = wait_for(&live, "left", Duration::from_secs(2), |s| {
        s.devices[0].dialog.is_none()
    });
    assert!(status.devices[0].loading);
    assert_eq!(status.devices[0].error, None);
    // The load limit starts again when the dialog closes, and still applies.
    thread::sleep(Duration::from_millis(300));
    assert_eq!(peer.count("Page.stopLoading", "S0"), 0);
    wait_for_requests(&peer, "Page.stopLoading", "S0", 1);
    let status = wait_for(&live, "the deadline", Duration::from_secs(2), |s| {
        s.devices[0].error.is_some()
    });
    assert!(
        status.devices[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("loading stopped"))
    );
    assert_eq!(peer.count("Page.navigate", "S0"), 2, "never retried");
    drop(live);
}

#[test]
fn staying_on_the_pages_own_link_keeps_the_broxser_navigation() {
    let root = profile_root();
    let peer = peer_holding_phone_navigations(root.path());
    let limits = Limits {
        load: Duration::from_millis(800),
        ..Limits::default()
    };
    let live = fake_live(root.path(), limits);
    assert!(live.send(Command::NavigateAll {
        url: "http://127.0.0.1:4173/next".into(),
    }));
    wait_for_requests(&peer, "Page.navigate", "S0", 2);
    // Before the server answers, the page follows its own link and asks
    // before leaving; Helium reports the request first.
    peer.event(phone(
        "Page.frameRequestedNavigation",
        json!({"frameId": "T0", "reason": "anchorClick", "disposition": "currentTab",
            "url": "http://127.0.0.1:4173/link"}),
    ));
    peer.event(dialog_opening("beforeunload", "", ""));
    let question = phone_dialog(&live);
    answer_phone(&live, question.token, false, None);
    wait_for_requests(&peer, "Page.handleJavaScriptDialog", "S0", 1);
    peer.event(dialog_closed(false));
    let status = wait_for(&live, "stayed", Duration::from_secs(2), |s| {
        s.devices[0].dialog.is_none()
    });
    // Chromium cancels only the link. Broxser's navigation is still loading
    // and keeps its deadline.
    assert!(status.devices[0].loading);
    assert_eq!(status.devices[0].error, None);
    thread::sleep(Duration::from_millis(300));
    assert_eq!(peer.count("Page.stopLoading", "S0"), 0);
    wait_for_requests(&peer, "Page.stopLoading", "S0", 1);
    drop(live);
}

#[test]
fn staying_ends_the_broxser_navigation_after_a_page_request_outside_its_tab() {
    let root = profile_root();
    let peer = peer_holding_phone_navigations(root.path());
    let limits = Limits {
        load: Duration::from_millis(600),
        ..Limits::default()
    };
    let live = fake_live(root.path(), limits);
    for (round, disposition) in ["newTab", "newWindow", "download"].into_iter().enumerate() {
        assert!(live.send(Command::NavigateAll {
            url: "http://127.0.0.1:4173/next".into(),
        }));
        wait_for_requests(&peer, "Page.navigate", "S0", round + 2);
        // A navigation outside the current tab leaves the page where it is;
        // the question that follows is about Broxser's navigation.
        peer.event(phone(
            "Page.frameRequestedNavigation",
            json!({"frameId": "T0", "reason": "anchorClick", "disposition": disposition,
                "url": "http://127.0.0.1:4173/link"}),
        ));
        peer.event(dialog_opening("beforeunload", "", ""));
        let question = phone_dialog(&live);
        answer_phone(&live, question.token, false, None);
        wait_for_requests(&peer, "Page.handleJavaScriptDialog", "S0", round + 1);
        peer.event(dialog_closed(false));
        wait_for(
            &live,
            &format!("staying after {disposition}"),
            Duration::from_secs(2),
            |s| s.devices[0].dialog.is_none() && !s.devices[0].loading,
        );
    }
    thread::sleep(limits.load + Duration::from_millis(200));
    let status = live.status();
    assert_eq!(status.devices[0].error, None);
    assert!(!status.devices[0].loading);
    assert_eq!(peer.count("Page.stopLoading", "S0"), 0);
    drop(live);
}

#[test]
fn a_dialog_drops_the_composition_without_a_cancel_or_a_not_responding_report() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, session| {
        method == "Input.dispatchKeyEvent" && session == Some("S0")
    });
    let live = fake_live(root.path(), Limits::default());
    peer.event(phone(
        "Runtime.executionContextCreated",
        json!({"context": {"id": 42, "name": IME_WORLD, "auxData": {"frameId": "T0", "type": "isolated"}}}),
    ));
    // The fake reads the current editable as anchor 1.
    let compose = |sent: usize| {
        peer.event(phone(
            "Runtime.bindingCalled",
            json!({"name": IME_BINDING, "executionContextId": 42,
                "payload": "{\"active\":true,\"anchor\":1,\"x\":20,\"y\":40,\"width\":1,\"height\":18}"}),
        ));
        let target = wait_for(&live, "the caret", Duration::from_secs(2), |s| {
            s.devices[0].text_input.is_some()
        })
        .devices[0]
            .text_input
            .unwrap()
            .target;
        assert!(live.send(Command::Ime {
            device: 0,
            target,
            action: ImeAction::Preedit {
                text: "ka".into(),
                selection: 2..2,
            },
        }));
        wait_for_requests(&peer, "Input.imeSetComposition", "S0", sent);
    };
    // A cancel would wait behind the dialog and, if no composition is left
    // by then, delete the page's selection.
    compose(1);
    peer.event(dialog_opening("alert", "hello", ""));
    let alert = phone_dialog(&live);
    assert_eq!(live.status().devices[0].text_input, None);
    thread::sleep(Duration::from_millis(200));
    assert_eq!(peer.count("Input.imeSetComposition", "S0"), 1);
    answer_phone(&live, alert.token, true, None);
    peer.event(dialog_closed(true));
    wait_for(&live, "the dialog to close", Duration::from_secs(2), |s| {
        s.devices[0].dialog.is_none()
    });
    // A page at the unanswered input limit waits for its user, not for input.
    // The composition's answer is read first, so the keys alone reach it.
    compose(2);
    wait_until_read(&peer);
    send_keys(&live, 0, MAX_UNANSWERED_INPUT / 2);
    wait_for_requests(&peer, "Input.dispatchKeyEvent", "S0", MAX_UNANSWERED_INPUT);
    peer.event(dialog_opening("alert", "again", ""));
    phone_dialog(&live);
    thread::sleep(Duration::from_millis(200));
    let status = live.status();
    assert_eq!(status.devices[0].error, None);
    assert_eq!(peer.count("Input.imeSetComposition", "S0"), 2);
    drop(live);
}

#[test]
fn a_dialog_broxser_cannot_show_is_reported_and_blocks_nothing() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = fake_live(root.path(), Limits::default());
    for opening in [
        dialog_opening("print", "", ""),
        phone(
            "Page.javascriptDialogOpening",
            json!({"url": "http://127.0.0.1:4173/", "message": "no type"}),
        ),
    ] {
        peer.event(opening);
        let status = wait_for(&live, "the report", Duration::from_secs(2), |s| {
            s.devices[0].error.is_some()
        });
        assert_eq!(status.devices[0].error.as_deref(), Some(UNKNOWN_DIALOG));
        assert_eq!(status.devices[0].dialog, None);
        // Broxser has no answer for it, so input still goes to the page.
        let keys = peer.count("Input.dispatchKeyEvent", "S0");
        send_keys(&live, 0, 1);
        wait_for_requests(&peer, "Input.dispatchKeyEvent", "S0", keys + 2);
        peer.event(dialog_closed(false));
        wait_for(&live, "the report cleared", Duration::from_secs(2), |s| {
            s.devices[0].error.is_none()
        });
    }
    // Reload is not refused; it cancels such a dialog.
    peer.event(dialog_opening("print", "", ""));
    wait_for(&live, "the report", Duration::from_secs(2), |s| {
        s.devices[0].error.is_some()
    });
    assert!(live.send(Command::Reload { device: 0 }));
    wait_for_requests(&peer, "Page.reload", "S0", 1);
    drop(live);
}

#[test]
fn a_refused_go_retires_the_devices_link_and_a_refused_synced_link_keeps_it() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = fake_live(root.path(), Limits::default());
    link_observer(&peer, &live);
    // The tablet, in the phone's session, observes its links too.
    let tablet_report = |phase: &str, id: u64, url: &str| {
        tablet(
            "Runtime.bindingCalled",
            json!({"name": LINK_BINDING, "executionContextId": 8,
                "payload": json!({"phase": phase, "id": id, "url": url}).to_string()}),
        )
    };
    peer.event(tablet(
        "Runtime.executionContextCreated",
        json!({"context": {"id": 8, "name": LINK_WORLD, "auxData": {"frameId": "T1", "type": "isolated"}}}),
    ));
    let link = "http://127.0.0.1:4173/linked";
    follow_link(&peer, 1, link, "LINK");
    // The old document asks something while the link's server has not answered.
    peer.event(dialog_opening("alert", "wait", ""));
    phone_dialog(&live);
    // The tablet's own link commits, and the phone refuses to follow it.
    let tablet_link = "http://127.0.0.1:4173/tablet";
    peer.event(tablet_report("C", 1, tablet_link));
    peer.event(tablet(
        "Page.frameRequestedNavigation",
        json!({"frameId": "T1", "reason": "anchorClick", "disposition": "currentTab",
            "url": tablet_link}),
    ));
    peer.event(tablet_report("Y", 1, tablet_link));
    peer.event(tablet(
        "Page.frameStartedNavigating",
        json!({"frameId": "T1", "loaderId": "TABLET", "navigationType": "differentDocument",
            "url": tablet_link}),
    ));
    peer.event(tablet(
        "Page.frameNavigated",
        json!({"frame": {"id": "T1", "loaderId": "TABLET", "url": tablet_link}}),
    ));
    wait_for(&live, "the refused link", Duration::from_secs(2), |s| {
        s.devices[0].error.as_deref() == Some(DIALOG_OPEN)
    });
    // That left the phone as it was: its link still commits and reaches the
    // tablet like any trusted link (ADR 0013).
    peer.event(commit("LINK", link, json!({})));
    wait_for_requests(&peer, "Page.navigate", "S1", 2);
    assert_eq!(
        peer.navigations("S1").last().map(String::as_str),
        Some(link)
    );

    // An explicit Go supersedes the link the phone follows, also when the
    // phone's dialog refuses it.
    let next = "http://127.0.0.1:4173/next";
    follow_link(&peer, 2, next, "NEXT");
    peer.event(dialog_opening("alert", "wait", ""));
    phone_dialog(&live);
    let go = "http://127.0.0.1:4173/go";
    assert!(live.send(Command::NavigateAll { url: go.into() }));
    wait_for_requests(&peer, "Page.navigate", "S1", 3);
    wait_for(&live, "the refused Go", Duration::from_secs(2), |s| {
        s.devices[0].error.as_deref() == Some(DIALOG_OPEN)
    });
    // The link commits later and leaves the tablet on the Go's page.
    peer.event(commit("NEXT", next, json!({})));
    wait_until_read(&peer);
    assert_eq!(live.status().devices[0].url, next);
    assert_eq!(peer.navigations("S1"), ["http://127.0.0.1:4173/", link, go]);
    assert_eq!(
        peer.navigations("S0").len(),
        1,
        "the phone was not navigated"
    );
    drop(live);
}

#[test]
fn a_stopped_runtime_leaves_no_dialog_shown() {
    let root = profile_root();
    // The browser never acknowledges a frame, so the runtime stops at the
    // command limit, as it does when the browser goes away.
    let peer = FakePeer::start(root.path(), |method, _| method == "Page.screencastFrameAck");
    let limits = Limits {
        command: Duration::from_secs(1),
        ..Limits::default()
    };
    let live = fake_live(root.path(), limits);
    peer.event(dialog_opening("confirm", "Continue?", ""));
    phone_dialog(&live);
    assert!(live.send(Command::NavigateAll {
        url: "http://127.0.0.1:4173/next".into(),
    }));
    wait_for(&live, "the refusal", Duration::from_secs(2), |s| {
        s.devices[0].error.as_deref() == Some(DIALOG_OPEN)
    });
    peer.event(phone("Page.screencastFrame", json!({"sessionId": 1})));
    let status = wait_for(&live, "the runtime to stop", Duration::from_secs(5), |s| {
        matches!(s.runtime, RuntimeState::Stopped { .. })
    });
    assert!(
        matches!(&status.runtime, RuntimeState::Stopped { error: Some(error) }
            if error.contains("Page.screencastFrameAck")),
        "{:?}",
        status.runtime
    );
    assert_eq!(status.devices[0].dialog, None);
    assert_eq!(status.devices[0].error, None);
    drop(live);
}

#[test]
fn a_dialog_refuses_reload_and_synced_links_and_drops_pointer_input() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = fake_live(root.path(), Limits::default());
    link_observer(&peer, &live);
    // The tablet shares the phone's session, so the phone's links reach it.
    peer.event(
        json!({"method": "Page.javascriptDialogOpening", "sessionId": "S1", "params": {
            "url": "http://127.0.0.1:4173/", "message": "wait", "type": "alert",
            "hasBrowserHandler": true, "defaultPrompt": ""
        }}),
    );
    wait_for(&live, "the tablet's dialog", Duration::from_secs(2), |s| {
        s.devices[1].dialog.is_some()
    });
    let link = "http://127.0.0.1:4173/linked";
    follow_link(&peer, 1, link, "LINK");
    peer.event(commit("LINK", link, json!({})));
    let status = wait_for(&live, "the refused link", Duration::from_secs(2), |s| {
        s.devices[1].error.is_some()
    });
    assert_eq!(status.devices[1].error.as_deref(), Some(DIALOG_OPEN));
    assert!(status.devices[1].dialog.is_some());
    assert!(live.send(Command::Reload { device: 1 }));
    for (kind, buttons) in [
        (PointerKind::Move, 0),
        (PointerKind::Down, 1),
        (PointerKind::Up, 0),
    ] {
        assert!(live.send(Command::Pointer {
            device: 1,
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
    assert!(live.send(Command::Wheel {
        device: 1,
        x: 10.0,
        y: 10.0,
        delta_x: 0.0,
        delta_y: 100.0,
    }));
    // Commands run in order: once the phone's key arrives, the tablet's
    // commands ran and anything they queued had a turn to go out.
    send_keys(&live, 0, 1);
    wait_for_requests(&peer, "Input.dispatchKeyEvent", "S0", 2);
    thread::sleep(Duration::from_millis(200));
    assert_eq!(peer.navigations("S1").len(), 1);
    assert_eq!(peer.count("Page.reload", "S1"), 0);
    assert_eq!(peer.count("Input.dispatchMouseEvent", "S1"), 0);
    assert_eq!(live.status().devices[1].error.as_deref(), Some(DIALOG_OPEN));
    drop(live);
}

#[test]
fn a_new_document_a_crash_or_a_detach_ends_the_dialog_and_its_token() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = fake_live(root.path(), Limits::default());
    for (end, error) in [
        (
            commit("next", "http://127.0.0.1:4173/next", json!({})),
            None,
        ),
        (
            json!({"method": "Target.targetCrashed", "params": {
                "targetId": "T0", "status": "crashed", "errorCode": 11
            }}),
            Some("crashed"),
        ),
        (
            json!({"method": "Target.detachedFromTarget", "params": {
                "sessionId": "S0", "targetId": "T0"
            }}),
            Some("detached"),
        ),
    ] {
        peer.event(dialog_opening("confirm", "Continue?", ""));
        let dialog = phone_dialog(&live);
        peer.event(end);
        let status = wait_for(&live, "the dialog to end", Duration::from_secs(2), |s| {
            s.devices[0].dialog.is_none()
        });
        match error {
            None => assert_eq!(status.devices[0].error, None),
            Some(word) => assert!(
                status.devices[0]
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains(word)),
                "{:?}",
                status.devices[0].error
            ),
        }
        // Its token answers nothing any more.
        answer_phone(&live, dialog.token, true, None);
        thread::sleep(Duration::from_millis(100));
        assert_eq!(peer.count("Page.handleJavaScriptDialog", "S0"), 0);
    }
    drop(live);
}

#[test]
fn popups_are_closed_at_once_and_reported_for_their_device() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = fake_live(root.path(), Limits::default());
    let origin = "http://127.0.0.1:4173";
    let window_open = |session: &str, url: &str| {
        json!({"method": "Page.windowOpen", "sessionId": session, "params": {
            "url": url, "windowName": "_blank", "windowFeatures": [], "userGesture": true}})
    };
    let created = |target: &str, opener: Option<&str>, context: &str| {
        let mut info = json!({"targetId": target, "type": "page", "title": "", "url": "",
            "attached": false, "canAccessOpener": false, "browserContextId": context});
        if let Some(opener) = opener {
            info["openerId"] = json!(opener);
        }
        json!({"method": "Target.targetCreated", "params": {"targetInfo": info}})
    };
    // The report is visible before the peer has read the close request that
    // went with it, so the check waits for `count` requests first.
    let closed = |peer: &FakePeer, count: usize| -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let targets: Vec<String> = peer
                .browser_requests("Target.closeTarget")
                .iter()
                .map(|params| params["targetId"].as_str().unwrap_or_default().to_owned())
                .collect();
            if targets.len() >= count || Instant::now() >= deadline {
                return targets;
            }
            thread::sleep(Duration::from_millis(10));
        }
    };

    // The phone opens a window: it is closed and reported with its URL.
    peer.event(window_open("S0", &format!("{origin}/popup")));
    peer.event(created("P1", Some("T0"), "CTX1"));
    let popup = wait_for(&live, "the report", Duration::from_secs(2), |s| {
        s.devices[0].popup.is_some()
    })
    .devices[0]
        .popup
        .clone()
        .unwrap();
    assert_eq!(popup.url, format!("{origin}/popup"));
    assert!(popup.openable);
    assert_eq!(closed(&peer, 1), ["P1"]);
    // Only the report of the latest window opens, and only once.
    peer.event(window_open("S0", "javascript:alert(1)"));
    peer.event(created("P2", Some("T0"), "CTX1"));
    let script = wait_for(&live, "the second report", Duration::from_secs(2), |s| {
        s.devices[0].popups == 2
    })
    .devices[0]
        .popup
        .clone()
        .unwrap();
    assert!(!script.openable, "a javascript: URL is not a destination");
    assert!(live.send(Command::OpenPopup {
        device: 0,
        token: popup.token,
    }));
    assert!(live.send(Command::OpenPopup {
        device: 0,
        token: script.token,
    }));
    // A long address is shown shortened but never loaded shortened.
    let long = format!("{origin}/long?{}", "x".repeat(9000));
    peer.event(window_open("S0", &long));
    peer.event(created("P3", Some("T0"), "CTX1"));
    let long_popup = wait_for(&live, "the long report", Duration::from_secs(2), |s| {
        s.devices[0].popups == 3
    })
    .devices[0]
        .popup
        .clone()
        .unwrap();
    assert!(!long_popup.openable);
    assert!(
        long_popup.url.ends_with('…') && long_popup.url.chars().count() == MAX_DIALOG_CHARS + 1
    );
    assert!(live.send(Command::OpenPopup {
        device: 0,
        token: long_popup.token,
    }));
    thread::sleep(Duration::from_millis(200));
    assert_eq!(peer.navigations("S0").len(), 1, "no report was openable");

    // Broxser's own targets have no opener; other contexts are not Broxser's.
    peer.event(created("T9", None, "CTX1"));
    peer.event(created("P4", Some("T9"), "default"));
    // A window whose opener is not a device, such as a frame's, is closed
    // without a report.
    peer.event(created("P5", Some("F1"), "CTX2"));
    // The tablet's window is reported on the tablet.
    peer.event(window_open("S1", &format!("{origin}/tablet-popup")));
    peer.event(created("P6", Some("T1"), "CTX1"));
    let tablet = wait_for(&live, "the tablet's report", Duration::from_secs(2), |s| {
        s.devices[1].popup.is_some()
    })
    .devices[1]
        .popup
        .clone()
        .unwrap();
    assert_eq!(closed(&peer, 5), ["P1", "P2", "P3", "P5", "P6"]);
    let status = live.status();
    assert_eq!(status.devices[0].popups, 3);
    assert_eq!(status.devices[1].popups, 1);
    assert_eq!(status.devices[2].popups, 0);

    // The user opens the tablet's window in the tablet: its URL is loaded once.
    assert!(live.send(Command::OpenPopup {
        device: 1,
        token: tablet.token,
    }));
    wait_for_requests(&peer, "Page.navigate", "S1", 2);
    assert_eq!(peer.navigations("S1")[1], format!("{origin}/tablet-popup"));
    wait_for(&live, "the report consumed", Duration::from_secs(2), |s| {
        s.devices[1].popup.is_none()
    });
    assert!(live.send(Command::OpenPopup {
        device: 1,
        token: tablet.token,
    }));
    thread::sleep(Duration::from_millis(200));
    assert_eq!(peer.navigations("S1").len(), 2);
    assert_eq!(peer.navigations("S0").len(), 1);
    drop(live);
}

#[test]
fn touch_devices_send_touches_and_nothing_for_hover_or_other_buttons() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let mut workspace = workspace("http://127.0.0.1:4173/".into());
    workspace.devices[0].touch = true;
    let live = LiveSession::start_with(
        workspace,
        fake_options(root.path()),
        Limits::default(),
        || {},
    )
    .unwrap();
    wait_for(&live, "the runtime", Duration::from_secs(10), running);
    let pointer = |kind, x, y, button, buttons| Command::Pointer {
        device: 0,
        event: PointerEvent {
            kind,
            x,
            y,
            button,
            buttons,
            click_count: 1,
            modifiers: Modifiers::default(),
        },
    };
    // Hover, a right click and a middle click send nothing to a touch device.
    for command in [
        pointer(PointerKind::Move, 10.0, 10.0, PointerButton::None, 0),
        pointer(PointerKind::Down, 10.0, 10.0, PointerButton::Right, 2),
        pointer(PointerKind::Up, 10.0, 10.0, PointerButton::Right, 0),
        pointer(PointerKind::Down, 10.0, 10.0, PointerButton::Middle, 4),
    ] {
        assert!(live.send(command));
    }
    // A press, a drag and a release are one finger. The move is coalesced
    // and sent by the loop; a release arriving before that drops it, as for
    // a mouse, so the release waits for the move here.
    assert!(live.send(pointer(
        PointerKind::Down,
        20.0,
        30.0,
        PointerButton::Left,
        1
    )));
    assert!(live.send(pointer(
        PointerKind::Move,
        20.0,
        40.0,
        PointerButton::None,
        1
    )));
    wait_for_requests(&peer, "Input.dispatchTouchEvent", "S0", 2);
    assert!(live.send(pointer(PointerKind::Up, 20.0, 40.0, PointerButton::Left, 0)));
    wait_for_requests(&peer, "Input.dispatchTouchEvent", "S0", 3);
    let touches: Vec<Value> = peer
        .received
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, session, _, _)| {
            method == "Input.dispatchTouchEvent" && session.as_deref() == Some("S0")
        })
        .map(|(_, _, _, params)| params.clone())
        .collect();
    assert_eq!(
        touches,
        [
            json!({"type": "touchStart", "touchPoints": [{"x": 20.0, "y": 30.0}], "modifiers": 0}),
            json!({"type": "touchMove", "touchPoints": [{"x": 20.0, "y": 40.0}], "modifiers": 0}),
            json!({"type": "touchEnd", "touchPoints": [], "modifiers": 0}),
        ]
    );
    assert_eq!(peer.count("Input.dispatchMouseEvent", "S0"), 0);
    // The mouse tablet still gets mouse events, hover included.
    assert!(live.send(Command::Pointer {
        device: 1,
        event: PointerEvent {
            kind: PointerKind::Move,
            x: 5.0,
            y: 5.0,
            button: PointerButton::None,
            buttons: 0,
            click_count: 0,
            modifiers: Modifiers::default(),
        },
    }));
    wait_for_requests(&peer, "Input.dispatchMouseEvent", "S1", 1);
    assert_eq!(peer.count("Input.dispatchTouchEvent", "S1"), 0);
    drop(live);
}

#[test]
fn permission_prompts_are_denied_in_every_session_context() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = fake_live(root.path(), Limits::default());
    // Keep the expected descriptors independent of the production helper: a
    // base camera denial does not cover the PTZ query in pinned Helium.
    let permissions = [
        json!({"name": "notifications"}),
        json!({"name": "idle-detection"}),
        json!({"name": "camera"}),
        json!({"name": "microphone"}),
        json!({"name": "camera", "panTiltZoom": true}),
    ];
    let expected: Vec<Value> = ["CTX1", "CTX2"]
        .iter()
        .flat_map(|context| {
            permissions.iter().map(move |permission| {
                json!({"permission": permission, "setting": "denied", "browserContextId": context})
            })
        })
        .collect();
    assert_eq!(peer.browser_requests("Browser.setPermission"), expected);
    drop(live);
}

#[test]
fn downloads_and_file_choosers_are_refused_and_reported_for_their_device() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = fake_live(root.path(), Limits::default());
    // Every session context denies downloads and reports them; every device
    // intercepts file choosers and still cancels them for the page.
    assert_eq!(
        peer.browser_requests("Browser.setDownloadBehavior"),
        [
            json!({"behavior": "deny", "browserContextId": "CTX1", "eventsEnabled": true}),
            json!({"behavior": "deny", "browserContextId": "CTX2", "eventsEnabled": true}),
        ]
    );
    for session in ["S0", "S1", "S2"] {
        assert_eq!(
            peer.last_params("Page.setInterceptFileChooserDialog", session),
            json!({"enabled": true, "cancel": true})
        );
    }
    let origin = "http://127.0.0.1:4173";
    let download = |frame: &str, url: &str, name: &str| {
        json!({"method": "Browser.downloadWillBegin", "params": {
            "frameId": frame, "guid": "G", "url": url, "suggestedFilename": name}})
    };
    let on = |session: &str, method: &str, params: Value| json!({"method": method, "sessionId": session, "params": params});

    // The phone's page: reported on the phone, on one line, without controls.
    peer.event(download(
        "T0",
        &format!("{origin}/report"),
        "re\u{7}port\n.pdf",
    ));
    let status = wait_for(&live, "the phone's report", Duration::from_secs(2), |s| {
        s.devices[0].downloads == 1
    });
    assert_eq!(
        status.devices[0].download,
        Some(DownloadState {
            filename: "report.pdf".into(),
            url: format!("{origin}/report"),
        })
    );
    // A tablet frame that another renderer process took over is the tablet's.
    peer.event(on(
        "S1",
        "Page.frameAttached",
        json!({"frameId": "F1", "parentFrameId": "T1"}),
    ));
    peer.event(on(
        "S1",
        "Page.frameDetached",
        json!({"frameId": "F1", "reason": "swap"}),
    ));
    peer.event(download("F1", "http://localhost:4173/frame", "frame.pdf"));
    wait_for(&live, "the tablet's report", Duration::from_secs(2), |s| {
        s.devices[1].downloads == 1
    });
    // A removed frame, a frame of the desktop's previous document and an
    // unknown frame are nobody's.
    peer.event(on(
        "S1",
        "Page.frameDetached",
        json!({"frameId": "F1", "reason": "remove"}),
    ));
    peer.event(on(
        "S2",
        "Page.frameAttached",
        json!({"frameId": "F2", "parentFrameId": "T2"}),
    ));
    peer.event(on(
        "S2",
        "Page.frameNavigated",
        json!({"frame": {"id": "T2", "loaderId": "L9", "url": format!("{origin}/next")}}),
    ));
    for frame in ["F1", "F2", "F404"] {
        peer.event(download(frame, &format!("{origin}/{frame}"), "x"));
    }
    // A long address is shown shortened.
    peer.event(download(
        "T2",
        &format!("{origin}/{}", "x".repeat(3000)),
        "long.bin",
    ));
    let status = wait_for(&live, "the desktop's report", Duration::from_secs(2), |s| {
        s.devices[2].downloads == 1
    });
    let url = &status.devices[2].download.as_ref().unwrap().url;
    assert!(url.ends_with('…') && url.chars().count() == MAX_DIALOG_CHARS + 1);
    assert_eq!(
        status
            .devices
            .iter()
            .map(|device| device.downloads)
            .collect::<Vec<_>>(),
        [1, 1, 1]
    );

    // The phone's page opens file choosers: counted, never given files.
    for mode in ["selectSingle", "selectMultiple"] {
        peer.event(on(
            "S0",
            "Page.fileChooserOpened",
            json!({"frameId": "T0", "mode": mode, "backendNodeId": 7}),
        ));
    }
    let status = wait_for(&live, "the file choosers", Duration::from_secs(2), |s| {
        s.devices[0].file_choosers == 2
    });
    assert_eq!(status.devices[1].file_choosers, 0);
    assert_eq!(peer.count("DOM.setFileInputFiles", "S0"), 0);

    // Going to an address that is a download ends without a navigation error;
    // the download report tells what happened.
    assert!(live.send(Command::NavigateAll {
        url: format!("{origin}/download"),
    }));
    for session in ["S0", "S1", "S2"] {
        wait_for_requests(&peer, "Page.navigate", session, 2);
    }
    thread::sleep(Duration::from_millis(200));
    for device in live.status().devices {
        assert_eq!(device.error, None);
        assert!(!device.loading);
    }
    drop(live);
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
let keys = 0;
area.addEventListener('keydown', e => report('keydown', {key: e.key, code: e.code, n: ++keys}));
area.addEventListener('paste', e => report('paste', {data: e.clipboardData.getData('text')}));
area.addEventListener('input', e => report('input', {v: area.value, type: e.inputType}));
</script></body></html>"#;

const IME_PAGE: &str = r#"<!doctype html><html><head><meta name=viewport content="width=device-width,initial-scale=1">
<style>body{margin:0}input{position:absolute;left:20px;top:40px;width:240px;height:32px;font:18px sans-serif}
textarea{position:absolute;left:20px;top:100px;width:240px;height:70px;font:18px sans-serif}
#editor{position:absolute;left:20px;top:200px;width:240px;height:70px;font:18px sans-serif;border:1px solid;padding:4px}
#secret{position:absolute;left:20px;top:300px;width:240px;height:32px}
#right,#center{position:absolute;left:20px;width:240px;height:32px;font:18px monospace}
#right{top:350px;text-align:right}#center{top:400px;text-align:center}</style></head><body>
<input id=field><textarea id=area></textarea><div id=editor contenteditable></div><input id=secret type=password>
<input id=right value=abcd><input id=center value=abcd><script>
const report = (kind, data) => fetch('/event?' + new URLSearchParams({kind,w:innerWidth,...data}));
for (const el of [field,area,editor,secret]) for (const kind of ['compositionstart','compositionupdate','compositionend','input'])
  el.addEventListener(kind,e=>report(kind,{id:el.id,value:el===secret?'':el.value??el.textContent,data:e.data||''}));
for (const el of [right,center]) el.addEventListener('focus',()=>setTimeout(()=>el.setSelectionRange(2,2),40));
field.focus();
</script></body></html>"#;

const IME_ATTACK_PAGE: &str = r#"<!doctype html><html><head><meta name=viewport content="width=device-width,initial-scale=1">
<style>body{margin:0}#field{position:absolute;left:20px;top:40px;width:240px;height:32px;font:18px monospace}
#tall{position:absolute;left:20px;top:160px;width:240px;height:100px;font:18px monospace}</style></head><body>
<input id=field value=abcdef><input id=other><input id=tall value=abcdef><script>
const report = (kind, data) => fetch('/event?' + new URLSearchParams({kind,w:innerWidth,...data}));
field.addEventListener('input',e=>report('input',{value:field.value,trusted:e.isTrusted}));
field.addEventListener('keydown',e=>{
  if(e.key==='F2') {field.setSelectionRange(5,5);field.dispatchEvent(new Event('input',{bubbles:true}));}
  if(e.key==='F3') {field.dispatchEvent(new CompositionEvent('compositionstart',{bubbles:true}));field.setSelectionRange(3,3);}
  if(e.key==='F4') {other.focus();field.focus();}
  if(['F2','F3','F4'].includes(e.key)) report('attack',{key:e.key});
});
field.focus();field.setSelectionRange(1,1);
</script></body></html>"#;

/// Answers F2 after 400 ms and F3 after 1.5 s.
const IME_BUSY_PAGE: &str = r#"<!doctype html><html><head><meta name=viewport content="width=device-width,initial-scale=1">
</head><body><input id=field style="width:240px;height:32px"><script>
const report = (kind, data) => fetch('/event?' + new URLSearchParams({kind,w:innerWidth,...data}));
field.addEventListener('input', () => report('input', {value: field.value}));
field.addEventListener('keydown', e => {
  const busy = {F2: 400, F3: 1500}[e.key];
  if (!busy) return;
  const end = performance.now() + busy;
  while (performance.now() < end) {}
  report('free', {key: e.key});
});
field.focus();
</script></body></html>"#;

/// Frame document of `/frames`: a link, a hash link and a History API link,
/// each 50 px below the previous one. Its reports name the top window's width.
const FRAME_PAGE: &str = r##"<!doctype html><html><head><style>body{margin:0}
a{display:block;width:160px;height:40px;margin-bottom:10px;background:#08f;color:#fff}</style></head><body>
<a id=a href="/frame-dest">frame link</a><a id=h href="#frame-part">frame hash</a><a id=s href="/frame-spa">frame spa</a>
<p id=frame-part>part</p><script>
const report = kind => fetch('/event?' + new URLSearchParams({kind, w: top.innerWidth}));
addEventListener('hashchange', () => report('framehash'));
s.addEventListener('click', e => { e.preventDefault(); history.pushState(null, '', s.href); report('framespa'); });
</script></body></html>"##;

/// Buttons that open each kind of dialog, 60 px apart, and a field below them.
/// Every dialog's outcome is reported once the page runs again.
const DIALOG_PAGE: &str = r#"<!doctype html><html><head><meta name=viewport content="width=device-width,initial-scale=1">
<style>body{margin:0;font:16px sans-serif}button{position:absolute;left:20px;width:160px;height:40px}
#b-alert{top:100px}#b-confirm{top:160px}#b-prompt{top:220px}#field{position:absolute;left:20px;top:300px;width:160px;height:30px}</style></head><body>
<button id=b-alert>alert</button><button id=b-confirm>confirm</button><button id=b-prompt>prompt</button><input id=field>
<script>
const report = (kind, data) => fetch('/event?' + new URLSearchParams({kind, w: innerWidth, ...data}));
const by = id => document.getElementById(id);
by('b-alert').addEventListener('click', () => { window.alert('hello from the page'); report('dialog', {result: 'alert closed'}); });
by('b-confirm').addEventListener('click', () => report('dialog', {result: 'confirm=' + window.confirm('Continue?')}));
by('b-prompt').addEventListener('click', () => report('dialog', {result: 'prompt=' + window.prompt('Your name?', 'guest')}));
by('field').addEventListener('input', () => report('input', {v: by('field').value}));
</script></body></html>"#;

/// Asks before leaving once its field holds text. Chromium shows that
/// question only after the user interacted with the page.
const DIRTY_PAGE: &str = r#"<!doctype html><html><head><meta name=viewport content="width=device-width,initial-scale=1">
<style>body{margin:0;font:16px sans-serif}#field{position:absolute;left:20px;top:100px;width:160px;height:30px}
#link{position:absolute;left:20px;top:160px;width:160px;height:40px;background:#08f;color:#fff}</style></head><body>
<input id=field><a id=link href="/next">next</a>
<script>
const report = (kind, data) => fetch('/event?' + new URLSearchParams({kind, w: innerWidth, ...data}));
const field = document.getElementById('field');
addEventListener('mousedown', e => report('down', {x: e.clientX, y: e.clientY, target: e.target.id}), true);
addEventListener('keydown', e => report('keydown', {key: e.key, target: document.activeElement.id}), true);
field.addEventListener('input', () => report('input', {v: field.value}));
addEventListener('beforeunload', e => { if (field.value) { e.preventDefault(); e.returnValue = 'unsaved'; } });
</script></body></html>"#;

/// Opens a window three ways, 60 px apart: `window.open`, a `target=_blank`
/// link and a named window with features.
const OPENER_PAGE: &str = r#"<!doctype html><html><head><meta name=viewport content="width=device-width,initial-scale=1">
<style>body{margin:0}button,a{position:absolute;left:20px;width:160px;height:40px;display:block;background:#08f;color:#fff}
#open{top:100px}#blank{top:160px}#named{top:220px}</style></head><body>
<button id=open onclick="window.open('/popup-page?opener', '_blank')">open</button>
<a id=blank href="/popup-page?blank" target=_blank>blank</a>
<button id=named onclick="window.open('/popup-page?named', 'win', 'width=300,height=300')">named</button>
</body></html>"#;

/// A window that keeps reporting that it runs and, 300 ms after it starts,
/// sends its opener elsewhere, as a hostile or careless popup can.
const POPUP_PAGE: &str = r#"<!doctype html><script>
const q = location.search.slice(1);
setInterval(() => fetch('/event?' + new URLSearchParams({kind: 'alive', q, opener: !!window.opener})), 200);
setTimeout(() => { if (window.opener) window.opener.location = '/hijacked'; }, 300);
</script>"#;

/// An attachment link, a `download` attribute link, a file input and a
/// cross-site frame (another renderer process) with its own attachment link.
const DOWNLOAD_PAGE: &str = r#"<!doctype html><html><head><meta name=viewport content="width=device-width,initial-scale=1">
<style>body{margin:0;font:16px sans-serif}a,input,iframe{position:absolute;left:20px;width:200px;height:40px;display:block;border:0}</style></head><body>
<a id=file href="/download?name=report.pdf" style="top:100px;background:#36c;color:#fff">attachment</a>
<a id=named href="/download" download="notes.txt" style="top:160px;background:#3a3;color:#fff">download attribute</a>
<input id=chooser type=file style="top:220px">
<iframe id=frame style="top:280px;width:300px;height:120px"></iframe>
<script>
const ping = result => fetch('/event?kind=chooser&result=' + result);
chooser.addEventListener('cancel', () => ping('cancel'));
chooser.addEventListener('change', () => ping('change'));
frame.src = 'http://localhost:' + location.port + '/frame-download';
</script></body></html>"#;

const FRAME_DOWNLOAD_PAGE: &str = r#"<!doctype html><body style="margin:0" onload="fetch('/event?kind=frame')"><a href="/download?name=frame.pdf" style="display:block;width:300px;height:120px;background:#c63">frame download</a></body>"#;

/// Asks for the notification permission without a gesture as soon as it
/// loads, and reports the answer, its delay and what `permissions.query` says.
const PERMISSION_PAGE: &str = r#"<!doctype html><script>
const ping = (name, value, ms) => fetch('/event?' + new URLSearchParams({kind: 'permission', name, value, ms: String(Math.round(ms))}));
const t0 = performance.now();
Notification.requestPermission().then(v => ping('notifications', v, performance.now() - t0), e => ping('notifications', 'error:' + e.name, performance.now() - t0));
navigator.permissions.query({name: 'notifications'}).then(s => ping('query', s.state, 0), e => ping('query', 'error:' + e.name, 0));
</script>"#;

/// Queries camera descriptors and unaffected controls without requesting a
/// camera, so the PTZ regression needs no physical or simulated media devices.
const CAMERA_PERMISSION_PAGE: &str = r#"<!doctype html><meta name=viewport content="width=device-width,initial-scale=1"><script>
for (const [name, descriptor] of [
  ['camera', {name: 'camera'}],
  ['camera-ptz', {name: 'camera', panTiltZoom: true}],
  ['clipboard-write', {name: 'clipboard-write'}],
  ['screen-wake-lock', {name: 'screen-wake-lock'}],
]) {
  const ping = value => fetch('/event?' + new URLSearchParams({kind: 'camera-permission', origin: location.origin, w: String(innerWidth), name, value}));
  navigator.permissions.query(descriptor).then(s => ping(s.state), e => ping('error:' + e.name));
}
</script>"#;

/// Reports every pointer, mouse and touch event on its button with the
/// pointer type, and its scroll position after a drag.
const TOUCH_PAGE: &str = r#"<!doctype html><html><head><meta name=viewport content="width=device-width,initial-scale=1">
<style>body{margin:0;height:3000px}#t{position:absolute;left:20px;top:100px;width:200px;height:200px;background:#36c;touch-action:pan-y}</style></head><body>
<div id=t></div>
<script>
let n = 0;
const ping = (name, value) => {
  const url = '/event?' + new URLSearchParams({kind: 'touch', name, value: String(value), w: innerWidth, n: n++});
  // HTTP reports may arrive out of order. Force pointerup to arrive after
  // click so the test cannot mistake the final event for a complete report.
  if (name === 'pointerup') setTimeout(() => fetch(url), 150);
  else fetch(url);
};
for (const type of ['pointerdown', 'pointerup', 'pointermove', 'touchstart', 'touchmove', 'touchend', 'mousedown', 'mouseup', 'click']) {
  t.addEventListener(type, e => ping(type, e.pointerType || (e.touches ? 'touches=' + e.touches.length : 'mouse')));
}
addEventListener('scrollend', () => ping('scrollend', Math.round(scrollY)));
</script></body></html>"#;

/// Reports what a page can tell about the browser it runs in: the user agent,
/// the pointer and hover media, and the screen the viewport sits on.
const FIDELITY_PAGE: &str = r#"<!doctype html><html><head><meta name=viewport content="width=device-width,initial-scale=1"></head><body><script>
const q = s => matchMedia(s).matches;
fetch('/event?' + new URLSearchParams({kind: 'fidelity', w: innerWidth, ua: navigator.userAgent, brands: (navigator.userAgentData ? navigator.userAgentData.brands.map(b => b.brand).join('|') : 'none'), hover: q('(hover: hover)') ? 'hover' : 'none', pointer: q('(pointer: fine)') ? 'fine' : q('(pointer: coarse)') ? 'coarse' : 'none', screen: screen.width + 'x' + screen.height, touch: navigator.maxTouchPoints}));
</script></body></html>"#;

/// `PAGE` whose link leads to the JavaScript expression `href`, then `script`.
fn link_page(href: &str, script: &str) -> String {
    PAGE.replace(
        "AUTO",
        &format!("const a = document.getElementById('link'); a.href = {href}; {script}"),
    )
}

/// Script that adds `link2`, a second link 200 px below the first, to `href`.
fn second_link(href: &str) -> String {
    format!(
        r##"document.body.insertAdjacentHTML('beforeend', '<a id=link2 href="{href}" style="position:absolute;left:20px;top:300px;width:160px;height:40px;background:#0a0;color:#fff">second</a>'); const link2 = document.getElementById('link2');"##
    )
}

/// Script that replaces the link with a button of the same size and place.
const BUTTON: &str = "const b = document.createElement('button'); b.textContent = 'button'; b.style.cssText = 'position:absolute;left:20px;top:100px;width:160px;height:40px'; a.style.display = 'none'; document.body.append(b);";

/// Script that adds the `#part` target of hash links, far below the fold.
const PART: &str = r#"document.body.insertAdjacentHTML('beforeend', '<div id=part style="position:absolute;top:3000px">part</div>');"#;

fn fixture() -> Fixture {
    Fixture::start(|request, _| {
        let path = request.path.split('?').next().unwrap_or("/");
        match path {
            // `/r?status=302&to=<location>`: a redirect.
            "/r" => {
                let query = request.path.split_once('?').map_or("", |(_, query)| query);
                let field = |name: &str| {
                    query.split('&').find_map(|pair| {
                        pair.split_once('=')
                            .filter(|(key, _)| *key == name)
                            .map(|(_, value)| query_value(value))
                    })
                };
                return Reply::Empty {
                    status: field("status").and_then(|s| s.parse().ok()).unwrap_or(302),
                    location: field("to"),
                };
            }
            "/nocontent" => {
                return Reply::Empty {
                    status: 204,
                    location: None,
                };
            }
            "/dropped" => return Reply::Drop,
            // `/download?name=<file name>`: an attachment; without a name, a
            // plain text file that only a `download` attribute saves.
            "/download" => {
                return Reply::File {
                    filename: request
                        .path
                        .split_once("?name=")
                        .map(|(_, name)| query_value(name)),
                };
            }
            _ => {}
        }
        let body = match path {
            "/redirect-chain" => link_page(
                "'/r?status=307&to=' + encodeURIComponent('/r?status=302&to=' + encodeURIComponent('/landed'))",
                "",
            ),
            "/redirect-fragment" => {
                link_page("'/r?status=302&to=' + encodeURIComponent('/landed#part')", "")
            }
            "/redirect-away" => link_page(
                "'/r?status=302&to=' + encodeURIComponent('http://localhost:' + location.port + '/landed')",
                "",
            ),
            "/fragment-link" => link_page("'/landed#part'", ""),
            "/landed" => PAGE.replace("AUTO", PART),
            "/nocontent-link" => link_page("'/nocontent'", ""),
            "/redirect-empty" => {
                link_page("'/r?status=302&to=' + encodeURIComponent('/nocontent')", "")
            }
            "/dropped-link" => link_page("'/dropped'", ""),
            "/stop-link" => link_page(
                "'/slow'",
                "a.addEventListener('click', () => setTimeout(() => window.stop(), 300));",
            ),
            "/two-links" => link_page("'/slow'", &second_link("/landed?second")),
            "/go-supersede" => link_page("'/slow'", ""),
            "/hash" => link_page("'#part'", PART),
            "/spa" => link_page(
                "'/spa-route'",
                "a.addEventListener('click', e => { e.preventDefault(); history.pushState(null, '', a.href); });",
            ),
            // A router that saves state on the current entry, then pushes the
            // link's URL after its data arrives.
            "/spa-late" => link_page(
                "'/spa-late-route'",
                "a.addEventListener('click', e => { e.preventDefault(); history.replaceState({y: scrollY}, '', location.href); setTimeout(() => history.pushState(null, '', a.href), 1500); });",
            ),
            "/navigation-api" => link_page(
                "'/nav-route'",
                "navigation.addEventListener('navigate', e => { if (e.canIntercept && !e.hashChange && new URL(e.destination.url).pathname === '/nav-route') e.intercept({handler: async () => {}}); });",
            ),
            "/spa-keyboard" => link_page(
                "'/spa-key-route'",
                "a.addEventListener('click', e => { e.preventDefault(); history.pushState(null, '', a.href); }); a.focus();",
            ),
            "/spa-script" => link_page(
                "'/next'",
                "setTimeout(() => history.pushState(null, '', '/spa-script-route'), 300);",
            ),
            "/spa-button" => link_page(
                "'/spa-button-route'",
                &format!("{BUTTON} b.addEventListener('click', () => history.pushState(null, '', a.href));"),
            ),
            "/hash-script" => link_page(
                "'#part'",
                &format!("{PART} {BUTTON} b.addEventListener('click', () => {{ location.hash = 'part'; }});"),
            ),
            "/scripted-hash" => link_page(
                "'#part'",
                &format!("{PART} {BUTTON} b.addEventListener('click', () => a.click());"),
            ),
            "/spa-other" => link_page(
                "'/spa-a'",
                "a.addEventListener('click', e => { e.preventDefault(); history.pushState(null, '', '/spa-b'); });",
            ),
            "/spa-superseded" => link_page(
                "'/spa-x'",
                &format!(
                    "a.addEventListener('click', e => e.preventDefault()); {} link2.addEventListener('click', e => {{ e.preventDefault(); history.pushState(null, '', a.href); }});",
                    second_link("/spa-y")
                ),
            ),
            "/spa-hidden" => link_page(
                "'/spa-hidden-route'",
                "a.addEventListener('click', e => { e.preventDefault(); setTimeout(() => history.pushState(null, '', a.href), 700); });",
            ),
            // Not scrollable: a frame's fragment navigation would scroll it.
            "/frames" => PAGE.replace(
                "AUTO",
                r##"document.body.style.height = '600px'; document.body.insertAdjacentHTML('beforeend', '<iframe name=f src="/frame" style="position:absolute;left:20px;top:300px;width:300px;height:200px;border:0"></iframe><a id=link2 href="/frame-dest" target=f style="position:absolute;left:20px;top:520px;width:160px;height:40px;background:#0a0;color:#fff">into frame</a>');"##,
            ),
            "/frame" => FRAME_PAGE.into(),
            "/frame-dest" => "<!doctype html><p>frame destination</p>".into(),
            "/dialogs" => DIALOG_PAGE.into(),
            "/popups" => OPENER_PAGE.into(),
            "/popup-page" => POPUP_PAGE.into(),
            "/downloads" => DOWNLOAD_PAGE.into(),
            "/frame-download" => FRAME_DOWNLOAD_PAGE.into(),
            "/permissions" => PERMISSION_PAGE.into(),
            "/camera-permissions" => CAMERA_PERMISSION_PAGE.into(),
            "/touch" => TOUCH_PAGE.into(),
            "/fidelity" => FIDELITY_PAGE.into(),
            "/dirty" => DIRTY_PAGE.into(),
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
            "/ime" => IME_PAGE.into(),
            "/ime-attacks" => IME_ATTACK_PAGE.into(),
            "/ime-busy" => IME_BUSY_PAGE.into(),
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

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_ime_composition_targets_caret_and_visibility() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/ime")));
    let status = live.wait("IME targets", Duration::from_secs(30), |status| {
        loaded(status, &fixture, "/ime") && status.devices.iter().all(|d| d.text_input.is_some())
    });
    let phone = status.devices[0].text_input.unwrap();
    let desktop = status.devices[2].text_input.unwrap();
    assert_ne!(phone.target, desktop.target);
    assert!((15.0..280.0).contains(&phone.caret.x));
    assert!((35.0..80.0).contains(&phone.caret.y));

    live.send(Command::Ime {
        device: 0,
        target: phone.target,
        action: ImeAction::Preedit {
            text: "に".into(),
            selection: 0..1,
        },
    });
    assert!(fixture.wait_for(Duration::from_secs(5), |_| {
        events(&fixture, "compositionstart")
            .iter()
            .any(|e| e["w"] == "360")
    }));
    live.send(Command::Ime {
        device: 0,
        target: phone.target,
        action: ImeAction::Preedit {
            text: "日本".into(),
            selection: 0..2,
        },
    });
    assert!(fixture.wait_for(Duration::from_secs(5), |_| {
        events(&fixture, "compositionupdate")
            .iter()
            .any(|e| e["w"] == "360" && e["data"] == "日本")
    }));
    live.send(Command::Ime {
        device: 0,
        target: phone.target,
        action: ImeAction::Commit {
            text: "日本".into(),
        },
    });
    assert!(fixture.wait_for(Duration::from_secs(5), |_| {
        events(&fixture, "input")
            .iter()
            .any(|e| e["w"] == "360" && e["value"] == "日本")
    }));
    assert!(events(&fixture, "input").iter().all(|e| e["w"] != "1000"));

    let next = live
        .wait("caret after commit", Duration::from_secs(5), |s| {
            s.devices[0]
                .text_input
                .is_some_and(|t| t.caret.x > phone.caret.x)
        })
        .devices[0]
        .text_input
        .unwrap();
    assert!(
        next.caret.x > phone.caret.x,
        "caret should move after typing"
    );
    assert_eq!(next.target, phone.target, "own input keeps the token");

    // GPUI can deliver identical committed text twice without any key-up.
    for _ in 0..2 {
        live.send(Command::Ime {
            device: 2,
            target: desktop.target,
            action: ImeAction::Commit { text: "é".into() },
        });
    }
    assert!(fixture.wait_for(Duration::from_secs(5), |_| {
        events(&fixture, "input")
            .iter()
            .any(|e| e["w"] == "1000" && e["value"] == "éé")
    }));

    click(&live, 0, 40.0, 120.0);
    let latest = live
        .wait("textarea target", Duration::from_secs(5), |s| {
            s.devices[0]
                .text_input
                .is_some_and(|t| t.target != phone.target && t.caret.y > 95.0)
        })
        .devices[0]
        .text_input
        .unwrap();
    live.send(Command::Ime {
        device: 0,
        target: phone.target,
        action: ImeAction::Commit {
            text: "stale".into(),
        },
    });
    live.send(Command::Ime {
        device: 0,
        target: latest.target,
        action: ImeAction::Commit {
            text: "日本".into(),
        },
    });
    assert!(fixture.wait_for(Duration::from_secs(5), |_| {
        events(&fixture, "input")
            .iter()
            .any(|e| e["w"] == "360" && e["id"] == "area" && e["value"] == "日本")
    }));
    live.send(Command::Ime {
        device: 0,
        target: latest.target,
        action: ImeAction::Preedit {
            text: "x".into(),
            selection: 0..1,
        },
    });
    assert!(fixture.wait_for(Duration::from_secs(5), |_| {
        events(&fixture, "compositionupdate")
            .iter()
            .any(|e| e["w"] == "360" && e["data"] == "x")
    }));
    let ended_before = events(&fixture, "compositionend").len();
    live.send(Command::Ime {
        device: 0,
        target: latest.target,
        action: ImeAction::Cancel,
    });
    assert!(fixture.wait_for(Duration::from_secs(5), |_| {
        events(&fixture, "compositionend").len() > ended_before
    }));
    click(&live, 0, 40.0, 220.0);
    let editor = live
        .wait("empty contenteditable caret", Duration::from_secs(5), |s| {
            s.devices[0].text_input.is_some_and(|t| {
                t.target != latest.target && t.caret.y > 200.0 && t.caret.height > 0.0
            })
        })
        .devices[0]
        .text_input
        .unwrap();
    live.send(Command::Ime {
        device: 0,
        target: editor.target,
        action: ImeAction::Commit { text: "z".into() },
    });
    assert!(fixture.wait_for(Duration::from_secs(5), |_| {
        events(&fixture, "input")
            .iter()
            .any(|e| e["w"] == "360" && e["id"] == "editor" && e["value"] == "z")
    }));
    click(&live, 0, 40.0, 315.0);
    live.wait("password focus omits caret", Duration::from_secs(5), |s| {
        s.devices[0].text_input.is_none()
    });
    live.send(Command::Ime {
        device: 0,
        target: editor.target,
        action: ImeAction::Commit {
            text: "stale".into(),
        },
    });
    click(&live, 0, 40.0, 220.0);
    let editor_again = live
        .wait("editable focus restored", Duration::from_secs(5), |s| {
            s.devices[0]
                .text_input
                .is_some_and(|t| t.target != editor.target)
        })
        .devices[0]
        .text_input
        .unwrap();
    click(&live, 0, 235.0, 365.0);
    live.wait("right-aligned input caret", Duration::from_secs(5), |s| {
        s.devices[0]
            .text_input
            .is_some_and(|t| t.target != editor_again.target && t.caret.y > 350.0)
    });
    thread::sleep(Duration::from_millis(150));
    let right = live.session().status().devices[0].text_input.unwrap();
    assert!(
        (220.0..250.0).contains(&right.caret.x),
        "right caret: {right:?}"
    );
    click(&live, 0, 140.0, 415.0);
    live.wait("center-aligned input caret", Duration::from_secs(5), |s| {
        s.devices[0]
            .text_input
            .is_some_and(|t| t.target != right.target && t.caret.y > 400.0)
    });
    thread::sleep(Duration::from_millis(150));
    let center = live.session().status().devices[0].text_input.unwrap();
    assert!(
        (125.0..155.0).contains(&center.caret.x),
        "center caret: {center:?}"
    );
    live.send(Command::SetVisible {
        device: 0,
        visible: false,
    });
    live.wait("hidden IME target", Duration::from_secs(5), |s| {
        s.devices[0].text_input.is_none()
    });
    live.send(Command::Ime {
        device: 0,
        target: center.target,
        action: ImeAction::Commit {
            text: "hidden".into(),
        },
    });
    thread::sleep(Duration::from_millis(300));
    assert!(
        !events(&fixture, "input")
            .iter()
            .any(|e| e["value"].contains("stale") || e["value"].contains("hidden"))
    );
    assert!(
        events(&fixture, "input")
            .iter()
            .all(|e| e["id"] != "secret")
    );
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_ime_rejects_scripted_focus_and_selection_after_synthetic_events() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/ime-attacks")));
    let status = live.wait("editable attack target", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/ime-attacks") && s.devices[0].text_input.is_some()
    });
    let mut target = status.devices[0].text_input.unwrap().target;
    for key in ["F2", "F3", "F4"] {
        live.send(Command::Key {
            device: 0,
            key: KeyInput::from_key(&key.to_lowercase(), None, Modifiers::default(), true).unwrap(),
        });
        // The old target reaches the worker immediately after the browser key,
        // before the observer's next animation-frame geometry report.
        live.send(Command::Ime {
            device: 0,
            target,
            action: ImeAction::Commit { text: "!".into() },
        });
        assert!(
            fixture.wait_for(Duration::from_secs(5), |_| {
                events(&fixture, "attack")
                    .iter()
                    .any(|event| event["w"] == "360" && event["key"] == key)
            }),
            "{key} handler did not execute"
        );
        let next = live.wait("fresh target after script", Duration::from_secs(5), |s| {
            s.devices[0]
                .text_input
                .is_some_and(|state| state.target != target)
        });
        target = next.devices[0].text_input.unwrap().target;
        assert!(
            events(&fixture, "input")
                .iter()
                .filter(|event| event["w"] == "360")
                .all(|event| !event["value"].contains('!')),
            "old IME target inserted text after {key}"
        );
    }
    // Genuine CDP commits remain repeatable without intervening key-up events.
    for _ in 0..2 {
        live.send(Command::Ime {
            device: 0,
            target,
            action: ImeAction::Commit { text: "✓".into() },
        });
    }
    assert!(fixture.wait_for(Duration::from_secs(5), |_| {
        events(&fixture, "input")
            .iter()
            .any(|event| event["w"] == "360" && event["value"].matches('✓').count() == 2)
    }));
    click(&live, 0, 40.0, 210.0);
    let tall = live
        .wait("tall input caret", Duration::from_secs(5), |s| {
            s.devices[0]
                .text_input
                .is_some_and(|state| state.target != target && state.caret.y > 160.0)
        })
        .devices[0]
        .text_input
        .unwrap();
    assert!(
        (188.0..215.0).contains(&tall.caret.y),
        "tall input caret: {tall:?}"
    );
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_ime_waits_for_a_slow_page_without_dropping_or_reordering_input() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/ime-busy")));
    let status = live.wait("busy editable", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/ime-busy") && s.devices[0].text_input.is_some()
    });
    let target = status.devices[0].text_input.unwrap().target;
    // The page answers F2 after 400 ms. The identity check of the commits
    // waits for that answer; a fixed 250 ms bound dropped them.
    live.send(Command::Key {
        device: 0,
        key: KeyInput::from_key("f2", None, Modifiers::default(), true).unwrap(),
    });
    for _ in 0..2 {
        live.send(Command::Ime {
            device: 0,
            target,
            action: ImeAction::Commit { text: "✓".into() },
        });
    }
    for down in [true, false] {
        live.send(Command::Key {
            device: 0,
            key: KeyInput::from_key("x", Some("x"), Modifiers::default(), down).unwrap(),
        });
    }
    let values = || {
        events(&fixture, "input")
            .into_iter()
            .filter(|event| event["w"] == "360")
            .map(|event| event["value"].clone())
            .collect::<Vec<_>>()
    };
    assert!(
        fixture.wait_for(Duration::from_secs(10), |_| values()
            .iter()
            .any(|value| value == "✓✓x")),
        "input: {:?}",
        values()
    );
    // Reports can arrive out of order; each value is still a prefix.
    assert!(
        values()
            .iter()
            .all(|value| "✓✓x".starts_with(value.as_str())),
        "input overtook a waiting commit: {:?}",
        values()
    );
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_input_held_behind_an_ime_check_is_dropped_when_hidden() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/ime-busy")));
    let status = live.wait("busy editable", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/ime-busy") && s.devices[0].text_input.is_some()
    });
    let target = status.devices[0].text_input.unwrap().target;
    // The page answers F3 after 1.5 s. The commit waits for that answer and
    // the key waits behind the commit when the device is hidden.
    live.send(Command::Key {
        device: 0,
        key: KeyInput::from_key("f3", None, Modifiers::default(), true).unwrap(),
    });
    live.send(Command::Ime {
        device: 0,
        target,
        action: ImeAction::Commit { text: "b".into() },
    });
    for down in [true, false] {
        live.send(Command::Key {
            device: 0,
            key: KeyInput::from_key("x", Some("x"), Modifiers::default(), down).unwrap(),
        });
    }
    for visible in [false, true] {
        live.send(Command::SetVisible { device: 0, visible });
    }
    assert!(fixture.wait_for(Duration::from_secs(10), |_| {
        events(&fixture, "free")
            .iter()
            .any(|event| event["w"] == "360" && event["key"] == "F3")
    }));
    let shown = live
        .wait("editable after showing", Duration::from_secs(5), |s| {
            s.devices[0]
                .text_input
                .is_some_and(|state| state.target != target)
        })
        .devices[0]
        .text_input
        .unwrap();
    thread::sleep(Duration::from_millis(500));
    let typed = |fixture: &Fixture| {
        events(fixture, "input")
            .into_iter()
            .filter(|event| event["w"] == "360")
            .map(|event| event["value"].clone())
            .collect::<Vec<_>>()
    };
    assert!(
        typed(&fixture).is_empty(),
        "sent later: {:?}",
        typed(&fixture)
    );
    // Nothing waits any longer: new input arrives alone.
    live.send(Command::Ime {
        device: 0,
        target: shown.target,
        action: ImeAction::Commit { text: "c".into() },
    });
    assert!(
        fixture.wait_for(Duration::from_secs(5), |fixture| typed(fixture) == ["c"]),
        "input: {:?}",
        typed(&fixture)
    );
    live.close();
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

/// Keys in the order the page's listener saw them. Each report is its own
/// request, so reports can reach the fixture out of order.
fn reported_keys(events: &[HashMap<String, String>]) -> Vec<String> {
    let mut events: Vec<&HashMap<String, String>> = events.iter().collect();
    events.sort_by_key(|event| {
        event["n"]
            .parse::<u32>()
            .expect("key reports carry their order")
    });
    events.iter().map(|event| event["key"].clone()).collect()
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
        reported_keys(&desktop("keydown")) == ["é", "F2"]
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
    assert_eq!(
        reported_keys(&events(&fixture, "keydown")),
        ["o", "k"],
        "browser keys must not reach pages"
    );
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
        |s| {
            // Device 0 already had /next in the preceding case. Its old
            // status is not evidence that this new navigation has happened.
            count(&fixture, "/next") >= 3
                && s.devices[0].url == fixture.url("/next")
                && s.devices[1].url == fixture.url("/forged")
        },
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

/// Loads `path` on every device and waits until all of them show it.
fn load_all(live: &Live, fixture: &Fixture, path: &str) {
    live.send(Command::NavigateAll {
        url: fixture.url(path),
    });
    live.wait(path, Duration::from_secs(10), |s| loaded(s, fixture, path));
}

fn sync_links(live: &Live, navigation: bool) {
    live.send(Command::SetSync(SyncSettings {
        navigation,
        scroll: false,
    }));
    live.wait("sync setting", Duration::from_secs(5), |s| {
        s.sync.navigation == navigation
    });
}

/// Requests whose path, including the query, starts with `prefix`.
fn count_prefix(fixture: &Fixture, prefix: &str) -> usize {
    fixture
        .requests()
        .iter()
        .filter(|request| request.path.starts_with(prefix))
        .count()
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_link_sync_follows_redirects_and_fragments_with_the_link_url() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/redirect-chain")));
    live.wait("pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/redirect-chain")
    });
    sync_links(&live, true);
    let away = fixture.url("/landed").replace("127.0.0.1", "localhost");
    for (start, link, landed) in [
        ("/redirect-chain", "/r?status=307&", fixture.url("/landed")),
        (
            "/redirect-fragment",
            "/r?status=302&to=%2Flanded%23part",
            fixture.url("/landed#part"),
        ),
        ("/redirect-away", "/r?status=302&to=http", away),
        ("/fragment-link", "/landed", fixture.url("/landed#part")),
    ] {
        if start != "/redirect-chain" {
            load_all(&live, &fixture, start);
        }
        let (links, landings) = (
            count_prefix(&fixture, link),
            count_prefix(&fixture, "/landed"),
        );
        click(&live, 0, 100.0, 120.0);
        live.wait(start, Duration::from_secs(10), |s| {
            s.devices[..2]
                .iter()
                .all(|device| device.url == landed && !device.loading)
        });
        thread::sleep(Duration::from_millis(500));
        assert_eq!(
            live.session().status().devices[2].url,
            fixture.url(start),
            "{start} left its session"
        );
        // The tablet requested the link itself and followed its own redirects.
        assert_eq!(count_prefix(&fixture, link) - links, 2, "{start}: link");
        assert_eq!(
            count_prefix(&fixture, "/landed") - landings,
            2,
            "{start}: destination"
        );
    }
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_same_document_link_navigations_sync_within_the_session() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/hash")));
    live.wait("pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/hash")
    });
    sync_links(&live, true);
    // A hash link scrolls the tablet within its document: nothing is requested.
    click(&live, 0, 100.0, 120.0);
    let hashed = fixture.url("/hash#part");
    live.wait(
        "tablet follows the hash link",
        Duration::from_secs(10),
        |s| s.devices[0].url == hashed && s.devices[1].url == hashed,
    );
    thread::sleep(Duration::from_millis(500));
    assert_eq!(count(&fixture, "/hash"), 3);
    assert_eq!(live.session().status().devices[2].url, fixture.url("/hash"));

    // History API and Navigation API routers: the tablet loads the route.
    for (start, route, keyboard) in [
        ("/spa", "/spa-route", false),
        ("/spa-late", "/spa-late-route", false),
        ("/navigation-api", "/nav-route", false),
        ("/spa-keyboard", "/spa-key-route", true),
    ] {
        load_all(&live, &fixture, start);
        if keyboard {
            for down in [true, false] {
                live.send(Command::Key {
                    device: 0,
                    key: KeyInput::from_key("enter", None, Modifiers::default(), down).unwrap(),
                });
            }
        } else {
            click(&live, 0, 100.0, 120.0);
        }
        let url = fixture.url(route);
        live.wait(route, Duration::from_secs(10), |s| {
            s.devices[..2]
                .iter()
                .all(|device| device.url == url && !device.loading)
        });
        thread::sleep(Duration::from_millis(500));
        assert_eq!(count(&fixture, route), 1, "only the tablet loads {route}");
        assert_eq!(live.session().status().devices[2].url, fixture.url(start));
    }
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_script_and_stale_same_document_changes_never_sync() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/")));
    live.wait("pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/")
    });
    sync_links(&live, true);
    // Every device's own script changes its URL; nothing follows.
    live.send(Command::NavigateAll {
        url: fixture.url("/spa-script"),
    });
    let route = fixture.url("/spa-script-route");
    live.wait("scripts change URLs", Duration::from_secs(10), |s| {
        s.devices.iter().all(|device| device.url == route)
    });
    thread::sleep(Duration::from_millis(700));
    assert_eq!(count(&fixture, "/spa-script-route"), 0);

    // A button, script hash changes, a script's click on a hash link, and a
    // link whose router pushes another URL.
    for (start, destination) in [
        ("/spa-button", "/spa-button-route"),
        ("/hash-script", "/hash-script#part"),
        ("/scripted-hash", "/scripted-hash#part"),
        ("/spa-other", "/spa-b"),
    ] {
        load_all(&live, &fixture, start);
        click(&live, 0, 100.0, 120.0);
        live.wait(destination, Duration::from_secs(10), |s| {
            s.devices[0].url == fixture.url(destination)
        });
        thread::sleep(Duration::from_millis(700));
        assert_eq!(
            live.session().status().devices[1].url,
            fixture.url(start),
            "{start} synchronized"
        );
        if !destination.contains('#') {
            assert_eq!(count(&fixture, destination), 0, "{destination} was loaded");
        }
    }

    // The second link's router pushes the first link's URL.
    load_all(&live, &fixture, "/spa-superseded");
    click(&live, 0, 100.0, 120.0);
    click(&live, 0, 100.0, 320.0);
    live.wait("the first link's URL", Duration::from_secs(10), |s| {
        s.devices[0].url == fixture.url("/spa-x")
    });
    thread::sleep(Duration::from_millis(700));
    assert_eq!(
        live.session().status().devices[1].url,
        fixture.url("/spa-superseded")
    );
    assert_eq!(count(&fixture, "/spa-x"), 0);

    // The router pushes after the phone was hidden.
    load_all(&live, &fixture, "/spa-hidden");
    click(&live, 0, 100.0, 120.0);
    live.send(Command::SetVisible {
        device: 0,
        visible: false,
    });
    live.wait("hidden route", Duration::from_secs(10), |s| {
        s.devices[0].url == fixture.url("/spa-hidden-route")
    });
    live.send(Command::SetVisible {
        device: 0,
        visible: true,
    });
    live.wait("phone visible", Duration::from_secs(5), |s| {
        s.devices[0].streaming
    });
    thread::sleep(Duration::from_millis(700));
    assert_eq!(
        live.session().status().devices[1].url,
        fixture.url("/spa-hidden")
    );
    assert_eq!(count(&fixture, "/spa-hidden-route"), 0);

    // The link was activated before sync was switched on. The router pushes
    // 1.5 s after the click; the activation arrives within milliseconds.
    sync_links(&live, false);
    load_all(&live, &fixture, "/spa-late");
    click(&live, 0, 100.0, 120.0);
    thread::sleep(Duration::from_millis(300));
    sync_links(&live, true);
    live.wait("late route", Duration::from_secs(10), |s| {
        s.devices[0].url == fixture.url("/spa-late-route")
    });
    thread::sleep(Duration::from_millis(700));
    assert_eq!(
        live.session().status().devices[1].url,
        fixture.url("/spa-late")
    );
    assert_eq!(count(&fixture, "/spa-late-route"), 0);
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_subframe_navigations_never_sync() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/frames")));
    live.wait("pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/frames")
    });
    assert!(
        fixture.wait_for(Duration::from_secs(10), |fixture| count(fixture, "/frame")
            == 3)
    );
    sync_links(&live, true);
    // The phone's frame follows a hash link, a History API link, a link, and
    // a main-document link that targets the frame.
    click(&live, 0, 100.0, 370.0);
    click(&live, 0, 100.0, 420.0);
    assert!(
        fixture.wait_for(Duration::from_secs(10), |fixture| {
            !events(fixture, "framehash").is_empty() && !events(fixture, "framespa").is_empty()
        }),
        "frame reports {:?}",
        fixture.requests()
    );
    click(&live, 0, 100.0, 320.0);
    assert!(fixture.wait_for(Duration::from_secs(10), |fixture| count(
        fixture,
        "/frame-dest"
    ) == 1));
    click(&live, 0, 100.0, 540.0);
    assert!(fixture.wait_for(Duration::from_secs(10), |fixture| count(
        fixture,
        "/frame-dest"
    ) == 2));
    thread::sleep(Duration::from_millis(700));
    let status = live.session().status();
    assert!(
        status
            .devices
            .iter()
            .all(|device| device.url == fixture.url("/frames")),
        "{status:#?}"
    );
    for kind in ["framehash", "framespa"] {
        assert!(
            events(&fixture, kind)
                .iter()
                .all(|event| event["w"] == "360")
        );
    }
    assert_eq!(count(&fixture, "/frame-dest"), 2);

    // A main-frame link still synchronizes afterwards.
    click(&live, 0, 100.0, 120.0);
    let next = fixture.url("/next");
    live.wait("tablet follows", Duration::from_secs(10), |s| {
        s.devices[0].url == next && s.devices[1].url == next
    });
    assert_eq!(count(&fixture, "/next"), 2);
    assert_eq!(
        live.session().status().devices[2].url,
        fixture.url("/frames")
    );
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_cancelled_and_superseded_link_navigations_sync_at_most_the_latest() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/nocontent-link")));
    live.wait("pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/nocontent-link")
    });
    sync_links(&live, true);
    // No document: 204, a redirect to 204, a failed load, a load the page stops.
    for (start, requested, settle) in [
        ("/nocontent-link", "/nocontent", 700),
        ("/redirect-empty", "/nocontent", 700),
        ("/dropped-link", "/dropped", 1500),
        ("/stop-link", "/slow", 3500),
    ] {
        if start != "/nocontent-link" {
            load_all(&live, &fixture, start);
        }
        let before = count(&fixture, requested);
        click(&live, 0, 100.0, 120.0);
        thread::sleep(Duration::from_millis(settle));
        let status = live.session().status();
        assert_eq!(
            status.devices[1].url,
            fixture.url(start),
            "{start} synchronized"
        );
        assert!(!status.devices[1].loading, "{start}: {status:#?}");
        assert!(count(&fixture, requested) > before, "{start} did not load");
    }
    assert_eq!(
        count(&fixture, "/slow"),
        1,
        "the tablet never requested /slow"
    );

    // A second link replaces the first; only the second synchronizes.
    load_all(&live, &fixture, "/two-links");
    click(&live, 0, 100.0, 120.0);
    thread::sleep(Duration::from_millis(300));
    click(&live, 0, 100.0, 320.0);
    let second = fixture.url("/landed?second");
    live.wait(
        "tablet follows the second link",
        Duration::from_secs(10),
        |s| {
            s.devices[..2]
                .iter()
                .all(|device| device.url == second && !device.loading)
        },
    );
    thread::sleep(Duration::from_millis(3500));
    assert_eq!(count(&fixture, "/slow"), 2);
    assert_eq!(count(&fixture, "/landed?second"), 2);

    // Go replaces a link that is still loading: every device loads Go once.
    load_all(&live, &fixture, "/go-supersede");
    click(&live, 0, 100.0, 120.0);
    thread::sleep(Duration::from_millis(300));
    live.send(Command::NavigateAll {
        url: fixture.url("/landed?go"),
    });
    live.wait("Go", Duration::from_secs(10), |s| {
        loaded(s, &fixture, "/landed?go")
    });
    thread::sleep(Duration::from_millis(3500));
    assert_eq!(count(&fixture, "/slow"), 3);
    assert_eq!(count(&fixture, "/landed?go"), 3);
    assert!(loaded(&live.session().status(), &fixture, "/landed?go"));
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_popups_are_closed_before_they_act_and_open_only_on_request() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/popups")));
    live.wait("pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/popups")
    });
    let alive = |fixture: &Fixture| events(fixture, "alive").len();
    for (y, query, closed) in [
        (120.0, "opener", 1),
        (180.0, "blank", 2),
        (240.0, "named", 3),
    ] {
        click(&live, 0, 100.0, y);
        let popup = live
            .wait(query, Duration::from_secs(10), |s| {
                s.devices[0].popups == closed
            })
            .devices[0]
            .popup
            .clone()
            .unwrap();
        assert_eq!(popup.url, fixture.url(&format!("/popup-page?{query}")));
        assert!(popup.openable);
        // Past the popup's 300 ms attempt to move its opener, and its pings.
        thread::sleep(Duration::from_millis(1000));
        let status = live.session().status();
        assert_eq!(
            status.devices[0].url,
            fixture.url("/popups"),
            "{query} moved its opener"
        );
        assert_eq!(count(&fixture, "/hijacked"), 0);
        assert_eq!(
            alive(&fixture),
            0,
            "{query} kept running: {:?}",
            events(&fixture, "alive")
        );
    }
    let status = live.session().status();
    assert!(status.devices[1..].iter().all(|device| device.popups == 0));
    let named = status.devices[0].popup.clone().unwrap();

    // Opening a report loads its URL in the phone alone, once, with no opener.
    live.send(Command::OpenPopup {
        device: 0,
        token: named.token,
    });
    let url = fixture.url("/popup-page?named");
    live.wait(
        "the popup's page in the phone",
        Duration::from_secs(10),
        |s| s.devices[0].url == url && !s.devices[0].loading && s.devices[0].popup.is_none(),
    );
    assert!(fixture.wait_for(Duration::from_secs(5), |fixture| alive(fixture) > 0));
    assert!(
        events(&fixture, "alive")
            .iter()
            .all(|event| event["opener"] == "false")
    );
    live.send(Command::OpenPopup {
        device: 0,
        token: named.token,
    });
    thread::sleep(Duration::from_millis(700));
    let status = live.session().status();
    assert_eq!(status.devices[1].url, fixture.url("/popups"));
    assert_eq!(count(&fixture, "/hijacked"), 0);
    let loads = fixture
        .requests()
        .iter()
        .filter(|request| request.path == "/popup-page?named")
        .count();
    assert!(
        loads <= 2,
        "the popup's own first request and one explicit load: {loads}"
    );
    live.close();
}

/// Reports of `DIALOG_PAGE` and `DIRTY_PAGE` dialogs, as `result` values.
fn dialog_results(fixture: &Fixture) -> Vec<String> {
    events(fixture, "dialog")
        .into_iter()
        .filter_map(|event| event.get("result").cloned())
        .collect()
}

fn answer(live: &Live, device: usize, token: u64, accept: bool, text: Option<&str>) {
    live.send(Command::AnswerDialog {
        device,
        token,
        accept,
        text: text.map(str::to_owned),
    });
}

/// The frame count of `device` once no frame has arrived for 700 ms. A frame
/// already in flight when the page stopped can land after it stopped.
fn settled_frames(live: &Live, device: usize) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut frames = live.session().status().devices[device].frames;
    let mut since = Instant::now();
    loop {
        thread::sleep(Duration::from_millis(50));
        let now = live.session().status().devices[device].frames;
        if now != frames {
            (frames, since) = (now, Instant::now());
        } else if since.elapsed() >= Duration::from_millis(700) {
            return frames;
        }
        assert!(
            Instant::now() < deadline,
            "frames of device {device} kept arriving: {frames}"
        );
    }
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_dialogs_wait_for_an_explicit_answer() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/dialogs")));
    live.wait("pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/dialogs")
    });
    let open = |live: &Live, y: f64, kind: DialogKind| {
        // A click before the previous dialog's closing is read is dropped.
        live.wait("no dialog", Duration::from_secs(5), |s| {
            s.devices[0].dialog.is_none()
        });
        click(live, 0, 100.0, y);
        let dialog = live
            .wait("the dialog", Duration::from_secs(10), |s| {
                s.devices[0].dialog.is_some()
            })
            .devices[0]
            .dialog
            .clone()
            .unwrap();
        assert_eq!(dialog.kind, kind);
        dialog
    };

    // alert: the page stops, and so do its frames.
    let alert = open(&live, 120.0, DialogKind::Alert);
    assert_eq!(alert.message, "hello from the page");
    let frames = settled_frames(&live, 0);
    thread::sleep(Duration::from_millis(700));
    assert_eq!(live.session().status().devices[0].frames, frames);
    // Typing does not answer it, and Go does not navigate past it.
    for down in [true, false] {
        live.send(Command::Key {
            device: 0,
            key: KeyInput::from_key("a", Some("a"), Modifiers::default(), down).unwrap(),
        });
    }
    live.send(Command::NavigateAll {
        url: fixture.url("/next"),
    });
    live.wait("peers navigate", Duration::from_secs(10), |s| {
        s.devices[1..]
            .iter()
            .all(|device| device.url == fixture.url("/next") && !device.loading)
    });
    thread::sleep(Duration::from_millis(500));
    let status = live.session().status();
    assert_eq!(
        status.devices[0].dialog.as_ref().map(|d| d.token),
        Some(alert.token)
    );
    assert_eq!(status.devices[0].url, fixture.url("/dialogs"));
    assert_eq!(status.devices[0].error.as_deref(), Some(DIALOG_OPEN));
    assert!(
        dialog_results(&fixture).is_empty(),
        "the dialog was answered"
    );
    assert_eq!(count(&fixture, "/next"), 2);
    // A stale token answers nothing; the user's answer does.
    answer(&live, 0, alert.token + 1000, true, None);
    thread::sleep(Duration::from_millis(400));
    assert!(live.session().status().devices[0].dialog.is_some());
    answer(&live, 0, alert.token, true, None);
    live.wait("alert answered", Duration::from_secs(5), |s| {
        s.devices[0].dialog.is_none() && s.devices[0].error.is_none()
    });
    assert!(fixture.wait_for(Duration::from_secs(5), |fixture| {
        dialog_results(fixture) == ["alert closed"]
    }));
    live.wait("frames resume", Duration::from_secs(5), |s| {
        s.devices[0].frames > frames
    });
    assert_eq!(
        live.session().status().devices[0].url,
        fixture.url("/dialogs")
    );
    // Input reaches the page again. The caret report shows the field has
    // focus before typing, as a key can overtake a click in Chromium.
    click(&live, 0, 100.0, 315.0);
    live.wait("the field's caret", Duration::from_secs(5), |s| {
        s.devices[0].text_input.is_some()
    });
    for down in [true, false] {
        live.send(Command::Key {
            device: 0,
            key: KeyInput::from_key("b", Some("b"), Modifiers::default(), down).unwrap(),
        });
    }
    assert!(fixture.wait_for(Duration::from_secs(5), |fixture| {
        events(fixture, "input")
            .iter()
            .any(|event| event["v"] == "b")
    }));

    // confirm: cancel, then accept.
    let confirm = open(&live, 180.0, DialogKind::Confirm);
    assert_eq!(confirm.message, "Continue?");
    answer(&live, 0, confirm.token, false, None);
    assert!(fixture.wait_for(Duration::from_secs(5), |fixture| {
        dialog_results(fixture).last().map(String::as_str) == Some("confirm=false")
    }));
    let confirm = open(&live, 180.0, DialogKind::Confirm);
    answer(&live, 0, confirm.token, true, None);
    assert!(fixture.wait_for(Duration::from_secs(5), |fixture| {
        dialog_results(fixture).last().map(String::as_str) == Some("confirm=true")
    }));

    // prompt: the user's text, then cancel.
    let prompt = open(&live, 240.0, DialogKind::Prompt);
    assert_eq!(prompt.message, "Your name?");
    assert_eq!(prompt.default_text, "guest");
    answer(&live, 0, prompt.token, true, Some("Broxser"));
    assert!(fixture.wait_for(Duration::from_secs(5), |fixture| {
        dialog_results(fixture).last().map(String::as_str) == Some("prompt=Broxser")
    }));
    let prompt = open(&live, 240.0, DialogKind::Prompt);
    answer(&live, 0, prompt.token, false, None);
    assert!(fixture.wait_for(Duration::from_secs(5), |fixture| {
        dialog_results(fixture).last().map(String::as_str) == Some("prompt=null")
    }));
    assert_eq!(
        dialog_results(&fixture),
        [
            "alert closed",
            "confirm=false",
            "confirm=true",
            "prompt=Broxser",
            "prompt=null"
        ]
    );
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_dialog_survives_hide_show_scroll_and_zoom() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/dialogs")));
    live.wait("pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/dialogs")
    });
    click(&live, 0, 100.0, 120.0);
    let alert = live
        .wait("the alert", Duration::from_secs(10), |s| {
            s.devices[0].dialog.is_some()
        })
        .devices[0]
        .dialog
        .clone()
        .unwrap();
    assert_eq!(alert.kind, DialogKind::Alert);
    // Hiding, showing, scrolling the device off the canvas and back, and a
    // new frame size all send commands for the device while its page waits
    // for the user. Only the browser may be expected to answer them: one
    // left to the blocked page would stop the runtime at the command limit.
    let streaming = |live: &Live, what: &str, on: bool| {
        live.wait(what, Duration::from_secs(5), |s| {
            s.devices[0].streaming == on
        });
    };
    live.send(Command::SetVisible {
        device: 0,
        visible: false,
    });
    streaming(&live, "hidden", false);
    live.send(Command::SetVisible {
        device: 0,
        visible: true,
    });
    streaming(&live, "shown", true);
    live.send(Command::SetOnScreen {
        device: 0,
        on_screen: false,
    });
    streaming(&live, "off screen", false);
    live.send(Command::SetOnScreen {
        device: 0,
        on_screen: true,
    });
    streaming(&live, "on screen", true);
    live.send(Command::SetFrameLimit {
        device: 0,
        width: 400,
        height: 700,
    });
    thread::sleep(Limits::default().command + Duration::from_secs(1));
    let status = live.session().status();
    assert!(running(&status), "{:?}", status.runtime);
    assert!(status.devices[0].streaming);
    assert_eq!(
        status.devices[0].dialog.as_ref().map(|d| d.token),
        Some(alert.token)
    );
    assert!(
        dialog_results(&fixture).is_empty(),
        "the dialog was answered"
    );
    answer(&live, 0, alert.token, true, None);
    live.wait("alert answered", Duration::from_secs(5), |s| {
        s.devices[0].dialog.is_none()
    });
    assert!(fixture.wait_for(Duration::from_secs(5), |fixture| {
        dialog_results(fixture) == ["alert closed"]
    }));
    // Input reaches the page again.
    click(&live, 0, 100.0, 315.0);
    live.wait("the field's caret", Duration::from_secs(5), |s| {
        s.devices[0].text_input.is_some()
    });
    for down in [true, false] {
        live.send(Command::Key {
            device: 0,
            key: KeyInput::from_key("c", Some("c"), Modifiers::default(), down).unwrap(),
        });
    }
    assert!(fixture.wait_for(Duration::from_secs(5), |fixture| {
        events(fixture, "input")
            .iter()
            .any(|event| event["v"] == "c")
    }));
    assert!(running(&live.session().status()));
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_beforeunload_dialog_needs_an_explicit_leave_or_stay() {
    let fixture = fixture();
    let limits = Limits {
        load: Duration::from_secs(3),
        ..Limits::default()
    };
    let live = Live::start_with(workspace(fixture.url("/dirty")), limits);
    live.wait("pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/dirty")
    });
    let dirty = |live: &Live| {
        let typed = events(&fixture, "input").len();
        click(live, 0, 100.0, 115.0);
        // Chromium routes pointer events through its compositor and keys to
        // the main thread directly; a key sent at once can overtake the click.
        // The caret report shows the field has focus.
        live.wait("the field's caret", Duration::from_secs(5), |s| {
            s.devices[0].text_input.is_some()
        });
        for down in [true, false] {
            live.send(Command::Key {
                device: 0,
                key: KeyInput::from_key("x", Some("x"), Modifiers::default(), down).unwrap(),
            });
        }
        assert!(
            fixture.wait_for(Duration::from_secs(5), |fixture| {
                events(fixture, "input")
                    .get(typed)
                    .is_some_and(|event| event["v"] == "x" && event["w"] == "360")
            }),
            "typing did not reach the phone's field: input {:?} down {:?} keydown {:?} status {:#?}",
            events(&fixture, "input"),
            events(&fixture, "down"),
            events(&fixture, "keydown"),
            live.session().status().devices[0]
        );
    };
    let asks = |live: &Live| {
        let dialog = live
            .wait("the question", Duration::from_secs(10), |s| {
                s.devices[0].dialog.is_some()
            })
            .devices[0]
            .dialog
            .clone()
            .unwrap();
        assert_eq!(dialog.kind, DialogKind::BeforeUnload);
        dialog
    };

    // Go: the peers leave; the dirty phone asks and waits past the load limit.
    dirty(&live);
    live.send(Command::NavigateAll {
        url: fixture.url("/next"),
    });
    let question = asks(&live);
    live.wait("peers leave", Duration::from_secs(10), |s| {
        s.devices[1..]
            .iter()
            .all(|device| device.url == fixture.url("/next") && !device.loading)
    });
    thread::sleep(limits.load + Duration::from_secs(1));
    let status = live.session().status();
    assert_eq!(
        status.devices[0].dialog.as_ref().map(|d| d.token),
        Some(question.token)
    );
    assert_eq!(status.devices[0].error, None, "the navigation was stopped");
    assert_eq!(status.devices[0].url, fixture.url("/dirty"));
    assert_eq!(count(&fixture, "/next"), 2);
    // Stay: the page keeps its text and reports no failure.
    answer(&live, 0, question.token, false, None);
    live.wait("stayed", Duration::from_secs(5), |s| {
        s.devices[0].dialog.is_none() && !s.devices[0].loading
    });
    thread::sleep(Duration::from_millis(500));
    let status = live.session().status();
    assert_eq!(status.devices[0].error, None, "{status:#?}");
    assert_eq!(status.devices[0].url, fixture.url("/dirty"));
    assert_eq!(count(&fixture, "/next"), 2);
    // Leave: the same Go, answered the other way, navigates. Go loads every
    // device once, so the peers load /next again.
    live.send(Command::NavigateAll {
        url: fixture.url("/next"),
    });
    let question = asks(&live);
    answer(&live, 0, question.token, true, None);
    live.wait("left", Duration::from_secs(10), |s| {
        s.devices[0].url == fixture.url("/next") && !s.devices[0].loading
    });
    assert_eq!(count(&fixture, "/next"), 5);

    // The page's own link asks the same question.
    live.send(Command::NavigateAll {
        url: fixture.url("/dirty"),
    });
    live.wait("dirty pages", Duration::from_secs(10), |s| {
        loaded(s, &fixture, "/dirty")
    });
    dirty(&live);
    click(&live, 0, 100.0, 180.0);
    let question = asks(&live);
    answer(&live, 0, question.token, false, None);
    live.wait("stayed again", Duration::from_secs(5), |s| {
        s.devices[0].dialog.is_none()
    });
    thread::sleep(Duration::from_millis(500));
    assert_eq!(
        live.session().status().devices[0].url,
        fixture.url("/dirty")
    );
    assert_eq!(count(&fixture, "/next"), 5);
    click(&live, 0, 100.0, 180.0);
    let question = asks(&live);
    answer(&live, 0, question.token, true, None);
    live.wait("followed the link", Duration::from_secs(10), |s| {
        s.devices[0].url == fixture.url("/next")
    });
    assert_eq!(count(&fixture, "/next"), 6);
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
fn live_off_screen_devices_pause_frames_keep_input_and_resume_fresh() {
    // A static page that changes once, when the test says so.
    let change = Arc::new(AtomicBool::new(false));
    let changed = Arc::clone(&change);
    let fixture = Fixture::start(move |request, _| {
        Reply::Html {
        body: match request.path.as_str() {
            "/change" if changed.load(Ordering::SeqCst) => "yes".into(),
            "/change" => "no".into(),
            path if path.starts_with("/event") => String::new(),
            _ => "<body style='margin:0;background:#fff'><script>
                const report = (kind) => fetch('/event?' + new URLSearchParams({kind, w: innerWidth}));
                addEventListener('mousedown', () => report('down'), true);
                const poll = setInterval(() => fetch('/change').then(r => r.text()).then(answer => {
                  if (answer !== 'yes') return;
                  clearInterval(poll);
                  document.body.style.background = '#f00';
                  report('changed');
                }), 100);
                </script>"
                .into(),
        },
        delay: Duration::ZERO,
        cookie: None,
    }
    });
    let live = Live::start(workspace(fixture.url("/")));
    live.wait("pages", Duration::from_secs(30), |status| {
        loaded(status, &fixture, "/")
    });
    thread::sleep(Duration::from_millis(500));
    live.send(Command::SetOnScreen {
        device: 0,
        on_screen: false,
    });
    live.wait("phone paused", Duration::from_secs(5), |status| {
        !status.devices[0].streaming
    });
    thread::sleep(Duration::from_millis(500));
    let paused = live.session().status().devices[0].frames;
    // Unlike a hidden device, a device off screen takes input.
    click(&live, 0, 40.0, 300.0);
    assert!(
        fixture.wait_for(Duration::from_secs(5), |fixture| {
            events(fixture, "down")
                .iter()
                .any(|event| event["w"] == "360")
        }),
        "an off-screen device dropped a click"
    );
    // Showing a device does not resume it while it is off screen.
    for visible in [false, true] {
        live.send(Command::SetVisible { device: 0, visible });
    }
    change.store(true, Ordering::SeqCst);
    assert!(fixture.wait_for(Duration::from_secs(5), |fixture| {
        events(fixture, "changed")
            .iter()
            .any(|event| event["w"] == "360")
    }));
    thread::sleep(Duration::from_millis(500));
    let status = live.session().status();
    assert!(!status.devices[0].streaming, "{status:#?}");
    assert_eq!(status.devices[0].frames, paused, "frames while off screen");
    assert!(live.session().take_frame(0).is_none());
    // The page is static again; resuming still brings a frame of its change.
    live.send(Command::SetOnScreen {
        device: 0,
        on_screen: true,
    });
    live.wait("phone resumes", Duration::from_secs(5), |status| {
        status.devices[0].streaming && status.devices[0].frames > paused
    });
    assert!(live.session().take_frame(0).is_some());
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

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_downloads_and_file_choosers_are_refused_and_reported() {
    let fixture = fixture();
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let downloads_before = download_names(&home.join("Downloads"));
    let live = Live::start(workspace(fixture.url("/downloads")));
    live.wait("pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/downloads")
    });
    assert!(
        fixture.wait_for(Duration::from_secs(10), |f| events(f, "frame").len() == 3),
        "the cross-site frames did not load"
    );
    let requested = |path: &str| {
        fixture
            .requests()
            .iter()
            .filter(|request| request.path == path)
            .count()
    };
    // Each click starts one download on its own device; the browser refuses
    // it and that device reports it. The desktop's link is in a cross-site
    // frame that another renderer process runs.
    for (device, y, filename, url) in [
        (
            0,
            120.0,
            "report.pdf",
            fixture.url("/download?name=report.pdf"),
        ),
        (1, 180.0, "notes.txt", fixture.url("/download")),
        (
            2,
            340.0,
            "frame.pdf",
            fixture
                .url("/download?name=frame.pdf")
                .replacen("127.0.0.1", "localhost", 1),
        ),
    ] {
        click(&live, device, 100.0, y);
        let status = live.wait(filename, Duration::from_secs(10), |s| {
            s.devices[device].downloads == 1
        });
        assert_eq!(
            status.devices[device].download,
            Some(DownloadState {
                filename: filename.into(),
                url,
            })
        );
    }
    let status = live.session().status();
    for device in &status.devices {
        assert_eq!(device.downloads, 1);
        assert_eq!(device.url, fixture.url("/downloads"));
        assert_eq!(device.error, None);
    }
    for path in [
        "/download?name=report.pdf",
        "/download",
        "/download?name=frame.pdf",
    ] {
        assert_eq!(requested(path), 1, "{path} was requested once");
    }

    // The phone's file input: reported, cancelled for the page, no file given.
    click(&live, 0, 100.0, 240.0);
    live.wait("the file chooser", Duration::from_secs(10), |s| {
        s.devices[0].file_choosers == 1
    });
    assert!(fixture.wait_for(Duration::from_secs(10), |f| {
        !events(f, "chooser").is_empty()
    }));
    thread::sleep(Duration::from_millis(300));
    let answers: Vec<_> = events(&fixture, "chooser")
        .iter()
        .map(|event| event["result"].clone())
        .collect();
    assert_eq!(answers, ["cancel"]);
    let status = live.session().status();
    assert_eq!(
        status
            .devices
            .iter()
            .map(|device| device.file_choosers)
            .collect::<Vec<_>>(),
        [1, 0, 0]
    );

    // Go to an address that is a download: every device reports it and stays
    // on its page without a navigation error.
    live.send(Command::NavigateAll {
        url: fixture.url("/download?name=go.pdf"),
    });
    let status = live.wait("the refused address", Duration::from_secs(10), |s| {
        s.devices
            .iter()
            .all(|device| device.downloads == 2 && !device.loading)
    });
    for device in &status.devices {
        assert_eq!(device.error, None);
        assert_eq!(device.url, fixture.url("/downloads"));
        assert_eq!(device.download.as_ref().unwrap().filename, "go.pdf");
    }
    assert_eq!(requested("/download?name=go.pdf"), 3);

    // Nothing was saved in the profile or the user's download folder.
    assert_eq!(download_names(live.root.path()), Vec::<String>::new());
    assert_eq!(download_names(&home.join("Downloads")), downloads_before);
    live.close();
}

/// Files below `root` named like the downloads of `DOWNLOAD_PAGE`, or partial
/// downloads.
fn download_names(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut directories = vec![(root.to_owned(), 0)];
    while let Some((directory, depth)) = directories.pop() {
        for entry in std::fs::read_dir(&directory)
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            if ["report", "notes", "frame.pdf", "go.pdf", ".crdownload"]
                .iter()
                .any(|part| name.contains(part))
            {
                found.push(entry.path().display().to_string());
            }
            if depth < 8 && entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                directories.push((entry.path(), depth + 1));
            }
        }
    }
    found.sort();
    found
}

fn iframe_event(session: &str, method: &str, params: Value) -> Value {
    json!({"method": method, "sessionId": session, "params": params})
}

fn iframe_attached(parent: &str, session: &str, frame: &str) -> Value {
    iframe_event(
        parent,
        "Target.attachedToTarget",
        json!({
            "sessionId": session, "waitingForDebugger": true,
            "targetInfo": {"targetId": frame, "type": "iframe", "browserContextId": "CTX1"}
        }),
    )
}

fn frame_download(frame: &str) -> Value {
    json!({"method": "Browser.downloadWillBegin", "params": {
        "frameId": frame, "guid": frame, "url": format!("http://localhost/{frame}"),
        "suggestedFilename": "nested.pdf"
    }})
}

#[test]
fn iframe_sessions_recursively_cancel_choosers_and_own_nested_downloads() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = fake_live(root.path(), Limits::default());
    assert!(peer.browser_requests("Target.setAutoAttach").is_empty());
    for session in ["S0", "S1", "S2"] {
        assert_eq!(
            peer.last_params("Target.setAutoAttach", session),
            iframe_auto_attach()
        );
    }
    peer.event(iframe_attached("S1", "I1", "F1"));
    wait_for_requests(&peer, "Runtime.runIfWaitingForDebugger", "I1", 1);
    peer.event(iframe_attached("I1", "I2", "F2"));
    wait_for_requests(&peer, "Runtime.runIfWaitingForDebugger", "I2", 1);
    for session in ["I1", "I2"] {
        assert_eq!(
            peer.last_params("Page.setInterceptFileChooserDialog", session),
            json!({"enabled": true, "cancel": true})
        );
        assert_eq!(
            peer.last_params("Target.setAutoAttach", session),
            iframe_auto_attach()
        );
        assert_eq!(peer.count("Page.enable", session), 1);
        assert_eq!(peer.count("DOM.setFileInputFiles", session), 0);
    }
    peer.event(iframe_event(
        "I2",
        "Page.frameAttached",
        json!({
            "frameId": "F3", "parentFrameId": "F2"
        }),
    ));
    for (session, frame) in [("I1", "F1"), ("I2", "F2"), ("I2", "F3")] {
        peer.event(iframe_event(
            session,
            "Page.fileChooserOpened",
            json!({"frameId": frame}),
        ));
    }
    // Top-session mirrors and events of other devices do not double-count.
    peer.event(iframe_event(
        "S1",
        "Page.fileChooserOpened",
        json!({"frameId": "F2"}),
    ));
    peer.event(iframe_event(
        "S0",
        "Page.fileChooserOpened",
        json!({"frameId": "F2"}),
    ));
    for frame in ["F1", "F2", "F3"] {
        peer.event(frame_download(frame));
    }
    // Child-session events must not modify top-level navigation/input/sync state.
    peer.event(iframe_event(
        "I2",
        "Page.frameNavigated",
        json!({
            "frame": {"id": "F2", "loaderId": "L1", "url": "http://localhost/nested"}
        }),
    ));
    peer.event(iframe_event(
        "I2",
        "Page.javascriptDialogOpening",
        json!({"type": "alert"}),
    ));
    let status = wait_for(&live, "nested activity", Duration::from_secs(3), |s| {
        s.devices[1].file_choosers == 3 && s.devices[1].downloads == 3
    });
    assert_eq!(status.devices[0].file_choosers, 0);
    assert_eq!(status.devices[2].file_choosers, 0);
    assert_eq!(status.devices[0].downloads, 0);
    assert_eq!(status.devices[2].downloads, 0);
    assert!(status.devices[1].dialog.is_none());
    assert_eq!(status.devices[1].url, "");
    // Removing an ancestor invalidates all child identities and sessions.
    peer.event(iframe_event(
        "S1",
        "Page.frameDetached",
        json!({"frameId": "F1", "reason": "remove"}),
    ));
    for frame in ["F1", "F2", "F3"] {
        peer.event(frame_download(frame));
    }
    peer.event(iframe_event(
        "I2",
        "Page.frameAttached",
        json!({"frameId": "STALE", "parentFrameId": "F2"}),
    ));
    peer.event(iframe_event(
        "I2",
        "Page.fileChooserOpened",
        json!({"frameId": "F2"}),
    ));
    peer.event(frame_download("STALE"));
    thread::sleep(Duration::from_millis(150));
    assert_eq!(live.status().devices[1].downloads, 3);
    assert_eq!(live.status().devices[1].file_choosers, 3);
    assert_eq!(peer.count("Runtime.runIfWaitingForDebugger", "I2"), 1);
    drop(live);
}

#[test]
fn iframe_frame_tree_snapshots_cannot_restore_removed_documents() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, session| {
        method == "Page.getFrameTree" && session.is_some_and(|s| s.starts_with('I'))
    });
    let live = fake_live(root.path(), Limits::default());
    let reply_tree = |peer: &FakePeer, session: &str, frame: &str, child: &str| {
        peer.event(
            json!({"id": peer.last_request_id("Page.getFrameTree", session), "result": {
                "frameTree": {"frame": {"id": frame}, "childFrames": [{"frame": {"id": child}}]}
            }}),
        );
    };
    peer.event(iframe_attached("S0", "I1", "F1"));
    wait_for_requests(&peer, "Page.getFrameTree", "I1", 1);
    reply_tree(&peer, "I1", "F1", "EXISTING");
    wait_for_requests(&peer, "Runtime.runIfWaitingForDebugger", "I1", 1);
    peer.event(frame_download("EXISTING"));
    wait_for(
        &live,
        "existing child ownership",
        Duration::from_secs(2),
        |s| s.devices[0].downloads == 1,
    );
    peer.event(iframe_attached("I1", "I2", "F2"));
    wait_for_requests(&peer, "Page.getFrameTree", "I2", 1);
    // An iframe new document clears descendants without retiring its own session.
    for loader in ["L1", "L2"] {
        peer.event(iframe_event(
            "I1",
            "Page.frameNavigated",
            json!({"frame": {
                "id": "F1", "loaderId": loader, "url": "http://localhost/new"
            }}),
        ));
    }
    reply_tree(&peer, "I2", "F2", "STALE");
    peer.event(iframe_event(
        "I2",
        "Page.fileChooserOpened",
        json!({"frameId": "F2"}),
    ));
    for frame in ["F2", "STALE", "EXISTING"] {
        peer.event(frame_download(frame));
    }
    peer.event(frame_download("F1"));
    wait_for(
        &live,
        "replacement ownership",
        Duration::from_secs(2),
        |s| s.devices[0].downloads == 2,
    );
    assert_eq!(live.status().devices[0].file_choosers, 0);
    // Main document replacement retires the remaining iframe session.
    peer.event(iframe_event(
        "S0",
        "Page.frameNavigated",
        json!({"frame": {
            "id": "T0", "loaderId": "MAIN2", "url": "http://localhost/new-main"
        }}),
    ));
    peer.event(iframe_event(
        "I1",
        "Page.frameAttached",
        json!({"frameId": "LATE", "parentFrameId": "F1"}),
    ));
    for frame in ["F1", "LATE"] {
        peer.event(frame_download(frame));
    }
    thread::sleep(Duration::from_millis(150));
    assert_eq!(live.status().devices[0].downloads, 2);
    peer.release();
    drop(live);
}

#[test]
fn iframe_detach_and_swaps_retire_setup_without_replaying_input() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, session| {
        method == "Page.enable" && session == Some("I-old")
    });
    let live = fake_live(root.path(), Limits::default());
    peer.event(iframe_attached("S0", "I-old", "F1"));
    wait_for_requests(&peer, "Page.enable", "I-old", 1);
    peer.event(iframe_event(
        "S0",
        "Target.detachedFromTarget",
        json!({"sessionId": "I-old"}),
    ));
    wait_for_requests(&peer, "Runtime.runIfWaitingForDebugger", "I-old", 1);
    peer.event(iframe_event(
        "S0",
        "Page.frameDetached",
        json!({"frameId": "F1", "reason": "swap"}),
    ));
    peer.event(iframe_attached("S0", "I-new", "F1"));
    wait_for_requests(&peer, "Runtime.runIfWaitingForDebugger", "I-new", 1);
    peer.release();
    peer.event(iframe_event(
        "I-old",
        "Page.fileChooserOpened",
        json!({"frameId": "F1"}),
    ));
    peer.event(iframe_event(
        "I-new",
        "Page.fileChooserOpened",
        json!({"frameId": "F1"}),
    ));
    peer.event(frame_download("F1"));
    wait_for(&live, "swapped renderer", Duration::from_secs(2), |s| {
        s.devices[0].downloads == 1 && s.devices[0].file_choosers == 1
    });
    assert_eq!(peer.count("Page.setInterceptFileChooserDialog", "I-old"), 0);
    assert_eq!(peer.count("Runtime.runIfWaitingForDebugger", "I-old"), 1);
    assert_eq!(peer.count("Input.dispatchMouseEvent", "I-new"), 0);
    peer.event(json!({"method": "Target.targetCrashed", "params": {"targetId": "F1"}}));
    peer.event(frame_download("F1"));
    peer.event(iframe_event(
        "I-new",
        "Page.fileChooserOpened",
        json!({"frameId": "F1"}),
    ));
    thread::sleep(Duration::from_millis(150));
    assert_eq!(live.status().devices[0].downloads, 1);
    assert_eq!(live.status().devices[0].file_choosers, 1);
    drop(live);
}

#[test]
fn iframe_setup_deadline_does_not_block_other_devices_or_leave_held_renderers() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, session| {
        matches!(method, "Page.enable" | "Runtime.runIfWaitingForDebugger")
            && session == Some("I-stuck")
    });
    let live = fake_live(
        root.path(),
        Limits {
            command: Duration::from_millis(300),
            ..Limits::default()
        },
    );
    peer.event(iframe_attached("S0", "I-stuck", "F1"));
    wait_for_requests(&peer, "Page.enable", "I-stuck", 1);
    send_keys(&live, 2, 1);
    wait_for_requests(&peer, "Input.dispatchKeyEvent", "S2", 2);
    let status = wait_for(&live, "iframe setup timeout", Duration::from_secs(3), |s| {
        s.devices[0].error.as_deref() == Some(INCOMPLETE_IFRAME_ACTIVITY)
    });
    assert!(running(&status));
    assert_eq!(status.devices[2].error, None);
    wait_for_requests(&peer, "Runtime.runIfWaitingForDebugger", "I-stuck", 1);
    assert_eq!(
        peer.count("Target.detachFromTarget", "S0"),
        0,
        "never detach while resume is held"
    );
    send_keys(&live, 2, 1);
    wait_for_requests(&peer, "Input.dispatchKeyEvent", "S2", 4);
    // Late page events from the retired session are ignored. Release answers
    // cleanup, then permits exactly one detach, never another setup attempt.
    peer.event(iframe_event(
        "I-stuck",
        "Page.fileChooserOpened",
        json!({"frameId": "F1"}),
    ));
    peer.release();
    wait_for_requests(&peer, "Target.detachFromTarget", "S0", 1);
    assert_eq!(peer.count("Page.enable", "I-stuck"), 1);
    assert_eq!(peer.count("Runtime.runIfWaitingForDebugger", "I-stuck"), 1);
    assert_eq!(live.status().devices[0].file_choosers, 0);
    assert!(live.send(Command::Reload { device: 0 }));
    wait_for_requests(&peer, "Page.reload", "S0", 1);
    assert_eq!(
        live.status().devices[0].error.as_deref(),
        Some(INCOMPLETE_IFRAME_ACTIVITY)
    );
    peer.event(iframe_event(
        "S0",
        "Page.frameNavigated",
        json!({"frame": {
            "id": "T0", "loaderId": "FRESH", "url": "http://localhost/fresh"
        }}),
    ));
    wait_for(
        &live,
        "fresh document clears degradation",
        Duration::from_secs(2),
        |s| s.devices[0].error.is_none(),
    );
    drop(live);
    assert_eq!(fs_entries(root.path()), ["fake-cdp-port"]);
}

#[test]
fn iframe_attachment_preserves_dom_ancestry_and_ignores_replaced_session_detach() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = fake_live(root.path(), Limits::default());
    for (frame, parent) in [("SAME", "T0"), ("CROSS", "SAME")] {
        peer.event(iframe_event(
            "S0",
            "Page.frameAttached",
            json!({"frameId": frame, "parentFrameId": parent}),
        ));
    }
    peer.event(iframe_attached("S0", "I-old", "CROSS"));
    wait_for_requests(&peer, "Runtime.runIfWaitingForDebugger", "I-old", 1);
    peer.event(iframe_attached("S0", "I-new", "CROSS"));
    wait_for_requests(&peer, "Runtime.runIfWaitingForDebugger", "I-new", 1);
    // An old session detach may arrive after the new session attached; targetId
    // alone must not invalidate the current session for that frame.
    peer.event(iframe_event(
        "S0",
        "Target.detachedFromTarget",
        json!({"sessionId": "I-old", "targetId": "CROSS"}),
    ));
    peer.event(iframe_event(
        "I-new",
        "Page.fileChooserOpened",
        json!({"frameId": "CROSS"}),
    ));
    wait_for(
        &live,
        "current session survives old detach",
        Duration::from_secs(2),
        |s| s.devices[0].file_choosers == 1,
    );
    peer.event(iframe_event(
        "S0",
        "Page.frameDetached",
        json!({"frameId": "SAME", "reason": "remove"}),
    ));
    peer.event(iframe_event(
        "I-new",
        "Page.frameAttached",
        json!({"frameId": "LATE", "parentFrameId": "CROSS"}),
    ));
    for frame in ["CROSS", "LATE"] {
        peer.event(frame_download(frame));
    }
    peer.event(iframe_event(
        "I-new",
        "Page.fileChooserOpened",
        json!({"frameId": "CROSS"}),
    ));
    thread::sleep(Duration::from_millis(150));
    assert_eq!(live.status().devices[0].downloads, 0);
    assert_eq!(live.status().devices[0].file_choosers, 1);
    drop(live);
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_nested_cross_site_file_choosers_and_downloads_belong_to_their_device() {
    // 127.0.0.1 -> localhost -> 127.0.0.1 forces two renderer boundaries,
    // including a nested OOPIF that the top page session cannot observe.
    let fixture = Fixture::start(|request, _| {
        let html = |body: String| Reply::Html {
            body,
            delay: Duration::ZERO,
            cookie: None,
        };
        if request.path.starts_with("/download?") {
            return Reply::File {
                filename: Some("frame.pdf".into()),
            };
        }
        if request.path.starts_with("/event?") {
            return Reply::Empty {
                status: 204,
                location: None,
            };
        }
        let style = "<style>body{margin:0}input,a,button{position:absolute;left:0;width:300px;height:40px}iframe{position:absolute;left:0;border:0;width:400px;height:250px}</style>";
        let chooser = |name: &str| {
            format!(
                "<input id=file type=file style='top:0'><script>file.addEventListener('cancel',()=>fetch('/event?cancel={name}'));file.addEventListener('change',()=>fetch('/event?change={name}&files='+file.files.length));</script>"
            )
        };
        let ready = |name: &str| format!("<script>fetch('/event?ready={name}')</script>");
        match request.path.as_str() {
            "/nested-main" => html(format!(
                "{style}<iframe id=outer style='top:0'></iframe><a style='top:520px' href='/download?top'>top download</a><script>outer.src=location.origin.replace('127.0.0.1','localhost')+'/nested-outer';</script>"
            )),
            "/nested-outer" => html(format!(
                "{style}{}<iframe id=inner style='top:60px'></iframe><a style='top:180px' href='/download?outer'>outer download</a><script>inner.src=location.origin.replace('localhost','127.0.0.1')+'/nested-inner';</script>{}",
                chooser("outer"),
                ready("outer")
            )),
            "/nested-inner" => html(format!(
                "{style}{}<a style='top:60px' href='/download?inner'>inner download</a>{}",
                chooser("inner"),
                ready("inner")
            )),
            "/top-control" => html(format!(
                "{style}{}<a style='top:60px' href='/download?control'>control download</a>{}",
                chooser("top"),
                ready("top")
            )),
            _ => html("<p>done</p>".into()),
        }
    });
    let mut workspace = workspace(fixture.url("/nested-main"));
    for device in &mut workspace.devices {
        device.width = 800;
        device.height = 700;
    }
    let live = Live::start(workspace);
    live.wait("nested pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/nested-main")
    });
    assert!(
        fixture.wait_for(Duration::from_secs(10), |f| count(f, "/event?ready=inner")
            == 3)
    );
    // Each chooser fires one cancel event; neither frame ever receives files.
    for (device, y, source) in [(0, 20.0, "outer"), (1, 80.0, "inner")] {
        click(&live, device, 100.0, y);
        live.wait(source, Duration::from_secs(10), |s| {
            s.devices[device].file_choosers == 1
        });
        assert!(fixture.wait_for(Duration::from_secs(5), |f| count(
            f,
            &format!("/event?cancel={source}")
        ) == 1));
    }
    for (device, y, source) in [(0, 200.0, "outer"), (1, 140.0, "inner"), (2, 540.0, "top")] {
        click(&live, device, 100.0, y);
        let status = live.wait(source, Duration::from_secs(10), |s| {
            s.devices[device].downloads == 1
        });
        assert_eq!(
            status.devices[device].download.as_ref().unwrap().filename,
            "frame.pdf"
        );
        assert!(
            status.devices[device]
                .download
                .as_ref()
                .unwrap()
                .url
                .ends_with(&format!("/download?{source}"))
        );
        assert_eq!(count(&fixture, &format!("/download?{source}")), 1);
    }
    let status = live.session().status();
    assert_eq!(
        status
            .devices
            .iter()
            .map(|d| d.file_choosers)
            .collect::<Vec<_>>(),
        [1, 1, 0]
    );
    assert_eq!(
        status
            .devices
            .iter()
            .map(|d| d.downloads)
            .collect::<Vec<_>>(),
        [1, 1, 1]
    );
    assert!(
        status
            .devices
            .iter()
            .all(|d| d.error.is_none() && d.url == fixture.url("/nested-main"))
    );
    // A new document retires both recursive iframe sessions. Only explicit
    // new controls add reports, and none of the old actions is replayed.
    live.send(Command::NavigateAll {
        url: fixture.url("/top-control"),
    });
    live.wait("top controls", Duration::from_secs(15), |s| {
        loaded(s, &fixture, "/top-control")
    });
    click(&live, 2, 100.0, 20.0);
    live.wait("top chooser control", Duration::from_secs(5), |s| {
        s.devices[2].file_choosers == 1
    });
    click(&live, 2, 100.0, 80.0);
    live.wait("top download control", Duration::from_secs(5), |s| {
        s.devices[2].downloads == 2
    });
    thread::sleep(Duration::from_millis(200));
    assert_eq!(count(&fixture, "/event?cancel=top"), 1);
    assert_eq!(count(&fixture, "/download?control"), 1);
    assert!(
        !fixture
            .requests()
            .iter()
            .any(|r| r.path.starts_with("/event?change="))
    );
    assert_eq!(download_names(live.root.path()), Vec::<String>::new());
    let status = live.session().status();
    assert_eq!(
        status
            .devices
            .iter()
            .map(|d| d.downloads)
            .collect::<Vec<_>>(),
        [1, 1, 2]
    );
    assert_eq!(status.protocol_error, None);
    live.close();
}

#[test]
fn iframe_stale_inventory_is_refreshed_without_resurrecting_removed_frames() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, session| {
        method == "Page.getFrameTree" && session == Some("I1")
    });
    let live = fake_live(root.path(), Limits::default());
    peer.event(iframe_attached("S0", "I1", "F1"));
    wait_for_requests(&peer, "Page.getFrameTree", "I1", 1);
    let old_id = peer.last_request_id("Page.getFrameTree", "I1");
    peer.event(iframe_event(
        "I1",
        "Page.frameAttached",
        json!({"frameId": "REMOVED", "parentFrameId": "F1"}),
    ));
    peer.event(iframe_event(
        "I1",
        "Page.frameDetached",
        json!({"frameId": "REMOVED", "reason": "remove"}),
    ));
    peer.event(json!({"id": old_id, "result": {"frameTree": {
        "frame": {"id": "F1"}, "childFrames": [{"frame": {"id": "REMOVED"}}]
    }}}));
    wait_for_requests(&peer, "Page.getFrameTree", "I1", 2);
    assert_eq!(peer.count("Runtime.runIfWaitingForDebugger", "I1"), 0);
    peer.event(
        json!({"id": peer.last_request_id("Page.getFrameTree", "I1"), "result": {"frameTree": {
            "frame": {"id": "F1"}, "childFrames": [{"frame": {"id": "EXISTING"}}]
        }}}),
    );
    wait_for_requests(&peer, "Runtime.runIfWaitingForDebugger", "I1", 1);
    for frame in ["REMOVED", "EXISTING"] {
        peer.event(frame_download(frame));
    }
    wait_for(&live, "fresh inventory", Duration::from_secs(2), |s| {
        s.devices[0].downloads == 1
    });
    assert!(
        live.status().devices[0]
            .download
            .as_ref()
            .unwrap()
            .url
            .ends_with("/EXISTING")
    );
    peer.release();
    drop(live);
}

#[test]
fn iframe_frame_capacity_degrades_only_its_device_and_remains_bounded() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = fake_live(root.path(), Limits::default());
    for frame in 0..MAX_TRACKED_FRAMES + 2 {
        peer.event(iframe_event(
            "S0",
            "Page.frameAttached",
            json!({"frameId": format!("F{frame}"), "parentFrameId": "T0"}),
        ));
    }
    let status = wait_for(&live, "frame capacity", Duration::from_secs(3), |s| {
        s.devices[0].error.as_deref() == Some(INCOMPLETE_IFRAME_ACTIVITY)
    });
    assert!(running(&status));
    assert_eq!(status.devices[2].error, None);
    send_keys(&live, 2, 1);
    wait_for_requests(&peer, "Input.dispatchKeyEvent", "S2", 2);
    peer.event(frame_download("F0"));
    peer.event(frame_download(&format!("F{MAX_TRACKED_FRAMES}")));
    wait_for(
        &live,
        "retained frame ownership",
        Duration::from_secs(2),
        |s| s.devices[0].downloads == 1,
    );
    thread::sleep(Duration::from_millis(100));
    assert_eq!(live.status().devices[0].downloads, 1);
    drop(live);
}

#[test]
fn iframe_retirement_transfers_pending_resume_and_releases_late_children_first() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, session| {
        method == "Runtime.runIfWaitingForDebugger" && session.is_some_and(|s| s.starts_with('I'))
    });
    let live = fake_live(root.path(), Limits::default());
    peer.event(iframe_attached("S0", "I1", "F1"));
    wait_for_requests(&peer, "Runtime.runIfWaitingForDebugger", "I1", 1);
    peer.event(iframe_event(
        "S0",
        "Page.frameDetached",
        json!({"frameId": "F1", "reason": "remove"}),
    ));
    // A debugger-held child attachment may already be queued from the parent
    // whose setup just retired. It gets release-only cleanup, no observation.
    peer.event(iframe_attached("I1", "I2", "F2"));
    wait_for_requests(&peer, "Runtime.runIfWaitingForDebugger", "I2", 1);
    assert_eq!(peer.count("Page.enable", "I2"), 0);
    assert_eq!(peer.count("Target.detachFromTarget", "S0"), 0);
    assert_eq!(peer.count("Target.detachFromTarget", "I1"), 0);
    let answer_resume = |session: &str| {
        peer.event(json!({"id": peer.last_request_id("Runtime.runIfWaitingForDebugger", session), "result": {}}))
    };
    answer_resume("I1");
    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        peer.count("Target.detachFromTarget", "S0"),
        0,
        "held child keeps parent attached"
    );
    answer_resume("I2");
    wait_for_requests(&peer, "Target.detachFromTarget", "I1", 1);
    assert_eq!(peer.count("Target.detachFromTarget", "S0"), 0);
    peer.event(iframe_event(
        "I1",
        "Target.detachedFromTarget",
        json!({"sessionId": "I2"}),
    ));
    wait_for_requests(&peer, "Target.detachFromTarget", "S0", 1);
    for session in ["I1", "I2"] {
        assert_eq!(peer.count("Runtime.runIfWaitingForDebugger", session), 1);
        peer.event(iframe_event(
            session,
            "Page.fileChooserOpened",
            json!({"frameId": "F2"}),
        ));
    }
    assert_eq!(live.status().devices[0].file_choosers, 0);
    peer.release();
    drop(live);
}

#[test]
fn iframe_setup_errors_degrade_only_the_owning_document() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, session| {
        method == "Page.enable" && session == Some("I-failed")
    });
    let live = fake_live(root.path(), Limits::default());
    peer.event(iframe_attached("S0", "I-failed", "F1"));
    wait_for_requests(&peer, "Page.enable", "I-failed", 1);
    peer.event(json!({"id": peer.last_request_id("Page.enable", "I-failed"), "error": {"code": -32000, "message": "renderer unavailable"}}));
    let status = wait_for(
        &live,
        "iframe setup rejection",
        Duration::from_secs(2),
        |s| s.devices[0].error.as_deref() == Some(INCOMPLETE_IFRAME_ACTIVITY),
    );
    assert!(running(&status));
    assert_eq!(status.devices[2].error, None);
    wait_for_requests(&peer, "Target.detachFromTarget", "S0", 1);
    assert_eq!(peer.count("Runtime.runIfWaitingForDebugger", "I-failed"), 1);
    assert_eq!(
        peer.count("Page.setInterceptFileChooserDialog", "I-failed"),
        0
    );
    peer.release();
    drop(live);
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_busy_iframe_setup_degrades_one_device_and_resumes_after_renderer_recovers() {
    let fixture = Fixture::start(|request, _| {
        if request.path.starts_with("/download?") {
            return Reply::File {
                filename: Some("frame.pdf".into()),
            };
        }
        if request.path.starts_with("/event?") {
            return Reply::Empty {
                status: 204,
                location: None,
            };
        }
        let controls = |name: &str| {
            format!(
                "<input id=file type=file style='top:0'><a href='/download?{name}' style='top:60px'>download</a><script>file.addEventListener('cancel',()=>fetch('/event?cancel={name}'));file.addEventListener('change',()=>fetch('/event?change={name}'));</script>"
            )
        };
        let style = "<style>body{margin:0}input,a{position:absolute;left:0;width:300px;height:40px}iframe{position:absolute;left:0;border:0;width:400px;height:200px}</style>";
        let body = match request.path.as_str() {
            "/busy-iframe-main" => format!(
                "{style}{}<script>if(innerWidth===800){{document.body.innerHTML='';const origin=location.origin.replace('127.0.0.1','localhost');const first=document.createElement('iframe');first.src=origin+'/busy-iframe-first';document.body.append(first);setTimeout(()=>{{const second=document.createElement('iframe');second.style.top='250px';second.src=origin+'/busy-iframe-second';document.body.append(second);}},1200);}}</script>",
                controls("healthy")
            ),
            "/busy-iframe-first" => format!(
                "{style}{}<script>fetch('/event?ready=first');setTimeout(()=>{{const end=performance.now()+4000;while(performance.now()<end){{}}fetch('/event?unblocked');}},300);</script>",
                controls("first")
            ),
            "/busy-iframe-second" => format!(
                "{style}{}<script>fetch('/event?ready=second')</script>",
                controls("second")
            ),
            _ => "<p>fresh</p>".into(),
        };
        Reply::Html {
            body,
            delay: Duration::ZERO,
            cookie: None,
        }
    });
    let mut workspace = workspace(fixture.url("/busy-iframe-main"));
    for (device, width) in workspace.devices.iter_mut().zip([800, 700, 600]) {
        device.width = width;
        device.height = 700;
    }
    let live = Live::start_with(
        workspace,
        Limits {
            command: Duration::from_secs(1),
            ..Limits::default()
        },
    );
    assert!(
        fixture.wait_for(Duration::from_secs(10), |f| count(f, "/event?ready=first")
            == 1)
    );
    let status = live.wait("one busy iframe setup", Duration::from_secs(10), |s| {
        s.devices[0].error.as_deref() == Some(INCOMPLETE_IFRAME_ACTIVITY)
    });
    assert!(running(&status));
    assert_eq!(status.devices[2].error, None);
    // The separate-session healthy device still receives input and reports it.
    click(&live, 2, 100.0, 20.0);
    live.wait("healthy chooser", Duration::from_secs(5), |s| {
        s.devices[2].file_choosers == 1
    });
    click(&live, 2, 100.0, 80.0);
    live.wait("healthy download", Duration::from_secs(5), |s| {
        s.devices[2].downloads == 1
    });
    // After the busy renderer returns, the held second iframe must finish
    // loading. A detach before acknowledged resume would leave it hung.
    assert!(
        fixture.wait_for(Duration::from_secs(10), |f| count(f, "/event?unblocked")
            == 1
            && count(f, "/event?ready=second") == 1)
    );
    click(&live, 0, 100.0, 20.0);
    live.wait(
        "configured iframe after recovery",
        Duration::from_secs(5),
        |s| s.devices[0].file_choosers == 1,
    );
    click(&live, 0, 100.0, 270.0);
    assert!(
        fixture.wait_for(Duration::from_secs(5), |f| count(f, "/event?cancel=second")
            == 1)
    );
    let status = live.session().status();
    assert!(running(&status));
    assert_eq!(
        status.devices[0].file_choosers, 1,
        "retired session must not report late activity"
    );
    assert_eq!(
        status.devices[0].error.as_deref(),
        Some(INCOMPLETE_IFRAME_ACTIVITY)
    );
    assert_eq!(status.devices[2].error, None);
    assert_eq!(count(&fixture, "/event?cancel=first"), 1);
    assert_eq!(count(&fixture, "/event?cancel=healthy"), 1);
    assert!(
        !fixture
            .requests()
            .iter()
            .any(|r| r.path.starts_with("/event?change="))
    );
    live.send(Command::NavigateAll {
        url: fixture.url("/fresh-iframe-document"),
    });
    live.wait("explicit fresh document", Duration::from_secs(10), |s| {
        loaded(s, &fixture, "/fresh-iframe-document") && s.devices[0].error.is_none()
    });
    assert_eq!(count(&fixture, "/busy-iframe-first"), 1);
    assert_eq!(count(&fixture, "/busy-iframe-second"), 1);
    assert_eq!(count(&fixture, "/download?healthy"), 1);
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_camera_permission_queries_deny_ptz_across_origins() {
    let fixture = fixture();
    let first_url = fixture.url("/camera-permissions");
    let second_url = first_url.replacen("127.0.0.1", "localhost", 1);
    // Phone and tablet share guest; desktop has its own admin context.
    let live = Live::start(workspace(first_url.clone()));
    let mut expected = Vec::new();
    for (index, url) in [first_url, second_url].iter().enumerate() {
        if index > 0 {
            live.send(Command::NavigateAll { url: url.clone() });
        }
        live.wait("camera permission pages", Duration::from_secs(30), |s| {
            running(s)
                && s.devices
                    .iter()
                    .all(|d| d.url == *url && !d.loading && d.frames > 0)
        });
        let origin = url.strip_suffix("/camera-permissions").unwrap();
        assert!(
            fixture.wait_for(Duration::from_secs(10), |f| {
                events(f, "camera-permission")
                    .iter()
                    .filter(|report| report["origin"] == origin)
                    .count()
                    >= 12
            }),
            "{:?}",
            events(&fixture, "camera-permission")
        );
        for width in ["360", "600", "1000"] {
            for (name, state) in [
                ("camera", "denied"),
                ("camera-ptz", "denied"),
                ("clipboard-write", "granted"),
                ("screen-wake-lock", "granted"),
            ] {
                expected.push((
                    origin.to_owned(),
                    width.to_owned(),
                    name.to_owned(),
                    state.to_owned(),
                ));
            }
        }
    }
    let mut observed: Vec<_> = events(&fixture, "camera-permission")
        .into_iter()
        .map(|report| {
            (
                report["origin"].clone(),
                report["w"].clone(),
                report["name"].clone(),
                report["value"].clone(),
            )
        })
        .collect();
    observed.sort();
    expected.sort();
    println!("Camera permission queries: {observed:?}");
    live.close();
    assert_eq!(observed, expected);
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_permission_requests_are_denied_at_once() {
    let fixture = fixture();
    let live = Live::start(workspace(fixture.url("/permissions")));
    live.wait("pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/permissions")
    });
    // Three devices, two reports each.
    assert!(
        fixture.wait_for(Duration::from_secs(10), |f| events(f, "permission").len()
            >= 6),
        "{:?}",
        events(&fixture, "permission")
    );
    for report in events(&fixture, "permission") {
        match report["name"].as_str() {
            // The page is answered, not left waiting for a prompt nobody
            // can see, and the answer is the one `query` reports.
            "notifications" => {
                assert_eq!(report["value"], "denied", "{report:?}");
                let ms: u64 = report["ms"].parse().unwrap();
                assert!(ms < 500, "answered after {ms} ms");
            }
            "query" => assert_eq!(report["value"], "denied", "{report:?}"),
            other => panic!("unexpected report {other}"),
        }
    }
    live.close();
}

/// Events of `TOUCH_PAGE` on the device with viewport width `width`, in order.
fn touch_events(fixture: &Fixture, width: &str) -> Vec<String> {
    // Reports are separate requests, so the page numbers them.
    let mut reports: Vec<_> = events(fixture, "touch")
        .into_iter()
        .filter(|event| event["w"] == width)
        .collect();
    reports.sort_by_key(|event| event["n"].parse::<u32>().unwrap());
    reports
        .iter()
        .map(|event| format!("{} {}", event["name"], event["value"]))
        .collect()
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_touch_devices_get_touches_and_mouse_devices_get_a_mouse() {
    let fixture = fixture();
    let mut workspace = workspace(fixture.url("/touch"));
    workspace.devices[0].touch = true;
    let live = Live::start(workspace);
    live.wait("pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/touch")
    });
    // A tap on the touch phone and a click on the mouse desktop.
    click(&live, 0, 100.0, 200.0);
    click(&live, 2, 100.0, 200.0);
    assert!(
        fixture.wait_for(Duration::from_secs(10), |f| {
            // Each event uses a separate HTTP request: receiving click does
            // not prove the earlier pointerup report has arrived yet.
            touch_events(f, "360").len() >= 7 && touch_events(f, "1000").len() >= 5
        }),
        "phone {:?}, desktop {:?}",
        touch_events(&fixture, "360"),
        touch_events(&fixture, "1000")
    );
    assert_eq!(
        touch_events(&fixture, "360"),
        [
            "pointerdown touch",
            "touchstart touches=1",
            "pointerup touch",
            "touchend touches=0",
            "mousedown mouse",
            "mouseup mouse",
            "click touch"
        ]
    );
    assert_eq!(
        touch_events(&fixture, "1000"),
        [
            "pointerdown mouse",
            "mousedown mouse",
            "pointerup mouse",
            "mouseup mouse",
            "click mouse"
        ]
    );
    // A drag on the phone is a swipe: the page scrolls and sees touch moves.
    let before = touch_events(&fixture, "360").len();
    for (kind, buttons, y) in [
        (PointerKind::Down, 1, 250.0),
        (PointerKind::Move, 1, 200.0),
        (PointerKind::Move, 1, 150.0),
        (PointerKind::Move, 1, 100.0),
        (PointerKind::Up, 0, 100.0),
    ] {
        live.send(Command::Pointer {
            device: 0,
            event: PointerEvent {
                kind,
                x: 100.0,
                y,
                button: PointerButton::Left,
                buttons,
                click_count: 1,
                modifiers: Modifiers::default(),
            },
        });
        thread::sleep(Duration::from_millis(40));
    }
    assert!(
        fixture.wait_for(Duration::from_secs(10), |f| {
            let after = touch_events(f, "360");
            let after = &after[before..];
            after.contains(&"touchmove touches=1".to_owned())
                && after.iter().any(|event| event.starts_with("scrollend "))
        }),
        "{:?}",
        &touch_events(&fixture, "360")[before..]
    );
    let after = touch_events(&fixture, "360")[before..].to_vec();
    assert!(
        after.contains(&"touchmove touches=1".to_owned()),
        "{after:?}"
    );
    let scrolled: u32 = after
        .iter()
        .find_map(|e| e.strip_prefix("scrollend "))
        .unwrap()
        .parse()
        .unwrap();
    assert!(scrolled > 0, "{after:?}");
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_pages_see_the_browser_a_user_would_run() {
    let fixture = fixture();
    let mut workspace = workspace(fixture.url("/fidelity"));
    workspace.devices[0].touch = true;
    workspace.devices[0].mobile = true;
    let live = Live::start(workspace);
    live.wait("pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/fidelity")
    });
    assert!(
        fixture.wait_for(Duration::from_secs(10), |f| events(f, "fidelity").len()
            == 3)
    );
    let report = |width: &str| {
        events(&fixture, "fidelity")
            .into_iter()
            .find(|event| event["w"] == width)
            .unwrap()
    };
    let (phone, tablet, desktop) = (report("360"), report("600"), report("1000"));
    for device in [&phone, &tablet, &desktop] {
        // The headless marker would tell pages that no user is there.
        assert!(
            device["ua"].contains(" Chrome/") && !device["ua"].contains("HeadlessChrome"),
            "{device:?}"
        );
        assert!(device["brands"].contains("Chromium"), "{device:?}");
    }
    // A mouse device hovers with a fine pointer on a screen the size of its
    // viewport; a touch device keeps its coarse pointer without hover.
    for device in [&tablet, &desktop] {
        assert_eq!(
            (device["hover"].as_str(), device["pointer"].as_str()),
            ("hover", "fine"),
            "{device:?}"
        );
    }
    assert_eq!(desktop["screen"], "1000x700", "{desktop:?}");
    assert_eq!(tablet["screen"], "600x800", "{tablet:?}");
    assert_eq!(
        (phone["hover"].as_str(), phone["pointer"].as_str()),
        ("none", "coarse"),
        "{phone:?}"
    );
    assert_eq!(phone["screen"], "360x640", "{phone:?}");
    assert_eq!(phone["touch"], "1");
    live.close();
}

/// A TLS server whose certificate no root signed, served by the `openssl`
/// command line tool with a key made for this test only. Stops when dropped.
struct SelfSignedServer {
    child: std::process::Child,
    port: u16,
    _files: tempfile::TempDir,
}

impl SelfSignedServer {
    fn start() -> Self {
        use std::process::Stdio;
        let files = tempfile::tempdir().unwrap();
        let (certificate, key) = (files.path().join("cert.pem"), files.path().join("key.pem"));
        let made = std::process::Command::new("openssl")
            .args([
                "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2",
            ])
            .args([
                "-config",
                "/dev/null",
                "-addext",
                "basicConstraints=critical,CA:TRUE",
            ])
            .args([
                "-subj",
                "/CN=broxser-test",
                "-addext",
                "subjectAltName=IP:127.0.0.1",
            ])
            .arg("-keyout")
            .arg(&key)
            .arg("-out")
            .arg(&certificate)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run openssl to make the test certificate");
        assert!(made.success(), "openssl req failed: {made}");
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = std::process::Command::new("openssl")
            .args(["s_server", "-www", "-accept", &format!("127.0.0.1:{port}")])
            .arg("-cert")
            .arg(&certificate)
            .arg("-key")
            .arg(&key)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start openssl s_server");
        // Own the child before readiness checks, including every failure path.
        let mut server = Self {
            child,
            port,
            _files: files,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "openssl s_server exited before listening"
            );
            assert!(Instant::now() < deadline, "openssl s_server did not listen");
            thread::sleep(Duration::from_millis(20));
        }
        server
    }
}

impl Drop for SelfSignedServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// ADR 0020: the browser's certificate trust is its own. Before the change the
/// browser opened the user's `~/.pki/nssdb` on the first HTTPS navigation and
/// trusted the CAs (and offered the client certificates) found there; now NSS
/// creates a database inside the private profile, which goes with it.
#[test]
#[ignore = "requires an installed CDP browser"]
fn live_certificate_trust_is_the_browsers_own_not_the_users() {
    let server = SelfSignedServer::start();
    let live = Live::start(workspace(format!("https://127.0.0.1:{}/", server.port)));
    let status = live.wait(
        "a certificate error on every device",
        Duration::from_secs(30),
        |status| status.devices.iter().all(|device| device.error.is_some()),
    );
    for device in &status.devices {
        let error = device.error.as_deref().unwrap();
        assert!(
            error.contains("net::ERR_CERT_AUTHORITY_INVALID")
                && error.contains("CACertificates policy")
                && error.ends_with("not retried"),
            "{error}"
        );
    }
    let profile = std::fs::read_dir(live.root.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("broxser-cdp-"))
        })
        .expect("the private profile");
    let database = profile
        .join(browser::PRIVATE_HOME)
        .join(".local/share/pki/nssdb");
    assert!(
        database.join("cert9.db").is_file(),
        "no NSS database of the browser's own in {}",
        database.display()
    );
    let status = live.wait(
        "the browser's error page",
        Duration::from_secs(30),
        |status| {
            // A navigation error can arrive while an initial about:blank
            // screencast frame is still current. Reload that only after an
            // error-page document has committed, rather than reloading blank.
            status.devices.iter().all(|device| {
                !device.loading
                    && device.frames > 0
                    && !device.url.is_empty()
                    && device.url != "about:blank"
            })
        },
    );
    let error = status.devices[0].error.clone();
    assert!(
        error
            .as_deref()
            .is_some_and(|error| error.contains("net::ERR_CERT_AUTHORITY_INVALID")),
        "the committed error page lost its certificate report: {:?}",
        status.devices[0]
    );
    let frames = status.devices[0].frames;
    let revision = status.devices[0].page_revision;
    live.send(Command::Reload { device: 0 });
    // Wait for another error-page frame after the explicit Reload. Page.reload
    // returns no errorText, so the last confirmed report stays.
    let status = live.wait(
        "the reloaded error page",
        Duration::from_secs(30),
        |status| {
            status.devices[0].page_revision > revision
                && status.devices[0].frames > frames
                && !status.devices[0].loading
        },
    );
    assert_eq!(status.devices[0].error, error);
    let fixture = fixture();
    live.send(Command::NavigateAll {
        url: fixture.url("/"),
    });
    let status = live.wait(
        "a successful document after the certificate failure",
        Duration::from_secs(30),
        |status| loaded(status, &fixture, "/"),
    );
    assert!(status.devices.iter().all(|device| device.error.is_none()));
    assert_eq!(
        count(&fixture, "/"),
        3,
        "one explicit navigation per device"
    );
    drop(server);
    // The profile, and the database with it, are gone after the session.
    live.close();
}

/// Level, kind, text, location, frame scope and repeats of each console entry.
fn console_rows(
    live: &LiveSession,
    device: usize,
) -> Vec<(ConsoleLevel, ConsoleKind, String, String, ConsoleScope, u32)> {
    live.console(device)
        .into_iter()
        .map(|entry| {
            (
                entry.level,
                entry.kind,
                entry.text,
                entry.location,
                entry.scope,
                entry.repeats,
            )
        })
        .collect()
}

/// ADR 0023: console calls, uncaught errors and browser log entries reach
/// the console of the device whose page or frame produced them, bounded and
/// on one line, with repeats counted; Broxser's own worlds never do; Clear
/// empties one device; old entries leave while the counts stay.
#[test]
fn device_consoles_are_bounded_attributed_and_cleared() {
    use ConsoleKind::{Console, Exception, Navigation, Network};
    use ConsoleLevel::{Error, Info, Warning};
    use ConsoleScope::{MainFrame, Subframe, Unknown};
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = fake_live(root.path(), Limits::default());
    for session in ["S0", "S1", "S2"] {
        assert_eq!(peer.count("Runtime.enable", session), 1, "{session}");
        assert_eq!(peer.count("Log.enable", session), 1, "{session}");
    }
    // The phone's page world, a same-process frame's world and Broxser's
    // link observer world.
    let context = |id: i64, frame: &str, default: bool, kind: &str, name: &str| {
        phone(
            "Runtime.executionContextCreated",
            json!({"context": {"id": id, "name": name,
                "auxData": {"frameId": frame, "isDefault": default, "type": kind}}}),
        )
    };
    peer.event(context(5, "T0", true, "default", ""));
    peer.event(context(6, "FX", true, "default", ""));
    peer.event(context(7, "T0", false, "isolated", LINK_WORLD));
    let call = |session: &str, context: i64, kind: &str, text: &str| {
        json!({"method": "Runtime.consoleAPICalled", "sessionId": session, "params": {
            "type": kind, "executionContextId": context,
            "args": [{"type": "string", "value": text}]}})
    };
    peer.event(call("S0", 5, "error", "main\nerror"));
    peer.event(call("S0", 5, "error", "main\nerror"));
    peer.event(call("S0", 6, "warning", "frame warning"));
    peer.event(call("S0", 7, "error", "from Broxser's own world"));
    peer.event(phone(
        "Runtime.exceptionThrown",
        json!({"exceptionDetails": {"text": "Uncaught", "executionContextId": 5,
            "url": "http://127.0.0.1:4173/app.js?token=T", "lineNumber": 1, "columnNumber": 2,
            "exception": {"type": "object", "subtype": "error", "description": "Error: boom\n    at x"}}}),
    ));
    peer.event(phone(
        "Log.entryAdded",
        json!({"entry": {"source": "network", "level": "error",
            "text": "Failed to load resource: the server responded with a status of 404 (Not Found)",
            "url": "http://127.0.0.1:4173/missing.png?session=S"}}),
    ));
    peer.event(call("S0", 5, "log", &"x".repeat(5000)));
    peer.event(commit(
        "L9",
        "http://127.0.0.1:4173/next?code=secret",
        json!({"urlFragment": "#part"}),
    ));
    // The tablet's out-of-process iframe reports console output from its
    // first script: Runtime and Log are enabled before it may run.
    peer.event(iframe_attached("S1", "I1", "F1"));
    wait_for_requests(&peer, "Runtime.runIfWaitingForDebugger", "I1", 1);
    let setup: Vec<String> = peer
        .received
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, session, _, _)| session.as_deref() == Some("I1"))
        .map(|(method, ..)| method.clone())
        .collect();
    assert_eq!(
        setup,
        [
            "Page.enable",
            "Page.setInterceptFileChooserDialog",
            "Target.setAutoAttach",
            "Page.getFrameTree",
            "Runtime.enable",
            "Log.enable",
            "Runtime.runIfWaitingForDebugger"
        ]
    );
    peer.event(call("I1", 1, "log", "iframe log"));
    // A session nobody owns reaches no console.
    peer.event(call("S9", 1, "error", "unknown session"));
    wait_until_read(&peer);

    let status = live.status();
    assert_eq!(
        (
            status.devices[0].console_errors,
            status.devices[0].console_warnings
        ),
        (4, 1)
    );
    let rows = console_rows(&live, 0);
    assert_eq!(rows.len(), 6, "{rows:#?}");
    assert_eq!(
        rows[..4],
        [
            (
                Error,
                Console,
                "main error".into(),
                String::new(),
                MainFrame,
                2
            ),
            (
                Warning,
                Console,
                "frame warning".into(),
                String::new(),
                Subframe,
                1
            ),
            (
                Error,
                Exception,
                "Uncaught Error: boom".into(),
                "http://127.0.0.1:4173/app.js:2:3".into(),
                MainFrame,
                1
            ),
            (
                Error,
                Network,
                "Failed to load resource: the server responded with a status of 404 (Not Found)"
                    .into(),
                "http://127.0.0.1:4173/missing.png".into(),
                Unknown,
                1
            ),
        ]
    );
    assert_eq!(rows[4].2.chars().count(), MAX_CONSOLE_TEXT);
    assert!(rows[4].2.ends_with("x…"));
    assert_eq!(
        rows[5],
        (
            Info,
            Navigation,
            "Navigated".into(),
            "http://127.0.0.1:4173/next".into(),
            MainFrame,
            1
        )
    );
    assert_eq!(
        console_rows(&live, 1),
        [(
            Info,
            Console,
            "iframe log".into(),
            String::new(),
            Subframe,
            1
        )]
    );
    assert_eq!(
        (
            status.devices[1].console_errors,
            status.devices[1].console_warnings
        ),
        (0, 0)
    );
    assert!(console_rows(&live, 2).is_empty());

    // Clear empties the phone only; the tablet keeps its entry.
    let revision = status.devices[0].console_revision;
    assert!(live.send(Command::ClearConsole { device: 0 }));
    let status = wait_for(&live, "the cleared phone", Duration::from_secs(2), |s| {
        s.devices[0].console_revision != revision
    });
    assert_eq!(
        (
            status.devices[0].console_errors,
            status.devices[0].console_warnings
        ),
        (0, 0)
    );
    assert!(console_rows(&live, 0).is_empty());
    assert_eq!(console_rows(&live, 1).len(), 1);

    // A same-process frame's failed resource arrives on the page session,
    // without an execution context, even though its Runtime world is known.
    // The address alone cannot establish which frame caused the failure.
    peer.event(phone(
        "Log.entryAdded",
        json!({"entry": {"source": "network", "level": "error",
        "text": "frame resource failed", "url": "http://127.0.0.1:4173/frame/missing.png"}}),
    ));
    wait_until_read(&peer);
    assert_eq!(
        console_rows(&live, 0),
        [(
            Error,
            Network,
            "frame resource failed".into(),
            "http://127.0.0.1:4173/frame/missing.png".into(),
            Unknown,
            1
        )]
    );

    // The desktop keeps the newest entries; its count keeps every error.
    for n in 0..MAX_CONSOLE_ENTRIES + 50 {
        peer.event(call("S2", 1, "error", &format!("error {n}")));
    }
    wait_until_read(&peer);
    let rows = console_rows(&live, 2);
    assert_eq!(rows.len(), MAX_CONSOLE_ENTRIES);
    assert_eq!(rows[0].2, "error 50");
    assert!(
        rows.iter().all(|row| row.4 == Unknown),
        "no known main context on the desktop"
    );
    assert_eq!(
        live.status().devices[2].console_errors,
        MAX_CONSOLE_ENTRIES as u32 + 50
    );
    drop(live);
}

#[test]
fn console_objects_are_released_in_coalesced_active_session_batches() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, _| {
        method == "Runtime.releaseObjectGroup"
    });
    let live = fake_live(root.path(), Limits::default());
    let object = |session: &str, context: i64| {
        json!({"method": "Runtime.consoleAPICalled",
        "sessionId": session, "params": {"type": "log", "executionContextId": context,
            "args": [{"type": "object", "objectId": "remote-object", "description": "Object"}]}})
    };
    peer.event(phone(
        "Runtime.consoleAPICalled",
        json!({"type": "log", "executionContextId": 1,
        "args": [{"type": "string", "value": "no bound object"}]}),
    ));
    peer.event(phone(
        "Log.entryAdded",
        json!({"entry": {"source": "network", "level": "error",
        "text": "plain failed request"}}),
    ));
    wait_until_read(&peer);
    assert_eq!(peer.count("Runtime.releaseObjectGroup", "S0"), 0);

    for _ in 0..100 {
        peer.event(object("S0", 1));
    }
    wait_for_requests(&peer, "Runtime.releaseObjectGroup", "S0", 1);
    assert_eq!(
        peer.last_params("Runtime.releaseObjectGroup", "S0"),
        json!({"objectGroup": "console"})
    );
    for _ in 0..100 {
        peer.event(object("S0", 1));
    }
    wait_until_read(&peer);
    assert_eq!(
        peer.count("Runtime.releaseObjectGroup", "S0"),
        1,
        "one request while its reply is held"
    );

    // Invisible isolated-world messages still bind objects in Runtime.
    peer.event(
        json!({"method": "Runtime.executionContextCreated", "sessionId": "S2",
        "params": {"context": {"id": 42, "name": LINK_WORLD,
            "auxData": {"frameId": "T2", "type": "isolated"}}}}),
    );
    peer.event(object("S2", 42));
    wait_for_requests(&peer, "Runtime.releaseObjectGroup", "S2", 1);
    assert!(live.console(2).is_empty());

    peer.event(iframe_attached("S1", "I1", "F1"));
    wait_for_requests(&peer, "Runtime.runIfWaitingForDebugger", "I1", 1);
    peer.event(object("I1", 1));
    wait_for_requests(&peer, "Runtime.releaseObjectGroup", "I1", 1);
    let retired_revision = live.status().devices[1].console_revision;
    peer.event(
        json!({"method": "Target.detachedFromTarget", "sessionId": "S1",
        "params": {"sessionId": "I1", "targetId": "F1"}}),
    );
    peer.event(object("I1", 1));
    wait_until_read(&peer);
    assert_eq!(live.status().devices[1].console_revision, retired_revision);

    peer.release();
    wait_for_requests(&peer, "Runtime.releaseObjectGroup", "S0", 2);
    thread::sleep(CONSOLE_RELEASE_INTERVAL * 2);
    assert_eq!(
        peer.count("Runtime.releaseObjectGroup", "S0"),
        2,
        "new objects during pending get one more batch"
    );
    assert_eq!(
        peer.count("Runtime.releaseObjectGroup", "I1"),
        1,
        "retirement abandons the pending release"
    );
    assert_eq!(peer.count("Runtime.discardConsoleEntries", "S0"), 0);
    assert_eq!(peer.count("Runtime.discardConsoleEntries", "I1"), 0);
    assert!(running(&live.status()));
    assert!(live.status().protocol_error.is_none());
    peer.event(json!({"method": "Target.targetCrashed", "params": {"targetId": "T2"}}));
    peer.event(object("S2", 42));
    wait_until_read(&peer);
    thread::sleep(CONSOLE_RELEASE_INTERVAL * 2);
    assert_eq!(
        peer.count("Runtime.releaseObjectGroup", "S2"),
        1,
        "late objects from the dead page do not recreate cleanup"
    );
    peer.event(
        json!({"method": "Runtime.executionContextCreated", "sessionId": "S2",
        "params": {"context": {"id": 55, "auxData": {"frameId": "T2", "isDefault": true}}}}),
    );
    peer.event(object("S2", 55));
    wait_for_requests(&peer, "Runtime.releaseObjectGroup", "S2", 2);
    assert_eq!(
        live.console(2)[0].scope,
        ConsoleScope::MainFrame,
        "a fresh page context can be observed again"
    );
    drop(live);
}

#[test]
fn console_release_timeout_retries_without_new_objects_or_runtime_failure() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, _| {
        method == "Runtime.releaseObjectGroup"
    });
    let live = fake_live(
        root.path(),
        Limits {
            command: Duration::from_millis(200),
            ..Limits::default()
        },
    );
    peer.event(phone(
        "Runtime.exceptionThrown",
        json!({"exceptionDetails": {"text": "Uncaught",
        "exception": {"type": "object", "objectId": "exception", "description": "Error: boom"}}}),
    ));
    wait_for_requests(&peer, "Runtime.releaseObjectGroup", "S0", 2);
    assert!(
        running(&live.status()),
        "cleanup timeout does not stop the runtime"
    );
    assert!(live.status().protocol_error.is_none());
    // An ordinary context race is retried without a console event or error UI.
    peer.event(
        json!({"id": peer.last_request_id("Runtime.releaseObjectGroup", "S0"),
        "error": {"code": -32000, "message": "Cannot find context with specified id"}}),
    );
    wait_for_requests(&peer, "Runtime.releaseObjectGroup", "S0", 3);
    assert!(running(&live.status()));
    assert!(live.status().protocol_error.is_none());
    peer.release();
    drop(live);
}

#[test]
fn console_objects_are_released_while_another_device_waits_in_setup() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, session| {
        method == "Runtime.enable" && session == Some("S2")
    });
    let live = LiveSession::start_with(
        workspace("http://127.0.0.1:4173/".into()),
        fake_options(root.path()),
        Limits::default(),
        || {},
    )
    .unwrap();
    wait_for_requests(&peer, "Runtime.enable", "S2", 1);
    peer.event(phone(
        "Runtime.consoleAPICalled",
        json!({"type": "log", "executionContextId": 1,
        "args": [{"type": "object", "objectId": "during-setup", "description": "Object"}]}),
    ));
    wait_for_requests(&peer, "Runtime.releaseObjectGroup", "S0", 1);
    assert_eq!(
        live.status().runtime,
        RuntimeState::Starting,
        "setup is still waiting on the desktop"
    );
    peer.release();
    wait_for(&live, "setup completion", Duration::from_secs(2), running);
    drop(live);
}

#[test]
fn stopped_runtime_console_can_be_cleared_without_browser_commands() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let mut live = fake_live(root.path(), Limits::default());
    for (session, kind) in [("S0", "error"), ("S1", "warning")] {
        peer.event(
            json!({"method": "Runtime.consoleAPICalled", "sessionId": session,
            "params": {"type": kind, "executionContextId": 1,
                "args": [{"type": "string", "value": format!("retained {session}")}]}}),
        );
    }
    wait_until_read(&peer);
    live.cancel.cancel();
    live.worker.take().unwrap().join().unwrap();
    let before = live.status();
    assert_eq!(before.runtime, RuntimeState::Stopped { error: None });
    assert!(live.is_finished());
    assert_eq!(live.console(0).len(), 1, "stopping retains the console");
    assert_eq!(before.devices[0].console_errors, 1);
    let other = live.console(1);
    let requests = lock(&peer.received).len();
    assert!(
        !live.send(Command::Reload { device: 0 }),
        "the receiver has exited"
    );

    assert!(live.send(Command::ClearConsole { device: 0 }));

    let after = live.status();
    assert_eq!(after.runtime, before.runtime);
    assert!(live.console(0).is_empty());
    assert_eq!(
        (
            after.devices[0].console_errors,
            after.devices[0].console_warnings
        ),
        (0, 0)
    );
    assert_eq!(
        after.devices[0].console_revision,
        before.devices[0].console_revision + 1
    );
    assert_eq!(live.console(1), other);
    assert_eq!(after.devices[1], before.devices[1]);
    assert!(!live.send(Command::ClearConsole {
        device: after.devices.len()
    }));
    assert_eq!(live.status(), after, "an invalid device changes nothing");
    assert_eq!(
        lock(&peer.received).len(),
        requests,
        "Clear sends no CDP command or replay"
    );
    drop(live);
}

#[test]
fn concurrent_console_clear_and_push_publish_consistent_snapshots() {
    const ROUNDS: usize = 128;
    let notified = Arc::new(AtomicUsize::new(0));
    let shared: Arc<Shared> = Arc::new_cyclic(|weak: &std::sync::Weak<Shared>| {
        let weak = weak.clone();
        let notified = Arc::clone(&notified);
        Shared {
            frames: Mutex::new(vec![None; 2]),
            status: Mutex::new(Status {
                devices: vec![DeviceStatus::default(); 2],
                ..Status::default()
            }),
            console: Mutex::new(vec![ConsoleLog::default(), ConsoleLog::default()]),
            screenshots: Mutex::new(vec![None, None]),
            screenshot_requests: Mutex::new(ScreenshotRequests::default()),
            notify: Box::new(move || {
                let shared = weak.upgrade().unwrap();
                // Callbacks can read both snapshots without deadlocking. The
                // same lock order also observes one complete publication.
                let logs = lock(&shared.console);
                let status = lock(&shared.status);
                for (log, device) in logs.iter().zip(&status.devices) {
                    assert_eq!(
                        (log.errors, log.warnings),
                        (device.console_errors, device.console_warnings)
                    );
                }
                notified.fetch_add(1, Ordering::SeqCst);
            }),
        }
    });
    let (commands, receiver) = sync_channel(COMMAND_QUEUE);
    drop(receiver);
    let live = LiveSession {
        commands,
        shared: Arc::clone(&shared),
        cancel: Cancellation::new(),
        worker: None,
    };
    let entry = |kind: &str| {
        console::console_call(&json!({"type": kind,
        "args": [{"type": "string", "value": "message"}]}))
        .unwrap()
    };
    shared.console(1, |log| log.push(entry("warning")));
    let other = live.console(1);
    let other_status = live.status().devices[1].clone();
    let error = entry("error");
    let barrier = std::sync::Barrier::new(3);
    thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..ROUNDS {
                barrier.wait();
                shared.console(0, |log| log.push(error.clone()));
                barrier.wait();
            }
        });
        scope.spawn(|| {
            for _ in 0..ROUNDS {
                barrier.wait();
                assert!(live.send(Command::ClearConsole { device: 0 }));
                barrier.wait();
            }
        });
        for round in 0..ROUNDS {
            barrier.wait();
            barrier.wait();
            let (status, entries) = live.console_snapshot(0).unwrap();
            assert_eq!(
                status.console_errors,
                entries
                    .iter()
                    .filter(|entry| entry.level == ConsoleLevel::Error)
                    .map(|entry| entry.repeats)
                    .sum::<u32>()
            );
            assert_eq!(status.console_warnings, 0);
            assert_eq!(status.console_revision, (round as u64 + 1) * 2);
            assert_eq!(live.console_snapshot(1).unwrap().0, other_status);
        }
    });
    assert_eq!(live.console(1), other);
    assert!(live.console_snapshot(2).is_none());
    assert_eq!(notified.load(Ordering::SeqCst), ROUNDS * 2 + 1);
}

const CONSOLE_PAGE: &str = r#"<!doctype html><html><head><meta name=viewport content="width=device-width,initial-scale=1"></head><body>
<img src="/missing.png" alt="">
<script>
console.error('boom on', innerWidth);
console.warn('careful on %s', innerWidth);
console.log('plain', {w: innerWidth});
setTimeout(() => { throw new Error('uncaught on ' + innerWidth); }, 0);
Promise.reject(new Error('rejected on ' + innerWidth));
document.body.insertAdjacentHTML('beforeend', '<iframe src="http://localhost:' + location.port + '/console-frame?w=' + innerWidth + '"></iframe>');
</script></body></html>"#;

const CONSOLE_FRAME_PAGE: &str = r#"<!doctype html><script>
const w = new URLSearchParams(location.search).get('w');
console.error('frame error on ' + w);
setTimeout(() => { throw new Error('frame uncaught on ' + w); }, 0);
</script>"#;

/// ADR 0023 with Helium: each device's console holds its own page's console
/// calls, uncaught errors and rejections, the failed image request and its
/// cross-site iframe's output, marked as a subframe; nothing from another
/// device. Before the change none of this reached Broxser at all.
#[test]
#[ignore = "requires an installed CDP browser"]
fn live_console_keeps_each_devices_errors() {
    let server = Fixture::start(|request, _| {
        let body = match request.path.split('?').next().unwrap_or("/") {
            "/console" => CONSOLE_PAGE,
            "/console-frame" => CONSOLE_FRAME_PAGE,
            _ => {
                return Reply::Empty {
                    status: 404,
                    location: None,
                };
            }
        };
        Reply::Html {
            body: body.into(),
            delay: Duration::ZERO,
            cookie: None,
        }
    });
    let image = server.url("/missing.png");
    let live = Live::start(workspace(server.url("/console")));
    let widths = [360, 600, 1000];
    let wanted = |width: u32| {
        [
            format!("boom on {width}"),
            format!("Uncaught Error: uncaught on {width}"),
            format!("Uncaught (in promise) Error: rejected on {width}"),
            format!("frame error on {width}"),
            format!("Uncaught Error: frame uncaught on {width}"),
        ]
    };
    let has_all = |status: &Status| {
        status
            .devices
            .iter()
            .zip(widths)
            .enumerate()
            .all(|(index, (device, width))| {
                let entries = live.session().console(index);
                device.console_errors >= 6
                    && entries.iter().any(|entry| entry.location == image)
                    && wanted(width)
                        .iter()
                        .all(|text| entries.iter().any(|entry| &entry.text == text))
            })
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        let status = live.session().status();
        if has_all(&status) {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for every device's console: {:#?}",
            (0..widths.len())
                .map(|index| live.session().console(index))
                .collect::<Vec<_>>()
        );
        thread::sleep(Duration::from_millis(20));
    };
    for (index, width) in widths.into_iter().enumerate() {
        let entries = live.session().console(index);
        let find = |text: &str| {
            entries
                .iter()
                .find(|entry| entry.text == text)
                .unwrap_or_else(|| panic!("{text} missing in {entries:#?}"))
        };
        assert_eq!(entries[0].kind, ConsoleKind::Navigation, "{entries:#?}");
        assert_eq!(entries[0].location, server.url("/console"));
        let boom = find(&format!("boom on {width}"));
        assert_eq!(
            (boom.level, boom.kind, boom.scope),
            (
                ConsoleLevel::Error,
                ConsoleKind::Console,
                ConsoleScope::MainFrame
            )
        );
        assert!(
            boom.location.starts_with(&server.url("/console:")),
            "{boom:?}"
        );
        let careful = find(&format!("careful on {width}"));
        assert_eq!(careful.level, ConsoleLevel::Warning);
        assert_eq!(
            find(&format!("plain {{w: {width}}}")).level,
            ConsoleLevel::Info
        );
        assert_eq!(
            find(&format!("Uncaught Error: uncaught on {width}")).kind,
            ConsoleKind::Exception
        );
        let failed = entries
            .iter()
            .find(|entry| entry.location == image)
            .unwrap();
        assert_eq!(
            (failed.level, failed.kind),
            (ConsoleLevel::Error, ConsoleKind::Network)
        );
        assert!(failed.text.contains("404"), "{failed:?}");
        for text in [
            format!("frame error on {width}"),
            format!("Uncaught Error: frame uncaught on {width}"),
        ] {
            let entry = find(&text);
            assert_eq!(entry.scope, ConsoleScope::Subframe, "{entry:?}");
            // The frame's address loses its query.
            assert!(
                entry.location.starts_with(&format!(
                    "http://localhost:{}/console-frame:",
                    server.url("").rsplit(':').next().unwrap()
                )),
                "{entry:?}"
            );
        }
        // Nothing from the other devices.
        for other in widths.into_iter().filter(|other| *other != width) {
            assert!(
                entries
                    .iter()
                    .all(|entry| !entry.text.contains(&format!(" {other}"))),
                "{width} shows {other}: {entries:#?}"
            );
        }
        assert_eq!(status.devices[index].console_warnings, 1);
    }
    let revision = status.devices[0].console_revision;
    live.send(Command::ClearConsole { device: 0 });
    live.wait("the cleared phone", Duration::from_secs(5), |status| {
        status.devices[0].console_revision != revision
    });
    // Only the favicon's 404, which the browser can request later, may
    // arrive after Clear.
    let left = live.session().console(0);
    assert!(
        left.iter()
            .all(|entry| entry.location == server.url("/favicon.ico")),
        "{left:#?}"
    );
    let status = live.session().status();
    assert_eq!(
        status.devices[0].console_errors,
        left.iter().map(|entry| entry.repeats).sum::<u32>()
    );
    assert!(status.devices[1].console_errors >= 6);
    live.close();
}

/// Enabling Runtime binds console RemoteObjects outside the text ring. After
/// Chromium's own message ring evicts the original object, our observation
/// must not keep a WeakRef-only object alive in an out-of-process iframe.
#[test]
#[ignore = "requires an installed CDP browser"]
fn live_console_releases_remote_objects_without_clearing_page_history() {
    const PAGE: &str = r#"<!doctype html><script>
(() => {
  const frame = document.createElement('iframe');
  frame.src = 'http://localhost:' + location.port + '/retention-frame';
  document.documentElement.appendChild(frame);
})();
</script>"#;
    const FRAME: &str = r#"<!doctype html><script>
(() => {
  let value = {payload: new Array(65536).fill(42)};
  globalThis.retentionProbe = new WeakRef(value);
  console.log(value);
})();
for (let n = 0; n < 1100; n++) console.log('tick' + n);
console.log('probe-done');
</script>"#;
    let server = Fixture::start(|request, _| Reply::Html {
        body: match request.path.as_str() {
            "/retention" => PAGE,
            "/retention-frame" => FRAME,
            _ => "missing",
        }
        .into(),
        delay: Duration::ZERO,
        cookie: None,
    });
    let mut workspace = workspace(server.url("/retention"));
    workspace.devices.truncate(1);
    let live = Live::start(workspace);
    live.wait(
        "the iframe's complete console output",
        Duration::from_secs(30),
        |_| {
            live.session()
                .console(0)
                .iter()
                .any(|entry| entry.text == "probe-done" && entry.scope == ConsoleScope::Subframe)
        },
    );
    // Attach a probe to this app-owned browser without Runtime.enable, which
    // would itself replay and bind all the console's RemoteObjects.
    let profiles: Vec<_> = fs_entries(live.root.path())
        .into_iter()
        .filter(|name| name.starts_with("broxser-cdp-"))
        .collect();
    assert_eq!(profiles.len(), 1);
    let endpoint = std::fs::read_to_string(
        live.root
            .path()
            .join(&profiles[0])
            .join("DevToolsActivePort"),
    )
    .unwrap();
    let (port, path) = browser::parse_endpoint(&endpoint).unwrap();
    let mut probe = Cdp::connect(port, &path, Duration::from_secs(5), Cancellation::new()).unwrap();
    let mut request = |method: &str, params: Value, session: Option<&str>| {
        let id = probe.send(method, params, session).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(response) = probe.take_response(id) {
                break parse_response(response, method).unwrap();
            }
            assert!(
                probe.read_until(deadline).unwrap(),
                "probe {method} timed out"
            );
        }
    };
    let targets = request("Target.getTargets", json!({}), None);
    let iframe = targets["targetInfos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|target| {
            target["type"] == "iframe"
                && target["url"]
                    .as_str()
                    .is_some_and(|url| url.ends_with("/retention-frame"))
        })
        .expect("owned out-of-process iframe");
    let attached = request(
        "Target.attachToTarget",
        json!({"targetId": iframe["targetId"], "flatten": true}),
        None,
    );
    let session = attached["sessionId"].as_str().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        // Separate protocol calls let WeakRef's per-job keep-alive end.
        request("HeapProfiler.collectGarbage", json!({}), Some(session));
        let retained = request(
            "Runtime.evaluate",
            json!({
                "expression": "Boolean(globalThis.retentionProbe.deref())", "returnByValue": true,
            }),
            Some(session),
        );
        let retained = retained
            .pointer("/result/value")
            .and_then(Value::as_bool)
            .unwrap();
        if !retained {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "observing console kept the evicted iframe object alive"
        );
        thread::sleep(CONSOLE_RELEASE_INTERVAL);
    }
    request(
        "Target.detachFromTarget",
        json!({"sessionId": session}),
        None,
    );
    drop(probe);
    assert!(
        live.session()
            .console(0)
            .iter()
            .any(|entry| entry.text == "probe-done"),
        "cleanup preserves displayed history"
    );
    live.close();
}

fn test_screenshot_png() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 2, 3);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[128; 24]).unwrap();
        writer.finish().unwrap();
    }
    bytes
}

fn settle_screenshot_pages(peer: &FakePeer) {
    for n in 0..3 {
        peer.event(json!({"method": "Page.frameNavigated", "sessionId": format!("S{n}"),
            "params": {"frame": {"id": format!("T{n}"), "url": "http://127.0.0.1:4173/", "loaderId": format!("L{n}")}}}));
        peer.event(
            json!({"method": "Page.frameStoppedLoading", "sessionId": format!("S{n}"),
            "params": {"frameId": format!("T{n}")}}),
        );
    }
    wait_until_read(peer);
}

/// Waits for the screenshot result of `device`.
fn screenshot_of(live: &LiveSession, device: usize) -> (u64, ScreenshotResult) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(result) = live.take_screenshot(device) {
            return result;
        }
        assert!(
            Instant::now() < deadline,
            "no screenshot result for device {device}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn screenshots_require_complete_bounded_png_data() {
    let parse = |bytes: &[u8]| {
        parse_screenshot(json!({"id": 1, "result": {
        "data": base64::engine::general_purpose::STANDARD.encode(bytes)}}))
    };
    let good = test_screenshot_png();
    assert!(parse(&good).is_ok());
    let header = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAIAAAADCAYAAAAAAAAA")
        .unwrap();
    assert!(
        parse(&header).is_err(),
        "a header with no CRC/IDAT/IEND is not an image"
    );
    let mut bad_crc = good.clone();
    bad_crc[29] ^= 1;
    assert!(parse(&bad_crc).is_err(), "IHDR CRC must be checked");
    assert!(parse(&good[..good.len() - 12]).is_err(), "IEND is required");
    assert!(
        parse(&good[..40]).is_err(),
        "partial compressed data is rejected"
    );
    let mut enormous = header.clone();
    enormous[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(
        parse(&enormous).unwrap_err().contains("size limit"),
        "check dimensions before decoder allocation"
    );
    let mut bad_idat = good.clone();
    let idat = bad_idat
        .windows(4)
        .position(|bytes| bytes == b"IDAT")
        .unwrap();
    bad_idat[idat + 4] ^= 1;
    assert!(
        parse(&bad_idat).is_err(),
        "compressed image checksum must be checked"
    );
}

#[test]
fn screenshots_are_invalidated_by_page_changes_without_replay() {
    for transition in [
        "navigate", "start", "loading", "commit", "within", "crash", "detach", "hide", "dialog",
    ] {
        let root = profile_root();
        let peer = FakePeer::start(root.path(), |method, _| method == "Page.captureScreenshot");
        let live = fake_live(root.path(), Limits::default());
        settle_screenshot_pages(&peer);
        let revision = live.status().devices[0].page_revision;
        assert!(live.send(Command::Screenshot {
            device: 0,
            token: 41,
            expected_revision: revision
        }));
        wait_for_requests(&peer, "Page.captureScreenshot", "S0", 1);
        match transition {
            "navigate" => { assert!(live.send(Command::Reload { device: 0 })); }
            "start" => peer.event(phone("Page.frameStartedNavigating", json!({"frameId": "T0", "navigationType": "differentDocument", "loaderId": "NEXT", "url": "http://127.0.0.1:4173/next"}))),
            "loading" => peer.event(phone("Page.frameStartedLoading", json!({"frameId": "T0"}))),
            "commit" => peer.event(commit("NEXT", "http://127.0.0.1:4173/next", json!({}))),
            "within" => peer.event(phone("Page.navigatedWithinDocument", json!({"frameId": "T0", "url": "http://127.0.0.1:4173/#new"}))),
            "crash" => peer.event(json!({"method": "Target.targetCrashed", "params": {"targetId": "T0"}})),
            "detach" => peer.event(json!({"method": "Target.detachedFromTarget", "params": {"sessionId": "S0"}})),
            "hide" => { assert!(live.send(Command::SetVisible { device: 0, visible: false })); }
            "dialog" => peer.event(dialog_opening("alert", "wait", "")),
            _ => unreachable!(),
        }
        let (token, result) = screenshot_of(&live, 0);
        assert_eq!(token, 41, "{transition}");
        assert!(result.is_err(), "{transition} must retire the capture");
        assert!(
            live.status().devices[0].page_revision > revision,
            "{transition}"
        );
        assert_ne!(
            live.status().devices[0].error.as_deref(),
            Some(NOT_RESPONDING)
        );
        peer.release();
        thread::sleep(Duration::from_millis(80));
        assert!(
            live.take_screenshot(0).is_none(),
            "{transition}: late reply must be discarded"
        );
        assert_eq!(
            peer.count("Page.captureScreenshot", "S0"),
            1,
            "{transition}: no replay"
        );
        drop(live);
    }
}

#[test]
fn stale_queued_screenshot_revision_is_refused_before_cdp() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |_, _| false);
    let live = fake_live(root.path(), Limits::default());
    settle_screenshot_pages(&peer);
    let revision = live.status().devices[0].page_revision;
    peer.event(phone(
        "Page.navigatedWithinDocument",
        json!({"frameId": "T0", "url": "http://127.0.0.1:4173/#new"}),
    ));
    wait_until_read(&peer);
    assert!(live.send(Command::Screenshot {
        device: 0,
        token: 51,
        expected_revision: revision
    }));
    let (token, result) = screenshot_of(&live, 0);
    assert_eq!(token, 51);
    assert!(result.unwrap_err().contains("page changed"));
    assert_eq!(peer.count("Page.captureScreenshot", "S0"), 0);
    let revision = live.status().devices[0].page_revision;
    assert!(live.send(Command::Screenshot {
        device: 0,
        token: 52,
        expected_revision: revision
    }));
    assert!(
        screenshot_of(&live, 0).1.is_ok(),
        "an explicit fresh snapshot works"
    );
    drop(live);
}

#[test]
fn worker_stop_completes_inflight_and_not_yet_processed_screenshot_tokens() {
    for queued in [false, true] {
        let root = profile_root();
        let peer = FakePeer::start(root.path(), move |method, session| {
            method == "Page.captureScreenshot"
                || (queued && method == "Runtime.enable" && session == Some("S2"))
        });
        let mut live = if queued {
            let live = LiveSession::start_with(
                workspace("http://127.0.0.1:4173/".into()),
                fake_options(root.path()),
                Limits::default(),
                || {},
            )
            .unwrap();
            wait_for_requests(&peer, "Runtime.enable", "S2", 1);
            live
        } else {
            let live = fake_live(root.path(), Limits::default());
            settle_screenshot_pages(&peer);
            live
        };
        assert!(live.send(Command::Screenshot {
            device: 0,
            token: 61,
            expected_revision: live.status().devices[0].page_revision
        }));
        if !queued {
            wait_for_requests(&peer, "Page.captureScreenshot", "S0", 1);
        }
        live.cancel.cancel();
        live.worker.take().unwrap().join().unwrap();
        let (token, result) = screenshot_of(&live, 0);
        assert_eq!(token, 61);
        assert!(result.unwrap_err().contains("runtime stopped"));
        assert!(matches!(
            live.status().runtime,
            RuntimeState::Stopped { .. }
        ));
        assert!(!live.send(Command::Screenshot {
            device: 0,
            token: 62,
            expected_revision: 0
        }));
        assert_eq!(
            peer.count("Page.captureScreenshot", "S0"),
            usize::from(!queued)
        );
        drop(live);
    }
}

#[test]
fn terminal_navigations_allow_fresh_screenshots_without_an_extra_loading_event() {
    for terminal in [
        "download",
        "failure",
        "timeout",
        "stay",
        "stay_reply_first",
        "page_stay",
        "reload_failure",
    ] {
        let root = profile_root();
        let peer = if terminal == "download" {
            FakePeer::start(root.path(), |_, _| false)
        } else if terminal == "reload_failure" {
            FakePeer::start(root.path(), |method, session| {
                method == "Page.reload" && session == Some("S0")
            })
        } else {
            peer_holding_phone_navigations(root.path())
        };
        let live = fake_live(
            root.path(),
            Limits {
                load: Duration::from_millis(500),
                ..Limits::default()
            },
        );
        settle_screenshot_pages(&peer);
        let before = live.status().devices[0].page_revision;
        match terminal {
            "page_stay" => peer.event(phone("Page.frameRequestedNavigation",
                json!({"frameId": "T0", "disposition": "currentTab", "reason": "scriptInitiated", "url": "http://127.0.0.1:4173/next"}))),
            "reload_failure" => {
                assert!(live.send(Command::Reload { device: 0 }));
                wait_for_requests(&peer, "Page.reload", "S0", 1);
            }
            _ => {
                assert!(live.send(Command::NavigateAll { url: format!("http://127.0.0.1:4173/{terminal}") }));
                wait_for_requests(&peer, "Page.navigate", "S0", 2);
            }
        }
        match terminal {
            "failure" => peer.event(failed_navigate_reply(&peer, "net::ERR_CONNECTION_REFUSED")),
            "reload_failure" => peer.event(json!({"id": peer.last_request_id("Page.reload", "S0"),
                "result": {"errorText": "net::ERR_ABORTED"}})),
            "stay" | "stay_reply_first" | "page_stay" => {
                if terminal == "page_stay" {
                    peer.event(phone("Page.frameStartedLoading", json!({"frameId": "T0"})));
                }
                peer.event(dialog_opening("beforeunload", "", ""));
                let dialog = phone_dialog(&live);
                answer_phone(&live, dialog.token, false, None);
                wait_for_requests(&peer, "Page.handleJavaScriptDialog", "S0", 1);
                if terminal == "stay_reply_first" {
                    peer.event(failed_navigate_reply(&peer, "net::ERR_ABORTED"));
                }
                peer.event(dialog_closed(false));
                if terminal == "stay" {
                    peer.event(failed_navigate_reply(&peer, "net::ERR_ABORTED"));
                }
            }
            _ => {}
        }
        wait_for(
            &live,
            "the terminal navigation",
            Duration::from_secs(2),
            |status| {
                status.devices[0].page_revision > before
                    && !status.devices[0].loading
                    && status.devices[0].dialog.is_none()
            },
        );
        assert!(live.send(Command::Screenshot {
            device: 0,
            token: 71,
            expected_revision: before
        }));
        assert!(
            screenshot_of(&live, 0)
                .1
                .unwrap_err()
                .contains("page changed"),
            "{terminal}: the old snapshot stays invalid"
        );
        let current = live.console_snapshot(0).unwrap().0;
        assert!(live.send(Command::Screenshot {
            device: 0,
            token: 72,
            expected_revision: current.page_revision
        }));
        let (_, result) = screenshot_of(&live, 0);
        assert!(
            result.is_ok(),
            "{terminal}: fresh screenshot must work: {result:?}"
        );
        assert_eq!(
            peer.count("Page.captureScreenshot", "S0"),
            1,
            "{terminal}: only the explicit fresh capture"
        );
        drop(live);
    }
}

/// ADR 0024: a screenshot is taken on request as a checked PNG, with its
/// token; a reply that is no PNG, no reply within the command limit, a second
/// request while one is in flight, a hidden device and a page frozen by its
/// dialog each end with a reason, and none of them counts as unanswered input.
#[test]
fn screenshots_are_taken_on_request_checked_and_bounded() {
    let root = profile_root();
    let peer = FakePeer::start(root.path(), |method, session| {
        method == "Page.captureScreenshot" && session == Some("S2")
    });
    let live = fake_live(
        root.path(),
        Limits {
            command: Duration::from_secs(1),
            ..Limits::default()
        },
    );
    settle_screenshot_pages(&peer);
    assert!(live.send(Command::Screenshot {
        device: 0,
        token: 7,
        expected_revision: live.status().devices[0].page_revision
    }));
    let (token, result) = screenshot_of(&live, 0);
    let screenshot = result.unwrap();
    assert_eq!((token, screenshot.width, screenshot.height), (7, 2, 3));
    assert!(screenshot.png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(
        peer.last_params("Page.captureScreenshot", "S0"),
        json!({"format": "png", "fromSurface": true, "captureBeyondViewport": false})
    );

    assert!(live.send(Command::Screenshot {
        device: 1,
        token: 8,
        expected_revision: live.status().devices[1].page_revision
    }));
    assert_eq!(
        screenshot_of(&live, 1),
        (8, Err("The browser's screenshot was not a PNG.".into()))
    );

    // The desktop's reply is held: a second request is refused at once, the
    // first ends at the command limit, and the page is not "not responding".
    assert!(live.send(Command::Screenshot {
        device: 2,
        token: 9,
        expected_revision: live.status().devices[2].page_revision
    }));
    wait_for_requests(&peer, "Page.captureScreenshot", "S2", 1);
    assert!(live.send(Command::Screenshot {
        device: 2,
        token: 10,
        expected_revision: live.status().devices[2].page_revision
    }));
    assert_eq!(
        screenshot_of(&live, 2),
        (
            10,
            Err("A screenshot of this device is already being taken.".into())
        )
    );
    assert_eq!(
        screenshot_of(&live, 2),
        (
            9,
            Err("The browser returned no screenshot within 1 seconds.".into())
        )
    );
    assert_eq!(live.status().devices[2].error, None);
    assert_eq!(peer.count("Page.captureScreenshot", "S2"), 1);

    assert!(live.send(Command::SetVisible {
        device: 0,
        visible: false
    }));
    assert!(live.send(Command::Screenshot {
        device: 0,
        token: 11,
        expected_revision: live.status().devices[0].page_revision
    }));
    assert_eq!(
        screenshot_of(&live, 0),
        (11, Err("Show the device to take its screenshot.".into()))
    );

    peer.event(tablet(
        "Page.javascriptDialogOpening",
        json!({"type": "alert", "message": "hi", "url": "http://127.0.0.1:4173/"}),
    ));
    wait_for(
        &live,
        "the tablet's dialog",
        Duration::from_secs(2),
        |status| status.devices[1].dialog.is_some(),
    );
    assert!(live.send(Command::Screenshot {
        device: 1,
        token: 12,
        expected_revision: live.status().devices[1].page_revision
    }));
    assert_eq!(
        screenshot_of(&live, 1),
        (
            12,
            Err("Answer the page's dialog first; the page is frozen until then.".into())
        )
    );
    assert_eq!(peer.count("Page.captureScreenshot", "S0"), 1);
    assert_eq!(peer.count("Page.captureScreenshot", "S1"), 1);
    drop(live);
}

/// ADR 0024 with Helium: each device's screenshot is its CSS viewport at its
/// device scale, a PNG that decodes.
#[test]
#[ignore = "requires an installed CDP browser"]
fn live_screenshots_show_each_viewport_at_its_scale() {
    let server = fixture();
    let live = Live::start(workspace(server.url("/")));
    live.wait(
        "frames on every device",
        Duration::from_secs(30),
        |status| {
            status
                .devices
                .iter()
                .all(|device| device.frames > 0 && !device.loading)
        },
    );
    for (device, token) in [(0, 21), (1, 22), (2, 23)] {
        assert!(live.session().send(Command::Screenshot {
            device,
            token,
            expected_revision: live.session().status().devices[device].page_revision
        }));
    }
    for (device, token, size) in [
        (0, 21, (720, 1280)),
        (1, 22, (600, 800)),
        (2, 23, (1000, 700)),
    ] {
        let (got, result) = screenshot_of(live.session(), device);
        assert_eq!(got, token);
        let screenshot = result.unwrap();
        assert_eq!(
            (screenshot.width, screenshot.height),
            size,
            "device {device}"
        );
        let decoder = png::Decoder::new(std::io::Cursor::new(&screenshot.png));
        let reader = decoder.read_info().expect("a PNG that decodes");
        assert_eq!((reader.info().width, reader.info().height), size);
    }
    let before = live.session().console_snapshot(0).unwrap().0;
    let next = server.url("/next");
    live.send(Command::NavigateAll { url: next.clone() });
    live.wait(
        "the new page after report snapshot",
        Duration::from_secs(30),
        |status| status.devices[0].url == next && !status.devices[0].loading,
    );
    assert!(live.session().status().devices[0].page_revision > before.page_revision);
    live.send(Command::Screenshot {
        device: 0,
        token: 31,
        expected_revision: before.page_revision,
    });
    let (token, stale) = screenshot_of(live.session(), 0);
    assert_eq!(token, 31);
    assert!(
        stale.unwrap_err().contains("page changed"),
        "a new-page PNG must not be paired with the old report snapshot"
    );
    let current = live.session().console_snapshot(0).unwrap().0;
    live.send(Command::Screenshot {
        device: 0,
        token: 32,
        expected_revision: current.page_revision,
    });
    assert!(
        screenshot_of(live.session(), 0).1.is_ok(),
        "fresh explicit capture after navigation works"
    );
    live.close();
}
