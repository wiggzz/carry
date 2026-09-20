"""Narrow preparation-only repairs for the pinned SWE-bench 4.1.0 recipes.

Call transform_test_specs once on fresh generated TestSpec objects, then pass the
returned objects (NOT dataset records) to build_instance_images. Persist its report
and include preparation_compatibility_sha256() in catalog/cache recipe identity.
Repaired instance tags are recipe-bound to prevent reuse of broken :latest images;
use the returned specs consistently when locating/retagging evaluator images.

No global harness constants, dataset records, evaluator commands, tests, or gold
patches are modified. Unexpected recipe fragments fail closed. These changes do
not establish successful image builds: remote rebuild/readiness is mandatory.
"""
from copy import deepcopy
import hashlib
import json
from pathlib import Path
from typing import Any, Iterable


SKLEARN_INSTALL = "python -m pip install -v --no-use-pep517 --no-build-isolation -e ."
# pip 26.0.1 is in the failed build log. Real pip parser probes reject its removed
# --no-use-pep517 flag and accept it under 25.2 with setuptools/wheel available.
PIP_PIN = "python -m pip install pip==25.2"
SKLEARN_ENV = [
    "source /opt/miniconda3/bin/activate",
    "conda create -n testbed python=3.9 'numpy==1.19.2' 'scipy==1.5.2' "
    "'cython==3.0.10' pytest 'pandas<2.0.0' 'matplotlib<3.9.0' setuptools "
    "pytest joblib threadpoolctl -y",
    "conda activate testbed",
    "python -m pip install cython setuptools numpy scipy",
]
# Matplotlib v3.5.0 setupext.py LOCAL_QHULL_HASH; independently downloaded
# mirror bytes match this upstream SHA-256 (1,298,874 bytes).
QHULL_SHA256 = "b5c2d7eb833278881b952c8a52d20179eab87766b00b865000469a45c1838b7e"
QHULL_URL = "https://sources.buildroot.net/qhull/qhull-2020-src-8.0.2.tgz"
QHULL_ORIGINAL = [
    'QHULL_URL="http://www.qhull.org/download/qhull-2020-src-8.0.2.tgz"',
    'QHULL_TAR="/tmp/qhull-2020-src-8.0.2.tgz"',
    'QHULL_BUILD_DIR="/testbed/build"',
    'wget -O "$QHULL_TAR" "$QHULL_URL"',
    'mkdir -p "$QHULL_BUILD_DIR"',
    'tar -xvzf "$QHULL_TAR" -C "$QHULL_BUILD_DIR"',
]
MPL_CONDA_VERSIONS = {"3.5", "3.6", "3.7", "3.8", "3.9"}
MPL_QHULL_VERSIONS = {"3.0", "3.1", "3.2", "3.3", "3.4"} | MPL_CONDA_VERSIONS


def preparation_compatibility_sha256() -> str:
    """Bind cache/catalog provenance to the complete local compatibility policy."""
    return hashlib.sha256(Path(__file__).read_bytes()).hexdigest()


def _sha256(payload: Any) -> str:
    return hashlib.sha256(
        json.dumps(payload, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def _recipe_sha256(spec: Any) -> str:
    # Deliberately exclude eval_script_list, F2P/P2P, and all gold-derived data.
    return _sha256({
        "env_script_list": spec.env_script_list,
        "repo_script_list": spec.repo_script_list,
    })


def _original_block_index(commands: list[str], block: list[str]) -> int:
    """Require one complete, contiguous upstream fragment (no partial rewrites)."""
    if any(commands.count(command) != 1 for command in block):
        raise ValueError("unexpected SWE-bench preparation recipe fragment")
    index = commands.index(block[0])
    if commands[index:index + len(block)] != block:
        raise ValueError("unexpected SWE-bench preparation recipe order")
    return index


def transform_test_specs(
    specs: Iterable[Any], *, swebench_version: str,
) -> tuple[list[Any], dict[str, Any]]:
    """Copy specs, validate known recipes, repair setup only, and return a report.

    swebench_version must come from importlib.metadata.version('swebench') at the
    integration boundary. This stdlib-only module does not import the harness.
    Repeat calls on fresh specs are deterministic; already-repaired input is an
    error, not a reason to silently insert a second compatibility layer.
    """
    if swebench_version != "4.1.0":
        raise ValueError("preparation compatibility requires swebench==4.1.0")
    result = deepcopy(list(specs))
    if len({spec.instance_id for spec in result}) != len(result):
        raise ValueError("preparation compatibility requires unique task IDs")
    fingerprint = preparation_compatibility_sha256()
    report: dict[str, Any] = {
        "schema_version": 1, "swebench_version": swebench_version,
        "compatibility_sha256": fingerprint, "tasks": {},
    }
    for spec in result:
        original_sha256 = _recipe_sha256(spec)
        repairs = []
        if spec.repo == "scikit-learn/scikit-learn" and spec.version in {"1.3", "1.4", "1.5", "1.6"}:
            if spec.env_script_list != SKLEARN_ENV or PIP_PIN in spec.repo_script_list:
                raise ValueError("unexpected scikit-learn preparation environment")
            index = _original_block_index(spec.repo_script_list, [SKLEARN_INSTALL])
            spec.repo_script_list.insert(index, PIP_PIN)
            repairs.append("sklearn-legacy-pip-25.2")
        if spec.repo == "matplotlib/matplotlib" and spec.version in MPL_CONDA_VERSIONS:
            # Work around the observed libsolv solver_addrule assertion without
            # removing/replacing packages. Classic solver success is not assumed.
            for old, new in (
                ("conda env create --file environment.yml",
                 "CONDA_SOLVER=classic conda env create --file environment.yml"),
                ("conda activate testbed && conda install python=3.11 -y",
                 "conda activate testbed && CONDA_SOLVER=classic conda install python=3.11 -y"),
            ):
                index = _original_block_index(spec.env_script_list, [old])
                spec.env_script_list[index] = new
            repairs.append("matplotlib-classic-solver")
        if spec.repo == "matplotlib/matplotlib" and spec.version in MPL_QHULL_VERSIONS:
            index = _original_block_index(spec.repo_script_list, QHULL_ORIGINAL)
            replacement = QHULL_ORIGINAL.copy()
            replacement[0] = f'QHULL_URL="{QHULL_URL}"'
            replacement[3:4] = [
                'timeout --kill-after=5s 120s wget --https-only --max-redirect=0 '
                '--timeout=30 --tries=3 -O "$QHULL_TAR" "$QHULL_URL"',
                f"printf '%s  %s\\n' '{QHULL_SHA256}' \"$QHULL_TAR\""
                " | sha256sum --check --strict -",
            ]
            spec.repo_script_list[index:index + len(QHULL_ORIGINAL)] = replacement
            repairs.append("matplotlib-qhull-https-sha256")
        effective_sha256 = _recipe_sha256(spec)
        if repairs:
            # Upstream instance keys do not hash repo_script_list (env keys do).
            # Keep repaired builds away from unmodified or previously built tags.
            spec.instance_image_tag = "compat-" + _sha256({
                "policy": fingerprint, "recipe": effective_sha256,
                "original_tag": spec.instance_image_tag,
            })[:24]
        report["tasks"][spec.instance_id] = {
            "repairs": repairs, "original_recipe_sha256": original_sha256,
            "effective_recipe_sha256": effective_sha256,
            "instance_image_tag": spec.instance_image_tag,
        }
    return result, report
