#!/usr/bin/env bash
# Resume a retained trial runtime's evidence transfer. This never launches a task.
set -euo pipefail

MODEL="fireworks_ai/accounts/fireworks/models/kimi-k3"
CMD_TEMPLATE="/work/harness/benchmarks/frontierharness/run-suite.sh {task} {suite} {model} {jobs}"

usage() {
  cat <<'EOF'
usage: recover-frontierharness-evidence.sh --source PATH --evaluator PATH --task ID \
       --runtime ID --checkpoint NAME --run-id ID --out DIR

Resumes only a task listed in the source's frozen tasks-30 manifest. It refuses a
provider API key and delegates to the pinned evaluator's documented same-run
recovery path; it never restores a checkpoint or launches an agent.
EOF
}

SOURCE=""
EVALUATOR=""
TASK=""
RUNTIME=""
CHECKPOINT=""
RUN_ID=""
OUT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --source) SOURCE=$2; shift 2 ;;
    --evaluator) EVALUATOR=$2; shift 2 ;;
    --task) TASK=$2; shift 2 ;;
    --runtime) RUNTIME=$2; shift 2 ;;
    --checkpoint) CHECKPOINT=$2; shift 2 ;;
    --run-id) RUN_ID=$2; shift 2 ;;
    --out) OUT=$2; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

[[ "$TASK" =~ ^[A-Za-z0-9_-]+/[A-Za-z0-9_-]+$ ]] || { echo "unsafe --task" >&2; exit 2; }
[[ "$RUNTIME" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{2,80}$ ]] || { echo "unsafe --runtime" >&2; exit 2; }
[[ "$CHECKPOINT" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{2,80}$ ]] || { echo "unsafe --checkpoint" >&2; exit 2; }
[[ "$RUN_ID" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{2,120}$ ]] || { echo "unsafe --run-id" >&2; exit 2; }
[[ -f "$SOURCE/benchmarks/frontierharness/tasks-30.txt" ]] || { echo "frozen task manifest is missing" >&2; exit 2; }
python3 - "$SOURCE/benchmarks/frontierharness/tasks-30.txt" "$TASK" <<'PY'
import sys
manifest, task = sys.argv[1:]
tasks = {line.split('#', 1)[0].strip() for line in open(manifest, encoding='utf-8')}
if task not in tasks:
    raise SystemExit(f"{task} is not in the frozen task manifest")
PY
[[ -n "$OUT" ]] || { echo "--out is required" >&2; exit 2; }
[[ -x "$EVALUATOR/skills/frontierharness-eval/scripts/run-trials.sh" ]] \
  || { echo "pinned evaluator run-trials.sh is missing or not executable" >&2; exit 2; }
[[ -z "${FIREWORKS_API_KEY:-}" ]] \
  || { echo "FIREWORKS_API_KEY must be absent for copy-only recovery" >&2; exit 2; }
[[ -n "${RUNTA_TOKEN:-}" ]] || { echo "RUNTA_TOKEN is required to retrieve retained evidence" >&2; exit 2; }
mkdir -p "$OUT"
TASKS=$(mktemp "$OUT/recovery-tasks.XXXXXX")
trap 'rm -f "$TASKS"' EXIT
printf '%s\n' "$TASK" > "$TASKS"

SUITE=${TASK%%/*}
TASK_NAME=${TASK#*/}
SLUG=${TASK//\//-}
RUNNER_COMMAND="/work/harness/benchmarks/frontierharness/run-suite.sh '$TASK_NAME' '$SUITE' '$MODEL' '/work/jobs/$SLUG'"
RUN_DIR="$OUT/runs/$RUN_ID"
TRIAL_DIR="$RUN_DIR/trials/$SLUG"
mkdir -p "$TRIAL_DIR"
python3 - "$TRIAL_DIR/trial.json" "$TASK" "$RUNTIME" "$CHECKPOINT" "$RUNNER_COMMAND" <<'PY'
import json
import sys
from pathlib import Path

Path(sys.argv[1]).write_text(json.dumps({
    "id": sys.argv[2], "title": sys.argv[2].split("/", 1)[1], "suite": sys.argv[2].split("/", 1)[0],
    "status": "infra_invalid", "success": False, "runtime": sys.argv[3],
    "checkpoint": sys.argv[4],
    "error": "evidence transfer incomplete; runtime retained for recovery",
    "recovery": True, "runner_command": sys.argv[5],
}, indent=2) + "\n")
PY

# In the evaluator, a recovery=true trial reconnects to the recorded runtime, waits
# only for its existing completion marker, bundles/copies evidence, then scores it.
# The provider key is absent and the resume branch does not create egress/secret rules.
"$EVALUATOR/skills/frontierharness-eval/scripts/run-trials.sh" \
  --checkpoint "$CHECKPOINT" --harness carry --provider fireworks --model "$MODEL" \
  --run-id "$RUN_ID" --tasks "$TASKS" --out "$OUT/runs" \
  --cmd "$CMD_TEMPLATE" --timeout 5400

python3 - "$TRIAL_DIR" <<'PY'
import json
import sys
from pathlib import Path

trial_dir = Path(sys.argv[1])
trial = json.loads((trial_dir / "trial.json").read_text())
if trial.get("status") not in {"success", "failure"} or trial.get("recovery") is True:
    raise SystemExit("upstream resumption did not produce complete evidence")
for name in ("completion.json", "runner.log", "manifest.json"):
    if not (trial_dir / name).is_file():
        raise SystemExit(f"upstream resumption did not produce complete evidence: missing {name}")
if not (trial_dir / "jobs").is_dir():
    raise SystemExit("upstream resumption did not produce complete evidence: missing jobs")
PY
archive_sha256=$(sha256sum "$TRIAL_DIR/trial.json" | awk '{print $1}')
python3 - "$OUT/recovery.json" "$TASK" "$RUNTIME" "$CHECKPOINT" "$RUN_ID" "$archive_sha256" <<'PY'
import json
import sys
from pathlib import Path

Path(sys.argv[1]).write_text(json.dumps({
    "operation": "existing-runtime-evidence-collection", "model_execution": False,
    "task": sys.argv[2], "runtime": sys.argv[3], "checkpoint": sys.argv[4],
    "run_id": sys.argv[5], "trial_json_sha256": sys.argv[6],
}, indent=2) + "\n")
PY
