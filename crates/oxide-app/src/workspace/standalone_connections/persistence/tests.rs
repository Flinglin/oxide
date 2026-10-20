use super::*;
use oxide_connections::{ConnectionStoreData, SerialProfile, TelnetProfile};
use oxide_remote_desktop_core::RemoteDesktopProtocol;

#[test]
fn saved_sessions_restore_from_current_profiles_and_removed_sessions_stay_removed() {
    let directory = tempfile::tempdir().unwrap();
    let store_path = directory.path().join("connections.json");
    let snapshot_path = directory.path().join("standalone_sessions.json");
    let mut data = ConnectionStoreData {
        serial_profiles: vec![SerialProfile::new("console", "COM5")],
        telnet_profiles: vec![TelnetProfile::new("switch", "switch.test", 23)],
        mosh_profiles: vec![MoshProfile::new(
            "shell",
            "mosh.test",
            22,
            "user",
            SavedAuth::Agent,
        )],
        remote_desktop_profiles: vec![
            RemoteDesktopProfile::new("desktop", RemoteDesktopProtocol::Rdp, "rdp.test", 3389),
            RemoteDesktopProfile::new("screen", RemoteDesktopProtocol::Vnc, "vnc.test", 5900),
        ],
        ..ConnectionStoreData::default()
    };
    fs::write(&store_path, serde_json::to_vec(&data).unwrap()).unwrap();
    let store = ConnectionStore::load_read_only(&store_path).unwrap();
    let mut registry = StandaloneConnectionRegistry::restore(snapshot_path.clone(), &store);
    let profiles = [
        (
            StandaloneConnectionKind::Serial,
            data.serial_profiles[0].id.clone(),
        ),
        (
            StandaloneConnectionKind::Telnet,
            data.telnet_profiles[0].id.clone(),
        ),
        (
            StandaloneConnectionKind::Mosh,
            data.mosh_profiles[0].id.clone(),
        ),
        (
            StandaloneConnectionKind::Rdp,
            data.remote_desktop_profiles[0].id.clone(),
        ),
        (
            StandaloneConnectionKind::Vnc,
            data.remote_desktop_profiles[1].id.clone(),
        ),
    ];
    let ids = profiles
        .iter()
        .map(|(kind, profile_id)| {
            let launch = LaunchSnapshot::Saved {
                profile_id: profile_id.clone(),
            }
            .restore(*kind, &store)
            .unwrap();
            registry.insert_pending(*kind, format!("{kind:?}"), launch)
        })
        .collect::<Vec<_>>();
    // A saved profile remains authoritative even when edited after the last active-session write.
    data.serial_profiles[0].baud_rate = 57600;
    fs::write(&store_path, serde_json::to_vec(&data).unwrap()).unwrap();
    let store = ConnectionStore::load_read_only(&store_path).unwrap();
    let mut restored = StandaloneConnectionRegistry::restore(snapshot_path.clone(), &store);
    assert_eq!(
        restored
            .records()
            .iter()
            .map(|record| (&record.id, record.kind))
            .collect::<Vec<_>>(),
        ids.iter()
            .zip(profiles.iter().map(|(kind, _)| *kind))
            .collect::<Vec<_>>()
    );
    for record in restored.records() {
        assert_eq!(record.readiness, ActiveSessionReadiness::Disconnected);
        assert!(record.surface.is_none());
        assert!(!restored.is_connecting_attempt(&record.id));
    }
    let StandaloneConnectionLaunch::SavedSerial { config, .. } = &restored.records()[0].launch
    else {
        panic!("serial record");
    };
    assert_eq!(
        (config.port_path.as_str(), config.baud_rate),
        ("COM5", 57600)
    );

    // Explicit removal is durable without an exit hook; deleting an asset also prevents revival.
    restored.remove(&ids[0]);
    data.telnet_profiles.clear();
    fs::write(&store_path, serde_json::to_vec(&data).unwrap()).unwrap();
    let store = ConnectionStore::load_read_only(&store_path).unwrap();
    let restored = StandaloneConnectionRegistry::restore(snapshot_path, &store);
    assert_eq!(
        restored
            .records()
            .iter()
            .map(|record| &record.id)
            .collect::<Vec<_>>(),
        ids[2..].iter().collect::<Vec<_>>()
    );
}
