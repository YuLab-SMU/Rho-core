# Rho plugin protocol v1

This package contains public TypeScript definitions and JSON Schemas. It has no
runtime dependency on Rho, React, a Studio singleton or a project database. The
Rust source of these definitions is the independently packageable
`rho-plugin-protocol` crate. Other languages can implement the same JSON protocol.

`schema/manifest.json`, `schema/archive.json`, `schema/rpc.json`,
`schema/resource-transfer-*.json` documents define the wire shapes.
Semantic checks (identity formats, references, scope, schema compilation, digests,
limits and lifecycle) also run in the receiving owner. JSON Schema alone does not
grant authority or establish runtime availability.

`Ready.features` is an optional set of up to 16 bounded protocol-extension names.
Hosts ignore unknown names and send an optional message only when its feature was
advertised by this exact instance. `pending_cancellation_v1` supports Host-only
`PreparePendingCancellation` / `PendingCancellationPrepared`: the owner atomically
fences a still-waiting original invocation, the Host records cancellation in the
same Operation journal, then its normal Cancel signal reaches the owner. A fence
alone is neither cancellation success nor a terminal result. Lost replies and
journal failures preserve the original fence and identity for explicit retry.
The existing manifest cancellation contract does not change. See the
[backend SDK](../../crates/plugin-sdk/README.md) for owner obligations.

## Package identity

`plugin.json` declares purpose, display version, exact dependencies, required
capabilities, views, context readers and the optional native entrypoint. There is
no bundled/trusted/origin permission flag. All entrypoints live in `dist/`.
First-party source paths, dependency lockfiles and build instructions are required.
A declared build command is metadata for external tools. Core never executes
it or installs a toolchain.

A `PluginRevision` hashes its parent, manifest and source inventory. A
`BuildArtifact` separately hashes its source revision, target and artifact
inventory. Display versions are labels, never binding keys. `PluginInstance`
fixes the revision, artifact, project, principal and configuration. Several
instances of the same plugin may use different revisions concurrently.

The local `.rho-plugin` archive is UTF-8 JSON with format version 1. It includes
one immutable source revision, zero or more build artifacts and content-addressed
base64 blobs. Paths are normalized relative POSIX paths, and symlinks, special
files, traversal, case collisions, undeclared blobs and digest mismatches are
rejected. Limits are 8,192 file entries and 256 MiB decoded package bytes.
Archives can be unbuilt source checkpoints; activation needs a complete artifact.

## Archive transfer ports

The view-channel `download_archive` intrinsic takes a `PluginArchiveReference`
and a plain filename, using that view's declared `plugins.archive_read@1` grant
intersected with current parent authority. Native admission validates the original
scope and byte reference; the containing browser separately checks a user gesture,
the complete bytes and live authority before requesting a download. Its response
does not prove local-file saving. Closure preparation refuses
this action. It creates no scientific Operation.

Ordinary plugins, connected CLI and MCP use the same `plugins.archive_*@1`
ports. `archive_stage` and `archive_discard` are Controls with `plugins.write`; `archive_import` is an
Operation with that scope. Progress, inspection, chunk reads and original receipt
queries use `plugins.read`; `archive_export` is an Operation with `plugins.read`.
Each reference binds an opaque archive ID, SHA-256 and exact encoded byte count.
Native project/principal identity supplies visibility; requests cannot select a
filesystem path, different principal or runtime resource owner.

Upload exact 65,536-byte chunks at aligned offsets, with a shorter final chunk.
Identical chunks may be retried in any order; changed content at a staged range is
rejected. `archive_progress.complete` means only that all ranges are retained.
`archive_inspect` verifies the full content digest and package contract before
returning metadata. `archive_import` revalidates and atomically installs the package
with its original transaction receipt. Neither staging nor inspection installs;
import never activates, builds or executes a package.

`ExportPluginArchive` specifies exact source and a sorted unique artifact list;
use an empty list for source only. Export fixes downloadable bytes in the scoped
transfer store and returns a `PluginArchiveReceipt`. Read at most 65,536 bytes per
`archive_read`, checking the unchanged reference, offsets and final digest.
Preparing or reading an export does not attest to a local file being saved.

The encoded archive limit is 374,691,157 bytes, with the package's separate
256 MiB decoded-content limit. Each principal may retain up to sixteen transfers
reserving twice that encoded limit; the repository allows 128 transfers reserving
eight times the limit. Upload declares and reserves its full length on the first
chunk. Explicit mutations collect expired unheld transfers after 24 hours;
queries neither renew leases nor collect bytes. Explicit `archive_discard` can
remove an unheld transfer to free capacity; it preserves package content and
original receipts and refuses accepted recovery bytes. Accepted unresolved imports and
exports keep their exact transfers and source revisions protected. Confirmed
original settlement releases protections and renews the download lease.

