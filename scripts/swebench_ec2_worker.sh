#!/usr/bin/env bash
# Runs as EC2 user data after the workflow prepends immutable run configuration.
set -euo pipefail
umask 077
worker_started_at=$(date +%s)

: "${SOURCE_URL_B64:?}"
: "${SOURCE_SHA256:?}"
: "${SOURCE_COMMIT:?}"
: "${BENCHMARK_MODE:?}"
: "${BENCHMARK_HARNESS:=carry}"
: "${BOOTSTRAP_WAIT_SECONDS:?}"
: "${RUN_ID:?}"
: "${TASK_IMAGE_REPOSITORY:=}"
: "${TASK_IMAGE_CATALOG:=}"
: "${RESULT_URL_B64:=}"
: "${KEY_URL_B64:=}"
: "${DOCKER_AUTH_URL_B64:=}"
: "${REGISTRY_AUTH_URL_B64:=}"
: "${CONTROL_URL_B64:=}"
: "${MODEL:=gpt-5.6-luna}"
: "${REASONING:=medium}"
: "${CARRY_COMPACTION_POLICY:=economic}"
: "${CARRY_ROOT:=/opt/carry}"
: "${SECRET_FILE:=/dev/shm/carry-openai-key}"
DOCKER_AUTH_FILE=/dev/shm/carry-dockerhub-auth
REGISTRY_AUTH_FILE=/dev/shm/carry-task-registry-auth
DOCKER_CONFIG=/dev/shm/carry-docker-config
: "${PYTHON_BIN:=}"

result_url=$(printf '%s' "$RESULT_URL_B64" | base64 -d)
result_url_file=/dev/shm/carry-result-url
capability_refresh_pid=""
finish() {
  status=$?
  trap - EXIT
  if [[ -n "$capability_refresh_pid" ]]; then
    kill "$capability_refresh_pid" 2>/dev/null || true
    wait "$capability_refresh_pid" 2>/dev/null || true
  fi
  if [[ -s "$result_url_file" ]]; then
    result_url=$(<"$result_url_file")
  fi
  unset OPENAI_API_KEY
  rm -f "$SECRET_FILE" "$DOCKER_AUTH_FILE" "$REGISTRY_AUTH_FILE" "$result_url_file"
  rm -rf "$DOCKER_CONFIG"
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
case "$BENCHMARK_MODE" in
  smoke-5|session-smoke-5|session-20|official-50|prepare-50) ;;
  *) echo "unknown benchmark mode" >&2; exit 2 ;;
esac
if [[ "$BENCHMARK_MODE" =~ ^session-(smoke-5|20)$ && "$BENCHMARK_HARNESS" != carry && "$BENCHMARK_HARNESS" != codex && "$BENCHMARK_HARNESS" != pi ]]; then
  echo "retained-session modes require exactly one BENCHMARK_HARNESS=carry, codex, or pi" >&2
  exit 2
fi
[[ "$CARRY_COMPACTION_POLICY" =~ ^(economic|disabled)$ ]] || {
  echo "CARRY_COMPACTION_POLICY must be economic or disabled" >&2
  exit 2
}

docker_auth_url=$(printf '%s' "$DOCKER_AUTH_URL_B64" | base64 -d)
[[ -n "$docker_auth_url" ]] || { echo "missing Docker Hub authentication capability" >&2; exit 2; }
curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location \
  "$docker_auth_url" -o "$DOCKER_AUTH_FILE"
chmod 0600 "$DOCKER_AUTH_FILE"
export DOCKER_CONFIG
"$PYTHON_BIN" "$CARRY_ROOT/source/scripts/docker_registry_login.py" \
  "$DOCKER_AUTH_FILE" "$DOCKER_CONFIG"

if [[ "$BENCHMARK_MODE" == prepare-50 ]]; then
  registry_auth_url=$(printf '%s' "$REGISTRY_AUTH_URL_B64" | base64 -d)
  [[ -n "$registry_auth_url" && -n "${TASK_IMAGE_REPOSITORY:-}" ]] || {
    echo "missing task-registry publication configuration" >&2
    exit 2
  }
  curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location \
    "$registry_auth_url" -o "$REGISTRY_AUTH_FILE"
  chmod 0600 "$REGISTRY_AUTH_FILE"
  "$PYTHON_BIN" "$CARRY_ROOT/source/scripts/docker_registry_login.py" \
    "$REGISTRY_AUTH_FILE" "$DOCKER_CONFIG" public.ecr.aws
fi

