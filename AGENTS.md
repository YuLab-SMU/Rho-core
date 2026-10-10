# Working on the Rho Core rebuild

`main` is the starting point for a new Core. The former implementation is retained
on `codex/legacy-core-before-rebuild`. Read [README](README.md),
[Mission and plan](docs/MISSION-AND-PLAN.md), [Architecture](docs/ARCHITECTURE.md)
and [Rebuild workflow](docs/REBUILD.md) before selecting work. These documents define
target behavior; they do not claim that a new runtime already exists.

Use [Engineering guide](docs/ENGINEERING.md) to select the first flow, describe its
contract, expose necessary facts and scope regression checks. The architecture's
core principles govern expansion; this guide does not authorize a new framework.

Read [Engineering progress](docs/PROGRESS.md) when selecting or resuming work.
Maintain one selected-task record under docs/work/ using the
[recording rules](docs/ENGINEERING-MONITORING.md). Record meaningful work, actual
timestamps, duration coverage and verification; historical commit times do not
establish work duration. Update the task and current summary at checkpoints and
closure, without introducing a runtime monitoring subsystem or automatic follow-up.

## Scope and design

- This repository owns Core's programmatic boundary and the selected managed
  execution and continuation guarantees. Optional protocol adapters depend on
  Core; Core must not depend on protocol types or client-specific behavior.
  Logical separation does not require separate repositories, services or crates.
- Agent harnesses read documentation, select tools, compose code and manage model
  context. They may use existing CLIs, SDKs and MCP servers directly. Do not require
  every external capability to be registered, proxied or packaged as a Rho plugin.
  Domain and native semantics remain with their actual owners.
- Start with a concrete caller action and observable completion conditions. Try
  maintained tools first and identify the managed guarantee they cannot provide.
  Uniform naming, a common catalog or possible future consumers do not justify
  adding a subsystem. Old APIs, tests, SDKs and formats are not requirements.
- Core describes its own capabilities, with documentation and examples available
  on demand. Do not predefine an endpoint for each scientific method, require a
  discovery pipeline or force every native result into one universal model.
  Compare focused tools and code composition on actual tasks.
- Reuse ordinary functions, files, Git, maintained libraries and native execution
  handles. Add shared mechanisms for a demonstrated gap, not an arbitrary count
  of flows. Do not recreate a capability already supplied by the execution owner.
- Scope every guarantee to the managed actions, process lifetime and retention
  actually covered. External actions may be observed later; never synthesize Core
  acceptance, deduplication or causal history for them.
- Native owners check changing object identities and preconditions at the actual
  operation boundary. Core enforces its own request identity and access scope,
  including direct calls. Model output and another adapter cannot expand either.
  A lost reply does not authorize replay. Script composition does not imply a
  transaction or permission to rerun partially executed code.
- Return known results and limitations honestly. Current reads are not historical
  inputs. Cancellation, confirmation, release and rollback remain distinct.
  Retain only information needed by selected continuation or effect guarantees;
  ordinary reads do not require logging or restarting a runtime.
- First deliver a trusted single-user local flow without another Rho approval for
  already authorized work. Authentication and transport limits belong to the
  selected edge; native and Core checks still apply. Isolation belongs to the
  actual execution environment. Path validation is not an OS sandbox.
- Bound resources actually owned by the implementation: identify the protected
  resource, default, configuration and exhaustion behavior. Include processes
  started or held by Core. Avoid unbounded queues, silent truncation and a global
  quota or monitoring system for the Agent's entire environment.

## Implementation and verification

Inspect Git status and preserve unrelated changes. Add new source on `main` in
small milestones. Consult the archive only for a concrete requirement or failure;
do not copy its subsystem map or revive a compatibility layer. Use fresh projects
and storage. Do not migrate or export legacy catalogs as part of this rebuild.

Core must build and be testable without Rho or Rho-plugins source or an embedded
model. Use a minimal native-execution fixture through the programmatic boundary;
scientific results require the real owner in a separately selected integration.
Protocol servers, a provider framework and an exported SDK are not milestone
requirements without a real consumer. Do not update sibling repositories, SDK
snapshots or application locks to close a Core milestone.

Run focused behavior checks; never run Cargo builds/tests concurrently. Report
only checks actually executed. Documentation changes need local link/anchor checks
and rendered inspection. Never start or restart a user's Host or R session for
verification. Old binaries and old passing tests do not verify the new Core.

Deliver callable examples and necessary fact retrieval with each selected flow.
Concurrent duplicate requests must be covered by behavior checks, not just a
sequential retry. Reproduce actual failures before fixing them and retain focused
regressions. Distinguish accepted requests, native effects and result retention;
missing or unreadable records never authorize replay. Use process-lifetime claims
until restart guarantees are implemented and tested.

Introduce CI with actual source and reproducible commands. Core checks must not
require model credentials, sibling sources or a user runtime. Agent evaluation,
protocol integration and native acceptance have separate owners and triggers.
Diagnostics are scoped and bounded; do not collect hidden reasoning, all external
actions or raw request/output content by default. Agent-authored repairs and
contract changes use the normal verified change process, without implicit deploys.

Commit coherent work. Keep design documents on `main`; archive source stays on its
branch. Creating the archive or committing locally does not publish it. Before an
authorized push, inspect the actual remote, branch and account and verify the
remote head afterward. Do not change sibling source, publish a release or install
an artifact as a side effect of this repository's work.
