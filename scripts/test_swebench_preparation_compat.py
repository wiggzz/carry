#!/usr/bin/env python3
"""Offline behavioral tests; no Docker builds or benchmark test execution."""
import copy
import os
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
import hashlib
import json
import io
import tarfile
from unittest import mock

from scripts import swebench_preparation_compat as compat
from scripts.swebench_preparation_compat import transform_test_specs

QHULL_ORIGINAL = [
    'QHULL_URL="http://www.qhull.org/download/qhull-2020-src-8.0.2.tgz"',
    'QHULL_TAR="/tmp/qhull-2020-src-8.0.2.tgz"',
    'QHULL_BUILD_DIR="/testbed/build"',
    'wget -O "$QHULL_TAR" "$QHULL_URL"',
    'mkdir -p "$QHULL_BUILD_DIR"',
    'tar -xvzf "$QHULL_TAR" -C "$QHULL_BUILD_DIR"',
]


INSTALL = "python -m pip install -v --no-use-pep517 --no-build-isolation -e ."
SKLEARN_ENV = [
    "source /opt/miniconda3/bin/activate",
    "conda create -n testbed python=3.9 'numpy==1.19.2' 'scipy==1.5.2' 'cython==3.0.10' pytest 'pandas<2.0.0' 'matplotlib<3.9.0' setuptools pytest joblib threadpoolctl -y",
    "conda activate testbed",
    "python -m pip install cython setuptools numpy scipy",
]


def spec(repo="scikit-learn/scikit-learn", version="1.3"):
    return SimpleNamespace(
        instance_id="scikit-learn__scikit-learn-25102", repo=repo, version=version,
        env_script_list=SKLEARN_ENV.copy(), repo_script_list=[INSTALL],
        eval_script_list=[INSTALL, "git apply hidden-test.patch", "pytest -rA"],
        instance_image_tag="latest",
        FAIL_TO_PASS=["hidden_failure"], PASS_TO_PASS=["hidden_pass"],
    )


