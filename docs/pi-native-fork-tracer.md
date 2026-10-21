# Isolated pinned Pi fork tracer

This is **not** full native integration or #148 readiness. The normal
`scripts/pi_carry.py`, installed Pi extensions/settings and the normal main-only
gateway remain unchanged. No Pi tree, Codex rollback/fork, policy inheritance or
native-dollar claim is made.

## Execute

Use an already-built Carry binary and the CI-installed **Pi 0.84.2** only:

```sh
python3 scripts/proxy_native_fixture.py --carry "$CARRY_TEST_BIN" \
  --pi "$(command -v pi)" --client pi --case fork --output "$FRESH_OUTPUT"
```

Without `--case`, the existing invocation still executes every old fixed case,
then this additive Pi case. Existing normal hosted CI already calls that path;
no workflow modification is needed. The fixture refuses another Pi version.
Pinned protocol source is earendil-works/pi at
`914cf1472e715297caa30db4b9535d534a9eb718` (0.84.2).
No provider network or paid model is used: only isolated loopback scripted
provider, fresh HOME/config/workspace/session/checkpoint directories and the
actual client executable.

## Real native operations and supported boundary

The trusted driver launches Pi `--mode rpc`, calls `get_state`, and sends real
`prompt` commands. It waits for `agent_settled`; full `get_entries` and
`get_fork_messages` supply native IDs. For B's actual user `entryId`,
`{type:fork,entryId}` creates a new native UUID, retains the path through B's
parent, and excludes B itself. `{type:clone}` instead retains the full current
path, including B. `switch_session` returns to the parent before the fork.
The new native session header and actual retained path are independently
checked. `parentSession` filesystem paths are **evidence only**, never Carry
lineage credentials.

`PiNativeBridge` is a reusable in-process trusted-controller seam, explicitly
created by this isolated fixture. Its control methods are not HTTP routes.
The operator fixes one trusted tenant/session family. `get_state`'s native UUID
plus navigation generation (only generation zero/new-UUID forks in this slice)
maps to a random, durable Carry branch. All incoming `x-carry-*` headers are
removed **before** operator identity/strict-history projection; raw complete
native body bytes are not rewritten.

After A's real first accepted unchanged raw review has committed, the controller
associates the actual A retained entry path with the last original full native
provider request. It calls authenticated `POST /carry/fork-points/match` with
that original body/settings and explicit parent identity, without fork headers.
Only a unique server-returned point whose exclusive `input_items` equals that
whole observed request boundary is supported. The terminal A assistant occurs
after that request; the actual fork-before-B retains it and the server validates
the complete child body and suffix. This does **not** translate arbitrary native
entries to cuts or invent authority from a new UUID/path/client hash. Other cuts
or horizons can fail; the bridge does not recover them through a fresh request.

For a validated new child, server-returned opaque `x-carry-fork-parent/point`
are projected on creation/retry. After an observed successful completed response,
the child uses only its own branch. Failure leaves creation intent intact.
Unknown, expired and incompatible native intent is typed and fail-closed.
Ordinary fitting unreviewed branches pass with no discovery requirement.

## Bounded state and evidence

The 0600 atomically replaced mapping has a 64 KiB / 32-native bound; it holds
only fixed scope, native UUID/generation, branch IDs, observed native path IDs,
server opaque handle/exclusive boundary/expiry and pending creation intent.
It never stores original bodies, raw transcripts or filesystem lineage paths.
A single bounded original request is volatile only, for explicit association;
there is no journal, global checkpoint search or reset-on-divergence fallback.
This is an exclusive single-controller trial seam, not a multi-process mapping
service or a production plugin.

Fixture-only `native-wire.jsonl` (actual caller), `carry-wire.jsonl` (operator
projection/matching), `provider-wire.json` (actual outbound requests, receipts,
reviews), native JSONL/RPC commands, and A/B/child checkpoint copies provide
complete evidence outside production bounded traces. Cardinality is checked on
both wire sides and all primary receipts. The native tool files must actually
exist. A fact must occur exactly once on child input; A raw removed cohort and
B-only advice must be absent; C, protected A and remaining tool cohorts must
survive. The parent has real B-only advice and later disposable reviewer cleanup.
The child has zero rescue review/summary, cold spend/cache, no inherited mutable
workers, an unchanged parent checkpoint and stable actual native restart/resume.

`wait_lineage_settled` locates precisely tenant/session/branch's checkpoint;
malformed/decoy/unrelated-held controls cannot substitute for or block that
lineage. Existing single-lineage cases keep their previous settling behavior.

Independent source review and exact-head full hosted GREEN are required before
accepting this narrow tracer. Inherited base discovery approval is not assumed.
