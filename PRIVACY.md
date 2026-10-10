# Rho Core data handling

Design baseline: 2026-10-09. `main` has no new executable runtime. The old runtime's
policy remains with its archived source. The following requirements must be
implemented and verified as the corresponding new capabilities are introduced.

Core operates on a user-selected local project. Accepted effectful calls may retain
request identities, tool targets, arguments, native handles, known results and
diagnostics needed to locate and continue that work. Arguments and output can
contain code, private data or credentials printed by a program; bounded storage
does not imply automatic redaction. Ordinary reads are not automatically retained
as a query history or model memory.

The new Core does not require a model provider, store model credentials, own Agent
conversations or collect hidden reasoning. External clients decide what material
to send to their model and are responsible for that service's handling. Explicit
scientific tools and programs may contact networks under their documented behavior;
local execution alone is not a network sandbox.

Each implementation milestone must describe its storage locations, capacity and
retention, access boundary and deletion behavior. Cleanup must protect running work
and material still needed by retained references. Deleting a record cannot undo a
native effect. Tokens and credentials must not enter source control or ordinary
diagnostic output. Telemetry, automatic uploads and publication require explicit
product decisions; they are not consequences of adding a transport or tool.

Actual data-handling behavior must be checked against the delivered capability
before this document describes it as implemented. Execution and deployment limits
are in [SECURITY](SECURITY.md).
