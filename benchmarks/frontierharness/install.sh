#!/usr/bin/env bash
# Executed inside a clean Runta runtime by FrontierHarness provisioning.
set -euo pipefail

cargo build --locked --release
./target/release/carry --help >/dev/null 2>&1 || true
python3 benchmarks/frontierharness/test_adapter.py
python3 benchmarks/frontierharness/test_agents.py
python3 benchmarks/frontierharness/test_run_suite.py
chmod 0755 benchmarks/frontierharness/run-suite.sh
