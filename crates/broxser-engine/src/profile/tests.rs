use super::*;
use crate::test_support::profile_root;
use std::ffi::OsString;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// A profile directory in `root` with private-looking content.
fn profile_dir(root: &Path, name: &str) -> PathBuf {
    let profile = root.join(format!("{PROFILE_PREFIX}{name}"));
    fs::create_dir_all(profile.join("Default")).unwrap();
    fs::write(profile.join("Default/Cookies"), "private").unwrap();
    profile
}

fn leased(root: &Path, name: &str, lease: &Lease) -> PathBuf {
    let profile = profile_dir(root, name);
    lease.write(&profile).unwrap();
    profile
}

fn with(lease: &Lease, change: impl FnOnce(&mut Lease)) -> Lease {
    let mut lease = lease.clone();
    change(&mut lease);
    lease
}

/// A process that has exited and been reaped: its PID is free or reused.
fn exited() -> ProcessIdentity {
    let mut child = Command::new("true").spawn().unwrap();
    let identity = browser::identity(child.id()).unwrap();
    child.wait().unwrap();
    identity
}

/// Blocks in a shell builtin (no child processes) with `argument` in its
/// command line, like a browser naming its profile.
fn holding(argument: impl Into<OsString>) -> Child {
    Command::new("sh")
        .args(["-c", "read line", "sh"])
        .arg(argument.into())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap()
}

fn user_data_dir(profile: &Path) -> OsString {
    let mut argument = OsString::from("--user-data-dir=");
    argument.push(profile);
    argument
}

fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn owned_profile_is_private_leased_and_guarded() {
    let root = profile_root();
    let started = Instant::now();
    let profile = OwnedProfile::create(Some(root.path()), &Cancellation::new()).unwrap();
    println!(
        "profile, lease and ready guardian after {} ms",
        started.elapsed().as_millis()
    );
    let path = profile.path().to_owned();
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "profile mode {mode:o}");
    let lease = Lease::read(&path).unwrap();
    let host = Host::current().unwrap();
    assert_eq!(lease.version, LEASE_VERSION);
    assert_eq!(
        (&lease.boot_id, &lease.pid_namespace),
        (&host.boot_id, &host.pid_namespace)
    );
    assert_eq!(lease.owner.pid, std::process::id());
    let guardian = lease.guardian.unwrap();
    assert_eq!(profile.guardian(), Some(guardian));
    assert!(browser::is_running(&guardian));
    assert_eq!(lease.browser, None);
    let lease_mode = fs::metadata(path.join(LEASE)).unwrap().permissions().mode() & 0o777;
    assert_eq!(lease_mode, 0o600);
    assert!(!path.join(LEASE_TEMPORARY).exists());
    profile.close().unwrap();
    assert!(!path.exists());
    assert!(
        !browser::is_running(&guardian),
        "a released guardian keeps running"
    );
}

#[test]
fn leases_reject_untrusted_contents() {
    let root = profile_root();
    let valid = Lease::for_current_process().unwrap();
    let profile = leased(root.path(), "checked", &valid);
    assert_eq!(Lease::read(&profile).unwrap(), valid);
    let json = serde_json::to_value(&valid).unwrap();
    let variants = [
        ("version", serde_json::json!(2)),
        ("boot_id", serde_json::json!("../../etc")),
        ("boot_id", serde_json::json!("")),
        ("pid_namespace", serde_json::json!("x".repeat(65))),
        ("owner", serde_json::json!({"pid": 0, "start_time": 1})),
        (
            "owner",
            serde_json::json!({"pid": 2_147_483_648_u64, "start_time": 1}),
        ),
        (
            "browser",
            serde_json::json!({"pid": 1, "start_time": 1, "extra": 1}),
        ),
        ("unknown", serde_json::json!(true)),
    ];
    for (field, value) in variants {
        let mut changed = json.clone();
        changed[field] = value;
        fs::write(profile.join(LEASE), changed.to_string()).unwrap();
        assert!(
            Lease::read(&profile).is_err(),
            "accepted {field}: {changed}"
        );
    }
    fs::write(profile.join(LEASE), " ".repeat(5000)).unwrap();
    assert!(
        Lease::read(&profile).is_err(),
        "accepted an oversized lease"
    );
    let elsewhere = leased(root.path(), "elsewhere", &valid);
    fs::remove_file(profile.join(LEASE)).unwrap();
    symlink(elsewhere.join(LEASE), profile.join(LEASE)).unwrap();
    assert!(Lease::read(&profile).is_err(), "followed a symlinked lease");
    // An interrupted write leaves a temporary file; the next write replaces it.
    fs::remove_file(profile.join(LEASE)).unwrap();
    fs::write(profile.join(LEASE_TEMPORARY), "partial").unwrap();
    valid.write(&profile).unwrap();
    assert_eq!(Lease::read(&profile).unwrap(), valid);
    assert!(!profile.join(LEASE_TEMPORARY).exists());
}

#[test]
fn profile_removal_never_follows_symlinks() {
    let root = profile_root();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("keep"), "user data").unwrap();
    let profile = leased(root.path(), "links", &Lease::for_current_process().unwrap());
    symlink(outside.path(), profile.join("Default/linked-dir")).unwrap();
    symlink(outside.path().join("keep"), profile.join("linked-file")).unwrap();
    remove_profile(&profile).unwrap();
    assert!(!profile.exists());
    assert_eq!(
        fs::read_to_string(outside.path().join("keep")).unwrap(),
        "user data"
    );
}

