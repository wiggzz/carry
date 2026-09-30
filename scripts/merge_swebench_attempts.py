#!/usr/bin/env python3
"""Merge artifact-gated independent official-50 attempts."""
from __future__ import annotations

import argparse
import importlib.util
import json
import pathlib
import re
import sys
from typing import Any


HARNESSES = ("carry", "codex", "pi")
MAX_ATTEMPTS = 10


IMMUTABLE_PROVENANCE_FIELDS = (
    "dataset", "dataset_revision", "swebench_version", "source_commit", "model", "reasoning",
    "carry_compaction_policy", "carry_keep_lease_turns", "carry_lease_review_policy", "carry_compaction_payoff_requests",
    "carry_compaction_min_payback_percent", "carry_compaction_rollout_samples", "carry_compaction_rollout_stop_probability_percent",
    "carry_compaction_neutral_high_watermark_tokens", "carry_compaction_neutral_low_watermark_tokens",
    "pricing_usd_per_million", "images", "harnesses",
)

# Reports produced before configurable neutral watermarks and payoff margin existed
# necessarily used their then-hard-coded defaults. Normalize only absent legacy fields
# so historical default-policy studies remain mergeable; explicit non-default values
# still differ from this identity and are rejected.
LEGACY_COMPACTION_DEFAULTS = {
    "carry_lease_review_policy": "baseline",
    # Absent means no sampled rollout for both pre-rollout and unified-forecast reports;
    # preserve explicit historical sample counts as an immutable treatment axis.
    "carry_compaction_rollout_samples": "0",
    "carry_compaction_neutral_high_watermark_tokens": "32768",
    "carry_compaction_neutral_low_watermark_tokens": "24576",
    "carry_compaction_min_payback_percent": "10",
}


IMMUTABLE_IMAGE_FIELDS = (
    "base_resolved_digest", "dockerfile_sha256", "package_version",
)


def immutable_images(images: Any) -> dict[str, Any]:
    """Keep run-local image tags/IDs out of cross-attempt identity."""
    if not isinstance(images, dict):
        raise ValueError("attempt provenance images must be a mapping")
    expected = set(HARNESSES) | {"execution_limits"}
    if set(images) != expected:
        raise ValueError("attempt provenance images must include every harness and execution limits")
    execution_limits = images["execution_limits"]
    if not isinstance(execution_limits, dict) or not execution_limits:
        raise ValueError("attempt provenance execution limits must be a nonempty mapping")
    normalized: dict[str, Any] = {"execution_limits": execution_limits}
    for harness in HARNESSES:
        image = images[harness]
        if not isinstance(image, dict):
            raise ValueError("attempt provenance images must map harness names to metadata")
        missing = [field for field in IMMUTABLE_IMAGE_FIELDS if field not in image]
        if missing:
            raise ValueError(
                "attempt provenance image is missing immutable fields: " + ", ".join(missing)
            )
        normalized[harness] = {field: image[field] for field in IMMUTABLE_IMAGE_FIELDS}
    return normalized


def normalize_legacy_provenance(provenance: dict[str, Any]) -> dict[str, Any]:
    normalized = dict(provenance)
    for key, value in LEGACY_COMPACTION_DEFAULTS.items():
        normalized.setdefault(key, value)
    return normalized


def immutable_provenance(provenance: dict[str, Any]) -> dict[str, Any]:
    normalized = normalize_legacy_provenance(provenance)
    missing = [key for key in IMMUTABLE_PROVENANCE_FIELDS if key not in normalized]
    if missing:
        raise ValueError(f"attempt provenance is missing immutable fields: {', '.join(missing)}")
    immutable = {key: normalized[key] for key in IMMUTABLE_PROVENANCE_FIELDS}
    immutable["images"] = immutable_images(normalized["images"])
    return immutable


def load_worker() -> Any:
    path = pathlib.Path(__file__).with_name("swebench_smoke.py")
    spec = importlib.util.spec_from_file_location("swebench_smoke", path)
    if spec is None or spec.loader is None:
        raise RuntimeError("could not load SWE-bench worker finalizer")
    worker = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(worker)
    return worker


def declared_attempt_numbers(total: int) -> tuple[int, ...]:
    if not 1 <= total <= MAX_ATTEMPTS:
        raise ValueError(f"attempts must be an integer from 1 through {MAX_ATTEMPTS}")
    return tuple(range(1, total + 1))


def selected_harnesses(value: str) -> tuple[str, ...]:
    if value == "all":
        return HARNESSES
    if value not in HARNESSES:
        raise ValueError("harness must be carry, codex, pi, or all")
    return (value,)


