#!/bin/bash
set -euo pipefail
archive=/opt/swebench-prepared/testbed-overlay.tar
if [[ -s "$archive" ]]; then
  tar --extract --file "$archive" --directory /testbed --no-same-owner
fi
