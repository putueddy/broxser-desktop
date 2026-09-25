//! The `broxser` binary runs its browser guardian from `main` (ADR 0007):
//! killing the CLI during a capture stops the browser and removes its profile,
//! and leaves the workspace file untouched.

use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Whether the lease identity (PID and kernel start time) names a running,
/// non-zombie process rather than a reused PID.
fn running(identity: &Value) -> bool {
    let Some(pid) = identity["pid"].as_u64() else {
        return false;
    };
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let fields: Vec<&str> = stat[stat.rfind(')').unwrap() + 1..]
        .split_whitespace()
        .collect();
    !matches!(fields[0], "Z" | "X") && fields[19].parse().ok() == identity["start_time"].as_u64()
}

fn command_line(identity: &Value) -> String {
    let pid = identity["pid"].as_u64().unwrap();
    String::from_utf8_lossy(&fs::read(format!("/proc/{pid}/cmdline")).unwrap()).replace('\0', " ")
}

#[test]
fn killed_capture_leaves_no_browser_profile_or_workspace_change() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let temporary = tempfile::tempdir().unwrap();
    // The user's workspace file must come through unchanged.
    let workspace = temporary.path().join("workspace.json");
    fs::copy(manifest.join("../../examples/workspace.json"), &workspace).unwrap();
    let original = (
        fs::read(&workspace).unwrap(),
        fs::metadata(&workspace).unwrap().modified().unwrap(),
    );
    let mut cli = Command::new(env!("CARGO_BIN_EXE_broxser"))
        .arg("capture")
        .arg("--workspace")
        .arg(&workspace)
        .arg("--browser")
        .arg(manifest.join("../broxser-engine/testdata/fake-browser/never-ready"))
        .arg("--output")
        .arg(temporary.path().join("captures"))
        // Profiles go to the test's own temporary directory.
        .env("TMPDIR", temporary.path())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let (profile, lease) = loop {
        let leased = fs::read_dir(temporary.path())
            .unwrap()
            .filter_map(|entry| Some(entry.ok()?.path()))
            .filter(|path| path.to_string_lossy().contains("/broxser-cdp-"))
            .find_map(|profile| {
                let lease = fs::read(profile.join("broxser-lease.json")).ok()?;
                let lease: Value = serde_json::from_slice(&lease).ok()?;
                (lease["guardian"].is_object() && lease["browser"].is_object())
                    .then_some((profile, lease))
            });
        if let Some(leased) = leased {
            break leased;
        }
        assert!(
            cli.try_wait().unwrap().is_none(),
            "the CLI exited before it started a browser"
        );
        assert!(
            Instant::now() < deadline,
            "the CLI never started a guarded browser"
        );
        thread::sleep(Duration::from_millis(20));
    };
    let (guardian, browser) = (&lease["guardian"], &lease["browser"]);
    assert_eq!(lease["owner"]["pid"].as_u64(), Some(u64::from(cli.id())));
    assert!(running(guardian) && running(browser), "{lease}");
    assert!(
        command_line(guardian).starts_with("broxser-guardian --broxser-browser-guardian"),
        "{}",
        command_line(guardian)
    );
    let killed = Instant::now();
    cli.kill().unwrap();
    cli.wait().unwrap();
    while running(guardian) || running(browser) || profile.exists() {
        assert!(
            killed.elapsed() < Duration::from_secs(5),
            "browser, guardian or profile left five seconds after the CLI was killed"
        );
        thread::sleep(Duration::from_millis(5));
    }
    println!(
        "CLI SIGKILL during capture: browser, guardian and profile gone after {} ms",
        killed.elapsed().as_millis()
    );
    assert_eq!(
        (
            fs::read(&workspace).unwrap(),
            fs::metadata(&workspace).unwrap().modified().unwrap()
        ),
        original,
        "the workspace file changed"
    );
}