Persist the original request ID and arguments before invoking. After a lost reply,
inspect that Operation and `archive_receipt` with its original operation ID.
A catalog receipt is evidence of one atomic catalog transaction, not authority to
rewrite an uncertain Operation or replay it. Recover a durable original commit
through the regular Operation ports; `plugins.reconcile_references` can release
protections only after the original result is certain. Removed package content is
not reinstalled by requesting an old successful Operation again.

## Host lifecycle ports

Active Hosts expose package/lifecycle DTOs from this package through their ordinary
Query and Operation ports. `plugins.repository` identifies the store and backend
target; `plugins.list`/`plugins.inspect` describe immutable installed content.
`plugins.activate` takes `ActivatePlugin`, and `plugins.resolve` returns the exact
`ProviderBinding` used in `PluginRequest`. `plugins.release` drains that selected
instance. A stored `PluginInstanceObservation` does not establish process liveness;
check `observed_in_this_host` together with its lifecycle state. Instance pages
are scoped to the current project and original principal before pagination.

Immutable packaged source is available through `plugins.source_tree` and
`plugins.read_source` queries (`plugins.read`). `ListPluginSource` /
`PluginSourcePage` pages at most 100 file identities. `ReadPluginSource` /
`PluginSourceChunk` addresses exact revision/path bytes, returns at most 65,536
binary-safe bytes and verifies the full stored file digest. Artifact paths are
not source reads. `plugins.compare` compares two installed immutable revisions.

Core has no editable source branches, checkpoint calls, build execution or build
queue. External tools or Studio own those workflows. Core checks existing package
identities and artifacts; source build recipes are descriptive metadata only.
Removed development contracts are absent from the SDK inventory.

### External development testing

The Core protocol has no test-project lifecycle or child Host selector. A developer
tool creates a fresh project and catalog, starts an ordinary `session` or `mcp`,
imports a validated immutable package and uses the public instance and Operation
ports. The tool owns the directories and process lifetime. The Host uses its
normal project/caller scope, launcher authority, journal and native lifecycle.
Queries never create or restart a Host. New Operations retain their original
identity and uncertainty; independent projects use independent journals.

Old test-project directories and history are outside this breaking upgrade's
contract. There is no compatibility selector, migration or historical child
journal service. SDK and product consumer integration are separate owner tasks.

`plugins.project_coverage@1` and `operation.project_coverage@1` accept
`ProjectReadCoverageArguments` (`{}`) and return `ProjectReadCoverage` in the query
envelope's `data`. They require `project.references.read` together with
`plugins.read` or `operation.read`, respectively, through the same declared grants
as other Host queries. The only field, `all_visible`, reports whether every
recorded instance or operation in the Host's project is visible to the original
principal. Failed and released instances still count. No foreign identity,
configuration, content or count is returned. An unavailable response or
`all_visible:false` means scoped pages cannot establish complete coverage. This
metadata neither freezes records nor proves that a native resource is unused;
owners must separately inspect their references and native preconditions.

`manifest.requires` declares mandatory capability grants. `optional_requires`
declares capabilities that an activation may explicitly select using
`ActivatePlugin.optional_capabilities`. Omitting the selection grants none of
those optional capabilities, even if their providers are available. Selected
versions and scopes must match this exact revision; Host admission checks every
selected handler and the original caller's authority. Unknown, repeated,
unavailable or unauthorized selections fail before activation. Configuration,
opening a view and later provider installation cannot change these frozen grants.
A view receives its instance's selected grants only when its opening caller can
delegate all of them; each subsequent call still intersects current parent scopes.

`host.core_contract@1` accepts `HostCapabilityArguments` with an exact capability
key and returns `HostCapabilityContract` in the ordinary query envelope. Declare
`plugins.read` to inspect the native Host port's project, kind, description, input
schema and required scopes. Metadata inspection does not grant execution scopes,
start a provider or create an Operation. Missing ports and dynamically contributed
plugin capabilities are refused. Inspect contributed capabilities through their
immutable package manifest and retain their exact `ProviderBinding`; a plugin
cannot become a native port by choosing a similar name or domain.

