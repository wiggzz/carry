# Transparent server-side history

Full-history Responses clients need no Carry adapter, fork-discovery call, entry
mapping or fork headers. Carry matches original input against a server-owned
prefix tree within the existing owner/settings boundary. Fork and rewind are
content-history operations. Content identity is not authorization or native UI
ancestry. Distinct logical-session identity/authentication is deliberately deferred.

## Ordinary full-history requests

Send the original complete `input`, model, instructions and tools to
`POST /v1/responses`. Without `x-carry-session`, Carry selects the deepest
compatible live tip or committed review version. Append continues that selection;
a divergent suffix or rewind creates a cold child without changing its parent.
Restart and repeated requests use the persisted index. Concurrent siblings
re-resolve an advanced tip after acquiring its lock, rather than resetting it.

Existing optional tenant/session/branch headers still isolate caller-selected
namespaces; explicit sessions retain their strict/reset policy. These controls
are not required for automatic matching. Owner, settings, route and reviewer
policy must match; native cache affinity is not lineage authority. Identical
headerless histories in one scope may share a selection. Existing shared-gateway
bearer/loopback rules have not become per-tenant ACLs.

Client summaries/rewrites are different histories, not something translated back
to old native entry trees. Unmatched full history starts a new selection subject
to ordinary review and capacity rules. The experimental
`/carry/fork-points/match` endpoint and Pi mapping bridge have been removed.
Obsolete `x-carry-fork-*` intent is rejected, not silently forwarded as new history.

## Committed review versions

Each immutable version binds the **entire ordered observation frontier**, native
member/cohort identity, exposure, protections, removed membership, accepted
opinions/memories, instruction context and primary/reviewer policy. Claimed memory
`source_ids` never shorten dependencies: advice produced after A+B cannot enter
A+C merely because it names A.

Versions publish only after atomic checkpoint commit. Later fully accepted
reviews can publish additional versions containing prior facts/removals; this is
not first-raw-only. All import chunks must be accepted. Failed/rejected reviews,
temporary files, and reviews whose live source/projection changed incompatibly
during dispatch cannot create portable authority. Conservative rejection leaves
already committed versions intact.

Hydration preserves whole cohorts, exposure, historical/current pins and removed
membership. It copies no pending worker/import, paid usage or provider/cache
receipt. The first child request does not launch a recovery review; normal primary
hard-capacity guards still apply. Later continuation uses the child's own state,
so an old parent version cannot resurrect its discarded sources or retired facts.

## Response and conversation lookup

In review modes, `previous_response_id` and `conversation` (a string or object
with `id`) select retained, successfully observed canonical transcript frontiers.
They are mutually exclusive. Carry expands delta input server-side and strips the
reference before sending selected full input upstream. No client glue is needed.
Conversation IDs are learned from actual provider responses, not invented remote
state. Unknown, expired, evicted or differently scoped IDs return HTTP409 before
upstream; corrupt/unreadable indexes fail closed with HTTP500.

The canonical archive contains original observed inputs **and actual assistant
outputs**, separately from selected primary/shadow active context. Canonical
here means parsed JSON values with object-key-order-independent identity, not
semantic normalization. Exact provider-output replay can match; changing message
shape, deleting provider fields, or replacing text with a summary is a different
history. Carry does not fuzzy-match SDK-specific assistant echoes. The existing
native Pi fixture forwards that client's actual ordinary history and fork replay;
it does not certify every client's possible output transformation. Top-level
`metadata`, `client_metadata` and cache affinity are excluded from settings scope,
not accepted as logical-session credentials; fields inside input remain content.
Response IDs
retain immutable frontiers, permitting rewind after restart. Conversation IDs
advance with observed successful responses. This does not implement remote
conversation retrieval/creation or unseen provider history. Background provider
jobs remain unsupported in review modes.

## Storage and retention

Private `history.index` is an atomic bounded JSON index. Each immutable node holds
a parsed canonical wire value and its parent digest. Tips, review versions and
external IDs reference frontiers, sharing prefixes instead of copying a raw
transcript per reference/version. Lookup hashes request prefixes and reads this
index, never scans state directories. Headerless selection/accounting snapshots
store null wire values plus a canonical frontier; loading resolves those references.
Snapshots and canonical nodes publish in one atomic index transaction. Per-session
JSON files are diagnostic mirrors, not independent authority. Existing explicit
session checkpoints retain their legacy format and lifetime.

