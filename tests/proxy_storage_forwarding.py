"""Real CLI/HTTP storage degradation; synthetic loopback only, no model calls."""
import base64
import contextlib
import io
import http.client
import json
import os
from pathlib import Path
import subprocess
import threading
import time
import sqlite3
import tempfile
import unittest

from proxy_state_regressions import Fixture, Proxy, completed, wire


class RecordingFixture(Fixture):
    def __init__(self, *args):
        self.wires = []
        self.split_response = None
        super().__init__(*args)
        fixture = self
        parent = self.server.RequestHandlerClass
        class Handler(parent):
            def do_POST(self):
                raw = self.rfile.read(int(self.headers["Content-Length"]))
                fixture.wires.append((self.path, dict(self.headers), raw))
                self.rfile = io.BytesIO(raw)
                target = self.wfile
                class Writer:
                    def write(self, data):
                        split = fixture.split_response
                        if split and data == split[0]:
                            target.write(data[:split[1]])
                            target.flush()
                            split[2]()
                            target.write(data[split[1]:])
                            return len(data)
                        return target.write(data)
                    def __getattr__(self, name):
                        return getattr(target, name)
                self.wfile = Writer()
                try:
                    super().do_POST()
                finally:
                    self.wfile = target
        self.server.RequestHandlerClass = Handler


