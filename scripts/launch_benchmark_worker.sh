#!/usr/bin/env bash
set -euo pipefail
: "${RUN_ID:?}"; : "${WORKER_LAUNCH_TEMPLATES:?}"; : "${WORKER_USER_DATA_FILE:?}"; : "${WORKER_TAGS:?}"
mapfile -t templates < <(python3 - <<'PY'
import json, os, re
items=json.loads(os.environ['WORKER_LAUNCH_TEMPLATES'])
if not isinstance(items,list) or not 2 <= len(items) <= 3: raise SystemExit('expected two or three launch-template candidates')
seen=set()
for item in items:
 az,template,version=(item.get(k) for k in ('availability_zone','launch_template_id','version'))
 if not all(isinstance(v,str) for v in (az,template,version)) or not re.fullmatch(r'[a-z]{2}-[a-z]+-\d[a-z]',az) or not re.fullmatch(r'lt-[0-9A-Za-z]+',template) or not re.fullmatch(r'[1-9][0-9]*',version) or az in seen: raise SystemExit('invalid or duplicate launch-template candidate')
 seen.add(az); print(f'{az}\t{template}\t{version}')
PY
)
attempted=()
for index in "${!templates[@]}"; do
 IFS=$'\t' read -r az template version <<< "${templates[$index]}"; attempted+=("$az")
 if output=$(aws ec2 run-instances --launch-template "LaunchTemplateId=$template,Version=$version" --count 1 --client-token "$RUN_ID-$((index+1))" --user-data "file://$WORKER_USER_DATA_FILE" --tag-specifications "ResourceType=instance,$WORKER_TAGS" "ResourceType=volume,$WORKER_TAGS" --query 'Instances[0].InstanceId' --output text 2>&1); then
  instance_id=$(printf '%s' "$output" | tail -n1); [[ "$instance_id" =~ ^i-[0-9a-f]+$ ]] || { echo "invalid worker instance ID" >&2; exit 1; }
  printf 'WORKER_INSTANCE_ID=%s\nWORKER_LAUNCH_ATTEMPTS=%s\n' "$instance_id" "$(IFS=,; echo "${attempted[*]}")"; exit 0
 fi
 [[ "$output" == *InsufficientInstanceCapacity* || "$output" == *InsufficientCapacityOnHost* ]] || { printf '%s\n' "$output" >&2; exit 1; }
 printf 'capacity unavailable in %s; trying next approved AZ\n' "$az" >&2
done
echo "no approved worker AZ has capacity: ${attempted[*]}" >&2; exit 1
