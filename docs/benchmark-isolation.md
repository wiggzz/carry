# Benchmark agent isolation policy

## Required boundary

Every credential-bearing harness runs as a separately reviewed external Docker
container. The agent container receives exactly a read-only task bundle and a
writable output directory. It receives no Docker socket, host home, Actions
temporary directory, runner source checkout, AWS credentials, or staging mount.

The protected GitHub Environment's `OPENAI_API_KEY` is forwarded by variable
name directly into the agent container. Its value is never placed in a Docker
argument, manifest, report, or evaluator environment.

## Separation and denominator

`scripts/swebench_live_runner.py` reuses the live planner to validate the frozen
50-task selection and all Carry/Codex/Pi pairs as exactly 150 records before a
single invocation. The agent and evaluator have distinct external-container
specifications:

- the agent receives the task bundle, its output directory, and only
  `OPENAI_API_KEY`; and
- the evaluator receives a copied task bundle containing the produced patch and
  its own output directory. It has an empty environment contract and never
  mounts agent output.

Both images require immutable `@sha256:` references. Docker receives the key as
`--env OPENAI_API_KEY`, copying the inherited value without putting it in the
command line. The evaluator Docker client is launched with that variable removed.
Run metadata contains only public identifiers, the selection hash, completion
status, and the two pinned image references.

## Manual protected workflow

The default-branch workflow has only `workflow_dispatch`, uses the protected
`swe-bench` Environment, and preserves credential-free `bootstrap` as its default.
`prepare-50` receives a short-lived public-ECR push token but no model credential;
`smoke-5` and `official-50` receive model credentials but have no registry-write or
AWS credentials. Every worker is disposable and the dispatcher terminates the exact
instance after the run.

The publisher computes a deterministic environment key, anonymously verifies cache
pairs, builds only missing official evaluator images and their thin sanitized agent
derivatives, runs networkless public-test readiness, and pushes the readiness-approved
agent tag last. Benchmark workers are pull-only: all selected evaluator/agent pairs
must resolve, pull, and pass identity checks before the first model process starts.
There is no build-on-cache-miss path in a model-bearing run.

`smoke-5` uses one frozen task from each repository family represented by the
official 50; fixed-denominator validation requires all selected task/harness records.
`harness=all` runs all three arms against one digest-pinned environment set.

### Independent official-50 attempts

`official-50` has an `attempts` input (default `1`, bounded to `1`–`10`). An
attempt count above one expands the same official lane into serial disposable workers,
one per declared attempt index. Every worker gets the same immutable candidate SHA,
catalog digest, 50-task manifest, model/reasoning configuration, and timeout/policy
inputs, while receiving a fresh EC2 instance, repositories, workspaces, containers,
evaluator run IDs, and an attempt-specific object prefix. No response retry within an
agent trajectory is counted as an independent attempt.

Each worker artifact contains exactly 150 records when `harness=all` (50 tasks × 3
harnesses × one declared attempt). Its record identity and evidence path include
`(instance_id, harness, attempt)` and `slots/<task>/<harness>/attempt-XX`, so retries
or subsequent attempts cannot overwrite a patch, evaluator report, or metadata. When
`attempts > 1`, an artifact-gated merge job accepts every declared complete worker
artifact and requires the exact combined attempt-record count before publishing
`report.json`, `records.json`, and `report.md`.

The combined report keeps both denominators explicit: `50 × attempts` executions per
harness and 50 unique task instances per harness. It reports every attempt record's
modeled model cost, runtime, status, and evidence location; per task/harness it reports
resolved attempts and resolve rate. Total modeled cost and elapsed execution are sums
across all attempts. Any task-level "resolved at least once" view is supplementary and
must be named as such; it is never silently substituted for the attempt-level rate.
Repeated results reduce variance uncertainty but do not create a normalized formal
leaderboard.

The worker builds each selected pinned harness image once. Carry is a portable static
executable from the archived commit; Codex is fixed at `@openai/codex@0.147.0`; Pi is
fixed at `@earendil-works/pi-coding-agent@0.84.2`. The worker exports exactly the
selected `/opt/swebench-harness` tree and mounts it read-only into each task container.
A task image contains no harness, so it is reusable across arms without creating a
task-by-harness derivative.

## Prepared environment contract

The public ECR catalog stores two related immutable manifests per task key: the
ordinary official instance image for trusted grading, and a sanitized agent derivative.
OCI layers deduplicate their common operating system and dependency content. The agent
derivative preserves only approved Git-ignored build products in a trusted overlay,
then removes `/testbed`, Git objects, and setup scripts. At agent launch `/testbed` is
a fresh base-ancestry-only checkout; the selected harness bundle is a separate read-only
mount. Registry credentials, other harnesses, hidden tests, and grading assets are never
mounted.

Readiness runs only in the credential-free publisher, in a disposable container with
`--network none`, no grading mounts, and no hidden test patch. The official parser must
observe at least one public test result. Baseline test failures are allowed; missing
runners, imports, plugins, parseable execution, package manifests, or images fail
publication. The `ready-*` tag is pushed only after readiness succeeds, and benchmark
runs record its resolved repository digest rather than trusting a tag as identity.

The canonical dataset is loaded at revision
`c104f840cc67f8b6eec6f759ebc8b2693d585d4a` and materialized as local JSON for
trusted preparation and grading only. Gold patches, hidden test patches, evaluator
assets, and canonical records are absent from agent mounts. Official reports alone
determine resolution. Evaluator processes have all `OPENAI_*` variables removed
and may use only host Docker and canonical task data.

## FrontierHarness Eval

`.github/workflows/run-frontierharness.yml` is a separate manual benchmark lane;
it never changes SWE-bench selection or execution. `carry_ref` is checked out and
resolved to a full SHA before any protected credential is available. That SHA—not a
branch name—is passed to the Runta provisioning script and recorded in artifacts.

The workflow has deliberately separate modes:

- `plan` is offline and exercises adapter-contract tests only.
- `provision` builds Carry once in a clean Runta runtime, freezes a named checkpoint,
  retrieves its manifest, and deletes that build runtime.
- `smoke-2` runs one Terminal-Bench and one DeepSWE task through the same hosted
  one-task shard and evidence-merge path used by the full run.
- `full-30` restores the same checkpoint for the complete published task set using
  deterministic two-task shards on GitHub-hosted runners. Shards run serially under
  the workflow-level checkpoint lock, so no EC2/self-hosted orchestrator remains
  running while Runta executes trials. The merger checks the frozen task-manifest
  SHA-256 and requires exactly one evidence record per task before normalization.

The checkpoint name is an explicit input: smoke/full must not silently rebuild or
retarget it. The protected `frontierharness` Environment supplies `RUNTA_TOKEN` and
`FIREWORKS_API_KEY`; Runta injects only a secret stub into the runtime and the actual
key is never a command-line value or artifact. Each task receives a fresh checkpoint
restore. Carry writes `trace.jsonl`, `result.json`, and `final.patch` under the task
agent log mount. The adapters preserve those files, mark an absent/crashed Carry run
as runtime evidence rather than rewriting a verifier result, and extract only actual
`model_response` usage from the trace for cost accounting.

The lane pins FrontierHarness Eval at
`e837a70bd6beb4e72eeeda62dd06e3bd34f6cb63`, Harbor `0.22.0`, Pier `0.3.1`, and
Runta CLI `0.2.4`. Scores are provisional until a matched Pi control uses the same
checkpoint resources, provider, task order, and model configuration.
