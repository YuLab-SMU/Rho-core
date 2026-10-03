# Rho UI SDK

A framework-independent browser client for an ordinary plugin's isolated view.
Compile `index.ts` beside the sibling `plugin-protocol` declarations and ship all
emitted JavaScript modules in the immutable plugin artifact. There are no third-party
runtime imports, application singleton or Host bearer credentials.

```ts
import { connectPluginView } from "@rho/plugin-ui";
const client = await connectPluginView();
const configuration = client.view.configuration;
const snapshot = await client.query({ id: "example.read", version: 1 }, {});
const accepted = await client.invoke({ id: "example.run", version: 1 }, {}, {
  requestId: "stable-user-action-id",
});
// Keep the original Operation identity and inspect it through operation().
```

Capability payloads and native preconditions come from the owner contract. Declare
mandatory external capabilities in `manifest.requires`; optional capabilities need
an explicit activation selection. Neither a declaration nor opening a view adds
caller authority. Each call uses the frozen grants and current parent scope.
Invocation may return an accepted record while work is still running. Retain its
original request and Operation identity; disconnect, timeout and cancellation
requests do not establish native stop, rollback or failure.

`operation(id)` and `cancel(id)` address operations started by this view under the
original principal. `inspectOriginalOperation` and `verifyOriginalOperation` help
check the original identity and normalized arguments before deciding what to do
next. A new current observation cannot prove that an uncertain historical action
caused it. `ViewRequestError.diagnostic` preserves the original Host diagnostic.

Use `control(capability, arguments)` for an existing owner's transient native
request, with its exact provider and request identity. It uses the same grants but
creates no Operation or saved answer. Inspect after a lost acknowledgement rather
than assuming a safe replay.

## Owner preparation before close

`installCloseHandler(handler)` registers one participant on this connection when
the containing shell advertises `view_close_v1`. Implement `handler.prepare()` to
complete the owner's own preparation and optional `handler.resume()` to reopen its
interaction after a refused close. The handler decides how to freeze input, handle
composition, preserve content, save and inspect its original save Operation.
The SDK does not impose a DOM layout, input policy or content model.

Preparation can call any already granted Owner query, control or Operation port.
All calls still cross the original Host authority and native checks. The containing
Core fences browser actions during preparation and new mutations after all
participants confirm. Owners must await the result they actually need before
returning; admission alone is not a saved-content confirmation.

`ViewCloseCooperation.observe()` joins concurrent observations and prepares a given
close only once. It acknowledges the original close Operation without a content
version. A preparation failure sends a UTF-8 bounded refusal; a lost acknowledgement
stays unconfirmed and does not replay preparation. Disposing a channel cannot
acknowledge an in-progress preparation or establish that bytes were saved.

The shell alone holds private connection credentials, owns the original close
Operation and reports actual disconnection. Its explicit disconnect option does
not establish content preservation or native cleanup. Every participant must
confirm; losing one during preparation cannot turn the remaining confirmations
into success.

Core stores connection metadata, immutable bootstrap configuration and optional
resource context. It has no `setState`, synchronized draft helpers, content history,
visual document parser, canvas renderer or component annotation model. Editor,
Studio, Console and the shell own their respective content and interaction flows;
see [responsibility transfer requirements](../../docs/RESPONSIBILITY-TRANSFER.md).

## Resources and package archives

`readResource(client, reference, { maxBytes, signal })` verifies the exact scoped
resource, bounded chunks, byte length and full checksum. The default view budget is
16 MiB; an owner may set `maxBytes` from zero to 256 MiB. Exceeding the chosen
budget rejects the read before allocation. An abort stops further reads and does
not cancel accepted work. The reference itself grants no resource authority.
Original bytes remain with the resource owner.

For `.rho-plugin` bytes, `capturePluginArchive(blob, archiveId)` binds an immutable
Blob, opaque identity, SHA-256 and length. Persist that capture in Owner state before
`stagePluginArchive(client, capture, { signal, progress })`. Staging requires the
declared `plugins.archive_stage@1` grant, verifies bounded acknowledgements and may
repeat identical chunks. It retains bytes, without installing or running code.
Resumption needs the exact original file; the SDK does not retain it on your behalf.

`readPluginArchive(client, reference, { maxBytes, signal })` verifies the reference,
64 KiB pages and full checksum through `plugins.archive_read@1`. The archive maximum
is 374,691,157 bytes, independent of the smaller resource-view budget. Exceeding it
rejects capture or reading. Aborting a transfer does not discard bytes or cancel an
accepted import/export. Use the separate package ports for inspection, import,
export and explicit discard with original Operation recovery.

## Explicit browser actions and channel bounds

`downloadResource` and `downloadArchive` request original downloads through the
container's advertised feature and declared read grants. The container verifies
original bytes, identity and a bounded safe filename. Resolution means the browser
was asked to download; it never proves a local file was saved.

`copyText(textOrProducer)` reserves an explicit browser gesture before asynchronous
reads and publishes only bounded text. `openExternal(url)` validates a bounded
HTTP(S) URL and requests a separate browser context. The container owns browser
permission and acknowledges the actual request; Core authorization alone proves
neither clipboard completion nor remote page loading. No Host credential, opener
or referrer is sent.

One channel belongs to one connection lifetime. The SDK validates nonce, parent,
connection, view, sequence and response identity. Limits are a fixed 1 MiB JSON
message, 128 pending requests and 32-bit message sequences. Overflow rejects the
request; exhausting the sequence disposes the channel. Ordinary replies have a
30-second deadline; archive downloads have a 10-minute deadline. Timeout disposes
pending transport calls while preserving uncertainty about accepted Operations.
These transport limits are not configurable per call; Owners paginate or transfer
resources for larger content. `dispose()` ends this browser channel and rejects
pending promises; it never cancels accepted Operations or releases a native instance.
