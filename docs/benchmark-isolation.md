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
