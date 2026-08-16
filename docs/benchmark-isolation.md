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

The live workflow has only a `workflow_dispatch` trigger, uses the protected
`swe-bench` Environment, and targets a self-hosted worker labeled
`swebench-disposable`; it cannot run as push or pull-request CI. One dispatch
invokes one selected task/method. Its task must already exist at
`/opt/swebench-tasks/<instance-id>`, and the workflow does not mount its checkout.

## Remaining runtime blockers

This layer is reviewable but not deployable until operators:

- build and review dedicated agent and evaluator images, including Carry and the
  `/opt/swebench/evaluate-task` adapter;
- publish them and set immutable digest references in protected Environment
  variables `SWEBENCH_AGENT_IMAGE` and `SWEBENCH_EVALUATOR_IMAGE`;
- install Docker in the disposable worker AMI and register that worker with only
  the `swebench-disposable` label;
- materialize selected task bundles containing the public record, prompt,
  disposable repository, and task-bundled evaluator testbed; and
- negative-test metadata-service access, host file access, operation without a
  Docker socket, and key retention in container outputs and logs.

The Dockerfiles require an explicit `BASE_IMAGE` build argument, preventing an
accidental build from a floating default. Building or reviewing images and
provisioning the worker are intentionally outside this change.
