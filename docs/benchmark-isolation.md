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

The existing default-branch workflow has only `workflow_dispatch`, uses the protected
`swe-bench` Environment, and preserves credential-free `bootstrap` as its default.
`smoke-5` resolves and archives the requested commit before AWS authentication, then
runs exactly the first five frozen IDs across three methods. It fails artifact validation
unless all 15 records exist, including explicit failed records with empty patches.

The `invoke` command is worker-side code. A future reviewed extension to the
existing EC2 dispatcher must copy it to the short-lived worker and invoke it
there; the dispatcher must terminate the instance after the slot/lane finishes.
No self-hosted or persistent GitHub runner is permitted.

The worker builds each run-local method image once. Carry is built from the archived
commit; Codex is fixed at `@openai/codex@0.147.0`; Pi is fixed at
`@earendil-works/pi-coding-agent@0.84.2`. Reviewed Node 22.19 and Rust base images are
pinned by manifest digest. The noninteractive command adapters are versioned with this
repository rather than supplied as mutable protected variables.

The canonical dataset is loaded at revision
`c104f840cc67f8b6eec6f759ebc8b2693d585d4a` and materialized as local JSON for
`swebench==4.1.0`. Official reports alone determine resolution. Evaluator processes have
all `OPENAI_*` variables removed and may use only host Docker and canonical task data.

## Remaining scaling blockers

Do not expand to the full 50 until smoke data establishes instance sizing, wall time,
token spend, evaluator image/cache pressure, and safe URL/job timeouts. A 50-task design
also needs reviewed concurrency and artifact aggregation while retaining the exact
150-record denominator and no selective retry/replacement policy.
