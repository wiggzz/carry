#!/usr/bin/env python3
"""Temporary Linux/stdlib hosted diagnosis; never a production repair.

inner executes the inherited ORIGINAL single case, preserving its 5-second socket.
hosted reuses executables reported by the preceding ordinary cargo test command.
"""
import argparse
import contextlib
import hashlib
import importlib
import json
import os
from pathlib import Path
import resource
import selectors
import signal
import subprocess
import sys
import threading
import time
import unittest
from unittest.mock import patch

CASE = "test_bootstrap_keeps_protected_sources_across_chunks"
BASE = "444e55e4ffe442d94ebc94b08d1e6d3d829734b4"
SOURCE_HASHES = {
    "tests/proxy_incremental.py": "8e3bccb9bbf3ae1ca5dd3bdbfff8d3cbb84f624f815f69ce270089de0c3678e7",
    "tests/proxy_state_regressions.py": "883661027ed87388f6df21fb2abe477c3ddfb3ac960c6ff0b8315b32728f6b52",
    "tests/proxy.rs": "354a308378c6df2fb3fd7ba035fec238b9b9a36fe4efa197c97fd9a8725ef61e",
}
OUTPUT_LIMIT = 131072
EVENT_LIMIT = 524288


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def dump(path, value):
    Path(path).write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def proc_sample(pid):
    """No cmdline/environ: numeric CPU, task runnable-wait and memory only.

    schedstat is per-thread; sum extant tasks, never mislabel it process lifetime.
    Values can decrease when a thread exits. Kernel support can leave wait zero.
    """
    try:
        stat = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        result = {"pid": pid, "state": stat[0], "cpu_ticks": int(stat[11]) + int(stat[12]),
                  "rss_pages": int(stat[21]), "threads": int(stat[17])}
        running = waiting = tasks = 0
        for task in Path(f"/proc/{pid}/task").iterdir():
            try:
                values = (task / "schedstat").read_text().split()
                running += int(values[0]); waiting += int(values[1]); tasks += 1
            except (OSError, ValueError, IndexError):
                pass
        result.update(extant_task_run_ns=running, extant_task_wait_ns=waiting,
                      sampled_tasks=tasks)
        return result
    except (OSError, ValueError, IndexError):
        return {"pid": pid, "unavailable": True}


def cpu_environment():
    result = {"cpu_count": os.cpu_count(), "affinity": sorted(os.sched_getaffinity(0))}
    try:
        result["schedstats_enabled"] = int(Path("/proc/sys/kernel/sched_schedstats").read_text())
    except (OSError, ValueError):
        result["schedstats_enabled"] = None
    try:
        entry = next(line[3:] for line in Path("/proc/self/cgroup").read_text().splitlines()
                     if line.startswith("0::"))
        directory = (Path("/sys/fs/cgroup") / entry.lstrip("/")).resolve()
        if directory.is_relative_to("/sys/fs/cgroup"):
            result["cgroup_cpu_stat"] = {key: int(value) for key, value in
                (line.split() for line in (directory / "cpu.stat").read_text().splitlines())
                if key in ("usage_usec", "user_usec", "system_usec", "nr_periods",
                           "nr_throttled", "throttled_usec")}
            quota, period = (directory / "cpu.max").read_text().split()
            result["cgroup_cpu_quota_usec"] = None if quota == "max" else int(quota)
            result["cgroup_cpu_period_usec"] = int(period)
    except (OSError, ValueError, StopIteration):
        result["cgroup_unavailable"] = True
    return result


