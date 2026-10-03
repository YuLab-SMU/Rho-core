use rho_plugin_protocol::*;
use rho_plugins::*;
use serde_json::json;
use std::{fs, path::Path};

fn fixture(path: &Path) {
    fs::create_dir_all(path.join("src")).unwrap();
    fs::create_dir_all(path.join("dist")).unwrap();
    fs::write(
        path.join("src/view.ts"),
        "document.body.textContent = 'Independent plugin';",
    )
    .unwrap();
    fs::write(
        path.join("deps.lock"),
        "No third-party dependencies. JavaScript ES2022.\n",
    )
    .unwrap();
    fs::write(
        path.join("BUILD.md"),
        "Build: copy src/view.ts to dist/view.js; include index.html.\n",
    )
    .unwrap();
    fs::write(
        path.join("dist/index.html"),
        "<!doctype html><html><body>Independent plugin</body></html>",
    )
    .unwrap();
    fs::write(path.join("plugin.json"), serde_json::to_vec_pretty(&json!({
        "protocol_version": 1, "id": "example.independent", "name": "Independent Viewer",
        "version": "1.0.0", "description": "A plugin created outside the Rho source tree", "license": "MIT",
        "source": { "files": ["src/view.ts"], "lockfiles": ["deps.lock"], "build_instructions": "BUILD.md", "build": null },
        "dependencies": {}, "requires": [], "capabilities": [], "contexts": [], "backend": null,
        "views": [{ "id": "report", "title": "Report", "entrypoint": "dist/index.html",
             "configuration_schema": {"type":"object"}, "resource_kinds": [] }],
        "configuration_schema": {"type":"object", "additionalProperties":false}, "default_configuration": {}
    })).unwrap()).unwrap();
}

#[test]
fn external_package_export_remove_reimport_has_identical_identity_and_permissions() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("external");
    fixture(&source);
    let archive = snapshot_directory(&source, None, "ui-web").unwrap();
    let mut repo = PluginRepository::open(&temp.path().join("store")).unwrap();
    let installed = repo.import(&archive).unwrap();
    assert_eq!(repo.import(&archive).unwrap(), installed);
    assert_eq!(repo.list().unwrap().len(), 1);
    repo.export_file(&installed.revision, &temp.path().join("plugin.rho-plugin"))
        .unwrap();
    assert!(
        repo.export_file(&installed.revision, &temp.path().join("plugin.rho-plugin"))
            .is_err()
    );
    repo.remove(&installed.revision).unwrap();
    assert!(repo.list().unwrap().is_empty());
    assert!(
        repo.blob(&archive.revision.files.values().next().unwrap().digest)
            .is_err()
    );
    let imported = read_archive(&temp.path().join("plugin.rho-plugin")).unwrap();
    assert_eq!(repo.import(&imported).unwrap(), installed);
    assert_eq!(repo.export(&installed.revision).unwrap(), archive);
    assert!(
        repo.revision(&installed.revision)
            .unwrap()
            .manifest
            .requires
            .is_empty()
    );
}

#[test]
fn source_and_artifacts_have_independent_immutable_identity() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    let original = snapshot_directory(temp.path(), None, "ui-web").unwrap();
    fs::write(
        temp.path().join("dist/index.html"),
        "<body>Second build</body>",
    )
    .unwrap();
    let rebuilt = snapshot_directory(temp.path(), None, "ui-web").unwrap();
    assert_eq!(original.revision.id, rebuilt.revision.id);
    assert_ne!(original.artifacts[0].id, rebuilt.artifacts[0].id);
    fs::write(
        temp.path().join("src/view.ts"),
        "document.body.textContent = 'User branch';",
    )
    .unwrap();
    let modified =
        snapshot_directory(temp.path(), Some(original.revision.id.clone()), "ui-web").unwrap();
    assert_ne!(original.revision.id, modified.revision.id);
    assert_eq!(original.revision.manifest.id, modified.revision.manifest.id);
    let mut repo = PluginRepository::open(&temp.path().join("store")).unwrap();
    repo.import(&original).unwrap();
    repo.import(&rebuilt).unwrap();
    repo.import(&modified).unwrap();
    assert_eq!(
        repo.inspect(&original.revision.id).unwrap().artifacts.len(),
        2
    );
    assert_eq!(repo.list().unwrap().len(), 2);
    // Immutable snapshots do not follow subsequent edits in the development tree.
    assert_eq!(
        repo.blob(&original.revision.files[&PackagePath::new("src/view.ts").unwrap()].digest)
            .unwrap(),
        b"document.body.textContent = 'Independent plugin';"
    );
}

