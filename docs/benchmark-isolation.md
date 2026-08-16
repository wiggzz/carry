# Benchmark agent isolation policy

## Required boundary

Every credential-bearing benchmark agent harness must run behind a separately
reviewed **external isolation boundary**. This applies uniformly to Carry, Codex, and any future
harness; an in-process or harness-specific guard is not the sole containment mechanism.

The agent container/sandbox must have only the benchmark checkout and explicit
scratch/output paths writable. It must not mount the worker home directory,
cloud credentials, workflow temporary directories, or the host's Docker socket.
Model credentials, when enabled later, are injected only into that confined
agent boundary and never into the evaluator lane.

## Separation of duties

The model-agent lane may produce a patch and compact run records. The evaluator
lane receives only the patch and required benchmark inputs; it **must not receive model credentials**.
SWE-bench's Docker testbed remains the evaluator's reproducibility boundary, not the agent's primary security boundary.

## Defense in depth and verification

Carry Bubblewrap is useful defense in depth, but does not replace the external
boundary. Before enabling model credentials, test the selected container/sandbox
with negative checks showing that the agent cannot read host-only files, access
instance metadata or the Docker socket, or retain/recover model credentials from
artifacts or logs. The workflow must retain its protected Environment, exact
source ref, run-scoped artifact access, and `if: always()` worker cleanup.
