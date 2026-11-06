# Receipt-anchored encrypted-reasoning forecasts

The request-time proxy planner can estimate removal of native encrypted
`reasoning` items without treating ciphertext as model text. This is a
**economic selection forecast**, not a capacity proof, provider measurement or savings receipt.
The report basis is `receipt_anchored_reasoning_forecast`; its retained token
quantity is the conservative decision quantity, not the fitted mean.

## Sizing order

- An exact compatible receipt for the target wins, including an unchanged prompt.
- Removing or restoring an entire measured native append block uses its recorded
  input delta, including blocks containing other opaque modalities.
- For part of a reasoning-containing measured block, relative item weights are
  normalized to that block's actual native total. Individual members are still
  estimated, not measured.
- For older history without measured append blocks, a compatible whole-prompt
  receipt supplies a residual budget. Fixed instructions/tools/framing, fixed
  system/developer messages and disjoint complete measured blocks are accounted
  for first. The residual is allocated across **all** unresolved visible and
  reasoning items, not assigned entirely to reasoning. The whole-prompt receipt
  is never inserted into the additive message-block collection.

Relative weights use `0.9432105075 * visible_o200k_JSON_tokens +
0.0432732920 * encrypted_reasoning_characters`. Visible JSON is sanitized by the
ordinary local quantity owner; encrypted bytes are not fed to the tokenizer.
These exploratory coefficients describe mixed native append blocks, not isolated
reasoning-token truth. They do not establish universal error bounds, independent
workload accuracy or another model's tokenizer contract.

## Conservative selection

Every candidate starts from the same available native-anchored source quantity.
Only changed items contribute a delta, so unchanged-context estimation errors
cancel. Exact native block deltas remain exact. Estimated removals receive half
of their allocated quantity as credit; estimated additions (including replacement
text) are charged twice their allocation. Integer division rounds removal credit
down and addition charges up. These margins are policy choices, **not guaranteed
confidence bounds derived from sample maxima**.

The existing price owner applies input tiers, current scoped cache receipts,
output allowances, scheduled reviewer costs and strict payback requirements.
Future requests for a reasoning forecast are charged uncached: a heuristic
quantity does not prove a future whole-input cache write. A nonpositive ordinary
conservative payoff keeps context. Heuristic quantities do not establish either
fit or overflow, cannot force a rewrite and cannot prove a capacity-rescue
candidate fits. Ordinary unknown-size forwarding remains provider-enforced.

## Evidence boundaries and continuation

Route, model/resolved contract and prompt settings must match. Missing evidence,
unknown changed image/audio/file/compaction items, unsupported reasoning shapes,
ambiguous identities, inconsistent budgets and checked arithmetic overflow remain
unknown. An unmeasured unknown modality cannot donate residual tokens to adjacent
reasoning. Conflicting native totals for the same prompt (or a decreasing count
on an exact append) disable heuristic calibration for that quantity contract,
including across restart. The latest actual receipt is still retained. A new
quantity contract starts fresh; there is no guessed count to repair a conflict.

After a filtered request gets its own actual receipt, the surviving unresolved
pool can be calibrated against that receipt, including after current-format
checkpoint reload and ordinary visible append. An incomplete older block is not
reused as though its entire native total belonged to its surviving members.
Restoring previously omitted **unmeasured** opaque items remains unsupported.
An exact compatible receipt prefix followed only by ordinary visible items and
supported new native reasoning has a separate `forecast_estimate`: keep the
receipt total, then double-charge the new suffix's empirical weights, rounded up.
It does not tokenize ciphertext or claim a measured source total. An unchanged
replay after settlement still takes its actual receipt first. This forecast
baseline allows authorized old-history edits to be compared while a newly emitted
reasoning item is awaiting its first input receipt; it does not make that item
removable. Other new opaque modalities, edited/reordered prefixes, duplicate
opaque identities and changed contracts stay unknown. A canonical restoration
alternative may remain unknown while a reauthorized continuation is sizeable.
No saved omission grants permission: current user protection, atomic cohorts and
reviewer authorization are always rechecked.

`predict`, `estimate`, `known_estimate` and `restoration` do not return these
heuristic allocations. In particular, canonical receipt reconstruction may
remain unavailable after an estimated edit; actual paid usage is preserved
without inventing comparative savings. Forecasts are not serialized as
`ProviderReceipt` or native cache extents. Forecast weights remain separate
from the neutral accounting allocations below. Dashboard polling remains
count-only and does not initiate full-history BPE.

## Traditional accounting estimates (residual v1)

The dashboard's traditional-history estimate uses **neutral accounting quantities**,
not the planner's asymmetric selection forecast. `InputMeasurements` remains the
single receipt/quantity evidence owner; `TraditionalHistoryState` still owns the
alternative's retirement, summaries, continuity and cache trajectory.

For a compatible native input increment containing supported encrypted reasoning,
subtract additive local visible-item quantities (including visible framing) first.
Divide the nonnegative residual equally among the block's reasoning members,
assigning integer remainders in source order. All member allocations together
conserve the measured native block increment. Complete blocks remain measured;
partial member sizes are explicitly estimated. For an unmeasured initial reasoning
item, the mean residual per reasoning member from compatible measured increments
is an explicit exchangeability assumption, **not** an allocation of the initial
whole-prompt residual. No compatible calibration means unavailable. Encrypted
length, billed output reasoning and unknown monetary tariffs are never conversion
rules. Unsupported modalities and inconsistent receipts stay unavailable.

Old-format blocks can gain allocations only when every original member is
currently available. These numeric allocations persist on their original blocks,
without copying source bodies. This is prospective calibration, not reconstruction
of past requests. New opaque retained output uses compatible input-increment
calibration, or remains unknown; actual output billing is never retained-input
measurement. A successful but unmodeled output keeps its native cursor and prior
summary state, not a fresh free-initialization allowance on every following call.

Each dispatch replaces the current signed canonical-minus-sent restoration.
Repeated omissions never accumulate; larger replacement memories can make it
negative. Estimated quantities enter the existing continuous alternative, including
its own summary timing/cost, frozen request tariffs and independent cache expiry.
Actual receipts and paid money remain independent. A quantity-policy boundary
freezes the prior cumulative comparison (including exclusions and reviewer money)
without replaying or repricing it. Old excluded calls do not become contributions.

The dashboard exposes measured, estimated and unavailable quantity coverage,
legacy provenance and matched-subset scope. These are point estimates: no
statistically calibrated confidence interval or whole-session bound is claimed.
The alternative assumes the same outputs and tool sequence, not measured causal
no-Carry expenditure. Controlled request-time/held-out synthetic receipts are
exercised in `src/proxy_quantity_accounting_tests.rs`; real-binary loopback
projection, legacy-checkpoint recovery and summary/restart behavior are exercised
in `tests/proxy_traditional_estimation.py`. Those controls verify accounting and
avoid training on held-out receipts; they do not establish accuracy on private
workloads or with real providers.
