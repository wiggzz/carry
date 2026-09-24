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
    # The official later pip pin downgrades the solved typing_extensions and
    # breaks Jupyter's TypedDict(extra_items=...). Keep its proven minimum.
    # pandas 3.x also cannot import with the later fixed NumPy 1.25.2; the
    # compatible pandas pin retains that core NumPy pin and the YAML exclusion.
    # Both changes remain under the original full-recipe guard.
    pip_command = spec.env_script_list[-1]
    old_pin = "typing-extensions==4.7.1"
    if not pip_command.startswith("python -m pip install ") or pip_command.split().count(old_pin) != 1:
        raise ValueError("unexpected Matplotlib typing-extensions preparation pin")
    spec.env_script_list[-1] = (
        pip_command.replace(old_pin, "typing-extensions==4.13.0", 1) + " pandas==2.3.3"
    )
    spec.env_script_list.extend([
        # Remove only the orphan plugin left by the official SCM downgrade.
        # Conservatively reject even conditional/extra requirements: installed
        # metadata alone cannot prove which extras the task will exercise.
        "python - <<'PY_SCM_GUARD'\n"
        "from importlib import metadata\n"
        "from packaging.requirements import Requirement\n"
        "from packaging.utils import canonicalize_name\n"
        "if metadata.version('setuptools-scm') != '7.1.0':\n"
        "    raise RuntimeError('unexpected Matplotlib setuptools-scm version')\n"
        "for distribution in metadata.distributions():\n"
        "    for raw in distribution.requires or []:\n"
        "        if canonicalize_name(Requirement(raw).name) == 'vcs-versioning':\n"
        "            raise RuntimeError('refusing to remove required vcs-versioning: ' + distribution.metadata['Name'])\n"
        "PY_SCM_GUARD",
        "python -m pip uninstall --yes vcs-versioning",
    ])
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
# longer bundles docutils.utils.roman, and no standalone roman. Restore that
# provider without re-resolving dependencies. Only the separately proven 3.4/3.5
# task repairs below also restore their historically configured docutils version.
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
    "sphinx-doc__sphinx-8548": (
        "3.4", "25d698cb70846dc24994dfdab5e9c3fd3b276fe1181b56c6cd87b744f7b3b259",
    ),
    "sphinx-doc__sphinx-8551": (
        "3.4", "797d38594fe65f0b16d5bb3e2cbc388b08403f15dafbab7aa32c80ba179d1aa9",
    ),
    "sphinx-doc__sphinx-8721": (
        "3.5", "f58acf8b69ddcd369b6a1e8d1a2da4c406efffd0299486f0890d0a2978dc7828",
    ),
}


# Public testutils extras in both historical bases require gitpython>3, but
# upstream installs only the runtime package. Pin the missing provider and its
# small dependency closure; --no-deps leaves every existing dependency intact.
PYLINT_GIT_RECIPES = {
    "pylint-dev__pylint-7080": (
        "2.15", "44012a8818598a9a91a8f65113018fc99d3de7e01af58e76ded6ba2bd54acd0a",
    ),
    "pylint-dev__pylint-8898": (
        "3.0", "8ca8094c8af5d6506909f6ece3b64961e2cb7a12ebb860408be5819e43928e41",
    ),
}

# These exact bases import xmlschema during public test collection, but upstream
# setup installs only pytest's runtime dependencies. --maxfail=1 otherwise aborts
# collection before a single public test executes.
PYTEST_XMLSCHEMA_RECIPES = {
    "pytest-dev__pytest-7205": ("5.4", "0dbeee626c03a9017f9477ec682b4bda98210d8033f2f975abfb6fd2508d412a"),
    "pytest-dev__pytest-7236": ("5.4", "7037f63968fcde659a042f0c1cdd5dd578f35a309592812d2c3524c5d2b545b3"),
    "pytest-dev__pytest-7490": ("6.0", "407aaa00d4945f9136831de7221146d7e6dad20459429399fb4634ce9e4fc910"),
    "pytest-dev__pytest-7521": ("6.0", "049de7de0ebbdb1a8036003d7524036a17cc83114a432f7bf9bf119bc271f4b5"),
}

# The exact 0.20 task pins pandas 1.5.3 after the Conda solve, but its historical
# public tests import UndefinedVariableError from ops, which moved in 1.5.x.
XARRAY_PANDAS_RECIPE = ("0.20", "aa3c2577487958d0e85f5f5f122df7da9262ff125bc1092e37b1f70eb3c225cf")


