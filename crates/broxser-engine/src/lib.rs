//! Blocking, bounded CDP capture through an owned browser subprocess.
//!
//! Call [`capture_workspace`] on a worker thread; it starts a fresh private
//! browser profile and stops only that child when capture finishes or fails.

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use broxser_core::Workspace;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tungstenite::WebSocket;
use tungstenite::client::client_with_config;
use tungstenite::protocol::{Message, WebSocketConfig};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);
const IO_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_MESSAGE: usize = 128 * 1024 * 1024;
const MAX_PENDING_RESPONSES: usize = 16;
const MAX_LOAD_EVENTS: usize = 512;

#[derive(Debug, Clone)]
pub struct CaptureOptions {
    /// Explicit browser executable. Only an explicit path can select Chromium
    /// or another CDP browser for diagnostics.
    pub executable: PathBuf,
    pub headless: bool,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            executable: discover_browser().unwrap_or_default(),
            headless: true,
        }
    }
}

/// Discover Helium via BROXSER_HELIUM_BIN, then helium/helium-browser on PATH.
pub fn discover_browser() -> Result<PathBuf> {
    if let Some(path) = env::var_os("BROXSER_HELIUM_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        bail!(
            "BROXSER_HELIUM_BIN does not name a browser file: {}",
            path.display()
        );
    }
    for name in ["helium", "helium-browser"] {
        if let Some(paths) = env::var_os("PATH") {
            for directory in env::split_paths(&paths) {
                let path = directory.join(name);
                if path.is_file() {
                    return Ok(path);
                }
            }
        }
    }
    bail!("Helium not found; set BROXSER_HELIUM_BIN or CaptureOptions.executable")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureReport {
    pub browser_product: String,
    pub protocol_version: String,
    pub frames: Vec<CaptureFrame>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureFrame {
    pub device_id: String,
    pub session_id: String,
    pub path: PathBuf,
    /// Actual PNG pixel dimensions, including device scale factor.
    pub width: u32,
    pub height: u32,
}

/// Capture each configured device as PNG. The URL must be accepted by core
/// validation; a failed navigation or incomplete load returns an error.
pub fn capture_workspace(
    workspace: &Workspace,
    options: &CaptureOptions,
    output_dir: &Path,
) -> Result<CaptureReport> {
    workspace
        .validate()
        .map_err(|error| anyhow!("invalid workspace: {error}"))?;
    if options.executable.as_os_str().is_empty() {
        bail!("browser executable is empty; install Helium or set BROXSER_HELIUM_BIN");
    }
    if !options.executable.is_file() {
        bail!(
            "browser executable does not exist: {}",
            options.executable.display()
        );
    }
    fs::create_dir_all(output_dir).with_context(|| format!("create {}", output_dir.display()))?;
    let mut browser = BrowserProcess::start(options)?;
    let (port, socket_path) = browser.wait_for_endpoint()?;
    let stream =
        TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), COMMAND_TIMEOUT)
            .context("connect to owned browser CDP endpoint")?;
    // The browser may still be initializing its GPU when it publishes the port.
    // Use the command deadline for HTTP upgrade; short polling timeouts are only
    // safe after tungstenite owns a fully established websocket.
    stream.set_read_timeout(Some(COMMAND_TIMEOUT))?;
    stream.set_write_timeout(Some(COMMAND_TIMEOUT))?;
    let address = format!("ws://127.0.0.1:{port}{socket_path}");
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(MAX_MESSAGE);
    config.max_frame_size = Some(MAX_MESSAGE);
    let (mut socket, _) = client_with_config(address, stream, Some(config))
        .map_err(|error| anyhow!("CDP websocket handshake: {error}"))?;
    socket.get_mut().set_read_timeout(Some(IO_TIMEOUT))?;
    let mut cdp = Cdp::new(socket);

    let version = cdp.command("Browser.getVersion", json!({}), None, COMMAND_TIMEOUT)?;
    let browser_product = required_str(&version, "product")?.to_owned();
    let protocol_version = required_str(&version, "protocolVersion")?.to_owned();

    let mut contexts = HashMap::new();
    for session in &workspace.sessions {
        let response = cdp.command(
            "Target.createBrowserContext",
            json!({}),
            None,
            COMMAND_TIMEOUT,
        )?;
        let context = required_str(&response, "browserContextId")?.to_owned();
        contexts.insert(session.id.as_str(), context);
    }

    let mut frames = Vec::with_capacity(workspace.devices.len());
    for device in &workspace.devices {
        let context = contexts.get(device.session.as_str()).ok_or_else(|| {
            anyhow!(
                "device {} references unknown session {}",
                device.id,
                device.session
            )
        })?;
        let response = cdp.command(
            "Target.createTarget",
            json!({"url": "about:blank", "browserContextId": context}),
            None,
            COMMAND_TIMEOUT,
        )?;
        let target = required_str(&response, "targetId")?;
        let response = cdp.command(
            "Target.attachToTarget",
            json!({"targetId": target, "flatten": true}),
            None,
            COMMAND_TIMEOUT,
        )?;
        let cdp_session = required_str(&response, "sessionId")?.to_owned();
        cdp.command(
            "Page.enable",
            json!({}),
            Some(&cdp_session),
            COMMAND_TIMEOUT,
        )?;
        cdp.command(
            "Page.setLifecycleEventsEnabled",
            json!({"enabled": true}),
            Some(&cdp_session),
            COMMAND_TIMEOUT,
        )?;
        cdp.command(
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": device.width,
                "height": device.height,
                "deviceScaleFactor": device.device_scale_factor,
                "mobile": device.mobile,
            }),
            Some(&cdp_session),
            COMMAND_TIMEOUT,
        )?;
        let touch_params = if device.touch {
            json!({"enabled": true, "maxTouchPoints": 1})
        } else {
            json!({"enabled": false})
        };
        cdp.command(
            "Emulation.setTouchEmulationEnabled",
            touch_params,
            Some(&cdp_session),
            COMMAND_TIMEOUT,
        )?;
        let navigation = cdp.command(
            "Page.navigate",
            json!({"url": workspace.url}),
            Some(&cdp_session),
            COMMAND_TIMEOUT,
        )?;
        if let Some(error) = navigation.get("errorText").and_then(Value::as_str) {
            bail!("navigation failed for {}: {error}", device.id);
        }
        let loader_id = required_str(&navigation, "loaderId")?;
        cdp.wait_for_load(&cdp_session, loader_id, LOAD_TIMEOUT)
            .with_context(|| format!("loading device {}", device.id))?;
        let screenshot = cdp.command(
            "Page.captureScreenshot",
            json!({"format": "png", "fromSurface": true, "captureBeyondViewport": false}),
            Some(&cdp_session),
            COMMAND_TIMEOUT,
        )?;
        let encoded = required_str(&screenshot, "data")?;
        let png = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .context("decode CDP screenshot")?;
        if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            bail!("browser returned a non-PNG screenshot for {}", device.id);
        }
        let (pixel_width, pixel_height) = png_dimensions(&png)?;
        let filename = format!("{}.png", safe_filename(&device.id));
        let path = output_dir.join(filename);
        fs::write(&path, png).with_context(|| format!("write screenshot {}", path.display()))?;
        frames.push(CaptureFrame {
            device_id: device.id.clone(),
            session_id: device.session.clone(),
            path,
            width: pixel_width,
            height: pixel_height,
        });
    }

    Ok(CaptureReport {
        browser_product,
        protocol_version,
        frames,
    })
}

