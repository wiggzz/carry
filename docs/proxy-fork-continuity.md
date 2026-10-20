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

Handles are `<review_epoch>:<lowercase SHA-256 of the canonical source array>`.
The authoritative available handles are the parent's checkpoint `fork_points`
`id` fields after the asynchronous review has committed. This experimental slice
has no remote point-discovery API or native client adapter. Python fixture
`cut()` in `tests/proxy_fork_continuity.py` computes handles for the tested text
and tool payloads; cross-language canonical-number interoperability is not a
native-client acceptance claim. A caller must know an actually retained handle,
not infer review completion from the primary's response.

A point binds the complete ordered observation/dependency frontier, item hashes,
original member/cohort IDs and exposure, group protections, request settings
(including primary model, instructions and tools), reviewer model/endpoint and
review policy version. Branch-local cache affinity is excluded. Memories do not
get to shorten their dependencies merely by claiming older `source_ids`.
Only first fully committed, fully accepted reviews on an unchanged raw cut with
no earlier advice/memory are published. Later updates, partially rejected
batches, legacy advice and reviews whose live history grew during dispatch are
conservatively not promoted to new portable cuts. Already published cuts remain
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
child or resurrect its removed sources. Later ordinary requests use the child's
own lineage and can omit the fork headers. Even existing children cannot use
expired/evicted parent evidence to request another recovery.

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
modes reject fork declarations rather than imply advice was applied.

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
cuts; a stable client-facing point-discovery contract. The inherited reviewer
bootstrap all-rejected current-budget merge issue RI-001 remains a separate
unresolved prerequisite; this slice does not repair it or introduce
receipt-anchored accounting. Passing loopback tests alone is not full #148
acceptance or permission to merge/deploy.
