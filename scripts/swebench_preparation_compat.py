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

# The only solver replacement is this exact immutable 4.1.0 task recipe.
# A successful hosted dry-run is NOT build/readiness proof. The real image
# resolves against Ubuntu 22.04's virtual packages, never the hosted plan.
MPL_SOLVER_TASK = "matplotlib__matplotlib-24627"
MPL_SOLVER_RECIPE_SHA256 = "30bc9cbdb163ff1aeb1c60c906fddd65f8791f8cf45c5e6328c9d0cb857c7fb0"
MPL_SOLVER_URL = ("https://github.com/mamba-org/micromamba-releases/releases/"
                  "download/2.3.3-0/micromamba-linux-64")
MPL_SOLVER_SHA256 = "9496f94a8b78c536573c93d946ec9bba74bd9ff79ee55aaa4b546e30db8f511b"


def _repair_matplotlib_solver(spec: Any, original_sha256: str) -> None:
    if (spec.repo != "matplotlib/matplotlib" or spec.version != "3.6"
            or getattr(spec, "arch", None) != "x86_64"
            or original_sha256 != MPL_SOLVER_RECIPE_SHA256):
        raise ValueError("unexpected Matplotlib solver preparation recipe")
    # Conda 23.11 ignores bare [execute]; equivalent canonical MatchSpec is
    # necessary for micromamba. All other YAML bytes (including pip) survive.
    spec.env_script_list[1] = spec.env_script_list[1].replace(
        "  - nbconvert[execute]!=6.0.0,!=6.0.1\n",
        "  - nbconvert[version='!=6.0.0,!=6.0.1']\n",
    )
    spec.env_script_list[2:4] = [
        # 32 MiB file limit, 120s wall bound, HTTPS-only redirects and checksum
        # before chmod/execute. No mutable latest endpoint or fallback solver.
        "(ulimit -f 32768; timeout --kill-after=5s 120s wget --https-only "
        "--max-redirect=5 --timeout=30 --tries=1 -O micromamba-preparation " + MPL_SOLVER_URL + ")",
        f"printf '%s  %s\\n' '{MPL_SOLVER_SHA256}' micromamba-preparation | sha256sum --check --strict -",
        "chmod 0500 micromamba-preparation",
        # Use Conda's exact named prefix for downstream activation/list/overlay.
        # Installation includes YAML pip requirements. No second Conda solve.
        "(ulimit -v 6291456; ulimit -t 1200; timeout --kill-after=5s 1200s "
        # Bound glibc arena reservations, not package selection or resource caps.
        # Per-package task threads can otherwise exhaust RLIMIT_AS with low RSS.
        "env MALLOC_ARENA_MAX=2 ./micromamba-preparation create --no-rc --no-env "
        "--root-prefix /opt/miniconda3 --prefix /opt/miniconda3/envs/testbed "
        "--file environment.yml -c conda-forge -c defaults "
        "--channel-priority flexible --platform linux-64 python=3.11 --yes)",
        "rm micromamba-preparation",
    ]


# These base commits declare docutils>=0.12 and latex.py explicitly supports the
# standalone roman fallback. The retained images have docutils 0.23, which no
# longer bundles docutils.utils.roman, and no standalone roman. Restore only that
# missing provider; do not downgrade docutils or re-resolve other dependencies.
# roman 3.3 (2020-07-12) retains the historical toRoman API. Local Python 3.9
# probes reproduced the import failure at all three commits; adding only this
# package let tests/test_util.py execute (8 passed each). Full readiness remains
# mandatory. Exact ORIGINAL setup hashes include both env and repository scripts.
SPHINX_ROMAN_RECIPES = {
    "sphinx-doc__sphinx-7440": (
        "3.0", "d1c3c02f7c21db76c04289f7d8e6a8ca3e8203feebcd0857b0c75a6671ca9d39",
    ),
    "sphinx-doc__sphinx-7590": (
        "3.1", "c686a763ee4b9b213e748a11cde37f9d1c3654eeaac1e8949181f23350f5388b",
    ),
    "sphinx-doc__sphinx-8056": (
        "3.2", "acaa62b82df5825ae2c807364f8b963441dc7840fc473fe8c4e5b3e0e3f47244",
    ),
}


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
        if spec.instance_id in SPHINX_ROMAN_RECIPES:
            expected_version, expected_hash = SPHINX_ROMAN_RECIPES[spec.instance_id]
            if (spec.repo != "sphinx-doc/sphinx" or spec.version != expected_version
                    or original_sha256 != expected_hash):
                raise ValueError("unexpected Sphinx preparation recipe")
            index = _original_block_index(
                spec.repo_script_list, ["python -m pip install -e .[test]"],
            )
            spec.repo_script_list.insert(index + 1, "python -m pip install --no-deps roman==3.3")
            repairs.append("sphinx-roman-3.3")
        if spec.repo == "scikit-learn/scikit-learn" and spec.version in {"1.3", "1.4", "1.5", "1.6"}:
            if spec.env_script_list != SKLEARN_ENV or PIP_PIN in spec.repo_script_list:
                raise ValueError("unexpected scikit-learn preparation environment")
            index = _original_block_index(spec.repo_script_list, [SKLEARN_INSTALL])
            spec.repo_script_list.insert(index, PIP_PIN)
            repairs.append("sklearn-legacy-pip-25.2")
        if spec.instance_id == MPL_SOLVER_TASK:
            _repair_matplotlib_solver(spec, original_sha256)
            repairs.append("matplotlib-micromamba-2.3.3")
        elif spec.repo == "matplotlib/matplotlib" and spec.version in MPL_CONDA_VERSIONS:
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