#[test]
fn every_reference_blocks_removal_and_query_never_creates_storage() {
    let temp = tempfile::tempdir().unwrap();
    let absent = temp.path().join("missing");
    assert!(PluginRepository::observe(&absent).unwrap().is_none());
    assert!(!absent.exists());
    fixture(temp.path());
    let archive = snapshot_directory(temp.path(), None, "ui-web").unwrap();
    let mut repo = PluginRepository::open(&temp.path().join("store")).unwrap();
    repo.import(&archive).unwrap();
    for kind in ["instance", "operation"] {
        repo.retain(kind, "original-identity", &archive.revision.id)
            .unwrap();
        match repo.remove(&archive.revision.id).unwrap_err() {
            PluginError::Referenced(refs) => {
                assert_eq!(refs, vec![format!("{kind}:original-identity")])
            }
            error => panic!("wrong error: {error}"),
        }
        repo.release_reference(kind, "original-identity", &archive.revision.id)
            .unwrap();
    }
    let observer = PluginRepository::observe(repo.root()).unwrap().unwrap();
    assert_eq!(observer.list().unwrap().len(), 1);
    repo.remove(&archive.revision.id).unwrap();
}

#[test]
fn immutable_package_compare_keeps_original_revision() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    let original = snapshot_directory(temp.path(), None, "ui-web").unwrap();
    let mut repo = PluginRepository::open(&temp.path().join("store")).unwrap();
    repo.import(&original).unwrap();
    fs::write(
        temp.path().join("src/view.ts"),
        "document.body.textContent = 'Modified';",
    )
    .unwrap();
    let changed =
        snapshot_directory(temp.path(), Some(original.revision.id.clone()), "ui-web").unwrap();
    repo.import(&changed).unwrap();
    let difference = repo
        .compare(&original.revision.id, &changed.revision.id)
        .unwrap();
    assert_eq!(difference.files.len(), 1);
    assert_eq!(difference.files[0].path.as_str(), "src/view.ts");
    assert_eq!(repo.export(&original.revision.id).unwrap(), original);
    assert!(
        repo.references(&original.revision.id)
            .unwrap()
            .contains(&format!("revision:{}", changed.revision.id))
    );
}

#[test]
fn tampering_and_validation_failures_leave_no_partial_installation() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    let archive = snapshot_directory(temp.path(), None, "ui-web").unwrap();
    let mut repo = PluginRepository::open(&temp.path().join("store")).unwrap();
    let mut tampered = archive.clone();
    tampered.revision.manifest.name = "Altered".into();
    assert!(repo.import(&tampered).is_err());
    let mut tampered = archive.clone();
    *tampered.blobs.values_mut().next().unwrap() = "YQ==".into();
    assert!(repo.import(&tampered).is_err());
    let mut tampered = archive.clone();
    tampered.artifacts[0].revision = RevisionId::new(format!("sha256:{}", "b".repeat(64))).unwrap();
    assert!(repo.import(&tampered).is_err());
    assert!(repo.list().unwrap().is_empty());
    // A forged delivery-origin flag is not a way to obtain different validation.
    let mut json = serde_json::to_value(&archive).unwrap();
    json["revision"]["manifest"]["bundled"] = json!(true);
    assert!(serde_json::from_value::<PluginArchive>(json).is_err());
    let mut tampered = archive;
    tampered.revision.files.values_mut().next().unwrap().bytes = u64::MAX;
    tampered.revision.id = revision_digest(&tampered.revision).unwrap();
    assert!(repo.import(&tampered).is_err());
}

