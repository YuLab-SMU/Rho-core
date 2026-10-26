# Working on Rho Core

Rho Core supplies managed local execution for Agents and retains selected external
Agent activity. Read [README](README.md), [Architecture](docs/ARCHITECTURE.md),
[Engineering](docs/ENGINEERING.md) and [Progress](docs/PROGRESS.md) before selecting
work. The contracts describe what is built: [managed runs](docs/MANAGED-RUN.md),
[information model](docs/INFORMATION-MODEL.md), [Host and CLI](docs/HOST-CLI.md) and
[activity observations](docs/ACTIVITY.md). Observing an external action does not make
it a managed run.

## Fixed boundaries

These protect callers' work and the truth of what is reported. They are not open to
experiment; the reasons are in [Architecture](docs/ARCHITECTURE.md#固定的边界).

- Core owns its programmatic boundary, the runs it accepts and the activity it
  receives. Protocol edges, Rho-plugins and the Rho application depend on Core; Core
  does not depend on protocol types, client behavior, plugins, models or sibling
  sources. Agents may use existing CLIs, SDKs and MCP servers directly; nothing has
  to be registered or wrapped as a Rho plugin. Domain semantics stay with their owners.
- Callers hold request identities. A duplicate, including a concurrent one, returns
  the original acceptance; changed content under the same identity is refused. A
  lost reply, timeout, restart or unreadable record never authorizes replay or a
  new identity.
- Acceptance, dispatch, native effects, retained output, cancellation, confirmed stop
  and release are separate facts. Unknown stays unknown, with its basis. Script
  composition is not a transaction; cancellation is not rollback.
- Guarantees cover only what Core accepted, within the stated process lifetime,
  retention and platforms. External actions never acquire Core acceptance,
  deduplication or causal history.
- Core checks its own identity and scope, including on direct calls; native owners
  check changing preconditions where the operation happens. Model output and
  adapters cannot expand authority. Already authorized work gets no extra Rho
  approval. Path containment is not a sandbox.
- Reads are bounded observations and never start or recover a runtime.
- Every resource Core holds is bounded: name the protected resource, default,
  configuration and exceeded-limit behavior. Never truncate silently.

## How work advances

Work advances from results, not from rules written ahead of them
([Engineering](docs/ENGINEERING.md)). Start from a real caller's action, do it first
with the existing tools and name the gap Core would close. Make the smallest change
in the actual owner, run the real case on disposable resources, look at the actual
result (dispatch counts, bytes, records, the Agent's actions), find the cause in its
owner and run the same case again. Explore quickly while a question is open. A
guarantee is claimed only after fault experiments that cover it; regression tests
harden what results have confirmed. API shapes, storage layout, limit values,
internal mechanisms, test organization and process stay open and change when a
reviewed result calls for it. Do not write interfaces, checklists, stage plans or
prohibitions for capabilities that no real caller has exercised.

## Verification

- Run the [README](README.md#构建与检查) commands in sequence; never run Cargo
  builds/tests concurrently. Core checks need no model credentials, sibling sources
  or user runtime. Only executed checks count, each for its recorded scope.
- Reproduce a failure before fixing it and keep a focused regression. Never delete
  failures, change pass criteria or rerun until green.
- Use disposable projects, state directories and owned Hosts. Never start or restart
  a user's Host or R session for verification.
- Documentation changes need local link/anchor checks and rendered inspection of the
  changed pages. `docs/MANAGED-RUN.md` and `docs/ACTIVITY.md` are compiled into the
  binary, so changing them also needs the Cargo checks.
- CI is deferred by the user's decision of 2026-10-10 until the user selects it.

## Records and delivery

- Keep one record per selected task under `docs/work/` and the current summary in
  [Progress](docs/PROGRESS.md), as described in
  [Engineering](docs/ENGINEERING.md#记录). Use actual clock times; unknown durations
  stay unknown.
- Inspect Git status and preserve unrelated changes. Commit coherent work on `main`.
  Before an authorized push, inspect the actual remote, branch and account, and
  verify the remote head afterward. Changing sibling repositories, publishing a
  release, installing a binary or replacing a user's Host is never a side effect of
  work here.
- The legacy implementation is on `codex/legacy-core-before-rebuild`; consult it for
  a concrete requirement or failure. Its APIs, tests and formats are not requirements,
  and ignored legacy `crates/`, `sdk/` or `target/` contents are not evidence.
- Keep credentials and private material out of source, records and diagnostics. Do
  not collect hidden reasoning, or all external actions, by default.
