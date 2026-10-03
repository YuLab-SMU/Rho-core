#[path = "../../plugins/tests/fixtures/ui_package.rs"]
mod ui_package;
use serde_json::{Value, json};
use std::process::Command;

#[test]
fn independent_cli_processes_reuse_durable_operation_and_query_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("next.sqlite");
    let project = dir.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let activation = ui_package::install(&db, &dir.path().join("package"));
    let invoke = |name: &str| {
        let mut arguments = activation.clone();
        arguments["alias"] = json!(name);
        Command::new(env!("CARGO_BIN_EXE_rho"))
            .arg("--project")
            .arg(&project)
            .arg("--database")
            .arg(&db)
            .args([
                "invoke",
                "--client-request-id",
                "cli-once",
                "--capability",
                "plugins.activate",
                "--arguments",
            ])
            .arg(arguments.to_string())
            .output()
            .unwrap()
    };
    let first = invoke("first");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let value: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(value["operation"]["status"], json!("succeeded"));
    assert_eq!(value["runtime"], json!("plugins"));
    let repeat = invoke("first");
    let repeated: Value = serde_json::from_slice(&repeat.stdout).unwrap();
    assert_eq!(repeated, value);
    let conflict = invoke("changed");
    assert!(!conflict.status.success());
    let conflict: Value = serde_json::from_slice(&conflict.stderr).unwrap();
    // A changed request cannot reuse the original idempotency identity.
    assert_eq!(conflict["diagnostic"]["code"], "idempotency_conflict");

    let before = std::fs::read(&db).unwrap();
    let query = Command::new(env!("CARGO_BIN_EXE_rho"))
        .arg("--database")
        .arg(&db)
        .args([
            "get-operation",
            value["operation"]["operation"]["operation_id"]
                .as_str()
                .unwrap(),
        ])
        .output()
        .unwrap();
    assert!(query.status.success());
    let queried: Value = serde_json::from_slice(&query.stdout).unwrap();
    // The durable result is identical; the journal-only reader advertises only available queries.
    let mut original_record = value["operation"].clone();
    let mut queried_record = queried["operation"].clone();
    let _original_reads = original_record
        .as_object_mut()
        .unwrap()
        .remove("next_reads")
        .unwrap();
    let queried_reads = queried_record
        .as_object_mut()
        .unwrap()
        .remove("next_reads")
        .unwrap();
    assert_eq!(queried_record, original_record);
    assert_eq!(queried_reads.as_array().unwrap().len(), 1);
    assert_eq!(queried_reads[0]["capability"]["id"], "operation.get");
    assert_eq!(
        queried_reads[0]["arguments"]["operation_id"],
        original_record["operation"]["operation_id"]
    );
    assert_eq!(before, std::fs::read(&db).unwrap());
}
