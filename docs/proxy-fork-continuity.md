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

Useful conclusions and unfinished work (status, next action, evidence pointers
and blockers) belong in concise factual memories, not repeated human instructions.
A memory is not required for every drop. Rejected
replacement memories preserve their identifiable eligible supporting sources,
including when concurrent growth exhausts admission at background merge; unrelated
valid drops remain valid. Validation cannot discover an unwritten fact or prove
that a live reviewer preserved its meaning.

Each projection inserts one neutral absence notice per contiguous omitted run.
Facts appear after their latest complete source cohort and do not split those
runs. Both notices and facts are rendered afresh, so repeat requests and restart
do not accumulate copies. Canonical history is unchanged. The actual rendered
request, including this overhead, is what sizing and cache-prefix checks use;
neither a notice nor a memory proves semantic safety or realized savings.

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

`history.index` is a SQLite store containing a shared immutable parent-linked
native-value graph, prefix-bound advice, wire evidence, transcript references,
paid-owner snapshots and independent numeric observations. Snapshots do not store
semantic authority; diagnostic JSON mirrors are not fallback authority. Atomic
transactions, in-flight owner pins and reserved disk headroom protect settlement.
Only one process may own a state directory.

Observed successful `previous_response_id` and `conversation` references expand
only retained original input plus actual provider output. They are mutually
exclusive. Carry strips these references after expansion. Unknown, expired,
evicted or differently scoped IDs return HTTP409 before primary dispatch;
unreadable/corrupt indexes fail closed. No remote unseen transcript is invented.

Detailed history defaults to seven days (`--history-max-age-secs`); numeric
observations default to 180 days (`--accounting-max-age-secs`). Disk/reference
pressure cannot shorten detailed retention. Node/record limits bound decoded
working sets, and the reference limit bounds concurrently pinned owners, not
total retained sessions. The separate `--history-disk-bytes` ceiling rejects
admission rather than evicting unexpired evidence; HTTP507 is not a reset request.
Maintenance collects expired history within its own bounded disk reserve.
See `carry proxy --help` for supported ranges and environment names.

Restart with the same state directory. Only the current SQLite store is supported;
predecessor JSON and corrupt state fail closed without replacing the original
file or becoming an empty history. These storage operations do not change gateway
authentication: content digests are not authorization. Scripted loopback tests
establish only their exercised transport/storage behavior, not native-client UI
fork acceptance or live-reviewer quality.
