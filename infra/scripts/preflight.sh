#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
INFRA_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd)

terraform -chdir="$INFRA_DIR" fmt -check -recursive
terraform -chdir="$INFRA_DIR" init -backend=false -input=false
terraform -chdir="$INFRA_DIR" validate
python3 -m unittest -v "$INFRA_DIR/test_infra_contract.py" "$INFRA_DIR/test_watchdog.py"
