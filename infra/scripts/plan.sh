#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
INFRA_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd)
TFVARS=${TFVARS:-"$INFRA_DIR/terraform.tfvars"}
PLAN_FILE=${PLAN_FILE:-"$INFRA_DIR/tfplan"}

[[ -f "$TFVARS" ]] || { echo "terraform variables file not found: $TFVARS" >&2; exit 66; }
terraform -chdir="$INFRA_DIR" fmt -check -recursive
terraform -chdir="$INFRA_DIR" validate
terraform -chdir="$INFRA_DIR" plan -input=false -var-file="$TFVARS" -out="$PLAN_FILE"
terraform -chdir="$INFRA_DIR" show "$PLAN_FILE"
