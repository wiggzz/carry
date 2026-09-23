#!/usr/bin/env python3
"""Retain bounded, allowlisted controller evidence; never persist raw EC2 output."""
import argparse
import json
import math
import os
from pathlib import Path
import re
import subprocess
import time

if __name__ == '__main__' and not __package__:
    from benchmark_progress import render as render_progress
else:
    from scripts.benchmark_progress import render as render_progress

MAX_EVENTS = 512
MAX_POLLS = 256
MAX_LINE = 8192
PHASES = {
    "bootstrap", "starting", "preparing", "building", "publishing", "complete",
    "failed", "finishing", "archiving", "uploading", "terminating", "running",
    "setup", "source", "dependencies", "benchmark", "cleanup", "unknown",
}
STRING_FIELDS = {
    "schema": {"carry.preparation-heartbeat.v1"},
    "dependency_status": {"unknown", "skipped", "running", "completed", "failed", "incomplete"},
    "latest_log_kind": {"none", "base", "env", "instances"},
    "latest_log_activity": {"unknown", "error", "solving", "downloading", "installing", "building"},
    "stage": {"worker_exit", "upload_complete", "upload_failed", "archive_failed", "finished",
              "starting", "bootstrap_config", "sanitize_failed", "log_drain_failed",
              "archive", "upload", "package_setup", "source_fetch", "source_ready",
              "credentials", "python_setup", "preparation", "benchmark"},
}
STATUSES = {
    "started", "running", "completed", "failed", "success", "pending", "built",
    "cached", "prepared", "published", "building", "skipped", "error",
    "agent-completed", "agent-failed", "task-timeout", "empty-patch", "empty_patch",
    "resolved", "unresolved", "evaluator-failed", "not-run", "evaluated",
    "evaluation-error", "evaluation-incomplete", "agent-budget-exhausted", "agent-session-context-missing",
}
STATES = {"pending", "running", "shutting-down", "terminated", "stopping", "stopped"}
AWS_ERRORS = {
    "AccessDenied", "AccessDeniedException", "UnauthorizedOperation", "AuthFailure",
    "ExpiredToken", "ExpiredTokenException", "InvalidClientTokenId", "RequestExpired",
    "InvalidInstanceID.NotFound", "InvalidInstanceID.Malformed", "RequestLimitExceeded",
    "Throttling", "ThrottlingException", "ServiceUnavailable", "InternalError",
}
NUMERIC_FIELDS = {
    "exit_code", "elapsed_seconds", "denominator", "completed", "total", "failed",
    "build_log_count", "build_log_bytes", "build_log_lines", "last_modified_age_seconds",
    "cpu_percent", "memory_used_bytes", "memory_available_bytes", "disk_used_bytes",
    "disk_available_bytes", "load_1m", "load_5m", "load_15m", "process_count",
    "observed_unix_seconds", "collection_errors", "log_count", "log_bytes", "last_log_age_seconds",
    "cached_count", "published_count", "failed_count", "blocked_by_environment_count", "pending_count",
    "disk_free_bytes", "disk_total_bytes", "load1", "mem_available_bytes",
}


def number(value):
    return type(value) in (int, float) and 0 <= value <= 10**15 and math.isfinite(value)


def safe_event(kind, event):
    """Reject unknown keys/strings and all unrecognized nesting, not just URLs."""
    if not isinstance(event, dict) or not event or len(event) > 40:
        return None
    for key, value in event.items():
        if key in NUMERIC_FIELDS:
            valid = number(value)
        elif key == "phase":
            valid = isinstance(value, str) and value in PHASES
        elif key == "status":
            valid = ((isinstance(value, str) and value in STATUSES)
                     or (kind == "BENCHMARK_WORKER" and type(value) is int and 0 <= value <= 255))
        elif key in STRING_FIELDS:
            valid = isinstance(value, str) and value in STRING_FIELDS[key]
        elif key == "status_counts":
            valid = (isinstance(value, dict) and len(value) <= len(STATUSES)
                     and all(k in STATUSES and type(v) is int and 0 <= v <= 100000
                             for k, v in value.items()))
        elif kind == "BENCHMARK_PROGRESS" and key == "instance_id":
            valid = isinstance(value, str) and bool(re.fullmatch(r"[A-Za-z0-9_.-]{1,160}", value))
        elif kind == "BENCHMARK_PROGRESS" and key == "harness":
            valid = isinstance(value, str) and value in {"carry", "codex", "pi"}
        elif kind == "BENCHMARK_PROGRESS" and key == "state":
            valid = isinstance(value, str) and value in {"started", "completed", "grading", "graded"}
        else:
            valid = False
        if not valid:
            return None
    if kind == "BENCHMARK_PROGRESS" and not {"instance_id", "harness", "state"} <= event.keys():
        return None
    return {"kind": kind, **event}


