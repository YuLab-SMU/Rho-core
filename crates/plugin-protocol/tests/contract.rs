use rho_plugin_protocol::*;
use serde_json::json;

fn frame(sequence: u32) -> RpcFrame {
    RpcFrame {
        protocol_version: 1,
        connection: ConnectionId::new("connection-1").unwrap(),
        instance: PluginInstanceId::new("instance-1").unwrap(),
        sequence,
        request: RequestId::new("request-1").unwrap(),
        body: RpcBody::Released,
    }
}

#[test]
fn readiness_extensions_are_optional_bounded_and_do_not_change_manifest_contracts() {
    let mut ready = frame(1);
    ready.body = RpcBody::Ready {
        revision: RevisionId::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        artifact: ArtifactId::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
        features: Default::default(),
    };
    let old = ready.encode().unwrap();
    assert!(!String::from_utf8_lossy(&old).contains("features"));
    assert_eq!(RpcFrame::decode(&old).unwrap(), ready);
    if let RpcBody::Ready { features, .. } = &mut ready.body {
        features.insert(PENDING_CANCELLATION_FEATURE.into());
        features.insert("another-owner.feature_v1".into());
    }
    assert_eq!(RpcFrame::decode(&ready.encode().unwrap()).unwrap(), ready);
    for invalid in [
        vec!["".to_owned()],
        vec!["x".repeat(65)],
        vec!["invalid value".into()],
        (0..17).map(|i| format!("feature-{i}")).collect(),
    ] {
        if let RpcBody::Ready { features, .. } = &mut ready.body {
            *features = invalid.into_iter().collect();
        }
        assert!(ready.encode().is_err());
    }
}

#[test]
fn instance_connection_and_sequence_are_checked_before_acceptance() {
    let mut guard = RpcSessionGuard::new(
        PluginInstanceId::new("instance-1").unwrap(),
        ConnectionId::new("connection-1").unwrap(),
    );
    let mut forged = frame(1);
    forged.instance = PluginInstanceId::new("other-instance").unwrap();
    assert!(guard.accept(&forged.encode().unwrap()).is_err());
    forged = frame(1);
    forged.connection = ConnectionId::new("old-connection").unwrap();
    assert!(guard.accept(&forged.encode().unwrap()).is_err());
    assert!(guard.accept(&frame(2).encode().unwrap()).is_err());
    assert_eq!(guard.accept(&frame(1).encode().unwrap()).unwrap(), frame(1));
    assert!(guard.accept(&frame(1).encode().unwrap()).is_err());
    assert!(guard.accept(&frame(2).encode().unwrap()).is_ok());
    guard.revoke();
    assert!(guard.accept(&frame(3).encode().unwrap()).is_err());
}

#[test]
fn oversized_unknown_and_unsupported_frames_are_rejected() {
    assert!(RpcFrame::decode(&vec![b' '; MAX_CONTROL_BYTES + 1]).is_err());
    let mut unknown = serde_json::to_value(frame(1)).unwrap();
    unknown["principal_override"] = json!("admin");
    assert!(RpcFrame::decode(&serde_json::to_vec(&unknown).unwrap()).is_err());
    let mut old = frame(1);
    old.protocol_version = 0;
    assert!(old.encode().is_err());
    assert!(frame(0).encode().is_err());
}

#[test]
fn control_diagnostics_redact_payloads_but_wire_retains_exact_answers() {
    let secret = "ephemeral-secret-Ω";
    let request: PluginViewRequest = serde_json::from_value(json!({"type":"control",
        "capability":{"id":"example.answer","version":2},"arguments":{"value":secret}}))
    .unwrap();
    assert!(!format!("{request:?}").contains(secret));
    assert_eq!(
        serde_json::to_value(&request).unwrap()["arguments"]["value"],
        secret
    );
    for body in [
        RpcBody::ControlResult {
            data: json!({"value":secret}),
        },
        RpcBody::Error {
            code: "bad".into(),
            message: secret.into(),
            recovery: Some(json!({"answer":secret})),
        },
    ] {
        let mut wire = frame(1);
        wire.body = body;
        assert!(!format!("{wire:?}").contains(secret));
        assert_eq!(RpcFrame::decode(&wire.encode().unwrap()).unwrap(), wire);
    }
}
