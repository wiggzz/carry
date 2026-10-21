# Explicit historical full-history forks (experimental)

The proxy can seed a **new explicit branch** from a retained, committed review
cut on another branch of the same explicit tenant/session namespace. Gateway
bearer authentication and the existing caller-supplied `x-carry-tenant` identity
contract are unchanged: this does not add per-tenant authentication or ACLs.
It hydrates accepted whole-group advice and memories **before** the normal primary planner.
This is a limited HTTP contract, not automatic native Pi or Codex fork support.

## Contract

Send the child's usual `x-carry-session` and distinct `x-carry-branch`, together
with both `x-carry-fork-parent` (the parent's branch name) and
`x-carry-fork-point` (an exact retained point handle). The owner and session are
not selectable through the fork headers. Use compact mode and a full `input`
array containing the entire cut in its original order followed by child input.
Do not combine this with `previous_response_id` or a native checkpoint request.

Handles are server-owned opaque identifiers. Use the read-only matching API
below rather than computing a handle or reading a checkpoint. The committed
parent checkpoint remains the authority after asynchronous review settlement;
the primary's response alone does not prove that a portable cut was committed.
Python fixture `cut()` is not a production canonicalization or authority API.

A point binds the complete ordered observation/dependency frontier, item hashes,
original member/cohort IDs and exposure, group protections, request settings
(including primary model, instructions and tools), reviewer model/endpoint and
review policy version. Branch-local cache affinity is excluded. Memories do not
get to shorten their dependencies merely by claiming older `source_ids`.
Only first fully committed reviews on an unchanged raw cut with no earlier
advice/memory are published. Every chunk in an import run must be fully
accepted; a clean final chunk cannot erase an earlier rejection. Ordinary
partially accepted advice remains usable on its own branch, but is not portable.
Later updates, legacy advice and reviews whose live history grew during dispatch
are conservatively not promoted to new portable cuts. Already published cuts remain
immutable; later parent advice cannot amend them.

On hydration, historical pins/keeps and current pins both apply. A child suffix
that extends an authorized cohort is incompatible rather than permission to
remove a partial group. New child suffix input stays unreviewed and retained.
No mutable parent usage, primary/reviewer cache receipts, response frontier,
worker, bootstrap-progress or reviewer replay is copied. The fork request does
not dispatch a paid recovery review. Genuine primary overflow is rejected by
the normal pre-upstream budget path; reviewer capacity is not a new primary
admission gate for accepted historical facts.

Declare intent only for the creation/retry request. An identical intent within
the evidence horizon is idempotent, not permission to rehydrate an existing
child or resurrect its removed sources or retired memories. Every retry still
validates the cut, scope/settings, cohort and live-child lineage. Contradictory
fork/reset or opaque-compaction intent is rejected before resetting state or
calling upstream, even with `x-carry-history-policy: reset-on-divergence`.
Later ordinary requests use the child's own lineage and can omit the fork
headers. Even existing children cannot use expired/evicted parent evidence to
request another recovery.

## Neutral retained-cut matching (schema 1)

`POST /carry/fork-points/match` takes the caller-owned **original complete
Responses body**: full `input` array plus original `model`, instructions, tools
and other settings. It compares the ordered prefix, scope and complete cohorts
server-side using the same validator as fork hydration. A suffix is allowed but
must not extend an authorized cohort. Send raw original input, not Carry's
compacted outgoing primary input. JSON object key order and equivalent numeric
spellings are parsed by the server; adapters must not reproduce Rust serde's
numeric hashing in Python/JavaScript. This is a matching operation, not a GET
listing or a source-ID-to-authority conversion.

Require `Authorization: Bearer <configured gateway token>` and all three
explicit `x-carry-tenant`, `x-carry-session`, `x-carry-branch` headers selecting
the **parent's exact namespace**. Missing, duplicate, invalid or defaulted
identity is rejected. Even loopback requires configured gateway authentication
for this endpoint. This reuses the existing shared gateway token and
caller-supplied identity: **it does not establish a per-tenant ACL**. A trusted
operator/gateway must own namespace assignment and continue stripping untrusted
Carry headers. No query/body parent namespace or arbitrary-session search exists;
fork-intent headers on matching requests are rejected.

The successful response contains only:

```json
{
  "schema_version": 1,
  "outcome": "matched",
  "matching": "ordered-prefix-cohort-scope-v1",
  "points": [{"id": "<opaque retained handle>", "input_items": 3, "expires_at": 1234567890}],
  "horizon": {
    "publication": "first-fully-accepted-raw-cut-v1",
    "max_age_secs": 86400,
    "max_nodes": 8,
    "max_bytes": 262144
  }
}
```

The example expiry is illustrative; actual `expires_at` is the absolute Unix
second of the point's current configured age limit. `input_items` is the retained
prefix length in the supplied full input array (exclusive end offset), not a
native entry/turn ID. Points retain committed publication order and stable IDs;
`horizon` reflects effective per-lineage configuration. There are at most
`max_nodes` rows. No bodies, source/member hashes, facts, advice, protections,
private paths, spend or cache receipts are returned.

A valid request without compatible committed retained evidence returns HTTP200,
`outcome: "no_match"`, `points: []` and the same version/matching/horizon metadata.
Unknown, evicted, expired, uncommitted, other-namespace and incompatible cuts do
not cause fresh-history forwarding, recovery review or fabricated handles. This
bounded empty result deliberately does not reveal which unavailable condition
applied. An explicit later fork still uses the typed HTTP409 errors below;
matching does not reserve a point against expiry/eviction or guarantee primary
capacity. The creation/retry request must use a distinct child branch and the
returned handle with the existing fork headers, and is revalidated normally.

HTTP401 means missing/wrong/unconfigured gateway auth; HTTP400 means malformed
JSON/full-input/model or explicit identity, fork-intent headers or unsupported
stateful/background body. Browser Origin returns HTTP403; audit/off modes return
HTTP409 `fork_unsupported`. Corrupt/unreadable exact committed checkpoints retain
HTTP500 rather than being treated as fresh. The existing 16MiB request bound
applies. This is read-only despite POST: it acquires only the exact lineage lock,
loads/prunes a local copy of its committed checkpoint, and never saves/creates a
checkpoint, refreshes the model catalog, changes receipts, or starts a worker or
outbound request. No other branch/directory scan, index or background job exists.
Age/node/byte filtering during discovery does not physically erase saved bytes.

A future native adapter must bind an observed original full-wire prefix and
this boundary to the actual native entry/turn selected by the pinned client's
fork/tree/rollback operation. It must preserve operator namespace ownership and
create a distinct Carry branch/generation as required. A matching handle is not
native-operation acceptance, arbitrary later-cut portability, or proof that an
adapter has observed the right native boundary.

## Retention and failure

Per parent lineage, defaults are 8 points, 262144 serialized evidence-vector
bytes and 86400 seconds. CLI/environment controls:

| CLI | Environment | Allowed range |
| --- | --- | --- |
| `--fork-max-nodes` | `CARRY_PROXY_FORK_MAX_NODES` | 0–32 |
| `--fork-max-bytes` | `CARRY_PROXY_FORK_MAX_BYTES` | 2–1048576 |
| `--fork-max-age-secs` | `CARRY_PROXY_FORK_MAX_AGE_SECS` | 0–604800 |

Zero nodes or age disables retention. Evidence contains hashes and accepted
memory text, not duplicate raw native histories; memory text is still sensitive
checkpoint data. At load/publication, expired points and then oldest excess
points are pruned. At most 32 expired handles remain as per-lineage diagnostics.
Older discarded handles become unknown. This is **not a global state-directory
quota** or global source-removal tombstone. Existing-lineage removal safety does
not expire with the fork archive. Parent source removal, reviewer cleanup and
active-history resets do not erase retained cuts.

Publication uses the parent's existing bounded atomic checkpoint rename. A
failed write cannot advertise a point in a separate index; an uncommitted
`.tmp` file is never recovery authority. There is no background janitor: age is
enforced when loading/resolving/publishing, not by a running cleanup service.

Unavailable declarations return HTTP409 with `error.code`, never silently start
a fresh raw branch. Codes include `fork_invalid_intent`, `fork_unsupported`,
`fork_requires_distinct_branch`, `fork_unknown`, `fork_expired`,
`fork_incompatible`, and `fork_child_conflict`. Invalid preexisting session
identity/checkpoint errors retain their existing validation errors. Audit/off
modes reject fork declarations rather than imply advice was applied, including
when no valid UTF-8 explicit session identity was supplied. Undeclared off-mode
requests retain ordinary unchanged forwarding.

## Evidence and remaining work

The hosted loopback suite in `tests/proxy_fork_continuity.py` exercises the real
proxy binary and actual reviewer HTTP responses, including historical A→parentB
cleanup→childA+C, bounded retention, restart/reset, sibling-memory exclusion,
cohort/protection incompatibility, independent child caches/spend, held unrelated
reviewer, and hard primary limits. It is explicitly **not** a handwritten-JSON
substitute for actual native branch operations.

Still required: Pi RPC fork/new session ID and SDK tree/same session ID adapters
and distinct actual-client fixtures; Codex real thread/fork/new IDs and rollback/
same-ID adapters and fixtures; general later-review/dependency-closure portable
cuts. The neutral server-side matching contract does not implement these native
adapters. The separate reviewer
bootstrap current-budget merge repair RI-001 has its own behavioral evidence and
independent-review gate; these HTTP fork repairs do not replace that gate or add
receipt-anchored accounting. Passing loopback tests alone is not full #148
acceptance or permission to merge/deploy.
