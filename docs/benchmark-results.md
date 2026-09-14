# Benchmark evidence

Carry is experimental. These are artifact-gated observations from fixed task sets, not
leaderboard claims or evidence that one agent is generally better than another.

## Official SWE-bench Verified 50

All three agents ran the same 50 tasks in the same order at source
`763c04c1b40caa6b3c01a3eb2d9fc610c00805b3`, with SWE-bench Verified revision
`c104f840cc67f8b6eec6f759ebc8b2693d585d4a`, `gpt-5.6-luna`, medium reasoning,
and a 360-second task limit. Each run produced 50 predictions and completed
provenance and cleanup checks.

| Harness | Resolved | Modeled model cost | Workflow wall time |
| --- | ---: | ---: | ---: |
| [Carry](https://github.com/wiggzz/carry/actions/runs/32545967486) | 32 / 50 (64%) | $0.598024 | 40m44s |
| [Pi](https://github.com/wiggzz/carry/actions/runs/32549988183) | 36 / 50 (72%) | $0.661475 | 45m00s |
| [Codex](https://github.com/wiggzz/carry/actions/runs/32547842935) | 37 / 50 (74%) | $1.045054 | 46m38s |

Against Pi on this one matched catalog, Carry used **9.6% less modeled model
cost** and finished **9.5% sooner**, while resolving **8 percentage points fewer
tasks**. Model cost is artifact-recorded usage priced by the benchmark, not a
provider invoice; infrastructure cost is deliberately excluded from this table.

## FrontierHarness 30

Carry ran a frozen 30-task mix of Terminal-Bench and DataCurve at candidate
`fbafa2ad28bf1012dab17b11118c4d2016ad20c8`, using Kimi K3 through Fireworks and
checkpoint `carry-fh-fbafa2ad28bf`. The main workflow is
[34796266482](https://github.com/wiggzz/carry/actions/runs/34796266482). One
runtime's evidence transfer failed, then was recovered without model execution
through [34844989144](https://github.com/wiggzz/carry/actions/runs/34844989144).
The original artifact was not rewritten.

| Carry result | Value |
| --- | ---: |
| Resolved | 19 / 30 (63.3%) |
| Direct modeled token-cost lower bound | $25.9656411 |
| Direct modeled token cost / resolved task | $1.3666 |
| Median recorded agent duration | 5m51s |
| Workflow wall time | 5h19m45s |

The chart in the README places that run beside FrontierHarness's published
configurations. It is **directional only**: task IDs overlap, but agent/model
versions, evaluator/runtime/egress policy, timeout behavior, and verifier
semantics are not normalized. On the published Pi point, Carry is +3.3 points in
pass rate (63.3% versus 60.0%), 43.8% lower in direct modeled cost per resolved
task ($1.37 versus $2.43), and 22.6% lower in median agent duration (351s versus
453s). Those differences are not a controlled head-to-head comparison.

Published reference data: FrontierHarness `eval-data.json` commit
`96922ac653d073a16271929fc21033f95875f957`, generated 2026-08-22, SHA-256
`ffd18213a165e985fd7d876395a3f53e00961253a89bbc88420a41374bc20c4c`.

## Accounting and limits

- “Modeled model cost” applies the benchmark's pinned price table to recorded
  model usage; it is not provider billing.
- The FrontierHarness total is a lower bound. It excludes Runta, checkpoint and
  storage, GitHub Actions, and provider-billing differences.
- Neither task set is a universal quality ranking. Re-run matched, predeclared
  configurations before making causal or product-wide claims.