| Control | Default | Range/meaning |
| --- | --- | --- |
| `--history-max-nodes` / `CARRY_PROXY_HISTORY_MAX_NODES` | 16384 | 1–65536 unique canonical nodes |
| `--history-max-references` / `CARRY_PROXY_HISTORY_MAX_REFERENCES` | 256 | 1–4096 tip/review-frontier/transcript references |
| `--history-max-bytes` / `CARRY_PROXY_HISTORY_MAX_BYTES` | 33554432 | 67–67108864 total index bytes, including selected snapshots; smaller values reject startup |
| `--fork-max-nodes` / `CARRY_PROXY_FORK_MAX_NODES` | 8 | 0–32 committed review versions per lineage and across the shared index |
| `--fork-max-bytes` / `CARRY_PROXY_FORK_MAX_BYTES` | 262144 | 2–1048576 serialized review-version bytes per lineage and across the shared index |
| `--fork-max-age-secs` / `CARRY_PROXY_FORK_MAX_AGE_SECS` | 86400 | 0–604800 seconds |

Historical `fork-max-*` operator names now bound transparent evidence, not a
client fork API. The shared-index review budget measures the exact serialized
flattened version vector, including array punctuation, in addition to each
lineage's evidence vector. This aggregate bound is deliberately stricter than
independent per-lineage allowances. Retiring portable evidence alone does not
clear a retained mutable owner's memories, removals or paid receipts.
Expired/oldest references are removed until bounds fit, then
mark-and-sweep removes unreachable nodes. Age is enforced at lookup/publication;
physical erasure occurs on the next index publication, not a background janitor.
Evicting a transparent tip evicts its selected snapshot and diagnostic mirror;
unreachable canonical nodes are collected. This does not delete legacy explicit
checkpoints. Retained children preserve their own removal state. Large/unretained
histories may lack reusable evidence/ID expansion. One proxy
process owns a state directory; independent concurrent process writers are not
supported. Selected owners are pinned across lock waits, primary transport and
paid reviewer settlement. Pins are bounded by the reference budget and released
when work finishes; they are not a durable no-GC flag. If live authority cannot
fit the configured bounds, new work fails with HTTP507 before primary dispatch
rather than evicting that authority. Even a valid 67-byte limit fits only the
empty envelope, not a working owner. Lowering a limit below a previously published
index fails closed without replacing that generation.

The node graph shares raw wire payloads; it does not make the entire state
constant-size. A request reads/parses the bounded JSON index, hashes its full
input, clones selected state and serializes the index on publication (including
prune size checks). Each retained tip can have a selected snapshot and diagnostic
mirror. Atomic replacement temporarily holds old and new generations, and raw
reviewer replay/diagnostic mirrors can add bytes beyond the index bound. The
synthetic many-sibling control reports observed index/mirror bytes and node/reference
counts only, not peak temporary storage, operation counts or latency. No speedup,
linear-work, or whole-directory byte-bound claim follows from prefix sharing.

Predecessor unmerged headerless checkpoints are not automatically migrated into
this index. Missing history begins with supplied full input; unknown explicit
IDs reject. A lookup miss reserves a cold owner without reusing an expired
snapshot or treating a diagnostic mirror as authority. Concurrent same-input
fresh requests share a bounded reservation even in the mirror-before-index
publication window; selected live owners still retain their normal pins.
Existing explicit-session legacy checkpoint handling remains separate;
no directory scan promotes arbitrary old/temporary files into authority.

Diagnostic mirror publication precedes atomic index publication. A crash between
them leaves the prior indexed selection authoritative, never new uncommitted
advice. Orphan temporary files are never
authority. Archive size is bounded on load/publication; loss/corruption is not
repaired by scanning arbitrary old checkpoints.

## Verification and scope

`tests/proxy_transparent_history.py`, in ordinary Cargo CI, drives actual loopback
primary/reviewer requests: historical A→B then A+C, parent preservation, later
review dependency closure, parallel cohorts, cold receipts, restart/rewind,
failed/killed reviews, concurrent siblings, changed settings/rewrites, observed
response/conversation expansion and byte/node/reference/age eviction. Existing
explicit-lineage no-resurrection, capacity, economics and lease controls remain.

The pinned Pi RPC fixture executes real fork-before-B, inclusive clone and native
process resume using ordinary full-history forwarding, without a mapping bridge.
Its wire, receipt cardinality, cohort, spend and parent-byte graders remain.
Synthetic-provider evidence is not semantic quality, production cache efficacy
or net-savings evidence. Receipt-anchored dashboard work and logical-session auth
remain separate. No merge/deployment or broad #148 readiness is implied.