The recovery CLI and active Host use `plugins-v1` beside the configured database,
with an explicit CLI `--store` override. Importing or observing a revision never
activates it. Lifecycle operations use stable caller request identities and the
same authoritative Operation journal as contributed capabilities. The official
MCP connection updates its tool catalog after provider publication or failure;
page cursors cannot cross changed catalogs. The UI SDK remains separate from
these public type definitions.

`workspace.paths@1` takes `{}` and returns `WorkspacePaths` under `project.read`.
The Host supplies its normalized project root and protected storage boundaries;
neither arguments nor plugin configuration can supply those paths. A native
backend declares this requirement and uses a reverse `HostCall` with an active
parent that holds the scope. The reply is the ordinary Host query envelope, with
the paths in `data`. Preserve lexical paths and resolved aliases, including
nonexistent sidecars. Limits are 256 paths, 4096 UTF-8 bytes per path and 128 KiB
for the encoded path list. This query neither scans files nor starts a runtime,
and does not extend the initialization message or provide an OS sandbox.

## Runtime protocol

Each control message is one UTF-8 JSON `RpcFrame` of at most 1 MiB. A connection
has a Host-issued identity and an independent monotonically increasing sequence
in each direction, starting at one. Messages bind an instance and request. A
reconnected process receives a new connection identity; old messages are stale.
Stdout is reserved for control frames, stderr for logs. Large bytes travel through
bounded owner-scoped `ResourceReference` reads, never inline control payloads.

Initialization must finish before contributions become visible. A process may
return a proposed `PluginCommitPlan`; only the core Operation owner may validate
and commit it. Accepted calls retain their exact provider and native target.
An unavailable process, closed connection or cancellation acknowledgement with
`confirmed:false` cannot mean success or confirmed cancellation. Reconnection
does not authorize replay. Reverse calls require the delegated instance grants.

Each capability declares 1–16 valid scientific input `examples` for discovery.
Host callers submit `PluginRequest`: `binding`, `arguments`, and owner-defined
`preconditions`. A binding names the capability version, exact instance/revision/
artifact, project and optional native target. The Host derives the authenticated
principal and scopes; a backend cannot supply or expand them.

An operation may name a same-package query as `preflight`. That query receives
`PluginPreflightRequest` and must return a complete `PluginPreflightResult` with
normalized arguments, native target and owner qualification. It must not start
work or perform effects. A supplied target cannot be changed by preflight. The
Host freezes the result before Operation admission and sends the qualification
as `PluginCall.owner_context` along with the original Operation ID. Queries carry
no Operation ID. Duplicate original requests read their saved record without
running preflight again.

`kind: "control"` contributes a transient handler for an existing owner request.
Use `Control` / `ControlResult` frames with no new `operation_id`; Host callers
use the same `PluginRequest` binding envelope through the Control port. Native
request/answer identities and preconditions remain owner-defined. The Host limits
arguments/results to 256 KiB, redacts validation/native errors, and creates no
Operation, receipt, event, recovery candidate or resource-transfer parent. Only
explicitly selected queries and controls can reach a draining instance. Read the
pending native request after an unacknowledged answer; never replay automatically.

After the original journal reaches a terminal state, the Host sends
`OperationSettled(OperationSettlement)` to that exact native instance. The payload
contains only the original ID, binding and terminal outcome, not inputs or result
bytes. The owner applies matching scheduling cleanup idempotently and echoes the
exact payload as `SettlementAcknowledged`. Neither public Control nor reverse
`HostCall` exposes this notification. It has no resource-transfer parent. Never
advance the next queue item merely because a native CommitPlan was returned.
The Host preserves the protecting operation reference until acknowledgement;
lost acknowledgement cannot alter the committed scientific result. Explicit
`plugins.reconcile_references` uses original journal proof to resend an unanswered
settlement, with its original request identity and a fresh ordered frame sequence.
An exact late duplicate acknowledgement is accepted within the bounded 128-request
transport history. Unrelated identities and unsolicited replies fence the instance.

The core bridge wraps native recovery as `plugin_owner_recovery`; transport and
contract failures retain their original candidate under a distinct boundary
recovery. Proposed resource evidence is accepted only after the resource owner
verifies its identity, visibility and digest. Active Hosts compose this resource
owner; a standalone bridge with `NoPluginResources` deliberately rejects evidence.
No verifier fetches arbitrary remote data to satisfy a plugin's claim.

