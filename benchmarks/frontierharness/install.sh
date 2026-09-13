#!/usr/bin/env bash
# Executed inside a clean Runta runtime by FrontierHarness provisioning.
set -euo pipefail

# Cargo compiles native build scripts, so a clean Debian runtime also needs its C
# toolchain. Keep this here: the evaluator's base-tooling step is pinned upstream.
if ! command -v cc >/dev/null 2>&1; then
  command -v apt-get >/dev/null 2>&1 || { echo "cargo requires a C compiler and apt-get is unavailable" >&2; exit 1; }
  export DEBIAN_FRONTEND=noninteractive
  # Ubuntu mirrors occasionally return transient 5xx responses while a fresh
  # runtime is bootstrapping. Retry the idempotent refresh/install pair before
  # declaring the checkpoint unusable.
  for attempt in 1 2 3; do
    if apt-get update -qq && apt-get install -y -qq build-essential >/dev/null; then
      break
    fi
    if [[ "$attempt" == 3 ]]; then
      echo "cargo requires build-essential; apt-get failed after 3 attempts" >&2
      exit 1
    fi
    sleep "$attempt"
  done
fi
command -v cc >/dev/null 2>&1 || { echo "cargo requires a C compiler" >&2; exit 1; }

# A fully static musl binary runs in older task images as well as the modern
# checkpoint runtime. Install its linker separately because a host C compiler
# does not imply that musl-gcc is available.
if ! command -v musl-gcc >/dev/null 2>&1; then
  command -v apt-get >/dev/null 2>&1 || { echo "static Carry builds require musl-tools and apt-get is unavailable" >&2; exit 1; }
  export DEBIAN_FRONTEND=noninteractive
  for attempt in 1 2 3; do
    if apt-get update -qq && apt-get install -y -qq musl-tools >/dev/null; then
      break
    fi
    if [[ "$attempt" == 3 ]]; then
      echo "static Carry builds require musl-tools; apt-get failed after 3 attempts" >&2
      exit 1
    fi
    sleep "$attempt"
  done
fi
command -v musl-gcc >/dev/null 2>&1 || { echo "static Carry builds require musl-gcc" >&2; exit 1; }

# The FrontierHarness provisioning base image supplies Python tooling but not Rust.
# Bootstrap a minimal toolchain only when the runtime does not already provide one.
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  export PATH="$HOME/.cargo/bin:$PATH"
fi

rustup target add x86_64-unknown-linux-musl
# The cc crate otherwise guesses x86_64-linux-musl-gcc; Debian's musl-tools
# guarantees the portable musl-gcc wrapper instead.
export CC_x86_64_unknown_linux_musl=musl-gcc
cargo build --locked --release --target x86_64-unknown-linux-musl
./target/x86_64-unknown-linux-musl/release/carry --help >/dev/null 2>&1 || true
python3 benchmarks/frontierharness/test_adapter.py
python3 benchmarks/frontierharness/test_agents.py
python3 benchmarks/frontierharness/test_run_suite.py
chmod 0755 benchmarks/frontierharness/run-suite.sh
