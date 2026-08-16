#!/usr/bin/env bash
# Wait for one EC2 instance to reach terminated without treating pending as fatal.
set -euo pipefail

INSTANCE_ID=${1:?usage: wait_for_ec2_termination.sh INSTANCE_ID}
POLL_SECONDS=${POLL_SECONDS:-5}
TIMEOUT_SECONDS=${TIMEOUT_SECONDS:-900}

[[ "$POLL_SECONDS" =~ ^[0-9]+$ ]] || { echo "POLL_SECONDS must be a non-negative integer" >&2; exit 2; }
[[ "$TIMEOUT_SECONDS" =~ ^[1-9][0-9]*$ ]] || { echo "TIMEOUT_SECONDS must be a positive integer" >&2; exit 2; }

deadline=$((SECONDS + TIMEOUT_SECONDS))
while (( SECONDS < deadline )); do
  state=$(aws ec2 describe-instances --instance-ids "$INSTANCE_ID" --query 'Reservations[0].Instances[0].State.Name' --output text 2>/dev/null || true)
  case "$state" in
    terminated|None|"")
      printf 'instance %s terminated\n' "$INSTANCE_ID"
      exit 0
      ;;
    pending|running|stopping|shutting-down|stopped)
      sleep "$POLL_SECONDS"
      ;;
    *)
      echo "unexpected instance state: $state" >&2
      exit 1
      ;;
  esac
done

echo "timed out waiting for instance termination: $INSTANCE_ID" >&2
exit 1
