#!/usr/bin/env bash
# Persist an already verified local manifest after a Runta checkpoint freeze.

copy_verified_provision_manifest() {
  local source=$1
  local destination=$2
  local expected_commit=$3
  local expected_checkpoint=$4

  [[ -s "$source" ]] || { echo "provision manifest is missing or empty" >&2; return 1; }
  if ! python3 - "$source" "$expected_commit" "$expected_checkpoint" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    manifest = json.load(handle)
if manifest.get("harness_commit") != sys.argv[2] or manifest.get("checkpoint") != sys.argv[3]:
    raise SystemExit(1)
PY
  then
    echo "provision manifest identity does not match the requested checkpoint" >&2
    return 1
  fi
  mkdir -p "$(dirname "$destination")"
  cp "$source" "$destination"
  cmp -s "$source" "$destination" \
    || { echo "provision manifest copy verification failed" >&2; return 1; }
}
