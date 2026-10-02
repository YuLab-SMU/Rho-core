# Rho core

The generic Host, operation journal, plugin lifecycle, public protocol and SDK,
CLI, HTTP and MCP edges. Scientific implementations live in
[Rho-plugins](https://github.com/YuLab-SMU/Rho-plugins); the application shell and
product assembly live in [Rho](https://github.com/YuLab-SMU/Rho).

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

Local callers of `session`, `mcp` and `workbench` (HTTP and MCP) receive only the
scopes that Core's own capabilities need. A plugin's domain authority is granted
explicitly by the launcher with repeatable `--grant-scope SCOPE`, for example
`--grant-scope project.write`. Manifests, request bodies and clients of an
existing Host (`--connect-url-file`) cannot add scopes.

Context search and references are windowless: an external caller addresses the
provider and its owner-defined selector directly. `cargo test -p rho-cli --test
headless --locked` runs that flow through a CLI session and stdio MCP with a
protocol-only test plugin.

Queries report scoped observations, not complete knowledge of external activity.
Plugin `QueryResult` can supply `observed_at_ms` (Unix milliseconds) and `notices`;
Core preserves them in `QuerySnapshot`. An omitted or null time remains unknown,
including in composed observations. Core never substitutes the reply time for an
unknown owner observation time. Notices share the existing fixed 1 MiB control
frame and public query-response budgets; exceeding either rejects that response
rather than truncating its limitations. These protocol bounds are not configurable
per call; owners paginate or use resources for larger payloads.

Cached data remains readable (`ready`, with `partial` completeness and a cache
notice). Its time describes the retained observation, not a fresh native check.
Neither readability nor completeness establishes that the data is suitable for a
write; authority and owner-specific native preconditions still govern that action.

`Uncertain` ends an Operation's execution lifecycle, not later learning. Read the
original journal record and make new owner queries without replaying the action.
Matching current state does not prove that the old Operation caused it. External
changes do not create invented Operations, and independent work is not globally
blocked. `operation.reconcile_commit` commits a retained validated result; it does
not reconstruct unknown external history or establish a scientific conclusion.

The headless test above also exercises actual external file changes, cached and
partial reads, unknown times, and fresh reads after an uncertain Operation. It
checks original-record preservation, idempotent retries and independent work.
This is a Core boundary check, not Editor draft merging, a persistent evidence
network, a scientific verification result or application integration. Consumer
SDK refresh and domain workflows remain separate integration work.

Public SDK source is maintained here. `node scripts/generate.mjs` regenerates its
contracts. Commit first, then `node scripts/export-sdk.mjs /new/snapshot` exports
the public Rust/TypeScript dependency, exact file hashes, license and source
revision. `--javascript-only` exports the application's dependency. These
snapshots are vendored dependencies, never a second source of maintenance.

Tests are selected by concrete core behavior. Domain artifact conformance tests
can consume explicitly provided plugin packages; their presence does not require
all scientific workflows for a core edit. Old monorepo layout checks are retired.
See the application's [development guide](https://github.com/YuLab-SMU/Rho/blob/main/docs/DEVELOPMENT.md)
for coordinated work.
