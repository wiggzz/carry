"""Offline behavioral tests; fake solver output is NOT solve evidence."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

MODULE = Path(__file__).with_name("probe_matplotlib_solver.py")


class ProbeTests(unittest.TestCase):
    def load_probe(self):
        self.assertTrue(MODULE.exists(), "bounded solver diagnostic is not implemented")
        spec = importlib.util.spec_from_file_location("probe", MODULE)
        assert spec is not None and spec.loader is not None
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def test_download_hash_is_verified_before_executable_is_published(self):
        probe = self.load_probe()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "source"
            source.write_bytes(b"not a solver")
            target = root / "micromamba"
            with self.assertRaisesRegex(ValueError, "SHA-256"):
                probe.download_verified(source.as_uri(), "0" * 64, target)
            self.assertFalse(target.exists())
            expected = hashlib.sha256(source.read_bytes()).hexdigest()
            probe.download_verified(source.as_uri(), expected, target)
            self.assertEqual(target.read_bytes(), source.read_bytes())


    def test_success_records_exact_input_and_validates_all_conda_dependencies(self):
        probe = self.load_probe()
        self.assertTrue(hasattr(probe, "run_probe"), "diagnostic runner is missing")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            # Explicit fake transaction: high versions satisfy this fixture's floors.
            # Its only purpose is exercising validation; it is NOT a real solve.
            names = []
            environment = MODULE.parent / "fixtures/matplotlib-24627-environment.yml"
            import re
            for line in environment.read_text().splitlines():
                if line.startswith("  - ") and line != "  - pip:" and line != "  - conda-forge":
                    names.append(re.match(r"  - ([\w-]+)", line).group(1))
            plan = {"success": True, "dry_run": True, "actions": {"LINK": [
                {"name": name, "version": "99.0.0"} for name in names
            ] + [{"name": "python", "version": "3.11.9"}]}}
            solver = root / "fake-solver"
            solver.write_text("#!/usr/bin/env python3\nimport json,sys\n"
                              "from pathlib import Path\n"
                              "Path(__file__).with_suffix('.argv').write_text(json.dumps(sys.argv))\n"
                              "data=Path(sys.argv[sys.argv.index('--file')+1]).read_bytes()\n"
                              "assert b\"  - nbconvert[version='!=6.0.0,!=6.0.1']\\n\" in data\n"
                              "assert b'nbconvert[execute]' not in data\n"
                              "print(" + repr(json.dumps(plan)) + ")\n")
            status = probe.run_probe(root / "evidence", root / "work",
                                     solver_url=solver.as_uri(),
                                     solver_sha256=hashlib.sha256(solver.read_bytes()).hexdigest(),
                                     timeout_seconds=2, memory_bytes=256 * 1024**2)
            self.assertEqual(status, 0)
            report = json.loads((root / "evidence/result.json").read_text())
            self.assertEqual(report["status"], "conda-dry-run-validated")
            self.assertFalse(report["environment_ready"])
            self.assertEqual((root / "evidence/environment.yml").read_bytes(), environment.read_bytes())
            effective = (root / "evidence/effective-environment.yml").read_bytes()
            self.assertEqual(effective, environment.read_bytes().replace(
                b"  - nbconvert[execute]!=6.0.0,!=6.0.1\n",
                b"  - nbconvert[version='!=6.0.0,!=6.0.1']\n"))
            self.assertEqual(report["effective_environment_sha256"], hashlib.sha256(effective).hexdigest())
            self.assertEqual(report["effective_environment_size_bytes"], len(effective))
            self.assertEqual(report["environment_sha256"], hashlib.sha256(environment.read_bytes()).hexdigest())
            self.assertEqual(set(report["validation"]["conda_dependencies"]), set(names))
            self.assertEqual(report["validation"]["pip_not_validated"],
                             ["mpl-sphinx-theme", "sphinxcontrib-svg2pdfconverter", "pikepdf"])
            argv = json.loads((root / "work/micromamba.argv").read_text())
            self.assertIn("python=3.11", argv)
            self.assertIn("--dry-run", argv)
            self.assertEqual([argv[i + 1] for i, x in enumerate(argv) if x == "-c"],
                             ["conda-forge", "defaults"])
            self.assertIn("--no-rc", argv)
            self.assertIn("--no-env", argv)
            self.assertGreater(report["process"]["max_rss_kib"], 0)


    def run_fake(self, body, *, timeout=2, digest=None, missing=False):
        probe = self.load_probe()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "fake-solver"
            source.write_text("#!/usr/bin/env python3\n" + body)
            expected = digest or hashlib.sha256(source.read_bytes()).hexdigest()
            url = (root / "missing" if missing else source).as_uri()
            code = probe.run_probe(root / "evidence", root / "work", solver_url=url,
                                   solver_sha256=expected, timeout_seconds=timeout,
                                   memory_bytes=128 * 1024**2)
            files = {p.name: p.read_bytes() for p in (root / "evidence").iterdir()}
            report = json.loads(files["result.json"])
            return code, report, files

    def test_failed_download_retains_failure_evidence_without_executing(self):
        code, report, files = self.run_fake("raise SystemExit('must not run')", missing=True)
        self.assertEqual(code, 1)
        self.assertEqual(report["status"], "failed")
        self.assertNotIn("process", report)
        self.assertEqual(files["stdout.log"], b"")
        self.assertIn("environment.yml", files)

    def test_bad_download_hash_never_runs_solver_or_reports_success(self):
        code, report, _ = self.run_fake("print('must not run')", digest="0" * 64)
        self.assertEqual(code, 1)
        self.assertIn("SHA-256", report["error"])
        self.assertNotIn("solver_download_verified", report)
        self.assertNotIn("process", report)
        self.assertIn("command", report, "failure evidence omitted planned command")
        self.assertIn("python=3.11", report["command"])
        self.assertEqual(report["channels"], ["conda-forge", "defaults"])

    def test_timeout_kills_child_group_and_preserves_output(self):
        code, report, files = self.run_fake(
            "import subprocess,sys,time,os\n"
            "p=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)'])\n"
            "print(p.pid,flush=True)\n"
            "print('pending',file=sys.stderr,flush=True)\n"
            "time.sleep(30)\n", timeout=0.3)
        self.assertEqual(code, 1)
        self.assertTrue(report["process"]["timed_out"])
        self.assertLess(report["elapsed_seconds"], 3)
        self.assertEqual(files["stderr.log"], b"pending\n")
        child = int(files["stdout.log"])
        # Orphaned children can remain zombies briefly; neither state is running.
        stat = Path(f"/proc/{child}/stat")
        if stat.exists():
            self.assertEqual(stat.read_text().split()[2], "Z")

    def test_zero_exit_with_missing_plan_is_failure(self):
        code, report, _ = self.run_fake("print('{}')")
        self.assertEqual(code, 1)
        self.assertEqual(report["process"]["returncode"], 0)
        self.assertEqual(report["status"], "failed")

    def test_failed_exit_cannot_be_overridden_by_success_json(self):
        code, report, _ = self.run_fake("print('{\"success\":true,\"dry_run\":true}')\nraise SystemExit(7)")
        self.assertEqual(code, 1)
        self.assertEqual(report["process"]["returncode"], 7)
        self.assertNotIn("validation", report)

    def test_memory_limit_is_applied_to_executed_solver(self):
        code, report, files = self.run_fake("x=bytearray(200*1024**2)")
        self.assertEqual(code, 1)
        self.assertIn(b"MemoryError", files["stderr.log"])
        self.assertEqual(report["memory_limit_bytes"], 128 * 1024**2)

    def test_logs_are_bounded_and_overflow_never_succeeds(self):
        code, report, files = self.run_fake("import sys\nsys.stdout.write('x' * (5*1024**2))")
        self.assertEqual(code, 1)
        self.assertTrue(report["process"]["logs_truncated"])
        self.assertEqual(len(files["stdout.log"]), 4 * 1024**2)

    def test_solver_receives_no_inherited_credentials(self):
        from unittest.mock import patch
        import os
        with patch.dict(os.environ, {"AWS_SECRET_ACCESS_KEY": "TEST-SENTINEL",
                                     "OPENAI_API_KEY": "TEST-SENTINEL",
                                     "GITHUB_TOKEN": "TEST-SENTINEL"}):
            _, _, files = self.run_fake("import os,json\nprint(json.dumps(dict(os.environ)))")
        self.assertNotIn(b"TEST-SENTINEL", files["stdout.log"])

    def test_modified_environment_is_rejected(self):
        probe = self.load_probe()
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            probe.parse_requirements(probe.ENVIRONMENT.read_bytes() + b"# changed\n")

    def test_plan_checks_python_and_every_declared_conda_spec(self):
        probe = self.load_probe()
        requirements, pip = probe.parse_requirements(probe.ENVIRONMENT.read_bytes())
        import re
        records = [{"name": re.match(r"[\w-]+", spec).group(), "version": "99.0.0"}
                   for spec in requirements] + [{"name": "python", "version": "3.11.9"}]
        for missing in records:
            with self.subTest(missing=missing["name"]), self.assertRaises(ValueError):
                probe.validate_plan({"success": True, "dry_run": True,
                                     "actions": {"LINK": [r for r in records if r != missing]}}, requirements, pip)
        for name, version in [("python", "3.12.1"), ("numpy", "1.20.0"),
                              ("sphinx", "2.0.0"), ("pytest", "5.4.0"),
                              ("nbconvert", "6.0.1"), ("pandas", "0.25.0")]:
            with self.subTest(name=name), self.assertRaises(ValueError):
                altered = [dict(r, version=version) if r["name"] == name else r for r in records]
                probe.validate_plan({"success": True, "dry_run": True,
                                     "actions": {"LINK": altered}}, requirements, pip)

    def test_cli_refuses_a_real_solve_off_hosted_ci(self):
        import os
        import subprocess
        import sys
        environment = {k: v for k, v in os.environ.items() if k not in ("GITHUB_ACTIONS", "RUNNER_ENVIRONMENT")}
        run = subprocess.run([sys.executable, str(MODULE), "--evidence-dir", "unused",
                              "--work-dir", "unused"], env=environment, capture_output=True, text=True)
        self.assertEqual(run.returncode, 2)
        self.assertIn("restricted", run.stderr)


    def test_dribbling_download_obeys_wall_deadline(self):
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
        from threading import Thread, Event
        from unittest.mock import patch
        import time
        release = Event()

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                self.send_response(200)
                self.send_header("Content-Length", "2")
                self.end_headers()
                self.wfile.write(b"a")
                self.wfile.flush()
                release.wait(1.5)
                self.wfile.write(b"b")

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = Thread(target=server.serve_forever, daemon=True)
        thread.start()
        probe = self.load_probe()
        started = time.monotonic()
        try:
            with tempfile.TemporaryDirectory() as tmp, patch.object(probe.time, "monotonic", side_effect=[0, 121]):
                with self.assertRaisesRegex(ValueError, "budget"):
                    probe.download_verified(f"http://127.0.0.1:{server.server_port}/", "0" * 64, Path(tmp) / "solver")
            self.assertLess(time.monotonic() - started, 1.0,
                            "buffered reads must not hide a slow download from the deadline")
        finally:
            release.set()
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == "__main__":
    unittest.main()
