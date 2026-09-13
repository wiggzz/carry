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
export CARRY_FRONTIER_BINARY="$ROOT/target/release/carry"
export CARRY_FRONTIER_API_BASE=https://api.fireworks.ai/inference/v1
# Runta injects the real key only into egress. Its secret-rule API can omit the
# process-visible placeholder, so always provide a nonsecret value for SDKs that
# refuse to start without one; the egress rule replaces the outbound credential.
export FIREWORKS_API_KEY="${FIREWORKS_API_KEY:-runta-secret-stub}"

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
      --agent-import-path carry_frontierharness.pier_agent:CarryAgent \
      --environment-import-path carry_frontierharness.pier_environment:RuntaDockerEnvironment \
      --environment-kwarg runta_compose_file=/work/runta-ca-overlay.yaml \
      -m "$MODEL" \
      --jobs-dir "$JOBS_DIR"
    ;;
  *)
    echo "unsupported FrontierHarness suite: $SUITE" >&2
    exit 2
    ;;
esac