def console_events(console):
    for line in console[-1048576:].splitlines():
        if len(line) > MAX_LINE:
            continue
        for kind in ("BENCHMARK_WORKER", "BENCHMARK_PREPARATION", "BENCHMARK_PROGRESS"):
            marker = kind + " "
            if marker not in line:
                continue
            try:
                event = safe_event(kind, json.loads(line.partition(marker)[2]))
            except (ValueError, RecursionError):
                event = None
            if event:
                yield event
            break


def aws_read(arguments):
    """Use invocation-scoped dispatch credentials, leaving rotation state intact."""
    env = dict(os.environ)
    for name in ("AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN"):
        if "BASE_" + name in env:
            env[name] = env["BASE_" + name]
    env["AWS_PAGER"] = ""
    env["AWS_MAX_ATTEMPTS"] = "1"
    try:
        result = subprocess.run(
            ["aws", "ec2", *arguments, "--cli-connect-timeout", "5", "--cli-read-timeout", "10"],
            env=env, capture_output=True, text=True, timeout=20,
        )
    except subprocess.TimeoutExpired:
        return "", {"exit_code": 124, "error_code": "Timeout"}
    except OSError:
        return "", {"exit_code": 127, "error_code": "CommandUnavailable"}
    if result.returncode:
        match = re.search(r"An error occurred \(([^)]+)\)", result.stderr[:8192])
        code = match.group(1) if match and match.group(1) in AWS_ERRORS else "UnknownAwsError"
        return "", {"exit_code": result.returncode, "error_code": code}
    return result.stdout, {"exit_code": 0, "error_code": None}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--instance-id", default="")
    parser.add_argument("--archive-exit-code", type=int)
    parser.add_argument("--archive-http-status", type=int)
    parser.add_argument("--final", action="store_true")
    parser.add_argument("--outcome", choices=("archive-received", "worker-unavailable", "deadline-exceeded", "interrupted"))
    args = parser.parse_args()
    if args.output.exists():
        evidence = json.loads(args.output.read_text())
    else:
        evidence = {"schema": "carry.benchmark-worker-diagnostics.v1", "events": [], "polls": []}
    poll = {"observed_at_epoch": int(time.time()), "worker_state": "unknown"}
    if args.archive_exit_code is not None:
        poll["archive"] = {"exit_code": args.archive_exit_code, "http_status": args.archive_http_status}
    if re.fullmatch(r"i-[0-9a-f]{8,17}", args.instance_id):
        evidence["worker_instance_id"] = args.instance_id
        state, poll["describe_instances"] = aws_read([
            "describe-instances", "--instance-ids", args.instance_id, "--query",
            "Reservations[0].Instances[0].[InstanceId,State.Name,State.Code,StateReason.Code]",
            "--output", "json",
        ])
        if poll["describe_instances"]["exit_code"] == 0:
            try:
                identity, name, code, _reason = json.loads(state)
                if identity != args.instance_id or name not in STATES or type(code) is not int:
                    raise ValueError("invalid state")
                poll.update(worker_state=name, worker_state_code=code)
            except (ValueError, TypeError):
                poll["describe_instances"]["error_code"] = "InvalidResponse"
        console, poll["get_console_output"] = aws_read([
            "get-console-output", "--instance-id", args.instance_id, "--latest",
            "--query", "Output", "--output", "text",
        ])
        for event in console_events(console):
            if event not in evidence["events"]:
                evidence["events"].append(event)
                print(render_progress(event) if event['kind'] == 'BENCHMARK_PROGRESS'
                      else json.dumps(event, sort_keys=True))
        evidence["events"] = evidence["events"][-MAX_EVENTS:]
    else:
        poll["describe_instances"] = {"exit_code": None, "error_code": "WorkerNotLaunched"}
    evidence["polls"] = (evidence["polls"] + [poll])[-MAX_POLLS:]
    # Only successful exact-instance lifecycle reads establish unavailability.
    # Unknown/access-denied resets the consecutive-observation race grace.
    if not args.final:
        unavailable = (poll["worker_state"] in {"shutting-down", "terminated", "stopped"}
                       and args.archive_exit_code not in (None, 0))
        evidence["unavailable_polls"] = evidence.get("unavailable_polls", 0) + 1 if unavailable else 0
    if args.outcome:
        evidence["outcome"] = args.outcome
    args.output.parent.mkdir(parents=True, exist_ok=True)
    temporary = args.output.with_suffix(".tmp")
    temporary.write_text(json.dumps(evidence, sort_keys=True, indent=2) + "\n")
    temporary.replace(args.output)
    print("BENCHMARK_CONTROLLER " + json.dumps(poll, sort_keys=True))
    return 3 if not args.final and evidence.get("unavailable_polls", 0) >= 3 else 0


if __name__ == "__main__":
    raise SystemExit(main())
