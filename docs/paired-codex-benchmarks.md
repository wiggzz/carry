# Paired Codex raw / Carry benchmarks

These adapters compare **native Codex through a trusted direct gateway**
(`disabled`) with the same native Codex through Carry (`compact`) on SWE-bench
and the pinned official SlopCodeBench runner. `off` is an optional proxy
**diagnostic**, not raw Codex. Both arms retain scoped client authentication and
trusted native provider receipts. Ordinary disabled-mode callers are unchanged;
trusted direct is enabled only inside these benchmark workers. Protected paired
callers also freeze exact catalog primary/reviewer models at the gateway; a scoped
token cannot select another model or role. Ordinary gateway callers do not opt in.

Agent output import rejects symlinks, hardlinks, special files and oversized
artifacts on successful and failed worker paths. Receipt-capture loss remains
sticky: known charges survive as a lower bound but total cost stays unknown and
subsequent slots stop. SCB assigns each request to its first observed checkpoint
and reconciles later lifecycle receipts there, including the final capture.
Gold readiness is bound to the selected official pass policy; changing policy
requires new readiness. These checks do not replace a provider-side spending cap.

Preparation remains trusted operator input, not an authenticated attestation.
In particular, the SWE exported harness bundle is checked against the preparer's
bundle checksum while the native version/hash check runs inside the pinned image;
a trusted preparer can reseal a different export. This limitation is not agent
permission and is not qualified by synthetic execution.

No model results are included here. CI uses owned synthetic native executables,
tasks, and a TLS provider; it exercises the real adapters, network containment,
upstream runner and graders, but cannot establish savings, speed or quality on
real tasks. A compact request is not proof of a selected/dispatched/committed
rewrite. Reports keep unknown counters null and qualify rewrite-unproven runs.

## Safe first commands

From the clean pinned Carry checkout, these commands only print a plan:

```sh
python3 scripts/swebench_comparison.py --contract /absolute/swe-contract.json
python3 scripts/slopcodebench.py --contract /absolute/scb-contract.json
```

They validate the shared contract without creating workers, fetching a dataset,
starting Docker, or calling a provider. `--help` works without upstream packages.
The [existing SWE worker](../scripts/swebench_smoke.py) and
[proxy controls](proxy-benchmarking.md) remain the owners of prepared-image
catalogs, native adapters, trusted sidecars and receipt accounting.

Live commands below are **operator instructions, not authorization**. Use a
disposable isolated Linux worker with Docker, approved model credentials in its
trusted host environment, explicit instance selection and budget approval. Do
not run them on the constrained development host. Preparation may fetch public
artifacts or run gold tests, but never calls a model. Measured slots never build
or pull missing images. A missing runtime is a failed preflight, not readiness.

## Shared contract (schema_version 1)

A JSON object contains:

- `benchmark`: `swebench` or `slopcodebench`.
- `evidence_kind`: `measured-benchmark` for real runs; `synthetic-fixture` only
  for explicitly owned fixtures. Never relabel fixture output as a benchmark.
- `source_commit`, `runner_commit`, `dataset_commit`: full lowercase 40-hex
  immutable revisions. SWE's runner is the Carry adapter source revision and
  its dataset revision is `scripts/swebench_smoke.py:DATASET_REVISION`.
- `repetitions`: positive integer, at most 100.
- `selection`: ordered objects with `id` and ordered unique `checkpoints`.
  SWE checkpoints must be exactly `["final"]`; SCB must include the complete
  official problem checkpoint order (not a cherry-picked subset).
- `settings`: `codex_version`, `codex_sha256`, `prompt_sha256`,
  `environment_digest`, `harness_digest`, `gateway_digest`, `carry_digest`,
  `model`, `reasoning`, `auth_mode`, `native_config`, `timeout_seconds`,
  `retries`, `concurrency`. Digests are `sha256:` plus 64 lowercase hex digits;
  file hashes omit the prefix. Only scoped `api-key` transport is supported;
  subscription login is not silently treated as API billing. Native config is
  `{}` in this initial adapter, so unsupported knobs cannot be ignored.
