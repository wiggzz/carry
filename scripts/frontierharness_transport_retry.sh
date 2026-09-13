#!/usr/bin/env bash
# Give a newly created Runta runtime enough time to become callable before
# FrontierHarness applies its first idempotent secret/egress operation.
run_with_frontierharness_ready_retries() {
  FH_TRANSPORT_ATTEMPTS="${FH_TRANSPORT_ATTEMPTS:-12}" \
  FH_RETRY_DELAY="${FH_RETRY_DELAY:-5}" \
    "$@"
}
