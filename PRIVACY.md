# Rho Core data handling

Design baseline: 2026-10-10. `main` contains the first managed flow; its data
handling is listed below and in the [managed-run contract](docs/MANAGED-RUN.md). The
old policy remains with its archived source. The other requirements here must be
implemented and verified as the corresponding capabilities are introduced.

Core retains data only for the selected managed guarantees: request identities
and scope, actual arguments and targets, execution-owner associations, native
handles, known results or result references, and necessary diagnostics. Arguments
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
retention before use. The [engineering guide](docs/ENGINEERING.md) defines the
minimum fact and validation checklist, without selecting a telemetry service.

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
no network service. Existing file-format stores are preserved and rejected, not
silently imported or treated as empty.

Check delivered behavior before describing it as implemented. Execution and
deployment limits are in [SECURITY](SECURITY.md).
