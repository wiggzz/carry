# Native Messages protocol boundary (partial #171)

`carry::anthropic` is the native protocol observation seam for a future
`/v1/messages` path. It follows the existing `proxy_sse` separation between
bounded observation and unchanged wire relay. It is deliberately not connected
to the live proxy yet; existing OpenAI requests are unaffected.

## Contract

`decode_request` and `decode_message` check the native JSON envelope and return
its unnormalized JSON value. `decode_response` distinguishes native messages
from preserved provider error envelopes without interpreting HTTP status. Content arrays retain their order, system blocks,
multimodal payloads, tool-use/result IDs, cache controls, signed thinking and
unknown fields. This is semantic JSON preservation, not byte-for-byte JSON
serialization. It is not feature admission or validation of tool dependencies.
Callers must retain original request/response bytes for transparent relay.

Feed arbitrary transport chunks into `Observer`, then call `finish(false)` at
clean EOF or `finish(true)` on transport failure/cancellation/disconnect. Only
`Status::Complete` is a complete protocol observation. A `message_stop` alone
does not prove downstream delivery, HTTP success or eligibility for persistence.
The eventual wire-finalization owner must combine those facts. An incomplete
observation's `message` is diagnostic partial content, never a successful turn.

The observer checks the sequential message/block/delta/stop lifecycle. It
assembles text, thinking and tool JSON, applies opaque signature replacements,
and retains complete
redacted and other opaque blocks unchanged. Input JSON is parsed only when its
block closes. It preserves native usage fields: message-delta usage replaces
reported fields rather than summing cumulative counters. Missing usage is not
zero. No cost, cache eligibility, token-budget or retry inference is performed.

The raw JSON events remain available alongside the assembled message, including
extension fields and provider errors. They may contain private prose and signed
material: these are **not logging-safe**. Unknown event/delta semantics produce
`Unsupported`, so callers can continue unchanged relay but cannot silently
commit a partially understood assistant turn. Unknown fields in known events
are retained in the event record; this does not imply they are safe to transform.

Observation has a 4 MiB total wire-byte budget, including comments and pings.
Overflow gives `LimitExceeded`, without affecting separately owned relay.
This is an observation limit, not a provider request or context-window limit.
LF, CRLF and CR framing, multiline data, UTF-8 split across chunks and SSE
comments are supported. Incomplete trailing frames never complete a message.

## Remaining integration

Issue #171 remains open. Routing, version/beta headers, authentication, HTTP
status/error handling, retry policy, cancellation propagation, Session/history,
whole signed-block retention and tool dependency selection, reviewer memories,
cache/accounting/persistence, and actual Anthropic-client integration all remain
future work. New provider event/delta semantics require explicit support before
transformed retention; opaque preservation alone is insufficient. This module
makes no claim of signed-block validity verification or live-provider validation.

The model-free fixtures in `tests/fixtures/anthropic` are synthetic, not captured
provider traffic. Tests run in the existing `cargo test --locked` CI job.
Protocol references: [Messages streaming](https://platform.claude.com/docs/en/build-with-claude/streaming)
and [Messages request/response](https://platform.claude.com/docs/en/api/messages/create).

Signature replacement and empty tool-input delta behavior follow the
[official Python SDK accumulator](https://github.com/anthropics/anthropic-sdk-python/blob/main/src/anthropic/lib/streaming/_messages.py).
Supported content deltas are text, thinking, signature and input JSON; citations
and newer semantic deltas currently return `Unsupported`. Message-level updates
other than stop reason/sequence also return `Unsupported`.
