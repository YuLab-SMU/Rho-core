# Working on the Rho Core rebuild

`main` is the starting point for a new Core. The former implementation is retained
on `codex/legacy-core-before-rebuild`. Read [README](README.md),
[Mission and plan](docs/MISSION-AND-PLAN.md), [Architecture](docs/ARCHITECTURE.md)
and [Rebuild workflow](docs/REBUILD.md) before selecting work. These documents define
target behavior; they do not claim that a new runtime already exists.

## Scope and design

- This repository owns the generic local tool runtime, its public contracts and
  transport adapters. Scientific semantics belong to their real adapters; the
  application, model execution and Agent orchestration have separate owners.
- Start with a concrete caller action and an observable completion condition.
  Choose the smallest implementation that satisfies it. Old crate boundaries,
  APIs, catalogs, tests, SDK exports and storage formats are not requirements.
- Use maintained libraries, ordinary files, Git and native task handles first.
  A new shared abstraction must solve the same demonstrated gap in more than one
  selected flow. Do not build a framework to accommodate hypothetical consumers.
- Tools expose understandable descriptions and bounded structured results.
  Skills, loops and graphs guide callers; they do not prescribe Core modules,
  persistent state or mandatory discovery steps.
- Read real state on demand. Preserve source, scope, known observation time and
  limitations when they matter. Do not substitute model context for a fresh read
  or silently present current bytes as a retained historical input.
- Protect concrete effects: target containment, conflicting writes, explicit
  retry identity and native execution conditions. A lost reply does not authorize
  rerunning an action. Cancellation, disconnection and rollback remain distinct.
- First deliver a trusted single-user local flow. Role systems, multi-user
  deployment and general approval policies are later requirements. Already
  authorized work receives no additional Rho approval. Local execution is not an
  OS sandbox; scope declarations cannot claim isolation they do not enforce.
- Define every resource bound by the resource protected, configuration and
  behavior on exhaustion. Avoid silent truncation, unbounded queues and logging
  every read. Record only what a selected continuation or effect requires.

## Implementation and verification

Inspect Git status and preserve unrelated changes. Add new source on `main` in
small milestones. Consult the archive only for a concrete requirement or failure;
do not copy its subsystem map or revive a compatibility layer. Use fresh projects
and storage. Do not migrate or export legacy catalogs as part of this rebuild.

Core must build and be testable without Rho or Rho-plugins source. Use a minimal
public-contract fixture for Core tests; scientific results require the real owner
in a separately selected integration. Do not update sibling repositories, their
SDK snapshots or application locks to close a Core milestone.

Run focused behavior checks; never run Cargo builds/tests concurrently. Report
only checks actually executed. Documentation changes need local link/anchor checks
and rendered inspection. Never start or restart a user's Host or R session for
verification. Old binaries and old passing tests do not verify the new Core.

Commit coherent work. Keep design documents on `main`; archive source stays on its
branch. Creating the archive or committing locally does not publish it. Before an
authorized push, inspect the actual remote, branch and account and verify the
remote head afterward. Do not change sibling source, publish a release or install
an artifact as a side effect of this repository's work.
