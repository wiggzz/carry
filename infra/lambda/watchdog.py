"""Terminate expired Carry SWE-bench workers; invoked every five minutes."""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
import json
import logging
import os

import boto3

LOG = logging.getLogger()
LOG.setLevel(os.environ.get("LOG_LEVEL", "INFO"))
MAX_RUNTIME = timedelta(minutes=int(os.environ["MAX_WORKER_RUNTIME_MINUTES"]))
REQUIRED_TAGS = {
    "ManagedBy": "carry-swebench",
    "Project": "carry-swebench",
    "Purpose": "benchmark-worker",
}


def parse_expiry(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00")).astimezone(timezone.utc)


def instance_tags(instance: dict) -> dict[str, str]:
    return {tag["Key"]: tag["Value"] for tag in instance.get("Tags", [])}


def should_terminate(instance: dict, now: datetime) -> tuple[bool, str]:
    tags = instance_tags(instance)
    if any(tags.get(key) != value for key, value in REQUIRED_TAGS.items()):
        return False, "tag-mismatch"

    state = instance.get("State", {}).get("Name")
    if state not in {"pending", "running", "stopping", "stopped"}:
        return False, f"state-{state}"

    launch_time = instance.get("LaunchTime")
    if not isinstance(launch_time, datetime):
        return False, "missing-launch-time"
    launch_deadline = launch_time.astimezone(timezone.utc) + MAX_RUNTIME

    expires_at = tags.get("ExpiresAt")
    if expires_at:
        try:
            deadline = min(launch_deadline, parse_expiry(expires_at))
        except ValueError:
            LOG.warning("invalid ExpiresAt=%s on %s; enforcing launch deadline", expires_at, instance.get("InstanceId"))
            deadline = launch_deadline
    else:
        deadline = launch_deadline

    return now >= deadline, "expired" if now >= deadline else "not-expired"


def target_instances(ec2, instance_id: str | None) -> list[dict]:
    if instance_id:
        response = ec2.describe_instances(InstanceIds=[instance_id])
    else:
        response = ec2.describe_instances(
            Filters=[
                {"Name": "tag:ManagedBy", "Values": [REQUIRED_TAGS["ManagedBy"]]},
                {"Name": "tag:Project", "Values": [REQUIRED_TAGS["Project"]]},
                {"Name": "tag:Purpose", "Values": [REQUIRED_TAGS["Purpose"]]},
            ]
        )
    return [item for reservation in response.get("Reservations", []) for item in reservation.get("Instances", [])]


def lambda_handler(event: dict, _context: object) -> dict:
    instance_id = event.get("instance_id")
    if instance_id is not None and (not isinstance(instance_id, str) or not instance_id):
        raise ValueError("event.instance_id must be a non-empty string when supplied")

    ec2 = boto3.client("ec2")
    now = datetime.now(timezone.utc)
    instances = target_instances(ec2, instance_id)
    expired_ids: list[str] = []

    for instance in instances:
        terminate, reason = should_terminate(instance, now)
        current_id = instance.get("InstanceId")
        if terminate and isinstance(current_id, str):
            expired_ids.append(current_id)
        else:
            LOG.info("retaining %s: %s", current_id, reason)

    if expired_ids:
        ec2.terminate_instances(InstanceIds=expired_ids)
        LOG.info("terminated expired workers: %s", expired_ids)

    return {"terminated": expired_ids, "inspected": len(instances)}


if __name__ == "__main__":
    print(json.dumps(lambda_handler(json.loads(os.environ["WATCHDOG_EVENT"]), None)))