control_url=$(printf '%s' "$CONTROL_URL_B64" | base64 -d)
if [[ -n "$control_url" ]]; then
  printf '%s' "$result_url" > "$result_url_file"
  chmod 0600 "$result_url_file"
  CONTROL_GET_URL="$control_url" RESULT_URL_FILE="$result_url_file" \
    "$PYTHON_BIN" "$CARRY_ROOT/source/scripts/refresh_benchmark_capabilities.py" &
  capability_refresh_pid=$!
fi

if [[ "$BENCHMARK_MODE" != prepare-50 ]]; then
  key_url=$(printf '%s' "$KEY_URL_B64" | base64 -d)
  [[ -n "$key_url" ]] || { echo "missing model credential capability" >&2; exit 2; }
  curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location \
    "$key_url" -o "$SECRET_FILE"
  chmod 0600 "$SECRET_FILE"
  export OPENAI_API_KEY
  OPENAI_API_KEY=$(cat "$SECRET_FILE")
  export OPENAI_SECRET_FILE="$SECRET_FILE"
fi

"$PYTHON_BIN" -m venv "$CARRY_ROOT/venv"
"$CARRY_ROOT/venv/bin/pip" install --disable-pip-version-check \
  'swebench==4.1.0' 'datasets>=2.19,<4'
export PATH="$CARRY_ROOT/venv/bin:$PATH"
export RUN_ID SOURCE_COMMIT MODEL REASONING BENCHMARK_MODE BENCHMARK_HARNESS CARRY_COMPACTION_POLICY CARRY_CONTEXT_PRESSURE_REMINDER_AT_TOKENS TASK_IMAGE_REPOSITORY TASK_IMAGE_CATALOG
export BASE_IMAGE='node@sha256:afff6d8c97964a438d2e6a9c96509367e45d8bf93f790ad561a1eaea926303d9'
export CARRY_BASE_IMAGE='rust@sha256:948f9b08a66e7fe01b03a98ef1c7568292e07ec2e4fe90d88c07bb14563c84ff'
export CODEX_VERSION='0.147.0'
export PI_VERSION='0.84.2'
export AGENT_TIMEOUT_SECONDS=360
export READINESS_TIMEOUT_SECONDS=180
export EVALUATOR_TIMEOUT_SECONDS=270
export AGENT_CONCURRENCY=3
export READINESS_CONCURRENCY=5

if [[ "$BENCHMARK_MODE" == official-50 ]]; then
  # Five concurrent Docker creates have been reliable; ten repeatedly saturated
  # the daemon and left half of a shard without evaluator outcomes.
  export AGENT_CONCURRENCY=5
  export EVALUATOR_CONCURRENCY=5
  export OFFICIAL_WORKER_SECONDS=18900
  export OFFICIAL_PREPARATION_PHASE_SECONDS=3000
  export OFFICIAL_AGENT_PHASE_SECONDS=4500
  export OFFICIAL_EVALUATION_PHASE_SECONDS=10200
  export OFFICIAL_SETUP_RESERVE_SECONDS=1200
  worker_elapsed_seconds=$(( $(date +%s) - worker_started_at ))
  OVERALL_TIMEOUT_SECONDS=$(( OFFICIAL_WORKER_SECONDS - worker_elapsed_seconds ))
  (( OVERALL_TIMEOUT_SECONDS > 0 )) || {
    echo "official worker budget exhausted during setup" >&2
    exit 124
  }
elif [[ "$BENCHMARK_MODE" == prepare-50 ]]; then
  export EVALUATOR_CONCURRENCY=5
  OVERALL_TIMEOUT_SECONDS=18000
elif [[ "$BENCHMARK_MODE" == session-smoke-5 ]]; then
  export AGENT_CONCURRENCY=1
  export EVALUATOR_CONCURRENCY=5
  OVERALL_TIMEOUT_SECONDS=3000
elif [[ "$BENCHMARK_MODE" == session-20 ]]; then
  export AGENT_CONCURRENCY=1
  export EVALUATOR_CONCURRENCY=5
  OVERALL_TIMEOUT_SECONDS=12600
else
  export EVALUATOR_CONCURRENCY=5
  OVERALL_TIMEOUT_SECONDS=3000
fi
runner_mode=--run
[[ "$BENCHMARK_MODE" == prepare-50 ]] && runner_mode=--prepare-images
timeout --signal=TERM --kill-after=30s "$OVERALL_TIMEOUT_SECONDS" python3 "$CARRY_ROOT/source/scripts/swebench_smoke.py" \
  "$runner_mode" --source "$CARRY_ROOT/source" --work "$CARRY_ROOT/work" \
  --output "$CARRY_ROOT/results" --harness "$BENCHMARK_HARNESS"