class StorageForwarding(unittest.TestCase):
    @contextlib.contextmanager
    def directory(self):
        # Close child writers before removing their state, even on assertion failure.
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        yield temporary.name

    def authority_rows(self, root):
        with contextlib.closing(sqlite3.connect(root / "history.index")) as db:
            return db.execute("SELECT kind,key,data,at,owner FROM records ORDER BY kind,key").fetchall()

    def setup_proxy(self, root, *, extra=(), environment=None, primary=None):
        self.receipt = completed(response_id="resp_storage_fixture")
        fixture = RecordingFixture(primary or (lambda body, n: (200, self.receipt)),
                          lambda body, n: self.fail("degraded requests must not review"))
        self.addCleanup(fixture.close)
        proxy = Proxy(root, fixture, extra=("--review-min-primary-tokens", "2000000",
                      "--review-min-new-tokens", "2000000", *extra),
                      environment=environment, wait_reviews=False)
        self.addCleanup(proxy.close)
        return proxy, fixture

    def request(self, proxy, body, headers=None, raw=None):
        conn = http.client.HTTPConnection(*proxy.address, timeout=15)
        try:
            outgoing = {"Content-Type": "application/json", "x-carry-session": "storage-fixture", **(headers or {})}
            conn.request("POST", "/v1/responses", body=raw or wire(body),
                         headers={k: v for k, v in outgoing.items() if v is not None})
            response = conn.getresponse()
            return response.status, dict(response.getheaders()), response.read()
        finally:
            conn.close()

    def body(self, stream=False):
        return {"model": "gpt-6-luna", "input": [{"role": "user", "content": "synthetic storage test"}],
                "stream": stream, "store": False, "temperature": 0.25,
                "metadata": {"fixture": "transparent"}, "max_output_tokens": 32}

    def test_admission_refusal_forwards_original_once_without_mutating_history(self):
        with self.directory() as directory:
            root = Path(directory)
            proxy, fixture = self.setup_proxy(root)
            body = self.body()
            status, headers, raw = self.request(proxy, body)
            self.assertEqual(status, 200)
            self.assertNotIn("x-carry-durability", headers)
            self.assertEqual(json.loads(raw), self.receipt)
            saved = proxy.saved()
            self.assertEqual(saved["primary"]["calls"], 1)
            self.assertEqual(saved["primary"]["input_tokens"], 100)
            proxy.close()
            # Same authority, unchanged small-record policy, cap too small to admit
            # another promise. This is real configured-quota refusal, not host fill.
            durable = {p.name: p.read_bytes() for p in root.glob("*.json")}
            authority = self.authority_rows(root)
            proxy, fixture = self.setup_proxy(root, extra=("--history-disk-bytes", "65536"))
            status, headers, raw = self.request(proxy, body)
            self.assertEqual(status, 200, raw)
            self.assertEqual(json.loads(raw), self.receipt)
            self.assertEqual(headers.get("x-carry-durability"), "unavailable")
            self.assertEqual({p.name: p.read_bytes() for p in root.glob("*.json")}, durable)
            self.assertEqual(self.authority_rows(root), authority, "preserve every historic record and paid observation")
            self.assertFalse(saved["primary_pending"])
            self.assertEqual(fixture.primaries, [body])

    def test_actual_write_limits_remain_bounded_without_future_receipt_reservations(self):
        for implicit in (False, True):
            for cap, durable in ((393216, False), (650000, True)):
                with self.subTest(implicit=implicit, cap=cap), self.directory() as directory:
                    proxy, fixture = self.setup_proxy(Path(directory), extra=(
                        "--history-max-bytes", "16384", "--history-disk-bytes", str(cap)))
                    body = self.body()
                    status, headers, raw = self.request(proxy, body,
                        {"x-carry-session": None} if implicit else {})
                    self.assertEqual(status, 200, raw)
                    self.assertEqual(fixture.primaries, [body])
                    self.assertEqual(fixture.reviews, [])
                    if durable:
                        self.assertNotIn("x-carry-durability", headers)
                        self.assertEqual(proxy.saved()["primary"]["calls"], 1,
                            "future receipt headroom must not prevent bounded actual writes")
                    else:
                        self.assertEqual(headers.get("x-carry-durability"), "unavailable")
                        self.assertEqual(list(Path(directory).glob("*.json")), [])

    def test_second_live_owner_persists_optimistically_without_future_receipt_reservations(self):
        with self.directory() as directory:
            root = Path(directory)
            held, release = threading.Event(), threading.Event()
            self.addCleanup(release.set)
            def primary(body, n):
                if n == 1:
                    held.set()
                    self.assertTrue(release.wait(10))
                return 200, completed(response_id=f"resp_concurrent_{n}")
            proxy, fixture = self.setup_proxy(root, primary=primary, extra=(
                "--history-max-bytes", "65536", "--history-disk-bytes", "1572864"))
            first, second = self.body(), {**self.body(), "metadata": {"fixture": "second owner"}}
            outcome = {}
            def send_first():
                try:
                    outcome["first"] = self.request(proxy, first, {"x-carry-session": "held-owner"})
                except Exception as error:
                    outcome["error"] = repr(error)
            worker = threading.Thread(target=send_first, daemon=True)
            worker.start()
            self.assertTrue(held.wait(5), "the first owner must genuinely dispatch before the second arrives")
            authority = self.authority_rows(root)
            try:
                status, headers, raw = self.request(proxy, second, {"x-carry-session": "non-durable-owner"})
                self.assertEqual(status, 200, raw)
                self.assertNotIn("x-carry-durability", headers)
                self.assertEqual(json.loads(raw), completed(response_id="resp_concurrent_2"))
                owners = {row[4] for row in authority if row[0] == "snapshots"}
                self.assertEqual([r for r in self.authority_rows(root) if r[4] in owners],
                                 [r for r in authority if r[4] in owners], "concurrent writes preserve the held owner's authority")
            finally:
                release.set()
                worker.join(timeout=15)
            self.assertFalse(worker.is_alive())
            self.assertNotIn("error", outcome)
            self.assertEqual(outcome["first"][0], 200)
            self.assertNotIn("x-carry-durability", outcome["first"][1])
            self.assertEqual(json.loads(outcome["first"][2]), completed(response_id="resp_concurrent_1"))
            self.assertEqual(fixture.primaries, [first, second])
            self.assertEqual(fixture.reviews, [])
            saved = [json.loads(p.read_text()) for p in root.glob("*.json")]
            self.assertEqual(len(saved), 2)
            self.assertTrue(all(s["primary"]["calls"] == 1 and s["primary"]["input_tokens"] == 100
                                and not s["primary_pending"] for s in saved))
            self.assertEqual({s["last_response_id"] for s in saved}, {"resp_concurrent_1", "resp_concurrent_2"})

    def test_unreadable_existing_sqlite_authority_forwards_self_contained_input_once(self):
        with self.directory() as directory:
            state = Path(directory)
            proxy, fixture = self.setup_proxy(state)
            self.assertEqual(self.request(proxy, self.body())[0], 200)
            before = self.authority_rows(state)
            authority = state / "history.index"
            mode = authority.stat().st_mode & 0o777
            authority.chmod(0)
            try:
                try:
                    authority.open("rb").close()
                except PermissionError:
                    pass
                else:
                    self.fail("permission fault requires an unprivileged process; do not skip its qualification")
                body = {**self.body(), "input": [{"role": "user", "content": "self-contained permission-fault request"}]}
                status, headers, raw = self.request(proxy, body)
                self.assertEqual(status, 200, raw)
                self.assertEqual(headers.get("x-carry-durability"), "unavailable")
                self.assertEqual(fixture.primaries, [self.body(), body])
                self.assertEqual(fixture.reviews, [])
            finally:
                authority.chmod(mode)
            self.assertEqual(self.authority_rows(state), before,
                "failed reads must not replace valid prior authority with fresh history")

    def test_corrupt_authority_is_not_mistaken_for_storage_pressure(self):
        with self.directory() as directory:
            root = Path(directory)
            proxy, fixture = self.setup_proxy(root)
            corrupt = b"not a sqlite history authority"
            (root / "history.index").write_bytes(corrupt)
            status, _, _ = self.request(proxy, self.body())
            self.assertEqual(status, 409)
            self.assertEqual(fixture.primaries, [])
            self.assertEqual((root / "history.index").read_bytes(), corrupt)

    def test_corrupt_accounting_records_reject_before_primary_or_review_dispatch(self):
        # These are malformed records in a valid SQLite authority, not an I/O
        # outage. Preparation must not turn their validation error into fallback.
        for kind in (None, "accounted", "metadata"):
            with self.subTest(kind=kind), self.directory() as directory, contextlib.ExitStack() as stack:
                root = Path(directory)
                proxy, fixture = self.setup_proxy(root)
                stack.callback(fixture.close)
                stack.callback(proxy.close)
                body = self.body()
                self.assertEqual(self.request(proxy, body)[0], 200)
                self.assertEqual(proxy.saved()["primary"]["calls"], 1)
                proxy.close()  # Join every legitimate publisher before injection.
                if kind is not None:
                    with contextlib.closing(sqlite3.connect(root / "history.index")) as db:
                        with db:
                            changed = db.execute(
                                "UPDATE records SET data=? WHERE kind=?" +
                                (" AND key='accounting_clock'" if kind == "metadata" else ""),
                                (b"{}", kind)).rowcount
                        self.assertGreater(changed, 0, "must corrupt existing accounting authority")
                        self.assertEqual(db.execute("PRAGMA integrity_check").fetchone()[0], "ok")
                before = self.authority_rows(root)
                proxy, fresh = self.setup_proxy(root)
                stack.callback(fresh.close)
                stack.callback(proxy.close)
                status, headers, raw = self.request(proxy, body)
                # Check dispatch and retained bytes even when status is wrong.
                rejected = {
                    "status": status,
                    "durability": headers.get("x-carry-durability"),
                    "primaries": fresh.primaries,
                    "reviews": fresh.reviews,
                    "authority_unchanged": self.authority_rows(root) == before,
                    "fixture_errors": fresh.errors,
                }
                if kind is None:
                    self.assertEqual(status, 200, raw)
                    self.assertNotIn("x-carry-durability", headers)
                    self.assertEqual(fresh.primaries, [body])
                    self.assertEqual(proxy.saved()["primary"]["calls"], 2)
                else:
                    self.assertEqual(rejected, {
                        "status": 409, "durability": None, "primaries": [],
                        "reviews": [], "authority_unchanged": True, "fixture_errors": [],
                    })
                self.assertFalse(fresh.errors, fresh.errors)
                self.assertEqual(fresh.reviews, [])

    def test_unsupported_model_keeps_forwarding_semantics_under_pressure(self):
        with self.directory() as directory:
            proxy, fixture = self.setup_proxy(Path(directory), extra=("--history-disk-bytes", "65536"))
            body = {"model": "synthetic-unknown-model", "input": "valid standalone provider input", "store": False}
            status, _, raw = self.request(proxy, body)
            self.assertEqual(status, 200, raw)
            self.assertEqual(fixture.primaries, [body])

    def test_provider_owned_references_remain_forwardable_when_tracking_fails(self):
        # Off and unsupported-model forwarding never require Carry ancestry.
        # The ordinary provider owns these references, including under pressure.
        for mode, model in (("off", "gpt-6-luna"), ("audit", "synthetic-unknown-model")):
            for field in ("previous_response_id", "conversation"):
                for pressured in (False, True):
                    with self.subTest(mode=mode, field=field, pressured=pressured), self.directory() as directory, contextlib.ExitStack() as stack:
                        fixture = RecordingFixture(lambda body, n: (200, completed(response_id="provider-resolved")),
                                                   lambda body, n: self.fail("forwarding must not review"))
                        stack.callback(fixture.close)
                        options = ("--history-disk-bytes", "65536") if pressured else ()
                        proxy = Proxy(Path(directory), fixture, extra=options, mode=mode, wait_reviews=False)
                        stack.callback(proxy.close)
                        body = {**self.body(), "model": model, field: "provider-owned-reference"}
                        status, headers, raw = self.request(proxy, body)
                        self.assertEqual(status, 200, raw)
                        self.assertEqual(json.loads(raw), completed(response_id="provider-resolved"))
                        self.assertEqual(fixture.primaries, [body])
                        self.assertEqual(fixture.reviews, [])
                        self.assertEqual(headers.get("x-carry-durability"), "unavailable" if pressured else None)
                        proxy.close()

    def test_literal_wire_auth_privacy_and_upstream_error_under_quota(self):
        with self.directory() as directory:
            root = Path(directory)
            proxy, fixture = self.setup_proxy(root, extra=("--history-disk-bytes", "65536"),
                environment={"CARRY_PROXY_AUTH_TOKEN": "synthetic-local-token"},
                primary=lambda body, n: (429, {"error": {"message": "synthetic provider refusal"}}))
            body = self.body()
            raw = json.dumps(body, indent=2).encode() + b"\n"
            status, headers, response = self.request(proxy, body, {
                "Authorization": "Bearer synthetic-local-token", "Cookie": "private-fixture-cookie",
                "x-carry-tenant": "private-fixture-tenant", "x-request-id": "synthetic-public-id"}, raw)
            self.assertEqual(status, 429)
            self.assertEqual(json.loads(response), {"error": {"message": "synthetic provider refusal"}})
            self.assertEqual(headers.get("x-carry-durability"), "unavailable")
            self.assertEqual(fixture.primaries, [body])
            path, sent, actual = fixture.wires[0]
            self.assertEqual(path, "/v1/responses")
            self.assertEqual(actual, raw)
            self.assertEqual(sent["authorization"], "Bearer fixture-primary")
            self.assertEqual(sent["x-request-id"], "synthetic-public-id")
            self.assertFalse({name.lower() for name in sent} & {"cookie", "x-carry-session", "x-carry-tenant"})
            proxy.close()
            for path in root.rglob("*.jsonl"):
                self.assertNotIn("synthetic-local-token", path.read_text())
                self.assertNotIn("private-fixture-cookie", path.read_text())

    def test_pressure_preserves_security_validation_and_missing_ancestry(self):
        with self.directory() as directory:
            proxy, fixture = self.setup_proxy(Path(directory), extra=("--history-disk-bytes", "65536"),
                environment={"CARRY_PROXY_AUTH_TOKEN": "synthetic-local-token"})
            body = self.body()
            for headers, expected in [({}, 401), ({"Authorization": "Bearer wrong"}, 401),
                ({"Authorization": "Bearer synthetic-local-token", "Origin": "null"}, 403),
                ({"Authorization": "Bearer synthetic-local-token", "x-carry-history-policy": "invalid"}, 400)]:
                self.assertEqual(self.request(proxy, body, headers)[0], expected)
            auth = {"Authorization": "Bearer synthetic-local-token"}
            self.assertEqual(self.request(proxy, body, auth, raw=b"not json")[0], 400)
            for field in ("previous_response_id", "conversation"):
                dependent = {**body, field: "missing-local-ancestor"}
                self.assertNotEqual(self.request(proxy, dependent, auth)[0], 200)
            self.assertEqual(fixture.primaries, [])
            self.assertEqual(self.request(proxy, body, auth)[0], 200)
            self.assertEqual(fixture.primaries, [body])

    def fault_library(self, root):
        # Inject ENOSPC at the OS write boundary, not a mocked Carry/storage API.
        # Only the disposable child and its exact private state directory qualify.
        source = root / "enospc.c"
        library = root / "enospc.so"
        source.write_text(r'''
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <sys/types.h>
#include <sys/syscall.h>
#include <fcntl.h>
static int refused(int fd) {
    const char *root = getenv("CARRY_FIXTURE_STORAGE_ROOT");
    const char *marker = getenv("CARRY_FIXTURE_STORAGE_FAULT");
    char link[64], path[4096];
    if (!root || !marker || access(marker, F_OK)) return 0;
    snprintf(link, sizeof link, "/proc/self/fd/%d", fd);
    ssize_t n = readlink(link, path, sizeof(path)-1);
    if (n < 0) return 0;
    path[n] = 0;
    if (!strncmp(path, root, strlen(root)) && path[strlen(root)] == '/') {
        int log = strstr(path + strlen(root), "/diagnostics/") != NULL;
        const char *allow_logs = getenv("CARRY_FIXTURE_ALLOW_LOGS");
        if (log && allow_logs && !strcmp(allow_logs, "1")) return 0;
        int witness = open(marker, O_WRONLY | O_APPEND);
        if (witness >= 0) { syscall(SYS_write, witness, log ? "L" : "E", 1); close(witness); }
        errno = ENOSPC; return 1;
    }
    return 0;
}
ssize_t write(int fd, const void *buf, size_t n) {
    if (refused(fd)) return -1;
    return ((ssize_t (*)(int,const void*,size_t))dlsym(RTLD_NEXT,"write"))(fd,buf,n);
}
ssize_t pwrite(int fd, const void *buf, size_t n, off_t at) {
    if (refused(fd)) return -1;
    return ((ssize_t (*)(int,const void*,size_t,off_t))dlsym(RTLD_NEXT,"pwrite"))(fd,buf,n,at);
}
ssize_t pwrite64(int fd, const void *buf, size_t n, off64_t at) {
    if (refused(fd)) return -1;
    return ((ssize_t (*)(int,const void*,size_t,off64_t))dlsym(RTLD_NEXT,"pwrite64"))(fd,buf,n,at);
}
''')
        subprocess.run([os.environ.get("CC", "cc"), "-shared", "-fPIC", "-O0", str(source),
                        "-o", str(library), "-ldl"], check=True, capture_output=True, timeout=30)
        return library

    def test_enospc_after_client_receives_first_sse_event_preserves_the_entire_stream(self):
        with self.directory() as directory:
            root = Path(directory)
            state = root / "state"
            state.mkdir()
            marker = root / "stream-fault"
            lib = self.fault_library(root)
            resume = threading.Event()
            self.addCleanup(resume.set)
            prefix = b'event: response.output_text.delta\ndata: {"type":"response.output_text.delta","delta":"fixture delta"}\n\n'
            receipt = completed(response_id="resp_stream_fault")
            suffix = b"event: response.completed\ndata: " + wire({"type": "response.completed", "response": receipt}) + b"\n\n"
            payload = prefix + suffix
            def resume_with_storage_failure():
                self.assertTrue(resume.wait(8), "client must receive the first event before fault injection")
                marker.touch()
            proxy, fixture = self.setup_proxy(state, primary=lambda body, n: (200, payload), environment={
                "LD_PRELOAD": str(lib), "CARRY_FIXTURE_STORAGE_ROOT": str(state),
                "CARRY_FIXTURE_STORAGE_FAULT": str(marker)})
            fixture.split_response = (payload, len(prefix), resume_with_storage_failure)
            body = self.body(True)
            conn = http.client.HTTPConnection(*proxy.address, timeout=12)
            try:
                conn.request("POST", "/v1/responses", wire(body), {
                    "Content-Type": "application/json", "x-carry-session": "storage-fixture"})
                response = conn.getresponse()
                self.assertEqual(response.status, 200)
                first = response.read(len(prefix))
                self.assertEqual(first, prefix)
                self.assertFalse(marker.exists(), "fault must be later than real client delivery")
                resume.set()
                self.assertEqual(first + response.read(), payload)
            finally:
                resume.set()
                conn.close()
            self.assertGreater(marker.stat().st_size, 0, "storage write must actually encounter ENOSPC")
            self.assertEqual(fixture.primaries, [body])
            self.assertEqual(fixture.reviews, [])
            pending = proxy.saved()
            self.assertTrue(pending["primary_pending"])
            self.assertEqual(pending["primary"]["calls"], 0)
            # The zero above is the old pending snapshot, NOT an invented receipt.
            before = self.authority_rows(state)
            proxy.close()
            marker.unlink()
            restarted = Proxy(state, fixture, extra=("--review-min-primary-tokens", "2000000",
                "--review-min-new-tokens", "2000000"), wait_reviews=False)
            self.addCleanup(restarted.close)
            self.assertNotEqual(self.request(restarted, {**body, "stream": False,
                "previous_response_id": "resp_stream_fault"})[0], 200)
            self.assertEqual(fixture.primaries, [body])
            self.assertEqual(self.authority_rows(state), before)

    def test_storage_warning_is_private_and_failed_log_sink_does_not_gate_delivery(self):
        with self.directory() as directory:
            root = Path(directory)
            lib = self.fault_library(root)
            for fail_logs in (False, True):
                with self.subTest(fail_logs=fail_logs):
                    state = root / f"state-logs-{fail_logs}"
                    state.mkdir()
                    marker = root / f"fault-logs-{fail_logs}"
                    proxy, fixture = self.setup_proxy(state, environment={
                        "LD_PRELOAD": str(lib), "CARRY_FIXTURE_STORAGE_ROOT": str(state),
                        "CARRY_FIXTURE_STORAGE_FAULT": str(marker),
                        "CARRY_FIXTURE_ALLOW_LOGS": "0" if fail_logs else "1"})
                    marker.touch()
                    body = self.body()
                    body["input"][0]["content"] = "private-fixture-prompt-never-log"
                    status, _, raw = self.request(proxy, body, {"Cookie": "private-fixture-cookie"})
                    self.assertEqual(status, 200, raw)
                    self.assertEqual(fixture.primaries, [body])
                    self.assertEqual(fixture.reviews, [])
                    deadline = time.monotonic() + 3
                    warnings = []
                    while time.monotonic() < deadline:
                        logs = "\n".join(p.read_text() for p in state.rglob("*.jsonl"))
                        warnings = []
                        for line in logs.splitlines():
                            try:
                                record = json.loads(line)
                            except ValueError:
                                continue  # A concurrent best-effort append may be partial.
                            if record.get("event") == "storage_unavailable":
                                warnings.append(record)
                        if warnings or (fail_logs and b"L" in marker.read_bytes()):
                            break
                        time.sleep(.01)
                    self.assertNotIn("private-fixture-prompt-never-log", logs)
                    self.assertNotIn("private-fixture-cookie", logs)
                    if fail_logs:
                        self.assertIn(b"L", marker.read_bytes(), "actual log write must hit ENOSPC")
                    else:
                        self.assertEqual(len(warnings), 1)
                        self.assertEqual(warnings[0]["data"], {"stage": "preparation", "recorded": False})
                    proxy.close()

    def test_subscription_adapter_keeps_converted_json_on_storage_failure(self):
        with self.directory() as directory:
            root = Path(directory)
            lib = self.fault_library(root)
            for phase in ("preparation", "settlement"):
                with self.subTest(phase=phase):
                    state = root / ("state-" + phase)
                    state.mkdir()
                    auth = root / ("auth-" + phase)
                    auth.mkdir()
                    payload = base64.urlsafe_b64encode(wire({"https://api.openai.com/auth": {
                        "chatgpt_account_id": "fixture-account"}})).decode().rstrip("=")
                    token = f"header.{payload}.signature"
                    (auth / "auth.json").write_bytes(wire({"version": 1, "access_token": token,
                        "refresh_token": "fixture-refresh", "expires_at_ms": 4102444800000}))
                    (auth / "auth.json").chmod(0o600)
                    marker = root / ("fault-subscription-" + phase)
                    receipt = completed(response_id="resp_subscription_fixture")
                    def primary(body, n):
                        if phase == "settlement":
                            marker.touch()
                        return 200, b"data: " + wire({"type": "response.completed", "response": receipt}) + b"\n\n"
                    proxy, fixture = self.setup_proxy(state, primary=primary,
                        extra=("--codex-login", "--codex-home", str(auth)), environment={
                            "LD_PRELOAD": str(lib), "CARRY_FIXTURE_STORAGE_ROOT": str(state),
                            "CARRY_FIXTURE_STORAGE_FAULT": str(marker)})
                    if phase == "preparation":
                        marker.touch()
                    body = {"model": "gpt-6.1-sol", "input": [{"role": "user", "content": "synthetic subscription"}],
                            "stream": False, "store": False, "instructions": "fixture direction",
                            "prompt_cache_key": "fixture-fixed-cache", "max_output_tokens": 32}
                    status, headers, raw = self.request(proxy, body)
                    self.assertEqual(status, 200, raw)
                    self.assertEqual(json.loads(raw), receipt)
                    self.assertEqual(fixture.primaries, [{k: v for k, v in {**body, "stream": True}.items() if k != "max_output_tokens"}])
                    self.assertEqual(fixture.wires[0][1]["authorization"], "Bearer " + token)
                    self.assertEqual(fixture.reviews, [])
                    self.assertGreater(marker.stat().st_size, 0)
                    if phase == "preparation":
                        self.assertEqual(headers.get("x-carry-durability"), "unavailable")
                    proxy.close()

    def test_enospc_before_dispatch_and_after_receipt_preserves_json_and_sse(self):
        with self.directory() as directory:
            root = Path(directory)
            lib = self.fault_library(root)
            for phase in ("preparation", "settlement"):
                for stream in (False, True):
                    with self.subTest(phase=phase, stream=stream):
                        state = root / f"{phase}-{stream}"
                        state.mkdir()
                        marker = root / f"fault-{phase}-{stream}"
                        payload = completed(response_id=f"resp_{phase}_{stream}")
                        output = (b"event: response.completed\ndata: " + wire({"type": "response.completed", "response": payload}) + b"\n\n") if stream else wire(payload)
                        def primary(body, n):
                            if phase == "settlement":
                                marker.touch()
                            return 200, output if stream else payload
                        proxy, fixture = self.setup_proxy(state, primary=primary, environment={
                            "LD_PRELOAD": str(lib), "CARRY_FIXTURE_STORAGE_ROOT": str(state),
                            "CARRY_FIXTURE_STORAGE_FAULT": str(marker)})
                        if phase == "preparation":
                            marker.touch()
                        body = self.body(stream)
                        status, headers, raw = self.request(proxy, body)
                        self.assertEqual(status, 200, raw)
                        self.assertEqual(raw, output)
                        self.assertEqual(fixture.primaries, [body], "never regenerate after a local write failure")
                        self.assertEqual(fixture.reviews, [])
                        self.assertGreater(marker.stat().st_size, 0, "real storage write must encounter ENOSPC")
                        self.assertFalse(fixture.errors, fixture.errors)
                        if phase == "preparation":
                            self.assertEqual(headers.get("x-carry-durability"), "unavailable")
                        else:
                            self.assertTrue(proxy.saved()["primary_pending"], "failed persistence must not invent settled accounting")
                            self.assertIsNone(proxy.saved().get("last_response_id"))
                        proxy.close()
                        marker.unlink()
                        # A degraded response must never become local ancestry authority.
                        proxy, fresh = self.setup_proxy(state)
                        status, _, _ = self.request(proxy, {**body, "previous_response_id": payload["id"]})
                        self.assertNotEqual(status, 200)
                        self.assertEqual(fresh.primaries, [])
                        proxy.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
