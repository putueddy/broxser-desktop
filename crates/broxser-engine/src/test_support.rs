//! Test-owned fixtures: an HTTP server on a random loopback port, fake browser
//! executables, and cleanup assertions scoped to a unique profile root so tests
//! running in parallel never inspect each other's processes or files.

use crate::browser::{self, ProcessIdentity};
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Browser for `#[ignore]` live tests: Helium, or an explicitly chosen Chromium
/// for comparison. Chromium refuses to run as root without `--no-sandbox`, which
/// Broxser never adds, so run live tests as an unprivileged user.
pub(crate) fn test_browser() -> PathBuf {
    PathBuf::from(
        std::env::var_os("BROXSER_TEST_BROWSER")
            .expect("set BROXSER_TEST_BROWSER to a CDP browser executable"),
    )
}

/// A unique directory for one test's private profiles.
pub(crate) fn profile_root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("broxser-test-")
        .tempdir()
        .unwrap()
}

/// Asserts that every observed browser process exited, that no running process
/// names the test's profile root, and that the root holds no profile.
pub(crate) fn assert_cleaned_up(root: &Path, processes: &[ProcessIdentity]) {
    assert!(!processes.is_empty(), "no browser process was observed");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut running = processes.len();
    while running > 0 && Instant::now() < deadline {
        running = processes.iter().filter(|p| browser::is_running(p)).count();
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(running, 0, "browser processes still running");
    assert_eq!(
        browser::referencing(root),
        [],
        "processes still reference the test profile root"
    );
    let leftovers: Vec<_> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with("broxser-cdp-"))
        .collect();
    assert!(leftovers.is_empty(), "profiles left: {leftovers:?}");
}

#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub path: String,
    /// Value of the `fixture_session` cookie, if sent.
    pub session_cookie: Option<String>,
}

pub(crate) enum Reply {
    /// Complete HTML response after `delay`, optionally setting `fixture_session`.
    Html {
        body: String,
        delay: Duration,
        cookie: Option<String>,
    },
    /// Headers and the start of a body, then hold the connection open.
    Stall,
    /// Hold the request open without any response.
    Hang,
    /// Close the connection without responding.
    Drop,
}

type Handler = dyn Fn(&Request, usize) -> Reply + Send + Sync;

/// Local HTTP fixture on `127.0.0.1:0`. Each connection gets its own thread so
/// slow responses overlap like a real server. `/favicon.ico` is always 404.
pub(crate) struct Fixture {
    address: SocketAddr,
    shared: Arc<Shared>,
    acceptor: Option<JoinHandle<()>>,
}

struct Shared {
    stop: AtomicBool,
    requests: Mutex<Vec<Request>>,
    /// Held requests whose client closed the connection.
    abandoned: AtomicUsize,
    workers: Mutex<Vec<JoinHandle<()>>>,
    handler: Box<Handler>,
}

impl Fixture {
    /// `handler` receives each non-favicon request and its zero-based sequence.
    pub(crate) fn start(
        handler: impl Fn(&Request, usize) -> Reply + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            requests: Mutex::new(Vec::new()),
            abandoned: AtomicUsize::new(0),
            workers: Mutex::new(Vec::new()),
            handler: Box::new(handler),
        });
        let accept_shared = Arc::clone(&shared);
        let acceptor = thread::spawn(move || {
            while !accept_shared.stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let shared = Arc::clone(&accept_shared);
                        let worker = thread::spawn(move || serve(stream, &shared));
                        let mut workers = accept_shared.workers.lock().unwrap();
                        workers.retain(|worker| !worker.is_finished());
                        assert!(workers.len() < 64, "fixture connection limit");
                        workers.push(worker);
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            }
        });
        Self {
            address,
            shared,
            acceptor: Some(acceptor),
        }
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.address.port())
    }

    pub(crate) fn requests(&self) -> Vec<Request> {
        self.shared.requests.lock().unwrap().clone()
    }

    pub(crate) fn abandoned(&self) -> usize {
        self.shared.abandoned.load(Ordering::SeqCst)
    }

    pub(crate) fn wait_for(&self, timeout: Duration, condition: impl Fn(&Self) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if condition(self) {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        condition(self)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
        let workers = std::mem::take(&mut *self.shared.workers.lock().unwrap());
        for worker in workers {
            let _ = worker.join();
        }
    }
}