fn required_str<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("CDP response missing string field {field}"))
}

fn safe_filename(id: &str) -> String {
    let mut name = String::with_capacity(id.len());
    for byte in id.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' => name.push(byte as char),
            _ => name.push('_'),
        }
    }
    if name.is_empty() || name == "." || name == ".." {
        "device".to_owned()
    } else {
        name
    }
}

fn png_dimensions(png: &[u8]) -> Result<(u32, u32)> {
    if png.len() < 24 || &png[12..16] != b"IHDR" {
        bail!("screenshot PNG is missing IHDR");
    }
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    if width == 0 || height == 0 {
        bail!("screenshot PNG has zero dimensions");
    }
    Ok((width, height))
}

struct BrowserProcess {
    child: Child,
    profile: TempDir,
}

impl BrowserProcess {
    fn start(options: &CaptureOptions) -> Result<Self> {
        let profile = tempfile::Builder::new()
            .prefix("broxser-cdp-")
            .tempdir()
            .context("create private browser profile")?;
        let mut command = Command::new(&options.executable);
        command
            .arg("--remote-debugging-address=127.0.0.1")
            .arg("--remote-debugging-port=0")
            .arg(format!("--user-data-dir={}", profile.path().display()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if options.headless {
            command.arg("--headless=new");
        }
        let child = command
            .spawn()
            .with_context(|| format!("launch browser {}", options.executable.display()))?;
        Ok(Self { child, profile })
    }

    fn wait_for_endpoint(&mut self) -> Result<(u16, String)> {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        let active_port = self.profile.path().join("DevToolsActivePort");
        loop {
            if let Ok(contents) = fs::read_to_string(&active_port) {
                return parse_endpoint(&contents);
            }
            if let Some(status) = self.child.try_wait().context("poll browser startup")? {
                bail!("browser exited before CDP was ready ({status})");
            }
            if Instant::now() >= deadline {
                bail!(
                    "browser did not publish a CDP endpoint within {} seconds",
                    STARTUP_TIMEOUT.as_secs()
                );
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for BrowserProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse_endpoint(contents: &str) -> Result<(u16, String)> {
    let mut lines = contents.lines();
    let port: u16 = lines
        .next()
        .ok_or_else(|| anyhow!("CDP endpoint missing port"))?
        .parse()
        .context("CDP endpoint invalid port")?;
    if port == 0 {
        bail!("CDP endpoint port must be nonzero");
    }
    let path = lines
        .next()
        .ok_or_else(|| anyhow!("CDP endpoint missing websocket path"))?;
    let suffix = path
        .strip_prefix("/devtools/browser/")
        .ok_or_else(|| anyhow!("CDP endpoint has unexpected websocket path"))?;
    if suffix.is_empty()
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        bail!("CDP endpoint has unsafe websocket path");
    }
    if lines.next().is_some() {
        bail!("CDP endpoint has unexpected extra lines");
    }
    Ok((port, path.to_owned()))
}

struct Cdp {
    socket: WebSocket<TcpStream>,
    next_id: u64,
    responses: HashMap<u64, Value>,
    loaded: HashSet<(String, String)>,
}

impl Cdp {
    fn new(socket: WebSocket<TcpStream>) -> Self {
        Self {
            socket,
            next_id: 1,
            responses: HashMap::new(),
            loaded: HashSet::new(),
        }
    }

    fn command(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
        timeout: Duration,
    ) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let mut request = json!({"id": id, "method": method, "params": params});
        if let Some(session) = session {
            request["sessionId"] = json!(session);
        }
        self.socket
            .send(Message::Text(request.to_string().into()))
            .with_context(|| format!("send CDP {method}"))?;
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(response) = self.responses.remove(&id) {
                return parse_response(response, method);
            }
            self.read_one(deadline)
                .with_context(|| format!("wait for CDP {method}"))?;
        }
    }

    fn wait_for_load(&mut self, session: &str, loader_id: &str, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while !self
            .loaded
            .remove(&(session.to_owned(), loader_id.to_owned()))
        {
            self.read_one(deadline)?;
        }
        Ok(())
    }

    fn read_one(&mut self, deadline: Instant) -> Result<()> {
        loop {
            if Instant::now() >= deadline {
                bail!("CDP response timed out");
            }
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    let value: Value =
                        serde_json::from_str(text.as_str()).context("parse CDP JSON")?;
                    accept_message(value, &mut self.responses, &mut self.loaded)?;
                    return Ok(());
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

fn accept_message(
    value: Value,
    responses: &mut HashMap<u64, Value>,
    loaded: &mut HashSet<(String, String)>,
) -> Result<()> {
    if let Some(id) = value.get("id").and_then(Value::as_u64) {
        if responses.len() >= MAX_PENDING_RESPONSES {
            bail!("CDP pending response limit exceeded");
        }
        if responses.insert(id, value).is_some() {
            bail!("duplicate CDP response id {id}");
        }
    } else if value.get("method").and_then(Value::as_str) == Some("Page.lifecycleEvent")
        && value.pointer("/params/name").and_then(Value::as_str) == Some("load")
        && let (Some(session), Some(loader)) = (
            value.get("sessionId").and_then(Value::as_str),
            value.pointer("/params/loaderId").and_then(Value::as_str),
        )
    {
        if loaded.len() >= MAX_LOAD_EVENTS {
            bail!("CDP lifecycle event limit exceeded");
        }
        loaded.insert((session.to_owned(), loader.to_owned()));
    }
    Ok(())
}

fn parse_response(value: Value, method: &str) -> Result<Value> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[test]
    fn endpoint_rejects_remote_or_injected_paths() {
        assert_eq!(
            parse_endpoint("12345\n/devtools/browser/abc-123\n").unwrap(),
            (12345, "/devtools/browser/abc-123".to_owned())
        );
        for bad in [
            "0\n/devtools/browser/abc",
            "555\n/devtools/page/abc",
            "555\n/devtools/browser/a?x=1",
            "555\n/devtools/browser/../x",
            "555\n/devtools/browser/abc\nextra",
        ] {
            assert!(parse_endpoint(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn response_ids_events_and_errors() {
        let mut responses = HashMap::new();
        let mut loaded = HashSet::new();
        accept_message(json!({"method":"Page.lifecycleEvent","sessionId":"one","params":{"name":"load","loaderId":"old"}}), &mut responses, &mut loaded).unwrap();
        accept_message(json!({"method":"Page.lifecycleEvent","sessionId":"one","params":{"name":"load","loaderId":"new"}}), &mut responses, &mut loaded).unwrap();
        accept_message(
            json!({"id":7,"result":{"value":42}}),
            &mut responses,
            &mut loaded,
        )
        .unwrap();
        assert!(loaded.remove(&("one".into(), "new".into())));
        assert!(loaded.contains(&("one".into(), "old".into())));
        assert_eq!(
            parse_response(responses.remove(&7).unwrap(), "test").unwrap()["value"],
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
        assert_eq!(
            png_dimensions(
                &[
                    b"\x89PNG\r\n\x1a\n".as_slice(),
                    &[0, 0, 0, 13],
                    b"IHDR",
                    &[0, 0, 1, 44, 0, 0, 1, 44]
                ]
                .concat()
            )
            .unwrap(),
            (300, 300)
        );
    }

    #[test]
    fn failed_browser_start_is_bounded() {
        let options = CaptureOptions {
            executable: PathBuf::from("/bin/false"),
            headless: true,
        };
        let mut browser = BrowserProcess::start(&options).unwrap();
        assert!(
            browser
                .wait_for_endpoint()
                .unwrap_err()
                .to_string()
                .contains("exited before CDP")
        );
    }

    #[test]
    fn protocol_buffers_reject_event_floods() {
        let mut responses = HashMap::new();
        let mut loaded = HashSet::new();
        for id in 0..MAX_PENDING_RESPONSES {
            accept_message(json!({"id": id, "result": {}}), &mut responses, &mut loaded).unwrap();
        }
        assert!(
            accept_message(
                json!({"id": 999, "result": {}}),
                &mut responses,
                &mut loaded
            )
            .is_err()
        );
        for id in 0..MAX_LOAD_EVENTS {
            accept_message(json!({"method":"Page.lifecycleEvent", "sessionId":"one", "params":{"name":"load", "loaderId":id.to_string()}}), &mut responses, &mut loaded).unwrap();
        }
        assert!(accept_message(json!({"method":"Page.lifecycleEvent", "sessionId":"one", "params":{"name":"load", "loaderId":"overflow"}}), &mut responses, &mut loaded).is_err());
    }

    // Run with BROXSER_TEST_BROWSER=/path/to/helium (or /usr/bin/chromium
    // for protocol diagnostics) cargo test -p broxser-engine -- --ignored.
    #[test]
    #[ignore = "requires an installed CDP browser"]
    fn live_capture_has_expected_pixels_and_isolated_sessions() {
        let executable = PathBuf::from(
            env::var_os("BROXSER_TEST_BROWSER")
                .expect("set BROXSER_TEST_BROWSER to a CDP browser executable"),
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let next_cookie = Arc::new(AtomicUsize::new(1));
        let server_stop = Arc::clone(&stop);
        let server_cookies = Arc::clone(&next_cookie);
        let server = thread::spawn(move || {
            while !server_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut request = Vec::new();
                        let mut chunk = [0_u8; 1024];
                        while request.len() < 8192 && !request.ends_with(b"\r\n\r\n") {
                            match stream.read(&mut chunk) {
                                Ok(0) | Err(_) => break,
                                Ok(count) => request.extend_from_slice(&chunk[..count]),
                            }
                        }
                        let request = String::from_utf8_lossy(&request);
                        let cookie = request
                            .lines()
                            .find_map(|line| {
                                line.split_once(':')
                                    .filter(|(key, _)| key.eq_ignore_ascii_case("cookie"))
                                    .map(|(_, value)| value)
                            })
                            .and_then(|value| {
                                value
                                    .split(';')
                                    .find_map(|part| part.trim().strip_prefix("fixture_session="))
                            })
                            .and_then(|value| value.parse::<usize>().ok())
                            .unwrap_or_else(|| server_cookies.fetch_add(1, Ordering::Relaxed));
                        let color = if cookie % 2 == 1 {
                            "#ff0000"
                        } else {
                            "#00ff00"
                        };
                        let body = format!(
                            "<!doctype html><html><head><meta name=viewport content='width=device-width, initial-scale=1'></head><body style='margin:0;background:{color};min-height:100vh'></body></html>"
                        );
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nSet-Cookie: fixture_session={cookie}; Path=/; SameSite=Lax\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("fixture server: {error}"),
                }
            }
        });

        let mut workspace = Workspace::demo();
        workspace.url = format!("http://127.0.0.1:{}/", address.port());
        for device in &mut workspace.devices {
            device.width = 300;
            device.height = 300;
            device.device_scale_factor = 1.0;
            device.mobile = false;
            device.touch = false;
        }
        workspace.devices[0].device_scale_factor = 2.0;
        let output = tempfile::tempdir().unwrap();
        let report = capture_workspace(
            &workspace,
            &CaptureOptions {
                executable,
                headless: true,
            },
            output.path(),
        )
        .unwrap();
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
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
    }
}
