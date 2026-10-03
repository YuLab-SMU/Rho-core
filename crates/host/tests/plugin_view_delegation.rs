#[path = "fixtures/view_authority.rs"]
mod authority;
#[path = "fixtures/plugins.rs"]
mod fixture;
use rho_contract::*;
use rho_host::{NextHost, OperationError};
use rho_plugin_protocol::{PluginArchive, PluginViewConnection};
use rho_plugins::{PluginRepository, backend_target, repository_path, snapshot_directory};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn package(path: &Path) -> PluginArchive {
    fixture::package(path, "view-delegation", false);
    for file in ["backend.py", "dist/backend"] {
        fs::write(path.join(file), include_str!("fixtures/view_delegation.py")).unwrap();
    }
    let file = path.join("plugin.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let caps = [
        ("fixture.read", "query"),
        ("fixture.origin", "query"),
        ("fixture.origin_run", "operation"),
        ("fixture.control", "control"),
        ("fixture.run", "operation"),
        ("fixture.prepare", "query"),
        ("fixture.prepared", "operation"),
    ];
    manifest["requires"] = Value::Array(
        caps.iter()
            .map(|(id, _)| *id)
            .chain(["views.caller", "views.presence"])
            .map(|id| json!({"capability":{"id":id,"version":1},"scopes":[if matches!(id, "fixture.origin" | "fixture.origin_run" | "views.caller" | "views.presence") { "plugins.read" } else { "resources.read" }]}))
            .collect(),
    );
    manifest["capabilities"] = Value::Array(caps.iter().map(|(id, kind)| {
        let mut cap = json!({"capability":{"id":id,"version":1},"kind":kind,"title":id,"description":"Independent view delegation fixture",
            "input_schema":{"type":"object"},"examples":[{}],"output_schema":{"type":"object"},"recovery_schema":true,
            "required_scopes":[if matches!(*id, "fixture.origin" | "fixture.origin_run") { "plugins.read" } else { "resources.read" }],"effects":if *kind == "operation" {json!(["fixture.write"])} else {json!([])},"cancellation":"unsupported"});
        if *id == "fixture.prepared" { cap["preflight"] = json!({"id":"fixture.prepare","version":1}); }
        cap
    }).collect());
    manifest["source"]["files"]
        .as_array_mut()
        .unwrap()
        .push(json!("index.html"));
    manifest["views"] = json!([{"id":"view","title":"Delegation","entrypoint":"dist/index.html","configuration_schema":{"type":"object"},"resource_kinds":[]}]);
    let mut other = manifest["views"][0].clone();
    other["id"] = json!("other");
    manifest["views"].as_array_mut().unwrap().push(other);
    fs::write(
        path.join("index.html"),
        "<!doctype html><p>Delegation fixture</p>",
    )
    .unwrap();
    fs::copy(path.join("index.html"), path.join("dist/index.html")).unwrap();
    fs::write(file, serde_json::to_vec(&manifest).unwrap()).unwrap();
    snapshot_directory(path, None, &backend_target()).unwrap()
}
fn invocation(id: &str, cap: &str, arguments: Value) -> Invocation {
    Invocation {
        client_request_id: id.into(),
        capability: CapabilityRef::new(cap, 1).unwrap(),
        arguments,
        preconditions: vec![],
    }
}
async fn query(host: &NextHost, context: &CallContext, cap: &str, arguments: Value) -> Value {
    host.query_snapshot(
        context,
        QueryRequest {
            capability: CapabilityRef::new(cap, 1).unwrap(),
            arguments,
        },
    )
    .await
    .unwrap()
    .data
    .unwrap()
}
async fn invoke(
    host: &NextHost,
    context: &CallContext,
    id: &str,
    cap: &str,
    arguments: Value,
) -> Value {
    let result = host
        .invoke(context, invocation(id, cap, arguments))
        .await
        .unwrap();
    assert_eq!(result.status, OperationStatus::Succeeded, "{result:?}");
    result.output.unwrap()
}
struct Channel {
    connection: PluginViewConnection,
    sequence: u32,
}
impl Channel {
    async fn send(
        &mut self,
        host: &NextHost,
        context: &CallContext,
        body: Value,
    ) -> Result<Value, OperationError> {
        self.sequence += 1;
        let message = serde_json::from_value(json!({"protocol_version":1,"connection":self.connection.connection,"view":self.connection.view.view,
            "sequence":self.sequence,"request":format!("message-{}",self.sequence),"body":body})).unwrap();
        host.dispatch_plugin_view(
            context,
            self.connection.view.window.as_str(),
            &self.connection.call_token,
            message,
        )
        .await
    }
    async fn call(
        &mut self,
        host: &NextHost,
        context: &CallContext,
        kind: &str,
        cap: &str,
        arguments: Value,
    ) -> Result<Value, OperationError> {
        let mut body =
            json!({"type":kind,"capability":{"id":cap,"version":1},"arguments":arguments});
        if kind == "invoke" {
            body["request_id"] = json!(format!("invoke-{}", self.sequence + 1));
            body["preconditions"] = json!([]);
        }
        self.send(host, context, body).await
    }
}
async fn settled(host: &NextHost, context: &CallContext, value: Value) -> OperationRecord {
    let accepted: OperationRecord = serde_json::from_value(value).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let record = host
                .get_operation(context, &accepted.operation.operation_id)
                .await
                .unwrap()
                .unwrap();
            if record.status.is_terminal() {
                return record;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn caller_view_observation_is_native_credential_free_and_survives_backend_hops() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    fs::create_dir(&root).unwrap();
    let db = temp.path().join("state.sqlite");
    let archive = package(&temp.path().join("package"));
    PluginRepository::open(&repository_path(&db))
        .unwrap()
        .import(&archive)
        .unwrap();
    let host = NextHost::open_plugin_workspace(&db, &root).await.unwrap();
    let context = NextHost::local_context();
    assert_eq!(
        query(&host, &context, "views.caller", json!({})).await,
        json!({"view":null})
    );
    let instance = invoke(&host,&context,"activate-origin","plugins.activate",json!({"revision":archive.revision.id,"artifact":archive.artifacts[0].id,"target":backend_target(),"alias":"origin","configuration":{}})).await["instance"]["identity"].clone();
    let binding = query(
        &host,
        &context,
        "plugins.resolve",
        json!({"instance":instance,"capability":{"id":"fixture.origin","version":1}}),
    )
    .await;
    let view = invoke(
        &host,
        &context,
        "open-origin",
        "views.open",
        json!({"instance":instance,"contribution":"view","window":"window-a","configuration":{}}),
    )
    .await;
    let connection: PluginViewConnection = serde_json::from_value(
        query(
            &host,
            &context,
            "views.connection",
            json!({"view":view["view"]}),
        )
        .await,
    )
    .unwrap();
    let expected = json!({"view":{"view":connection.view.view,"window":"window-a","connection":connection.connection}});
    let captured: rho_plugin_protocol::PluginViewCaller =
        serde_json::from_value(expected.clone()).unwrap();
    let mut channel = Channel {
        connection,
        sequence: 0,
    };
    let direct = channel
        .call(&host, &context, "query", "views.caller", json!({}))
        .await
        .unwrap();
    assert_eq!(direct["data"], expected);
    let observed = serde_json::to_string(&direct).unwrap();
    assert!(!observed.contains(&channel.connection.call_token));
    assert!(!observed.contains(&channel.connection.asset_token));
    assert!(
        channel
            .call(
                &host,
                &context,
                "query",
                "views.caller",
                json!({"view":"another","window":"forged"})
            )
            .await
            .is_err()
    );
    let request = |arguments: Value| json!({"binding":binding,"arguments":arguments});
    let inner = request(
        json!({"capability":{"id":"views.caller","version":1},"host_arguments":{},"view_scope":{"window":"forged","origin":{"view":"forged"}}}),
    );
    let once = channel
        .call(&host, &context, "query", "fixture.origin", inner.clone())
        .await
        .unwrap();
    assert_eq!(
        once["data"]["delegated"]["data"]["result"]["data"],
        expected
    );
    let twice = channel
        .call(
            &host,
            &context,
            "query",
            "fixture.origin",
            request(
                json!({"capability":{"id":"fixture.origin","version":1},"host_arguments":inner}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        twice["data"]["delegated"]["data"]["result"]["data"]["delegated"]["data"]["result"]["data"],
        expected
    );
    // A native view scope remains a restriction across principal/connection loss;
    // it cannot be replaced by a caller-selected fallback identity.
    let mut original = context.clone();
    original.view_scope = Some(ViewCallScope {
        window: channel.connection.view.window.clone(),
        origin: captured.view,
    });
    assert_eq!(
        query(&host, &original, "views.caller", json!({})).await,
        expected
    );
    let mut wrong_connection = original.clone();
    wrong_connection
        .view_scope
        .as_mut()
        .unwrap()
        .origin
        .as_mut()
        .unwrap()
        .connection = rho_plugin_protocol::ConnectionId::new("stale-connection").unwrap();
    let mut other_principal = original.clone();
    other_principal.caller.id = "other-principal".into();
    let mut wrong_window = original.clone();
    wrong_window.view_scope.as_mut().unwrap().window =
        rho_plugin_protocol::WindowId::new("another-window").unwrap();
    for denied in [wrong_connection, other_principal, wrong_window] {
        assert!(
            host.query_snapshot(
                &denied,
                QueryRequest {
                    capability: CapabilityRef::new("views.caller", 1).unwrap(),
                    arguments: json!({})
                }
            )
            .await
            .is_err()
        );
    }
    let mut missing_origin = original.clone();
    missing_origin.view_scope.as_mut().unwrap().origin = None;
    assert!(
        host.query_snapshot(
            &missing_origin,
            QueryRequest {
                capability: CapabilityRef::new("views.caller", 1).unwrap(),
                arguments: json!({})
            }
        )
        .await
        .is_err()
    );
    let mut weak = context.clone();
    weak.scopes.remove("plugins.read");
    assert!(
        channel
            .call(&host, &weak, "query", "views.caller", json!({}))
            .await
            .is_err()
    );
    let running_binding = query(
        &host,
        &context,
        "plugins.resolve",
        json!({"instance":instance,"capability":{"id":"fixture.origin_run","version":1}}),
    )
    .await;
    let accepted = channel.call(&host,&context,"invoke","fixture.origin_run",json!({"binding":running_binding,"arguments":{"action":"hold","capability":{"id":"views.caller","version":1},"host_arguments":{}}})).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if query(
                &host,
                &context,
                "fixture.origin",
                request(json!({"action":"held"})),
            )
            .await["held"]
                == 1
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    invoke(
        &host,
        &context,
        "close-origin",
        "views.close",
        json!({"view":view["view"],"mode":{"kind":"disconnect","connection":channel.connection.connection}}),
    )
    .await;
    assert!(
        host.query_snapshot(
            &original,
            QueryRequest {
                capability: CapabilityRef::new("views.caller", 1).unwrap(),
                arguments: json!({})
            }
        )
        .await
        .is_err()
    );
    query(
        &host,
        &context,
        "fixture.origin",
        request(json!({"action":"release_held"})),
    )
    .await;
    let settled = settled(&host, &context, accepted).await;
    assert_eq!(settled.status, OperationStatus::Succeeded);
    let failure = settled.output.unwrap();
    assert_eq!(failure["delegated"]["type"], "error");
    assert!(
        failure["delegated"]["data"]["message"]
            .as_str()
            .unwrap()
            .contains("original calling view")
    );
    // Journal completion precedes native settlement acknowledgement. Wait for
    // the original execution lease rather than treating it as already released.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let owner = query(
                &host,
                &context,
                "plugins.instance",
                json!({"instance":instance}),
            )
            .await;
            if owner["retained_calls"] == 0 && owner["pending_messages"] == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    invoke(
        &host,
        &context,
        "release-origin",
        "plugins.release",
        json!({"instance":instance}),
    )
    .await;
    host.drain().await;
}

#[path = "fixtures/view_presence.rs"]
mod view_presence;
