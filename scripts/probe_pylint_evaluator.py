#!/usr/bin/env python3
"""Default-off exact-image diagnostic. Never a repair, catalog, or scoring gate.

Expected negatives are mandatory. Only public, unchanged test_self selectors run;
no task/test/gold patch is loaded. The path-based .pth arm is disposable evidence.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys

TASK = {"instance_id": "pylint-dev__pylint-7080", "repo": "pylint-dev/pylint",
        "version": "2.15", "base_commit": "3c5eca2ded3dd2b59ebaf23eb289453b5d2930f0",
        "environment_setup_commit": "e90702074e68e20dc8e5df5013ee3ecf22139c3e"}
IMAGES = {
    "evaluator": "public.ecr.aws/r2a8a1y1/carry-swebench-tasks@sha256:061541ce3505bf651c0ddd0508bab7099f76a62d6cf6f32b139169cdac0d99a6",
    "agent": "public.ecr.aws/r2a8a1y1/carry-swebench-tasks@sha256:09bda346378c2ca50e68da95540584b060453b5b32cf838c5ddba69af2df4765",
}
PHASES = ("baseline", "reinstalled", "control", "control-reinstalled")
INSTALL = "python -m pip install -e ."
PYTHON = "/opt/miniconda3/envs/testbed/bin/python"

# This runner adds a live disk watchdog to the existing process/log conventions:
# Docker's daemon is outside RLIMIT_AS, so container memory is bounded separately.
LOG_LIMIT = 1024**2
WALL_SECONDS = 1200
MEMORY_BYTES = 3 * 1024**3


class Transport:
    def __init__(self, evidence, work, timeout=WALL_SECONDS, disk_reserve=8 * 1024**3):
        import shutil
        import time
        self.evidence, self.work = Path(evidence), Path(work)
        self.evidence.mkdir(parents=True, exist_ok=True)
        self.work.mkdir(parents=True, exist_ok=True)
        self.home = self.work / "home"
        self.home.mkdir(exist_ok=True)
        self.deadline = time.monotonic() + timeout
        self.disk_floors = {}
        self.watch_disk(self.work, reserve=disk_reserve)
        self.serial = 0

    def watch_disk(self, path, reserve=8 * 1024**3):
        import shutil
        self.disk_floors[Path(path)] = max(reserve, shutil.disk_usage(path).free - 12 * 1024**3)

    def __call__(self, command, *, label, allowed=(0,), seconds=180, cleanup=False):
        import resource
        import selectors
        import shutil
        import signal
        import subprocess
        import time
        self.serial += 1
        directory = self.evidence / ("%03d-" % self.serial + label)
        directory.mkdir()
        deadline = time.monotonic() + seconds
        if not cleanup:
            deadline = min(deadline, self.deadline)
        def limits():
            resource.setrlimit(resource.RLIMIT_AS, (MEMORY_BYTES, MEMORY_BYTES))
            resource.setrlimit(resource.RLIMIT_CPU, (300, 300))
            resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        # Never consult caller Docker credentials, HF tokens, cloud/model vars,
        # proxies, PYTHONPATH or the caller's home. Only public pulls are allowed.
        environment = {"HOME": str(self.home), "PATH": "/usr/bin:/bin", "LANG": "C.UTF-8",
                       "DOCKER_CONFIG": str(self.home / "docker"), "TMPDIR": str(self.work)}
        process = subprocess.Popen(command, cwd=self.work, env=environment,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True, preexec_fn=limits)
        selector = selectors.DefaultSelector()
        streams = {"stdout": bytearray(), "stderr": bytearray()}
        reason = None
        try:
            selector.register(process.stdout, selectors.EVENT_READ, "stdout")
            selector.register(process.stderr, selectors.EVENT_READ, "stderr")
            while selector.get_map() or process.poll() is None:
                if time.monotonic() >= deadline:
                    reason = "timeout"
                    break
                if not cleanup and any(shutil.disk_usage(path).free < floor for path, floor in self.disk_floors.items()):
                    reason = "disk budget"
                    break
                for key, _ in selector.select(0.1):
                    chunk = os.read(key.fileobj.fileno(), 65536)
                    if not chunk:
                        selector.unregister(key.fileobj)
                    else:
                        target = streams[key.data]
                        room = LOG_LIMIT - len(target)
                        target.extend(chunk[:room])
                        if len(chunk) > room:
                            reason = "log budget"
                if reason:
                    break
        finally:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=10)
            selector.close()
            process.stdout.close()
            process.stderr.close()
            for name, value in streams.items():
                (directory / (name + ".log")).write_bytes(value)
            write_json(directory / "process.json", {"command": command, "returncode": process.returncode,
                                                    "failure": reason})
        if reason or process.returncode not in allowed:
            raise RuntimeError(label + ": " + (reason or "unexpected exit " + str(process.returncode)))
        return subprocess.CompletedProcess(command, process.returncode,
            streams["stdout"].decode(errors="replace"), streams["stderr"].decode(errors="replace"))


def write_json(path, value):
    # Reuse the existing atomic writer without changing its semantics.
    from scripts.probe_matplotlib_preparation import write_json as write
    write(path, value)


def preflight(work):
    import platform
    import shutil
    from scripts.probe_matplotlib_solver import memory_snapshot
    if (os.geteuid() != 0 or platform.system() != "Linux" or platform.machine() != "x86_64"
            or memory_snapshot()["MemAvailable_bytes"] < 6 * 1024**3
            or shutil.disk_usage(work).free < 20 * 1024**3):
        raise ValueError("requires root hosted Linux amd64, 6 GiB available RAM, 20 GiB disk")


def parse_payload(process):
    payloads = [line.removeprefix("PROBE_JSON=") for line in process.stdout.splitlines()
                if line.startswith("PROBE_JSON=")]
    if len(payloads) != 1:
        raise ValueError("missing or duplicate container diagnostic payload")
    payload = json.loads(payloads[0])
    if "returncode" in payload and payload["returncode"] != process.returncode:
        raise ValueError("container payload disagrees with actual process exit")
    return payload


def run_probe(evidence, work, *, execute=None):
    import uuid
    from scripts.probe_matplotlib_preparation import index_evidence
    evidence, work = Path(evidence).resolve(), Path(work).resolve()
    evidence.mkdir(parents=True, exist_ok=False)
    work.mkdir(parents=True, exist_ok=False)
    report = {"status": "failed", "diagnostic_validated": False, "environment_ready": False,
              "score_validated": False, "cleanup_verified": False, "task": TASK,
              "github_sha": os.environ.get("GITHUB_SHA"), "github_run_id": os.environ.get("GITHUB_RUN_ID"),
              "limits": {"wall_seconds": WALL_SECONDS, "container_memory_bytes": MEMORY_BYTES,
                         "disk_growth_budget_bytes": 12 * 1024**3, "disk_reserve_bytes": 8 * 1024**3,
                         "disk_enforcement": "100ms free-space watchdog, ephemeral hosted daemon",
                         "log_bytes_per_stream": LOG_LIMIT}, "arms": {}}
    name = "carry-pylint-probe-" + uuid.uuid4().hex
    owned = False
    run = execute or Transport(evidence, work)
    try:
        preflight(work)
        # An empty hosted daemon is necessary for meaningful disk accounting;
        # cleanup below still targets ONLY the randomly named probe container.
        if run(["docker", "ps", "-aq"], label="initial").stdout.strip():
            raise ValueError("requires empty ephemeral Docker daemon")
        info_format = '{"MemoryLimit":{{json .MemoryLimit}},"SwapLimit":{{json .SwapLimit}},"DockerRootDir":{{json .DockerRootDir}}}'
        info = json.loads(run(["docker", "info", "--format", info_format], label="daemon").stdout)
        if info.get("MemoryLimit") is not True or info.get("SwapLimit") is not True:
            raise ValueError("Docker memory/swap limits unavailable")
        if isinstance(run, Transport):
            import shutil
            daemon_root = Path(info["DockerRootDir"])
            if shutil.disk_usage(daemon_root).free < 20 * 1024**3:
                raise ValueError("insufficient Docker daemon disk reserve")
            run.watch_disk(daemon_root)
        metadata = json.loads(run([sys.executable, str(Path(__file__).resolve()), "--metadata"],
                                  label="metadata").stdout)
        if metadata != TASK:
            raise ValueError("pinned public metadata mismatch")
        write_json(evidence / "inputs.json", {"task": TASK, "install": INSTALL,
                   "dataset_revision": "c104f840cc67f8b6eec6f759ebc8b2693d585d4a",
                   "origin_run": "35684292331", "catalog_run": "35681907012",
                   "build_isolation": "official pip defaults preserved; runtime setuptools does not identify the isolated build backend",
                   "agent_image_scope": "immutable identity inspection only; evaluator image runs the differential"})
        identities = {}
        # Project identity fields in Docker itself, never dump Config.Env.
        fmt = ('{"Id":{{json .Id}},"RepoDigests":{{json .RepoDigests}},'
               '"Architecture":{{json .Architecture}},"Os":{{json .Os}}}')
        for role, reference in IMAGES.items():
            run(["docker", "pull", "--platform", "linux/amd64", reference], label=role+"-pull", seconds=300)
            identity = json.loads(run(["docker", "image", "inspect", "--format", fmt, reference],
                                      label=role+"-identity").stdout)
            if (reference not in identity.get("RepoDigests", []) or identity.get("Architecture") != "amd64"
                    or identity.get("Os") != "linux" or not identity.get("Id", "").startswith("sha256:")):
                raise ValueError("immutable image identity mismatch")
            identities[role] = identity
        write_json(evidence / "image-identities.json", identities)
        # Only this nonsecret, immutable diagnostic script is bound into the
        # container. No host checkout/home/socket, credentials or test artifacts.
        command = ["docker", "create", "--name", name, "--network", "none", "--user", "0:0",
            "--cap-drop=ALL", "--security-opt", "no-new-privileges", "--memory", str(MEMORY_BYTES),
            "--memory-swap", str(MEMORY_BYTES), "--cpus", "2", "--pids-limit", "256",
            "--ulimit", "fsize=67108864:67108864", "--log-driver", "none",
            "--tmpfs", "/tmp:rw,exec,nosuid,nodev,size=1073741824", "--workdir", "/tmp",
            "--mount", "type=bind,src=" + str(Path(__file__).resolve()) + ",dst=/diagnostic/probe.py,readonly",
            "--entrypoint", "/bin/sleep", IMAGES["evaluator"], str(WALL_SECONDS)]
        # Record ownership before create: a timed-out CLI may already have
        # created its unique target. Never remove unrelated daemon containers.
        owned = True
        run(command, label="create")
        run(["docker", "start", name], label="start")
        prefix = ["docker", "exec", "--workdir", "/tmp", name, "env", "-i", "HOME=/tmp",
                  "PATH=/opt/miniconda3/envs/testbed/bin:/opt/miniconda3/bin:/usr/bin:/bin", "LANG=C.UTF-8"]
        def inside(action, phase, allowed=(0,)):
            return parse_payload(run(prefix + [PYTHON, "/diagnostic/probe.py", "--inside", action],
                                     label=phase+"-"+action, allowed=allowed))
        def network(expected):
            actual = json.loads(run(["docker", "inspect", "--format", "{{json .NetworkSettings.Networks}}", name],
                                    label="network").stdout)
            if set(actual) != {expected}:
                raise ValueError("unexpected network before execution")
        def switch(old, new):
            run(["docker", "network", "disconnect", old, name], label="disconnect-"+old)
            run(["docker", "network", "connect", new, name], label="connect-"+new)
            network(new)
        for phase in PHASES:
            if phase == "control":
                inside("path-control", phase)
            if phase in ("reinstalled", "control-reinstalled"):
                switch("none", "bridge")
                run(prefix + ["/bin/bash", "-ec", "source /opt/miniconda3/bin/activate; conda activate testbed; cd /testbed; " + INSTALL],
                    label=phase+"-install", seconds=240)
                switch("bridge", "none")
            network("none")
            arm = {"snapshot": inside("snapshot", phase)}
            # Fail before any installer if source/image/version is wrong.
            snapshot = arm["snapshot"]
            expected = phase.startswith("control")
            validate_source_identity(snapshot)
            if (not snapshot["clean"]
                    or snapshot["python"] != [3, 9]
                    or snapshot["versions"].get("pylint") != "2.15.0.dev0"
                    or snapshot["versions"].get("astroid") != "2.11.7"
                    or snapshot["versions"].get("setuptools") != "67.4.0"
                    or snapshot["origins"].get("pylint") != "/testbed/pylint/__init__.py"
                    or not snapshot["editable_finder"] or snapshot["control_pth"] != expected
                    or ("/testbed" in snapshot["sys_path"]) != expected):
                raise ValueError("source/version/editable layout invariant failed")
            arm["checkers"] = inside("checkers", phase, allowed=(0, 1))
            cli = run(prefix + ["/opt/miniconda3/envs/testbed/bin/pylint", "--rcfile=/dev/null",
                "--persistent=n", "--disable=all", "--enable=c-extension-no-member", "pylint.__pkginfo__"],
                label=phase+"-cli", allowed=(0, 1))
            log = cli.stdout + cli.stderr
            arm["cli"] = {"returncode": cli.returncode,
                          "unknown_message": "UnknownMessageError" in log and "c-extension-no-member" in log}
            arm["tests"] = inside("tests", phase, allowed=(0, 1))
            after = inside("snapshot", phase)
            validate_source_identity(after)
            if (not after["clean"] or any(after[key] != snapshot[key] for key in ("base_commit", "source_identity", "public_test_sha256"))):
                raise ValueError("public source mutated during tests")
            report["arms"][phase] = arm
            write_json(evidence / "result.json", report)
        report.update(validate_observations(report["arms"]), status="diagnostic-validated")
    except Exception as error:
        report["error"] = type(error).__name__ + ": " + str(error)[:1000]
    finally:
        if owned:
            try:
                run(["docker", "rm", "-f", name], label="remove", seconds=30, cleanup=True, allowed=(0, 1))
                remaining = run(["docker", "ps", "-aq", "--filter", "name=^/"+name+"$"],
                                label="remaining", seconds=30, cleanup=True).stdout.strip()
                if remaining:
                    raise RuntimeError("exact probe container remains")
                report["cleanup_verified"] = True
            except Exception as error:
                report.update(status="failed", diagnostic_validated=False, cleanup_error=str(error)[:1000])
        if not report["cleanup_verified"]:
            report.update(status="failed", diagnostic_validated=False)
        write_json(evidence / "result.json", report)
        if index_evidence(evidence):
            report.update(status="failed", diagnostic_validated=False, error="evidence exceeded budget")
            write_json(evidence / "result.json", report)
            index_evidence(evidence)
    return 0 if report["diagnostic_validated"] else 1



def validate_source_identity(snapshot):
    """Bind the setup commit to the dataset base, without trusting its build-time SHA.

    SWE-bench 4.1.0 test_spec/python.py:make_repo_script_list_py finishes
    with `git commit --allow-empty -am SWE-bench`. Pylint 2.15 has only
    the editable install, no source-changing pre_install. Require an unchanged
    tree and exactly that base as parent; never accept arbitrary descendants.
    Legacy snapshots without this evidence deliberately fail closed.
    """
    import re
    identity = snapshot.get("source_identity", {})
    if (not isinstance(identity, dict)
            or not all(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{40}", value)
                       for value in (snapshot.get("base_commit"), identity.get("tree"), identity.get("base_tree")))
            or identity.get("parents") != [TASK["base_commit"]]
            or identity.get("tree") != identity.get("base_tree")
            or identity.get("subject") != "SWE-bench"
            or snapshot.get("base_commit") == TASK["base_commit"]
            or snapshot.get("clean") is not True):
        raise ValueError("source identity requires unchanged SWE-bench setup commit directly on dataset base")


def validate_observations(arms):
    """A successful diagnostic proves the differential, not evaluator validity."""
    if set(arms) != set(PHASES):
        raise ValueError("missing diagnostic phase")
    hashes = set()
    for phase in PHASES:
        arm = arms[phase]
        snapshot = arm["snapshot"]
        positive = phase.startswith("control")
        validate_source_identity(snapshot)
        if (snapshot["clean"] is not True
                or snapshot["python"] != [3, 9]
                or snapshot["versions"].get("pylint") != "2.15.0.dev0"
                or snapshot["versions"].get("astroid") != "2.11.7"
                or snapshot["versions"].get("setuptools") != "67.4.0"
                or snapshot["origins"].get("pylint") != "/testbed/pylint/__init__.py"
                or snapshot["editable_finder"] is not True
                or snapshot["control_pth"] != positive
                or ("/testbed" in snapshot["sys_path"]) != positive):
            raise ValueError("source/version/editable layout invariant failed: " + phase)
        hashes.add(snapshot["public_test_sha256"])
        checkers, cli, tests = (arm[key] for key in ("checkers", "cli", "tests"))
        if tests["collected"] != 2 or tests["errors"] or tests["skipped"]:
            raise ValueError("public tests did not execute exactly twice: " + phase)
        if positive:
            good = (checkers["returncode"] == cli["returncode"] == tests["returncode"] == 0
                    and checkers["checker_count"] >= 30 and checkers["error"] is None
                    and not cli["unknown_message"] and tests["passed"] == 2
                    and tests["failed"] == tests["unknown_message_failures"] == 0)
        else:
            good = (checkers["returncode"] == cli["returncode"] == tests["returncode"] == 1
                    and checkers["error"] == "UnknownMessageError"
                    and "c-extension-no-member" in checkers["message"]
                    and cli["unknown_message"] and tests["passed"] == 0
                    and tests["failed"] == tests["unknown_message_failures"] == 2)
        if not good:
            raise ValueError("unexpected CLI/checker/public-test outcome: " + phase)
    if len(hashes) != 1:
        raise ValueError("public test bytes changed")
    return {"diagnostic_validated": True, "environment_ready": False, "score_validated": False,
            "scope": "path-discovery differential only; full unchanged official evaluator/gold gate still required"}

SELECTORS = ["/testbed/tests/test_self.py::TestRunTC::" + name
             for name in ("test_pkginfo", "test_ignore_path_recursive")]


def install_control(site):
    path = Path(site) / "carry_diagnostic_testbed.pth"
    with path.open("x") as stream:
        stream.write("/testbed\n")
    return {"created": True, "path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}


def snapshot(repo=Path("/testbed"), site=None):
    """Project only public setup/layout metadata, never dump env or task data."""
    import importlib.metadata as metadata
    import subprocess
    import sysconfig
    import pylint
    import astroid
    site = Path(site or sysconfig.get_paths()["purelib"])
    def git(*args):
        return subprocess.run(["git", "-c", "safe.directory="+str(repo), "-C", str(repo), *args],
                              check=True, capture_output=True, text=True, timeout=10).stdout.strip()
    files = {}
    paths = list(site.glob("__editable__*pylint*.pth")) + list(site.glob("__editable__*pylint*finder.py"))
    paths += [site / "carry_diagnostic_testbed.pth"]
    paths += [repo / name for name in ("setup.py", "setup.cfg", "pyproject.toml", "pylint/__pkginfo__.py")]
    total = 0
    for path in paths:
        if not path.exists():
            continue
        if path.is_symlink() or path.stat().st_size > 65536:
            raise ValueError("unexpected public layout file")
        data = path.read_bytes()
        total += len(data)
        if total > 256 * 1024:
            raise ValueError("layout evidence budget exceeded")
        files[str(path)] = {"sha256": hashlib.sha256(data).hexdigest(), "content": data.decode()}
    # `base_commit` is the legacy field name for actual HEAD, not dataset base.
    return {"base_commit": git("rev-parse", "HEAD"),
            "source_identity": {"parents": git("show", "-s", "--format=%P", "HEAD").split(),
                                "tree": git("rev-parse", "HEAD^{tree}"),
                                "base_tree": git("rev-parse", TASK["base_commit"] + "^{tree}"),
                                "subject": git("show", "-s", "--format=%s", "HEAD")},
            "clean": not git("status", "--porcelain", "--untracked-files=no"),
            "python": list(sys.version_info[:2]), "executable": sys.executable,
            "versions": {name: metadata.version(name) for name in ("pylint", "astroid", "setuptools", "pip", "pytest")},
            "inventory": sorted((d.metadata["Name"], d.version) for d in metadata.distributions() if d.metadata["Name"]),
            "origins": {"pylint": pylint.__file__, "astroid": astroid.__file__},
            "sys_path": sys.path, "cwd": os.getcwd(),
            "meta_path": [getattr(f, "__module__", type(f).__module__) for f in sys.meta_path],
            "editable_finder": any("finder.py" in p and "/testbed/pylint" in v["content"] for p, v in files.items()),
            "control_pth": (site / "carry_diagnostic_testbed.pth").is_file(),
            "public_test_sha256": hashlib.sha256((repo / "tests/test_self.py").read_bytes()).hexdigest(),
            "public_layout": files}


def checker_probe(factory=None):
    if factory is None:
        from pylint.lint import PyLinter
        factory = PyLinter
    linter = factory()
    result = {"returncode": 0, "error": None, "message": "", "checker_count": 0}
    try:
        linter.load_default_plugins()
        linter.enable("c-extension-no-member")
    except Exception as error:
        result.update(returncode=1, error=type(error).__name__, message=str(error))
    result["checker_count"] = len(linter.get_checkers())
    return result


class TestRecorder:
    def __init__(self):
        self.result = {"collected": 0, "passed": 0, "failed": 0, "errors": 0,
                       "skipped": 0, "unknown_message_failures": 0}

    def pytest_collection_finish(self, session):
        self.result["collected"] = len(session.items)

    def pytest_runtest_logreport(self, report):
        if report.outcome == "skipped":
            self.result["skipped"] += 1
        elif report.when == "call":
            self.result[report.outcome] += 1
            text = str(report.longrepr)
            if report.outcome == "failed" and "UnknownMessageError" in text and "c-extension-no-member" in text:
                self.result["unknown_message_failures"] += 1
        elif report.outcome == "failed":
            self.result["errors"] += 1


def public_tests(selectors=SELECTORS):
    import pytest
    recorder = TestRecorder()
    # Explicit diagnostic import mode prevents pytest prepending /testbed to
    # sys.path. This is NOT the full official evaluator or a score validation.
    code = pytest.main(["-rA", "-vv", "--tb=short", "--import-mode=importlib", *selectors], plugins=[recorder])
    return dict(recorder.result, returncode=int(code))


def public_metadata():
    from importlib.metadata import version
    from datasets import load_dataset
    if version("swebench") != "4.1.0":
        raise ValueError("requires pinned swebench==4.1.0")
    records = load_dataset("princeton-nlp/SWE-bench_Verified", split="test",
        revision="c104f840cc67f8b6eec6f759ebc8b2693d585d4a", token=False)
    rows = [{key: record.get(key) for key in TASK} for record in records
            if record["instance_id"] == TASK["instance_id"]]
    if rows != [TASK]:
        raise ValueError("pinned public task metadata mismatch")
    return rows[0]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence-dir", type=Path)
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--inside", choices=("snapshot", "checkers", "tests", "path-control"), help=argparse.SUPPRESS)
    parser.add_argument("--metadata", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.inside:
        if Path(__file__) != Path("/diagnostic/probe.py") or sys.executable != PYTHON or os.geteuid() != 0:
            parser.error("container phases require the exact diagnostic mount and image interpreter")
        import sysconfig
        if args.inside == "snapshot":
            result = snapshot()
        elif args.inside == "checkers":
            result = checker_probe()
        elif args.inside == "tests":
            result = public_tests()
        else:
            result = install_control(sysconfig.get_paths()["purelib"])
        print("PROBE_JSON=" + json.dumps(result, sort_keys=True))
        return result.get("returncode", 0)
    if args.metadata:
        print(json.dumps(public_metadata(), sort_keys=True))
        return 0
    if (os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted"
            or os.geteuid() != 0):
        parser.error("actual Docker diagnostic requires the opt-in root GitHub-hosted job")
    if not args.evidence_dir or not args.work_dir:
        parser.error("--evidence-dir and --work-dir are required")
    # Host-only imports; never add /testbed or any PYTHONPATH in the container.
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    import signal
    def interrupted(signum, frame):
        raise RuntimeError("diagnostic interrupted by signal " + str(signum))
    previous = signal.signal(signal.SIGTERM, interrupted)
    try:
        return run_probe(args.evidence_dir, args.work_dir)
    finally:
        signal.signal(signal.SIGTERM, previous)


if __name__ == "__main__":
    raise SystemExit(main())
