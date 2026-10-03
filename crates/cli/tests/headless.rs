//! Headless public flow: an external caller discovers an ordinary plugin, reads
//! its context, invokes one managed Operation and inspects that original record.
//! No browser, window, scientific package, R runtime or built-in Agent is used.
use rho_plugin_protocol::PluginArchive;
use rho_plugins::{PluginRepository, backend_target, repository_path, snapshot_directory};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
};

const DOMAIN_SCOPE: &str = "fixture.notes";

struct Session {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<std::process::ChildStdout>,
    serial: u32,
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Session {
    fn open(db: &Path, project: &Path, grants: &[&str]) -> (Self, Value) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rho"));
        command
            .arg("--database")
            .arg(db)
            .arg("--project")
            .arg(project);
        for scope in grants {
            command.arg("--grant-scope").arg(scope);
        }
        let mut child = command
            .arg("session")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut session = Self {
            child,
            input,
            output,
            serial: 0,
        };
        let ready = session.line();
        assert_eq!(ready["type"], "ready", "{ready}");
        (session, ready)
    }
    fn line(&mut self) -> Value {
        let mut line = String::new();
        assert!(
            self.output.read_line(&mut line).unwrap() > 0,
            "session exited"
        );
        serde_json::from_str(&line).unwrap()
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.serial += 1;
        let id = format!("frame-{}", self.serial);
        let input = self.input.as_mut().expect("open session input");
        writeln!(
            input,
            "{}",
            json!({"id":id,"request":{"method":method,"params":params}})
        )
        .unwrap();
        input.flush().unwrap();
        let reply = self.line();
        assert_eq!(reply["id"], id);
        reply
    }
    fn query(&mut self, id: &str, arguments: Value) -> Value {
        self.request(
            "query_snapshot",
            json!({"capability":{"id":id,"version":1},"arguments":arguments}),
        )
    }
    fn invoke(&mut self, request: &str, id: &str, arguments: Value) -> Value {
        self.request(
            "invoke",
            json!({"client_request_id":request,"capability":{"id":id,"version":1},
            "arguments":arguments,"preconditions":[]}),
        )
    }
    /// Closing stdin is the session's normal end; accepted work drains first.
    fn close(mut self) {
        drop(self.input.take());
        assert!(self.child.wait().unwrap().success());
    }
}

#[test]
fn ordinary_host_rejects_retired_product_work_without_recording_an_operation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("catalog.sqlite");
    let project = dir.path().join("project");
    fs::create_dir(&project).unwrap();
    let (mut session, ready) = Session::open(&db, &project, &[]);
    let retired = [
        "plugins.build",
        "plugins.branch",
        "plugins.advance_branch",
        "plugins.checkpoint",
        "plugins.branches",
        "plugins.branch_head",
        "plugins.check_source",
        "plugins.preview",
        "windows.layout",
        "windows.update_layout",
        "windows.open_view",
        "windows.scenario",
        "windows.resolve",
        "scenarios.list",
        "scenarios.get",
        "scenarios.prepare",
        "scenarios.apply",
        "scenarios.checkpoint",
    ];
    let capabilities = ready["capabilities"].as_array().unwrap();
    for id in retired {
        assert!(
            !capabilities.iter().any(|cap| cap["capability"]["id"] == id),
            "{id}"
        );
        let reply = session.invoke(id, id, json!({}));
        assert_eq!(reply["ok"], false, "{reply}");
        assert!(
            reply["error"].as_str().unwrap().contains("not registered"),
            "{reply}"
        );
    }
    let records = session.query("operation.list_recent", json!({}));
    assert!(
        records["result"]["data"]["operations"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{records}"
    );
    assert!(!repository_path(&db).join("builds-v1").exists());
    session.close();
}

/// The fixture publishes the Core context contract itself, so a change to the
/// public type is exercised by real input validation rather than a copy.
fn schema<T: schemars::JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap()
}

