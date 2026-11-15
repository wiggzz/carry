# Client receipt accounting candidate

Status: source/test candidate only. No Rust compilation or candidate runtime execution has occurred on the constrained author host. Format/parser and browser-script controls are not implementation acceptance.

## Population and ownership

`proxy_client_receipt::ClientReceipt` is the single client-layer owner of dispatch-frozen forecast/pricing provenance, bounded native usage, terminal/transport classification and ledger settlement. Primary and reviewer paths transfer this same record; a reviewer result does not rebuild a price from current settings. Auth refresh contributes a receipt for each actual provider dispatch (including the 401), never an invented second dispatch when refresh fails. Retry admission reserves the serialized envelope for both possible receipts.

All actual provider attempts belong to actual-money accounting, independently of HTTP success, provider completion, valid advice, downstream delivery or persistence success. Known failed/incomplete usage remains paid. Missing usage, rates or historical facts are unknown, not zero. Bare legacy failures are not backfilled. Existing success-only comparison fixtures have been reconciled explicitly: settled failed money participates; unavailable failed money makes the whole window unavailable while known subtotals remain visible. A failure does not retain output or trigger a new summary/cache cursor transition.

Input forecast error is native **input total** minus the predispatch visible-input forecast, not retained-history accuracy or a cache forecast. Its serialized signed magnitude (`negative`, `absolute_tokens`) spans the entire u64 domain. Calibration stores paired count, bias, absolute error and population variance by role; it is descriptive and does not tune planning, prices, cache assumptions or admission.

## Phase telemetry schema version 2

The existing bounded nonblocking phase sink remains the sink. This is not another ledger.

* `actual_dispatch` means at least one transport attempt exists. Predispatch cancellation has no attempt/receipt or phantom cost; absence of a terminal is not evidence of an explicit cancellation.
* `attempts[]` holds actual dispatch timestamps, `http_status`, bounded provider-request ID/error category and the corresponding numeric `receipt`. HTTP status is not a top-level substitute for provider status.
* Receipt telemetry identifies `attempt_id`, optional parent, model, hashed route and frozen `contract_fingerprint`, version, `provider_status`, safe error codes, `transport`, `usage_present`, `usage_status` (`missing`, `numeric`, `invalid`), `native_usage_valid`, `pricing_available` and bounded service tier. It does not include prompts, tools, response prose, auth headers, tokens or full URLs.
* `downstream_disconnected` is independent of provider completion. An already-dispatched response drains only within its existing operation deadline. No new work is dispatched to recover a receipt.
* Settlement timing and `persistence`/receipt persistence events are distinct from transport/provider outcome. A telemetry write/drop cannot change paid settlement; dropped events are exposed by the existing sink's counter after recovery.

No historical bare failure is retrospectively attributed to a timeout, cancel, provider or client cause.

## Conditional summary comparison

`GET /carry/dashboard/comparison/{id}?strategy=` accepts `replacement_cold`, `appended_history`, and `conservative_lower`. The latter is the explicit default, with frozen scenario summary allowance and instruction overhead. This is a read-only counterfactual: no summary request, admission change or repricing of settled receipts. Provenance describes Pi standalone versus Codex ordinary appended fallback; neither is claimed to implement Codex dedicated V2 compaction. Prior cache/quantity/legacy unknown guardrails remain.

## Behavioral qualification inventory

`tests/proxy_receipt_accounting.py` adds real loopback HTTP fixtures for paid failed/incomplete primaries and reviewers, disconnect before/after the paid terminal, held timeout and early EOF for both roles, private failure telemetry/missing usage, sink obstruction, forecast-before-receipt/calibration restart, and read-only summary selection/restart. Existing Python population/state controls retain ledger, null, history, native/cache and stream controls under the all-outcome contract.

Native fixtures exercise cancelled queued/predispatch and preheader callers, both-role auth refresh success/failure with exact transport counts/wire equality/frozen money, full-width receipt serialization/admission, and known failed-primary money under real unchanged store pressure. These Rust/HTTP controls must run against a rebuilt exact-source executable; collection and rustfmt do not execute them.
