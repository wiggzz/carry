#!/bin/sh
set -eu

test "${1:-}" = evaluate
shift
test "${1:-}" = --instance-id
instance_id=${2:-}
shift 2
test "${1:-}" = --patch
patch=${2:-}
test -n "$instance_id"
test -r /benchmark/task/task.json
test -r "$patch"
test -w /benchmark/output

# The reviewed evaluator base image must provide this single-task adapter. It
# evaluates its task-bundled testbed directly; it must not require host Docker.
exec /opt/swebench/evaluate-task \
  --instance-id "$instance_id" \
  --task /benchmark/task/task.json \
  --repo /benchmark/task/repo \
  --patch "$patch" \
  --output /benchmark/output/result.json
