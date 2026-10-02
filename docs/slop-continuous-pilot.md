# Continuous SlopCodeBench pilot

This is **not** the upstream conversation-reset benchmark. Exactly `file_backup`
then `layered_config_synthesizer`, four published checkpoints each, run in one
workspace and native Carry conversation per project. Both reset between projects
and worker attempts. Frozen protocol and archive digests:
`benchmarks/slop-continuous-2.json`. Native `context.rs`, `run.rs`, and `log.rs`
remain unchanged from main `6029c1798fe02c251b60db6013807ffac8580432`.

## Protected workflow interface

Use the existing `.github/workflows/run-swebench.yml`, not a new workflow.
Dispatch ref and `carry_ref` must identify the same reviewed candidate. Require
regular **exact-head** test and Terraform infrastructure checks before execution.
Leave the PR open/unmerged. No new infrastructure or Environment is required.

All three dispatches use:

- `harness=carry`, `model=gpt-6-luna`, `reasoning=medium`
- `carry_compaction_policy=disabled` (the workflow's generic default is economic;
  explicitly override it), blank keep lease, all other policy inputs at defaults
- the existing `swe-bench` Environment, OIDC roles, registry and artifact paths

1. **Prepare once:** `mode=prepare-slop-2`, `attempts=1`, blank `catalog_digest`.
   Builds only the dependency image; hash-checks the two upstream tarballs; grades
   all eight genuine reference solutions plus one incorrect implementation per
   project; publishes a tiny immutable OCI catalog with the existing publisher.
   No model key is provided. Read `preparation-report.json.catalog_reference`.
2. **Canonical paid smoke:** `mode=slop-2`, `attempts=1`, `catalog_digest` set to
   the catalog's `sha256:…` digest (or its 64 lowercase hexadecimal digits).
   This runs two trajectories/eight stage slots, not additional smoke tasks.
3. Parent independently gates the smoke's raw artifacts and cleanup. **Only then**
   dispatch `mode=slop-2`, `attempts=2`, with the **same** source/settings/catalog.
   This runs four more trajectories/16 stage slots. Parent maps `(run_id, native
   attempt)` to global repeats 1/2/3; total **six chains / 24 stages**. No new
   global-repeat workflow input or automatic promotion/redispatch exists.

`slop-2` accepts 1–3 worker attempts. Each attempt always runs both projects/all
four checkpoints. Artifacts are individually gated; the official SWE attempt
merger is deliberately not applied to Slop. Parent owns cohort aggregation.
Artifact names (unchanged naming infrastructure):

- `swebench-prepare-slop-2-carry-RUN_ID-RUN_ATTEMPT-attempt-1`
- `swebench-slop-2-carry-RUN_ID-RUN_ATTEMPT-attempt-N`

Preparation compatibility fields are `protocol_task_sha256` (frozen manifest)
and `prepared_recipe_sha256` (dependency Dockerfile, pinned requirements, base
image). Unrelated harness changes **do not** invalidate prepared dependencies.
The current-source Carry bundle is still built/verified separately in worker
preflight, before fetching its model key. Prepared assets/image contain no tests,
solutions, future specs, benchmark source checkout, or Git objects.

## Runtime and evidence

The authoritative shared native adapter has only a narrow `--snapshot-only`
addition: do not require/create Git metadata or capture SWE patches. Existing
native command, annotation defaults, provider-only proxy isolation, read-only
resume source and fresh destination are reused. No response/turn cap is added.
Each checkpoint has 600 agent wall seconds plus 45 host-cleanup seconds.
The worker has 18,000 seconds including preflight, eight stages, grading and
cleanup; controller polls within 20,100 seconds and rotates existing result
capabilities. Ordinary grading failures continue, with only aggregate test counts
in the fixed boundary prompt. Missing native state/infrastructure failure blocks
later checkpoints without resets/retries; all eight records remain present.

Grading copies a separate snapshot and canonical cumulative pytest fixtures. It
never installs tests into the ongoing workspace; Docker grading has no network
or model credentials. Canonical plugins, entry points and checkpoint timeouts
are frozen. Collection-only/import errors fail readiness rather than count as
normal unresolved tasks. Correct unresolved implementations remain valid scored
outcomes. Complete cost requires native requests/responses matched to proxy
response IDs; cumulative `result.json` usage is not summed. Missing/unanswered,
truncated or retried-request accounting is explicitly incomplete, with null full
cost and an observed completed-response lower bound retained separately.

Per `PROJECT/stage-N/`:

- `user-message.md` and its immutable digest
- authentic `workspace-before.json` / `workspace-after.json`: complete tree
  entries (directories, file sizes/content digests; links are recorded but reject
  snapshot grading), captured by the source runner before/after model execution
- `workspace/`: plain final snapshot, independently graded under `grade/`
- `session/trace.jsonl`: untouched **stage-local delta**, full final
  `session/context-state.json`, result/trace/tool-output files and numeric proxy log
- `stage-provenance.json`: actual source SHA/settings, stable project
  `workspace_key_sha256`, before/after tree hashes, final/source checkpoint hashes,
  raw trace hash, and `trace_creation_code: 1` for later fresh checkpoint-only
  destinations (0 initially). Native logging-only `trace_recovery` is preserved.

Every next before hash must equal its prior after hash. Source checkpoint hashes
must equal the prior final checkpoint hash; prompt-cache identity stays stable;
context generation remains zero. The offline adapter receives these actual raw
artifacts, not reconstructed workspace declarations. Missing evidence stays a
blocker. Historical planner-source comparisons are descriptive, not causal.

## CLI and credential-free validation

```sh
python scripts/slopbench.py reference-check --work /new/trusted-assets --output /new/reference
# Optional --image immutable-image: grade using its installed canonical plugins.
python scripts/slopbench.py identity --output /unused
python scripts/slopbench.py validate-preparation --output /extracted/preparation
python scripts/slopbench.py validate-report --output /extracted/attempt --attempt 1 --attempts 1
```

Worker subcommands `prepare-images`, `preflight`, `run` require `--source`,
`--work`, `--output`; `run` additionally accepts `--attempt`/`--attempts`.
Worker environment supplies `TASK_IMAGE_REPOSITORY`, `TASK_IMAGE_CATALOG`,
`SOURCE_COMMIT`, `RUN_ID` and fixed settings. Only `run` receives a model key.
Do not run publication/build subcommands locally as a substitute for protected CI.

CI installs `containers/slopbench/requirements.txt`, executes
`scripts.test_slopbench scripts.test_slop_workflow`, builds the dependency image,
and grades all reference/negative checks without model credentials. The existing
portable-harness image check additionally runs `scripts.test_slop_native` against
its extracted **candidate-built** binary (`CARRY_NATIVE_TEST_BINARY`). That fixture
uses a local fake provider to exercise four native resumes, persistent workspace,
byte-unchanged source sessions, retained terminal/tool history, stable cache key,
annotations, zero generation/compaction and stage-local usage. Fake provider data
are tests only, never pilot evidence.
