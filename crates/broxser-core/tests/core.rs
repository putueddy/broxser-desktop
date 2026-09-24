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
