"""Offline behavioral contracts; synthetic grades are never hosted evidence."""
import importlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

from scripts import swebench_preparation_compat as compat
from scripts.probe_pylint_evaluator import TASK

TEST_PATCH = "diff --git a/tests/a.py b/tests/a.py\n--- a/tests/a.py\n+++ b/tests/a.py\n@@ -1 +1 @@\n-old\n+new\n"


def probe():
    try:
        return importlib.import_module("scripts.probe_pylint_gold")
    except ModuleNotFoundError:
        return None


class GoldTests(unittest.TestCase):
    def harness(self):
        try:
            from swebench.harness.test_spec.test_spec import make_test_spec
        except ImportError:
            self.skipTest("optional pinned SWE-bench harness unavailable")
        return make_test_spec

    def test_setup_is_real_transformed_recipe_and_contains_no_gold(self):
        make = self.harness()
        p = probe()
        self.assertIsNotNone(p, "missing bounded gold diagnostic")
        original, effective, identity = p.setup_specs()
        expected = make(dict(TASK, test_patch="", FAIL_TO_PASS=[], PASS_TO_PASS=[]))
        self.assertEqual(original, expected)
        self.assertEqual(effective, compat.transform_test_specs([expected], swebench_version="4.1.0")[0][0])
        self.assertNotEqual(original.instance_image_key, effective.instance_image_key)
        self.assertEqual(original.eval_script, effective.eval_script)
        self.assertNotEqual(identity["original_recipe_sha256"], identity["effective_recipe_sha256"])
        # Execute the actual emitted standalone purelib writer, not a copied fix.
        additions = [s for s in effective.repo_script_list if s not in original.repo_script_list]
        with tempfile.TemporaryDirectory() as directory:
            script = "import sysconfig\nsysconfig.get_paths=lambda *args, **kwargs: {'purelib': " + repr(directory) + "}\n"
            command = next(s for s in additions if "sysconfig" in s)
            body = command.split("\n", 1)[1].rsplit("\n", 1)[0]
            result = subprocess.run([sys.executable, "-c", script + body], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            files = list(Path(directory).glob("*.pth"))
            self.assertEqual(len(files), 1)
            self.assertEqual(files[0].read_text(), "/testbed\n")

    def grade_fixture(self, root, row, run_id):
        from swebench.harness.grading import get_eval_report
        from swebench.harness.constants import START_TEST_OUTPUT, END_TEST_OUTPUT, APPLY_PATCH_PASS
        from swebench.harness.utils import get_predictions_from_file
        make = self.harness()
        dataset = root / "dataset.json"
        dataset.write_text(json.dumps([row]))
        predictions = get_predictions_from_file("gold", str(dataset), "test")
        self.assertEqual(predictions[0]["model_patch"], row["patch"])
        folder = root / "logs/run_evaluation" / run_id / "gold" / TASK["instance_id"]
        folder.mkdir(parents=True)
        (folder / "test_output.txt").write_text("Applied patch tests/a.py cleanly.\n" + START_TEST_OUTPUT + "\nPASSED tests/a.py::test_fix\nPASSED tests/a.py::test_base\n" + END_TEST_OUTPUT)
        (folder / "run_instance.log").write_text(APPLY_PATCH_PASS)
        (folder / "patch.diff").write_text(row["patch"])
        (folder / "eval.sh").write_text(make(row).eval_script)
        report = get_eval_report(make(row), predictions[0], str(folder / "test_output.txt"), True)
        (folder / "report.json").write_text(json.dumps(report))
        aggregate = {k: [TASK["instance_id"]] for k in ("completed_ids", "resolved_ids", "submitted_ids")}
        aggregate.update({k: [] for k in ("error_ids", "incomplete_ids", "empty_patch_ids", "unresolved_ids")})
        aggregate.update({k: 1 for k in ("total_instances", "submitted_instances", "completed_instances", "resolved_instances")})
        aggregate.update({k: 0 for k in ("error_instances", "empty_patch_instances", "unresolved_instances")})
        (root / ("gold." + run_id + ".json")).write_text(json.dumps(aggregate))
        return folder, aggregate

    def test_actual_gold_loader_and_grader_reject_incomplete_or_corrupt_evidence(self):
        self.harness()
        p = probe()
        self.assertTrue(callable(getattr(p, "validate_grade", None)), "missing exact gold evidence gate")
        row = dict(TASK, patch="synthetic-private-gold", test_patch=TEST_PATCH, FAIL_TO_PASS=["tests/a.py::test_fix"], PASS_TO_PASS=["tests/a.py::test_base"])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            folder, aggregate = self.grade_fixture(root, row, "fixture")
            safe = p.validate_grade(root, row, "fixture")
            self.assertEqual(safe["fail_to_pass_count"], 1)
            self.assertEqual(safe["pass_to_pass_count"], 1)
            self.assertNotIn(row["patch"], json.dumps(safe))
            target = root / "gold.fixture.json"
            for field, value in (("completed_ids", []), ("resolved_ids", [TASK["instance_id"]]*2), ("error_ids", [TASK["instance_id"]]), ("total_instances", 5)):
                altered = dict(aggregate, **{field: value})
                target.write_text(json.dumps(altered))
                with self.subTest(field=field), self.assertRaises(ValueError):
                    p.validate_grade(root, row, "fixture")
            target.write_text(json.dumps(aggregate))
            for filename in ("test_output.txt", "eval.sh", "patch.diff", "report.json", "run_instance.log"):
                path = folder / filename
                old = path.read_text()
                path.write_text("corrupted")
                with self.subTest(filename=filename), self.assertRaises(ValueError):
                    p.validate_grade(root, row, "fixture")
                path.write_text(old)
            log = folder / "test_output.txt"
            log.write_text(log.read_text().replace("PASSED tests/a.py::test_base", "ERROR tests/a.py::test_base"))
            with self.assertRaises(ValueError):
                p.validate_grade(root, row, "fixture")


    def test_production_wrapper_gold_270_cap_fresh_directory_and_alias_guard(self):
        self.harness()
        p = probe()
        self.assertTrue(callable(getattr(p, "evaluate", None)), "missing unchanged official gold invocation")
        row = dict(TASK, patch="synthetic-private-gold", test_patch=TEST_PATCH, FAIL_TO_PASS=["tests/a.py::test_fix"], PASS_TO_PASS=["tests/a.py::test_base"])
        image = "sha256:" + "a"*64
        from types import SimpleNamespace
        client = SimpleNamespace(images=SimpleNamespace(get=lambda alias: SimpleNamespace(id=image)))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def execute(command, **kwargs):
                self.assertEqual(command[1:3], ["-m", "swebench.harness.run_evaluation"])
                self.assertEqual(command[command.index("--predictions_path")+1], "gold")
                self.assertEqual(command[command.index("--timeout")+1], "270")
                self.assertEqual(command[command.index("--max_workers")+1], "1")
                self.assertEqual(kwargs["timeout"], 270)
                self.assertNotIn("AWS_SECRET_ACCESS_KEY", kwargs["env"])
                dataset = Path(command[command.index("--dataset_name")+1])
                self.assertTrue(dataset.is_absolute())
                self.assertEqual(json.loads(dataset.read_text()), [row])
                self.grade_fixture(kwargs["cwd"], row, "fixture")
                return subprocess.CompletedProcess(command, 0)
            with mock.patch.object(p.smoke.subprocess, "run", side_effect=execute), mock.patch.object(p.smoke, "cleanup_evaluator_containers") as cleanup, mock.patch.object(p, "verify_events"):
                result = p.evaluate(row, root, "fixture", client, image)
                self.assertEqual(result["pass_to_pass_count"], 1)
                cleanup.assert_called_once_with("fixture")
                with self.assertRaises(FileExistsError):
                    p.evaluate(row, root, "fixture", client, image)
            with mock.patch.object(p.smoke, "run_official_evaluation") as launch:
                with self.assertRaises(ValueError):
                    p.evaluate(row, root / "drift", "fixture", client, "sha256:" + "b"*64)
                launch.assert_not_called()

    def test_alias_event_gate_rejects_fallback_and_missing_or_duplicate_create(self):
        self.harness()
        p = probe()
        self.assertTrue(callable(getattr(p, "verify_events", None)), "missing alias consumption gate")
        event = {"Type": "container", "Action": "create", "Actor": {"Attributes": {"name": "exact", "image": "alias"}}}
        for events, valid in (([event], True), ([], False), ([event, event], False),
                               ([event, {"Type": "image", "Action": "pull"}], False)):
            with self.subTest(events=events), mock.patch.object(p, "subprocess") as process:
                process.run.return_value.stdout = "\n".join(json.dumps(e) for e in events)
                if valid:
                    p.verify_events(1, "exact", "alias")
                else:
                    with self.assertRaises(ValueError):
                        p.verify_events(1, "exact", "alias")

    def test_build_pair_uses_transformed_upstream_builder_and_production_sanitizer(self):
        self.harness()
        p = probe()
        self.assertTrue(callable(getattr(p, "build_pair", None)), "missing real setup build path")
        from swebench.harness import docker_build
        from types import SimpleNamespace
        image = "sha256:" + "a"*64
        labels = {}
        client = SimpleNamespace(images=SimpleNamespace(get=lambda key: SimpleNamespace(id=image, attrs={"Config": {"Labels": labels}})))
        def prepare(**kwargs):
            labels.update({"org.carry.swebench.evaluator-image-id": image, "org.carry.swebench.task-cache-key": kwargs["cache_key"]})
            return {"image_id": "sha256:"+"b"*64}
        def build(actual_client, specs, **kwargs):
            self.assertIs(actual_client, client)
            self.assertEqual(specs, [p.setup_specs()[1]])
            self.assertEqual(kwargs["max_workers"], 1)
            return specs, []
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(docker_build, "build_instance_images", side_effect=build), mock.patch.object(p.preparation, "install_build_limits"), mock.patch.object(p.smoke, "build_prepared_task_image", side_effect=prepare) as prepared, mock.patch.object(p, "verify_source", return_value="c"*64):
            result = p.build_pair(Path(directory), "fixture", client)
            self.assertEqual(result["evaluator_image_id"], image)
            self.assertEqual(prepared.call_args.kwargs["task_image_id"], image)
            self.assertEqual(prepared.call_args.kwargs["cache_key"], result["cache_key"])
            self.assertFalse(any("gold" in path.name or "dataset" in path.name for path in Path(directory).rglob("*")))
            prepared.side_effect = None
            prepared.return_value = {"image_id": "sha256:"+"b"*64}
            labels["org.carry.swebench.evaluator-image-id"] = "sha256:"+"c"*64
            with self.assertRaises(ValueError, msg="agent/evaluator pair drift must fail"):
                p.build_pair(Path(directory), "fixture", client)



    def test_supervisor_timeout_cleanup_and_summary_only_even_on_failure(self):
        p = probe()
        self.assertTrue(callable(getattr(p, "run_probe", None)), "missing private bounded supervisor")
        for fail in ("timeout", "install", "cleanup"):
            with self.subTest(fail=fail), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                calls = []
                def run(command, **kwargs):
                    calls.append((command, kwargs))
                    if kwargs["label"] == "worker":
                        private = Path(command[command.index("--work-dir")+1])
                        self.assertEqual(private.name, command[command.index("--run-id")+1])
                        (private / "private-gold.log").write_text("NEVER-UPLOAD-GOLD")
                        (private / "worker-summary.json").write_text(json.dumps({"identity": {"evaluator_image_id": "sha256:"+"a"*64}}))
                        if fail == "timeout":
                            transport = p.diagnostic.Transport(private / "transport-test", private, timeout=0.1, disk_reserve=0)
                            transport([sys.executable, "-c", "import time; time.sleep(10)"], label="real-timeout", seconds=1)
                        raise RuntimeError("NEVER-UPLOAD-GOLD " + fail)
                    if kwargs["label"] == "daemon":
                        return subprocess.CompletedProcess(command, 0, json.dumps({"MemoryLimit": True, "SwapLimit": True, "DockerRootDir": str(root)}), "")
                    if kwargs["label"] == "cleanup-list":
                        return subprocess.CompletedProcess(command, 0, "a"*12, "")
                    if kwargs["label"] == "remaining" and fail == "cleanup":
                        return subprocess.CompletedProcess(command, 0, "a"*12, "")
                    return subprocess.CompletedProcess(command, 0, "", "")
                with mock.patch.object(p.diagnostic, "preflight"):
                    code = p.run_probe(root / "evidence", root / "private", execute=run)
                self.assertEqual(code, 1)
                report = json.loads((root / "evidence/result.json").read_text())
                self.assertEqual(report["cleanup_verified"], fail != "cleanup")
                self.assertFalse(report["gold_validated"])
                self.assertEqual(report.get("identity", {}).get("evaluator_image_id"), "sha256:"+"a"*64)
                self.assertFalse((root / "private").exists())
                self.assertEqual([x.name for x in (root / "evidence").iterdir()], ["result.json"])
                self.assertNotIn("NEVER-UPLOAD-GOLD", json.dumps(report))
                remove = [command for command, kwargs in calls if kwargs["label"] == "remove"]
                self.assertEqual(remove, [["docker", "rm", "-f", "a"*12]])

    def test_worker_builds_before_loading_gold_and_uses_exact_dataset_pin(self):
        self.harness()
        p = probe()
        self.assertTrue(callable(getattr(p, "worker", None)), "missing gold-isolated worker")
        import docker
        import datasets
        from types import SimpleNamespace
        order = []
        image = "sha256:" + "a"*64
        row = dict(TASK, patch="PRIVATE-GOLD", test_patch="PRIVATE-TEST", FAIL_TO_PASS=[], PASS_TO_PASS=[])
        client = mock.Mock()
        def build(*args):
            order.append("build")
            return {"evaluator_image_id": image}
        def load(name, **kwargs):
            self.assertEqual(order, ["build"])
            self.assertEqual(name, "princeton-nlp/SWE-bench_Verified")
            self.assertEqual(kwargs, {"split": "test", "revision": "c104f840cc67f8b6eec6f759ebc8b2693d585d4a", "token": False})
            order.append("load")
            return [row]
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(docker, "from_env", return_value=client), mock.patch.object(p, "build_pair", side_effect=build), mock.patch.object(datasets, "load_dataset", side_effect=load), mock.patch.object(p, "evaluate", return_value={"fail_to_pass_count": 1}) as evaluate:
            p.worker(Path(directory), "fixture")
            self.assertEqual(evaluate.call_args.args[0], row)
            self.assertEqual(order, ["build", "load"])
            self.assertNotIn("PRIVATE", (Path(directory)/"worker-summary.json").read_text())
            client.close.assert_called_once()

    def test_ci_gold_branch_executes_narrow_sudo_and_safe_upload(self):
        p = probe()
        workflow = Path(__file__).resolve().parents[1] / ".github/workflows/ci.yml"
        text = workflow.read_text()
        # Execute the actual case block with recording executables; not a string-presence test.
        block = text.split('          case "$PREPARATION_PROBE_STAGE" in', 1)[1].split("          esac", 1)[0]
        script = 'case "$PREPARATION_PROBE_STAGE" in' + block + "esac"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sudo = root / "sudo"
            sudo.write_text("#!/bin/sh\nprintf '%s\n' \"$@\" > \"$CALLS\"\n")
            sudo.chmod(0o755)
            result = subprocess.run(["/bin/bash", "-eu", "-c", script], env={"PATH": str(root), "CALLS": str(root/"calls"), "PREPARATION_PROBE_STAGE": "pylint-gold", "RUNNER_TEMP": str(root)}, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, "missing executable pylint-gold CI branch: " + result.stderr)
            args = (root/"calls").read_text().splitlines()
            self.assertIn("scripts/probe_pylint_gold.py", args)
            self.assertIn("--preserve-env=GITHUB_ACTIONS,RUNNER_ENVIRONMENT,GITHUB_SHA,GITHUB_RUN_ID", args)
            self.assertEqual(args[args.index("--evidence-dir")+1], str(root/"matplotlib-solver-evidence"))
            self.assertEqual(args[args.index("--work-dir")+1], str(root/"pylint-gold-private"))



    def test_test_patch_application_and_worker_failure_identity_are_required(self):
        self.harness()
        p = probe()
        row = dict(TASK, patch="synthetic-private-gold", test_patch=TEST_PATCH, FAIL_TO_PASS=["tests/a.py::test_fix"], PASS_TO_PASS=["tests/a.py::test_base"])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            folder, _ = self.grade_fixture(root, row, "fixture")
            log = folder / "test_output.txt"
            log.write_text(log.read_text().replace("Applied patch tests/a.py cleanly.", "error: patch failed"))
            with self.assertRaises(ValueError, msg="test patch application cannot be inferred from grades"):
                p.validate_grade(root, row, "fixture")

    def test_upstream_container_consumes_verified_alias_without_build_or_pull(self):
        make = self.harness()
        from swebench.harness import docker_build
        p = probe()
        spec = make(dict(TASK, test_patch="", FAIL_TO_PASS=[], PASS_TO_PASS=[]), namespace=p.smoke.OFFICIAL_IMAGE_NAMESPACE, instance_image_tag=p.smoke.OFFICIAL_IMAGE_TAG)
        client = mock.Mock()
        result = docker_build.build_container(spec, client, "fixture", mock.Mock(), False)
        self.assertIs(result, client.containers.create.return_value)
        self.assertEqual(client.containers.create.call_args.kwargs["image"], spec.instance_image_key)
        client.images.get.assert_called_once_with(spec.instance_image_key)
        client.images.pull.assert_not_called()
        client.api.build.assert_not_called()



    def test_gold_upload_selects_only_summaries_even_if_raw_file_appears(self):
        try:
            import yaml
        except ImportError:
            self.skipTest("optional YAML parser unavailable")
        workflow = Path(__file__).resolve().parents[1] / ".github/workflows/ci.yml"
        steps = yaml.safe_load(workflow.read_text())["jobs"]["preparation-solver-probe"]["steps"]
        upload = next(step for step in steps if step.get("uses", "").startswith("actions/upload-artifact@"))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            evidence = root / "matplotlib-solver-evidence"
            evidence.mkdir()
            for name in ("result.json", "job-outcome.json", "private-gold.log"):
                (evidence/name).write_text("synthetic")
            rendered = upload["with"]["path"].replace("${{ runner.temp }}", str(root)).replace("${{ inputs.preparation_probe_stage == 'pylint-gold' && 'result.json' || '' }}", "result.json")
            selected = set()
            for line in rendered.splitlines():
                path = Path(line)
                selected.update(path.rglob("*") if path.is_dir() else [path])
            self.assertEqual({p.name for p in selected}, {"result.json", "job-outcome.json"})



    def test_safe_summary_remains_upload_readable_with_private_umask(self):
        import stat
        p = probe()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            previous = os.umask(0o077)
            try:
                with mock.patch.object(p.diagnostic, "preflight", side_effect=ValueError("preflight fixture")):
                    self.assertEqual(p.run_probe(root/"evidence", root/"private"), 1)
            finally:
                os.umask(previous)
            self.assertEqual(stat.S_IMODE((root/"evidence").stat().st_mode), 0o755)
            self.assertEqual(stat.S_IMODE((root/"evidence/result.json").stat().st_mode), 0o644)
            self.assertFalse((root/"private").exists())



def injected_worker_failure(work, run_id, stage, abrupt=False):
    """Real worker in a real child; only external build/data/eval seams are fake."""
    import contextlib
    import signal
    import docker
    import datasets
    from swebench.harness import docker_build
    from swebench.harness.test_spec import python as harness_python
    from scripts.test_swebench_preparation_compat import pylint_spec
    # Reuse the existing checksum-guarded public recipe fixture instead of
    # making each isolated child redownload GitHub requirements without a timeout.
    requirements = pylint_spec("7080").env_script_list[2].split("\n", 1)[1].removesuffix("\nEOF_59812759871")
    p = probe()
    sentinel = "NEVER-UPLOAD-GOLD"
    row = dict(TASK, patch=sentinel, test_patch=TEST_PATCH,
               FAIL_TO_PASS=["tests/a.py::test_fix"], PASS_TO_PASS=["tests/a.py::test_base"])
    client = mock.Mock()
    image = "sha256:" + "a" * 64
    client.images.get.return_value.id = image
    labels = {}
    client.images.get.return_value.attrs = {"Config": {"Labels": labels}}
    def fail(*args, **kwargs):
        print(sentinel, flush=True)
        if abrupt:
            os.kill(os.getpid(), signal.SIGKILL)
        # Even a dynamically named exception must not export its private name.
        raise type(sentinel, (ValueError,), {})(sentinel)
    def prepare(**kwargs):
        labels.update({"org.carry.swebench.evaluator-image-id": image,
                       "org.carry.swebench.task-cache-key": kwargs["cache_key"]})
        return {"image_id": image}
    def official(**kwargs):
        output = kwargs["output"]
        folder, aggregate = GoldTests().grade_fixture(output, row, run_id)
        target = output / ("gold." + run_id + ".json")
        if stage == "official_gold_unresolved":
            aggregate.update(resolved_ids=[], unresolved_ids=[TASK["instance_id"]],
                             resolved_instances=0, unresolved_instances=1)
            target.write_text(json.dumps(aggregate))
        elif stage == "grade_aggregate_invalid":
            aggregate["total_instances"] = 5
            target.write_text(json.dumps(aggregate))
        elif stage == "missing_aggregate":
            target.unlink()
        elif stage == "malformed_aggregate":
            target.write_text(sentinel)
        elif stage == "grade_report_coverage_invalid":
            (folder / "report.json").unlink()
        elif stage == "grade_evaluation_identity_invalid":
            (folder / "patch.diff").write_text("different " + sentinel)
        elif stage == "grade_report_replay_invalid":
            (folder / "report.json").write_text(json.dumps({sentinel: sentinel}))
        else:
            log = folder / "test_output.txt"
            content = log.read_text()
            if stage == "grade_test_patch_unproven":
                content = content.replace("Applied patch tests/a.py cleanly.", sentinel)
            elif stage == "grade_test_output_invalid":
                content += "Traceback (most recent call last):" + sentinel
            elif stage == "grade_test_execution_invalid":
                content = content.replace("PASSED tests/a.py::test_base", "ERROR tests/a.py::test_base")
            elif stage == "grade_target_coverage_invalid":
                content = content.replace("PASSED tests/a.py::test_base", sentinel)
            log.write_text(content)
    replacements = [
        (harness_python, "get_requirements", dict(return_value=requirements)),
        (docker, "from_env", dict(return_value=client)),
        (docker_build, "build_instance_images", dict(return_value=([], []))),
        (p.preparation, "install_build_limits", {}),
        (p.smoke, "build_prepared_task_image", dict(side_effect=prepare)),
        (p, "verify_source", dict(return_value="c" * 64)),
        (datasets, "load_dataset", dict(return_value=[row])),
        (p.smoke, "run_official_evaluation", dict(side_effect=official)),
        (p, "verify_events", {}),
    ]
    targets = {"build": (docker_build, "build_instance_images"),
               "source": (p, "verify_source"), "dataset": (datasets, "load_dataset"),
               "alias": (client.images.get.return_value, "tag"),
               "evaluate": (p.smoke, "run_official_evaluation"),
               "events": (p, "verify_events"), "grade": (p, "validate_grade")}
    with contextlib.ExitStack() as stack:
        stack.enter_context(mock.patch("socket.socket.connect", side_effect=AssertionError("unexpected network")))
        for owner, name, options in replacements:
            stack.enter_context(mock.patch.object(owner, name, **options))
        if stage in targets:
            owner, name = targets[stage]
            stack.enter_context(mock.patch.object(owner, name, side_effect=fail))
        if stage == "atomic_checkpoint":
            replace = Path.replace
            def interrupted_replace(path, target):
                if (Path(target).name == "worker-summary.json"
                        and json.loads(path.read_text()).get("worker_stage") == "dataset"):
                    assert json.loads(Path(target).read_text())["worker_stage"] == "source"
                    os.kill(os.getpid(), signal.SIGKILL)
                return replace(path, target)
            stack.enter_context(mock.patch.object(Path, "replace", interrupted_replace))
        p.worker(work, run_id)
        if stage == "after_success":
            fail()


class FailureEvidenceTests(unittest.TestCase):
    def supervise(self, root, stage, abrupt=False):
        p = probe()
        def run(command, **kwargs):
            if kwargs["label"] == "worker":
                private = Path(command[command.index("--work-dir") + 1])
                run_id = command[command.index("--run-id") + 1]
                child = ("import sys; sys.dont_write_bytecode=True; sys.path.insert(0, "
                         + repr(str(Path(__file__).resolve().parents[1])) + "); "
                         "from pathlib import Path; "
                         "from scripts.test_probe_pylint_gold import injected_worker_failure; "
                         "injected_worker_failure(Path(" + repr(str(private)) + "), "
                         + repr(run_id) + ", " + repr(stage) + ", " + repr(abrupt) + ")")
                transport = p.diagnostic.Transport(private / "private-transport", private,
                                                   timeout=30, disk_reserve=0)
                return transport([sys.executable, "-B", "-c", child], label="worker", seconds=30)
            if kwargs["label"] == "daemon":
                return subprocess.CompletedProcess(command, 0, json.dumps({
                    "MemoryLimit": True, "SwapLimit": True, "DockerRootDir": str(root)}), "")
            return subprocess.CompletedProcess(command, 0, "", "")
        previous = os.umask(0o077)
        try:
            with mock.patch.object(p.diagnostic, "preflight"):
                self.assertEqual(p.run_probe(root / "evidence", root / "private", execute=run), 1)
        finally:
            os.umask(previous)
        result = root / "evidence/result.json"
        report = json.loads(result.read_text())
        self.assertEqual(report["status"], "failed")
        self.assertFalse(report["gold_validated"])
        self.assertTrue(report["cleanup_verified"])
        self.assertFalse((root / "private").exists())
        self.assertEqual({f.name for f in result.parent.iterdir()}, {"result.json"})
        self.assertEqual(result.stat().st_mode & 0o777, 0o644)
        self.assertEqual(result.parent.stat().st_mode & 0o777, 0o755)
        self.assertNotIn("NEVER-UPLOAD-GOLD", result.read_text())
        self.assertNotIn("tests/a.py", result.read_text())
        self.assertNotIn("diff --git", result.read_text())
        return report

    def test_interrupted_atomic_checkpoint_keeps_previous_complete_json(self):
        GoldTests().harness()
        with tempfile.TemporaryDirectory() as directory:
            report = self.supervise(Path(directory), "atomic_checkpoint")
            self.assertEqual(report.get("worker_stage"), "source")
            self.assertEqual(report.get("worker_reason"), "in_progress")
            self.assertNotIn("worker_exception_type", report)
            self.assertEqual(report["worker_returncode"], -9)

    def test_failed_transport_cannot_promote_completed_worker_summary(self):
        GoldTests().harness()
        with tempfile.TemporaryDirectory() as directory:
            report = self.supervise(Path(directory), "after_success")
            self.assertEqual(report.get("worker_stage"), "complete")
            self.assertEqual(report.get("worker_reason"), "validated")
            self.assertEqual(report["worker_returncode"], 1)

    def test_summary_diagnostics_accept_only_fixed_codes(self):
        p = probe()
        for key in ("worker_stage", "worker_reason", "worker_exception_type"):
            for value in ("NEVER-UPLOAD-GOLD", ["NEVER-UPLOAD-GOLD"], {"NEVER-UPLOAD-GOLD": 1}):
                with self.subTest(key=key, value=value):
                    self.assertEqual(p.safe_diagnostics({key: value, "unexpected": "NEVER-UPLOAD-GOLD"}), {})

    def test_official_unresolved_and_validator_failures_have_distinct_safe_codes(self):
        GoldTests().harness()
        codes = ("official_gold_unresolved", "grade_aggregate_invalid",
                 "grade_report_coverage_invalid", "grade_test_patch_unproven",
                 "grade_test_output_invalid", "grade_evaluation_identity_invalid",
                 "grade_test_execution_invalid", "grade_target_coverage_invalid",
                 "grade_report_replay_invalid", "missing_aggregate", "malformed_aggregate")
        for code in codes:
            with self.subTest(code=code), tempfile.TemporaryDirectory() as directory:
                report = self.supervise(Path(directory), code)
                self.assertEqual(report.get("worker_stage"), "grade")
                self.assertEqual(report.get("worker_reason"), "grade_aggregate_invalid"
                                 if code in {"missing_aggregate", "malformed_aggregate"} else code)
                self.assertEqual(report.get("worker_exception_type"), "FileNotFoundError"
                                 if code == "missing_aggregate" else "ValueError")
                self.assertEqual(report["worker_returncode"], 1)

    def test_worker_failure_checkpoints_survive_real_transport_and_cleanup(self):
        GoldTests().harness()
        for stage in ("build", "source", "dataset", "alias", "evaluate", "events", "grade"):
            for abrupt in (False, True):
                with self.subTest(stage=stage, abrupt=abrupt), tempfile.TemporaryDirectory() as directory:
                    report = self.supervise(Path(directory), stage, abrupt)
                    self.assertEqual(report.get("worker_stage"), stage, json.dumps(report))
                    self.assertEqual(report.get("worker_reason"), "in_progress" if abrupt else "worker_exception")
                    self.assertEqual(report.get("worker_exception_type"), None if abrupt else "ValueError")
                    self.assertEqual(report["worker_returncode"], -9 if abrupt else 1)
                    self.assertIsNone(report["worker_stop_reason"])


if __name__ == "__main__":
    unittest.main()
