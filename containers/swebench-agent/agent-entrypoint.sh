#!/bin/sh
set -eu

test "${1:-}" = run
shift
test "${1:-}" = --method
method=${2:-}
shift 2
test "${1:-}" = --instance-id
instance_id=${2:-}
test -n "$instance_id"
test -n "${OPENAI_API_KEY:-}"
test -r /benchmark/task/task.json
test -d /benchmark/task/repo
test -w /benchmark/output

case "$method" in
  carry)
    exec carry run \
      --cwd /benchmark/task/repo \
      --task-file /benchmark/task/task.md \
      --run-dir /benchmark/output
    ;;
  *)
    echo "agent image does not contain the requested method: $method" >&2
    exit 64
    ;;
esac
