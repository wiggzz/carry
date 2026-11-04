# Proxy phase timings

Carry writes content-free `phase_timing` records to its existing bounded private
`STATE_DIR/diagnostics/<session-hash>.jsonl` journal. They help separate local
preparation, upstream HTTP observations and local settlement without enabling
`--verbose`, making a model call, or changing the request wire or accounting.

This is observational instrumentation, not a speed improvement or a retrospective
profile of an older executable. Deploying a candidate is a separate operator
choice; do not restart a live proxy merely to inspect existing evidence.

## Read an existing record

Work on a private copied diagnostic file. The following reads only the selected
file and emits timing data, not request/response bodies:

```sh
python3 - /private/copied-session.jsonl <<'PY'
import json, sys
with open(sys.argv[1]) as lines:
    for line in lines:
        event = json.loads(line)
        if event.get('event') == 'phase_timing':
            print(json.dumps({key: event.get(key) for key in
                ('session_id', 'request_id', 'review_attempt_id', 'data')}))
PY
```

Request IDs join existing received/submitted/committed/failure records;
`review_attempt_id` joins a review chunk and its wire/receipt diagnostics.
`data.native_attempt_id` joins the existing `BENCHMARK_CONTEXT_EVENT` stdout
attempt. Never join merely by close timestamps. These IDs are private log fields,
not metric labels; no per-request registry or durable timing state is added.

## Units and boundaries

All numeric times end in `_us`: integer monotonic microseconds within **one**
primary `forward_request` or reviewer `review_chunk` invocation. They cannot be
subtracted across records/processes. The outer journal `at` remains integer wall
seconds and is for correlation only.

- **Entry:** starts after Axum has extracted the inbound body, before authorization,
  identity/owner resolution, lock waiting, loading and ingestion. HTTP accept,
  upload/body extraction and framework rejections before that handler are outside
  coverage. The older `primary_received` event occurs *after* much of that work.
  Preparation can include a source-catalog HTTP lookup and exceptional review;
  the envelope is Carry-owned work, **not necessarily local CPU time**.
- **`preparation_done_us`:** entry-relative offset after local body preparation,
  serialization/header setup and native attempt creation, before API-key send or
  subscription auth/send. Subscription auth/refresh gaps remain visible between
  this offset and the per-attempt dispatch offsets; they are not provider time.
- **`attempts`:** at most two records from the existing subscription retry owner,
  ordinal0 then1, or one API-key attempt. `dispatch_us` precedes the real HTTP
  execute/send; `headers_us` is when its response object returns. This interval
  includes client transport work, network and upstream headers latency—not just
  model work. A discarded 401 response has no observed body completion.
- **`first_body_us`:** first nonempty chunk returned to the existing reader,
  *not* first SSE data, first model token, or socket arrival. Top-level body
  offsets refer to the final attempt; per-attempt offsets retain earlier attempts.
- **`body_complete_us`:** reader saw clean HTTP EOF, not an SSE model terminal.
  Header/body intervals include streaming and reader scheduling, local parsing
  and relay-channel backpressure. Do not label them prefill, decode, pure model
  generation or provider-only latency.
- **`total_us`:** entry through terminal handling/settlement and local handoff.
  A streaming primary may already have handed response bytes to the client-facing
  relay before this ends. Neither channel enqueue nor JSON conversion proves
  client socket flush or client consumption. Primary settlement includes existing
  checkpoint/accounting work; reviewer settlement ends at chunk validation and
  snapshot accounting, **before** a background worker later locks/publishes its
  shared paid owner. Background merge is not included in primary blocking time.

`primary_lock_wait_us`, `state_load_us`, `planning_us`, `blocking_review_us`,
`settlement_us` and `relay_send_wait_us` are measured **durations**, not offsets.
They are nested subspans; never add them to `total_us`. Repeated loads/plans and
lock reacquisitions accumulate within their named duration. Uninstrumented local
preparation (identity/index/ingestion/projection/quantity/reservation) remains in
the entry-to-dispatch envelope rather than being guessed as provider time.
`relay_send_wait_us` includes channel enqueue/scheduling, not socket flush, and
is already inside body-consumption/total time.

## Reviews, failures and missing evidence

`role` distinguishes `primary`/`reviewer`; `background` is explicit. Only the
actual awaited exceptional `run_review` contributes `blocking_review_us` to a
primary. Ordinary background reviewer durations overlap primaries and must not be
charged to their critical path or added to elapsed session time.

One request-local guard emits one terminal summary on normal return or dropped
future, including pre-dispatch rejection, HTTP/transport errors and cancellation.
`outcome` describes that boundary, while `incomplete` marks censored timing.
`null` means a phase was not reached/completed/observed; it is not zero. A clean
HTTP EOF does not itself certify model success, valid advice or durable authority;
use existing committed/failed/review-finished records for those outcomes.
`active_phase` identifies the last owned phase on an interrupted path. A cancelled
lock wait has a nonzero lower-bound total but null completed lock duration.

The guard cannot run on process kill/abort or power loss. Journals remain
best-effort/fail-open with existing16KiB record,64KiB lane and256KiB global caps;
`dropped_before`/missing records mean incomplete coverage, never zero cost/time.
Do not exclude errors/timeouts or missing terminals from population denominators.

The records contain fixed enums, numeric observations and existing opaque IDs,
not URLs, query strings, headers, tokens, prompts, bodies or raw session material.
Logging is not authority for billing, capacity, removal or cache eligibility.

## Synthetic example

[Machine-readable fixture output](proxy-phase-timing-synthetic.json) is an actual
controlled loopback test record, **not live/model performance data**. Its source
head/run and the independently failing timeout-censoring control are identified in
the wrapper. Do not use these delays as a performance forecast.

## Model-free validation

The `proxy::tests::phase_timing_*` native tests execute actual forwarding/relay and
review owners with owned loopback providers. They check separated delayed lock,
headers, first-body and EOF boundaries; exact native request/response bytes and
usage; rejection/cancellation/HTTP failure; reviewer correlation/deadline; and the
bounded retry-span owner. The retry-span test is not an OAuth-refresh integration
claim and never contacts an auth service. Existing full native/HTTP suites remain
independent wire, cost and authority controls.