def task_ids(path: pathlib.Path) -> list[str]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    values = payload.get("instance_ids") if isinstance(payload, dict) else None
    if not isinstance(values, list) or len(values) != 50 or len(set(values)) != 50:
        raise ValueError("manifest must contain exactly 50 unique instance IDs")
    if not all(isinstance(value, str) and value for value in values):
        raise ValueError("manifest instance IDs must be nonempty strings")
    return values


def load_attempt_artifacts(root: pathlib.Path, *, tasks: list[dict[str, str]],
                           harnesses: tuple[str, ...], attempts: tuple[int, ...],
                           worker: Any) -> tuple[list[dict[str, Any]], list[str], dict[str, Any]]:
    paths = sorted(root.rglob("records.json"))
    if len(paths) != len(attempts):
        raise ValueError(f"expected exactly {len(attempts)} attempt artifacts")
    records: list[dict[str, Any]] = []
    observed_attempts: set[int] = set()
    candidate_commits: set[str] = set()
    immutable_identity: str | None = None
    canonical_provenance: dict[str, Any] | None = None
    sources: list[str] = []
    expected_denominator = len(tasks) * len(harnesses)
    for records_path in paths:
        report_path = records_path.with_name("report.json")
        if not report_path.is_file():
            raise ValueError(f"attempt artifacts must include report.json: {records_path}")
        report = json.loads(report_path.read_text(encoding="utf-8"))
        provenance = report.get("provenance") if isinstance(report, dict) else None
        if isinstance(provenance, dict):
            provenance = normalize_legacy_provenance(provenance)
        attempt_metadata = provenance.get("attempt") if isinstance(provenance, dict) else None
        attempt = attempt_metadata.get("number") if isinstance(attempt_metadata, dict) else None
        source_commit = provenance.get("source_commit") if isinstance(provenance, dict) else None
        if (report.get("denominator") != expected_denominator
                or report.get("attempt_numbers") != [attempt]
                or not isinstance(provenance, dict)
                or provenance.get("mode") != "official-50"
                or provenance.get("phase") != "complete"
                or not isinstance(attempt_metadata, dict)
                or attempt_metadata.get("total") != len(attempts)
                or attempt_metadata.get("independent_fresh_workspaces") is not True
                or not isinstance(source_commit, str)
                or not re.fullmatch(r"[0-9a-f]{40}", source_commit)
                or attempt not in attempts):
            raise ValueError(f"invalid official-50 attempt provenance: {records_path}")
        if candidate_commits and source_commit not in candidate_commits:
            raise ValueError("attempt artifacts must have identical candidate commits")
        candidate_commits.add(source_commit)
        immutable = immutable_provenance(provenance)
        identity = json.dumps(immutable, sort_keys=True, separators=(",", ":"))
        if immutable_identity is None:
            immutable_identity, canonical_provenance = identity, immutable
        elif immutable_identity != identity:
            raise ValueError("attempt artifacts must have identical immutable provenance")
        payload = json.loads(records_path.read_text(encoding="utf-8"))
        if not isinstance(payload, list):
            raise ValueError(f"attempt artifact records must be a list: {records_path}")
        worker._validate_records(tasks, payload, harnesses, (attempt,))
        if attempt in observed_attempts:
            raise ValueError("duplicate attempt artifacts")
        observed_attempts.add(attempt)
        records.extend(payload)
        sources.append(records_path.relative_to(root).as_posix())
    if observed_attempts != set(attempts):
        raise ValueError("attempt artifacts must contain every declared attempt exactly once")
    if len(candidate_commits) != 1 or canonical_provenance is None:
        raise ValueError("attempt artifacts must have identical candidate commits")
    return records, sources, canonical_provenance


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=pathlib.Path, required=True)
    parser.add_argument("--manifest", type=pathlib.Path, required=True)
    parser.add_argument("--harness", required=True, choices=(*HARNESSES, "all"))
    parser.add_argument("--attempts", type=int, required=True)
    parser.add_argument("--out", type=pathlib.Path, required=True)
    args = parser.parse_args()
    try:
        worker = load_worker()
        attempts = declared_attempt_numbers(args.attempts)
        ids = task_ids(args.manifest)
        harnesses = selected_harnesses(args.harness)
        tasks = [{"instance_id": instance_id} for instance_id in ids]
        records, sources, canonical_provenance = load_attempt_artifacts(
            args.artifacts, tasks=tasks, harnesses=harnesses, attempts=attempts, worker=worker,
        )
        worker.finalize(
            tasks=tasks, records=records, output=args.out,
            provenance={
                **canonical_provenance,
                "mode": "official-50", "phase": "complete",
                "attempt": {"total": len(attempts), "independent_fresh_workspaces": True},
                "source_record_artifacts": sources,
            },
            harnesses=harnesses, attempt_numbers=attempts,
        )
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
        print(f"official-50 attempt merge failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
