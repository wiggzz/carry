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

The readiness workflow has only a `workflow_dispatch` trigger and uses the
protected `swe-bench` Environment, but it is deliberately **credential-free**:
it writes and validates the 150-slot plan on a GitHub-hosted control-plane
runner. It cannot run as push or pull-request CI.

The `invoke` command is worker-side code. A future reviewed extension to the
existing EC2 dispatcher must copy it to the short-lived worker and invoke it
there; the dispatcher must terminate the instance after the slot/lane finishes.
No self-hosted or persistent GitHub runner is permitted.

## Remaining runtime blockers

This layer is reviewable but not deployable until operators:

- build and review dedicated agent images/adapters for **each** Carry, Codex, and
  Pi lane, plus the evaluator image containing the `/opt/swebench/evaluate-task`
  adapter (the included entrypoint currently implements Carry only);
- publish reviewed agent/evaluator images and make their immutable digest
  references available to the reviewed dispatcher as nonsecret run configuration;
- install Docker in the pinned disposable-worker AMI or its reviewed bootstrap,
  without registering a GitHub self-hosted runner;
- materialize selected task bundles containing the public record, prompt,
  disposable repository, and task-bundled evaluator testbed; and
- negative-test metadata-service access, host file access, operation without a
  Docker socket, and key retention in container outputs and logs.

The Dockerfiles require an explicit `BASE_IMAGE` build argument, preventing an
accidental build from a floating default. Building or reviewing images and
provisioning the worker are intentionally outside this change.
