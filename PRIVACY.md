# Rho Core data handling

Design baseline: 2026-10-10. `main` has no new executable runtime. The old policy
remains with its archived source. The following requirements must be implemented
and verified as the corresponding capabilities are introduced.

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

Check delivered behavior before describing it as implemented. Execution and
deployment limits are in [SECURITY](SECURITY.md).
