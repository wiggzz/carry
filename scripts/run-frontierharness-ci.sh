#!/usr/bin/env bash
# Manual CI entry point for the pinned FrontierHarness Eval workflow.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=frontierharness_transport_retry.sh
source "$SCRIPT_DIR/frontierharness_transport_retry.sh"

FRONTIERHARNESS_COMMIT=e837a70bd6beb4e72eeeda62dd06e3bd34f6cb63

usage() {
  cat <<'EOF'
usage: run-frontierharness-ci.sh --mode MODE --source PATH --commit SHA --repo URL \
       --checkpoint NAME --run-id ID --out DIR [--tasks FILE]

MODE is one of: plan, provision, smoke-2, full-30.
`provision` creates a frozen checkpoint for the supplied immutable Carry commit.
`smoke-2` and `full-30` restore an existing checkpoint; they never rebuild it.
EOF
}

MODE=""
SOURCE=""
COMMIT=""
REPO=""
CHECKPOINT=""
RUN_ID=""
OUT=""
TASKS=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode) MODE=$2; shift 2 ;;
    --source) SOURCE=$2; shift 2 ;;
    --commit) COMMIT=$2; shift 2 ;;
    --repo) REPO=$2; shift 2 ;;
    --checkpoint) CHECKPOINT=$2; shift 2 ;;
    --run-id) RUN_ID=$2; shift 2 ;;
    --out) OUT=$2; shift 2 ;;
    --tasks) TASKS=$2; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

case "$MODE" in plan|provision|smoke-2|full-30) ;; *) echo "invalid --mode" >&2; exit 2 ;; esac
if [[ -n "$TASKS" && "$MODE" != smoke-2 && "$MODE" != full-30 ]]; then
  echo "--tasks is valid only for smoke-2 and full-30" >&2
  exit 2
fi
if [[ -n "$TASKS" && ! -r "$TASKS" ]]; then
  echo "--tasks must name a readable task list" >&2
  exit 2
fi
[[ "$COMMIT" =~ ^[0-9a-f]{40}$ ]] || { echo "--commit must be a full lowercase SHA" >&2; exit 2; }
git -C "$SOURCE" rev-parse --is-inside-work-tree >/dev/null 2>&1 \
  || { echo "--source must be a Carry checkout" >&2; exit 2; }
[[ "$(git -C "$SOURCE" rev-parse HEAD)" == "$COMMIT" ]] || { echo "--commit does not match the checked-out Carry source" >&2; exit 2; }
[[ "$CHECKPOINT" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{2,80}$ ]] || { echo "unsafe --checkpoint" >&2; exit 2; }

if [[ "$MODE" == plan ]]; then
  python3 "$SOURCE/benchmarks/frontierharness/test_adapter.py"
  python3 "$SOURCE/benchmarks/frontierharness/test_agents.py"
  python3 "$SOURCE/benchmarks/frontierharness/test_run_suite.py"
  python3 "$SOURCE/benchmarks/frontierharness/test_normalizer_cwd.py"
  python3 "$SOURCE/benchmarks/frontierharness/test_prepare_image.py"
  python3 "$SOURCE/benchmarks/frontierharness/test_shards.py"
  python3 "$SOURCE/benchmarks/frontierharness/test_transport_retry.py"
  python3 "$SOURCE/benchmarks/frontierharness/test_install.py"
  python3 "$SOURCE/benchmarks/frontierharness/test_ci_entrypoint.py"
  python3 "$SOURCE/benchmarks/frontierharness/test_merge_shards.py"
  printf '{"mode":"plan","carry_commit":"%s","frontierharness_commit":"%s","checkpoint":"%s"}\n' \
    "$COMMIT" "$FRONTIERHARNESS_COMMIT" "$CHECKPOINT"
  exit 0
fi

