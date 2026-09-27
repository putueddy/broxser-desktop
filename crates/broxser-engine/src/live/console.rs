//! Page console output, uncaught errors and browser log entries kept per
//! device (ADR 0023). Everything here is untrusted page text: it is bounded,
//! put on one line and shown, never logged, stored on disk or acted on.

use serde_json::Value;
use std::collections::VecDeque;

/// Entries kept per device; older ones are dropped, counts keep counting.
pub const MAX_CONSOLE_ENTRIES: usize = 200;
/// Longest entry text, in characters.
pub const MAX_CONSOLE_TEXT: usize = 1000;
/// Longest entry location, in characters.
const MAX_LOCATION: usize = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleLevel {
    Error,
    Warning,
    Info,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleKind {
    /// A `console` call of the page.
    Console,
    /// An uncaught exception or an unhandled promise rejection.
    Exception,
    /// A request the browser reported as failed, such as a 404.
    Network,
    /// Another browser log entry, such as a blocked request or a violation.
    Browser,
    /// The device's main frame committed a new document at `location`.
    Navigation,
}

/// One line of the device console. `text` is page text; `location` is a
/// script or resource address reduced to its scheme, host and path, plus the
/// line and column when the browser reported them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleEntry {
    pub level: ConsoleLevel,
    pub kind: ConsoleKind,
    pub text: String,
    pub location: String,
    /// Reported by a frame inside the page rather than the page itself.
    pub subframe: bool,
    /// How many times this entry arrived in a row; at least 1.
    pub repeats: u32,
}

/// The console of one device: the newest entries and the counts since the
/// runtime started or the user cleared it.
#[derive(Default)]
pub(crate) struct ConsoleLog {
    entries: VecDeque<ConsoleEntry>,
    pub errors: u32,
    pub warnings: u32,
}

