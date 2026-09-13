#!/usr/bin/env bash
# Run the registered Carry adapter under the correct FrontierHarness suite runner.
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: $0 TASK SUITE MODEL JOBS_DIR" >&2
  exit 2
fi

TASK=$1
SUITE=$2
MODEL=$3
JOBS_DIR=$4
ROOT=/work/harness
export PYTHONPATH="$ROOT/benchmarks/frontierharness${PYTHONPATH:+:$PYTHONPATH}"
# This process runs on the restored Runta runtime. FIREWORKS_API_KEY is a Runta
# secret stub; the real value remains in the egress proxy and is never logged.
export CARRY_FRONTIER_BINARY="$ROOT/target/release/carry"
export CARRY_FRONTIER_API_BASE=https://api.fireworks.ai/inference/v1
: "${FIREWORKS_API_KEY:?FIREWORKS_API_KEY stub is required}"

case "$SUITE" in
  terminal-bench)
    exec harbor run \
      -d terminal-bench@2.0 -i "$TASK" \
      -a carry_frontierharness.harbor_agent:CarryAgent -m "$MODEL" \
      --jobs-dir "$JOBS_DIR" --extra-docker-compose /work/runta-ca-overlay.yaml \
      -r 2 -y
    ;;
  datacurve)
    exec pier run \
      -p "/work/deep-swe/tasks/$TASK" \
      --agent-import-path carry_frontierharness.pier_agent:CarryAgent -m "$MODEL" \
      --jobs-dir "$JOBS_DIR"
    ;;
  *)
    echo "unsupported FrontierHarness suite: $SUITE" >&2
    exit 2
    ;;
esac