# NumPy 1.25 introduced a warning in float(np.diff(...)) which these exact
# historical bases promote to an error during collection. Preserve all other
# pins and the upstream warning policy; this pin has public-source RED/GREEN proof.
ASTROPY_NUMPY_RECIPES = {
    "astropy__astropy-13398": (
        "5.0", "f86ff4361a98403f1fdc81c9ae146c3f1533fea011cc15195b6cf026e1e16c03",
    ),
    "astropy__astropy-14598": (
        "5.2", "0221e8455a85ac1a05d23b05e86eeeb5c57fc69835250dbfb6ce05703abf1024",
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
        if spec.instance_id in ASTROPY_NUMPY_RECIPES:
            expected_version, expected_hash = ASTROPY_NUMPY_RECIPES[spec.instance_id]
            if (spec.repo != "astropy/astropy" or spec.version != expected_version
                    or getattr(spec, "arch", None) != "x86_64"
                    or original_sha256 != expected_hash):
                raise ValueError("unexpected Astropy preparation recipe")
            spec.env_script_list[-1] = spec.env_script_list[-1].replace(
                "numpy==1.25.2", "numpy==1.24.4", 1,
            )
            repairs.append("astropy-numpy-1.24.4")
        if spec.instance_id in PYLINT_GIT_RECIPES:
            expected_version, expected_hash = PYLINT_GIT_RECIPES[spec.instance_id]
            if (spec.repo != "pylint-dev/pylint" or spec.version != expected_version
                    or original_sha256 != expected_hash):
                raise ValueError("unexpected Pylint preparation recipe")
            index = _original_block_index(spec.repo_script_list, ["python -m pip install -e ."])
            spec.repo_script_list.insert(index + 1,
                "python -m pip install --no-deps GitPython==3.1.43 gitdb==4.0.11 smmap==5.0.1")
            repairs.append("pylint-testutils-gitpython-3.1.43")
            if spec.instance_id == "pylint-dev__pylint-7080":
                # Last release before pkg_resources' import-time deprecation:
                # astroid imports it into a public CLI's tested-empty stderr.
                spec.repo_script_list.insert(index + 2,
                    "python -m pip install --no-deps setuptools==67.4.0")
                repairs.append("pylint-setuptools-67.4.0")
                # Historical Astroid discovers plugins via sys.path, not the PEP 660
                # finder. Keep that finder intact; a standalone, non-pip-owned
                # path file survives the evaluator's later editable reinstalls.
                # Both preparation and the agent workspace use /testbed.
                spec.repo_script_list.insert(index + 3,
                    "python - <<'PY_PYLINT_SOURCE_PATH'\n"
                    "from pathlib import Path\n"
                    "import sysconfig\n"
                    "(Path(sysconfig.get_path('purelib')) / 'carry_pylint_7080_source.pth')"
                    ".write_text('/testbed\\n', encoding='utf-8')\n"
                    "PY_PYLINT_SOURCE_PATH")
                repairs.append("pylint-7080-standalone-source-path")
        if spec.instance_id in PYTEST_XMLSCHEMA_RECIPES:
            expected_version, expected_hash = PYTEST_XMLSCHEMA_RECIPES[spec.instance_id]
            if (spec.repo != "pytest-dev/pytest" or spec.version != expected_version
                    or getattr(spec, "arch", None) != "x86_64"
                    or original_sha256 != expected_hash):
                raise ValueError("unexpected pytest preparation recipe")
            index = _original_block_index(spec.repo_script_list, ["python -m pip install -e ."])
            spec.repo_script_list.insert(index + 1,
                "python -m pip install --no-deps elementpath==2.5.3 xmlschema==1.11.3")
            repairs.append("pytest-xmlschema-1.11.3")
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
            if spec.instance_id in {"sphinx-doc__sphinx-8548", "sphinx-doc__sphinx-8551",
                                    "sphinx-doc__sphinx-8721"}:
                # These exact bases explicitly configure docutils 0.16 in tox.
                # 0.23 breaks public meta registration and text/docinfo output.
                spec.repo_script_list.insert(index + 2, "python -m pip install --no-deps docutils==0.16")
                repairs.append("sphinx-docutils-0.16")
        if spec.instance_id == "pydata__xarray-6461":
            expected_version, expected_hash = XARRAY_PANDAS_RECIPE
            if (spec.repo != "pydata/xarray" or spec.version != expected_version
                    or getattr(spec, "arch", None) != "x86_64"
                    or original_sha256 != expected_hash
                    or spec.env_script_list[-1].split().count("pandas==1.5.3") != 1):
                raise ValueError("unexpected xarray preparation recipe")
            spec.env_script_list[-1] = spec.env_script_list[-1].replace(
                "pandas==1.5.3", "pandas==1.4.4", 1,
            )
            repairs.append("xarray-pandas-1.4.4")
        if spec.repo == "scikit-learn/scikit-learn" and spec.version in {"1.3", "1.4", "1.5", "1.6"}:
            if spec.env_script_list != SKLEARN_ENV or PIP_PIN in spec.repo_script_list:
                raise ValueError("unexpected scikit-learn preparation environment")
            index = _original_block_index(spec.repo_script_list, [SKLEARN_INSTALL])
            spec.repo_script_list.insert(index, PIP_PIN)
            repairs.append("sklearn-legacy-pip-25.2")
        if spec.instance_id == "scikit-learn__scikit-learn-25102":
            if (spec.repo != "scikit-learn/scikit-learn" or spec.version != "1.3"
                    or getattr(spec, "arch", None) != "x86_64"
                    or original_sha256 != "47942c3d3568db54b97bd3ebb1f65115751ac18d9f6eae9506989925d592270f"):
                raise ValueError("unexpected scikit-learn legacy setuptools recipe")
            # New setuptools develop spawns nested isolated pip despite the outer
            # --no-build-isolation; its newer Cython drops the public RowMajor
            # enum export. Honor this base's pyproject setuptools<60 contract.
            index = _original_block_index(spec.repo_script_list, [SKLEARN_INSTALL])
            spec.repo_script_list.insert(index, "python -m pip install --no-deps setuptools==59.8.0")
            repairs.append("sklearn-legacy-setuptools-59.8.0")
        if spec.instance_id == MPL_SOLVER_TASK:
            _repair_matplotlib_solver(spec, original_sha256)
            repairs.extend(["matplotlib-micromamba-2.3.3", "matplotlib-typing-extensions-4.13.0",
                            "matplotlib-pandas-2.3.3", "matplotlib-remove-orphan-vcs-versioning"])
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
