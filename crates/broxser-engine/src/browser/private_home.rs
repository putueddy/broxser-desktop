//! Exercise the normal browser launch with disposable caller environments.

use super::*;
use std::os::unix::ffi::OsStringExt;

#[cfg(target_os = "linux")]
#[test]
fn private_home_launch_preserves_display_auth_and_validates_cache_paths() {
    let fixture = tempfile::tempdir().unwrap();
    let caller_home = fixture.path().join("caller-home");
    fs::create_dir(&caller_home).unwrap();
    fs::write(
        caller_home.join(".Xauthority"),
        b"test authorization fixture",
    )
    .unwrap();
    let explicit_auth = fixture.path().join("explicit-auth").into_os_string();
    let absolute_cache = fixture.path().join("cache").into_os_string();
    let non_utf8_auth = fixture
        .path()
        .join(OsString::from_vec(b"auth-\xff".to_vec()))
        .into_os_string();
    let cases = [
        (
            "implicit authorization, default cache",
            Some(caller_home.clone().into_os_string()),
            None,
            None,
        ),
        (
            "explicit authorization, absolute cache",
            Some(caller_home.clone().into_os_string()),
            Some(explicit_auth),
            Some(absolute_cache),
        ),
        (
            "relative authorization, empty cache fallback",
            Some(caller_home.clone().into_os_string()),
            Some("caller-home/.Xauthority".into()),
            Some("".into()),
        ),
        (
            "non-UTF8 authorization, relative cache fallback",
            Some(caller_home.into_os_string()),
            Some(non_utf8_auth),
            Some("cache".into()),
        ),
        (
            "explicit empty authorization remains empty",
            Some("caller-home".into()),
            Some("".into()),
            Some("cache".into()),
        ),
        (
            "relative original home preserves authorization, private cache",
            Some("caller-home".into()),
            None,
            None,
        ),
        (
            "absent original home, private authorization and cache",
            None,
            None,
            Some("cache".into()),
        ),
        (
            "empty original home, private authorization and cache",
            Some("".into()),
            None,
            Some("".into()),
        ),
    ];
    for (description, home, auth, cache) in cases {
        // Re-execute the existing launch assertion in a separate process so
        // these overrides cannot race other tests. Nothing reads auth contents.
        let mut command = Command::new("/proc/self/exe");
        command
            .args([
                "browser::tests::browser_runs_in_a_private_home",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .current_dir(fixture.path())
            .env(
                "SSLKEYLOGFILE",
                fixture.path().join("caller-home/tls-secrets"),
            )
            .env("XDG_DATA_HOME", fixture.path().join("caller-data"))
            .env("XDG_CONFIG_HOME", fixture.path().join("caller-config"));
        for (name, value) in [
            ("HOME", home),
            ("XAUTHORITY", auth),
            ("XDG_CACHE_HOME", cache),
        ] {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{description}:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    assert_eq!(
        fs::read(fixture.path().join("caller-home/.Xauthority")).unwrap(),
        b"test authorization fixture"
    );
}

#[test]
fn authorization_mapping_preserves_non_utf8_home_and_explicit_override() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("private-home");
    let caller_home = fixture
        .path()
        .join(OsString::from_vec(b"caller-\xff".to_vec()));
    let mapped = environment(&home, |name| match name {
        "HOME" => Some(caller_home.clone().into_os_string()),
        _ => None,
    });
    assert_eq!(
        mapped[4],
        (
            "XAUTHORITY",
            Some(caller_home.join(".Xauthority").into_os_string())
        )
    );
    let mapped = environment(&home, |name| match name {
        "XAUTHORITY" => Some("relative-auth".into()),
        _ => None,
    });
    assert_eq!(mapped[4], ("XAUTHORITY", Some("relative-auth".into())));
}