fn package(path: &Path) -> PluginArchive {
    fs::create_dir_all(path.join("dist")).unwrap();
    let code = include_str!("../../host/tests/fixtures/context_provider.py");
    fs::write(path.join("backend.py"), code).unwrap();
    fs::write(path.join("dist/backend"), code).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path.join("dist/backend"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(path.join("BUILD.md"), "Copy backend.py to dist/backend.").unwrap();
    fs::write(path.join("deps.lock"), "Python 3 standard library.").unwrap();
    let query = |id: &str, input: Value, example: Value| {
        json!({
        "capability":{"id":id,"version":1},"kind":"query","title":id,"description":"Fixture context",
        "input_schema":input,"examples":[example],"output_schema":{"type":"object"},"recovery_schema":true,
        "required_scopes":[DOMAIN_SCOPE],"effects":[],"cancellation":"unsupported"})
    };
    let append = json!({
        "capability":{"id":"fixture.notes.append","version":1},"kind":"operation","title":"Append",
        "description":"Append text to one exact note version",
        "input_schema":{"type":"object","properties":{"note":{"type":"string"},"expected_version":{"type":"integer"},
            "text":{"type":"string"}},"required":["note","expected_version","text"],"additionalProperties":false},
        "examples":[{"note":"alpha","expected_version":1,"text":"more\n"}],"output_schema":{"type":"object"},"recovery_schema":true,
        "required_scopes":[DOMAIN_SCOPE],"effects":["fixture.note"],"cancellation":"unsupported"});
    fs::write(path.join("plugin.json"), serde_json::to_vec(&json!({
        "protocol_version":1,"id":"example.notes","name":"Notes fixture","version":"1",
        "description":"Headless context provider","license":"MIT",
        "source":{"files":["backend.py"],"lockfiles":["deps.lock"],"build_instructions":"BUILD.md","build":null},
        "dependencies":{},"requires":[],"views":[],
        "contexts":[{"id":"notes","title":"Notes","search":{"id":"fixture.notes.search","version":1},
            "preview":{"id":"fixture.notes.preview","version":1}}],
        "backend":{"executable":"dist/backend","arguments":[]},
        "capabilities":[
            query("fixture.notes.observe", json!({"type":"object","properties":{
                "mode":{"enum":["live","cached","unknown_time","partial","unavailable"]}},
                "required":["mode"],"additionalProperties":false}), json!({"mode":"live"})),
            query("fixture.notes.search", schema::<rho_plugin_protocol::ContextSearch>(),
                json!({"text":"","after":null,"limit":20})),
            query("fixture.notes.preview", schema::<rho_plugin_protocol::PreviewContext>(),
                json!({"reference":{"provider":{"plugin":"example.notes","instance":"notes-example",
                    "revision":format!("sha256:{}","a".repeat(64)),"artifact":format!("sha256:{}","b".repeat(64))},
                    "contribution":"notes","selector":{"note":"alpha","version":1}},"inclusion":{},"max_bytes":4096})),
            append],
        "configuration_schema":{"type":"object"},"default_configuration":{}
    })).unwrap()).unwrap();
    snapshot_directory(path, None, &backend_target()).unwrap()
}

#[test]
fn external_tool_manages_independent_hosts_and_reads_each_original_journal() {
    let dir = tempfile::tempdir().unwrap();
    let archive = package(&dir.path().join("package"));
    let projects: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|name| {
            let root = dir.path().join(name);
            let project = root.join("project");
            fs::create_dir_all(&project).unwrap();
            let db = root.join("operations.sqlite");
            PluginRepository::open(&repository_path(&db))
                .unwrap()
                .import(&archive)
                .unwrap();
            (project, db)
        })
        .collect();
    let (mut one, ready) = Session::open(&projects[0].1, &projects[0].0, &[DOMAIN_SCOPE]);
    let (mut two, _) = Session::open(&projects[1].1, &projects[1].0, &[DOMAIN_SCOPE]);
    assert!(ready["capabilities"].as_array().unwrap().iter().all(|cap| {
        !cap["capability"]["id"]
            .as_str()
            .unwrap()
            .starts_with("plugins.test_")
    }));
    let activate = json!({"revision":archive.revision.id,"artifact":archive.artifacts[0].id,
        "target":backend_target(),"alias":"notes","configuration":{}});
    let original = one.invoke("activate", "plugins.activate", activate.clone());
    assert_eq!(original["result"]["status"], "succeeded", "{original}");
    let id = original["result"]["operation"]["operation_id"].clone();
    assert!(two.request("get_operation", json!({"operation_id":id}))["result"].is_null());
    let other = two.invoke("activate", "plugins.activate", activate.clone());
    assert_eq!(other["result"]["status"], "succeeded", "{other}");
    assert_ne!(other["result"]["operation"]["operation_id"], id);

    // A retired selector cannot create a second instance in the current Host.
    for selector in [json!("old-child"), Value::Null] {
        let input = one.input.as_mut().unwrap();
        writeln!(
            input,
            "{}",
            json!({"id":"legacy","test_project":selector,"request":{
            "method":"invoke","params":{"client_request_id":"legacy","capability":{
                "id":"plugins.activate","version":1},"arguments":activate,"preconditions":[]}}})
        )
        .unwrap();
        input.flush().unwrap();
        let reply = one.line();
        assert_eq!(reply["ok"], false, "{reply}");
        assert!(
            reply["error"]
                .as_str()
                .unwrap()
                .contains("unknown field `test_project`")
        );
    }
    assert_eq!(
        one.query("plugins.instances", json!({"limit":20}))["result"]["data"]["total"],
        1
    );
    one.close();
    assert_eq!(
        two.query("plugins.instances", json!({"limit":20}))["result"]["data"]["total"],
        1
    );
    // The external caller ends and reopens a normal Host; no parent survives it.
    let (mut reopened, _) = Session::open(&projects[0].1, &projects[0].0, &[DOMAIN_SCOPE]);
    assert_eq!(
        reopened.request("get_operation", json!({"operation_id":id}))["result"],
        original["result"]
    );
    reopened.close();
    two.close();
}

