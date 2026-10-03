use std::{fs, path::Path};
use serde_json::json;
fn source(path: &Path) {
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
            "state_schema": {"type":"object"}, "configuration_schema": {"type":"object"}, "resource_kinds": [] }],
        "configuration_schema": {"type":"object", "additionalProperties":false}, "default_configuration": {}
    })).unwrap()).unwrap();
}

pub fn package(path: &Path) -> rho_plugin_protocol::PluginArchive {
    source(path);
    rho_plugins::snapshot_directory(path, None, "ui-web").unwrap()
}

pub fn install(database: &Path, package_root: &Path) -> serde_json::Value {
    let archive = package(package_root);
    rho_plugins::PluginRepository::open(&rho_plugins::repository_path(database)).unwrap().import(&archive).unwrap();
    json!({"revision":archive.revision.id,"artifact":archive.artifacts[0].id,"target":"ui-web","alias":"edge","configuration":{}})
}
