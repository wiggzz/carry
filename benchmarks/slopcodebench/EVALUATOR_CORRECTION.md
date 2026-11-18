# Pinned evaluator correction: collection-canonical-adder-v1

The runner/dataset commits and strict `all-cases` correctness stay unchanged.
The pinned collection-inventory owner overwrote canonical skip-excluding counts
with a second aggregation that included skips. The sole semantic edit rebuilds
merged results through `CorrectnessResults.add_test_result`; it does not alter
collection, test identities/statuses, missing-test backfill, infrastructure
failures, group mapping, the scorer, or the execution-bearing readiness guard.

`upstream.json` versions the edit and exact before/after SHA-256 identities.
Trusted preparation first requires a clean pinned upstream checkout, then
explicitly installs the checked edit **before** importing official definitions
or starting gold evaluation. The hosted synthetic fixture uses the same installer.
Runtime activation/preflight only validate; they never repair source. Validation
allows exactly the bound collection-file working-tree edit, checks its base Git
object and effective bytes plus unchanged report bytes, and refuses any other
tracked dirt. Generic clean-checkout verification remains strict.

The effective evaluator identity is included in contract/prepared metadata and
every readiness binding; it therefore also changes the canonical plan/record
contract fingerprint. Missing/old/mismatched identities or old gold receipts
cannot authorize execution. No dependency upgrade, parallel scorer or report
rescoring is involved. Installation intentionally refuses a second application.
Use a fresh checkout/preparation namespace for new qualification.

The retained database_migration checkpoint_3 exact-source replay executes the
real public alias, pytest wrapper, collection owner, canonical policy and report
save/readback, with only native collector/subprocess/import boundaries shimmed.
Its original 86/87 rejection and raw evidence remain immutable. Corrected local
component replay is not hosted Docker/native gold attestation. Full unchanged
cohort, three repetitions, fresh model-free hosted qualification and independent
artifact reconciliation remain required before any paid authority.
