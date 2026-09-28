//! Tests only disposable caller stores; no personal NSS or desktop keyring.
use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;

const CAPTURE_URL: &str = "BROXSER_TEST_PRIVATE_HOME_CAPTURE";
const PROFILE_ROOT: &str = "BROXSER_TEST_PRIVATE_HOME_PROFILES";

#[test]
fn capture_with_disposable_caller_home() {
    let Ok(url) = std::env::var(CAPTURE_URL) else {
        return;
    };
    let root = std::path::PathBuf::from(std::env::var_os(PROFILE_ROOT).unwrap());
    let output = tempfile::tempdir().unwrap();
    let outcome = crate::capture::run(
        &workspace(url),
        &BrowserOptions {
            executable: test_browser(),
            headless: true,
            profile_root: Some(root.clone()),
            cancel: Cancellation::new(),
        },
        output.path(),
        crate::capture::Plan::default(),
    );
    let error = outcome
        .result
        .expect_err("caller NSS trust must not reach the private browser");
    let message = format!("{error:#}");
    assert!(
        message.contains("net::ERR_CERT_AUTHORITY_INVALID")
            && message.contains("CACertificates policy"),
        "{message}"
    );
    crate::test_support::assert_cleaned_up(&root, &outcome.diagnostics.processes);
}

fn certutil(database: &Path, args: &[&str]) {
    let result = std::process::Command::new("certutil")
        .arg("-d")
        .arg(format!("sql:{}", database.display()))
        .args(args)
        .output()
        .expect("certutil is required (libnss3-tools on Ubuntu)");
    assert!(
        result.status.success(),
        "certutil failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn snapshot(directory: &Path) -> Vec<(std::ffi::OsString, Vec<u8>)> {
    let mut files: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), fs::read(entry.path()).unwrap())
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

#[test]
#[ignore = "requires an installed CDP browser, openssl and certutil"]
fn live_capture_ignores_caller_nss_trust_and_never_writes_ambient_tls_keys() {
    let server = SelfSignedServer::start();
    for use_xdg in [false, true] {
        let root = profile_root();
        let caller_home = root.path().join("caller-home");
        let caller_data = root.path().join("caller-data");
        let profiles = root.path().join("profiles");
        for path in [&caller_home, &caller_data, &profiles] {
            fs::create_dir(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let database = if use_xdg {
            caller_data.join("pki/nssdb")
        } else {
            caller_home.join(".pki/nssdb")
        };
        fs::create_dir_all(&database).unwrap();
        certutil(&database, &["-N", "--empty-password"]);
        let certificate = server._files.path().join("cert.pem");
        certutil(
            &database,
            &[
                "-A",
                "-n",
                "broxser-test-root",
                "-t",
                "C,,",
                "-i",
                certificate.to_str().unwrap(),
            ],
        );
        certutil(&database, &["-V", "-n", "broxser-test-root", "-u", "V"]);
        let before = snapshot(&database);
        let keylog = caller_home.join("tls-keys.log");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "live::tests::private_home::capture_with_disposable_caller_home",
                "--exact",
                "--nocapture",
            ])
            .env(CAPTURE_URL, format!("https://127.0.0.1:{}/", server.port))
            .env(PROFILE_ROOT, &profiles)
            .env("HOME", &caller_home)
            .env("XDG_CONFIG_HOME", caller_home.join("config"))
            .env("XDG_CACHE_HOME", caller_home.join("cache"))
            .env("SSLKEYLOGFILE", &keylog)
            .env_remove("BROXSER_TEST_ROLE");
        if use_xdg {
            child.env("XDG_DATA_HOME", &caller_data);
        } else {
            child.env_remove("XDG_DATA_HOME");
        }
        let result = child.output().unwrap();
        assert!(
            result.status.success(),
            "xdg={use_xdg}: {}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            before == snapshot(&database),
            "caller NSS database changed (xdg={use_xdg})"
        );
        assert!(
            !keylog.exists(),
            "TLS secrets persisted outside the profile (xdg={use_xdg})"
        );
        assert_eq!(
            fs::read_dir(&profiles).unwrap().count(),
            0,
            "private profiles remain"
        );
    }
}
