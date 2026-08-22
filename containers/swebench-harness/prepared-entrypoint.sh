#!/bin/bash
set -euo pipefail

/usr/local/bin/apply-testbed-overlay
source /opt/miniconda3/bin/activate testbed
exec /usr/bin/python3 /opt/swebench-harness/bin/adapter "$@"