- `proxy`: optional frozen existing `proxy_benchmark.validate_config` controls;
  `CARRY_PROXY_MODE` is prohibited here because the paired planner owns mode.

Both arms use the same source, client/version/hash, model, reasoning, native
compaction configuration, task/checkpoint sequence, environment, timeout,
retry policy and concurrency. SWE runs serially with zero outer retries. SCB
runs serially with zero to three official native retries. Every task/arm/repeat
has an independent namespace, workspace, gateway and accounting owner. Execution
is serial and deterministic direct-then-compact, so time/order effects are not
randomized away; use repetitions and interpret real results accordingly.

## SWE-bench preparation and execution

Install the pinned `swebench==4.1.0` evaluator in a disposable Python 3.12 venv.
Use existing preparation/catalog workflows to publish and qualify task images
and the prepared native Codex harness. The paired adapter **reuses** the existing
catalog resolver: recipe/cache-key/readiness checks, immutable agent image,
official evaluator alias and prepared-harness export are not reimplemented.

Create a JSON config with the existing worker's `TASK_IMAGE_CATALOG`,
`TASK_IMAGE_REPOSITORY`, `PREPARED_HARNESS_IMAGES`, `PREPARED_HARNESS_IMAGE`,
`CODEX_VERSION`, absolute `SOURCE_ROOT`, immutable `BASE_IMAGE` (gateway),
`CARRY_IMAGE` and associated preparation controls. The catalog reference must
have an immutable OCI digest; `settings.environment_digest` binds that digest,
while `harness_digest` is the resolved harness config ID. See the existing
worker for required catalog credentials and namespaces. Do not put provider
credentials in JSON files or artifacts.

Bind `prompt_sha256` to
`benchmark_comparison.fingerprint([[record["instance_id"], swebench_smoke.task_prompt(record)], ...])`
in the contract's canonical selected-record order. The canonical records are
loaded from the pinned official dataset, not user-edited public task JSON.
Bind `codex_sha256` to the native client's resolved launcher bytes in the
prepared harness (`readlink -f /opt/swebench-harness/bin/codex`); the harness
image digest also binds the packaged native binary and dependencies.

```sh
python scripts/swebench_comparison.py --contract /absolute/swe-contract.json \
  --prepare-runtime --config /absolute/worker-config.json \
  --output /absolute/new-swe-preparation
```

