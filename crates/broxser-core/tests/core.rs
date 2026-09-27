use broxser_core::{Device, Error, Session, SyncAction, SyncEvent, SyncRouter, Workspace};

fn event(device: &str, session: &str, sequence: u64, action: SyncAction) -> SyncEvent {
    SyncEvent {
        origin_device: device.into(),
        origin_session: session.into(),
        sequence,
        action,
        replayed: false,
    }
}

#[test]
fn demo_and_example_are_valid_and_equivalent() {
    let demo = Workspace::demo();
    demo.validate().unwrap();
    let example = Workspace::load(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/workspace.json"
    ))
    .unwrap();
    assert_eq!(
        serde_json::to_value(demo).unwrap(),
        serde_json::to_value(example).unwrap()
    );
}

#[test]
fn load_refuses_future_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.json");
    let mut value = serde_json::to_value(Workspace::demo()).unwrap();
    value["schema_version"] = serde_json::json!(2);
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(matches!(
        Workspace::load(&path),
        Err(Error::UnsupportedSchema(2))
    ));
}

#[test]
fn rejects_invalid_ids_duplicates_and_session_references() {
    let mut workspace = Workspace::demo();
    workspace.devices[0].id = "../escape".into();
    assert!(workspace.validate().is_err());
    workspace.devices[0].id = "phone".into();
    workspace.devices[1].id = "phone".into();
    assert!(workspace.validate().is_err());
    workspace.devices[1].id = "tablet".into();
    workspace.sessions.push(Session {
        id: "guest".into(),
        name: "Again".into(),
    });
    assert!(workspace.validate().is_err());
    workspace.sessions.pop();
    workspace.devices[0].session = "missing".into();
    assert!(workspace.validate().is_err());
}

#[test]
fn rejects_unsafe_urls() {
    for url in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https://user:pass@example.com",
        "http://",
        "https://example.com\nInjected: x",
    ] {
        let mut workspace = Workspace::demo();
        workspace.url = url.into();
        assert!(workspace.validate().is_err(), "accepted {url:?}");
        assert!(broxser_core::validate_url(url).is_err(), "accepted {url:?}");
    }
    broxser_core::validate_url("http://localhost:3000/path?q=1").unwrap();
}

#[test]
fn enforces_viewport_scale_and_resource_budget() {
    let mut workspace = Workspace::demo();
    workspace.devices[0].width = 199;
    assert!(workspace.validate().is_err());
    workspace.devices[0].width = 390;
    for scale in [f64::NAN, f64::INFINITY, 0.49, 4.01] {
        workspace.devices[0].device_scale_factor = scale;
        assert!(workspace.validate().is_err(), "accepted scale {scale}");
    }
    workspace.devices[0].device_scale_factor = 1.0;
    workspace.devices = (0..2)
        .map(|n| Device {
            id: format!("wide{n}"),
            name: "Wide".into(),
            width: 4096,
            height: 4096,
            device_scale_factor: 1.0,
            mobile: false,
            touch: false,
            session: "guest".into(),
        })
        .collect();
    assert!(
        workspace.validate().is_err(),
        "oversized total physical pixels accepted"
    );
    workspace.devices = (0..9)
        .map(|n| Device {
            id: format!("small{n}"),
            name: "Small".into(),
            width: 200,
            height: 200,
            device_scale_factor: 1.0,
            mobile: false,
            touch: false,
            session: "guest".into(),
        })
        .collect();
    assert!(workspace.validate().is_err(), "too many devices accepted");
}

#[test]
fn save_validates_before_replacing_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.json");
    let workspace = Workspace::demo();
    workspace.save(&path).unwrap();
    assert_eq!(
        serde_json::to_value(Workspace::load(&path).unwrap()).unwrap(),
        serde_json::to_value(&workspace).unwrap()
    );
    let original = std::fs::read(&path).unwrap();
    let mut invalid = workspace;
    invalid.devices[0].id = "../bad".into();
    assert!(invalid.save(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn sync_requires_opt_in_and_stays_within_session() {
    let mut router = SyncRouter::new(&Workspace::demo()).unwrap();
    let navigation = event(
        "phone",
        "guest",
        1,
        SyncAction::Navigate {
            url: "http://127.0.0.1:4173/page".into(),
        },
    );
    assert!(router.route(&navigation).unwrap().is_empty());
    assert!(router.enable_route("phone", "desktop").is_err());
    assert!(router.enable_route("phone", "phone").is_err());
    router.enable_route("phone", "tablet").unwrap();
    let deliveries = router
        .route(&event(
            "phone",
            "guest",
            2,
            SyncAction::Scroll { x: 0, y: 42 },
        ))
        .unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].destination_device, "tablet");
    assert!(deliveries[0].event.replayed);
    assert!(router.route(&deliveries[0].event).unwrap().is_empty());
    assert!(matches!(
        router.route(&event(
            "phone",
            "guest",
            2,
            SyncAction::Scroll { x: 0, y: 42 }
        )),
        Err(Error::StaleSequence { .. })
    ));
    assert!(
        router
            .route(&event(
                "phone",
                "admin",
                3,
                SyncAction::Scroll { x: 0, y: 42 }
            ))
            .is_err()
    );
}

