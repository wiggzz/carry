#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
INFRA_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd)
PLAN_FILE=${PLAN_FILE:-"$INFRA_DIR/tfplan"}

[[ -f "$PLAN_FILE" ]] || { echo "saved plan not found: $PLAN_FILE; run scripts/plan.sh first" >&2; exit 66; }
terraform -chdir="$INFRA_DIR" show "$PLAN_FILE"
read -r -p "Type APPLY to apply this reviewed plan: " confirmation
[[ "$confirmation" == "APPLY" ]] || { echo "apply cancelled"; exit 0; }
terraform -chdir="$INFRA_DIR" apply "$PLAN_FILE"
