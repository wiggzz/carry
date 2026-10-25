# Prefix advice and request-time history

Carry stores original native history, completed semantic advice, actual upstream
cache evidence, and paid accounting separately. A request-time projection is not
a new history and is never the authority for later requests.

## Matching and advice

Full-history Responses clients need no fork adapter or discovery call. Within the
existing tenant/session/branch and request-settings scope, Carry matches the
longest reviewed original prefix. Client rewrites and summaries are different
histories; there is no fuzzy reconstruction or native UI ancestry claim.

A reviewer captures original **H** and its current projection. Its completed
record is indexed by original H even if that projection omitted source, or the
paid owner advanced to H+A while review ran. H+B and H+C may reuse that record.
Advice from H+A cannot enter H or H+B merely because its facts cite IDs in H.
The newest applicable complete record is selected first. Unusable actions fail
closed; Carry never falls back to an older record to revive a retired fact.
Accepted empty records also publish. A dispatch generation prevents stale
completion from overwriting newer advice at the same cut.

Advice uses original native member IDs, including unions when omission joins
previously separated groups. Removal and dependent facts require complete,
replayable, unpinned native cohorts. Appending a same-call result invalidates an
incomplete old authorization; unrelated later tool activity does not. Current
human direction and unresolved/unknown native structures remain protected.

The reviewer receives a fresh projection with original native payloads, quoted
primary instructions, current facts, and one current ledger. It does not retain
prior reviewer answers, repeated historical ledgers, or a second replay history.
Omitted advice preserves prior opinions. Retiring a fact removes it from future
projections and reviewer inputs, not from immutable historical audit evidence.

## Disposable planning and cache evidence

Every request starts from canonical source plus applicable completed advice.
Approved-removable source may be retained while a compatible cache prefix is
warm, omitted when cold, and retained again after a genuine canonical upstream
dispatch rewarms it. Past omission never grants or revokes semantic authority.
Both views are rendered and checked before dispatch. Capacity is a hard bound;
ordinary selection compares the configured primary payoff horizon plus the
review actually due now. Future hits and a fixed history are sensitivity
assumptions, not guaranteed provider savings.

One bounded upstream wire-evidence index serves all purposes. Lookup has no
primary/reviewer discriminator. Actual destination/authentication digest,
model/settings, cache affinity, exact serialized input prefix and time determine
compatibility. Unknown/missing/failed receipts do not establish a hit. Potential
history with zero native cached reads gives zero confirmed cache credit.
Cache expiry is separate from seven-day history retention. Model/route capability
and native usage remain distinct from hypothetical future cache savings.

Primary and reviewer bills remain separate, frozen to their actual dispatch
contracts. Rejected/stale advice still costs money; primary failure does not
undo a completed review bill. Restart-orphaned work is unknown spend, not free.

## Scheduling

Ordinary reviews are asynchronous and never hold a fitting primary. One review
per paid owner can be in flight. A selected projection does not suppress later
review. Ordinary cadence uses completed-primary age, newly eligible native
source growth, a primary-size floor, and bounded no-yield backoff.

The sole exceptional acquisition policy is deterministic: a compact-mode primary
has no fitting authorized projection; complete, exposed, non-human, unpinned,
not-explicitly-protected source exists; optimistically omitting it would fit;
and the complete reviewer request fits and has known pricing. At most one review
is attempted for that request, with a five-second reviewer transport limit.
Only validated returned advice can authorize omission. No-help, failure or timeout
returns local HTTP413 if there is still no fit. There is no arbitrary dollar
threshold, speculative acquisition rollout, or ordinary primary wait.
A reviewer that does not fit is skipped rather than creating partial-observation
or import-window authority. Oversized canonical histories may therefore have no
new review until a permissible projection fits; protection is never bypassed.

## Canonical storage and observed IDs

`history.index` contains a shared immutable parent-linked native-value graph,
review records keyed by original frontiers, bounded wire evidence, transcript
references, and paid-owner transport/accounting snapshots. Snapshots do not store
selected removals or semantic authority. Diagnostic JSON mirrors are not a
fallback authority. Atomic index publication, in-flight owner pins and bounded
backpressure protect settlement; independent process writers to one directory
are unsupported.

Observed successful `previous_response_id` and `conversation` references expand
only retained original input plus actual provider output. They are mutually
exclusive. Carry strips these references after expansion. Unknown, expired,
evicted or differently scoped IDs return HTTP409 before primary dispatch;
unreadable/corrupt indexes fail closed. No remote unseen transcript is invented.

| Control | Default | Meaning |
| --- | --- | --- |
| `--history-max-age-secs` / `CARRY_PROXY_HISTORY_MAX_AGE_SECS` | 604800 | 0–604800 seconds; seven-day age target |
| `--history-max-nodes` / `CARRY_PROXY_HISTORY_MAX_NODES` | 16384 | 1–65536 unique canonical nodes |
| `--history-max-references` / `CARRY_PROXY_HISTORY_MAX_REFERENCES` | 256 | 1–4096 total tip/review/transcript references |
| `--history-max-bytes` / `CARRY_PROXY_HISTORY_MAX_BYTES` | 33554432 | Total serialized index bytes; must fit its empty envelope |

There is no hidden eight-review cap. Explicit node/reference/byte capacity can
evict older settled evidence before seven days. Age is checked on lookup and
publication; unreachable nodes and expired records are physically collected on
the next publication, not by a background eraser. In-flight owners are pinned;
if mandatory live state cannot fit, admission fails locally with HTTP507.
Lowering a byte budget below an existing file fails closed rather than silently
reinterpreting it. The index limit is not a whole-directory or peak-memory limit:
atomic old/new generations and diagnostic mirrors also occupy storage.

## Intentional fresh-state boundary

This unmerged redesign uses `carry-prefix-history-v3` and session version 3.
Use a **new empty state directory** when adopting it. No legacy checkpoint loader,
migration, fork-point API, permanent selection, retired-member map, reviewer replay
or keep-lease compatibility is provided. No live directory is migrated or wiped
by this change. Removed operator surfaces and superseded tests are recorded with
the redesign evidence. Existing gateway authentication boundaries are unchanged;
content digests are not authorization. No deployment or native-client UI fork
acceptance follows from synthetic loopback tests.
