#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
INFRA_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd)
BACKEND_CONFIG="$INFRA_DIR/backend.hcl"
TFVARS="$INFRA_DIR/terraform.tfvars"

[[ -f "$BACKEND_CONFIG" ]] || { echo "missing $BACKEND_CONFIG (copy backend.hcl.example and set the existing state bucket)" >&2; exit 66; }
[[ -f "$TFVARS" ]] || { echo "missing $TFVARS (copy terraform.tfvars.example and set account-specific values)" >&2; exit 66; }

terraform -chdir="$INFRA_DIR" init -reconfigure -input=false -backend-config="$BACKEND_CONFIG"
terraform -chdir="$INFRA_DIR" apply -input=false -auto-approve