def supervise(argv, out, *, cwd=None, env=None, wall=60, limit=OUTPUT_LIMIT):
    """Bound combined output and wall; clean whole child group even on success.

    wait4 owns reaping so returned rusage is real, not estimated from /proc.
    No communicate() of unlimited data and no background polling service.
    """
    out = Path(out); out.mkdir(parents=True, exist_ok=False)
    start = time.monotonic_ns()
    selector = None
    status = usage = None
    reason = "exit"
    seen = kept = 0
    term_at = None
    error_type = None
    child = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                             start_new_session=True)
    try:
        selector = selectors.DefaultSelector()
        selector.register(child.stdout, selectors.EVENT_READ)
        with (out / "output.bin").open("wb") as stream:
            while True:
                now = time.monotonic()
                if status is None:
                    found, code, ru = os.wait4(child.pid, os.WNOHANG)
                    if found:
                        status, usage = code, ru
                        child.returncode = os.waitstatus_to_exitcode(code)
                if term_at is None and (status is not None or
                        time.monotonic_ns() - start >= wall * 1e9 or seen > limit):
                    if seen > limit:
                        reason = "output_limit"
                    elif status is None:
                        reason = "wall_timeout"
                    term_at = now
                    with contextlib.suppress(ProcessLookupError):
                        os.killpg(child.pid, signal.SIGTERM)
                if term_at is not None and now - term_at >= 0.2:
                    break
                for key, _ in selector.select(0.02):
                    data = os.read(key.fileobj.fileno(), 8192)
                    if not data:
                        selector.unregister(key.fileobj)
                        continue
                    seen += len(data)
                    chunk = data[:max(0, limit - kept)]
                    stream.write(chunk); kept += len(chunk)
    except BaseException as error:
        error_type = type(error).__name__
        raise
    finally:
        # Protect ALL post-spawn operations, including selector/output setup.
        # Always KILL the group even when the parent has already exited.
        cleanup_errors = []
        for sig in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.killpg(child.pid, sig)
            except ProcessLookupError:
                pass
            except BaseException as error:
                cleanup_errors.append(type(error).__name__)
            if sig == signal.SIGTERM:
                try:
                    time.sleep(max(0, 0.2 - (time.monotonic() - term_at))
                               if term_at is not None else 0.2)
                except BaseException as error:
                    cleanup_errors.append(type(error).__name__)
        try:
            deadline = time.monotonic() + 1
            while status is None:
                found, code, ru = os.wait4(child.pid, os.WNOHANG)
                if found:
                    status, usage = code, ru
                    child.returncode = os.waitstatus_to_exitcode(code)
                    break
                if time.monotonic() >= deadline:
                    raise TimeoutError("diagnostic child reap deadline")
                time.sleep(0.01)
        except BaseException as error:
            cleanup_errors.append(type(error).__name__)
        finally:
            for descriptor in (selector, child.stdout):
                if descriptor is not None:
                    try:
                        descriptor.close()
                    except BaseException as error:
                        cleanup_errors.append(type(error).__name__)
        if error_type is not None or cleanup_errors:
            # Best effort only: unavailable evidence storage must not replace
            # the original exception. No exception messages, argv or payloads.
            with contextlib.suppress(BaseException):
                dump(out / "result.json", {"reason": "internal_error",
                     "error_type": error_type or cleanup_errors[0],
                     "cleanup_errors": cleanup_errors, "pid": child.pid,
                     "child_reaped": status is not None, "exit_code": child.returncode,
                     "elapsed_ns": time.monotonic_ns() - start,
                     "output_bytes_seen": seen, "output_bytes_kept": kept,
                     "wall_seconds": wall, "output_limit": limit})
            if error_type is None:
                raise RuntimeError("diagnostic subprocess cleanup failed")
    if seen > limit:
        reason = "output_limit"
    record = {"argv": argv, "start_monotonic_ns": start,
              "elapsed_ns": time.monotonic_ns() - start, "pid": child.pid,
              "exit_code": child.returncode, "reason": reason,
              "output_bytes_seen": seen, "output_bytes_kept": kept,
              "wall_seconds": wall, "output_limit": limit,
              "rusage": {"user_seconds": usage.ru_utime, "system_seconds": usage.ru_stime,
                         "maxrss_kib_linux": usage.ru_maxrss,
                         "voluntary_switches": usage.ru_nvcsw,
                         "involuntary_switches": usage.ru_nivcsw}}
    # wait4 includes accounted-for reaped descendants, not all un-reaped orphans.
    dump(out / "result.json", record)
    return record


