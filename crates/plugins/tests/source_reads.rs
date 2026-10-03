use base64::{Engine, engine::general_purpose::STANDARD};
use rho_plugin_protocol::*;
use rho_plugins::*;
use serde_json::json;
use std::{collections::BTreeMap, fs, path::Path};

fn fixture(path: &Path) -> PluginArchive {
    fs::create_dir_all(path.join("dist")).unwrap();
    fs::write(path.join("main.ts"), "document.title = '研究';").unwrap();
    fs::write(path.join("data.bin"), [0u8, 255, 10, 128].repeat(150_000)).unwrap();
    fs::write(path.join("empty"), "").unwrap();
    fs::write(path.join("deps.lock"), "no external dependencies").unwrap();
    fs::write(
        path.join("BUILD.md"),
        "Build with an explicitly provided toolchain.",
    )
    .unwrap();
    fs::write(
        path.join("dist/index.html"),
        "<html>Original artifact</html>",
    )
    .unwrap();
    fs::write(path.join("plugin.json"), serde_json::to_vec(&json!({
        "protocol_version":1,"id":"example.source","name":"Source fixture","version":"1.0.0","description":"Independent package", "license":"MIT",
        "source":{"files":["main.ts","data.bin","empty"],"lockfiles":["deps.lock"],"build_instructions":"BUILD.md","build":null},
        "dependencies":{},"requires":[],"views":[{"id":"view","title":"View","entrypoint":"dist/index.html","state_schema":{"type":"object"},"configuration_schema":{"type":"object"},"resource_kinds":[]}],
        "capabilities":[],"contexts":[],"backend":null,"configuration_schema":{"type":"object"},"default_configuration":{}
    })).unwrap()).unwrap();
    snapshot_directory(path, None, "ui-web").unwrap()
}
fn inventory(repo: &PluginRepository) -> (u64, u64, u64, u64) {
    let db = rusqlite::Connection::open(repo.root().join("catalog-v1.sqlite3")).unwrap();
    db.query_row("SELECT (SELECT COUNT(*) FROM revisions),(SELECT COUNT(*) FROM blobs),(SELECT COUNT(*) FROM source_files),(SELECT COUNT(*) FROM revision_refs)", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap()
}

#[test]
fn immutable_source_pages_and_binary_chunks_are_bounded_pure_observations() {
    let temp = tempfile::tempdir().unwrap();
    let package = fixture(&temp.path().join("package"));
    let mut repo = PluginRepository::open(&temp.path().join("repo")).unwrap();
    repo.import(&package).unwrap();
    let before = inventory(&repo);
    let reader = PluginRepository::observe(repo.root()).unwrap().unwrap();
    let mut cursor = None;
    let mut files = BTreeMap::new();
    loop {
        let page = reader
            .source_page(&ListPluginSource {
                revision: package.revision.id.clone(),
                after: cursor,
                limit: 2,
            })
            .unwrap();
        assert!(page.files.len() <= 2);
        assert_eq!(page.total, package.revision.files.len() as u64);
        files.extend(page.files);
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(files, package.revision.files);
    let mut request = ReadPluginSource {
        revision: package.revision.id.clone(),
        path: PackagePath::new("data.bin").unwrap(),
        offset: 0,
        limit: 65536,
    };
    let mut bytes = vec![];
    loop {
        let chunk = reader.read_source(&request).unwrap();
        assert_eq!(chunk.offset, request.offset);
        let part = STANDARD.decode(chunk.content_base64).unwrap();
        assert!(part.len() <= 65536);
        bytes.extend(part);
        if let Some(next) = chunk.next_offset {
            request.offset = next;
        } else {
            break;
        }
    }
    assert_eq!(
        bytes,
        fs::read(temp.path().join("package/data.bin")).unwrap()
    );
    request.offset = bytes.len() as u64;
    assert!(
        reader
            .read_source(&request)
            .unwrap()
            .content_base64
            .is_empty()
    );
    request.offset += 1;
    assert!(reader.read_source(&request).is_err());
    request.path = PackagePath::new("empty").unwrap();
    request.offset = 0;
    assert!(reader.read_source(&request).unwrap().next_offset.is_none());
    for limit in [0, 65537] {
        request.limit = limit;
        assert!(reader.read_source(&request).is_err());
    }
    request.limit = 65536;
    request.path = PackagePath::new("dist/index.html").unwrap();
    assert!(reader.read_source(&request).is_err());
    for limit in [0, 101] {
        assert!(
            reader
                .source_page(&ListPluginSource {
                    revision: package.revision.id.clone(),
                    after: None,
                    limit
                })
                .is_err()
        );
    }
    assert_eq!(inventory(&repo), before);
}

#[test]
fn source_digest_checks_include_bytes_outside_the_requested_chunk() {
    let temp = tempfile::tempdir().unwrap();
    let package = fixture(&temp.path().join("package"));
    let mut repo = PluginRepository::open(&temp.path().join("repo")).unwrap();
    repo.import(&package).unwrap();
    let path = PackagePath::new("data.bin").unwrap();
    let file = &package.revision.files[&path];
    let request = ReadPluginSource {
        revision: package.revision.id.clone(),
        path,
        offset: 0,
        limit: 1,
    };
    let db = rusqlite::Connection::open(repo.root().join("catalog-v1.sqlite3")).unwrap();
    let mut bytes = STANDARD.decode(&package.blobs[&file.digest]).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    db.execute(
        "UPDATE blobs SET bytes=? WHERE digest=?",
        rusqlite::params![bytes, file.digest.as_str()],
    )
    .unwrap();
    assert!(
        repo.read_source(&request)
            .unwrap_err()
            .to_string()
            .contains("digest")
    );
    db.execute(
        "UPDATE blobs SET bytes=? WHERE digest=?",
        rusqlite::params![b"wrong length".as_slice(), file.digest.as_str()],
    )
    .unwrap();
    assert!(
        repo.read_source(&request)
            .unwrap_err()
            .to_string()
            .contains("length")
    );
}