class CompatibilityTests(unittest.TestCase):
    def test_legacy_pip_install_runs_after_compatible_tool_pin(self):
        original = spec()
        before = copy.deepcopy(original)
        changed, _ = transform_test_specs([original], swebench_version="4.1.0")
        # Execute the emitted repository commands against a small stateful tool
        # double: the observed pip 26 parser refuses the legacy flag until pinned.
        script = '''
set -eu
pip_version=26.0.1
python() {
    if [ "$*" = "-m pip install pip==25.2" ]; then
        pip_version=25.2
    elif [ "$*" = "-m pip install -v --no-use-pep517 --no-build-isolation -e ." ]; then
        [ "$pip_version" = 25.2 ] || return 2
        printf 'installed-with-%s\\n' "$pip_version"
    else
        return 99
    fi
}
'''
        result = subprocess.run(
            ["bash", "-c", script + "\n".join(changed[0].repo_script_list)],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "installed-with-25.2")
        self.assertEqual(original, before)
        self.assertEqual(changed[0].eval_script_list, original.eval_script_list)
        self.assertEqual(changed[0].FAIL_TO_PASS, original.FAIL_TO_PASS)
        self.assertEqual(changed[0].PASS_TO_PASS, original.PASS_TO_PASS)


    def test_matplotlib_uses_classic_for_both_conda_solves(self):
        original = spec("matplotlib/matplotlib", "3.7")
        original.repo_script_list = QHULL_ORIGINAL.copy()
        original.env_script_list = [
            "conda env create --file environment.yml",
            "conda activate testbed && conda install python=3.11 -y",
        ]
        changed, _ = transform_test_specs([original], swebench_version="4.1.0")
        script = '''
set -eu
conda() {
    if [ "$1" = activate ]; then return 0; fi
    [ "${CONDA_SOLVER:-libmamba}" = classic ] || return 134
    printf '%s\\n' "$*"
}
'''
        result = subprocess.run(
            ["bash", "-c", script + "\n".join(changed[0].env_script_list)],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), [
            "env create --file environment.yml", "install python=3.11 -y",
        ])
        self.assertEqual(changed[0].eval_script_list, original.eval_script_list)


    def test_qhull_download_requires_https_bounds_and_matching_bytes(self):
        original = spec("matplotlib/matplotlib", "3.4")
        original.repo_script_list = QHULL_ORIGINAL.copy()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            payload = root / "fixture.tgz"
            with tarfile.open(payload, "w:gz") as archive:
                member = tarfile.TarInfo("qhull-2020.2/proof.txt")
                member.size = len(b"verified")
                archive.addfile(member, io.BytesIO(b"verified"))
            digest = hashlib.sha256(payload.read_bytes()).hexdigest()
            for expected, success in ((digest, True), ("0" * 64, False)):
                with self.subTest(success=success), mock.patch.object(
                    compat, "QHULL_SHA256", expected, create=True,
                ):
                    changed, _ = transform_test_specs([original], swebench_version="4.1.0")
                # Override only filesystem locations, not generated network or
                # verification commands. wget double supplies known archive bytes.
                commands = [line for line in changed[0].repo_script_list
                            if not line.startswith(("QHULL_TAR=", "QHULL_BUILD_DIR="))]
                destination = root / ("good" if success else "bad")
                environment = dict(os.environ, QHULL_TAR=str(root / "download.tgz"),
                                   QHULL_BUILD_DIR=str(destination), FIXTURE=str(payload))
                shell = '''
set -euxo pipefail
timeout() {
    [ "$1" = --kill-after=5s ] && [ "$2" = 120s ] || return 90
    shift 2
    "$@"
}
wget() {
    [ "$1" = --https-only ] && [ "$2" = --max-redirect=0 ] || return 91
    [ "$3" = --timeout=30 ] && [ "$4" = --tries=3 ] || return 92
    [ "$5" = -O ] || return 93
    case "$7" in https://*) ;; *) return 94 ;; esac
    cp "$FIXTURE" "$6"
}
'''
                result = subprocess.run(["bash", "-c", shell + "\n".join(commands)],
                                        env=environment, capture_output=True, text=True)
                if success:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual((destination / "qhull-2020.2/proof.txt").read_bytes(), b"verified")
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(destination.exists(), "unverified archive was extracted")


    def test_qhull_download_failure_stops_before_verification_or_extraction(self):
        original = spec("matplotlib/matplotlib", "3.4")
        original.repo_script_list = QHULL_ORIGINAL.copy()
        changed, _ = transform_test_specs([original], swebench_version="4.1.0")
        for exit_code in (4, 124):
            with self.subTest(exit_code=exit_code):
                shell = f'''
set -euxo pipefail
wget() {{ return {exit_code}; }}
timeout() {{
    [ "$1" = --kill-after=5s ] && [ "$2" = 120s ] || return 91
    shift 2
    "$@"
}}
sha256sum() {{ printf 'checksum-reached\\n'; }}
mkdir() {{ printf 'mkdir-reached\\n'; }}
tar() {{ printf 'extraction-reached\\n'; }}
'''
                result = subprocess.run(
                    ["bash", "-c", shell + "\n".join(changed[0].repo_script_list)
                     + "\nprintf 'installation-reached\\n'"],
                    capture_output=True, text=True,
                )
                self.assertEqual(result.returncode, exit_code, result.stderr)
                self.assertEqual(result.stdout, "", "download failure did not stop preparation")

    def test_recipe_drift_and_wrong_harness_fail_closed_without_mutation(self):
        cases = []
        changed_install = spec()
        changed_install.repo_script_list = [INSTALL, INSTALL]
        cases.append((changed_install, "4.1.0"))
        changed_env = spec()
        changed_env.env_script_list[-1] += " wheel"
        cases.append((changed_env, "4.1.0"))
        cases.append((spec(), "4.2.0"))
        for position in range(len(QHULL_ORIGINAL)):
            changed_qhull = spec("matplotlib/matplotlib", "3.4")
            changed_qhull.repo_script_list = QHULL_ORIGINAL.copy()
            changed_qhull.repo_script_list[position] += " # upstream changed"
            cases.append((changed_qhull, "4.1.0"))
        for original, version in cases:
            before = copy.deepcopy(original)
            with self.subTest(recipe=original.repo_script_list, version=version):
                with self.assertRaisesRegex(ValueError, "unexpected|requires"):
                    transform_test_specs([original], swebench_version=version)
                self.assertEqual(original, before)


    def test_provenance_fingerprints_and_tags_bind_exact_effective_recipe(self):
        original = spec()
        unrelated = spec("django/django", "3.0")
        unrelated.instance_id = "django__django-123"
        inputs = [original, unrelated]
        before = copy.deepcopy(inputs)
        changed, report = transform_test_specs(inputs, swebench_version="4.1.0")
        again, same_report = transform_test_specs(inputs, swebench_version="4.1.0")
        self.assertEqual(inputs, before)
        self.assertEqual(changed, again)
        self.assertEqual(report, same_report)
        self.assertEqual(changed[1], unrelated)
        self.assertEqual(report.get("schema_version"), 1)
        self.assertEqual(report["compatibility_sha256"], compat.preparation_compatibility_sha256())
        self.assertEqual(report["compatibility_sha256"], hashlib.sha256(Path(compat.__file__).read_bytes()).hexdigest())
        task = report["tasks"][original.instance_id]
        self.assertEqual(task["repairs"], ["sklearn-legacy-pip-25.2"])
        for name, item in (("original", original), ("effective", changed[0])):
            payload = {"env_script_list": item.env_script_list,
                       "repo_script_list": item.repo_script_list}
            expected = hashlib.sha256(json.dumps(payload, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
            self.assertEqual(task[name + "_recipe_sha256"], expected)
        self.assertNotEqual(task["original_recipe_sha256"], task["effective_recipe_sha256"])
        self.assertNotEqual(changed[0].instance_image_tag, original.instance_image_tag)
        self.assertEqual(report["tasks"][unrelated.instance_id]["repairs"], [])
        # Eval/gold-derived state does not enter preparation identity/provenance.
        alternate = copy.deepcopy(original)
        alternate.eval_script_list = ["entirely different hidden test patch"]
        alternate.FAIL_TO_PASS = ["different gold expectation"]
        _, alternate_report = transform_test_specs([alternate], swebench_version="4.1.0")
        self.assertEqual(task, alternate_report["tasks"][original.instance_id])

    def test_duplicate_tasks_and_double_application_fail_closed(self):
        original = spec()
        with self.assertRaisesRegex(ValueError, "unique"):
            transform_test_specs([original, original], swebench_version="4.1.0")
        changed, _ = transform_test_specs([original], swebench_version="4.1.0")
        with self.assertRaisesRegex(ValueError, "unexpected"):
            transform_test_specs(changed, swebench_version="4.1.0")


    def test_pinned_harness_preserves_transformed_specs_and_evaluator(self):
        try:
            from importlib.metadata import version
            from dataclasses import asdict
            from swebench.harness.test_spec import python as recipes
            from swebench.harness.test_spec.test_spec import (
                get_test_specs_from_dataset, make_test_spec,
            )
        except ImportError:
            self.skipTest("optional pinned SWE-bench harness is not installed")
        self.assertEqual(version("swebench"), "4.1.0")
        versions = [("matplotlib/matplotlib", f"3.{minor}") for minor in range(10)]
        versions += [("scikit-learn/scikit-learn", f"1.{minor}") for minor in range(3, 7)]
        for repo, release in versions:
            with self.subTest(repo=repo, release=release), \
                 mock.patch.object(recipes, "load_cached_environment_yml", return_value=None), \
                 mock.patch.object(recipes, "get_environment_yml", return_value="name: testbed\ndependencies:\n  - pip\n"), \
                 mock.patch.object(recipes, "get_requirements", return_value="pytest\n"):
                original = make_test_spec({
                    "instance_id": repo.replace("/", "__") + "-123", "repo": repo,
                    "version": release, "base_commit": "a" * 40, "test_patch": "",
                    "FAIL_TO_PASS": ["private-test"], "PASS_TO_PASS": ["private-pass"],
                })
                changed, _ = transform_test_specs([original], swebench_version=version("swebench"))
                actual = get_test_specs_from_dataset(changed, instance_image_tag="latest")
                self.assertIs(actual[0], changed[0])
                self.assertNotEqual(actual[0].instance_image_key, original.instance_image_key)
                for key, value in asdict(original).items():
                    if key not in {"env_script_list", "repo_script_list", "instance_image_tag"}:
                        self.assertEqual(getattr(actual[0], key), value, key)
                # Invoke upstream script renderers, then validate real shell syntax.
                for script in (actual[0].setup_env_script, actual[0].install_repo_script):
                    checked = subprocess.run(["bash", "-n"], input=script, text=True, capture_output=True)
                    self.assertEqual(checked.returncode, 0, checked.stderr)


if __name__ == "__main__":
    unittest.main()
