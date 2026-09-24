//! Minimal blocking CDP client for the owned browser's loopback websocket.
//!
//! Responses, detached commands and events are buffered with fixed limits so a
//! misbehaving browser cannot make the client allocate without bound.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::WebSocket;
use tungstenite::client::client_with_config;
use tungstenite::handshake::HandshakeError;
use tungstenite::protocol::{Message, WebSocketConfig};

/// Socket read timeout once the websocket is established. It bounds how late a
/// blocked read notices a deadline or a cancellation.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_MESSAGE: usize = 128 * 1024 * 1024;
const MAX_PENDING_RESPONSES: usize = 16;
const MAX_DETACHED: usize = 256;
const MAX_QUEUED_EVENTS: usize = 1024;

/// Cooperative cancellation shared with a worker thread. Blocking engine calls
/// check it at least once per poll interval and then return [`Cancelled`].
#[derive(Clone, Debug, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            return Err(Cancelled.into());
        }
        Ok(())
    }
}

/// Error for work stopped through [`Cancellation`]; test with `error.is::<Cancelled>()`.
#[derive(Debug)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("cancelled by the application")
    }
}

impl std::error::Error for Cancelled {}

#[derive(Debug)]
pub(crate) struct Event {
    pub method: String,
    pub session: Option<String>,
    pub params: Value,
}

pub(crate) struct Cdp {
    socket: WebSocket<TcpStream>,
    next_id: u64,
    inbox: Inbox,
    cancel: Cancellation,
}

impl Cdp {
    /// Connects to `ws://127.0.0.1:{port}{path}`. `handshake_timeout` covers the
    /// TCP connect and HTTP upgrade; afterwards reads use [`POLL_INTERVAL`].
    pub(crate) fn connect(
        port: u16,
        path: &str,
        handshake_timeout: Duration,
        cancel: Cancellation,
    ) -> Result<Self> {
        cancel.check()?;
        if handshake_timeout.is_zero() {
            bail!("CDP websocket handshake timed out");
        }
        let deadline = Instant::now() + handshake_timeout;
        let connected = TcpStream::connect_timeout(
            &SocketAddr::from(([127, 0, 0, 1], port)),
            handshake_timeout.min(POLL_INTERVAL),
        );
        cancel.check()?;
        let stream = connected.context("connect to owned browser CDP endpoint")?;
        if Instant::now() >= deadline {
            bail!("CDP websocket handshake timed out");
        }
        // Tungstenite retains partial HTTP reads/writes in MidHandshake. A
        // nonblocking socket lets each interrupted attempt check cancellation
        // without resending the upgrade request or discarding partial headers.
        stream.set_nonblocking(true)?;
        let address = format!("ws://127.0.0.1:{port}{path}");
        let mut config = WebSocketConfig::default();
        config.max_message_size = Some(MAX_MESSAGE);
        config.max_frame_size = Some(MAX_MESSAGE);
        let mut attempted = client_with_config(address, stream, Some(config));
        let mut socket = loop {
            cancel.check()?;
            if Instant::now() >= deadline {
                bail!("CDP websocket handshake timed out");
            }
            match attempted {
                Ok((socket, _)) => break socket,
                Err(HandshakeError::Interrupted(mid)) => {
                    thread::sleep(
                        Duration::from_millis(10)
                            .min(deadline.saturating_duration_since(Instant::now())),
                    );
                    attempted = mid.handshake();
                }
                Err(HandshakeError::Failure(error)) => {
                    return Err(anyhow!("CDP websocket handshake: {error}"));
                }
            }
        };
        cancel.check()?;
        socket.get_mut().set_nonblocking(false)?;
        socket.get_ref().set_read_timeout(Some(POLL_INTERVAL))?;
        socket
            .get_ref()
            .set_write_timeout(Some(handshake_timeout))?;
        Ok(Self {
            socket,
            next_id: 1,
            inbox: Inbox::default(),
            cancel,
        })
    }

    /// Shortens the read timeout for interactive loops that also poll a UI queue.
    pub(crate) fn set_poll_interval(&mut self, interval: Duration) -> Result<()> {
        self.socket.get_ref().set_read_timeout(Some(interval))?;
        Ok(())
    }