[[ -n "${RUNTA_TOKEN:-}" ]] || { echo "RUNTA_TOKEN is required for model-bearing/provisioning work" >&2; exit 2; }
[[ -n "${FIREWORKS_API_KEY:-}" ]] || { echo "FIREWORKS_API_KEY is required" >&2; exit 2; }
[[ -n "$REPO" ]] || { echo "--repo is required" >&2; exit 2; }
[[ -n "$OUT" ]] || { echo "--out is required" >&2; exit 2; }
mkdir -p "$OUT"

command -v runta >/dev/null || { echo "runta CLI is missing" >&2; exit 2; }
command -v jq >/dev/null || { echo "jq is missing" >&2; exit 2; }
runta checkpoint ls >/dev/null

FH="$OUT/frontierharness-eval"
# Fetch the evaluator separately at a fixed commit; $REPO is only ever passed to
# the pinned provisioning script as Carry's immutable source repository.
git clone --quiet https://github.com/frontier-harness-eval/eval.git "$FH"
git -C "$FH" checkout --quiet "$FRONTIERHARNESS_COMMIT"
python3 "$SOURCE/benchmarks/frontierharness/patch_usage_details.py" \
  --target "$FH/skills/frontierharness-eval/scripts/usage_details.py"
python3 "$SOURCE/benchmarks/frontierharness/patch_prepare_image.py" \
  --target "$FH/skills/frontierharness-eval/scripts/run-trials.sh"

if [[ "$MODE" == provision ]]; then
  RUNTIME="carry-fh-build-${COMMIT:0:12}-${RUN_ID}"
  RUNTIME=${RUNTIME:0:80}
  cleanup() { runta rm "$RUNTIME" >/dev/null 2>&1 || true; }
  trap cleanup EXIT
  run_with_frontierharness_ready_retries \
    bash "$FH/skills/frontierharness-eval/scripts/provision-golden-checkpoint.sh" \
    --runtime "$RUNTIME" --checkpoint "$CHECKPOINT" --harness carry \
    --provider fireworks --repo "$REPO" --commit "$COMMIT" \
    --cpus 4 --memory 8192 --disk-size-gib 50 --keep-runtime \
    --install-script "$SOURCE/benchmarks/frontierharness/install.sh"
  runta cp "$RUNTIME:/work/manifest.json" "$OUT/manifest.json"
  python3 - "$OUT/provision.json" "$COMMIT" "$FRONTIERHARNESS_COMMIT" "$CHECKPOINT" <<'PY'
import json, sys
from pathlib import Path
Path(sys.argv[1]).write_text(json.dumps({
    "carry_commit": sys.argv[2], "frontierharness_commit": sys.argv[3],
    "checkpoint": sys.argv[4],
}, indent=2) + "\n")
PY
  exit 0
fi

runta checkpoint ls --json | jq -e --arg name "$CHECKPOINT" \
  '.checkpoints[] | select(.display_name == $name and .state == "ready")' >/dev/null \
  || { echo "ready checkpoint not found: $CHECKPOINT" >&2; exit 2; }
[[ "$RUN_ID" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{2,120}$ ]] || { echo "unsafe --run-id" >&2; exit 2; }

if [[ -z "$TASKS" ]]; then
  if [[ "$MODE" == smoke-2 ]]; then
    TASKS="$SOURCE/benchmarks/frontierharness/smoke-2.txt"
  else
    TASKS="$FH/tasks"
  fi
fi

bash "$FH/skills/frontierharness-eval/scripts/run-trials.sh" \
  --checkpoint "$CHECKPOINT" --harness carry --provider fireworks \
  --run-id "$RUN_ID" --tasks "$TASKS" --out "$OUT/runs" \
  --cmd '/work/harness/benchmarks/frontierharness/run-suite.sh {task} {suite} {model} {jobs}'
(
  cd "$FH"
  node "skills/frontierharness-eval/scripts/normalize-results.mjs" \
    --run "$OUT/runs/$RUN_ID" --label "Carry $COMMIT"
)