class Events:
    def __init__(self, path, limit=EVENT_LIMIT):
        self.stream = Path(path).open("xb")
        self.lock = threading.Lock(); self.used = 0; self.limit = limit
        self.dropped = 0; self.processes = []; self.fixtures = []
        self.instrument_errors = []; self.cleanup_failed = False
        self.response_write_errors = 0

    def emit(self, event, **fields):
        data = (json.dumps({"t_ns": time.monotonic_ns(), "thread_id": threading.get_ident(),
                            "event": event, **fields},
                           separators=(",", ":")) + "\n").encode()
        with self.lock:
            if self.used + len(data) > self.limit:
                self.dropped += 1
                return
            self.stream.write(data); self.stream.flush(); self.used += len(data)

    def sample(self):
        for process in list(self.processes):
            self.emit("proxy_resource", **proc_sample(process.pid))

    def check(self):
        # This is registered on the OUTER real TestCase, not harness.doCleanups.
        errors = sum(len(fixture.errors) for fixture in self.fixtures)
        self.emit("fixture_cleanup_result", fixture_errors=errors,
                  instrumentation_errors=len(self.instrument_errors), dropped=self.dropped,
                  response_write_errors=self.response_write_errors)
        if ((errors and not self.cleanup_failed) or self.instrument_errors or self.dropped
                or self.response_write_errors):
            raise AssertionError("diagnostic fixture/instrumentation/evidence failure: "
                                 f"{errors}/{len(self.instrument_errors)}/{self.dropped}/"
                                 f"{self.response_write_errors}")