#[cfg(unix)]
#[test]
fn symlinks_missing_source_case_collisions_and_effectful_queries_are_rejected() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("private"), "outside").unwrap();
    symlink(
        outside.path().join("private"),
        temp.path().join("dist/escape"),
    )
    .unwrap();
    assert!(snapshot_directory(temp.path(), None, "ui-web").is_err());
    fs::remove_file(temp.path().join("dist/escape")).unwrap();
    fs::remove_file(temp.path().join("src/view.ts")).unwrap();
    assert!(snapshot_directory(temp.path(), None, "ui-web").is_err());
    fixture(temp.path());
    let archive = snapshot_directory(temp.path(), None, "ui-web").unwrap();
    let mut collision = archive.clone();
    let file = collision.artifacts[0]
        .files
        .values()
        .next()
        .unwrap()
        .clone();
    collision.artifacts[0]
        .files
        .insert(PackagePath::new("dist/INDEX.html").unwrap(), file);
    collision.artifacts[0].id = artifact_digest(&collision.artifacts[0]).unwrap();
    assert!(validate_archive(&collision).is_err());
    let mut manifest = archive.revision.manifest;
    manifest.backend = Some(BackendEntrypoint {
        executable: PackagePath::new("dist/backend").unwrap(),
        arguments: vec![],
    });
    manifest.capabilities.push(CapabilityContribution {
        preflight: None,
        capability: CapabilityKey {
            id: ContributionId::new("example.read").unwrap(),
            version: 1,
        },
        kind: CapabilityKind::Query,
        title: "Read".into(),
        description: "Read without starting work".into(),
        input_schema: json!({}),
        examples: vec![json!({})],
        output_schema: json!({}),
        recovery_schema: json!({}),
        required_scopes: Default::default(),
        effects: ["start-runtime".to_string()].into(),
        cancellation: CancellationSupport::Unsupported,
    });
    assert!(manifest.validate().is_err());
}

#[test]
fn native_code_and_build_recipes_are_not_executed_during_snapshot_or_import() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    let path = temp.path().join("plugin.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let build_effect = temp.path().join("build-ran");
    let native_effect = temp.path().join("native-ran");
    let write_marker = |marker: &Path| {
        format!(
            "from pathlib import Path; Path({}).write_text('executed')",
            serde_json::to_string(&marker.to_string_lossy()).unwrap()
        )
    };
    manifest["source"]["build"] = json!({"command":["python3", "-c", write_marker(&build_effect)]});
    let backend = format!("#!/usr/bin/env python3\n{}\n", write_marker(&native_effect));
    fs::write(temp.path().join("src/backend.py"), &backend).unwrap();
    fs::write(temp.path().join("dist/backend"), &backend).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            temp.path().join("dist/backend"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    manifest["source"]["files"]
        .as_array_mut()
        .unwrap()
        .push(json!("src/backend.py"));
    manifest["backend"] = json!({"executable":"dist/backend","arguments":[]});
    fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let archive = snapshot_directory(temp.path(), None, &backend_target()).unwrap();
    let mut repo = PluginRepository::open(&temp.path().join("store")).unwrap();
    repo.import(&archive).unwrap();
    assert_eq!(repo.export(&archive.revision.id).unwrap(), archive);
    assert!(
        !build_effect.exists(),
        "package inspection must not execute a build recipe"
    );
    assert!(
        !native_effect.exists(),
        "package import must not start its native owner"
    );
}

#[test]
fn catalog_pagination_returns_every_installed_revision_once() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    let root = temp.path().join("store");
    let mut repo = PluginRepository::open(&root).unwrap();
    let mut installed = vec![];
    for index in 0..5 {
        fs::write(
            temp.path().join("src/view.ts"),
            format!("document.body.textContent='{index}';"),
        )
        .unwrap();
        installed.push(
            repo.import(&snapshot_directory(temp.path(), None, "ui-web").unwrap())
                .unwrap()
                .revision,
        );
    }
    assert!(repo.list_page(None, 0).is_err());
    assert!(repo.list_page(None, 101).is_err());
    let mut after = None;
    let mut ids = vec![];
    loop {
        let page = repo.list_page(after.as_ref(), 2).unwrap();
        assert_eq!(page.total, 5);
        assert!(page.revisions.len() <= 2);
        ids.extend(page.revisions.into_iter().map(|v| v.revision));
        after = page.next;
        if after.is_none() {
            break;
        }
    }
    installed.sort();
    assert_eq!(
        ids, installed,
        "pagination must neither lose nor duplicate a revision"
    );
}

