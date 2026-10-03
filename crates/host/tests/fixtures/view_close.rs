use super::*;
use rho_plugin_protocol::{PluginViewConnection, PluginViewMessage};

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
        let message: PluginViewMessage = serde_json::from_value(json!({"protocol_version":1,
            "connection":self.connection.connection,"view":self.connection.view.view,
            "sequence":self.sequence,"request":format!("message-{}",self.sequence),"body":body}))
        .unwrap();
        host.dispatch_plugin_view(
            context,
            self.connection.view.window.as_str(),
            &self.connection.call_token,
            message,
        )
        .await
    }
    async fn requested(
        &mut self,
        host: &NextHost,
        context: &CallContext,
        renderer: &str,
    ) -> String {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let state = self
                    .send(
                        host,
                        context,
                        json!({"type":"observe_lifecycle","renderer":renderer}),
                    )
                    .await
                    .unwrap();
                if state["close"]["phase"] == "requested" {
                    break state["close"]["operation"].as_str().unwrap().to_owned();
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }
}
async fn accepted(
    host: &NextHost,
    context: &CallContext,
    id: &str,
    view: &Value,
) -> OperationRecord {
    serde_json::from_value(
        host.dispatch(
            context,
            HostRequest::Invoke(InvokeRequest {
                invocation: invocation(id, "views.close", json!({"view":view})),
                return_after_acceptance: Some(true),
            }),
        )
        .await
        .unwrap(),
    )
    .unwrap()
}
async fn settled(
    host: &NextHost,
    context: &CallContext,
    record: &OperationRecord,
) -> OperationRecord {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let current = host
                .get_operation(context, &record.operation.operation_id)
                .await
                .unwrap()
                .unwrap();
            if current.status.is_terminal() {
                break current;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn close_requires_owner_preparation_and_preserves_authority_and_atomic_release() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir(&project).unwrap();
    let db = temp.path().join("state.sqlite");
    let package = temp.path().join("ui");
    ui_package(&package);
    let file = package.join("plugin.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    manifest["requires"].as_array_mut().unwrap().push(
        json!({"capability":{"id":"plugins.archive_export","version":1},"scopes":["plugins.read"]}),
    );
    fs::write(file, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let archive = rho_plugins::snapshot_directory(&package, None, "ui-web").unwrap();
    let mut repository = PluginRepository::open(&repository_path(&db)).unwrap();
    repository.import(&archive).unwrap();
    let host = NextHost::open_plugin_workspace(&db, &project)
        .await
        .unwrap();
    let context = NextHost::local_context();
    let instance = observation(
        &run(
            &host,
            &context,
            "activate",
            "plugins.activate",
            json!({"revision":archive.revision.id,
        "artifact":archive.artifacts[0].id,"target":"ui-web","alias":"close","configuration":{}}),
        )
        .await,
    );
    let opened = run(
        &host,
        &context,
        "open",
        "views.open",
        json!({"instance":instance.instance.identity,
        "contribution":"view","window":"close-window","configuration":{}}),
    )
    .await
    .output
    .unwrap();
    let view = opened["view"].clone();
    let connection = serde_json::from_value(
        query(&host, &context, "views.connection", json!({"view":view})).await,
    )
    .unwrap();
    let mut channel = Channel {
        connection,
        sequence: 0,
    };
    assert!(
        host.invoke(
            &context,
            invocation("missing-handler", "views.close", json!({"view":view}))
        )
        .await
        .is_err()
    );
    assert!(
        host.invoke(
            &context,
            invocation(
                "stale-disconnect",
                "views.close",
                json!({"view":view,
        "mode":{"kind":"disconnect","connection":"another-connection"}})
            )
        )
        .await
        .is_err()
    );
    assert!(
        host.invoke(
            &context,
            invocation(
                "old-mode",
                "views.close",
                json!({"view":view,
        "mode":{"kind":"retain_acknowledged","expected_version":0}})
            )
        )
        .await
        .is_err()
    );
    for renderer in ["first", "second"] {
        channel
            .send(
                &host,
                &context,
                json!({"type":"register_close_handler","renderer":renderer}),
            )
            .await
            .unwrap();
    }
    let mut revoked = context.clone();
    revoked.scopes.remove("plugins.run");
    assert!(
        channel
            .send(
                &host,
                &revoked,
                json!({"type":"observe_lifecycle","renderer":"first"})
            )
            .await
            .is_err()
    );
    let first = accepted(&host, &context, "refused-close", &view).await;
    let operation = channel.requested(&host, &context, "first").await;
    assert_eq!(operation, first.operation.operation_id.as_str());
    assert_eq!(
        accepted(&host, &context, "refused-close", &view)
            .await
            .operation
            .operation_id,
        first.operation.operation_id
    );
    for bad in [
        json!({"type":"register_close_handler","renderer":"late"}),
        json!({"type":"prepare_close","renderer":"unknown","operation":operation}),
        json!({"type":"prepare_close","renderer":"first","operation":"another-operation"}),
        json!({"type":"begin_text_copy"}),
        json!({"type":"cancel","operation_id":"accepted-work"}),
    ] {
        assert!(channel.send(&host, &context, bad).await.is_err());
    }
    // Preparing owners can use any declared Operation port. No content-specific
    // exception, extra scope, or assumption that the operation saved a buffer.
    let mut no_read = context.clone();
    no_read.scopes.remove("plugins.read");
    let export = json!({"type":"invoke","request_id":"prepare-export","capability":{"id":"plugins.archive_export","version":1},
        "arguments":{"revision":archive.revision.id,"artifacts":[]},"preconditions":[]});
    assert!(channel.send(&host, &no_read, export.clone()).await.is_err());
    let original: OperationRecord =
        serde_json::from_value(channel.send(&host, &context, export).await.unwrap()).unwrap();
    assert_eq!(
        settled(&host, &context, &original).await.status,
        OperationStatus::Succeeded
    );
    assert_eq!(original.operation.caller.id, view.as_str().unwrap());
    channel
        .send(
            &host,
            &context,
            json!({"type":"prepare_close","renderer":"first","operation":operation}),
        )
        .await
        .unwrap();
    assert!(
        !host
            .get_operation(&context, &first.operation.operation_id)
            .await
            .unwrap()
            .unwrap()
            .status
            .is_terminal()
    );
    channel.send(&host, &context, json!({"type":"refuse_close","renderer":"second","operation":operation,"reason":"Owner preparation failed"})).await.unwrap();
    let failed = settled(&host, &context, &first).await;
    assert_eq!(failed.status, OperationStatus::Failed);
    assert!(
        !query(&host, &context, "views.inspect", json!({"view":view})).await["closed"]
            .as_bool()
            .unwrap()
    );
    // Only the unanswered preparation uses virtual time. Native work and the
    // other close attempts keep their ordinary clock and acknowledgement path.
    tokio::time::pause();
    let timed = host
        .invoke(
            &context,
            invocation("unanswered", "views.close", json!({"view":view})),
        )
        .await
        .unwrap();
    tokio::time::resume();
    assert_eq!(timed.status, OperationStatus::Failed);
    assert!(
        !query(&host, &context, "views.inspect", json!({"view":view})).await["closed"]
            .as_bool()
            .unwrap(),
        "missing preparation acknowledgement must keep the view open"
    );
    // Ending a participant during preparation refuses the close even if it
    // already acknowledged. It cannot shrink the set into apparent success.
    let ending = accepted(&host, &context, "participant-ended", &view).await;
    let operation = channel.requested(&host, &context, "first").await;
    channel
        .send(
            &host,
            &context,
            json!({"type":"prepare_close","renderer":"second","operation":operation}),
        )
        .await
        .unwrap();
    let release = json!({"view":view,"connection":channel.connection.connection,"window":"close-window",
        "renderer":"second","call_token":channel.connection.call_token});
    let control = |arguments| {
        HostRequest::Control(ControlRequest {
            capability: CapabilityRef::new("views.release_renderer", 1).unwrap(),
            arguments,
        })
    };
    let mut wrong = release.clone();
    wrong["call_token"] = json!("wrong");
    assert!(host.dispatch(&context, control(wrong)).await.is_err());
    let mut foreign = context.clone();
    foreign.caller.id = "foreign".into();
    assert!(
        host.dispatch(&foreign, control(release.clone()))
            .await
            .is_err()
    );
    assert_eq!(
        host.dispatch(&context, control(release.clone()))
            .await
            .unwrap()["released"],
        true
    );
    assert_eq!(
        host.dispatch(&context, control(release)).await.unwrap()["released"],
        false
    );
    assert_eq!(
        settled(&host, &context, &ending).await.status,
        OperationStatus::Failed
    );
    channel
        .send(
            &host,
            &context,
            json!({"type":"register_close_handler","renderer":"replacement"}),
        )
        .await
        .unwrap();
    let catalog =
        rusqlite::Connection::open(repository_path(&db).join("catalog-v1.sqlite3")).unwrap();
    catalog.execute_batch("CREATE TRIGGER reject_close_write BEFORE UPDATE ON plugin_views BEGIN SELECT RAISE(ABORT, 'close write failure'); END;").unwrap();
    for (request, succeeds) in [("write-fails", false), ("succeeds", true)] {
        let close = accepted(&host, &context, request, &view).await;
        let operation = channel.requested(&host, &context, "first").await;
        for renderer in ["first", "replacement"] {
            channel
                .send(
                    &host,
                    &context,
                    json!({"type":"prepare_close","renderer":renderer,"operation":operation}),
                )
                .await
                .unwrap();
        }
        let result = settled(&host, &context, &close).await;
        assert_eq!(
            result.status == OperationStatus::Succeeded,
            succeeds,
            "{:?}",
            result.error
        );
        let record = query(&host, &context, "views.inspect", json!({"view":view})).await;
        assert_eq!(record["closed"], succeeds);
        let protected = repository
            .references(&archive.revision.id)
            .unwrap()
            .iter()
            .any(|reference| reference.contains(view.as_str().unwrap()));
        assert_eq!(protected, !succeeds);
        if !succeeds {
            catalog
                .execute_batch("DROP TRIGGER reject_close_write;")
                .unwrap();
        }
    }
    assert!(
        channel
            .send(
                &host,
                &context,
                json!({"type":"observe_lifecycle","renderer":"first"})
            )
            .await
            .is_err()
    );
    assert_eq!(
        query(
            &host,
            &context,
            "plugins.instance",
            json!({"instance":instance.instance.identity})
        )
        .await["instance"]["state"],
        "active"
    );
    host.drain().await;
}