#[test]
fn observations_keep_their_time_and_limits_after_external_changes_and_uncertain_work() {
    let dir = tempfile::tempdir().unwrap();
    let archive = package(&dir.path().join("package"));
    let db = dir.path().join("catalog.sqlite");
    PluginRepository::open(&repository_path(&db))
        .unwrap()
        .import(&archive)
        .unwrap();
    let project = dir.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("external.txt"), "A 研究\n").unwrap();
    let (mut session, _) = Session::open(&db, &project, &[DOMAIN_SCOPE]);
    let activated = session.invoke(
        "activate",
        "plugins.activate",
        json!({"revision":archive.revision.id,
        "artifact":archive.artifacts[0].id,"target":backend_target(),"alias":"notes",
        "configuration":{"uncertain_note":"alpha"}}),
    );
    assert_eq!(activated["result"]["status"], "succeeded", "{activated}");
    let instance = activated["result"]["output"]["instance"]["identity"].clone();
    let resolve = |session: &mut Session, id: &str| {
        let reply = session.query(
            "plugins.resolve",
            json!({"instance":instance,"capability":{"id":id,"version":1}}),
        );
        assert_eq!(reply["ok"], true, "{reply}");
        reply["result"]["data"].clone()
    };
    let binding = resolve(&mut session, "fixture.notes.observe");
    let observe = |session: &mut Session, mode: &str| {
        let reply = session.query(
            "fixture.notes.observe",
            json!({"binding":binding,
            "arguments":{"mode":mode},"preconditions":null}),
        );
        assert_eq!(reply["ok"], true, "{reply}");
        reply["result"].clone()
    };
    let first = observe(&mut session, "live");
    assert_eq!(first["data"]["text"], "A 研究\n");
    assert!(first["observed_at_ms"].is_i64());

    // A write outside Rho is a new observation, not an invented Operation.
    fs::write(project.join("external.txt"), "B 🧬\n").unwrap();
    let cached = observe(&mut session, "cached");
    assert_eq!(cached["observed_at_ms"], first["observed_at_ms"]);
    assert_eq!(cached["data"], first["data"]);
    assert_eq!(cached["status"], "ready", "cached bytes remain readable");
    assert_eq!(cached["completeness"], "partial");
    assert!(
        cached["notices"]
            .as_array()
            .unwrap()
            .contains(&json!("Retained bytes; disk has not been read again."))
    );
    let current = observe(&mut session, "live");
    assert_eq!(current["data"]["text"], "B 🧬\n");
    assert_ne!(current["data"]["digest"], first["data"]["digest"]);
    let unknown = observe(&mut session, "unknown_time");
    assert!(unknown["observed_at_ms"].is_null(), "{unknown}");
    assert_eq!(unknown["status"], "ready");
    let partial = observe(&mut session, "partial");
    assert_eq!(partial["completeness"], "partial");
    assert_eq!(
        partial["notices"],
        json!(["Only external.txt was inspected; its producer is unknown."])
    );
    let unavailable = observe(&mut session, "unavailable");
    assert_eq!(unavailable["status"], "unavailable");
    assert_eq!(unavailable["completeness"], "unknown");
    assert!(unavailable["observed_at_ms"].is_null());
    assert_eq!(
        unavailable["notices"],
        json!(["No native observation is available."])
    );

    let append = resolve(&mut session, "fixture.notes.append");
    let args = json!({"binding":append,"arguments":{"note":"alpha","expected_version":1,"text":"new\n"},"preconditions":null});
    let original = session.invoke("uncertain", "fixture.notes.append", args.clone());
    assert_eq!(original["result"]["status"], "uncertain", "{original}");
    let id = original["result"]["operation"]["operation_id"].clone();
    let search = resolve(&mut session, "fixture.notes.search");
    let page = session.query(
        "fixture.notes.search",
        json!({"binding":search,
        "arguments":{"text":"alpha","after":null,"limit":20},"preconditions":null}),
    );
    let reference = page["result"]["data"]["items"][0]["reference"].clone();
    assert_eq!(reference["selector"]["version"], 2);
    let preview = resolve(&mut session, "fixture.notes.preview");
    let shown = session.query(
        "fixture.notes.preview",
        json!({"binding":preview,
        "arguments":{"reference":reference,"inclusion":{},"max_bytes":4096},"preconditions":null}),
    );
    assert_eq!(shown["result"]["data"]["text"], "首行 alpha\nnew\n");
    assert_eq!(shown["result"]["data"]["data"]["executions"], 1);
    assert!(
        shown["result"]["observed_at_ms"].is_null(),
        "omitted owner time remains unknown"
    );
    // Current matching state cannot rewrite attribution or replay the old action.
    assert_eq!(
        session.request("get_operation", json!({"operation_id":id}))["result"],
        original["result"]
    );
    assert_eq!(
        session.invoke("uncertain", "fixture.notes.append", args)["result"],
        original["result"]
    );
    assert_eq!(observe(&mut session, "live")["data"], current["data"]);
    let unrelated = session.invoke("independent", "fixture.notes.append", json!({"binding":append,
        "arguments":{"note":"beta","expected_version":3,"text":"independent\n"},"preconditions":null}));
    assert_eq!(unrelated["result"]["status"], "succeeded", "{unrelated}");
    assert_eq!(unrelated["result"]["output"]["executions"], 2);
    let recent = session.query("operation.list_recent", json!({}));
    assert_eq!(recent["ok"], true, "{recent}");
    assert_eq!(
        recent["result"]["data"]["operations"]
            .as_array()
            .unwrap()
            .len(),
        3,
        "only activation and the two requested actions enter the journal: {recent}"
    );
    session.close();
}

