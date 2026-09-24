//! Minimal blocking CDP client for the owned browser's loopback websocket.
//!
//! Responses, detached commands and events are buffered with fixed limits so a
//! misbehaving browser cannot make the client allocate without bound.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tungstenite::WebSocket;
use tungstenite::client::client_with_config;
use tungstenite::protocol::{Message, WebSocketConfig};

/// Socket read timeout once the websocket is established. It bounds how late a
/// blocked read notices a deadline or a cancellation.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_MESSAGE: usize = 128 * 1024 * 1024;
const MAX_PENDING_RESPONSES: usize = 16;
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
        let stream = TcpStream::connect_timeout(
            &SocketAddr::from(([127, 0, 0, 1], port)),
            handshake_timeout,
        )
        .context("connect to owned browser CDP endpoint")?;
        // The browser may still be initializing its GPU when it publishes the port.
        // Use the command deadline for HTTP upgrade; short polling timeouts are only
        // safe after tungstenite owns a fully established websocket.
        stream.set_read_timeout(Some(handshake_timeout))?;
        stream.set_write_timeout(Some(handshake_timeout))?;
        let address = format!("ws://127.0.0.1:{port}{path}");
        let mut config = WebSocketConfig::default();
        config.max_message_size = Some(MAX_MESSAGE);
        config.max_frame_size = Some(MAX_MESSAGE);
        let (socket, _) = client_with_config(address, stream, Some(config))
            .map_err(|error| anyhow!("CDP websocket handshake: {error}"))?;
        socket.get_ref().set_read_timeout(Some(POLL_INTERVAL))?;
        Ok(Self {
            socket,
            next_id: 1,
            inbox: Inbox::default(),
            cancel,
        })
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
    events: VecDeque<Event>,
}

impl Inbox {
    fn accept(&mut self, value: Value) -> Result<()> {
        let Value::Object(mut message) = value else {
            bail!("CDP message is not an object");
        };
        if let Some(id) = message.get("id").and_then(Value::as_u64) {
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
}
