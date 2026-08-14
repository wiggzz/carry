#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <clamp|slugify|median|release-plan|config-loader>" >&2
  exit 2
fi

task_name=$1
case "$task_name" in
  clamp|slugify|median|release-plan|config-loader) ;;
  *) echo "unknown task: $task_name" >&2; exit 2 ;;
esac

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
timestamp=$(date -u +%Y%m%dT%H%M%SZ)
run_root="$repo_root/runs/${task_name}-codex-${timestamp}"
workspace="$run_root/workspace"
artifacts="$run_root/artifacts"
model=${CODEX_MODEL:-gpt-5.6-luna}
reasoning_effort=${CODEX_REASONING_EFFORT:-medium}

mkdir -p "$workspace" "$artifacts"
cp -R "$repo_root/fixtures/repo/." "$workspace/"
find "$workspace" -type f -name '*.pyc' -delete
python3 "$repo_root/fixtures/setup.py" "$task_name" "$workspace"
git -C "$workspace" init -q
git -C "$workspace" add .
git -C "$workspace" \
  -c commit.gpgsign=false \
  -c user.name=Carry \
  -c user.email=carry@example.invalid \
  commit -qm baseline

codex exec \
  --cd "$workspace" \
  --model "$model" \
  --sandbox workspace-write \
  --ephemeral \
  --ignore-user-config \
  --ignore-rules \
  --color never \
  --json \
  --output-last-message "$artifacts/final.txt" \
  --config "model_reasoning_effort=\"$reasoning_effort\"" \
  - \
  < "$repo_root/fixtures/tasks/$task_name.md" \
  > "$artifacts/codex.jsonl" \
  2> "$artifacts/codex.stderr.log"

docker run --rm \
  --entrypoint python \
  --volume "$workspace:/workspace:ro" \
  --volume "$repo_root/fixtures/graders/$task_name.py:/grader.py:ro" \
  carry:dev /grader.py /workspace

echo "run artifacts: $run_root"