impl ConsoleLog {
    /// Adds `entry`, or counts it on the last entry when it repeats it.
    pub fn push(&mut self, entry: ConsoleEntry) {
        match entry.level {
            ConsoleLevel::Error => self.errors = self.errors.saturating_add(1),
            ConsoleLevel::Warning => self.warnings = self.warnings.saturating_add(1),
            ConsoleLevel::Info => {}
        }
        if let Some(last) = self.entries.back_mut()
            && last.level == entry.level
            && last.kind == entry.kind
            && last.subframe == entry.subframe
            && last.text == entry.text
            && last.location == entry.location
        {
            last.repeats = last.repeats.saturating_add(1);
            return;
        }
        if self.entries.len() == MAX_CONSOLE_ENTRIES {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn entries(&self) -> Vec<ConsoleEntry> {
        self.entries.iter().cloned().collect()
    }
}

/// A `Runtime.consoleAPICalled` event as an entry, or `None` for calls that
/// show nothing (`console.groupEnd`, `console.clear` and profiling).
/// `console.clear` does not clear Broxser's console: a page could otherwise
/// hide its own errors.
pub(crate) fn console_call(params: &Value) -> Option<ConsoleEntry> {
    let kind = params.get("type").and_then(Value::as_str)?;
    let level = match kind {
        "error" | "assert" => ConsoleLevel::Error,
        "warning" => ConsoleLevel::Warning,
        "endGroup" | "clear" | "profile" | "profileEnd" => return None,
        _ => ConsoleLevel::Info,
    };
    let args = params
        .get("args")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut text = Text::default();
    if kind == "assert" {
        text.push("Assertion failed");
        if !args.is_empty() {
            text.push(": ");
        }
    }
    format_args(args, &mut text);
    let frame = params.pointer("/stackTrace/callFrames/0");
    Some(ConsoleEntry {
        level,
        kind: ConsoleKind::Console,
        text: text.finish(),
        location: frame.map(call_frame_location).unwrap_or_default(),
        subframe: false,
        repeats: 1,
    })
}

/// A `Runtime.exceptionThrown` event as an entry: "Uncaught" or "Uncaught
/// (in promise)" followed by the first line of what was thrown.
pub(crate) fn exception(params: &Value) -> Option<ConsoleEntry> {
    let details = params.get("exceptionDetails")?;
    let prefix = details
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("Uncaught");
    let mut text = Text::default();
    text.push(prefix);
    if let Some(thrown) = details.get("exception") {
        let mut shown = Text::default();
        remote_object(thrown, &mut shown, true);
        let shown = shown.finish();
        if !shown.is_empty() && !prefix.ends_with(shown.as_str()) {
            text.push(" ");
            text.push(&shown);
        }
    }
    let location = match details.get("url").and_then(Value::as_str) {
        Some(url) if !url.is_empty() => location(
            url,
            details.get("lineNumber").and_then(Value::as_u64),
            details.get("columnNumber").and_then(Value::as_u64),
        ),
        _ => details
            .pointer("/stackTrace/callFrames/0")
            .map(call_frame_location)
            .unwrap_or_default(),
    };
    Some(ConsoleEntry {
        level: ConsoleLevel::Error,
        kind: ConsoleKind::Exception,
        text: text.finish(),
        location,
        subframe: false,
        repeats: 1,
    })
}

/// A `Log.entryAdded` event as an entry. Verbose entries are left out.
pub(crate) fn log_entry(params: &Value) -> Option<ConsoleEntry> {
    let entry = params.get("entry")?;
    let level = match entry.get("level").and_then(Value::as_str)? {
        "error" => ConsoleLevel::Error,
        "warning" => ConsoleLevel::Warning,
        "info" => ConsoleLevel::Info,
        _ => return None,
    };
    let kind = match entry.get("source").and_then(Value::as_str)? {
        "network" => ConsoleKind::Network,
        // Older browsers also reported console calls here.
        "console-api" => return None,
        _ => ConsoleKind::Browser,
    };
    let mut text = Text::default();
    text.push(
        entry
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    Some(ConsoleEntry {
        level,
        kind,
        text: text.finish(),
        location: location(
            entry.get("url").and_then(Value::as_str).unwrap_or_default(),
            entry.get("lineNumber").and_then(Value::as_u64),
            None,
        ),
        subframe: false,
        repeats: 1,
    })
}

/// The entry for a new document in the device's main frame.
pub(crate) fn navigation(url: &str) -> ConsoleEntry {
    ConsoleEntry {
        level: ConsoleLevel::Info,
        kind: ConsoleKind::Navigation,
        text: "Navigated".into(),
        location: location(url, None, None),
        subframe: false,
        repeats: 1,
    }
}

/// Where a script or resource is, for display and bug reports: hierarchical
/// addresses keep their scheme, host, port and path, but lose user
/// information, query and fragment, which can carry codes and tokens; other
/// addresses (`data:`, `blob:`, `about:`) keep only their scheme. Lines and
/// columns arrive zero-based and are shown one-based.
pub(crate) fn location(url: &str, line: Option<u64>, column: Option<u64>) -> String {
    let Some((scheme, rest)) = url.split_once(':') else {
        return String::new();
    };
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return String::new();
    }
    let mut shown = String::new();
    shown.push_str(scheme);
    shown.push(':');
    if let Some(rest) = rest.strip_prefix("//") {
        let end = rest.find(['?', '#']).unwrap_or(rest.len());
        let rest = &rest[..end];
        let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        shown.push_str("//");
        shown.push_str(host);
        shown.push_str(path);
    }
    let mut text = Text::with_limit(MAX_LOCATION);
    text.push(&shown);
    let mut shown = text.finish();
    if let Some(line) = line {
        shown.push_str(&format!(":{}", line.saturating_add(1)));
        if let Some(column) = column {
            shown.push_str(&format!(":{}", column.saturating_add(1)));
        }
    }
    shown
}

fn call_frame_location(frame: &Value) -> String {
    location(
        frame.get("url").and_then(Value::as_str).unwrap_or_default(),
        frame.get("lineNumber").and_then(Value::as_u64),
        frame.get("columnNumber").and_then(Value::as_u64),
    )
}

/// Console arguments as DevTools would print them on one line: a leading
/// format string takes the arguments its `%s`, `%d`, `%i`, `%f`, `%o`, `%O`
/// and `%c` ask for (`%c` styles, so it shows nothing), and the others follow
/// separated by spaces.
fn format_args(args: &[Value], text: &mut Text) {
    let mut rest = args.iter();
    if let Some(format) = args
        .first()
        .filter(|first| first.get("type").and_then(Value::as_str) == Some("string"))
        .and_then(|first| first.get("value").and_then(Value::as_str))
    {
        rest.next();
        let mut chars = format.chars();
        while let Some(c) = chars.next() {
            if text.full() {
                return;
            }
            if c != '%' {
                text.push_char(c);
                continue;
            }
            match chars.clone().next() {
                Some('%') => {
                    chars.next();
                    text.push_char('%');
                }
                Some(spec @ ('s' | 'd' | 'i' | 'f' | 'o' | 'O' | 'c')) => {
                    chars.next();
                    let Some(arg) = rest.next() else {
                        text.push_char('%');
                        text.push_char(spec);
                        continue;
                    };
                    match spec {
                        'c' => {}
                        'd' | 'i' => text.push(&integer(arg)),
                        'f' => text.push(&float(arg)),
                        _ => remote_object(arg, text, false),
                    }
                }
                _ => text.push_char('%'),
            }
        }
    }
    for arg in rest {
        if text.full() {
            return;
        }
        if !text.is_empty() {
            text.push(" ");
        }
        remote_object(arg, text, false);
    }
}

fn number(arg: &Value) -> Option<f64> {
    arg.get("value").and_then(Value::as_f64).or_else(|| {
        arg.get("value")
            .and_then(Value::as_str)
            .and_then(|value| value.trim().parse().ok())
    })
}

fn integer(arg: &Value) -> String {
    number(arg).map_or_else(
        || "NaN".into(),
        |value| {
            if value.is_finite() {
                format!("{}", value.trunc())
            } else {
                format!("{value}")
            }
        },
    )
}

fn float(arg: &Value) -> String {
    number(arg).map_or_else(|| "NaN".into(), |value| format!("{value}"))
}

/// One CDP `RemoteObject` as text. Objects use their preview when the
/// browser sent one; `first_line` keeps only the first line of descriptions,
/// such as an error's message without its stack.
fn remote_object(object: &Value, text: &mut Text, first_line: bool) {
    let field = |name: &str| object.get(name).and_then(Value::as_str);
    let description = |text: &mut Text| {
        let description = field("description").unwrap_or_default();
        text.push(if first_line || field("type") == Some("function") {
            description.lines().next().unwrap_or_default()
        } else {
            description
        });
    };
    match field("type").unwrap_or_default() {
        "string" => text.push(field("value").unwrap_or_default()),
        "undefined" => text.push("undefined"),
        "number" | "boolean" | "bigint" => match object.get("value") {
            Some(Value::Number(value)) => text.push(&value.to_string()),
            Some(Value::Bool(value)) => text.push(if *value { "true" } else { "false" }),
            _ => text.push(
                field("unserializableValue")
                    .or(field("description"))
                    .unwrap_or_default(),
            ),
        },
        "object" => match field("subtype") {
            Some("null") => text.push("null"),
            Some("error" | "node" | "regexp" | "date") => description(text),
            _ => match object.get("preview") {
                Some(preview) if !first_line => object_preview(preview, text),
                _ => description(text),
            },
        },
        _ => description(text),
    }
}

/// `{a: 1, b: "x"}` for objects, `[1, 2]` for arrays, from a CDP
/// `ObjectPreview`; nested objects show as their description.
fn object_preview(preview: &Value, text: &mut Text) {
    let array = preview.get("subtype").and_then(Value::as_str) == Some("array");
    let description = preview
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !array && description != "Object" {
        text.push(description);
        text.push(" ");
    }
    text.push(if array { "[" } else { "{" });
    let properties = preview
        .get("properties")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    for (index, property) in properties.iter().enumerate() {
        if text.full() {
            return;
        }
        if index > 0 {
            text.push(", ");
        }
        let name = property.get("name").and_then(Value::as_str).unwrap_or("?");
        if !array {
            text.push(name);
            text.push(": ");
        }
        let value = property
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if property.get("type").and_then(Value::as_str) == Some("string") {
            text.push("\"");
            text.push(value);
            text.push("\"");
        } else {
            text.push(value);
        }
    }
    if preview.get("overflow").and_then(Value::as_bool) == Some(true) {
        text.push(if properties.is_empty() {
            "…"
        } else {
            ", …"
        });
    }
    text.push(if array { "]" } else { "}" });
}

/// Bounded one-line text: line breaks and tabs become spaces, other control
/// characters are dropped, and text beyond the limit is cut and marked.
struct Text {
    shown: String,
    chars: usize,
    limit: usize,
    cut: bool,
}

impl Default for Text {
    fn default() -> Self {
        Self::with_limit(MAX_CONSOLE_TEXT)
    }
}

impl Text {
    fn with_limit(limit: usize) -> Self {
        Self {
            shown: String::new(),
            chars: 0,
            limit,
            cut: false,
        }
    }

    fn full(&self) -> bool {
        self.cut
    }

    fn is_empty(&self) -> bool {
        self.chars == 0
    }

    fn push(&mut self, text: &str) {
        for c in text.chars() {
            if self.cut {
                return;
            }
            self.push_char(c);
        }
    }

    fn push_char(&mut self, c: char) {
        if self.cut {
            return;
        }
        let c = match c {
            '\n' | '\r' | '\t' => ' ',
            c if c.is_control() => return,
            c => c,
        };
        if self.chars == self.limit {
            self.cut = true;
            return;
        }
        self.shown.push(c);
        self.chars += 1;
    }

    fn finish(mut self) -> String {
        if self.cut {
            self.shown.push('…');
        }
        self.shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(kind: &str, args: Value) -> ConsoleEntry {
        console_call(&json!({"type": kind, "args": args, "executionContextId": 1,
            "stackTrace": {"callFrames": [{"url": "http://user:pw@127.0.0.1:8080/app.js?token=SECRET#x",
                "lineNumber": 3, "columnNumber": 7, "functionName": ""}]}}))
        .unwrap()
    }

    #[test]
    fn console_calls_become_bounded_one_line_entries() {
        let entry = call(
            "log",
            json!([{"type": "string", "value": "count %d of %s%c done %o"},
                {"type": "number", "value": 3.7, "description": "3.7"},
                {"type": "string", "value": "ten"},
                {"type": "string", "value": "color: red"},
                {"type": "object", "className": "Object", "description": "Object",
                 "preview": {"type": "object", "description": "Object", "overflow": false,
                   "properties": [{"name": "a", "type": "number", "value": "1"},
                                  {"name": "b", "type": "string", "value": "x"},
                                  {"name": "c", "type": "object", "value": "Array(3)", "subtype": "array"}]}},
                {"type": "object", "subtype": "array", "description": "Array(2)",
                 "preview": {"type": "object", "subtype": "array", "description": "Array(2)", "overflow": true,
                   "properties": [{"name": "0", "type": "number", "value": "1"}]}},
                {"type": "undefined"},
                {"type": "object", "subtype": "null", "value": null},
                {"type": "boolean", "value": true},
                {"type": "number", "unserializableValue": "NaN", "description": "NaN"},
                {"type": "bigint", "unserializableValue": "12n", "description": "12n"},
                {"type": "function", "description": "function f() {\n  return 1;\n}"},
                {"type": "object", "subtype": "error", "description": "Error: boom\n    at http://a/b.js:1:2"},
                {"type": "object", "subtype": "node", "description": "div#main.card"}]),
        );
        assert_eq!(entry.level, ConsoleLevel::Info);
        assert_eq!(entry.kind, ConsoleKind::Console);
        assert_eq!(
            entry.text,
            "count 3 of ten done {a: 1, b: \"x\", c: Array(3)} [1, …] undefined null true NaN 12n function f() { Error: boom     at http://a/b.js:1:2 div#main.card"
        );
        // Query, fragment and user information never reach the location.
        assert_eq!(entry.location, "http://127.0.0.1:8080/app.js:4:8");
        assert_eq!(entry.repeats, 1);

        let entry = call(
            "warning",
            json!([{"type": "string", "value": "line one\nline\ttwo\u{7}\u{1b}[31m"}]),
        );
        assert_eq!(entry.level, ConsoleLevel::Warning);
        assert_eq!(entry.text, "line one line two[31m");

        let entry = call("assert", json!([{"type": "string", "value": "x > 0"}]));
        assert_eq!(
            (entry.level, entry.text.as_str()),
            (ConsoleLevel::Error, "Assertion failed: x > 0")
        );
        assert_eq!(call("assert", json!([])).text, "Assertion failed");
        assert_eq!(
            call(
                "error",
                json!([{"type": "string", "value": "100%% and %q and %s"}])
            )
            .text,
            "100% and %q and %s"
        );

        let long = call(
            "log",
            json!([{"type": "string", "value": "é".repeat(5000)}, {"type": "string", "value": "more"}]),
        );
        assert_eq!(long.text.chars().count(), MAX_CONSOLE_TEXT + 1);
        assert!(long.text.ends_with("é…"));

        for silent in ["endGroup", "clear", "profile", "profileEnd"] {
            assert!(
                console_call(&json!({"type": silent, "args": []})).is_none(),
                "{silent}"
            );
        }
        assert!(console_call(&json!({"args": []})).is_none());
    }

    #[test]
    fn exceptions_and_log_entries_name_what_failed_and_where() {
        let thrown = exception(&json!({"exceptionDetails": {"text": "Uncaught", "lineNumber": 9,
            "columnNumber": 2, "url": "https://example.test/page?code=1",
            "exception": {"type": "object", "subtype": "error", "description": "TypeError: x is undefined\n    at f (https://example.test/page:10:3)"}}}))
        .unwrap();
        assert_eq!(thrown.level, ConsoleLevel::Error);
        assert_eq!(thrown.kind, ConsoleKind::Exception);
        assert_eq!(thrown.text, "Uncaught TypeError: x is undefined");
        assert_eq!(thrown.location, "https://example.test/page:10:3");

        let rejected = exception(&json!({"exceptionDetails": {"text": "Uncaught (in promise)",
            "lineNumber": 0, "columnNumber": 0,
            "stackTrace": {"callFrames": [{"url": "http://127.0.0.1/a.js", "lineNumber": 4, "columnNumber": 0}]},
            "exception": {"type": "string", "value": "plain reason"}}}))
        .unwrap();
        assert_eq!(rejected.text, "Uncaught (in promise) plain reason");
        assert_eq!(rejected.location, "http://127.0.0.1/a.js:5:1");

        let failed = log_entry(&json!({"entry": {"source": "network", "level": "error",
            "text": "Failed to load resource: the server responded with a status of 404 (Not Found)",
            "url": "http://127.0.0.1:9/missing.png?session=abc"}}))
        .unwrap();
        assert_eq!(
            (failed.level, failed.kind),
            (ConsoleLevel::Error, ConsoleKind::Network)
        );
        assert_eq!(failed.location, "http://127.0.0.1:9/missing.png");
        let blocked = log_entry(
            &json!({"entry": {"source": "security", "level": "warning", "text": "Mixed content"}}),
        )
        .unwrap();
        assert_eq!(
            (blocked.level, blocked.kind),
            (ConsoleLevel::Warning, ConsoleKind::Browser)
        );
        assert_eq!(blocked.location, "");
        assert!(
            log_entry(&json!({"entry": {"source": "other", "level": "verbose", "text": "x"}}))
                .is_none()
        );
        assert!(
            log_entry(&json!({"entry": {"source": "console-api", "level": "error", "text": "x"}}))
                .is_none()
        );
    }

    #[test]
    fn locations_keep_only_scheme_host_and_path() {
        for (url, shown) in [
            ("https://u:p@host:8443/a/b?c=d#e", "https://host:8443/a/b"),
            ("http://host", "http://host"),
            (
                "file:///home/me/site/index.html?x",
                "file:///home/me/site/index.html",
            ),
            ("data:text/html,<script>secret</script>", "data:"),
            ("blob:http://host/1234-5678", "blob:"),
            ("about:srcdoc", "about:"),
            ("", ""),
            ("no scheme here", ""),
            ("://host/x", ""),
        ] {
            assert_eq!(location(url, None, None), shown, "{url}");
        }
        let long = location(
            &format!("https://host/{}", "p".repeat(1000)),
            Some(0),
            Some(0),
        );
        assert!(long.ends_with("…:1:1"), "{long}");
        assert_eq!(long.chars().count(), MAX_LOCATION + 1 + 4);
    }

    #[test]
    fn repeats_collapse_and_old_entries_leave_while_counts_stay() {
        let mut log = ConsoleLog::default();
        let error = |text: &str| ConsoleEntry {
            level: ConsoleLevel::Error,
            kind: ConsoleKind::Console,
            text: text.into(),
            location: String::new(),
            subframe: false,
            repeats: 1,
        };
        log.push(error("same"));
        log.push(error("same"));
        log.push(ConsoleEntry {
            subframe: true,
            ..error("same")
        });
        assert_eq!(
            log.entries().iter().map(|e| e.repeats).collect::<Vec<_>>(),
            [2, 1]
        );
        for n in 0..MAX_CONSOLE_ENTRIES + 5 {
            log.push(ConsoleEntry {
                level: ConsoleLevel::Warning,
                ..error(&n.to_string())
            });
        }
        let entries = log.entries();
        assert_eq!(entries.len(), MAX_CONSOLE_ENTRIES);
        assert_eq!(entries[0].text, "5");
        assert_eq!(
            (log.errors, log.warnings),
            (3, MAX_CONSOLE_ENTRIES as u32 + 5)
        );
        log.clear();
        assert!(log.entries().is_empty());
        assert_eq!((log.errors, log.warnings), (0, 0));
    }
}