def inner(root, out):
    root = Path(root).resolve(); out = Path(out); out.mkdir(parents=True, exist_ok=False)
    for name, expected in SOURCE_HASHES.items():
        if digest(root / name) != expected:
            raise RuntimeError("original source hash mismatch: " + name)
    sys.path.insert(0, str(root / "tests"))
    original = importlib.import_module("proxy_incremental")
    support = original.support
    events = Events(out / "events.jsonl")
    stop = threading.Event()
    real_popen = support.subprocess.Popen
    real_fixture_init = support.Fixture.__init__
    real_request = support.Proxy.request
    real_close = support.Proxy.close
    real_cleanups = support.StateRegressions.doCleanups
    real_server = support.http.server.ThreadingHTTPServer
    real_connection = support.http.client.HTTPConnection

    class ObservedConnection(real_connection):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, **kwargs)
            events.emit("client_connection_constructed", timeout_seconds=self.timeout)

        def request(self, *args, **kwargs):
            result = super().request(*args, **kwargs)
            events.emit("client_request_bytes_sent")
            return result

        def getresponse(self):
            events.emit("client_getresponse_begin")
            try:
                result = super().getresponse()
                events.emit("client_getresponse_return", status=result.status)
                return result
            except BaseException as error:
                events.emit("client_getresponse_error", error_type=type(error).__name__)
                raise

    def popen(*args, **kwargs):
        events.emit("proxy_spawn_begin")
        child = real_popen(*args, **kwargs)
        events.processes.append(child)
        events.emit("proxy_spawned", pid=child.pid)
        return child

    def fixture_init(fixture, primary, review):
        events.fixtures.append(fixture)
        def timed(callback, kind):
            def invoke(body, number):
                events.emit(kind + "_callback_begin", number=number)
                try:
                    value = callback(body, number)
                    events.emit(kind + "_callback_return", number=number)
                    return value
                except BaseException as error:
                    events.emit(kind + "_callback_error", number=number,
                                error_type=type(error).__name__)
                    raise
            return invoke
        return real_fixture_init(fixture, timed(primary, "primary"), timed(review, "reviewer"))

    class ObservedServer(real_server):
        def finish_request(self, request, address):
            handler = self.RequestHandlerClass
            class ObservedHandler(handler):
                def do_POST(self):
                    kind = "reviewer" if self.path == "/review" else "primary"
                    events.emit(kind + "_request_handler_enter")
                    writer = self.wfile
                    class Writer:
                        def write(self, data):
                            try:
                                result = writer.write(data)
                                events.emit(kind + "_response_write_complete", bytes=len(data))
                                return result
                            except BaseException as error:
                                with events.lock:
                                    events.response_write_errors += 1
                                events.emit(kind + "_response_write_error", error_type=type(error).__name__)
                                raise
                        def __getattr__(self, name):
                            return getattr(writer, name)
                    self.wfile = Writer()
                    try:
                        return super().do_POST()
                    finally:
                        self.wfile = writer
                        events.emit(kind + "_request_handler_exit")
            ObservedHandler(request, address, self)

    def request(proxy, method, path, body=None):
        events.emit("client_request_begin", method=method)
        events.sample()
        try:
            result = real_request(proxy, method, path, body)
            events.emit("client_request_return", method=method, status=result[0])
            return result
        except BaseException as error:
            events.emit("client_request_error", method=method, error_type=type(error).__name__)
            raise
        finally:
            events.sample()

    def close(proxy):
        try:
            return real_close(proxy)
        finally:
            usage = resource.getrusage(resource.RUSAGE_CHILDREN)
            events.emit("inner_reaped_children_rusage", user_seconds=usage.ru_utime,
                        system_seconds=usage.ru_stime, maxrss_kib_linux=usage.ru_maxrss,
                        voluntary_switches=usage.ru_nvcsw, involuntary_switches=usage.ru_nivcsw)

    def strict_cleanups(harness):
        success = real_cleanups(harness)
        events.emit("nested_harness_cleanup_result", success=success)
        if not success:
            events.cleanup_failed = True
            raise AssertionError("diagnostic fixture/instrumentation/evidence failure: nested harness cleanup")
        return success

    class DiagnosticOriginal(original.IncrementalReviewer):
        def setUp(self):
            # LIFO: original harness.doCleanups runs first. Its standalone outcome
            # may swallow assertions; explicitly propagate fixture errors OUTWARD.
            self.addCleanup(events.check)
            super().setUp()

    def sample_loop():
        while not stop.wait(0.05):
            try:
                events.sample()
            except Exception as error:
                events.instrument_errors.append(type(error).__name__)
                return
    sampler = threading.Thread(target=sample_loop, daemon=True)
    events.emit("original_case_begin", socket_timeout_seconds=5,
                clock_ticks=os.sysconf("SC_CLK_TCK"), page_bytes=os.sysconf("SC_PAGE_SIZE"))
    events.emit("cpu_environment", **cpu_environment())
    sampler.start()
    try:
        with patch.object(support.subprocess, "Popen", popen), \
             patch.object(support.Fixture, "__init__", fixture_init), \
             patch.object(support.Proxy, "request", request), \
             patch.object(support.Proxy, "close", close), \
             patch.object(support.StateRegressions, "doCleanups", strict_cleanups), \
             patch.object(support.http.client, "HTTPConnection", ObservedConnection), \
             patch.object(support.http.server, "ThreadingHTTPServer", ObservedServer):
            result = unittest.TextTestRunner(verbosity=2).run(DiagnosticOriginal(CASE))
        events.emit("original_case_end", tests=result.testsRun, failures=len(result.failures),
                    errors=len(result.errors), skipped=len(result.skipped))
        events.emit("cpu_environment", **cpu_environment())
        success = (result.testsRun == 1 and result.wasSuccessful() and not result.skipped
                   and not events.response_write_errors)
        dump(out / "case-result.json", {"tests": result.testsRun, "success": success,
             "failures": len(result.failures), "errors": len(result.errors),
             "skips": len(result.skipped), "dropped_events": events.dropped,
             "response_write_errors": events.response_write_errors})
        return 0 if success and not events.dropped else 1
    finally:
        stop.set(); sampler.join(timeout=1); events.stream.close()


def make_shim(directory, root, trace):
    directory = Path(directory); directory.mkdir()
    script = Path(__file__).resolve()
    expected = ["-B", str(Path(root).resolve() / "tests/proxy_incremental.py"), "-v"]
    args = ["-B", str(script), "inner", "--root", str(Path(root).resolve()), "--out", str(trace)]
    shim = directory / "python3"
    shim.write_text(f"#!{sys.executable}\nimport os,sys\n"
                    f"args = {args!r} if sys.argv[1:] == {expected!r} else sys.argv[1:]\n"
                    f"os.execv({sys.executable!r}, [{sys.executable!r}, *args])\n")
    shim.chmod(0o700)
    return shim