#[test]
fn stale_profiles_are_recovered_conservatively() {
    let root = profile_root();
    let root = root.path();
    let current = Lease::for_current_process().unwrap();
    let dead = exited();
    let dead_owner = with(&current, |lease| lease.owner = dead);
    let mut zombie_child = Command::new("true").spawn().unwrap();
    let zombie = browser::identity(zombie_child.id()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !browser::procfs::read(zombie.pid).is_some_and(|process| process.zombie) {
        assert!(Instant::now() < deadline, "no zombie");
        thread::sleep(Duration::from_millis(5));
    }

    let removed = [
        leased(root, "dead-owner", &dead_owner),
        leased(root, "zombie-owner", &with(&current, |l| l.owner = zombie)),
        leased(
            root,
            "reused-owner-pid",
            &with(&current, |l| l.owner.start_time += 1),
        ),
        leased(
            root,
            "dead-guardian",
            &with(&dead_owner, |l| l.guardian = Some(dead)),
        ),
        leased(
            root,
            "previous-boot",
            &with(&current, |l| {
                l.boot_id = "00000000-0000-0000-0000-000000000000".into()
            }),
        ),
    ];
    let kept = [
        leased(root, "live-owner", &current),
        leased(
            root,
            "live-guardian",
            &with(&dead_owner, |l| l.guardian = Some(current.owner)),
        ),
        leased(
            root,
            "other-namespace",
            &with(&dead_owner, |l| l.pid_namespace = "pid:[1]".into()),
        ),
    ];
    let unleased = profile_dir(root, "unleased");
    let malformed = profile_dir(root, "malformed");
    fs::write(malformed.join(LEASE), "{").unwrap();
    let future = profile_dir(root, "future");
    let mut version = serde_json::to_value(&dead_owner).unwrap();
    version["version"] = serde_json::json!(LEASE_VERSION + 1);
    fs::write(future.join(LEASE), version.to_string()).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let stale_elsewhere = leased(outside.path(), "elsewhere", &dead_owner);
    let symlinked_lease = profile_dir(root, "symlinked-lease");
    symlink(stale_elsewhere.join(LEASE), symlinked_lease.join(LEASE)).unwrap();
    let symlinked_profile = root.join(format!("{PROFILE_PREFIX}symlinked"));
    symlink(&stale_elsewhere, &symlinked_profile).unwrap();
    let not_a_directory = root.join(format!("{PROFILE_PREFIX}file"));
    fs::write(&not_a_directory, "not a profile").unwrap();
    let unrelated = root.join("unrelated");
    fs::create_dir(&unrelated).unwrap();

    // A stale profile some process still names is kept, and that process is not ours.
    let in_use = leased(root, "in-use", &dead_owner);
    let user = holding(&in_use);
    let user_identity = browser::identity(user.id()).unwrap();
    // A stale profile holding links to data outside it.
    let data = tempfile::tempdir().unwrap();
    fs::write(data.path().join("keep"), "user data").unwrap();
    let linked = leased(root, "linked", &dead_owner);
    symlink(data.path(), linked.join("Default/linked")).unwrap();
    // The orphaned browser of a dead owner still runs on its profile.
    let orphaned = profile_dir(root, "orphaned");
    let orphan = holding(user_data_dir(&orphaned));
    let orphan_identity = browser::identity(orphan.id()).unwrap();
    with(&dead_owner, |l| l.browser = Some(orphan_identity))
        .write(&orphaned)
        .unwrap();
    // The recorded browser PID now belongs to a process without that profile.
    let reused = profile_dir(root, "reused-browser-pid");
    let mut stranger = Command::new("sleep").arg("60").spawn().unwrap();
    let stranger_identity = browser::identity(stranger.id()).unwrap();
    with(&dead_owner, |l| l.browser = Some(stranger_identity))
        .write(&reused)
        .unwrap();

    recover_stale(root);

    for profile in removed.iter().chain([&linked, &orphaned, &reused]) {
        assert!(!profile.exists(), "{} was kept", profile.display());
    }
    for profile in kept
        .iter()
        .chain([&unleased, &malformed, &future, &symlinked_lease, &in_use])
    {
        assert!(
            profile.join("Default/Cookies").is_file(),
            "{} was touched",
            profile.display()
        );
    }
    for lease in [&kept[0], &kept[1], &kept[2], &in_use] {
        assert!(
            Lease::read(lease).is_ok(),
            "{} lost its lease",
            lease.display()
        );
    }
    assert!(
        fs::symlink_metadata(&symlinked_profile)
            .unwrap()
            .is_symlink()
    );
    assert!(stale_elsewhere.join("Default/Cookies").is_file());
    assert!(Lease::read(&stale_elsewhere).is_ok());
    assert!(not_a_directory.is_file() && unrelated.is_dir());
    assert_eq!(
        fs::read_to_string(data.path().join("keep")).unwrap(),
        "user data"
    );
    assert!(
        browser::is_running(&user_identity),
        "a process naming a profile was signaled"
    );
    assert!(
        !browser::is_running(&orphan_identity),
        "the orphaned browser still runs"
    );
    assert!(
        browser::is_running(&stranger_identity),
        "a reused browser PID was signaled"
    );

    stop(user);
    stop(orphan);
    let _ = stranger.kill();
    let _ = stranger.wait();
    zombie_child.wait().unwrap();
}
