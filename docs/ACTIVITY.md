# External activity observations

This contract covers selected external observations, separately from managed runs.
Receiving, reading or reprocessing an event starts no Agent, tool or runtime. The
original tool owner remains authoritative about execution and its effects.

## Source and identity

`Core::open_activity(caller, source)` binds a stream ID to a producer, native
session ID and inert metadata (maximum 4096 serialized bytes). Same identity and
content returns the existing stream; changed content conflicts. Caller and source
IDs use Core's existing identity grammar. The state directory binds the project.
Caller namespaces are not authentication; the local Host retains its same-UID
boundary. A metadata path is never opened by Core and grants no resource access.

`append_activity` atomically retains a batch of `{sequence, kind, data}` events.
The adapter assigns stable contiguous ordinals from zero for its explicitly
selected event stream. Preserve native event IDs, native sequence, timestamps,
call relationships, content and completeness in `data`. The source metadata must
identify its normalization version and selection. A changed parser/selection
needs a deliberately new stream, not reassigned ordinals in the original stream.

Retained duplicates return their original receipt positions. Changed data under
an existing ordinal conflicts; a forward ordinal gap rejects the entire batch.
An evicted ordinal is explicitly expired and cannot be appended again. This is
observation delivery protection, not deduplication of external tool execution.

Receipt cursors describe ingestion order only. They do not replace native order,
timestamps or causality. A source that has lost evidence must record that loss
explicitly; Core cannot detect events the source never supplied. Tool return,
process exit, artifact integrity and scientific correctness remain separate facts.

## Reading and retention

`read_activity(caller, stream, after, max_events, max_bytes)` returns whole events,
the immutable source selection, retained range, `next_cursor`, `more` and `gap`.
The byte budget includes the complete serialized page. A single event that cannot
fit is an error; content is never silently clipped. An old cursor reports eviction
and returns the retained suffix. Reads never create a stream or start a runtime.

`list_activity` discovers this caller's streams. `forget_activity` requires the
latest expected cursor so a concurrent append prevents deletion. Explicit forget
ends duplicate protection; callers must not treat it as permission to rerun tools.

| Resource | Default configuration in `Limits.activity` | Exceeded behavior |
| --- | --- | --- |
| Streams per project store | `max_streams = 64` | Reject a new stream; keep existing sources/watermarks |
| Retained events per stream | `max_events = 4096` | Evict oldest events; keep next ordinal and expose gaps |
| Serialized event bytes per stream | `max_bytes = 4 MiB` | Evict oldest events; keep the newest complete event |
| One event | `max_event_bytes = 64 KiB` | Reject the batch without partial acceptance |
| One batch | `max_batch_events = 64`, `max_batch_bytes = 256 KiB` | Reject without partial acceptance |
| One read | `max_read_events = 128`, `max_read_bytes = 256 KiB` | Cap requested bounds; refuse an event/page that cannot fit |

Stream and row counts also bound metadata. SQLite pages and indexes add overhead
to serialized payload budgets. Limits cover Core storage, not an Agent's own logs.

## Durability and continuation

Activity uses the existing project SQLite transaction, lock and durability
settings. Store format 4 includes activity tables; older formats are refused and
left intact. This change does not migrate user stores or restart an existing Host.

After a lost acknowledgement, retry the original batch or read its retained stream.
After collector interruption, read the next ordinal and rescan the same native
source without invoking the Agent. Advance a collector checkpoint only after a
durable receipt. Native source loss, truncation and parser failures remain visible.
No guarantee is made for content that neither the source nor Core retained.

Adapters decide which necessary fields to collect, redact credentials, and omit
hidden reasoning. Core does not interpret arbitrary event text as instructions.
Derived summaries and artifact candidates should cite exact source event IDs and
parser versions; a fresh filesystem read cannot prove historical artifact bytes.