#[test]
fn external_caller_reads_context_and_invokes_an_operation_without_a_window() {
    let dir = tempfile::tempdir().unwrap();
    let archive = package(&dir.path().join("package"));
    let db = dir.path().join("catalog.sqlite");
    PluginRepository::open(&repository_path(&db))
        .unwrap()
        .import(&archive)
        .unwrap();
    let project = dir.path().join("project");
    fs::create_dir(&project).unwrap();

    let (mut session, _) = Session::open(&db, &project, &[DOMAIN_SCOPE]);
    let activated = session.invoke("activate", "plugins.activate", json!({"revision":archive.revision.id,
        "artifact":archive.artifacts[0].id,"target":backend_target(),"alias":"notes","configuration":{}}));
    assert_eq!(activated["result"]["status"], "succeeded", "{activated}");
    let instance = activated["result"]["output"]["instance"]["identity"].clone();
    let resolve = |session: &mut Session, id: &str| {
        let reply = session.query(
            "plugins.resolve",
            json!({"instance":instance,"capability":{"id":id,"version":1}}),
        );
        assert_eq!(reply["ok"], true, "{reply}");
        reply["result"]["data"].clone()
    };

    // Discover the contribution, then search without naming a window.
    let search = resolve(&mut session, "fixture.notes.search");
    let page = session.query(
        "fixture.notes.search",
        json!({"binding":search,
        "arguments":{"text":"","after":null,"limit":20},"preconditions":null}),
    );
    assert_eq!(page["ok"], true, "{page}");
    let items = page["result"]["data"]["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 2);
    let alpha = items[0]["reference"].clone();
    assert_eq!(alpha["selector"], json!({"note":"alpha","version":1}));
    assert!(alpha.get("window").is_none(), "{alpha}");

    let preview_binding = resolve(&mut session, "fixture.notes.preview");
    let preview = |session: &mut Session, reference: &Value| {
        session.query("fixture.notes.preview",
        json!({"binding":preview_binding,"arguments":{"reference":reference,"inclusion":{},"max_bytes":4096},"preconditions":null}))
    };
    let shown = preview(&mut session, &alpha);
    assert_eq!(shown["result"]["data"]["text"], "首行 alpha\n", "{shown}");

    // A context reference with a legacy window field is a different contract.
    let mut legacy = alpha.clone();
    legacy["window"] = json!("window-example");
    assert_eq!(preview(&mut session, &legacy)["ok"], false);

    // One managed Operation, its original record, and an idempotent retry.
    let append = resolve(&mut session, "fixture.notes.append");
    let arguments = json!({"binding":append,"arguments":{"note":"alpha","expected_version":1,"text":"第二行\n"},"preconditions":null});
    let original = session.invoke("append-once", "fixture.notes.append", arguments.clone());
    assert_eq!(original["result"]["status"], "succeeded", "{original}");
    assert_eq!(original["result"]["output"]["executions"], 1);
    let operation_id = original["result"]["operation"]["operation_id"].clone();
    let retained = session.request("get_operation", json!({"operation_id":operation_id}));
    assert_eq!(retained["result"], original["result"]);
    let retry = session.invoke("append-once", "fixture.notes.append", arguments.clone());
    assert_eq!(retry["result"]["operation"]["operation_id"], operation_id);
    assert_eq!(
        retry["result"]["output"]["executions"], 1,
        "retry must not execute again"
    );

    // The old reference no longer matches the owner's version evidence.
    assert_eq!(preview(&mut session, &alpha)["ok"], false);
    let current = session.query(
        "fixture.notes.search",
        json!({"binding":search,
        "arguments":{"text":"alpha","after":null,"limit":20},"preconditions":null}),
    );
    let fresh = current["result"]["data"]["items"][0]["reference"].clone();
    let updated = preview(&mut session, &fresh);
    assert_eq!(updated["result"]["data"]["text"], "首行 alpha\n第二行\n");
    assert_eq!(updated["result"]["data"]["data"]["executions"], 1);

    // A stale expected version fails before any effect.
    let stale = session.invoke("append-stale", "fixture.notes.append", arguments);
    assert_eq!(stale["result"]["status"], "failed", "{stale}");
    session.close();
}

#[test]
fn domain_scopes_come_only_from_the_trusted_launcher() {
    let dir = tempfile::tempdir().unwrap();
    let archive = package(&dir.path().join("package"));
    let db = dir.path().join("catalog.sqlite");
    PluginRepository::open(&repository_path(&db))
        .unwrap()
        .import(&archive)
        .unwrap();
    let project = dir.path().join("project");
    fs::create_dir(&project).unwrap();

    // Default local authority covers generic Core work but no domain scope.
    let (mut session, ready) = Session::open(&db, &project, &[]);
    let generic = ready["capabilities"].as_array().unwrap();
    assert!(
        generic
            .iter()
            .any(|d| d["capability"]["id"] == "plugins.activate")
    );
    let activated = session.invoke("activate", "plugins.activate", json!({"revision":archive.revision.id,
        "artifact":archive.artifacts[0].id,"target":backend_target(),"alias":"notes","configuration":{}}));
    assert_eq!(activated["result"]["status"], "succeeded", "{activated}");
    let instance = activated["result"]["output"]["instance"]["identity"].clone();
    let binding = session.query(
        "plugins.resolve",
        json!({"instance":instance,"capability":{"id":"fixture.notes.append","version":1}}),
    );
    let binding = binding["result"]["data"].clone();
    let denied = session.invoke(
        "append",
        "fixture.notes.append",
        json!({"binding":binding,
        "arguments":{"note":"alpha","expected_version":1,"text":"x"},"preconditions":null}),
    );
    assert_eq!(denied["ok"], false, "{denied}");
    assert_eq!(denied["diagnostic"]["code"], "access_denied", "{denied}");
    // Request bodies cannot add authority.
    let forged = session.request("invoke", json!({"client_request_id":"forged",
        "capability":{"id":"fixture.notes.append","version":1},"scopes":[DOMAIN_SCOPE],
        "arguments":{"binding":binding,"arguments":{"note":"alpha","expected_version":1,"text":"x"},"preconditions":null},
        "preconditions":[]}));
    assert_eq!(forged["ok"], false, "{forged}");
    session.close();

    // The same catalog, relaunched with an explicit grant, admits the call. The
    // denied request created no Operation, so its identity is still unused.
    // (Ending a session does not leave a resumable live instance; activate anew.)
    let (mut granted, _) = Session::open(&db, &project, &[DOMAIN_SCOPE]);
    let activated = granted.invoke("activate-granted", "plugins.activate", json!({"revision":archive.revision.id,
        "artifact":archive.artifacts[0].id,"target":backend_target(),"alias":"notes-granted","configuration":{}}));
    assert_eq!(activated["result"]["status"], "succeeded", "{activated}");
    let instance = activated["result"]["output"]["instance"]["identity"].clone();
    let binding = granted.query(
        "plugins.resolve",
        json!({"instance":instance,"capability":{"id":"fixture.notes.append","version":1}}),
    );
    let binding = binding["result"]["data"].clone();
    let admitted = granted.invoke(
        "append",
        "fixture.notes.append",
        json!({"binding":binding,
        "arguments":{"note":"alpha","expected_version":1,"text":"x"},"preconditions":null}),
    );
    assert_eq!(admitted["result"]["status"], "succeeded", "{admitted}");
    assert_eq!(admitted["result"]["output"]["executions"], 1);
    // The owner receives the trusted caller scopes, now including the launcher grant.
    let scopes = admitted["result"]["output"]["scopes"].as_array().unwrap();
    assert!(scopes.contains(&json!(DOMAIN_SCOPE)), "{admitted}");
    assert!(
        !scopes.iter().any(|scope| scope == "workspace.run_r"),
        "{admitted}"
    );
    granted.close();

    // Invalid scope tokens are rejected before a Host opens.
    let refused = Command::new(env!("CARGO_BIN_EXE_rho"))
        .args(["--database"])
        .arg(&db)
        .arg("--project")
        .arg(&project)
        .args(["--grant-scope", "not a scope", "session"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!refused.status.success());
}

/// Newline-delimited JSON-RPC over `rho mcp` stdio, as an external Agent uses it.
struct Mcp {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<std::process::ChildStdout>,
    serial: u64,
}
impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Mcp {
    fn open(db: &Path, project: &Path, grants: &[&str]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rho"));
        command
            .arg("--database")
            .arg(db)
            .arg("--project")
            .arg(project);
        for scope in grants {
            command.arg("--grant-scope").arg(scope);
        }
        let mut child = command
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut mcp = Self {
            input: child.stdin.take(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
            serial: 0,
        };
        let initialized = mcp.call(
            "initialize",
            json!({"protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"headless-test","version":"1"}}),
        );
        assert!(
            initialized["result"]["serverInfo"].is_object(),
            "{initialized}"
        );
        mcp.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        mcp
    }
    fn send(&mut self, message: Value) {
        let input = self.input.as_mut().expect("open MCP input");
        writeln!(input, "{message}").unwrap();
        input.flush().unwrap();
    }
    fn call(&mut self, method: &str, params: Value) -> Value {
        self.serial += 1;
        let id = self.serial;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        loop {
            let mut line = String::new();
            assert!(
                self.output.read_line(&mut line).unwrap() > 0,
                "MCP server exited"
            );
            let message: Value = serde_json::from_str(&line).unwrap();
            // Skip server notifications such as tools/list_changed.
            if message["id"] == json!(id) {
                return message;
            }
        }
    }
    fn tools(&mut self) -> Vec<String> {
        let mut names = vec![];
        let mut cursor = Value::Null;
        loop {
            let params = if cursor.is_null() {
                json!({})
            } else {
                json!({"cursor":cursor})
            };
            let page = self.call("tools/list", params);
            names.extend(
                page["result"]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|tool| tool["name"].as_str().unwrap().to_owned()),
            );
            cursor = page["result"]["nextCursor"].clone();
            if cursor.is_null() {
                return names;
            }
        }
    }
    /// Structured tool result, or the tool-level error text.
    fn tool(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        let reply = self.call("tools/call", json!({"name":name,"arguments":arguments}));
        let result = &reply["result"];
        if result["isError"] == true || result.is_null() {
            return Err(reply.to_string());
        }
        Ok(result["structuredContent"]["result"].clone())
    }
    fn close(mut self) {
        drop(self.input.take());
        assert!(self.child.wait().unwrap().success());
    }
}

#[test]
fn external_mcp_client_discovers_and_calls_the_same_headless_capabilities() {
    let dir = tempfile::tempdir().unwrap();
    let archive = package(&dir.path().join("package"));
    let db = dir.path().join("catalog.sqlite");
    PluginRepository::open(&repository_path(&db))
        .unwrap()
        .import(&archive)
        .unwrap();
    let project = dir.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("external.txt"), "MCP observation 🧬\n").unwrap();

    // Without a launcher grant the domain tools are not offered.
    let mut ungranted = Mcp::open(&db, &project, &[]);
    let activated = ungranted
        .tool(
            "rho.plugins.activate.v1",
            json!({"client_request_id":"activate",
        "arguments":{"revision":archive.revision.id,"artifact":archive.artifacts[0].id,
        "target":backend_target(),"alias":"notes","configuration":{}}}),
        )
        .unwrap();
    assert_eq!(activated["status"], "succeeded", "{activated}");
    let tools = ungranted.tools();
    assert!(tools.iter().any(|name| name == "rho.plugins.resolve.v1"));
    assert!(
        !tools
            .iter()
            .any(|name| name.starts_with("rho.fixture.notes")),
        "{tools:?}"
    );
    assert!(
        ungranted
            .tool("rho.fixture.notes.search.v1", json!({}))
            .is_err()
    );
    ungranted.close();

    let mut mcp = Mcp::open(&db, &project, &[DOMAIN_SCOPE]);
    let activated = mcp
        .tool(
            "rho.plugins.activate.v1",
            json!({"client_request_id":"activate-granted",
        "arguments":{"revision":archive.revision.id,"artifact":archive.artifacts[0].id,
        "target":backend_target(),"alias":"notes-mcp","configuration":{}}}),
        )
        .unwrap();
    assert_eq!(activated["status"], "succeeded", "{activated}");
    let instance = activated["output"]["instance"]["identity"].clone();
    let tools = mcp.tools();
    for name in [
        "rho.fixture.notes.search.v1",
        "rho.fixture.notes.preview.v1",
        "rho.fixture.notes.append.v1",
    ] {
        assert!(
            tools.iter().any(|tool| tool == name),
            "{name} missing from {tools:?}"
        );
    }
    let resolve = |mcp: &mut Mcp, id: &str| {
        mcp.tool(
            "rho.plugins.resolve.v1",
            json!({"instance":instance,"capability":{"id":id,"version":1}}),
        )
        .unwrap()["data"]
            .clone()
    };
    let observation = resolve(&mut mcp, "fixture.notes.observe");
    let live = mcp
        .tool(
            "rho.fixture.notes.observe.v1",
            json!({"binding":observation,
        "arguments":{"mode":"live"},"preconditions":null}),
        )
        .unwrap();
    let cached = mcp
        .tool(
            "rho.fixture.notes.observe.v1",
            json!({"binding":observation,
        "arguments":{"mode":"cached"},"preconditions":null}),
        )
        .unwrap();
    assert!(live["observed_at_ms"].is_i64());
    assert_eq!(cached["observed_at_ms"], live["observed_at_ms"]);
    assert_eq!(cached["status"], "ready");
    assert_eq!(
        cached["notices"][0],
        "Retained bytes; disk has not been read again."
    );
    let unknown = mcp
        .tool(
            "rho.fixture.notes.observe.v1",
            json!({"binding":observation,
        "arguments":{"mode":"unknown_time"},"preconditions":null}),
        )
        .unwrap();
    assert!(unknown["observed_at_ms"].is_null());
    let search = resolve(&mut mcp, "fixture.notes.search");
    let page = mcp
        .tool(
            "rho.fixture.notes.search.v1",
            json!({"binding":search,
        "arguments":{"text":"beta","after":null,"limit":20},"preconditions":null}),
        )
        .unwrap();
    let beta = page["data"]["items"][0]["reference"].clone();
    assert_eq!(beta["selector"], json!({"note":"beta","version":3}));
    assert!(beta.get("window").is_none());
    let preview = resolve(&mut mcp, "fixture.notes.preview");
    let shown = mcp
        .tool(
            "rho.fixture.notes.preview.v1",
            json!({"binding":preview,
        "arguments":{"reference":beta,"inclusion":{},"max_bytes":4096},"preconditions":null}),
        )
        .unwrap();
    assert_eq!(shown["data"]["text"], "beta 🧬\n");

    let append = resolve(&mut mcp, "fixture.notes.append");
    let call = json!({"client_request_id":"mcp-append","arguments":{"binding":append,
        "arguments":{"note":"beta","expected_version":3,"text":"MCP\n"},"preconditions":null}});
    let original = mcp
        .tool("rho.fixture.notes.append.v1", call.clone())
        .unwrap();
    assert_eq!(original["status"], "succeeded", "{original}");
    assert_eq!(original["operation"]["caller"]["kind"], "agent");
    let operation_id = original["operation"]["operation_id"].clone();
    let retained = mcp
        .tool("rho.operation.get.v1", json!({"operation_id":operation_id}))
        .unwrap();
    assert_eq!(
        retained["data"]["record"]["status"], "succeeded",
        "{retained}"
    );
    let retry = mcp.tool("rho.fixture.notes.append.v1", call).unwrap();
    assert_eq!(retry["operation"]["operation_id"], operation_id);
    assert_eq!(
        retry["output"]["executions"], 1,
        "retry must not execute again"
    );
    mcp.close();
}
