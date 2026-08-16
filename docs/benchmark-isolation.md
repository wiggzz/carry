# Benchmark agent isolation policy

## Required boundary

Every credential-bearing benchmark agent harness must run behind a separately
reviewed **external isolation boundary**. This applies uniformly to Carry, Codex, and any future
harness; an in-process or harness-specific guard is not the sole containment mechanism.

The agent container/sandbox must have only the benchmark checkout and explicit
scratch/output paths writable. It must not mount the worker home directory,
cloud credentials, workflow temporary directories, or the host's Docker socket.
Model credentials, when enabled later, are injected only into that confined
agent boundary through the credential broker and never into the evaluator lane.
Raw model, AWS, or staging credentials are not container inputs. The broker
endpoint itself must vend run-scoped, short-lived access without exposing its
upstream credential to the worker or its artifacts.

## Separation of duties

The model-agent lane may produce a patch and compact run records. The evaluator
lane receives only the patch and required benchmark inputs; it **must not receive model credentials**.
SWE-bench's Docker testbed remains the evaluator's reproducibility boundary, not the agent's primary security boundary.

## Reviewable v1 contract

`scripts/swebench_live_runner.py plan` materializes the frozen 50-task by
Carry/Codex/Pi matrix as exactly 150 immutable slots. It reuses
`swebench_benchmark.py` for the selection hash, method set, and exact-denominator
validation. The manifest also describes two distinct external-container
contracts:

- agents receive only a checkout, one public task record, an output directory,
  and a broker socket name; and
- the evaluator receives only an independent checkout, that task record, the
  produced patch, and an output directory. Its environment is empty.

The manifest uses logical mount names, not host paths. A later implementation
must resolve those names in a disposable worker without mounting a host home,
the Docker socket, workflow temporary storage, or cloud/staging credentials.

The manually dispatched `SWE-bench live (blocked)` workflow uses the protected
`swe-bench` Environment, tests these contracts, and produces the plan locally.
It then calls `authorize-live`, which unconditionally fails because v1 contains
no credential broker. There is deliberately no secret input or execution
bypass. Do not add model invocation, Docker, AWS, or evaluation steps until a
separately reviewed broker protocol and container launcher replace that gate
with behavior tests proving isolation and credential non-disclosure.

## Defense in depth and verification

Carry Bubblewrap is useful defense in depth, but does not replace the external
boundary. Before enabling model credentials, test the selected container/sandbox
with negative checks showing that the agent cannot read host-only files, access
instance metadata or the Docker socket, or retain/recover model credentials from
artifacts or logs. The workflow must retain its protected Environment, exact
source ref, run-scoped artifact access, and `if: always()` worker cleanup.