fn serve(mut stream: TcpStream, shared: &Shared) {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let mut head = Vec::new();
    let mut chunk = [0_u8; 1024];
    let started = Instant::now();
    while head.len() < 16 * 1024 && !head.windows(4).any(|w| w == b"\r\n\r\n") {
        if shared.stop.load(Ordering::SeqCst) || started.elapsed() > Duration::from_secs(5) {
            return;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(count) => head.extend_from_slice(&chunk[..count]),
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => return,
        }
    }
    let head = String::from_utf8_lossy(&head);
    let path = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_owned();
    if path == "/favicon.ico" {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    let session_cookie = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.eq_ignore_ascii_case("cookie"))
        .flat_map(|(_, value)| value.split(';'))
        .find_map(|part| part.trim().strip_prefix("fixture_session="))
        .map(str::to_owned);
    let request = Request {
        path,
        session_cookie,
    };
    let sequence = {
        let mut requests = shared.requests.lock().unwrap();
        requests.push(request.clone());
        requests.len() - 1
    };
    match (shared.handler)(&request, sequence) {
        Reply::Html {
            body,
            delay,
            cookie,
        } => {
            let deadline = Instant::now() + delay;
            while Instant::now() < deadline {
                if shared.stop.load(Ordering::SeqCst) {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            let cookie = cookie
                .map(|value| {
                    format!("Set-Cookie: fixture_session={value}; Path=/; SameSite=Lax\r\n")
                })
                .unwrap_or_default();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n{cookie}Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
        Reply::Stall => {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n<!doctype html><title>stalled</title><p>",
            );
            hold(&mut stream, shared);
        }
        Reply::Hang => hold(&mut stream, shared),
        Reply::Drop => {}
    }
}

/// Keeps a connection open until the client closes it or the fixture stops.
fn hold(stream: &mut TcpStream, shared: &Shared) {
    let mut chunk = [0_u8; 256];
    while !shared.stop.load(Ordering::SeqCst) {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    }
    if !shared.stop.load(Ordering::SeqCst) {
        shared.abandoned.fetch_add(1, Ordering::SeqCst);
    }
}

/// Fake browser scripts checked into `testdata/fake-browser`. Tests never write
/// them: an executable written while another test thread forks a child can fail
/// to start with `ETXTBSY` (text file busy).
pub(crate) fn fake_browser(kind: FakeBrowser) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/fake-browser")
        .join(match kind {
            FakeBrowser::NeverReady => "never-ready",
            FakeBrowser::LoopbackEndpoint => "loopback-endpoint",
        })
}

pub(crate) enum FakeBrowser {
    /// Starts and never publishes a CDP endpoint.
    NeverReady,
    /// Publishes `<profile root>/fake-cdp-port` as its endpoint.
    LoopbackEndpoint,
}

/// How a fake CDP websocket peer behaves after its (optionally delayed) handshake.
#[derive(Clone, Copy)]
pub(crate) enum FakeCdp {
    /// Answers `Browser.getVersion`, ignores everything else.
    VersionOnly,
    /// Closes the websocket when the first command arrives.
    CloseOnFirstCommand,
}

/// Serves one fake CDP websocket connection and writes its port into `root`.
pub(crate) fn fake_cdp(
    root: &Path,
    handshake_delay: Duration,
    behavior: FakeCdp,
) -> JoinHandle<()> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    fs::write(
        root.join("fake-cdp-port"),
        listener.local_addr().unwrap().port().to_string(),
    )
    .unwrap();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        thread::sleep(handshake_delay);
        let Ok(mut socket) = tungstenite::accept(stream) else {
            return;
        };
        while let Ok(message) = socket.read() {
            let tungstenite::Message::Text(text) = message else {
                continue;
            };
            let request: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
            match behavior {
                FakeCdp::CloseOnFirstCommand => {
                    let _ = socket.close(None);
                    let _ = socket.flush();
                    return;
                }
                FakeCdp::VersionOnly if request["method"] == "Browser.getVersion" => {
                    let reply = serde_json::json!({
                        "id": request["id"],
                        "result": {"product": "Fake/1.0", "protocolVersion": "1.3"}
                    });
                    if socket
                        .send(tungstenite::Message::Text(reply.to_string().into()))
                        .is_err()
                    {
                        return;
                    }
                }
                FakeCdp::VersionOnly => {}
            }
        }
    })
}
