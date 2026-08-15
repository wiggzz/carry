#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 path/to/backend.hcl" >&2
  exit 64
fi

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
INFRA_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd)
BACKEND_CONFIG=$(cd -- "$(dirname -- "$1")" && pwd)/$(basename -- "$1")

[[ -f "$BACKEND_CONFIG" ]] || { echo "backend config not found: $BACKEND_CONFIG" >&2; exit 66; }
terraform -chdir="$INFRA_DIR" init -reconfigure -input=false -backend-config="$BACKEND_CONFIG"
