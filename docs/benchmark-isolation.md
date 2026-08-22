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
runs exactly the first five frozen IDs with the selected harness. It fails artifact
validation unless all five records exist, including explicit failed records with empty
patches. `harness=all` runs all three arms against one prepared image set; single-harness
runs remain available for diagnostics.

The dispatcher copies worker-side code to a short-lived EC2 instance and always
terminates that exact instance after the run. No self-hosted or persistent GitHub
runner is permitted.

The worker builds all three pinned run-local harness images. Carry is built as a
portable static executable from the archived commit; Codex is fixed at
`@openai/codex@0.147.0`; Pi is fixed at
`@earendil-works/pi-coding-agent@0.84.2`. Reviewed Node 22.19 and Rust base images
are pinned by manifest digest. The noninteractive command adapters are versioned
with this repository rather than supplied as mutable protected variables.

## Prepared environment contract

Before model spend, trusted setup uses `swebench==4.1.0` to build each official
instance image with its ordinary repository dependencies. It captures the source
and derivative image IDs plus a complete conda package manifest. It creates one
derivative per harness from the same task-image parent and copies only that harness's
bundle, so an agent cannot invoke a competing harness. Git-ignored in-tree build
products, including compiled extensions, are retained in a trusted overlay; tracked
source, Git objects, and non-ignored files are excluded. Every derivative removes
`/testbed` and the setup scripts from the parent image. At agent launch,
`/testbed` is replaced by a fresh base-ancestry-only checkout, so editable installs
still resolve to the expected path without exposing the parent image's Git objects
or future history.

Trusted readiness runs in a disposable container with `--network none`, no model
credential, no grading mounts, and no hidden test patch. It activates the prepared
environment and launches the official public test path against a throwaway
base-only checkout. The official parser must observe at least one test result.
Baseline failures and timeouts after tests begin are diagnostic and acceptable;
missing runners, imports, plugins, parseable test execution, package manifests,
or images fail the run before any agent starts. Readiness workspaces are discarded.
Its command, output, timing, and dependency resolution remain in trusted artifacts
that are never mounted into an agent slot.

The canonical dataset is loaded at revision
`c104f840cc67f8b6eec6f759ebc8b2693d585d4a` and materialized as local JSON for
trusted preparation and grading only. Gold patches, hidden test patches, evaluator
assets, and canonical records are absent from agent mounts. Official reports alone
determine resolution. Evaluator processes have all `OPENAI_*` variables removed
and may use only host Docker and canonical task data.
