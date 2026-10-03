# Core architecture and code navigation

Core coordinates Rho's contact with the outside world: caller identity, authority,
provider routing, accepted Operations and their recorded outcomes. Owner queries
describe bounded observations. The journal records what Core accepted and settled;
it does not establish every external action or the truth of a scientific claim.

The [mission and implementation plan](MISSION-AND-PLAN.md) records the durable
design direction and work selection criteria. This page describes current code
and contracts; the plan's candidate mechanisms are not automatically Core APIs.

## Crate ownership

| Location | Responsibility |
| --- | --- |
| [contract](../crates/contract/src/lib.rs) | Public Host requests, Operations, observations and discovery schemas. |
| [operation](../crates/operation/src/lib.rs) | Registry, admission, idempotency, execution, cancellation and retained-result commit reconciliation. |
| [adapters/sqlite](../crates/adapters/sqlite/src/lib.rs) | Journal persistence and scoped record reads. Application settings have a separate store in this adapter. |
| [host](../crates/host/src/lib.rs) | Project ownership, launcher authority, composition and shared public calls. |
| [plugins](../crates/plugins/src/lib.rs) | Package containment, provider registration, native instances, resources and restricted views. |
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

## Responsibility boundary after the breaking upgrade

Core no longer owns test-project orchestration, editable source branches,
checkpoints, build execution, development previews, scenarios, layouts, visual
models, synchronized drafts or saved view content. Their implementation, public
contracts and dedicated storage have been removed under the
[responsibility transfer requirements](RESPONSIBILITY-TRANSFER.md).

The remaining view record binds an immutable instance, contribution, bootstrap
configuration, optional resource and authenticated window. It is connection
metadata, with no buffer, content version or history. Close preparation coordinates
registered participants and their original Operation; the owner decides what its
preparation requires through declared ports. All confirmations seal new actions.
An explicit disconnect checks the observed connection identity and does not attest
to saved content or native cleanup. Renderer destruction cannot stand in for a
participant's confirmation.

The breaking upgrade uses fresh projects and storage. Catalog format 2 rejects a
previous catalog before any mutation; no migration, compatibility or historical
product-data recovery path remains. Operations accepted under the new contract
keep their identity, original request, outcomes and uncertainty. Consumer SDK
refresh and product integration remain separate owner tasks.

## Focused verification

Start with the concrete user action, its observable result and a plausible failure.
Reuse the smallest existing test that distinguishes that failure from correct
behavior. Add a case only when it covers a missing outcome or trust boundary;
changing a file or adding a field alone does not require another test.

Examples below select different responsibilities, not a mandatory combined suite.
Run Cargo commands serially in this checkout.

| Changed intent | Select the relevant check |
| --- | --- |
| An external caller reads and acts without a window, retries an original request, or needs a launcher grant | `cargo test -p rho-cli --test headless --locked` |
| A query must leave storage and a live writer alone | `cargo test -p rho-cli --test query_purity --locked` |
| A package is inspected without execution, round-trips unchanged, or refuses an incompatible catalog | `cargo test -p rho-plugins --test package_repository --locked` |
| A view may close only after participant confirmation | `cargo test -p rho-host --test plugins close_requires_owner_preparation --locked` |
| Accepted work survives disconnect, cancellation or lost native settlement | Select the matching test in `rho-host --test plugins` or `--test plugin_delegated_operations` |
| A validated result cannot commit, then must reconcile without native reexecution | `cargo test -p rho-sqlite --lib commit_recovery_tests --locked` |
| A shared transport changes framing, authentication or response handling | Select `rho-cli --test session`, `--test connection`, `rho-mcp`, or `rho-workbench` according to the changed edge |

Assert observable effects and retained identities: actual owner execution counts,
unchanged bytes after refusal, original records after retry, and an open connection
when preparation is missing. Do not keep inventories of deleted capabilities,
private table names or exact diagnostic wording as substitutes for these results.
SQL fault injection is useful when it exposes a commit failure that a public caller
must handle; merely counting implementation tables is not acceptance.

Use virtual time for a timer-only timeout, and ordinary time for native process or
I/O cooperation. Tests must still pass through the timeout and inspect its outcome.
A protocol fixture proves Core authority, dispatch and settlement; it does not prove
a scientific conclusion or an Owner's native behavior. The former ignored Agent,
real-R and Files product suites are retired from Core; selected scientific and
composition acceptance belongs to the corresponding source owner or application.
No consumer SDK refresh, domain migration or composition is implied by a Core pass.

For changed production dependencies or assembly, build the independent binary and
check source closure with `cargo build --locked` and
`node scripts/check-boundaries.mjs`. For test-only edits, run the affected checks;
do not rebuild the product or run unrelated native workflows to increase a count.
Use prior passing evidence when it still covers unchanged behavior.

For UI SDK changes, compile and test the public browser handshake independently:

```sh
npm exec --yes --package=typescript@5.9.3 -- tsc --target ES2022 --module NodeNext --moduleResolution NodeNext --lib ES2022,DOM --strict --rootDir sdk --outDir target/sdk-test sdk/plugin-ui/index.ts
RHO_UI_TEST_BUILD="$PWD/target/sdk-test" node --test scripts/tests/plugin-ui.test.mjs
```
