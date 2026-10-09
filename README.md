# Rho core

The generic Host, operation journal, plugin lifecycle, public protocol and SDK,
CLI, HTTP and MCP edges. Scientific implementations live in
[Rho-plugins](https://github.com/YuLab-SMU/Rho-plugins); the application shell and
product assembly live in [Rho](https://github.com/YuLab-SMU/Rho).

Read the [mission, boundaries and implementation plan](docs/MISSION-AND-PLAN.md)
before selecting work. It preserves the observation-first design direction,
engineering tradeoffs and the next owner-scoped flows; it is not an implementation
or verification ledger.

Rho supports researchers and external agents who gather material, act and revise
their understanding in a changing environment. Core supplies request routing,
execution coordination and retained reports; owners describe bounded observations.
Context is acquired for the question at hand. Ordinary work does not wait for a
complete environment model, provenance graph or scientific validity judgment.
Permission-system construction is deferred in the plan.

The [architecture and code navigation](docs/ARCHITECTURE.md) describes crate
ownership, Host entry points and the shared execution lifetime.
Its [agent-facing entry points](docs/ARCHITECTURE.md#agent-facing-entry-points)
explain discovery, observation, action, explicit relationships and continuation.
Skill-shaped progressive disclosure, Loop continuation points and Graph anchors are
embedded in those information modules. Goals, selection, reasoning, graph layout and
orchestration stay with external runtimes. Context views are assembled by the caller
for a particular question, not materialized as a Core world model.

The [responsibility transfer requirements](docs/RESPONSIBILITY-TRANSFER.md) name
the owners of development, presentation and draft capabilities leaving Core.
Development test-project orchestration, editable source branches, checkpoints and
build execution, development preview, scenarios, layout and visual models have
been removed, together with synchronized drafts and saved view content.
This breaking upgrade uses fresh projects and storage. Old projects,
directories and history are not migration or recovery requirements. New Operations
still retain their original requests, outcomes and uncertainty.

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

Core exposes one ordinary Host per endpoint. Developer tools create disposable
project/catalog directories, launch `session` or `mcp`, address that Host directly,
and end its process explicitly. Core has no test-project manager, child Host
selector, dedicated test-project storage or child workspace navigation. Retired
CLI, frame, HTTP header and view selectors fail instead of calling the current
project. No legacy migration or test-history recovery service remains.

Core accepts existing immutable package content. It does not run a package build
recipe, maintain editable source heads or manage build directories. Package source
listing, bounded reads and immutable revision comparison remain read-only. Source
and build metadata describe package provenance; they do not authorize execution.

Core does not manage editor buffers, upload leases for drafts, scene definitions or
window layouts. Views retain only immutable bootstrap configuration, resource
context and connection identity. Owners prepare their own content before a
cooperative close; an explicit disconnect checks the observed connection and makes
no claim about saved bytes. Generic resources, declared grants and original
Operations remain the shared mechanisms. The package catalog format is version 2;
previous catalogs are rejected before mutation, with no automatic migration.

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

Request retry protection uses an explicit caller request ID and matching request,
not code similarity or inferred scientific intent. A deliberate new run uses a new
request ID and may produce a different result. This does not require deterministic
computation or promise exactly-once effects in an external system. Reconnecting to
a native job, reading its logs, committing a retained report and rerunning code are
separate actions; only the retained-report commit belongs to Core reconciliation.

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

Choose tests by the user action and failure being changed; see the
[intent-based verification guide](docs/ARCHITECTURE.md#focused-verification).
Core tests exercise public contracts with disposable projects and protocol-only
owners. Scientific workflows, Agent products and application composition are
verified in their owning repositories when that integration is selected.
See the application's [development guide](https://github.com/YuLab-SMU/Rho/blob/main/docs/DEVELOPMENT.md)
for coordinated work.