    pub(crate) fn send(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<u64> {
        self.cancel.check()?;
        let id = self.next_id;
        self.next_id += 1;
        let mut request = json!({"id": id, "method": method, "params": params});
        if let Some(session) = session {
            request["sessionId"] = json!(session);
        }
        self.socket
            .send(Message::Text(request.to_string().into()))
            .with_context(|| format!("send CDP {method}"))?;
        Ok(id)
    }

    /// Sends a command whose result is not needed. Its response is discarded on
    /// arrival; a protocol error is kept for [`Cdp::take_detached_error`].
    pub(crate) fn send_detached(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<()> {
        if self.inbox.detached.len() >= MAX_DETACHED {
            bail!("too many unanswered CDP commands");
        }
        let id = self.send(method, params, session)?;
        self.inbox.detached.insert(id);
        Ok(())
    }

    pub(crate) fn take_detached_error(&mut self) -> Option<String> {
        self.inbox.detached_error.take()
    }

    pub(crate) fn take_response(&mut self, id: u64) -> Option<Value> {
        self.inbox.responses.remove(&id)
    }

    pub(crate) fn pop_event(&mut self) -> Option<Event> {
        self.inbox.events.pop_front()
    }

    /// Reads and buffers one message. Returns `false` if `deadline` passed first.
    pub(crate) fn read_until(&mut self, deadline: Instant) -> Result<bool> {
        loop {
            self.cancel.check()?;
            if Instant::now() >= deadline {
                return Ok(false);
            }
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    let value: Value =
                        serde_json::from_str(text.as_str()).context("parse CDP JSON")?;
                    self.inbox.accept(value)?;
                    return Ok(true);
                }
                Ok(Message::Close(_)) => bail!("CDP websocket closed"),
                Ok(_) => continue,
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error).context("read CDP websocket"),
            }
        }
    }
}

#[derive(Default)]
struct Inbox {
    responses: HashMap<u64, Value>,
    detached: HashSet<u64>,
    detached_error: Option<String>,
    events: VecDeque<Event>,
}

impl Inbox {
    fn accept(&mut self, value: Value) -> Result<()> {
        let Value::Object(mut message) = value else {
            bail!("CDP message is not an object");
        };
        if let Some(id) = message.get("id").and_then(Value::as_u64) {
            if self.detached.remove(&id) {
                if let Some(error) = message
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                {
                    self.detached_error = Some(error.chars().take(200).collect());
                }
                return Ok(());
            }
            if self.responses.len() >= MAX_PENDING_RESPONSES {
                bail!("CDP pending response limit exceeded");
            }
            if self.responses.insert(id, Value::Object(message)).is_some() {
                bail!("duplicate CDP response id {id}");
            }
            return Ok(());
        }
        let Some(Value::String(method)) = message.remove("method") else {
            return Ok(());
        };
        if self.events.len() >= MAX_QUEUED_EVENTS {
            bail!("CDP event queue limit exceeded");
        }
        let session = match message.remove("sessionId") {
            Some(Value::String(session)) => Some(session),
            _ => None,
        };
        self.events.push_back(Event {
            method,
            session,
            params: message.remove("params").unwrap_or(Value::Null),
        });
        Ok(())
    }
}

pub(crate) fn parse_response(value: Value, method: &str) -> Result<Value> {
    if let Some(error) = value.get("error") {
        bail!(
            "CDP {method} failed: {}",
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
        );
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("CDP {method} missing result"))
}

