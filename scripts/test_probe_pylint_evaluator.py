"""Synthetic offline seams; no claim of Docker/installed-image validation."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.find_spec("scripts.probe_pylint_evaluator")
probe = __import__("scripts.probe_pylint_evaluator", fromlist=["*"]) if SPEC else None


def observation(positive=False):
    return {
        "snapshot": {"base_commit": "3c5eca2ded3dd2b59ebaf23eb289453b5d2930f0",
            "clean": True, "python": [3, 9], "versions": {"pylint": "2.15.0.dev0", "astroid": "2.11.7", "setuptools": "67.4.0"},
            "origins": {"pylint": "/testbed/pylint/__init__.py", "astroid": "/opt/miniconda3/envs/testbed/lib/python3.9/site-packages/astroid/__init__.py"},
            "sys_path": ["/testbed"] if positive else ["/diagnostic", "/opt/miniconda3/envs/testbed/lib/python3.9/site-packages"],
            "editable_finder": True, "control_pth": positive, "public_test_sha256": "a"*64},
        "checkers": {"returncode": 0 if positive else 1, "checker_count": 40 if positive else 1,
                     "error": None if positive else "UnknownMessageError", "message": "" if positive else "c-extension-no-member"},
        "cli": {"returncode": 0 if positive else 1, "unknown_message": not positive},
        "tests": {"returncode": 0 if positive else 1, "collected": 2, "passed": 2 if positive else 0,
                  "failed": 0 if positive else 2, "errors": 0, "skipped": 0,
                  "unknown_message_failures": 0 if positive else 2},
    }


class ValidationTests(unittest.TestCase):
    def test_expected_negative_controls_are_not_environment_readiness(self):
        self.assertIsNotNone(probe, "Pylint evaluator diagnostic missing")
        arms = {name: observation(i >= 2) for i, name in enumerate(probe.PHASES)}
        result = probe.validate_observations(arms)
        self.assertTrue(result["diagnostic_validated"])
        self.assertFalse(result["environment_ready"])
        self.assertFalse(result["score_validated"])
        for phase, key, value in (("baseline", "base_commit", "wrong"),
                                  ("baseline", "editable_finder", False),
                                  ("reinstalled", "clean", False)):
            broken = copy.deepcopy(arms)
            broken[phase]["snapshot"][key] = value
            with self.subTest(phase=phase, key=key), self.assertRaises(ValueError):
                probe.validate_observations(broken)
        for section, key, value in (("tests", "collected", 0), ("tests", "skipped", 2),
                                    ("tests", "returncode", 5), ("checkers", "returncode", 0),
                                    ("cli", "unknown_message", False)):
            broken = copy.deepcopy(arms)
            broken["baseline"][section][key] = value
            with self.subTest(section=section, key=key), self.assertRaises(ValueError):
                probe.validate_observations(broken)
        broken = copy.deepcopy(arms)
        broken["control-reinstalled"]["tests"]["failed"] = 1
        with self.assertRaises(ValueError):
            probe.validate_observations(broken)
        broken = copy.deepcopy(arms)
        broken["control"]["snapshot"]["public_test_sha256"] = "b"*64
        with self.assertRaises(ValueError):
            probe.validate_observations(broken)


class FakeDocker:
    """Docker transport only; orchestration/validation/evidence are real."""
    def __init__(self, fault=None):
        self.calls = []
        self.phase = 0
        self.fault = fault
        self.created = False
        self.network = "none"

    def __call__(self, command, *, label, allowed=(0,), **kwargs):
        self.calls.append((command, label))
        output, rc = "", 0
        if label == "metadata":
            output = json.dumps(probe.TASK)
        elif label == "daemon":
            output = json.dumps({"MemoryLimit": True, "SwapLimit": True})
        elif label.endswith("-identity"):
            ref = command[-1]
            output = json.dumps({"Id": "sha256:" + "c"*64, "RepoDigests": [ref],
                                 "Architecture": "amd64", "Os": "linux"})
            if self.fault == "image":
                output = output.replace(ref, "wrong")
        elif label == "control-path-control":
            output = 'PROBE_JSON={"created": true}'
        elif label == "create":
            self.created = True
            output = "d"*64
        elif label == "network":
            output = json.dumps({self.network: {}})
        elif command[:3] == ["docker", "network", "connect"]:
            self.network = command[3]
        elif label.endswith("-snapshot"):
            self.phase = probe.PHASES.index(label.removesuffix("-snapshot"))
            arm = observation(self.phase >= 2)
            if self.fault == "base":
                arm["snapshot"]["base_commit"] = "wrong"
            output = "PROBE_JSON=" + json.dumps(arm["snapshot"])
        elif label.endswith(("-checkers", "-tests")):
            key = label.rsplit("-", 1)[1]
            value = observation(self.phase >= 2)[key]
            if self.fault == "tests" and self.phase == 3:
                value["skipped"] = 2
            rc = value["returncode"]
            output = "PROBE_JSON=" + json.dumps(value)
        elif label.endswith("-cli"):
            rc = 0 if self.phase >= 2 else 1
            output = "" if rc == 0 else "UnknownMessageError: c-extension-no-member"
        elif label == "remaining":
            output = "d"*64 if self.fault == "cleanup" else ""
        elif label == self.fault:
            raise RuntimeError("synthetic command failure")
        if rc not in allowed:
            raise RuntimeError("unexpected return code")
        return subprocess.CompletedProcess(command, rc, output, "")


class OrchestrationTests(unittest.TestCase):

    def test_final_source_check_fails_closed_with_cleanup(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            docker = FakeDocker()
            counts = {}
            def dirty(command, **kwargs):
                result = docker(command, **kwargs)
                label = kwargs["label"]
                counts[label] = counts.get(label, 0) + 1
                if label == "control-reinstalled-snapshot" and counts[label] == 2:
                    value = observation(True)["snapshot"]
                    value["clean"] = False
                    result.stdout = "PROBE_JSON=" + json.dumps(value)
                return result
            self.assertEqual(self.run_fake(root, dirty), 1, "missing post-test source integrity gate")
            report = json.loads((root / "evidence/result.json").read_text())
            self.assertFalse(report["diagnostic_validated"])
            self.assertTrue(report["cleanup_verified"])

    def test_disk_watch_accounts_for_docker_daemon_filesystem(self):
        self.assertTrue(hasattr(probe.Transport, "watch_disk"), "daemon disk watchdog missing")
        from types import SimpleNamespace as S
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            runner = probe.Transport(root / "evidence", root / "work", timeout=5, disk_reserve=0)
            with mock.patch("shutil.disk_usage", return_value=S(free=30*1024**3)):
                runner.watch_disk(root / "daemon", reserve=8*1024**3)
            def disk(path):
                return S(free=4*1024**3 if Path(path) == root / "daemon" else 30*1024**3)
            with mock.patch("shutil.disk_usage", side_effect=disk), self.assertRaisesRegex(RuntimeError, "disk"):
                runner([sys.executable, "-c", "import time; time.sleep(10)"], label="disk")


    def run_fake(self, root, docker):
        with mock.patch.object(probe, "preflight", return_value=None):
            return probe.run_probe(root / "evidence", root / "work", execute=docker)

    def test_exact_images_install_and_offline_controls_cleanup(self):
        self.assertTrue(hasattr(probe, "run_probe"), "hosted orchestration missing")
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            docker = FakeDocker()
            self.assertEqual(self.run_fake(root, docker), 0)
            result = json.loads((root / "evidence/result.json").read_text())
            self.assertTrue(result["cleanup_verified"])
            self.assertTrue(result["diagnostic_validated"])
            self.assertFalse(result["score_validated"])
            pulls = [c for c, label in docker.calls if label.endswith("-pull")]
            self.assertEqual({c[-1] for c in pulls}, set(probe.IMAGES.values()))
            create = next(c for c, label in docker.calls if label == "create")
            self.assertEqual(create[create.index("--network")+1], "none")
            self.assertIn("--cap-drop=ALL", create)
            self.assertNotIn("--privileged", create)
            self.assertNotIn("/var/run/docker.sock", " ".join(create))
            self.assertEqual(create[-2], probe.IMAGES["evaluator"])
            installs = [c for c, label in docker.calls if label.endswith("-install")]
            self.assertEqual(len(installs), 2)
            for c in installs:
                self.assertEqual(c[-1], "source /opt/miniconda3/bin/activate; conda activate testbed; cd /testbed; " + probe.INSTALL)
            names = [c[-1] for c, label in docker.calls if label == "remove"]
            self.assertEqual(len(names), 1)
            self.assertTrue(names[0].startswith("carry-pylint-probe-"))
            self.assertTrue((root / "evidence/artifact-index.json").is_file())
            # No source tree, data, test patch or entire image config is exported.
            self.assertNotIn("Config", (root / "evidence/image-identities.json").read_text())

    def test_failed_install_image_base_tests_or_cleanup_never_validates(self):
        self.assertTrue(hasattr(probe, "run_probe"), "hosted orchestration missing")
        for fault in ("image", "base", "reinstalled-install", "tests", "cleanup"):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as d:
                root, docker = Path(d), FakeDocker(fault)
                self.assertEqual(self.run_fake(root, docker), 1)
                result = json.loads((root / "evidence/result.json").read_text())
                self.assertFalse(result["diagnostic_validated"])
                self.assertFalse(result["environment_ready"])
                if docker.created:
                    self.assertTrue(any(label == "remove" for _, label in docker.calls))
                if fault in ("image", "base"):
                    self.assertFalse(any(label.endswith("-install") for _, label in docker.calls))

    def test_bounded_transport_scrubs_secrets_and_fails_on_timeout_or_log_limit(self):
        self.assertTrue(hasattr(probe, "Transport"), "bounded transport missing")
        with tempfile.TemporaryDirectory() as d, mock.patch.dict(os.environ, OPENAI_API_KEY="never-export-this"):
            root = Path(d)
            runner = probe.Transport(root / "evidence", root / "work", timeout=5, disk_reserve=0)
            child = runner([sys.executable, "-c", "import os; print(os.getenv('OPENAI_API_KEY', 'absent'))"], label="clean")
            self.assertEqual(child.stdout.strip(), "absent")
            with self.assertRaisesRegex(RuntimeError, "timeout"):
                runner([sys.executable, "-c", "import time; time.sleep(5)"], label="timeout", seconds=0.1)
            with mock.patch.object(probe, "LOG_LIMIT", 100), self.assertRaisesRegex(RuntimeError, "log"):
                runner([sys.executable, "-c", "print('x'*1000)"], label="overflow")


class InsideAndWorkflowTests(unittest.TestCase):

    def test_snapshot_records_only_public_layout_and_rejects_dirty_checkout(self):
        self.assertTrue(hasattr(probe, "snapshot"), "actual installed-layout inspection missing")
        from types import SimpleNamespace as S
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            repo, site = root / "repo", root / "site"
            (repo / "tests").mkdir(parents=True)
            (repo / "tests/test_self.py").write_text("# unchanged public fixture\n")
            (repo / "setup.py").write_text("# public setup fixture\n")
            site.mkdir()
            (site / "__editable___pylint_2_15_finder.py").write_text("MAPPING = {'pylint': '/testbed/pylint'}\n")
            (site / "__editable__.pylint.pth").write_text("import __editable___pylint_2_15_finder\n")
            (site / "private.txt").write_text("never-export")
            for args in (("init", "-q"), ("add", "."), ("-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture")):
                subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)
            with mock.patch.dict(sys.modules, pylint=S(__file__="/testbed/pylint/__init__.py"), astroid=S(__file__="/opt/astroid/__init__.py")), \
                 mock.patch("importlib.metadata.version", return_value="fixture"):
                snap = probe.snapshot(repo=repo, site=site)
                self.assertTrue(snap["clean"])
                self.assertTrue(snap["editable_finder"])
                self.assertIn("MAPPING", json.dumps(snap))
                self.assertNotIn("never-export", json.dumps(snap))
                (repo / "tests/test_self.py").write_text("# changed\n")
                self.assertFalse(probe.snapshot(repo=repo, site=site)["clean"])



    def test_container_only_mutation_cannot_be_invoked_on_host(self):
        with mock.patch.object(sys, "argv", ["probe", "--inside", "path-control"]), \
             mock.patch.object(probe, "install_control", return_value={"created": True}) as install:
            with self.assertRaises(SystemExit) as e:
                probe.main()
            self.assertEqual(e.exception.code, 2)
            install.assert_not_called()

    def test_public_metadata_projects_only_setup_keys_and_rejects_drift(self):
        from types import SimpleNamespace as S
        record = dict(probe.TASK, test_patch="never-export", patch="never-export")
        load = mock.Mock(return_value=[record])
        with mock.patch.dict(sys.modules, datasets=S(load_dataset=load)), \
             mock.patch("importlib.metadata.version", return_value="4.1.0"):
            self.assertEqual(probe.public_metadata(), probe.TASK)
            self.assertEqual(load.call_args.kwargs["revision"], "c104f840cc67f8b6eec6f759ebc8b2693d585d4a")
            self.assertIs(load.call_args.kwargs["token"], False)
            record["base_commit"] = "wrong"
            with self.assertRaises(ValueError):
                probe.public_metadata()

    def test_checkers_executes_real_plugin_registration_and_preserves_error(self):
        self.assertTrue(hasattr(probe, "checker_probe"), "container checker path missing")
        class Linter:
            def load_default_plugins(self):
                self.loaded = True
            def enable(self, message):
                assert self.loaded
                assert message == "c-extension-no-member"
            def get_checkers(self):
                return list(range(40))
        self.assertEqual(probe.checker_probe(Linter)["checker_count"], 40)
        class UnknownMessageError(Exception):
            pass
        class Broken(Linter):
            def enable(self, message):
                raise UnknownMessageError(message)
        result = probe.checker_probe(Broken)
        self.assertEqual(result["returncode"], 1)
        self.assertEqual(result["error"], "UnknownMessageError")

    def test_public_test_recorder_counts_calls_not_collection_or_skips(self):
        self.assertTrue(hasattr(probe, "TestRecorder"), "execution-bearing recorder missing")
        from types import SimpleNamespace as S
        plugin = probe.TestRecorder()
        plugin.pytest_collection_finish(S(items=[1, 2]))
        for when, outcome, longrepr in (("setup", "passed", ""), ("call", "failed", "UnknownMessageError c-extension-no-member"),
                                       ("teardown", "passed", ""), ("setup", "skipped", "skip")):
            plugin.pytest_runtest_logreport(S(when=when, outcome=outcome, longrepr=longrepr))
        self.assertEqual(plugin.result, {"collected": 2, "passed": 0, "failed": 1, "errors": 0,
                                        "skipped": 1, "unknown_message_failures": 1})

    def test_control_is_standalone_pth_not_pythonpath_or_source_mutation(self):
        self.assertTrue(hasattr(probe, "install_control"), "path control missing")
        with tempfile.TemporaryDirectory() as d:
            site = Path(d)
            result = probe.install_control(site)
            self.assertTrue(result["created"])
            self.assertEqual((site / "carry_diagnostic_testbed.pth").read_text(), "/testbed\n")
            with self.assertRaises(FileExistsError):
                probe.install_control(site)

    def test_cli_refuses_nonhosted_or_nonroot_without_docker_or_writes(self):
        self.assertTrue(hasattr(probe, "main"), "CLI gate missing")
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            for hosted, uid in ((False, 0), (True, 1001)):
                with mock.patch.dict(os.environ, GITHUB_ACTIONS=str(hosted).lower(), RUNNER_ENVIRONMENT="github-hosted"), \
                     mock.patch.object(probe.os, "geteuid", return_value=uid), \
                     mock.patch.object(sys, "argv", ["probe", "--evidence-dir", str(root/"evidence"), "--work-dir", str(root/"work")]), \
                     mock.patch.object(probe, "run_probe") as run:
                    with self.assertRaises(SystemExit) as e:
                        probe.main()
                    self.assertEqual(e.exception.code, 2)
                    run.assert_not_called()
            self.assertFalse((root / "evidence").exists())

    def test_workflow_runs_new_stage_through_narrow_sudo_and_retains_failure(self):
        try:
            import yaml
        except ImportError:
            self.skipTest("PyYAML unavailable; pinned-harness CI executes this boundary")
        wf = yaml.load((Path(__file__).parent.parent / ".github/workflows/ci.yml").read_text(), Loader=yaml.BaseLoader)
        inputs = wf["on"]["workflow_dispatch"]["inputs"]
        self.assertIn("pylint-evaluator", inputs["preparation_probe_stage"]["options"], "third stage missing")
        self.assertEqual(inputs["preparation_solver_probe"]["default"], "false")
        self.assertEqual(wf["permissions"], {"contents": "read"})
        job = wf["jobs"]["preparation-solver-probe"]
        self.assertNotIn("environment", job)
        step = next(s for s in job["steps"] if s.get("id") == "solver")
        outcome = next(s for s in job["steps"] if s.get("name", "").startswith("Record job outcome"))
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "swebench-image-probe/bin").mkdir(parents=True)
            python = root / "swebench-image-probe/bin/python"
            python.write_text('#!/bin/bash\n[[ "$SUDO_FIXTURE" == yes ]] || exit 99\nprintf "%s\\n" "$*" > "$RUNNER_TEMP/calls"\nexit 47\n')
            python.chmod(0o700)
            sudo = root / "sudo"
            sudo.write_text('#!/bin/bash\nset -eu\n[[ "$1" == --non-interactive ]]\nshift\n'
                '[[ "$1" == --preserve-env=* ]]\nshift\nexport SUDO_FIXTURE=yes\nexec "$@"\n')
            sudo.chmod(0o700)
            env = dict(os.environ, PATH=str(root)+":"+os.environ["PATH"], RUNNER_TEMP=str(root),
                       PREPARATION_PROBE_STAGE="pylint-evaluator", SOLVER_STEP_OUTCOME="failure", GITHUB_SHA="f"*40)
            result = subprocess.run(["bash", "-eu", "-c", step["run"]], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 47, result.stderr)
            self.assertEqual((root / "calls").read_text().split()[0], "scripts/probe_pylint_evaluator.py")
            result = subprocess.run(["bash", "-eu", "-c", outcome["run"]], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            saved = json.loads((root / "matplotlib-solver-evidence/job-outcome.json").read_text())
            self.assertEqual(saved["preparation_probe_stage"], "pylint-evaluator")
            self.assertEqual(saved["solver_step_outcome"], "failure")
            self.assertFalse(saved["result_present"])


if __name__ == "__main__":
    unittest.main()
