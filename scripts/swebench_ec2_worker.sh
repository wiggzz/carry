#!/usr/bin/env bash
# Runs as EC2 user data after the workflow prepends immutable run configuration.
set -euo pipefail
umask 077

: "${SOURCE_URL_B64:?}"
: "${SOURCE_SHA256:?}"
: "${SOURCE_COMMIT:?}"
: "${BENCHMARK_MODE:?}"
: "${BOOTSTRAP_WAIT_SECONDS:?}"
: "${RUN_ID:?}"
: "${RESULT_URL_B64:=}"
: "${KEY_URL_B64:=}"
: "${MODEL:=gpt-5.6-luna}"
: "${REASONING:=medium}"
: "${CARRY_ROOT:=/opt/carry}"
: "${SECRET_FILE:=/dev/shm/carry-openai-key}"
: "${PYTHON_BIN:=}"

result_url=$(printf '%s' "$RESULT_URL_B64" | base64 -d)
finish() {
  status=$?
  trap - EXIT
  unset OPENAI_API_KEY
  rm -f "$SECRET_FILE"
  if [[ -n "$result_url" && -d "$CARRY_ROOT/results" ]]; then
    printf '%s\n' "$status" > "$CARRY_ROOT/results/worker-exit-status"
    tar -C "$CARRY_ROOT/results" -czf "$CARRY_ROOT/results.tar.gz" . || true
    curl --proto '=https' --tlsv1.2 --fail --silent --show-error \
      -X PUT -T "$CARRY_ROOT/results.tar.gz" "$result_url" || true
  fi
  [[ "${SKIP_SHUTDOWN:-0}" == 1 ]] || shutdown -h now
  exit "$status"
}
trap finish EXIT

mkdir -p "$CARRY_ROOT/results" "$CARRY_ROOT/source" "$CARRY_ROOT/work"
exec > >(tee -a "$CARRY_ROOT/results/worker.log") 2>&1
if command -v dnf >/dev/null 2>&1; then
  dnf install -y ca-certificates git python3.11 python3.11-pip docker tar gzip
  : "${PYTHON_BIN:=python3.11}"
elif command -v apt-get >/dev/null 2>&1; then
  # The worker security group permits HTTPS, not plaintext package mirrors.
  sed -i 's|http://|https://|g' /etc/apt/sources.list 2>/dev/null || true
  sed -i 's|http://|https://|g' /etc/apt/sources.list.d/*.sources 2>/dev/null || true
  apt-get update
  DEBIAN_FRONTEND=noninteractive apt-get install -y \
    ca-certificates curl git python3 python3-venv docker.io
  : "${PYTHON_BIN:=python3}"
else
  echo "unsupported worker operating system: dnf or apt-get required" >&2
  exit 2
fi
systemctl enable --now docker
"$PYTHON_BIN" - <<'PY'
import sys
if sys.version_info < (3, 10):
    raise SystemExit(f"Python >=3.10 is required, found {sys.version.split()[0]}")
PY

source_url=$(printf '%s' "$SOURCE_URL_B64" | base64 -d)
curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location --retry 3 \
  "$source_url" -o "$CARRY_ROOT/source.tar.gz"
printf '%s  %s\n' "$SOURCE_SHA256" "$CARRY_ROOT/source.tar.gz" | sha256sum -c -
tar -xzf "$CARRY_ROOT/source.tar.gz" -C "$CARRY_ROOT/source"

if [[ "$BENCHMARK_MODE" == bootstrap ]]; then
  sleep "$BOOTSTRAP_WAIT_SECONDS"
  exit 0
fi
[[ "$BENCHMARK_MODE" == smoke-5 ]] || { echo "unknown benchmark mode" >&2; exit 2; }

key_url=$(printf '%s' "$KEY_URL_B64" | base64 -d)
curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location \
  "$key_url" -o "$SECRET_FILE"
chmod 0600 "$SECRET_FILE"
export OPENAI_API_KEY
OPENAI_API_KEY=$(cat "$SECRET_FILE")
export OPENAI_SECRET_FILE="$SECRET_FILE"

"$PYTHON_BIN" -m venv "$CARRY_ROOT/venv"
"$CARRY_ROOT/venv/bin/pip" install --disable-pip-version-check \
  'swebench==4.1.0' 'datasets>=2.19,<4'
export PATH="$CARRY_ROOT/venv/bin:$PATH"
export RUN_ID SOURCE_COMMIT MODEL REASONING
export BASE_IMAGE='node@sha256:afff6d8c97964a438d2e6a9c96509367e45d8bf93f790ad561a1eaea926303d9'
export CARRY_BASE_IMAGE='rust@sha256:948f9b08a66e7fe01b03a98ef1c7568292e07ec2e4fe90d88c07bb14563c84ff'
export CODEX_VERSION='0.147.0'
export PI_VERSION='0.84.2'
export AGENT_TIMEOUT_SECONDS=360
export EVALUATOR_TIMEOUT_SECONDS=300
export AGENT_CONCURRENCY=3
export EVALUATOR_CONCURRENCY=5

timeout --signal=TERM --kill-after=30s 3000 python3 "$CARRY_ROOT/source/scripts/swebench_smoke.py" \
  --run --source "$CARRY_ROOT/source" --work "$CARRY_ROOT/work" --output "$CARRY_ROOT/results"