pub(crate) fn required_str<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("CDP response missing string field {field}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    #[test]
    fn responses_events_and_errors_are_separated() {
        let mut inbox = Inbox::default();
        inbox
            .accept(json!({"method":"Page.lifecycleEvent","sessionId":"one","params":{"name":"load","loaderId":"new"}}))
            .unwrap();
        inbox.accept(json!({"id":7,"result":{"value":42}})).unwrap();
        let event = inbox.events.pop_front().unwrap();
        assert_eq!(event.method, "Page.lifecycleEvent");
        assert_eq!(event.session.as_deref(), Some("one"));
        assert_eq!(event.params["loaderId"], "new");
        assert_eq!(
            parse_response(inbox.responses.remove(&7).unwrap(), "test").unwrap()["value"],
            42
        );
        assert!(
            parse_response(
                json!({"id":8,"error":{"message":"bad navigation"}}),
                "Page.navigate"
            )
            .unwrap_err()
            .to_string()
            .contains("bad navigation")
        );
        assert!(parse_response(json!({"id":9}), "missing").is_err());
        assert!(inbox.accept(json!([1, 2])).is_err());
        inbox.accept(json!({"unrelated": true})).unwrap();
        assert!(inbox.events.is_empty() && inbox.responses.is_empty());
    }

    #[test]
    fn detached_responses_are_dropped_but_errors_kept() {
        let mut inbox = Inbox::default();
        inbox.detached.extend([3, 4]);
        inbox.accept(json!({"id":3,"result":{}})).unwrap();
        inbox
            .accept(json!({"id":4,"error":{"message":"No target with given id"}}))
            .unwrap();
        assert!(inbox.responses.is_empty() && inbox.detached.is_empty());
        assert_eq!(
            inbox.detached_error.as_deref(),
            Some("No target with given id")
        );
    }

    #[test]
    fn protocol_buffers_reject_floods_and_duplicates() {
        let mut inbox = Inbox::default();
        for id in 0..MAX_PENDING_RESPONSES as u64 {
            inbox.accept(json!({"id": id, "result": {}})).unwrap();
        }
        assert!(inbox.accept(json!({"id": 999, "result": {}})).is_err());
        inbox.responses.clear();
        inbox.accept(json!({"id": 1, "result": {}})).unwrap();
        assert!(inbox.accept(json!({"id": 1, "result": {}})).is_err());
        for n in 0..MAX_QUEUED_EVENTS {
            inbox
                .accept(
                    json!({"method":"Page.lifecycleEvent", "params":{"loaderId": n.to_string()}}),
                )
                .unwrap();
        }
        assert!(
            inbox
                .accept(json!({"method":"Page.lifecycleEvent", "params":{}}))
                .is_err()
        );
    }

    #[test]
    fn cancellation_is_shared_and_typed() {
        let cancel = Cancellation::new();
        let other = cancel.clone();
        assert!(cancel.check().is_ok());
        other.cancel();
        let error = cancel.check().unwrap_err().context("waiting for browser");
        assert!(error.is::<Cancelled>());
    }

    #[test]
    fn split_upgrade_response_preserves_partial_handshake() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut head = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !head.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0);
                head.extend_from_slice(&chunk[..count]);
            }
            let head = String::from_utf8(head).unwrap();
            let key = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("Sec-WebSocket-Key")
                        .then_some(value.trim())
                })
                .unwrap();
            let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
            stream
                .write_all(b"HTTP/1.1 101 Switching Protocols\r\n")
                .unwrap();
            thread::sleep(POLL_INTERVAL + Duration::from_millis(200));
            stream.write_all(b"Upgrade: websocket\r\n").unwrap();
            thread::sleep(POLL_INTERVAL + Duration::from_millis(200));
            stream
                .write_all(
                    format!("Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n")
                        .as_bytes(),
                )
                .unwrap();
        });
        let socket = Cdp::connect(
            port,
            "/devtools/browser/test",
            Duration::from_secs(3),
            Cancellation::new(),
        )
        .unwrap();
        drop(socket);
        server.join().unwrap();
    }

    #[test]
    fn upgrade_timeout_is_total_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(600));
            drop(stream);
        });
        let started = Instant::now();
        let error = Cdp::connect(
            port,
            "/devtools/browser/test",
            Duration::from_millis(250),
            Cancellation::new(),
        )
        .err()
        .unwrap();
        assert!(
            error.to_string().contains("handshake timed out"),
            "{error:#}"
        );
        assert!(started.elapsed() < Duration::from_millis(500));
        server.join().unwrap();
    }

    #[test]
    fn stalled_upgrade_cancels_with_typed_error_under_half_second() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (request_tx, request_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut head = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !head.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0);
                head.extend_from_slice(&chunk[..count]);
            }
            request_tx.send(()).unwrap();
            thread::sleep(Duration::from_secs(1));
        });
        let cancel = Cancellation::new();
        let worker_cancel = cancel.clone();
        let worker = thread::spawn(move || {
            Cdp::connect(
                port,
                "/devtools/browser/test",
                Duration::from_secs(15),
                worker_cancel,
            )
        });
        request_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let cancelled_at = Instant::now();
        cancel.cancel();
        let error = worker.join().unwrap().err().unwrap();
        assert!(error.is::<Cancelled>(), "{error:#}");
        assert!(cancelled_at.elapsed() < Duration::from_millis(500));
        server.join().unwrap();
    }
}
