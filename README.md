# Rho core

The generic Host, operation journal, plugin lifecycle, public protocol and SDK,
CLI, HTTP and MCP edges. Scientific implementations live in `Rho-plugins`;
the application shell and product assembly live in `Rho`.

```sh
cargo build --locked
node scripts/check-boundaries.mjs
cargo test -p rho-operation --lib --locked
```

`target/debug/rho --project /absolute/project --database /absolute/catalog.sqlite
workbench` starts an empty generic Host. It embeds no application, scientific
plugin or R example. Add `--assets /absolute/application/assets` to select an
application's `index.html`, `app.js` and `style.css`; changing those files does not
rebuild the Host. `--default-project /absolute/existing/project` provides an
application-selected project action without creating or populating a project.

Public SDK source is maintained here. `node scripts/generate.mjs` regenerates its
contracts. Commit first, then `node scripts/export-sdk.mjs /new/snapshot` exports
the public Rust/TypeScript dependency, exact file hashes, license and source
revision. `--javascript-only` exports the application's dependency. These
snapshots are vendored dependencies, never a second source of maintenance.

Tests are selected by concrete core behavior. Domain artifact conformance tests
can consume explicitly provided plugin packages; their presence does not require
all scientific workflows for a core edit. Old monorepo layout checks are retired.
See the application repository's `docs/DEVELOPMENT.md` for coordinated work.
