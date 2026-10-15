"""Model-free regressions against the actual Carry CLI (stdlib only).

CARRY_TEST_BINARY=/path/to/carry python3 -B tests/proxy_state_regressions.py -v
The Rust integration selectors run this same fixture with CARGO_BIN_EXE_carry.
All provider endpoints are ephemeral loopback listeners; no auth files are read.
"""

import contextlib
import http.client
import http.server
import json
import os
from pathlib import Path
import select
import subprocess
import tempfile
import threading
import time
import unittest


def wire(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()


def completed(output=(), response_id="resp_fixture"):
    return {"id": response_id, "status": "completed", "output": list(output),
            "usage": {"input_tokens": 100, "output_tokens": 1,
                      "input_tokens_details": {"cached_tokens": 0}}}


def advice_response(advice):
    return completed([{"type": "message", "role": "assistant", "content": [
        {"type": "output_text", "text": wire(advice).decode()}]}])


def ledger(body):
    for item in reversed(body["input"]):
        text = item.get("content")
        if isinstance(text, str):
            try:
                value = json.loads(text)
            except ValueError:
                continue
            if isinstance(value, dict) and "eligible_group_ids" in value:
                return value
    raise AssertionError("review request must expose its actionable ledger")


class Fixture:
    def __init__(self, primary, review):
        self.primary = primary
        self.review = review
        self.primaries = []
        self.reviews = []
        self.errors = []
        self.release = threading.Event()
        self.held = threading.Event()
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_args):
                pass

            def do_POST(self):
                try:
                    body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                    if self.path == "/review":
                        fixture.reviews.append(body)
                        result = fixture.review(body, len(fixture.reviews))
                    elif self.path == "/v1/responses":
                        fixture.primaries.append(body)
                        result = fixture.primary(body, len(fixture.primaries))
                    else:
                        raise AssertionError(f"unexpected fixture endpoint {self.path}")
                    if result is None:
                        # Close before even sending headers: a genuine upstream
                        # transport error, not a synthetic HTTP error response.
                        self.close_connection = True
                        return
                    status, value = result
                    raw = wire(value)
                    self.send_response(status)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(raw)))
                    self.send_header("Connection", "close")
                    self.end_headers()
                    self.wfile.write(raw)
                except (BrokenPipeError, ConnectionResetError):
                    # Expected only when teardown kills an in-flight reviewer.
                    pass
                except Exception as error:
                    fixture.errors.append(repr(error))
                    self.close_connection = True

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.url = f"http://127.0.0.1:{self.server.server_port}"

    def close(self):
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


