#!/usr/bin/env python3
"""Behavior test for the EC2 worker bootstrap path without AWS or Docker."""
import base64
import hashlib
import json
import shutil
import sys
import time
import os
import pathlib
import subprocess
import tarfile
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("swebench_ec2_worker.sh")


class Ec2WorkerBootstrapTests(unittest.TestCase):
    def test_bootstrap_downloads_verifies_extracts_and_exits_without_secret(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            payload = root / "payload"
            payload.mkdir()
            (payload / "marker.txt").write_text("immutable source\n")
            archive = root / "source.tar.gz"
            with tarfile.open(archive, "w:gz") as stream:
                stream.add(payload / "marker.txt", arcname="marker.txt")

            fake_bin = root / "bin"
            fake_bin.mkdir()
            for command in ("dnf", "systemctl"):
                path = fake_bin / command
                body = "#!/bin/sh\n"
                if command == "dnf":
                    body += 'printf "%s\\n" "$*" > "$FAKE_DNF_LOG"\n'
                body += "exit 0\n"
                path.write_text(body)
                path.chmod(0o755)
            curl = fake_bin / "curl"
            curl.write_text(
                "#!/bin/sh\n"
                "while [ $# -gt 0 ]; do\n"
                "  if [ \"$1\" = -o ]; then cp \"$FAKE_SOURCE_ARCHIVE\" \"$2\"; exit 0; fi\n"
                "  shift\n"
                "done\n"
                "exit 2\n"
            )
            curl.chmod(0o755)

            carry_root = root / "worker"
            env = dict(
                os.environ,
                PATH=f"{fake_bin}:{os.environ['PATH']}",
                SOURCE_URL_B64=base64.b64encode(b"https://example.invalid/source").decode(),
                KEY_URL_B64="",
                RESULT_URL_B64="",
                SOURCE_SHA256=hashlib.sha256(archive.read_bytes()).hexdigest(),
                SOURCE_COMMIT="a" * 40,
                BENCHMARK_MODE="bootstrap",
                BOOTSTRAP_WAIT_SECONDS="1",
                RUN_ID="gh-test-1",
                CARRY_ROOT=str(carry_root),
                SECRET_FILE=str(root / "secret"),
                PYTHON_BIN="python3",
                SKIP_SHUTDOWN="1",
                FAKE_SOURCE_ARCHIVE=str(archive),
                FAKE_DNF_LOG=str(root / "dnf.log"),
            )
            run = subprocess.run(["bash", str(SCRIPT)], env=env, text=True, capture_output=True)
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertEqual((carry_root / "source" / "marker.txt").read_text(), "immutable source\n")
            self.assertFalse((root / "secret").exists())
            packages = (root / "dnf.log").read_text(encoding="utf-8").split()
            self.assertIn("python3.11", packages)
            self.assertNotIn("curl", packages)

    def test_bootstrap_loads_run_configuration_from_one_capability(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            payload = root / "payload"; payload.mkdir()
            (payload / "marker.txt").write_text("immutable source\n")
            archive = root / "source.tar.gz"
            with tarfile.open(archive, "w:gz") as stream:
                stream.add(payload / "marker.txt", arcname="marker.txt")

            config = root / "worker-config.env"
            source_url = base64.b64encode(b"https://example.invalid/source").decode()
            config.write_text(
                f"SOURCE_URL_B64={source_url}\n"
                "KEY_URL_B64=\nRESULT_URL_B64=\nDOCKER_AUTH_URL_B64=\nREGISTRY_AUTH_URL_B64=\nCONTROL_URL_B64=\n"
                f"SOURCE_SHA256={hashlib.sha256(archive.read_bytes()).hexdigest()}\n"
                "SOURCE_COMMIT=" + "a" * 40 + "\n"
                "BENCHMARK_MODE=bootstrap\nBOOTSTRAP_WAIT_SECONDS=1\nRUN_ID=gh-test-config\n",
                encoding="utf-8",
            )
            fake_bin = root / "bin"; fake_bin.mkdir()
            for command in ("dnf", "systemctl"):
                path = fake_bin / command
                path.write_text("#!/bin/sh\nexit 0\n")
                path.chmod(0o755)
            curl = fake_bin / "curl"
            curl.write_text(
                "#!/bin/sh\noutput=\nwhile [ $# -gt 0 ]; do\n"
                "  if [ \"$1\" = -o ]; then output=$2; shift 2; continue; fi\n  shift\ndone\n"
                "case \"$output\" in *carry-bootstrap-config) cp \"$FAKE_CONFIG\" \"$output\" ;; *) cp \"$FAKE_SOURCE_ARCHIVE\" \"$output\" ;; esac\n"
            )
            curl.chmod(0o755)
            carry_root = root / "worker"
            env = dict(
                os.environ, PATH=f"{fake_bin}:{os.environ['PATH']}",
                BOOTSTRAP_CONFIG_URL_B64=base64.b64encode(b"https://example.invalid/config").decode(),
                CARRY_ROOT=str(carry_root), SECRET_FILE=str(root / "secret"), PYTHON_BIN="python3",
                SKIP_SHUTDOWN="1", FAKE_CONFIG=str(config), FAKE_SOURCE_ARCHIVE=str(archive),
            )
            run = subprocess.run(["bash", str(SCRIPT)], env=env, text=True, capture_output=True)
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertEqual((carry_root / "source" / "marker.txt").read_text(), "immutable source\n")
            self.assertFalse((carry_root / "carry-bootstrap-config").exists())

    def test_session_worker_rejects_non_retained_harness_before_fetching_credentials(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            payload = root / "payload"; payload.mkdir()
            archive = root / "source.tar.gz"
            with tarfile.open(archive, "w:gz") as stream:
                stream.add(payload, arcname="payload")
            fake_bin = root / "bin"; fake_bin.mkdir()
            for command in ("dnf", "systemctl"):
                path = fake_bin / command
                path.write_text("#!/bin/sh\nexit 0\n")
                path.chmod(0o755)
            curl = fake_bin / "curl"
            curl.write_text(
                "#!/bin/sh\n"
                "while [ $# -gt 0 ]; do\n"
                "  if [ \"$1\" = -o ]; then cp \"$FAKE_SOURCE_ARCHIVE\" \"$2\"; exit 0; fi\n"
                "  shift\n"
                "done\nexit 2\n"
            )
            curl.chmod(0o755)
            carry_root = root / "worker"
            env = dict(
                os.environ, PATH=f"{fake_bin}:{os.environ['PATH']}",
                SOURCE_URL_B64=base64.b64encode(b"https://example.invalid/source").decode(),
                KEY_URL_B64="", DOCKER_AUTH_URL_B64="", RESULT_URL_B64="",
                SOURCE_SHA256=hashlib.sha256(archive.read_bytes()).hexdigest(), SOURCE_COMMIT="a" * 40,
                BENCHMARK_MODE="session-smoke-5", BENCHMARK_HARNESS="all", BOOTSTRAP_WAIT_SECONDS="1",
                RUN_ID="gh-test-session", CARRY_ROOT=str(carry_root), SECRET_FILE=str(root / "secret"),
                PYTHON_BIN="python3", SKIP_SHUTDOWN="1", FAKE_SOURCE_ARCHIVE=str(archive),
            )
            run = subprocess.run(["bash", str(SCRIPT)], env=env, text=True, capture_output=True)
            self.assertEqual(run.returncode, 2)
            self.assertIn("retained-session modes require exactly one BENCHMARK_HARNESS=carry, codex, or pi", (carry_root / "results" / "worker.log").read_text())
            self.assertFalse((root / "secret").exists())

    def test_worker_rejects_an_oversized_payback_percent_before_fetching_credentials(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            payload = root / "payload"; payload.mkdir()
            archive = root / "source.tar.gz"
            with tarfile.open(archive, "w:gz") as stream:
                stream.add(payload, arcname="payload")
            fake_bin = root / "bin"; fake_bin.mkdir()
            for command in ("dnf", "systemctl"):
                path = fake_bin / command
                path.write_text("#!/bin/sh\nexit 0\n")
                path.chmod(0o755)
            curl = fake_bin / "curl"
            curl.write_text(
                "#!/bin/sh\n"
                "while [ $# -gt 0 ]; do\n"
                "  if [ \"$1\" = -o ]; then\n"
                "    if [ \"$2\" = \"$CARRY_ROOT/source.tar.gz\" ]; then cp \"$FAKE_SOURCE_ARCHIVE\" \"$2\"; exit 0; fi\n"
                "    touch \"$CREDENTIAL_FETCHED\"; exit 99\n"
                "  fi\n"
                "  shift\n"
                "done\nexit 2\n"
            )
            curl.chmod(0o755)
            carry_root = root / "worker"
            env = dict(
                os.environ, PATH=f"{fake_bin}:{os.environ['PATH']}",
                SOURCE_URL_B64=base64.b64encode(b"https://example.invalid/source").decode(),
                KEY_URL_B64="", DOCKER_AUTH_URL_B64="", RESULT_URL_B64="",
                SOURCE_SHA256=hashlib.sha256(archive.read_bytes()).hexdigest(), SOURCE_COMMIT="a" * 40,
                BENCHMARK_MODE="session-smoke-5", BENCHMARK_HARNESS="carry", BOOTSTRAP_WAIT_SECONDS="1",
                RUN_ID="gh-test-payback-overflow", CARRY_ROOT=str(carry_root), SECRET_FILE=str(root / "secret"),
                PYTHON_BIN="python3", SKIP_SHUTDOWN="1", FAKE_SOURCE_ARCHIVE=str(archive),
                CREDENTIAL_FETCHED=str(root / "credential-fetched"),
                CARRY_COMPACTION_MIN_PAYBACK_PERCENT="9" * 36,
            )
            run = subprocess.run(["bash", str(SCRIPT)], env=env, text=True, capture_output=True)
            self.assertEqual(run.returncode, 2)
            self.assertIn("CARRY_COMPACTION_MIN_PAYBACK_PERCENT must be an integer from 0 through 100", (carry_root / "results" / "worker.log").read_text())
            self.assertFalse((root / "credential-fetched").exists())

    def test_official_worker_forwards_one_declared_attempt_with_official_limits(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            payload = root / "payload"
            (payload / "scripts").mkdir(parents=True)
            archive = root / "source.tar.gz"
            with tarfile.open(archive, "w:gz") as stream:
                stream.add(payload / "scripts", arcname="scripts")

            capability = base64.b64encode(b"https://example.invalid/object").decode()
            config = root / "worker-config"
            config.write_text(
                f"SOURCE_URL_B64={capability}\nKEY_URL_B64={capability}\nDOCKER_AUTH_URL_B64={capability}\n"
                "RESULT_URL_B64=\n"
                "SOURCE_SHA256=" + hashlib.sha256(archive.read_bytes()).hexdigest() + "\n"
                "SOURCE_COMMIT=" + "a" * 40 + "\n"
                "BENCHMARK_MODE=official-50\nBENCHMARK_HARNESS=carry\n"
                "BENCHMARK_ATTEMPT=2\nBENCHMARK_ATTEMPTS=3\nBOOTSTRAP_WAIT_SECONDS=1\nRUN_ID=gh-test-2\n"
                "CARRY_COMPACTION_POLICY=disabled\nCARRY_KEEP_LEASE_TURNS=8\n"
                "CARRY_COMPACTION_MIN_PAYBACK_PERCENT=25\n"
                "CARRY_COMPACTION_NEUTRAL_HIGH_WATERMARK_TOKENS=0\n"
                "CARRY_COMPACTION_NEUTRAL_LOW_WATERMARK_TOKENS=0\n",
                encoding="utf-8",
            )

            fake_bin = root / "bin"
            fake_bin.mkdir()
            for command in ("dnf", "systemctl"):
                path = fake_bin / command
                path.write_text("#!/bin/sh\nexit 0\n")
                path.chmod(0o755)

            curl = fake_bin / "curl"
            curl.write_text(
                "#!/bin/sh\n"
                "output=\n"
                "while [ $# -gt 0 ]; do\n"
                "  if [ \"$1\" = -o ]; then output=$2; shift 2; continue; fi\n"
                "  shift\n"
                "done\n"
                "if [ \"$output\" = \"$CARRY_ROOT/carry-bootstrap-config\" ]; then\n"
                "  cp \"$FAKE_CONFIG\" \"$output\"\n"
                "elif [ \"$output\" = \"$CARRY_ROOT/source.tar.gz\" ]; then\n"
                "  cp \"$FAKE_SOURCE_ARCHIVE\" \"$output\"\n"
                "else\n"
                "  printf fake > \"$output\"\n"
                "fi\n"
            )
            curl.chmod(0o755)

            python = fake_bin / "python3"
            python.write_text(
                "#!/bin/sh\n"
                "if [ \"${1:-}\" = -m ] && [ \"${2:-}\" = venv ]; then\n"
                "  mkdir -p \"$3/bin\"\n"
                "  printf '#!/bin/sh\\nexit 0\\n' > \"$3/bin/pip\"\n"
                "  chmod +x \"$3/bin/pip\"\n"
                "  exit 0\n"
                "fi\n"
                "case \"$*\" in\n"
                "  *swebench_smoke.py*)\n"
                "    printf 'agent=%s\\nevaluator=%s\\nmode=%s\\nbenchmark_attempt=%s\\nbenchmark_attempts=%s\\npolicy=%s\\nlease=%s\\nmargin=%s\\nhigh=%s\\nlow=%s\\nworker=%s\\nagent_phase=%s\\n' \"$AGENT_CONCURRENCY\" \"$EVALUATOR_CONCURRENCY\" \"$BENCHMARK_MODE\" \"$BENCHMARK_ATTEMPT\" \"$BENCHMARK_ATTEMPTS\" \"${CARRY_COMPACTION_POLICY-unset}\" \"${CARRY_KEEP_LEASE_TURNS-unset}\" \"${CARRY_COMPACTION_MIN_PAYBACK_PERCENT-unset}\" \"${CARRY_COMPACTION_NEUTRAL_HIGH_WATERMARK_TOKENS-unset}\" \"${CARRY_COMPACTION_NEUTRAL_LOW_WATERMARK_TOKENS-unset}\" \"$OFFICIAL_WORKER_SECONDS\" \"$OFFICIAL_AGENT_PHASE_SECONDS\" > \"$FAKE_RUNNER_ENV\";;\n"
                "esac\n"
                "exit 0\n"
            )
            python.chmod(0o755)

            carry_root = root / "worker"
            runner_env = root / "runner.env"
            env = dict(
                os.environ,
                PATH=f"{fake_bin}:{os.environ['PATH']}",
                BOOTSTRAP_CONFIG_URL_B64=capability,
                CARRY_ROOT=str(carry_root),
                SECRET_FILE=str(root / "secret"),
                PYTHON_BIN="python3",
                SKIP_SHUTDOWN="1",
                FAKE_CONFIG=str(config),
                FAKE_SOURCE_ARCHIVE=str(archive),
                FAKE_RUNNER_ENV=str(runner_env),
            )
            run = subprocess.run(["bash", str(SCRIPT)], env=env, text=True, capture_output=True)

            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertEqual(
                runner_env.read_text(),
                "agent=5\nevaluator=5\nmode=official-50\nbenchmark_attempt=2\nbenchmark_attempts=3\npolicy=disabled\nlease=8\nmargin=25\nhigh=0\nlow=0\nworker=18900\n"
                "agent_phase=4500\n",
            )

    def test_prepare_worker_uses_registry_auth_without_model_credentials(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            payload = root / "payload"
            (payload / "scripts").mkdir(parents=True)
            archive = root / "source.tar.gz"
            with tarfile.open(archive, "w:gz") as stream:
                stream.add(payload / "scripts", arcname="scripts")

            fake_bin = root / "bin"
            fake_bin.mkdir()
            for command in ("dnf", "systemctl"):
                path = fake_bin / command
                path.write_text("#!/bin/sh\nexit 0\n")
                path.chmod(0o755)
            curl = fake_bin / "curl"
            curl.write_text(
                "#!/bin/sh\n"
                "output=\n"
                "while [ $# -gt 0 ]; do\n"
                "  if [ \"$1\" = -o ]; then output=$2; shift 2; continue; fi\n"
                "  shift\n"
                "done\n"
                "if [ \"$output\" = \"$CARRY_ROOT/source.tar.gz\" ]; then\n"
                "  cp \"$FAKE_SOURCE_ARCHIVE\" \"$output\"\n"
                "else\n"
                "  printf '{\"username\":\"AWS\",\"token\":\"temporary\"}' > \"$output\"\n"
                "fi\n"
            )
            curl.chmod(0o755)
            python = fake_bin / "python3"
            python.write_text(
                "#!/bin/sh\n"
                "if [ \"${1:-}\" = -m ] && [ \"${2:-}\" = venv ]; then\n"
                "  mkdir -p \"$3/bin\"\n"
                "  printf '#!/bin/sh\\nexit 0\\n' > \"$3/bin/pip\"\n"
                "  chmod +x \"$3/bin/pip\"\n"
                "  exit 0\n"
                "fi\n"
                "case \"$*\" in\n"
                "  *docker_registry_login.py*) printf 'registry=%s\\n' \"${4:-}\" > \"$FAKE_LOGIN_ENV\";;\n"
                "  *swebench_smoke.py*)\n"
                "    printf 'argv=%s\\nrepository=%s\\nopenai=%s\\n' \"$*\" \"$TASK_IMAGE_REPOSITORY\" \"${OPENAI_API_KEY-unset}\" > \"$FAKE_RUNNER_ENV\";;\n"
                "esac\n"
                "exit 0\n"
            )
            python.chmod(0o755)

            carry_root = root / "worker"
            capability = base64.b64encode(b"https://example.invalid/object").decode()
            runner_env = root / "runner.env"
            login_env = root / "login.env"
            env = dict(
                os.environ,
                PATH=f"{fake_bin}:{os.environ['PATH']}",
                SOURCE_URL_B64=capability,
                KEY_URL_B64="",
                DOCKER_AUTH_URL_B64=capability,
                REGISTRY_AUTH_URL_B64=capability,
                RESULT_URL_B64="",
                SOURCE_SHA256=hashlib.sha256(archive.read_bytes()).hexdigest(),
                SOURCE_COMMIT="a" * 40,
                BENCHMARK_MODE="prepare-50",
                BENCHMARK_HARNESS="all",
                BOOTSTRAP_WAIT_SECONDS="1",
                RUN_ID="gh-test-3",
                TASK_IMAGE_REPOSITORY="public.ecr.aws/example/carry-swebench-tasks",
                CARRY_ROOT=str(carry_root),
                PYTHON_BIN="python3",
                SKIP_SHUTDOWN="1",
                FAKE_SOURCE_ARCHIVE=str(archive),
                FAKE_RUNNER_ENV=str(runner_env),
                FAKE_LOGIN_ENV=str(login_env),
            )

            run = subprocess.run(["bash", str(SCRIPT)], env=env, text=True, capture_output=True)

            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertIn("--prepare-images", runner_env.read_text())
            self.assertIn("repository=public.ecr.aws/example/carry-swebench-tasks", runner_env.read_text())
            self.assertIn("openai=unset", runner_env.read_text())
            self.assertEqual(login_env.read_text(), "registry=public.ecr.aws\n")


class WorkerDeliveryTests(unittest.TestCase):
    """Run the real shell with isolated side effects and real gzip archives."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        self.worker = self.root / "worker"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.sentinel = "SENTINEL-PRIVATE-CAPABILITY"
        self.capability = "https://example.invalid/" + self.sentinel
        payload = self.root / "payload"
        (payload / "scripts").mkdir(parents=True)
        (payload / "marker.txt").write_text("immutable source\n")
        self.archive = self.root / "source.tar.gz"
        with tarfile.open(self.archive, "w:gz") as stream:
            stream.add(payload, arcname=".")
        self.env = dict(
            os.environ,
            PATH=f"{self.bin}:{os.environ['PATH']}",
            CARRY_ROOT=str(self.worker),
            SOURCE_URL_B64=base64.b64encode(self.capability.encode()).decode(),
            RESULT_URL_B64=base64.b64encode(self.capability.encode()).decode(),
            SOURCE_SHA256=hashlib.sha256(self.archive.read_bytes()).hexdigest(),
            SOURCE_COMMIT="a" * 40, BENCHMARK_MODE="bootstrap",
            BOOTSTRAP_WAIT_SECONDS="0", RUN_ID="test-delivery", PYTHON_BIN=sys.executable,
            KEY_URL_B64="", DOCKER_AUTH_URL_B64="", REGISTRY_AUTH_URL_B64="",
            CONTROL_URL_B64="", BOOTSTRAP_CONFIG_URL_B64="", SKIP_SHUTDOWN="0",
            SECRET_FILE=str(self.root / "secret"),
            DOCKER_AUTH_FILE=str(self.root / "docker-auth"),
            REGISTRY_AUTH_FILE=str(self.root / "registry-auth"),
            DOCKER_CONFIG=str(self.root / "docker-config"),
            FIXTURE_ROOT=str(self.root),
        )
        self.write_command("dnf", 'exit "${PACKAGE_STATUS:-0}"\n')
        self.write_command("systemctl", "exit 0\n")
        self.write_command("shutdown", 'printf shutdown > "$FIXTURE_ROOT/shutdown"\n')
        self.write_command("curl", '''
import json, os, pathlib, shutil, sys
root = pathlib.Path(os.environ['FIXTURE_ROOT'])
args = sys.argv[1:]
with (root / 'curl.jsonl').open('a') as out:
    out.write(json.dumps(args) + '\\n')
if '-T' in args:
    print(os.environ.get('PUT_DIAGNOSTIC', ''))
    print(os.environ.get('PUT_DIAGNOSTIC', ''), file=sys.stderr)
    status = int(os.environ.get('PUT_STATUS', '0'))
    if not status:
        shutil.copyfile(args[args.index('-T') + 1], root / 'uploaded.tar.gz')
    sys.exit(status)
output = pathlib.Path(args[args.index('-o') + 1])
if output.name == 'carry-bootstrap-config':
    output.write_text(os.environ.get('CONFIG_CONTENT', ''))
    print(os.environ.get('CONFIG_DIAGNOSTIC', ''), file=sys.stderr)
    sys.exit(int(os.environ.get('CONFIG_STATUS', '0')))
if output.name == 'source.tar.gz':
    print(os.environ.get('SOURCE_DIAGNOSTIC', ''), file=sys.stderr)
    if os.environ.get('SOURCE_STATUS'):
        sys.exit(int(os.environ['SOURCE_STATUS']))
    shutil.copyfile(root / 'source.tar.gz', output)
else:
    output.write_text('fixture credential')
    if output.name == os.environ.get('CREDENTIAL_FAILURE_TARGET'):
        print(os.environ['CREDENTIAL_DIAGNOSTIC'])
        print(os.environ['CREDENTIAL_DIAGNOSTIC'], file=sys.stderr)
        sys.exit(22)
''', python=True)
        self.write_command("tar", f'''
case " $* " in
  *" -czf "*)

    if [ "${{ARCHIVE_STATUS:-0}}" != 0 ]; then
      printf '%s\\n' "${{ARCHIVE_DIAGNOSTIC:-}}" >&2
      exit "$ARCHIVE_STATUS"
    fi ;;
esac
exec {shutil.which('tar')} "$@"
''')
        self.write_command("timeout", f'''
import json, os, pathlib, sys
args = sys.argv[1:]
with (pathlib.Path(os.environ['FIXTURE_ROOT']) / 'timeout.jsonl').open('a') as out:
    out.write(json.dumps(args) + '\\n')
# Accelerate only delivery deadlines; the real coreutils timeout still owns
# TERM/KILL and process-group enforcement, and the runner's 18000 is untouched.
if os.environ.get('FAST_DELIVERY_TIMEOUTS') == '1' and any(x in args for x in ('tar', 'curl', 'tail')):
    args = ['1s' if x in ('120s', '125s') else '2s' if x == '5s' else
            '--kill-after=0.2s' if x == '--kill-after=5s' else x for x in args]
os.execv({shutil.which('timeout')!r}, ['timeout', *args])
''', python=True)

    def write_command(self, name, body, python=False):
        path = self.bin / name
        path.write_text((f"#!{sys.executable}\n" if python else "#!/bin/bash\n") + body)
        path.chmod(0o755)

    def install_hanging_command(self, name, trigger, *, child=True):
        delegate = self.bin / (name + ".delegate")
        if (self.bin / name).exists():
            (self.bin / name).rename(delegate)
        else:
            delegate.symlink_to(shutil.which(name))
        pids_file = self.root / (name + "-pids.json")
        self.write_command(name, f'''
import json, os, pathlib, signal, subprocess, sys, time
if {trigger!r} is not None and {trigger!r} not in sys.argv:
    os.execv({str(delegate)!r}, [{str(delegate)!r}, *sys.argv[1:]])
signal.signal(signal.SIGTERM, signal.SIG_IGN)
pids = [os.getpid()]
if {child!r}:
    proc = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])
    pids.append(proc.pid)
pathlib.Path({str(pids_file)!r}).write_text(json.dumps(pids))
while True:
    time.sleep(1)
''', python=True)

        def cleanup():
            if pids_file.exists():
                for pid in json.loads(pids_file.read_text()):
                    try:
                        os.kill(pid, 9)
                    except ProcessLookupError:
                        pass
        self.addCleanup(cleanup)
        return pids_file

    def assert_processes_stopped(self, pids_file):
        self.assertTrue(pids_file.exists(), "hanging command was never exercised")
        for pid in json.loads(pids_file.read_text()):
            # A killed grandchild can await PID 1 reaping; it must not run or
            # retain the worker's pipe descriptors after the deadline.
            stat = pathlib.Path(f"/proc/{pid}/stat")
            try:
                state = stat.read_text().rsplit(")", 1)[1].split()[0]
            except FileNotFoundError:
                continue
            self.assertEqual(state, "Z", f"fixture process {pid} survived: {state}")

    def run_worker(self, **changes):
        env = dict(self.env, **changes)
        self.completed = subprocess.run(
            ["bash", str(SCRIPT)], env=env, text=True, capture_output=True, timeout=15,
        )
        return self.completed

    def events(self):
        events = [json.loads(line.split("BENCHMARK_WORKER ", 1)[1])
                  for line in self.completed.stdout.splitlines() if line.startswith("BENCHMARK_WORKER ")]
        for event in events:
            self.assertIsInstance(event.pop("elapsed_seconds"), int)
        return events

    def calls(self, kind):
        path = self.root / f"{kind}.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def configure_runner(self, mode):
        payload = self.root / "payload"
        (payload / "scripts" / "benchmark_preparation_telemetry.py").write_text('''
import json, os, pathlib, signal, sys, time
root = pathlib.Path(os.environ['FIXTURE_ROOT'])
signal.signal(signal.SIGTERM, signal.SIG_IGN)
(root / 'telemetry.json').write_text(json.dumps({'pid': os.getpid(), 'args': sys.argv[1:]}))
print('BENCHMARK_PREPARATION {"stage":"fixture"}', flush=True)
while True:
    time.sleep(1)
''')
        with tarfile.open(self.archive, "w:gz") as stream:
            stream.add(payload, arcname=".")
        self.env.update(
            SOURCE_SHA256=hashlib.sha256(self.archive.read_bytes()).hexdigest(),
            BENCHMARK_MODE=mode, PYTHON_BIN="python3",
            DOCKER_AUTH_URL_B64=self.env["SOURCE_URL_B64"],
            REGISTRY_AUTH_URL_B64=self.env["SOURCE_URL_B64"],
            KEY_URL_B64=self.env["SOURCE_URL_B64"],
            TASK_IMAGE_REPOSITORY="public.ecr.aws/example/tasks",
        )
        self.write_command("python3", f'''
import json, os, pathlib, sys, time
root = pathlib.Path(os.environ['FIXTURE_ROOT'])
args = sys.argv[1:]
if args[:2] == ['-m', 'venv']:
    if os.environ['BENCHMARK_MODE'].startswith('prepare-'):
        deadline = time.monotonic() + 1
        while not (root / 'telemetry.json').exists() and time.monotonic() < deadline:
            time.sleep(.01)
        (root / 'telemetry-before-python-setup').write_text(str((root / 'telemetry.json').exists()))
    bindir = pathlib.Path(args[2]) / 'bin'
    bindir.mkdir(parents=True, exist_ok=True)
    pip = bindir / 'pip'
    pip.write_text('#!/bin/sh\\nexit 0\\n')
    pip.chmod(0o755)
    sys.exit(0)
if args and args[0].endswith('benchmark_preparation_telemetry.py'):
    os.execv({sys.executable!r}, [{sys.executable!r}, *args])
if args and args[0].endswith('swebench_smoke.py'):
    (root / 'runner.json').write_text(json.dumps(args))
    sys.exit(int(os.environ.get('RUNNER_STATUS', '0')))
sys.exit(0)
''', python=True)

    def test_preparation_telemetry_starts_before_python_setup_and_stops_bounded(self):
        self.configure_runner("prepare-long-50")
        started = time.monotonic()
        run = self.run_worker(RUNNER_STATUS="37", PUT_STATUS="28")
        self.assertEqual(run.returncode, 37, run.stdout + run.stderr)
        self.assertTrue((self.root / "telemetry.json").exists())
        telemetry = json.loads((self.root / "telemetry.json").read_text())
        self.assertEqual(telemetry["args"], [
            "--root", str(self.worker / "results"), "--work", str(self.worker / "work"), "--interval", "60",
        ])
        self.assertEqual((self.root / "telemetry-before-python-setup").read_text(), "True")
        self.assertLess(time.monotonic() - started, 10)
        with self.assertRaises(ProcessLookupError):
            os.kill(telemetry["pid"], 0)
        stages = [event["stage"] for event in self.events()]
        for stage in ("source_ready", "credentials", "python_setup", "preparation", "worker_exit", "finished"):
            self.assertIn(stage, stages)
        runner = json.loads((self.root / "runner.json").read_text())
        self.assertIn("--prepare-images", runner)
        deadlines = [call for call in self.calls("timeout") if "python3" in call]
        self.assertEqual(deadlines[0][2], "18000")

    def test_credential_fetch_failures_never_publish_curl_diagnostics(self):
        for target, mode in (("docker-auth", "smoke-5"),
                             ("registry-auth", "prepare-50"), ("secret", "smoke-5")):
            with self.subTest(target=target):
                self.configure_runner(mode)
                run = self.run_worker(CREDENTIAL_FAILURE_TARGET=target,
                                      CREDENTIAL_DIAGNOSTIC=self.capability)
                self.assertEqual(run.returncode, 22, run.stdout + run.stderr)
                self.assertIn({"stage": "credentials", "exit_code": 0}, self.events())
                self.assertNotIn(self.sentinel, run.stdout + run.stderr)
                with tarfile.open(self.root / "uploaded.tar.gz") as stream:
                    for member in stream.getmembers():
                        if member.isfile():
                            content = stream.extractfile(member)
                            assert content is not None
                            with content:
                                self.assertNotIn(self.sentinel.encode(), content.read())
                for name in ("SECRET_FILE", "DOCKER_AUTH_FILE", "REGISTRY_AUTH_FILE"):
                    self.assertFalse(pathlib.Path(self.env[name]).exists())
                self.assertTrue((self.root / "shutdown").exists())

    def test_secret_cleanup_failure_blocks_archive_and_reports_failure(self):
        secret = self.worker / "results" / "secret"
        secret.parent.mkdir(parents=True)
        secret.write_text(self.sentinel)
        self.write_command("rm", f'''
for arg in "$@"; do
  if [ "$arg" = "$SECRET_FILE" ]; then
    printf '%s\\n' "$CLEANUP_DIAGNOSTIC" >&2
    exit 13
  fi
done
exec {shutil.which('rm')} "$@"
''')
        run = self.run_worker(SECRET_FILE=str(secret), CLEANUP_DIAGNOSTIC=self.capability)
        self.assertEqual(run.returncode, 13, run.stdout + run.stderr)
        self.assertIn({"stage": "sanitize_failed", "exit_code": 13}, self.events())
        self.assertTrue((self.root / "shutdown").exists())
        self.assertFalse(any('-T' in call for call in self.calls("curl")))
        self.assertFalse(any('tar' in call for call in self.calls("timeout")))
        self.assertNotIn(self.sentinel, run.stdout + run.stderr)

    def test_source_fetch_failure_records_stage_without_capability_diagnostics(self):
        run = self.run_worker(SOURCE_STATUS="22", SOURCE_DIAGNOSTIC=self.capability)
        self.assertEqual(run.returncode, 22)
        self.assertIn({"stage": "source_fetch", "exit_code": 0}, self.events())
        self.assertIn({"stage": "worker_exit", "exit_code": 22}, self.events())
        self.assertNotIn(self.sentinel, run.stdout + run.stderr)
        with tarfile.open(self.root / "uploaded.tar.gz") as stream:
            log = stream.extractfile("./worker.log")
            assert log is not None
            self.assertNotIn(self.sentinel, log.read().decode())

    def test_early_log_writer_exit_still_sanitizes_and_shuts_down(self):
        self.write_command("tee", '''
import sys
print(sys.stdin.readline(), end='', flush=True)
sys.exit(19)
''', python=True)
        self.write_command("dnf", "sleep 0.2\n")
        secret = pathlib.Path(self.env["SECRET_FILE"])
        secret.write_text(self.sentinel)
        run = self.run_worker()
        self.assertNotEqual(run.returncode, 0)
        self.assertFalse(secret.exists())
        self.assertTrue((self.root / "shutdown").exists())
        self.assertFalse(any('-T' in call for call in self.calls("curl")))
        self.assertEqual(self.events()[-1]["stage"], "finished")

    def test_log_writer_failure_blocks_archive_and_changes_success_status(self):
        self.write_command("tee", f'{shutil.which("tee")} "$@"\nexit 19\n')
        run = self.run_worker()
        self.assertEqual(run.returncode, 19, run.stdout + run.stderr)
        self.assertIn({"stage": "log_drain_failed", "exit_code": 19}, self.events())
        self.assertFalse(any('-T' in call for call in self.calls("curl")))
        self.assertTrue((self.root / "shutdown").exists())

    def test_success_returns_complete_sanitized_archive_after_log_drain(self):
        # Delayed tee exposes archive-vs-log-flush races without replacing tar.
        self.write_command("tee", f'sleep 0.4\nexec {shutil.which("tee")} "$@"\n')
        self.worker.mkdir()
        (self.worker / "carry-bootstrap-config").write_text(self.sentinel)
        for name in ("SECRET_FILE", "DOCKER_AUTH_FILE", "REGISTRY_AUTH_FILE"):
            pathlib.Path(self.env[name]).write_text(self.sentinel)
        config = pathlib.Path(self.env["DOCKER_CONFIG"])
        config.mkdir()
        (config / "config.json").write_text(self.sentinel)
        run = self.run_worker()
        self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
        with tarfile.open(self.root / "uploaded.tar.gz") as stream:
            files = {}
            for member in stream.getmembers():
                if member.isfile():
                    content = stream.extractfile(member)
                    assert content is not None
                    files[member.name] = content.read()
        self.assertEqual(files["./worker-exit-status"], b"0\n")
        self.assertIn("./worker.log", files)
        self.assertIn(b'"stage":"worker_exit"', files["./worker.log"])
        self.assertNotIn(self.sentinel.encode(), b"".join(files.values()))
        for name in ("SECRET_FILE", "DOCKER_AUTH_FILE", "REGISTRY_AUTH_FILE", "DOCKER_CONFIG"):
            self.assertFalse(pathlib.Path(self.env[name]).exists())
        self.assertFalse((self.worker / "carry-bootstrap-config").exists())
        self.assertEqual(self.events()[-1], {"stage": "finished", "exit_code": 0})
        self.assertTrue((self.root / "shutdown").exists())

    def test_package_failure_has_lifecycle_status_and_preserves_original_failure(self):
        run = self.run_worker(PACKAGE_STATUS="37", PUT_STATUS="28")
        self.assertEqual(run.returncode, 37, run.stdout + run.stderr)
        self.assertTrue((self.root / "shutdown").exists())
        self.assertEqual(self.events(), [
            {"stage": "starting", "exit_code": 0},
            {"stage": "package_setup", "exit_code": 0},
            {"stage": "worker_exit", "exit_code": 37},
            {"stage": "archive", "exit_code": 0},
            {"stage": "upload", "exit_code": 0},
            {"stage": "upload_failed", "exit_code": 28},
            {"stage": "finished", "exit_code": 37},
        ])
        self.assertEqual((self.worker / "results" / "worker-exit-status").read_text(), "37\n")

    def test_bootstrap_fetch_failure_is_observable_and_shuts_down_without_config(self):
        self.env.pop("SOURCE_URL_B64")
        self.env.pop("RESULT_URL_B64")
        run = self.run_worker(
            BOOTSTRAP_CONFIG_URL_B64=base64.b64encode(self.capability.encode()).decode(),
            CONFIG_STATUS="22", CONFIG_CONTENT=self.sentinel, CONFIG_DIAGNOSTIC=self.capability,
        )
        self.assertEqual(run.returncode, 22)
        self.assertTrue((self.root / "shutdown").exists())
        self.assertFalse((self.worker / "carry-bootstrap-config").exists())
        self.assertIn({"stage": "bootstrap_config", "exit_code": 0}, self.events())
        self.assertIn({"stage": "worker_exit", "exit_code": 22}, self.events())
        self.assertIn({"stage": "finished", "exit_code": 22}, self.events())
        log = (self.worker / "results" / "worker.log").read_text()
        self.assertIn('"stage":"bootstrap_config"', log)
        self.assertNotIn(self.sentinel, run.stdout + run.stderr + log)

    def test_archive_has_deadline_and_does_not_log_raw_errors(self):
        run = self.run_worker(ARCHIVE_STATUS="23", ARCHIVE_DIAGNOSTIC=self.capability)
        archives = [call for call in self.calls("timeout") if "tar" in call]
        self.assertEqual(len(archives), 1)
        self.assertEqual(archives[0][:3], ["--signal=TERM", "--kill-after=5s", "120s"])
        self.assertNotIn(self.sentinel, run.stdout + run.stderr)

    def test_put_failure_is_bounded_reported_and_changes_success_status(self):
        run = self.run_worker(PUT_STATUS="28", PUT_DIAGNOSTIC=self.capability)
        self.assertEqual(run.returncode, 28, run.stdout + run.stderr)
        self.assertTrue((self.root / "shutdown").exists())
        self.assertIn({"stage": "upload_failed", "exit_code": 28}, self.events())
        self.assertNotIn(self.sentinel, run.stdout + run.stderr)
        puts = [call for call in self.calls("curl") if '-T' in call]
        self.assertEqual(len(puts), 1)
        for flag, expected in (("--connect-timeout", "10"), ("--max-time", "60"),
                               ("--retry", "2"), ("--retry-max-time", "120")):
            self.assertEqual(puts[0][puts[0].index(flag) + 1], expected)
        self.assertIn("--retry-all-errors", puts[0])
        upload_deadlines = [call for call in self.calls("timeout") if "curl" in call]
        self.assertEqual(len(upload_deadlines), 1)
        self.assertEqual(upload_deadlines[0][:3], ["--signal=TERM", "--kill-after=5s", "125s"])

    def test_archive_failure_preserves_the_original_worker_failure(self):
        run = self.run_worker(PACKAGE_STATUS="37", ARCHIVE_STATUS="23")
        self.assertEqual(run.returncode, 37, run.stdout + run.stderr)
        self.assertIn({"stage": "archive_failed", "exit_code": 23}, self.events())
        self.assertEqual(self.events()[-1], {"stage": "finished", "exit_code": 37})
        self.assertEqual((self.worker / "results/worker-exit-status").read_text(), "37\n")
        self.assertFalse(any('-T' in call for call in self.calls("curl")))
        self.assertTrue((self.root / "shutdown").exists())

    def test_non_preparation_modes_do_not_start_preparation_telemetry(self):
        for mode in ("bootstrap", "smoke-5", "long-smoke-5", "session-smoke-5",
                     "session-20", "official-50", "long-official-50"):
            with self.subTest(mode=mode):
                self.configure_runner(mode)
                run = self.run_worker()
                self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
                self.assertFalse((self.root / "telemetry.json").exists())
                self.assertNotIn("BENCHMARK_PREPARATION", run.stdout + run.stderr)
                self.assertNotIn("preparation", [event["stage"] for event in self.events()])

    def test_hanging_archive_is_killed_without_uploading_or_blocking_shutdown(self):
        pids = self.install_hanging_command("tar", "-czf")
        started = time.monotonic()
        run = self.run_worker(FAST_DELIVERY_TIMEOUTS="1")
        self.assertLess(time.monotonic() - started, 5)
        self.assertEqual(run.returncode, 137, run.stdout + run.stderr)
        self.assertIn({"stage": "archive_failed", "exit_code": 137}, self.events())
        self.assertFalse(any('-T' in call for call in self.calls("curl")))
        self.assertTrue((self.root / "shutdown").exists())
        self.assert_processes_stopped(pids)

    def test_hanging_put_is_killed_without_blocking_shutdown(self):
        pids = self.install_hanging_command("curl", "-T")
        started = time.monotonic()
        run = self.run_worker(FAST_DELIVERY_TIMEOUTS="1")
        self.assertLess(time.monotonic() - started, 5)
        self.assertEqual(run.returncode, 137, run.stdout + run.stderr)
        self.assertIn({"stage": "upload_failed", "exit_code": 137}, self.events())
        self.assertTrue((self.root / "shutdown").exists())
        self.assert_processes_stopped(pids)

    def test_hanging_log_writer_is_killed_without_archiving_incomplete_logs(self):
        pids = self.install_hanging_command("tee", None, child=False)
        started = time.monotonic()
        run = self.run_worker(FAST_DELIVERY_TIMEOUTS="1")
        self.assertLess(time.monotonic() - started, 5)
        self.assertEqual(run.returncode, 137, run.stdout + run.stderr)
        self.assertIn({"stage": "log_drain_failed", "exit_code": 137}, self.events())
        self.assertFalse(any('-T' in call for call in self.calls("curl")))
        self.assertTrue((self.root / "shutdown").exists())
        self.assert_processes_stopped(pids)

    def test_archive_failure_is_not_success_and_never_uploads_partial_tar(self):
        run = self.run_worker(ARCHIVE_STATUS="23")
        self.assertNotEqual(run.returncode, 0, run.stdout + run.stderr)
        self.assertTrue((self.root / "shutdown").exists())
        self.assertFalse(any('-T' in call for call in self.calls("curl")))
        self.assertIn({"stage": "archive_failed", "exit_code": 23}, self.events())


if __name__ == "__main__":
    unittest.main(verbosity=2)