#[test]
fn previous_catalog_is_rejected_without_changing_its_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let incompatible = temp.path().join("incompatible");
    fs::create_dir(&incompatible).unwrap();
    let connection = rusqlite::Connection::open(incompatible.join("catalog-v1.sqlite3")).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE plugin_schema(version INTEGER); INSERT INTO plugin_schema VALUES(1);",
        )
        .unwrap();
    drop(connection);
    let database = incompatible.join("catalog-v1.sqlite3");
    let original = fs::read(&database).unwrap();
    assert!(PluginRepository::observe(&incompatible).is_err());
    assert!(PluginRepository::open(&incompatible).is_err());
    assert_eq!(fs::read(database).unwrap(), original);
}

#[test]
fn storage_failure_rolls_back_source_artifacts_and_blobs_together() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    let archive = snapshot_directory(temp.path(), None, "ui-web").unwrap();
    let mut repo = PluginRepository::open(&temp.path().join("store")).unwrap();
    let injection = rusqlite::Connection::open(repo.root().join("catalog-v1.sqlite3")).unwrap();
    injection.execute_batch("CREATE TRIGGER fail_artifact BEFORE INSERT ON artifacts BEGIN SELECT RAISE(ABORT, 'injected storage failure'); END;").unwrap();
    assert!(matches!(
        repo.import(&archive),
        Err(PluginError::Database(_))
    ));
    assert!(repo.list().unwrap().is_empty());
    assert!(
        repo.blob(&archive.revision.files.values().next().unwrap().digest)
            .is_err()
    );
    injection
        .execute_batch("DROP TRIGGER fail_artifact;")
        .unwrap();
    repo.import(&archive).unwrap();
    assert_eq!(repo.export(&archive.revision.id).unwrap(), archive);
}

#[test]
fn packaged_visual_source_is_opaque_to_core_and_keeps_exact_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("external");
    fixture(&source);
    fs::create_dir(source.join("views")).unwrap();
    let bytes = b"Owner-defined canvas format; Core does not interpret this.";
    fs::write(source.join("views/canvas.json"), bytes).unwrap();
    let file = source.join("plugin.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    manifest["source"]["files"]
        .as_array_mut()
        .unwrap()
        .push(json!("views/canvas.json"));
    fs::write(file, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let archive = snapshot_directory(&source, None, "ui-web").unwrap();
    let mut repo = PluginRepository::open(&temp.path().join("store")).unwrap();
    repo.import(&archive).unwrap();
    let read = repo
        .read_source(&ReadPluginSource {
            revision: archive.revision.id.clone(),
            path: PackagePath::new("views/canvas.json").unwrap(),
            offset: 0,
            limit: 100,
        })
        .unwrap();
    use base64::Engine;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(read.content_base64)
            .unwrap(),
        bytes
    );
    assert_eq!(read.file.digest, content_digest(bytes));
}
