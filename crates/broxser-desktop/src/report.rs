//! Bug reports (ADR 0024): one device's screenshot and a text report, written
//! only when the user asks, each into a new folder of the reports directory.
//! Broxser never reads them back, sends them anywhere or deletes them.

use broxser_core::{Device, redact_text, redact_url};
use broxser_engine::{ConsoleEntry, ConsoleKind, ConsoleLevel, ConsoleScope};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const SCREENSHOT_FILE: &str = "screenshot.png";
pub(crate) const REPORT_FILE: &str = "report.md";

/// What a report says about one device.
pub(crate) struct Report<'a> {
    pub device: &'a Device,
    pub session: &'a str,
    /// The committed main-frame address; the report keeps only where it points.
    pub url: &'a str,
    /// Browser product and protocol version, when the runtime runs.
    pub browser: Option<(&'a str, &'a str)>,
    /// The device's console, oldest first, and its counts.
    pub console: &'a [ConsoleEntry],
    pub errors: u32,
    pub warnings: u32,
    pub saved: SystemTime,
    /// Pixel size of the screenshot.
    pub screenshot: (u32, u32),
}

/// `BROXSER_REPORT_DIR` if it is absolute, else `Broxser` in the XDG download
/// directory, else `~/Downloads/Broxser`.
pub(crate) fn reports_dir(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    if let Some(dir) = var("BROXSER_REPORT_DIR").map(PathBuf::from) {
        return dir.is_absolute().then_some(dir);
    }
    let home = var("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())?;
    let config = var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let downloads = fs::read_to_string(config.join("user-dirs.dirs"))
        .ok()
        .and_then(|dirs| download_dir(&dirs, &home))
        .unwrap_or_else(|| home.join("Downloads"));
    Some(downloads.join("Broxser"))
}

/// `XDG_DOWNLOAD_DIR` of a `user-dirs.dirs` file: an absolute path or one
/// under `$HOME`; the home directory itself means the directory is disabled.
fn download_dir(dirs: &str, home: &Path) -> Option<PathBuf> {
    let value = dirs
        .lines()
        .find_map(|line| line.trim().strip_prefix("XDG_DOWNLOAD_DIR="))?
        .trim()
        .trim_matches('"');
    let dir = match value.strip_prefix("$HOME") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(value),
    };
    (dir.is_absolute() && dir != home).then_some(dir)
}

/// Writes the screenshot and the report into a new folder of `root` named
/// after the time and the device, private to the user. Returns the folder.
pub(crate) fn write_report(
    root: &Path,
    device_id: &str,
    saved: SystemTime,
    png: &[u8],
    text: &str,
) -> io::Result<PathBuf> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)?;
    let stamp = utc(saved).replace([' ', ':'], "-");
    let stamp = stamp.trim_end_matches("-UTC");
    let mut folder = root.join(format!("{stamp}-{device_id}"));
    let mut next = 2;
    loop {
        match fs::DirBuilder::new().mode(0o700).create(&folder) {
            Ok(()) => break,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && next < 100 => {
                folder = root.join(format!("{stamp}-{device_id}-{next}"));
                next += 1;
            }
            Err(error) => return Err(error),
        }
    }
    for (name, bytes) in [(SCREENSHOT_FILE, png), (REPORT_FILE, text.as_bytes())] {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(folder.join(name))?
            .write_all(bytes)?;
    }
    Ok(folder)
}