class Proxy:
    def __init__(self, state, fixture, extra=()):
        binary = os.environ["CARRY_TEST_BINARY"]
        # Explicit minimal environment prevents ambient provider/auth/refresh
        # configuration from entering even this loopback-only fixture.
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"),
               "HOME": str(state), "XDG_CONFIG_HOME": str(state),
               "CARRY_HOME": str(state), "NO_PROXY": "127.0.0.1,localhost",
               "CARRY_PROXY_UPSTREAM_KEY": "fixture-primary",
               "CARRY_PROXY_CLASSIFIER_KEY": "fixture-shadow"}
        args = [binary, "proxy", "--listen", "127.0.0.1:0", "--no-open-dashboard",
                "--state-dir", str(state), "--mode", "compact",
                "--upstream-url", fixture.url + "/v1/responses",
                "--classifier-url", fixture.url + "/review",
                "--classifier-model", "gpt-6-luna",
                *([] if "--min-payback-percent" in extra else ["--min-payback-percent", "0"]),
                "--bootstrap-min-input-bytes", "8388608",
                "--request-timeout-secs", "5", "--classifier-timeout-secs", "10",
                *extra]
        if not any(arg.startswith("--background-reviews") for arg in args):
            args += ["--background-reviews=false"]
        if "--payoff-requests" not in args:
            args += ["--payoff-requests", "5"]
        self.process = subprocess.Popen(args, env=env, stdin=subprocess.DEVNULL,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        if not select.select([self.process.stdout], [], [], 5)[0]:
            self.close()
            raise AssertionError("Carry did not announce its loopback listener")
        banner = self.process.stdout.readline().decode().strip()
        if not banner.startswith("CARRY_PROXY_LISTEN "):
            self.close()
            raise AssertionError(f"Carry startup failed: {banner}")
        host, port = banner.split(" ", 1)[1].rsplit(":", 1)
        self.address = (host, int(port))
        self.state = Path(state)
        status, _ = self.request("GET", "/health")
        assert status == 200, "real Carry health route is required"

    def request(self, method, path, body=None):
        connection = http.client.HTTPConnection(*self.address, timeout=5)
        try:
            connection.request(method, path, body=None if body is None else wire(body),
                               headers={"Content-Type": "application/json",
                                        "x-carry-session": "state-regression"})
            response = connection.getresponse()
            raw = response.read()
            return response.status, json.loads(raw)
        finally:
            connection.close()

    def send(self, input_items, **fields):
        return self.request("POST", "/v1/responses",
                            {"model": "gpt-6-luna", "input": input_items, **fields})

    def saved(self):
        files = list(self.state.glob("*.json"))
        assert len(files) == 1, f"expected exactly one session checkpoint, got {files}"
        return json.loads(files[0].read_text())

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(timeout=5)
        self.process.stdout.close()
        self.process.stderr.close()


class StateRegressions(unittest.TestCase):
    def setUp(self):
        self.stack = contextlib.ExitStack()
        self.addCleanup(self.stack.close)
        self.state = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
        self.evidence = {}
        self.addCleanup(self.record_evidence)

    def record_evidence(self):
        directory = os.environ.get("CARRY_TEST_EVIDENCE_DIR")
        if directory:
            path = Path(directory)
            path.mkdir(parents=True, exist_ok=True)
            (path / (self._testMethodName + ".json")).write_text(
                json.dumps(self.evidence, indent=2) + "\n")

    def fixture(self, primary, review=lambda _body, _n: (200, advice_response(
            {"protected": [], "removable": [], "memories": []}))):
        fixture = Fixture(primary, review)
        self.stack.callback(lambda: self.assertEqual(fixture.errors, []))
        self.stack.callback(fixture.close)
        return fixture

    def proxy(self, fixture, extra=()):
        proxy = Proxy(self.state, fixture, extra)
        self.stack.callback(proxy.close)
        return proxy

    def wait_saved(self, proxy, predicate):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            saved = proxy.saved()
            if predicate(saved):
                return saved
            time.sleep(0.01)
        self.fail("review checkpoint did not reach the required state")

    def continuation(self, failure, restart):
        a = {"role": "user", "content": "A: original goal"}
        o = {"type": "message", "role": "assistant", "content": [
            {"type": "output_text", "text": "O: committed answer"}]}
        b = {"role": "user", "content": "B: retry this delta exactly once"}
        after_retry = {"role": "assistant", "content": "O3: retry succeeded"}
        c = {"role": "user", "content": "C: next committed continuation"}

        def primary(_body, n):
            if n == 2:
                return (429, {"error": "fixture rate limit"}) if failure == "429" else None
            return 200, completed([o if n == 1 else after_retry], f"resp_{n}")

        fixture = self.fixture(primary)
        options = ["--review-every-requests", "100"]
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send([a])[0], 200)
        failed, _ = proxy.send([b], previous_response_id="resp_1")
        self.assertEqual(failed, 429 if failure == "429" else 502)
        saved = proxy.saved()
        self.assertEqual(saved["completed_requests"], 1)
        self.assertEqual(saved["failed_primaries"], 1)
        self.assertEqual(saved["last_response_id"], "resp_1")
        if restart:
            proxy.close()
            proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send([b], previous_response_id="resp_1")[0], 200)
        self.assertEqual(proxy.send([c], previous_response_id="resp_3")[0], 200)
        captured = [body["input"] for body in fixture.primaries]
        self.evidence.update(failure=failure, restart=restart, inputs=captured,
                             checkpoint_after_failure=saved)
        self.assertEqual(captured[1], [a, o, b], "initial failed attempt must expand its committed parent")
        self.assertEqual(captured[2], [a, o, b],
                         "S1: failed continuation must not append its attempted delta on retry")
        self.assertEqual(captured[3], [a, o, b, after_retry, c],
                         "successful retry must advance the frontier once, not retain speculative duplicates")
        self.assertTrue(all("previous_response_id" not in body for body in fixture.primaries))

    def test_failed_429_continuation_retry_once(self):
        self.continuation("429", False)

    def test_failed_429_continuation_retry_once_after_restart(self):
        self.continuation("429", True)

    def test_transport_failed_continuation_retry_once(self):
        self.continuation("transport", False)

    def test_transport_failed_continuation_retry_once_after_restart(self):
        self.continuation("transport", True)

    def test_old_drop_does_not_remove_grown_group_during_held_review(self):
        def review(_body, n):
            if n == 1:
                return 200, advice_response({"protected": [], "removable": ["g2"], "memories": []})
            fixture.held.set()
            if not fixture.release.wait(8):
                raise AssertionError("held reviewer was not released during teardown")
            return 200, advice_response({"protected": ["g2"], "removable": [], "memories": []})

        fixture = self.fixture(lambda _body, n: (200, completed(response_id=f"resp_{n}")), review)
        proxy = self.proxy(fixture, ["--background-reviews", "--payoff-requests", "1",
                                     "--min-payback-percent", "99"])
        seed = [{"role": "user", "content": "goal"},
                {"role": "assistant", "content": "old"}]
        self.assertEqual(proxy.send(seed)[0], 200)
        self.assertEqual(proxy.send(seed)[0], 200)
        saved = self.wait_saved(proxy, lambda s: s["shadow"]["calls"] == 1)
        self.assertEqual(saved["opinions"].get("2"), "drop", "setup requires actual accepted old advice")
        self.assertEqual(saved["compactions"], 0, "old advice must be unprofitable for the small old group")
        grown = seed + [
            {"type": "reasoning", "encrypted_content": "OPAQUE_NEW_EVIDENCE", "summary": []},
            {"type": "function_call", "call_id": "grow", "name": "inspect", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "grow",
             "output": "NEW_COMPLETED_TOOL_EVIDENCE ".join(str(i) for i in range(1000))},
            {"role": "user", "content": "current task after the completed tool step"}]
        self.assertEqual(proxy.send(grown)[0], 200)
        self.assertTrue(fixture.held.wait(3), "second actual review must be held in flight")
        self.assertEqual(fixture.primaries[-1]["input"], grown, "new unexposed members must first reach primary")
        self.assertEqual(proxy.saved()["background_review_pending"], True)
        self.assertEqual(proxy.send(grown)[0], 200)
        self.evidence.update(review_calls=len(fixture.reviews),
                             last_primary_input=fixture.primaries[-1]["input"],
                             final_checkpoint=proxy.saved())
        self.assertEqual(len(fixture.reviews), 2, "no replacement reviewer may authorize the grown group yet")
        self.assertFalse(fixture.release.is_set(), "reviewer must remain held through live planning")
        self.assertTrue(proxy.saved()["background_review_pending"])
        self.assertEqual(fixture.primaries[-1]["input"], grown,
                         "S2: old drop for one assistant member cannot remove newly exposed atomic evidence")

    def memory_progress(self, limit):
        text_a = ("a0b1c2d3e4f5g6h7i8j9" * 205)[:4096]
        text_b = ("k9l8m7n6o5p4q3r2s1t0" * 205)[:4096]
        facts = [{"source_ids": ["g2"], "text": text_a},
                 {"source_ids": ["g2"], "text": text_b}]

        def review(body, n):
            data = ledger(body)
            advice = {"protected": [], "removable": [], "memories": facts} if n == 1 else {
                "protected": [], "removable": data["eligible_memory_ids"], "memories": []}
            return 200, advice_response(advice)

        fixture = self.fixture(lambda _body, n: (200, completed(response_id=f"resp_{n}")), review)
        options = ["--classifier-max-input-bytes", "8388608" if limit == "tokens" else "10000",
                   "--classifier-max-input-tokens", "4000" if limit == "tokens" else "120000",
                   "--payoff-requests", "1"]
        proxy = self.proxy(fixture, options)
        seed = [{"role": "user", "content": "goal"},
                {"role": "assistant", "content": "small eligible source"},
                {"role": "user", "content": "current"}]
        self.assertEqual(proxy.send(seed)[0], 200)
        self.assertEqual(proxy.send(seed)[0], 200)
        self.assertEqual(len(fixture.reviews), 1, "initial source must fit the configured review budget")
        saved = proxy.saved()
        accepted = len(saved["memories"])
        if accepted < 2:
            self.assertGreater(saved["review_feedback"]["rejected_count"], 0,
                               "budget admission may reject facts only with explicit feedback")
        self.evidence.update(limit=limit, accepted_memories=accepted,
                             feedback=saved["review_feedback"])
        proxy.close()
        proxy = self.proxy(fixture, options)
        # Several ordinary due reviews give a bounded window sweep room to
        # finish; no edited checkpoint, direct API shortcut, or new session.
        for _ in range(3):
            self.assertEqual(proxy.send(seed)[0], 200)
        self.evidence.update(review_calls=len(fixture.reviews),
                             reviewer_requests=fixture.reviews,
                             reviewer_limits={"bytes": int(options[1]), "tokens": int(options[3])},
                             reviewer_wire_bytes=[len(wire(body)) for body in fixture.reviews],
                             final_checkpoint=proxy.saved())
        self.assertGreater(len(fixture.reviews), 1,
                           "S3: accepted near-limit memories must not silently disable subsequent review dispatch")
        for body in fixture.reviews:
            self.assertLessEqual(len(wire(body)), int(options[1]),
                                 "cleanup must stay inside the configured byte transport guard")
        restored = proxy.saved()
        self.assertEqual(restored["memories"], [], "subsequent in-budget advice must retire accepted memories")
        self.assertFalse(any(record["source_id"] == 0 for record in restored["active_shadow"]),
                         "memory retirement must prune the shared reviewer fact projection too")

    def test_small_memory_budget_rejects_large_fact_preserves_small_sibling_and_opinions(self):
        large = ("a0b1c2d3e4f5g6h7i8j9" * 205)[:4096]
        small = "The useful result is 42."

        def review(body, n):
            data = ledger(body)
            advice = {"protected": ["g1"], "removable": ["g2"], "memories": [
                {"source_ids": ["g2"], "text": large},
                {"source_ids": ["g2"], "text": small}]} if n == 1 else {
                "protected": [], "removable": data["eligible_memory_ids"], "memories": []}
            return 200, advice_response(advice)

        fixture = self.fixture(lambda _body, n: (200, completed(response_id=f"resp_{n}")), review)
        options = ["--classifier-max-input-bytes", "10000",
                   "--classifier-max-input-tokens", "4000", "--payoff-requests", "1"]
        proxy = self.proxy(fixture, options)
        seed = [{"role": "user", "content": "goal"},
                {"role": "assistant", "content": "small eligible source"},
                {"role": "user", "content": "current"}]
        self.assertEqual(proxy.send(seed)[0], 200)
        self.assertEqual(proxy.send(seed)[0], 200)
        saved = proxy.saved()
        self.assertEqual([m["text"] for m in saved["memories"]], [small])
        self.assertEqual(saved["opinions"].get("1"), "keep")
        self.assertEqual(saved["opinions"].get("2"), "drop")
        self.assertEqual(saved["review_feedback"]["rejected_count"], 1)
        self.assertEqual(saved["review_feedback"]["rejected"][0]["index"], 0)
        proxy.close()
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send(seed)[0], 200)
        self.assertGreaterEqual(len(fixture.reviews), 2)
        self.assertEqual(proxy.saved()["memories"], [])
        self.assertFalse(any(r["source_id"] == 0 for r in proxy.saved()["active_shadow"]))
        self.evidence.update(reviewer_requests=fixture.reviews,
                             reviewer_limits={"bytes": 10000, "tokens": 4000})

    def test_small_byte_budget_memory_admission_preserves_review_progress(self):
        self.memory_progress("bytes")

    def test_small_token_budget_memory_admission_preserves_review_progress(self):
        self.memory_progress("tokens")


if __name__ == "__main__":
    unittest.main()
