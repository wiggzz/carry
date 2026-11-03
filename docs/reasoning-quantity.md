# Receipt-anchored encrypted-reasoning forecasts

The request-time proxy planner can estimate removal of native encrypted
`reasoning` items without treating ciphertext as model text. This is a
**selection/capacity forecast**, not a provider measurement or a savings receipt.
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
conservative payoff keeps context. Existing authorized no-fit/capacity handling
is unchanged.

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
Restoring previously omitted **unmeasured** opaque items, or predicting brand-new
opaque appends before another receipt, remains unsupported. Such a canonical
restoration alternative may be unknown while a reauthorized continuation remains
sizeable. No saved omission grants permission: current user protection, atomic
cohorts and reviewer authorization are always rechecked.

`predict`, `estimate`, `known_estimate` and `restoration` do not return these
heuristic allocations. In particular, canonical receipt reconstruction may
remain unavailable after an estimated edit; actual paid usage is preserved
without inventing comparative savings. Forecasts are not serialized as
`ProviderReceipt`, `MeasuredBlock` or native cache extents. Only existing numeric
receipt/identity state and the conflict flag persist. Dashboard polling remains
count-only and does not initiate full-history BPE.