Preparation resolves/pulls qualified images and exports the harness, then
writes `prepared.json`; it never builds missing images or dispatches Codex.
The manifest contains `source_root`, `canonical_dataset`, `config`,
`harness_bundle`, `harness_bundle_sha256`, `gateway_image`, `carry_image`,
`evaluation_timeout_seconds` and `tasks` (the existing resolver's full results).
Retain it with the contract and catalog provenance; it is trusted operator
input, not an untrusted agent artifact.

After independent readiness and spend approval:

```sh
python scripts/swebench_comparison.py --contract /absolute/swe-contract.json \
  --prepared /absolute/new-swe-preparation/prepared.json --run --allow-paid \
  --budget-usd 10 --output /absolute/new-swe-paired-run
```

This invokes the existing native Codex adapter and isolated worker once per
slot, exports its patch and trusted receipt events, and uses the pinned official
SWE evaluator and existing parser compatibility shim. The paired evaluator
spawn boundary requires the prepared alias to still match its config ID,
disables network, and prohibits build/pull fallback. Provider environment
variables are removed from the evaluator process. Agent and grader remain
separate; gold/test patches and provider credentials are not mounted into the
agent. Eval artifacts and unresolved/infrastructure outcomes remain retained.

## Official SlopCodeBench preparation and execution

Pins live in [upstream.json](../benchmarks/slopcodebench/upstream.json):
runner `SprocketLab/slop-code-bench@af5a9cbc311605b5cceb80200ca9832bdff2d7b1`,
problems `gabeorlanski/scb-problems@9cd9ca3a51c3d3e2a99d2488a25baf73a2204451`.
Install the runner using its committed `uv.lock` and supported Python 3.12;
CI's standalone report component also uses 3.12 and Pydantic 2.11.7. Do not use
a newer inherited Python with an unsupported old Pydantic native extension.

Prepare a clean checkout of each pinned source, the exact upstream problem
trees (including tests/gold/assets), an upstream-compatible prompt template,
Docker environment YAML, and already-built immutable agent/evaluator/gateway/
Carry images. The evaluator image must include the official pinned pytest/uv
requirements in `/opt/scb-uv-cache`, so evaluation works offline. This adapter
does not run upstream's mutable catalog bootstrap or build-on-miss CLI.
`settings.scb_pass_policy` is one of the pinned official `PassPolicy` values;
freeze it before looking at model results (for example `any-case`).

SCB `prepared.json` has `source_root`, `upstream_root`, `problems_root`, `prompt`,
`environment`, `agent_image`, `evaluator_image`, `gateway_image`, `carry_image`
and `tasks`. Each selected task maps to `definition_sha256` (computed by
`slopcodebench_runtime.tree_digest` over its complete directory) and an absolute
`readiness` receipt path. Hash the prompt file into `prompt_sha256`; bind
agent/evaluator images to `harness_digest`/`environment_digest` respectively.
The environment must be Docker, with no extra host mounts, exposed ports or
inherited host environment. Native version/hash are checked inside the pinned
agent image. Images must exist locally before either command.

```sh
python scripts/slopcodebench.py --contract /absolute/scb-contract.json \
  --prepared /absolute/scb-prepared.json --prepare-readiness
python scripts/slopcodebench.py --contract /absolute/scb-contract.json \
  --prepared /absolute/scb-prepared.json --run --allow-paid \
  --budget-usd 10 --output /absolute/new-scb-paired-run
```

The first command runs the **official gold evaluator** on all selected
checkpoints, writes image/source/environment/definition-bound readiness receipts,
and makes zero model calls. The second accepts qualified prepared inputs and
explicit consent; it does not unconditionally refuse execution.

Execution uses the actual official `AgentRunner`, `CodexAgent`, streaming
runtime and checkpoint scorer, with a narrow protected runtime factory instead
of a copied problem loop or new scorer. Upstream preserves workspace and HOME
within one problem, but starts a **fresh native `codex exec` per checkpoint**.
Only official retry uses `exec resume --last`. Carry's conversation owner rotates
at each fresh native exec and stays unchanged for a retry. Gold/evaluation
containers have no network or provider credentials; solving containers see only
scoped gateway tokens and an internal gateway network. Cleanup fails closed.
The offline `--score-root` plus `--official-report-module` path is inspection
only, never evidence that tests or models were executed by that command.

## Outputs, accounting and limits

Both execution commands create a new exclusive output directory with the
frozen `plan.json`, slot artifacts, `records.jsonl` and shared `report.json`.
The denominator is fixed to selected task/checkpoint/arm/repetition slots;
failures and undispatched slots remain present. Missing or duplicate records
cannot inflate quality. Official resolved outcomes and SCB static quality
artifacts stay separate from timing and accounting.

Trusted native primary **and reviewer/shadow/retry** events feed the existing
`proxy_benchmark` accounting owner and model catalog. Missing/unpriced usage is
null/incomplete with a known lower bound, never silently zero. API-equivalent
estimated cost is not an observed subscription invoice. Native billing classes
(cache reads/writes, uncached input, output) and latency are retained. Setup,
agent, evaluation and end-to-end wall times are separate fields; overlapping
checkpoint timings must not be summed as a faster wall-time claim. Shared
paired deltas compare compact minus direct and qualify unknown scores,
infrastructure failures, changed quality and unproven rewrites.

`--budget-usd` is a **between-slot stop threshold, not a hard provider spending
cap**. An in-flight task/retry can exceed it. Unknown spend or evaluator failure
halts further dispatch while retaining fixed-denominator not-run records. Also
apply provider-side limits, per-task timeouts, explicit selection and operator
approval; this source change does not authorize paid runs or provision workers.
