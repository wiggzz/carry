#!/usr/bin/env bash
# Executed inside a clean Runta runtime by FrontierHarness provisioning.
set -euo pipefail

# Cargo compiles native build scripts, so a clean Debian runtime also needs its C
# toolchain. Keep this here: the evaluator's base-tooling step is pinned upstream.
if ! command -v cc >/dev/null 2>&1; then
  command -v apt-get >/dev/null 2>&1 || { echo "cargo requires a C compiler and apt-get is unavailable" >&2; exit 1; }
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y -qq build-essential >/dev/null
fi
command -v cc >/dev/null 2>&1 || { echo "cargo requires a C compiler" >&2; exit 1; }

# The FrontierHarness provisioning base image supplies Python tooling but not Rust.
# Bootstrap a minimal toolchain only when the runtime does not already provide one.
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  export PATH="$HOME/.cargo/bin:$PATH"
fi

cargo build --locked --release
./target/release/carry --help >/dev/null 2>&1 || true
python3 benchmarks/frontierharness/test_adapter.py
python3 benchmarks/frontierharness/test_agents.py
python3 benchmarks/frontierharness/test_run_suite.py
chmod 0755 benchmarks/frontierharness/run-suite.sh