/// The report as Markdown. Page text is redacted and indented, so a bug
/// tracker shows it as code and interprets nothing in it.
pub(crate) fn report_text(report: &Report) -> String {
    let device = report.device;
    let mut traits = vec![format!(
        "{} × {} CSS px at {}×",
        device.width, device.height, device.device_scale_factor
    )];
    if device.mobile {
        traits.push("mobile".into());
    }
    if device.touch {
        traits.push("touch".into());
    }
    let page = redact_url(report.url);
    let mut text = format!(
        "# Broxser bug report\n\n\
         - Device: {} ({})\n\
         - Session: {}\n\
         - Page: {}\n",
        device.name,
        traits.join(", "),
        report.session,
        if page.is_empty() { "none" } else { &page },
    );
    match report.browser {
        Some((product, protocol)) => text.push_str(&format!(
            "- Browser: {product}, CDP {protocol}, headless Helium (ADR 0019)\n"
        )),
        None => text.push_str("- Browser: not running\n"),
    }
    text.push_str(&format!(
        "- Broxser: {}\n- Saved: {}\n- Screenshot: {SCREENSHOT_FILE}, {} × {} px\n\n",
        env!("CARGO_PKG_VERSION"),
        utc(report.saved),
        report.screenshot.0,
        report.screenshot.1
    ));
    text.push_str(
        "Addresses show only their scheme, host and path. In console text, the query,\n\
         fragment and user information of addresses, JWT-shaped tokens and bearer\n\
         credentials were removed; anything else the page printed is included as it was\n\
         logged. Review the text and the screenshot before sharing them.\n\n",
    );
    let counts = match (report.errors, report.warnings) {
        (0, 0) => "no errors or warnings".to_owned(),
        (errors, warnings) => format!("{errors} error(s), {warnings} warning(s)"),
    };
    text.push_str(&format!("## Console: {counts}, oldest first\n\n"));
    if report.console.is_empty() {
        text.push_str("No console messages.\n");
    }
    for entry in report.console {
        text.push_str("    ");
        text.push_str(&console_line(entry));
        text.push('\n');
    }
    text
}

/// One console entry on one line: level, what reported it, text, location.
fn console_line(entry: &ConsoleEntry) -> String {
    let level = match entry.level {
        ConsoleLevel::Error => "Error",
        ConsoleLevel::Warning => "Warning",
        ConsoleLevel::Info => "Info",
    };
    if entry.kind == ConsoleKind::Navigation {
        return format!("{level:<8}Navigated to {}", entry.location);
    }
    let mut tags = Vec::new();
    match entry.kind {
        ConsoleKind::Exception => tags.push("uncaught".to_owned()),
        ConsoleKind::Network => tags.push("request".to_owned()),
        ConsoleKind::Browser => tags.push("browser".to_owned()),
        ConsoleKind::Console | ConsoleKind::Navigation => {}
    }
    // The same tags as the Console panel.
    match entry.scope {
        ConsoleScope::MainFrame => {}
        ConsoleScope::Subframe => tags.push("frame".into()),
        ConsoleScope::Unknown => tags.push("frame unknown".into()),
    }
    if entry.repeats > 1 {
        tags.push(format!("×{}", entry.repeats));
    }
    let mut line = format!("{level:<8}");
    if !tags.is_empty() {
        line.push_str(&format!("[{}] ", tags.join(", ")));
    }
    line.push_str(&redact_text(&entry.text));
    if !entry.location.is_empty() {
        line.push_str(" · ");
        line.push_str(&entry.location);
    }
    line
}

