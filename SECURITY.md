# Rho Core security boundary

Rho Core is a Unix-only Rust library and `rho-core` binary. Its implemented behavior
is specified in the [managed-run](docs/MANAGED-RUN.md), [Host and CLI](docs/HOST-CLI.md)
and [activity](docs/ACTIVITY.md) contracts; only behavior described there and
exercised by regressions is implemented. This document states the security boundary.
Responsibilities are defined in [Architecture](docs/ARCHITECTURE.md). The former
implementation and its assumptions remain on `codex/legacy-core-before-rebuild`.

The first selected deployment is a trusted, single-user local project. Core
enforces the request identity, object association and access scope of its own
managed calls, including direct programmatic calls. Native owners enforce changing
preconditions where the operation actually occurs. Model output, tool descriptions,
skills and protocol adapters cannot establish or expand authority. Already
authorized work receives no additional Rho approval.

Agents may use external tools directly within their actual environment's access
boundary. Such actions do not acquire Core's acceptance, deduplication or
continuation guarantees. Core records and locks do not isolate a shared native
resource from external actors; the owner must check its actual identity and state.

Optional protocol adapters own connection authentication, exposure, protocol
compatibility and transport limits. They depend on Core's programmatic boundary.
Authentication at an edge does not remove Core or native constraints. A local
stdio connection's trust cannot simply transfer to a network deployment.

The actual execution environment owns its process, file, network and isolation
controls. Arbitrary code may use the user's OS privileges; path containment is not
a process sandbox. If Core starts or holds processes, it must enforce the limits
and lifetime behavior it declares for those resources.

In the first managed flow the application, not the request, configures executors
as absolute program paths; a request can only name one of them. Runs inherit the
user's privileges and Core's environment. A workdir must resolve inside the
project root, which is a containment check rather than isolation. Each run gets
its own process group, held and signaled only by Core until it is reaped. A
process that leaves that group is outside Core's hold. Bounded resources reject
new work explicitly. The caller name is a namespace in a trusted single-user
setting, not authentication. The library opens no network listener. The
`rho-core serve` host listens only on a mode-0600 Unix socket in its state
directory and refuses peers whose uid differs from its own; any process of that
user may still use any caller name. See [Host and CLI](docs/HOST-CLI.md).

Activity observations are inert data. Core stores the events an adapter sends but
never executes their content or treats it as instructions, and a path in source
metadata grants no resource access. Receiving an event gives Core no control over
the external action.

Remote sharing, multi-user policies and stronger isolation need concrete
requirements and verification, rather than a general security framework or a
model's judgment.

Engineering checks exercise rejection through the direct programmatic boundary;
selected protocol edges additionally test their own authentication and limits.
Core tests run on disposable resources without model or publication credentials.
Build/test jobs cannot deploy or replace a user's runtime as an incidental step.
Agent-generated changes follow the same scoped review and verification process as
other changes; a suggested repair cannot expand execution authority or replay an
uncertain native action. There is no blanket extra approval for writes or network
access already authorized in the selected environment. See
[Engineering](docs/ENGINEERING.md#检查) for checks and delivery.

For suspected vulnerabilities, use
[private vulnerability reporting](https://github.com/YuLab-SMU/Rho/security/advisories/new).
Include the affected source or artifact, platform, impact and minimal reproduction.
Keep credentials, private project content and unredacted diagnostics out of public
reports. Historical development artifacts should not be assumed to receive fixes.
Data-handling requirements are described in [PRIVACY](PRIVACY.md).
