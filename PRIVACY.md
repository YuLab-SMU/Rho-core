# Rho Core data handling

This document describes how Rho Core handles data. Implemented behavior is listed
below and in the [managed-run](docs/MANAGED-RUN.md) and [activity](docs/ACTIVITY.md)
contracts. The old policy remains with its archived source.

Core retains data only for its selected guarantees. For managed runs these are
request identities and scope, actual arguments and targets, execution-owner
associations, native handles, known results or result references, and necessary
diagnostics; for activity observations, the events an adapter chooses to send. Arguments
and output may contain code, private data or program-printed credentials; bounded
storage does not imply automatic redaction. Exact bytes are retained only for an
identified continuation or review need. Ordinary reads do not create a query
history, conversation memory or a record of every environmental change.

External clients may operate CLIs, SDKs, MCP services and execution environments
directly. Their data handling belongs to those tools and environments. A later
Core observation does not create a managed history of the external action or prove
its cause. Core makes no whole-environment audit or retention guarantee.

The new Core requires no model provider, stores no model credentials, and owns no
Agent conversations or hidden reasoning. Clients decide what to send to models.
Intermediate data may stay in their execution environment and be filtered or
summarized before retrieval; it need not pass through Core or model context.
Programs may contact networks under their actual behavior and permissions; local
execution alone is not a network sandbox.

Each capability must describe its storage locations, capacity, retention, access
boundary and deletion behavior. Expired identities must not silently become new
actions under a deduplication promise. Cleanup protects running work and material
needed by retained references. Deleting a record cannot undo a native effect.
Credentials must not enter source control or ordinary diagnostics. Telemetry,
uploads and publication require explicit product decisions, not an implicit
consequence of adding a tool or transport.

Within the selected caller's access scope, necessary managed facts are available
on demand: original request, owner/object association, known execution state,
uncertainty, result location and guarantee limits. A missing record, failed read or
expired reference is reported explicitly; retrieval does not imply exposing other
callers' data or unlimited history. Diagnostic logs reference these facts rather
than copying full arguments or outputs by default. Observation timestamps and
native event times remain distinct; unknown provenance is not invented. Any
optional metric or trace export declares its content, destination, access and
retention before use.

The selected managed flow retains accepted requests and independent facts across
Core lifetimes in `state_dir/core.sqlite3`, including SQLite's WAL while present.
Script snapshots and bounded output chunks are immutable content-addressed files
under `blobs/sha256/`; data files use mode 0600, without redaction. `runs/` contains
only per-operation observation locks. Dropping Core does not delete its records.
Records do not expire, and uncertain native completion never authorizes automatic
replay or deletion. `forget` requires confirmed completion/release and saved facts;
its committed transaction explicitly releases the request key and artifact
references. Unreferenced blobs are collected, except those pinned by unsaved
in-memory output. Logical deletion is not secure erasure of database pages, WAL,
filesystem blocks or backup copies. The library sends no telemetry and contacts
no network service. Only the current SQLite format is maintained; obsolete
development fixtures are disposable when changing storage formats.

Activity observations are stored in the same `core.sqlite3`: each stream's
immutable source metadata and, for each received event, its content and receipt
time. Adapters choose which native fields to send, redact credentials and omit hidden
reasoning; Core does not redact event content. Retention is bounded per stream and
per project ([activity contract](docs/ACTIVITY.md#reading-and-retention)): the oldest
events are evicted and reads report the gap. `forget_activity` removes a stream and
its retained events in one transaction while the expected cursor still matches. This
ends duplicate protection for that stream and, like other logical deletion, is not
secure erasure. Core never executes event content.

Check delivered behavior before describing it as implemented. Execution and
deployment limits are in [SECURITY](SECURITY.md).
