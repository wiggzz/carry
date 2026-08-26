#!/usr/bin/env python3
"""Create and validate the non-secret SWE-bench deployment manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import sys
from typing import Any


SCHEMA = "carry.swebench-deployment.v1"
BUCKET = re.compile(r"^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$")
REGION = re.compile(r"^[a-z]{2}-[a-z]+-\d+$")
ROLE_ARN = re.compile(r"^arn:aws:iam::\d{12}:role/[A-Za-z0-9+=,.@_/-]+$")
TEMPLATE = re.compile(r"^lt-[0-9a-f]+$")
REPOSITORY = re.compile(r"^public\.ecr\.aws/[a-z0-9][a-z0-9-]*/[a-z0-9][a-z0-9._/-]*$")


def fail(message: str) -> None:
    raise ValueError(message)


def string(value: Any, field: str) -> str:
    if not isinstance(value, (str, int)):
        fail(f"{field} must be a string")
    result = str(value)
    if not result:
        fail(f"{field} must not be empty")
    return result


def output_value(outputs: dict[str, Any], name: str) -> str:
    entry = outputs.get(name)
    if not isinstance(entry, dict) or "value" not in entry:
        raise ValueError(f"terraform output {name} is missing")
    return string(entry["value"], f"terraform output {name}")


def require(pattern: re.Pattern[str], value: str, field: str) -> str:
    if not pattern.fullmatch(value):
        fail(f"{field} is malformed")
    return value


def manifest_from_outputs(outputs: dict[str, Any], backend_bucket: str, backend_key: str, backend_region: str) -> dict[str, Any]:
    if not isinstance(outputs, dict):
        fail("terraform output must be a JSON object")
    backend_bucket = require(BUCKET, backend_bucket, "backend bucket")
    backend_region = require(REGION, backend_region, "backend region")
    if not re.fullmatch(r"carry/swebench-benchmark-infra/[a-z0-9-]+\.tfstate", backend_key):
        fail("backend key is malformed")
    artifact_bucket = require(BUCKET, output_value(outputs, "artifact_bucket_name"), "artifact bucket")
    dispatch_role = require(ROLE_ARN, output_value(outputs, "github_dispatch_role_arn"), "dispatch role")
    artifact_role = require(ROLE_ARN, output_value(outputs, "artifact_session_role_arn"), "artifact session role")
    publisher_role = require(ROLE_ARN, output_value(outputs, "task_image_publisher_role_arn"), "task image publisher role")
    template_id = require(TEMPLATE, output_value(outputs, "worker_launch_template_id"), "worker launch template ID")
    template_version = output_value(outputs, "worker_launch_template_version")
    if not re.fullmatch(r"[1-9][0-9]*", template_version):
        fail("worker launch template version is malformed")
    repository = require(REPOSITORY, output_value(outputs, "task_image_repository_uri"), "task image repository")
    return {
        "schema": SCHEMA,
        "terraform_backend": {"bucket": backend_bucket, "key": backend_key, "region": backend_region},
        "aws_region": backend_region,
        "artifact_bucket": artifact_bucket,
        "artifact_session_role_arn": artifact_role,
        "github_dispatch_role_arn": dispatch_role,
        "worker_launch_template_id": template_id,
        "worker_launch_template_version": template_version,
        "task_image_publisher_role_arn": publisher_role,
        "task_image_repository": repository,
    }


def write_manifest(arguments: argparse.Namespace) -> None:
    outputs = json.loads(pathlib.Path(arguments.terraform_output).read_text(encoding="utf-8"))
    manifest = manifest_from_outputs(outputs, arguments.backend_bucket, arguments.backend_key, arguments.backend_region)
    pathlib.Path(arguments.output).write_text(json.dumps(manifest, sort_keys=True, indent=2) + "\n", encoding="utf-8")


def manifest_value(document: dict[str, Any], name: str, pattern: re.Pattern[str]) -> str:
    value = document.get(name)
    return require(pattern, string(value, f"manifest {name}"), f"manifest {name}")


def resolve_manifest(arguments: argparse.Namespace) -> None:
    path = pathlib.Path(arguments.manifest)
    document = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(document, dict) or document.get("schema") != SCHEMA:
        fail("manifest schema is unsupported")
    backend = document.get("terraform_backend")
    if not isinstance(backend, dict):
        fail("manifest terraform_backend is missing")
    expected_backend = {
        "bucket": require(BUCKET, arguments.expected_backend_bucket, "expected backend bucket"),
        "key": arguments.expected_backend_key,
        "region": require(REGION, arguments.expected_backend_region, "expected backend region"),
    }
    if backend != expected_backend:
        fail("manifest backend does not match the constrained dispatch backend")
    region = manifest_value(document, "aws_region", REGION)
    if region != expected_backend["region"]:
        fail("manifest aws_region does not match its backend region")
    dispatch_role = manifest_value(document, "github_dispatch_role_arn", ROLE_ARN)
    if dispatch_role != arguments.expected_dispatch_role_arn:
        fail("manifest dispatch role does not match the protected bootstrap role")
    environment = {
        "AWS_REGION": region,
        "ARTIFACT_BUCKET": manifest_value(document, "artifact_bucket", BUCKET),
        "ARTIFACT_SESSION_ROLE_ARN": manifest_value(document, "artifact_session_role_arn", ROLE_ARN),
        "TASK_IMAGE_PUBLISHER_ROLE_ARN": manifest_value(document, "task_image_publisher_role_arn", ROLE_ARN),
        "TASK_IMAGE_REPOSITORY": manifest_value(document, "task_image_repository", REPOSITORY),
        "LAUNCH_TEMPLATE_ID": manifest_value(document, "worker_launch_template_id", TEMPLATE),
        "LAUNCH_TEMPLATE_VERSION": string(document.get("worker_launch_template_version"), "manifest worker_launch_template_version"),
        "CONFIGURATION_MANIFEST_SHA256": hashlib.sha256(path.read_bytes()).hexdigest(),
    }
    if not re.fullmatch(r"[1-9][0-9]*", environment["LAUNCH_TEMPLATE_VERSION"]):
        fail("manifest worker launch template version is malformed")
    if arguments.catalog_digest:
        catalog_digest = arguments.catalog_digest
        if re.fullmatch(r"[0-9a-f]{64}", catalog_digest):
            catalog_digest = f"sha256:{catalog_digest}"
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", catalog_digest):
            fail("catalog_digest must be a sha256 digest")
        environment["TASK_IMAGE_CATALOG"] = f"{environment['TASK_IMAGE_REPOSITORY']}@{catalog_digest}"
    for name in sorted(environment):
        print(f"{name}={environment[name]}")


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)
    write = commands.add_parser("write", help="write a deployment manifest from terraform output -json")
    write.add_argument("--terraform-output", required=True)
    write.add_argument("--backend-bucket", required=True)
    write.add_argument("--backend-key", required=True)
    write.add_argument("--backend-region", required=True)
    write.add_argument("--output", required=True)
    write.set_defaults(handler=write_manifest)
    resolve = commands.add_parser("resolve", help="validate a manifest and emit GitHub environment values")
    resolve.add_argument("--manifest", required=True)
    resolve.add_argument("--expected-backend-bucket", required=True)
    resolve.add_argument("--expected-backend-key", required=True)
    resolve.add_argument("--expected-backend-region", required=True)
    resolve.add_argument("--expected-dispatch-role-arn", required=True)
    resolve.add_argument("--catalog-digest", default="")
    resolve.set_defaults(handler=resolve_manifest)
    return root


def main() -> int:
    try:
        arguments = parser().parse_args()
        arguments.handler(arguments)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"benchmark deployment manifest: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