#[test]
fn pointer_and_keys_need_separate_opt_in() {
    let mut router = SyncRouter::new(&Workspace::demo()).unwrap();
    router.enable_route("phone", "tablet").unwrap();
    assert!(
        router
            .route(&event(
                "phone",
                "guest",
                1,
                SyncAction::Pointer { x: 10, y: 20 }
            ))
            .unwrap()
            .is_empty()
    );
    assert!(
        router
            .route(&event(
                "phone",
                "guest",
                2,
                SyncAction::Key {
                    key: "Enter".into()
                }
            ))
            .unwrap()
            .is_empty()
    );
    router.set_pointer_enabled(true);
    router.set_key_enabled(true);
    assert_eq!(
        router
            .route(&event(
                "phone",
                "guest",
                3,
                SyncAction::Pointer { x: 10, y: 20 }
            ))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        router
            .route(&event(
                "phone",
                "guest",
                4,
                SyncAction::Key {
                    key: "Enter".into()
                }
            ))
            .unwrap()
            .len(),
        1
    );
    assert!(
        router
            .route(&event(
                "phone",
                "guest",
                5,
                SyncAction::Key { key: "\n".into() }
            ))
            .is_err()
    );
}

#[test]
fn presets_add_named_unique_devices_and_roll_back_over_budget() {
    use broxser_core::{MAX_DEVICES, PRESETS};
    for preset in &PRESETS {
        let mut workspace = Workspace::demo();
        workspace.devices.clear();
        workspace.devices.push(Workspace::demo().devices[0].clone());
        workspace.add_device_from_preset(preset, "guest").unwrap();
        workspace.validate().unwrap();
    }
    let mut workspace = Workspace::demo();
    let phone = PRESETS
        .iter()
        .find(|preset| preset.name == "Phone")
        .unwrap();
    // The demo already has a device named Phone with the id `phone`.
    let index = workspace.add_device_from_preset(phone, "admin").unwrap();
    assert_eq!(index, 3);
    assert_eq!(workspace.devices[3].id, "phone-2");
    assert_eq!(workspace.devices[3].name, "Phone 2");
    assert_eq!(workspace.devices[3].session, "admin");
    assert_eq!(workspace.devices[3].device_scale_factor, 2.0);
    let index = workspace.add_device_from_preset(phone, "guest").unwrap();
    assert_eq!(workspace.devices[index].id, "phone-3");
    assert_eq!(workspace.devices[index].name, "Phone 3");
    let large = PRESETS
        .iter()
        .find(|preset| preset.name == "Large desktop")
        .unwrap();
    let index = workspace.add_device_from_preset(large, "guest").unwrap();
    assert_eq!(workspace.devices[index].id, "large-desktop");
    assert!(matches!(
        workspace.add_device_from_preset(phone, "nobody"),
        Err(Error::Invalid(message)) if message.contains("unknown session")
    ));
    // Eight devices at most; the ninth leaves the workspace unchanged.
    while workspace.devices.len() < MAX_DEVICES {
        workspace.add_device_from_preset(large, "guest").unwrap();
    }
    let before = workspace.clone();
    assert!(workspace.add_device_from_preset(large, "guest").is_err());
    assert_eq!(workspace, before);
    // Removal keeps at least one device.
    let mut workspace = Workspace::demo();
    let removed = workspace.remove_device(1).unwrap();
    assert_eq!(removed.id, "tablet");
    assert_eq!(workspace.devices.len(), 2);
    workspace.remove_device(0).unwrap();
    assert!(workspace.remove_device(0).is_err());
    assert_eq!(workspace.devices.len(), 1);
    assert!(workspace.remove_device(5).is_err());
}

#[test]
fn application_state_remembers_workspaces_and_the_window_without_secrets() {
    use broxser_core::{AppState, MAX_RECENT_WORKSPACES, WindowSize};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state").join("state.json");
    // A missing file is the default state; its directory is created on save.
    let mut state = AppState::load(&path).unwrap();
    assert_eq!(state, AppState::default());
    let workspace = dir.path().join("a.json");
    Workspace::demo().save(&workspace).unwrap();
    state.remember_workspace(&workspace);
    state.remember_workspace(&dir.path().join("gone.json"));
    state.remember_workspace(&workspace);
    assert_eq!(
        state.recent_workspaces,
        [workspace.clone(), dir.path().join("gone.json")]
    );
    assert_eq!(state.latest_existing_workspace(), Some(workspace.as_path()));
    state.window = Some(WindowSize {
        width: 1280,
        height: 800,
    });
    state.save(&path).unwrap();
    assert_eq!(AppState::load(&path).unwrap(), state);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("url") && !text.contains("cookie"), "{text}");
    // Bounded, most recent first.
    for n in 0..20 {
        state.remember_workspace(&dir.path().join(format!("{n}.json")));
    }
    assert_eq!(state.recent_workspaces.len(), MAX_RECENT_WORKSPACES);
    assert_eq!(state.recent_workspaces[0], dir.path().join("19.json"));
    // Invalid contents are refused, not repaired silently.
    std::fs::write(&path, r#"{"schema_version": 2}"#).unwrap();
    assert!(matches!(
        AppState::load(&path),
        Err(Error::UnsupportedStateSchema(2))
    ));
    std::fs::write(
        &path,
        r#"{"schema_version": 1, "recent_workspaces": ["relative.json"]}"#,
    )
    .unwrap();
    assert!(AppState::load(&path).is_err());
    std::fs::write(
        &path,
        r#"{"schema_version": 1, "window": {"width": 10, "height": 10}}"#,
    )
    .unwrap();
    assert!(AppState::load(&path).is_err());
    std::fs::write(&path, "not json").unwrap();
    assert!(AppState::load(&path).is_err());
}