/// `2026-09-27 22:40:12 UTC`.
fn utc(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    fn phone() -> Device {
        Device {
            id: "phone".into(),
            name: "Phone".into(),
            width: 390,
            height: 844,
            device_scale_factor: 2.0,
            mobile: true,
            touch: true,
            session: "guest".into(),
        }
    }

    fn entry(level: ConsoleLevel, kind: ConsoleKind, text: &str, location: &str) -> ConsoleEntry {
        ConsoleEntry {
            level,
            kind,
            text: text.into(),
            location: location.into(),
            scope: ConsoleScope::MainFrame,
            repeats: 1,
        }
    }

    #[test]
    fn times_are_utc_calendar_dates() {
        let at = |seconds| utc(UNIX_EPOCH + Duration::from_secs(seconds));
        assert_eq!(at(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(at(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(at(1_790_462_412), "2026-09-26 22:40:12 UTC");
    }

    #[test]
    fn a_report_names_the_device_and_keeps_secrets_out() {
        let device = phone();
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.c2lnbmF0dXJlLXZhbHVl";
        let console = [
            entry(
                ConsoleLevel::Info,
                ConsoleKind::Navigation,
                "Navigated",
                "https://app.test/cart",
            ),
            ConsoleEntry {
                scope: ConsoleScope::Subframe,
                repeats: 3,
                ..entry(
                    ConsoleLevel::Error,
                    ConsoleKind::Exception,
                    &format!(
                        "Uncaught Error: token {jwt} rejected by https://api.test/pay?card=4111"
                    ),
                    "https://app.test/app.js:10:3",
                )
            },
            entry(
                ConsoleLevel::Warning,
                ConsoleKind::Console,
                "# not a heading `code`",
                "",
            ),
        ];
        let text = report_text(&Report {
            device: &device,
            session: "Guest",
            url: "https://user:pw@app.test/cart?session=S3CR3T#step=2",
            browser: Some(("Chrome/154.0.8037.57", "1.3")),
            console: &console,
            errors: 3,
            warnings: 1,
            saved: UNIX_EPOCH + Duration::from_secs(1_790_462_412),
            screenshot: (780, 1688),
        });
        for wanted in [
            "- Device: Phone (390 × 844 CSS px at 2×, mobile, touch)",
            "- Session: Guest",
            "- Page: https://app.test/cart\n",
            "- Browser: Chrome/154.0.8037.57, CDP 1.3",
            "- Saved: 2026-09-26 22:40:12 UTC",
            "- Screenshot: screenshot.png, 780 × 1688 px",
            "## Console: 3 error(s), 1 warning(s), oldest first",
            "    Info    Navigated to https://app.test/cart\n",
            "    Error   [uncaught, frame, ×3] Uncaught Error: token [token removed] rejected by https://api.test/pay · https://app.test/app.js:10:3\n",
            "    Warning # not a heading `code`\n",
        ] {
            assert!(text.contains(wanted), "{wanted:?} missing in:\n{text}");
        }
        for secret in ["S3CR3T", "step=2", "user:pw", "4111", jwt] {
            assert!(!text.contains(secret), "{secret} leaked:\n{text}");
        }
    }

    #[test]
    fn reports_go_to_new_private_folders_in_the_reports_directory() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("Downloads").join("Broxser");
        let saved = UNIX_EPOCH + Duration::from_secs(1_790_462_412);
        let first = write_report(&dir, "phone", saved, b"png", "text").unwrap();
        let second = write_report(&dir, "phone", saved, b"png2", "text2").unwrap();
        assert_eq!(first, dir.join("2026-09-26-22-40-12-phone"));
        assert_eq!(second, dir.join("2026-09-26-22-40-12-phone-2"));
        assert_eq!(fs::read(first.join(SCREENSHOT_FILE)).unwrap(), b"png");
        assert_eq!(
            fs::read_to_string(second.join(REPORT_FILE)).unwrap(),
            "text2"
        );
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&first), 0o700);
        assert_eq!(mode(&first.join(REPORT_FILE)), 0o600);
    }

    #[test]
    fn the_reports_directory_follows_the_override_then_the_download_directory() {
        let config = tempfile::tempdir().unwrap();
        let vars = |pairs: Vec<(&'static str, OsString)>| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.clone())
            }
        };
        let home = OsString::from("/home/me");
        assert_eq!(
            reports_dir(vars(vec![("BROXSER_REPORT_DIR", "/tmp/reports".into())])),
            Some(PathBuf::from("/tmp/reports"))
        );
        assert_eq!(
            reports_dir(vars(vec![("BROXSER_REPORT_DIR", "reports".into())])),
            None
        );
        assert_eq!(
            reports_dir(vars(vec![("HOME", home.clone())])),
            Some(PathBuf::from("/home/me/Downloads/Broxser"))
        );
        fs::write(
            config.path().join("user-dirs.dirs"),
            "# written by xdg-user-dirs-update\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_DOWNLOAD_DIR=\"$HOME/Unduhan\"\n",
        )
        .unwrap();
        let with_config = vec![
            ("HOME", home.clone()),
            ("XDG_CONFIG_HOME", config.path().as_os_str().to_owned()),
        ];
        assert_eq!(
            reports_dir(vars(with_config.clone())),
            Some(PathBuf::from("/home/me/Unduhan/Broxser"))
        );
        // A download directory set to the home directory is disabled.
        fs::write(
            config.path().join("user-dirs.dirs"),
            "XDG_DOWNLOAD_DIR=\"$HOME/\"\n",
        )
        .unwrap();
        assert_eq!(
            reports_dir(vars(with_config)),
            Some(PathBuf::from("/home/me/Downloads/Broxser"))
        );
        assert_eq!(reports_dir(vars(vec![])), None);
    }
}
