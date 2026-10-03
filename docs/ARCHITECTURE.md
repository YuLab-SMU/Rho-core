# Core architecture and code navigation

Core coordinates Rho's contact with the outside world: caller identity, authority,
provider routing, accepted Operations and their recorded outcomes. Owner queries
describe bounded observations. The journal records what Core accepted and settled;
it does not establish every external action or the truth of a scientific claim.

## Crate ownership

| Location | Responsibility |
| --- | --- |
| [contract](../crates/contract/src/lib.rs) | Public Host requests, Operations, observations and discovery schemas. |
| [operation](../crates/operation/src/lib.rs) | Registry, admission, idempotency, execution, cancellation and retained-result commit reconciliation. |
| [adapters/sqlite](../crates/adapters/sqlite/src/lib.rs) | Journal persistence and scoped record reads. Application settings have a separate store in this adapter. |
| [host](../crates/host/src/lib.rs) | Project ownership, launcher authority, composition and shared public calls. |
| [plugins](../crates/plugins/src/lib.rs) | Package containment, provider registration, native instances, resources and restricted views. Remaining development and presentation responsibilities are listed below. |
| [plugin-protocol](../crates/plugin-protocol/src/lib.rs) | Wire contracts between Core and native owners. |
| [plugin-sdk](../crates/plugin-sdk/README.md) | Public backend transport helpers; no Host or journal dependency. |
| [process-engine](../crates/process-engine/README.md) | Bounded process supervision shared by native callers. |
| [cli](../crates/cli/src/main.rs), [mcp](../crates/mcp/src/lib.rs), [workbench](../crates/workbench/src/lib.rs) | CLI/session, MCP and HTTP adapters to the same Host, plus application asset serving. |
| [sdk](../sdk) | Public dependency source and generated TypeScript contracts, exported as pinned snapshots. |

The `rho-plugins` crate here is Core's plugin infrastructure. Scientific plugin
implementations belong to the independent Rho-plugins repository; application
composition belongs to Rho. Core builds without either sibling checkout.

## Host entry points

`NextHost` keeps one public facade. Its implementation is divided by responsibility,
without introducing another dispatch or execution layer.

| Module | Start here when changing |
| --- | --- |
| [lib.rs](../crates/host/src/lib.rs) | Public exports, runtime ownership and capability publication. |
| [authority.rs](../crates/host/src/authority.rs) | Local identity, Core scopes and explicit launcher grants. |
| [config.rs](../crates/host/src/config.rs), [workspace.rs](../crates/host/src/workspace.rs) | Host reservation, writable assembly and explicit read-only entry points. |
| [ownership.rs](../crates/host/src/ownership.rs), [paths.rs](../crates/host/src/paths.rs) | Cooperative project lease, canonical identity and protected storage paths. |
| [ports.rs](../crates/host/src/ports.rs), [port_contracts.rs](../crates/host/src/port_contracts.rs) | Public requests and the existing operation/query/control adapters. |
| [lifecycle.rs](../crates/host/src/lifecycle.rs) | Accepted-task draining and quit preconditions. |
| [observer.rs](../crates/host/src/observer.rs), [discovery.rs](../crates/host/src/discovery.rs) | Read-only records, scoped discovery and observation composition. |
| [plugin_views.rs](../crates/host/src/plugin_views.rs) | View calls delegated through the original Host authority and operation path. |

Writable assembly always has a canonical project lease, a journal and the plugin
service. Opening the service registers infrastructure; it does not select or start
a scientific provider. The reserved entry point consumes the already acquired lease.
Read-only observers compose record queries separately, without writer ownership,
provider startup or incomplete-operation recovery.

The project lease coordinates cooperative Rho Hosts. It is not a filesystem sandbox
and does not prevent the same user or an external tool from changing project files.

## Execution and lifetime

Transport adapters supply a mechanically checked request and an established caller
context to the Host. Launcher grants are fixed authority; manifests and request
bodies cannot add scopes. The Operation gateway and selected owner then check the
original target, authority and native preconditions.

Accepted work belongs to the Host task tracker. Tasks retain the entire runtime,
including the project lease, until their own completion; losing a response does not
abandon a commit or confirm cancellation. Runtime fields keep the lease after the
registry, gateways and plugin service so teardown cannot release ownership first.
Before draining, callers stop admitting requests through every transport.

`operation.reconcile_commit` uses the retained validated result of the original
Operation. It neither replays the native action nor reconstructs external history.
Later owner observations can improve understanding without rewriting that record.

## Remaining responsibility cuts

Test-project orchestration has left Core. Source branches and builds, scenarios,
layout, visual models and edit drafts still have implementations in Core's plugin
infrastructure. Their removal is separate work governed by
[responsibility transfer requirements](RESPONSIBILITY-TRANSFER.md). Splitting Host
modules does not claim these capabilities have moved or that consumers are integrated.

The current breaking upgrade uses fresh projects and storage. Retired capability
contracts and their dedicated storage are removed rather than migrated. Operations
accepted under the new contract still keep their identity, original request,
outcomes and uncertainty.

## Focused verification

Select the affected behavior; run Cargo commands serially in this checkout.

- Host ownership, observations and disconnected work: `cargo test -p rho-host --test plugin_workspace --test observer --test plugin_restart --test plugin_view_delegation --locked`.
- Operation controls and native lifecycle: `cargo test -p rho-host --test plugins --locked`.
- Actual CLI/MCP callers and read purity: `cargo test -p rho-cli --test headless --test connection --test query_purity --locked`.
- Independent binary and source closure: `cargo build --locked`, then `node scripts/check-boundaries.mjs`.

These are Core boundary checks with disposable projects and minimal callers.
Application assembly, consumer SDK refresh and scientific owner acceptance are
separately selected work. See [README](../README.md) for launch and SDK export usage.