def hosted(root, cargo_log, out):
    root = Path(root).resolve(); out = Path(out).resolve(); out.mkdir(exist_ok=False)
    if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        raise RuntimeError("hosted mode requires authorized GitHub-hosted runner")
    for name, expected in SOURCE_HASHES.items():
        if digest(root / name) != expected:
            raise RuntimeError("original source hash mismatch: " + name)
    artifacts = []
    with Path(cargo_log).open() as stream:
        for line in stream:
            try:
                obj = json.loads(line)
            except ValueError:
                continue
            if obj.get("reason") == "compiler-artifact" and obj.get("executable"):
                artifacts.append(obj)
    def select(name, kind, test):
        matches = {str(Path(a["executable"]).resolve()) for a in artifacts
                   if a["target"]["name"] == name and kind in a["target"]["kind"]
                   and a["profile"]["test"] == test and a["profile"]["opt_level"] == "0"}
        if len(matches) != 1:
            raise RuntimeError("expected one emitted debug executable: " + name)
        binary = Path(matches.pop())
        if not binary.is_relative_to(root / "target/debug") or not binary.is_file():
            raise RuntimeError("executable outside same-checkout target/debug")
        return str(binary)
    integration = select("proxy", "test", True)
    carry = select("carry", "bin", False)
    inventory = supervise([integration, "--list", "--format", "terse"], out / "inventory", cwd=root, wall=10)
    lines = (out / "inventory/output.bin").read_text().splitlines()
    names = [line[:-6] for line in lines if line.endswith(": test")]
    if inventory["exit_code"] != 0 or inventory["reason"] != "exit" or len(names) != 50 or len(set(names)) != 50 or "incremental_reviewer_context_regressions" not in names:
        raise RuntimeError("integration inventory must contain exact original 50 cases")
    manifest = {"source_base": BASE, "source_hashes": SOURCE_HASHES,
                "integration": integration, "integration_sha256": digest(integration),
                "carry": carry, "carry_sha256": digest(carry), "cases": names,
                "cpu_count": os.cpu_count(), "affinity": sorted(os.sched_getaffinity(0)),
                "rust_test_threads": os.environ.get("RUST_TEST_THREADS"),
                "cargo_log_sha256": digest(cargo_log),
                "github_sha": os.environ.get("GITHUB_SHA"),
                "github_run_id": os.environ.get("GITHUB_RUN_ID"),
                "github_run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT"),
                "helper_sha256": digest(__file__)}
    dump(out / "manifest.json", manifest)
    env = dict(os.environ, CARRY_TEST_BINARY=carry)
    serial = supervise([sys.executable, "-B", str(Path(__file__).resolve()), "inner",
                        "--root", str(root), "--out", str(out / "serial-trace")],
                       out / "serial", cwd=root, env=env, wall=30)
    make_shim(out / "shim", root, out / "concurrent-trace")
    env["PATH"] = str(out / "shim") + os.pathsep + env.get("PATH", "/usr/bin:/bin")
    # All original 50 Rust selectors, libtest's actual default concurrency and
    # original alphabetical scheduling. Only the incremental Python launcher is
    # temporarily narrowed to the unchanged failing single case for this arm.
    concurrent = supervise([integration], out / "concurrent", cwd=root, env=env, wall=180)
    arms = {"serial": serial, "concurrent": concurrent}
    trace_ok = {}
    for name in arms:
        path = out / (name + "-trace") / "case-result.json"
        trace_ok[name] = path.is_file() and json.loads(path.read_text())["success"]
    ok = all(a["reason"] == "exit" and a["exit_code"] == 0 for a in arms.values()) and all(trace_ok.values())
    dump(out / "outcome.json", {"success": ok, "arms": arms, "trace_success": trace_ok,
                               "is_regular_ci_green": False})
    return 0 if ok else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["inner", "hosted"])
    parser.add_argument("--root", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--cargo-log")
    args = parser.parse_args()
    if not SOURCE_HASHES:
        raise RuntimeError("source hashes must be frozen before use")
    if args.mode == "inner":
        return inner(args.root, args.out)
    if not args.cargo_log:
        parser.error("hosted requires --cargo-log")
    return hosted(args.root, args.cargo_log, args.out)


if __name__ == "__main__":
    raise SystemExit(main())
