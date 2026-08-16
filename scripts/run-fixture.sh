#!/bin/sh
set -eu

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  echo "usage: $0 <clamp|slugify|median|release-plan|config-loader> [live|scripted]" >&2
  exit 2
fi

task_name=$1
mode=${2:-live}
case "$task_name" in
  clamp|slugify|median|release-plan|config-loader) ;;
  *) echo "unknown task: $task_name" >&2; exit 2 ;;
esac
case "$mode" in
  live|scripted) ;;
  *) echo "mode must be live or scripted" >&2; exit 2 ;;
esac
if [ "$mode" = scripted ] && [ "$task_name" != clamp ]; then
  echo "only the clamp fixture has scripted steps" >&2
  exit 2
fi
if [ "$mode" = live ] && [ -z "${OPENAI_API_KEY:-}" ]; then
  echo "OPENAI_API_KEY is required for live mode" >&2
  exit 2
fi

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
timestamp=$(date -u +%Y%m%dT%H%M%SZ)
run_root="$repo_root/runs/${task_name}-${mode}-${timestamp}"
workspace="$run_root/workspace"
artifacts="$run_root/artifacts"
mkdir -p "$workspace" "$artifacts"
cp -R "$repo_root/fixtures/repo/." "$workspace/"
find "$workspace" -type f -name '*.pyc' -delete
python3 "$repo_root/fixtures/setup.py" "$task_name" "$workspace"
git -C "$workspace" init -q
git -C "$workspace" add .
git -C "$workspace" -c commit.gpgsign=false -c user.name=Carry -c user.email=carry@example.invalid commit -qm baseline

task_prompt=$(cat "$repo_root/fixtures/tasks/$task_name.md")
set -- docker run --rm \
  --user "$(id -u):$(id -g)" \
  --volume "$workspace:/workspace" \
  --volume "$artifacts:/run"

if [ "$mode" = live ]; then
  set -- "$@" --env OPENAI_API_KEY carry:dev \
    --cwd /workspace --session-dir /run -p "$task_prompt"
else
  set -- "$@" \
    --volume "$repo_root/fixtures/scripted/clamp.jsonl:/steps.jsonl:ro" \
    carry:dev --cwd /workspace --session-dir /run -p "$task_prompt" \
    --scripted-steps /steps.jsonl
fi

"$@"

docker run --rm \
  --entrypoint python \
  --volume "$workspace:/workspace:ro" \
  --volume "$repo_root/fixtures/graders/$task_name.py:/grader.py:ro" \
  carry:dev /grader.py /workspace

echo "run artifacts: $run_root"