The optional initialization `resource_channel` is an ephemeral per-instance Unix
socket and credential. Data headers are BE u32 lengths plus at most 16 KiB JSON;
`ResourceTransferRequest` and `ResourceTransferResponse` define the language-neutral
protocol. Raw upload/read bytes use this separate socket, not control/stdout.
Uploads inherit the active parent's exact instance/project/principal, verify the
declared length and SHA-256, and become visible only after complete atomic retention.
A query may retain observation bytes without committing scientific facts. Backend
channels read only their own instance's resources; cross-owner reads require a
granted Host query. See the public [backend SDK](../../crates/plugin-sdk/README.md)
for framing and streaming examples.

`resources.list` takes `ResourceList` and returns a scoped `ResourcePage`;
`resources.inspect` takes `ResourceInspect` and verifies the full reference;
`resources.read` takes `ResourceRead` and returns `ResourceChunk`. Host queries
require `resources.read` plus original project/principal visibility, even after
a provider is released. Read offsets and cursors count bytes, not characters.
Limits are 256 MiB per upload, 256 KiB per read, four concurrent transfers per Host,
512 MiB retained per instance and 2 GiB / 16,384 entries per store. A failed or
partial upload is not evidence; lost acknowledgement does not delete complete
retained bytes. Identical uploads resolve to the same immutable resource identity.

## View connections

UI views use isolated iframes and an instance-bound channel. Core binds the exact
instance, contribution, immutable bootstrap configuration, optional resource,
project, principal and authenticated window. `views.open` creates this identity;
`views.reconnect` takes only its retained view ID and requires the original instance
to be active. Neither action restores content, restarts a provider or replays work.

`views.inspect` returns public metadata. Private `views.connection` material stays
with the containing shell; a plugin cannot query it even with a declared grant.
Assets are restricted to the exact immutable artifact. View messages carry no Host
bearer, caller-selected principal or project root. Calls use selected declared
grants, intersected with the current parent authority.

`views.caller@1` takes `{}` under `plugins.read` and returns `PluginViewCaller`
([schema](schema/view-caller.json)), including across backend delegation. Only an
admitted caller without a view returns `null`; a closed, closing, missing or replaced
calling view fails. It opens nothing, accepts no selector and discloses no credential.
`views.presence` observes native attachment for one known caller-visible view,
without establishing browser responsiveness or authority for a later action.

Default `views.close` asks every registered participant to prepare through its
owner's declared ports. `prepare_close` carries the original close Operation ID,
with no content version. Core checks authority for all preparation calls; it does
not know which call saves content. After all participants confirm, new mutations
are fenced and the connection closes atomically with its package reference.
Refusal, a participant ending or the fixed 15-second preparation deadline keeps
the connection open. At most 32 participants may register per view; exceeding this
fixed memory bound refuses registration. These limits are not per-call settings.

Explicit close mode `{kind:"disconnect", connection:<ConnectionId or null>}` must
match the currently observed connection, using `null` only for a detached record.
It acknowledges disconnection, not saved content or stopped native work.
`views.release_renderer` is a private shell control for an ended participant,
requires the original connection credential, and never confirms preparation.
Closing a view, ending its transport and releasing its backend are separate actions.

Core has no `windows.*`, `scenarios.*`, development preview, synchronized draft
ports, visual tree model, `views.update` or `set_state`. A view has no content state
or `state_schema`. Owners use their own content models and persistence, with the
existing resource and Operation boundaries. The catalog format is 2; previous
formats are refused before mutation. Consumers must explicitly select this breaking
SDK contract; Core supplies no compatibility adapter or old-data migration.

Native backends are trusted local code. These are channel and lifetime boundaries,
not an OS sandbox.

## Contributed context

The four `schema/context-search.json`, `schema/context-page.json`,
`schema/preview-context.json` and `schema/context-preview.json` files are standalone
contracts for contributed queries.

`ContextSearch` / `ContextPage` and `PreviewContext` / `ContextPreview` describe
bounded read-only discovery and preview. A `ContextContribution` declares its own
search and preview queries; consumers resolve the exact active provider. A
`ContextReference` retains that provider, contribution and an opaque
owner-defined selector; it names no window. Project, caller and authority come
from the trusted call context, and a view caller's window restriction stays in
the Host-held call scope. The owner must revalidate the selector's native
identity/version before preview; references grant no scope and do not retain bytes.
Page cursors are owner-defined, with at most 20 items per response. Preview text
is plain text bounded to 64 KiB, with explicit truncation, bounded presentation
data and at most eight resource references. Scientific inclusion choices and
source freshness remain owner semantics. A read must not activate a provider,
flush an editor, recover work or perform scientific writes.
