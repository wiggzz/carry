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
                    # Subscription fixtures return actual terminal SSE frames.
                    raw = value if isinstance(value, bytes) else wire(value)
                    self.send_response(status)
                    self.send_header("Content-Type", "text/event-stream" if isinstance(value, bytes)
                                     else "application/json")
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
        # Bootstrap import runs a dozen sequential review round-trips before
        # answering (~4s on fast hardware); a 5s client timeout flakes on
        # loaded runners. The proxy itself has no inbound request deadline.
        connection = http.client.HTTPConnection(*self.address, timeout=30)
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

    def assert_estimate_available(self, estimate, available, message=None):
        self.assertNotIn("priced", estimate, "public estimates have no eligibility flag")
        for field in ("carry_cost", "traditional_cost", "reviewer_cost", "summary_cost", "savings"):
            if available:
                self.assertIsInstance(estimate[field], (int, float), message or field)
            else:
                self.assertIsNone(estimate[field], message or field)
        if not available:
            self.assertIsNone(estimate["savings_percent"], message)

    def test_dashboard_backend_authoritative_savings(self):
        fixture = self.fixture(lambda _body, _n: (200, completed()))
        proxy = self.proxy(fixture, ["--review-every-requests", "100"])
        self.assertEqual(proxy.send([{"role": "user", "content": "synthetic accounting seed"}])[0], 200)
        template = proxy.saved()
        proxy.close()
        self.evidence["cases"] = []
        # Exercise the real HTTP endpoint with owned, serialized checkpoints.
        # Subtests keep every semantic boundary visible on a prebuilt baseline.
        cases = [
            ("empty", [], 0, None),
            ("positive", [(2, 10, None)], 0, (2, 10, 8, 80)),
            ("negative", [(10, 2, None)], 0, (10, 2, -8, -400)),
            ("zero_savings", [(4, 4, None)], 0, (4, 4, 0, 0)),
            ("zero_costs", [(0, 0, None)], 0, (0, 0, 0, None)),
            ("mixed_background", [(2, 10, None), (10, 2, None),
                                  (999, 1000, "legacy"), (888, 1000, "missing_usage"),
                                  (777, 1000, "pending"), (666, 1000, "no_trajectory"),
                                  (555, 1000, "unknown_cache"), (444, 1000, "untracked")],
             1, (12, 12, 0, 0)),
            ("no_eligible", [(999, 1000, "legacy"), (888, 1000, "pending")], 1, None),
        ]
        for name, entries, unreadable, expected in cases:
            with self.subTest(case=name):
                self.state = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
                for index, (carry, traditional, invalid) in enumerate(entries, 1):
                    saved = json.loads(json.dumps(template))
                    trajectory = saved["cost_trajectory"]
                    result = trajectory["result"]
                    result.update(carry_cost=carry, traditional_cost=traditional,
                                  reviewer_cost=0, summary_cost=0, excluded_failed_calls=1)
                    result["coverage"]["failed_calls"] = 1
                    if invalid == "legacy":
                        trajectory.pop("successful_requests_only", None)
                        trajectory["available"] = False
                        result["excluded_failed_calls"] = 0
                    elif invalid in ("missing_usage", "untracked"):
                        trajectory["available"] = False
                        result["coverage"]["missing_usage" if invalid == "missing_usage"
                                           else "prior_untracked_calls"] = 1
                    elif invalid == "unknown_cache":
                        result["coverage"]["unknown_cache_requests"] = 1
                    elif invalid == "pending":
                        saved["background_review_pending"] = True
                    elif invalid == "no_trajectory":
                        saved.pop("cost_trajectory")
                    (self.state / (f"{index:064x}.json")).write_text(json.dumps(saved))
                if unreadable:
                    (self.state / ("f" * 64 + ".json")).write_text("not a checkpoint")
                checkpoint_bytes = {path.name: path.read_bytes() for path in self.state.glob("*.json")}
                proxy = self.proxy(fixture, ["--background-reviews=true"])
                status, stats = proxy.request("GET", "/carry/dashboard/stats")
                proxy.close()
                self.evidence["cases"].append({"case": name, "status": status, "stats": stats})
                self.assertEqual(status, 200)
                self.assertTrue(stats["background_reviews"])
                self.assertEqual(stats["unavailable_sessions"], unreadable)
                self.assertEqual(len(stats["sessions"]), len(entries))
                self.assertIn("cost_estimate", stats, "backend must supply dashboard totals")
                aggregate = stats["cost_estimate"]
                included = sum(invalid is None for _, _, invalid in entries)
                self.assertEqual(aggregate["included_sessions"], included)
                self.assertEqual(aggregate["excluded_sessions"], len(entries) - included + unreadable)
                self.assertEqual(aggregate["excluded_failed_calls"], included,
                                 "failed-call scope belongs only to included comparisons")
                for field, value in zip(("carry_cost", "traditional_cost", "savings", "savings_percent"),
                                        expected or (None, None, None, None)):
                    self.assertEqual(aggregate[field], value, field)
                by_id = {session["id"]: session for session in stats["sessions"]}
                for index, (carry, traditional, invalid) in enumerate(entries, 1):
                    estimate = by_id[f"{index:064x}"]["cost_estimate"]
                    self.assert_estimate_available(estimate, invalid is None)
                    if invalid is None:
                        self.assertEqual(estimate["carry_cost"], carry)
                        self.assertEqual(estimate["traditional_cost"], traditional)
                        self.assertEqual(estimate["savings"], traditional - carry)
                    self.assertIn("coverage", estimate)
                    self.assertIn("traditional_compactions", estimate)
                # GET-only projection cannot migrate/rewrite saved accounting.
                self.assertEqual({path.name: path.read_bytes() for path in self.state.glob("*.json")},
                                 checkpoint_bytes)
        self.assertEqual(len(fixture.primaries), 1)
        self.assertEqual(fixture.reviews, [])

    def test_priced_subset_primary_omissions_and_restart(self):
        self.evidence["cases"] = []
        for variant in ("missing_usage", "unknown_model", "unknown_cache", "audio", "nonstandard"):
            with self.subTest(variant=variant):
                self.state = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))

                def primary(_body, n):
                    value = completed(response_id=f"resp_{n}")
                    if n == 2:
                        if variant == "missing_usage":
                            value.pop("usage")
                        elif variant == "audio":
                            value["usage"]["output_tokens_details"] = {"audio_tokens": 1}
                        elif variant == "nonstandard":
                            value["service_tier"] = "priority"
                    return 200, value

                fixture = self.fixture(primary)
                options = ["--review-every-requests", "100"]
                proxy = self.proxy(fixture, options)
                seed = [{"role": "user", "content": "priced prefix " * 600}]
                self.assertEqual(proxy.send(seed)[0], 200)
                before = proxy.saved()["cost_trajectory"]["result"]
                fields = {"model": "unknown-model" if variant == "unknown_model" else "gpt-6-astra"} if variant in ("unknown_model", "unknown_cache") else {}
                self.assertEqual(proxy.send(seed, **fields)[0], 200)
                omitted = proxy.saved()["cost_trajectory"]
                proxy.close()
                proxy = self.proxy(fixture, options)
                self.assertEqual(proxy.send(seed)[0], 200)
                stats = proxy.request("GET", "/carry/dashboard/stats")[1]
                self.evidence["cases"].append({"variant": variant, "before": before,
                                               "omitted": omitted, "stats": stats,
                                               "checkpoint": proxy.saved()})
                proxy.close()
                fixture.close()
                self.assertEqual(fixture.errors, [])
                self.assertEqual(len(fixture.primaries), 3)
                self.assertEqual(omitted["result"]["carry_cost"], before["carry_cost"], "omit both branches, never a partial pair")
                self.assertEqual(omitted["result"]["traditional_cost"], before["traditional_cost"])
                self.assertTrue(omitted["previous"] is None, "an omitted primary cannot preserve a stale cache predecessor")
                estimate = stats["sessions"][0]["cost_estimate"]
                self.assert_estimate_available(estimate, True, "unpriced calls must not hide priced comparisons")
                self.assertEqual(estimate.get("excluded_unpriced_calls"), 1)
                self.assertEqual(estimate.get("excluded_unpriced_reviewer_calls"), 0)
                self.assertAlmostEqual(estimate["carry_cost"], 2 * before["carry_cost"], delta=1e-12,
                                       msg="post-omission primary is cold, not a stale-prefix cache hit")
                self.assertEqual(stats["cost_estimate"]["included_sessions"], 1)
                self.assertEqual(stats["cost_estimate"]["excluded_unpriced_calls"], 1)

    def test_priced_subset_all_unpriced_and_aggregate_counters(self):
        def primary(_body, n):
            value = completed(response_id=f"resp_{n}")
            if n == 1:
                value.pop("usage")
            return 200, value

        fixture = self.fixture(primary)
        proxy = self.proxy(fixture, ["--review-every-requests", "100"])
        seed = [{"role": "user", "content": "synthetic task"}]
        self.assertEqual(proxy.send(seed)[0], 200)
        unpriced = proxy.saved()
        null_stats = proxy.request("GET", "/carry/dashboard/stats")[1]
        self.assertEqual(proxy.send(seed)[0], 200)
        partial = proxy.saved()
        proxy.close()
        # Actual producer snapshots plus unavailable/pending variants verify
        # omission counts do not disappear when the monetary scope is excluded.
        entries = [unpriced, partial, json.loads(json.dumps(partial)), json.loads(json.dumps(partial))]
        entries[2]["background_review_pending"] = True
        entries[3]["cost_trajectory"]["available"] = False
        entries[3]["cost_trajectory"]["result"]["coverage"]["prior_untracked_calls"] = 1
        self.state = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
        for n, saved in enumerate(entries, 1):
            (self.state / f"{n:064x}.json").write_text(json.dumps(saved))
        proxy = self.proxy(fixture, ["--background-reviews=true"])
        stats = proxy.request("GET", "/carry/dashboard/stats")[1]
        self.evidence.update(all_unpriced=null_stats, aggregate=stats, partial=partial)
        estimate = null_stats["sessions"][0]["cost_estimate"]
        self.assert_estimate_available(estimate, False, "all-unpriced must remain null, not zero")
        self.assertEqual(estimate.get("excluded_unpriced_calls"), 1)
        self.assertIsNone(null_stats["cost_estimate"]["carry_cost"])
        aggregate = stats["cost_estimate"]
        self.assertEqual(aggregate["included_sessions"], 1)
        self.assertEqual(aggregate["excluded_sessions"], 3)
        self.assertEqual(aggregate["excluded_unpriced_calls"], 4)
        self.assertEqual(aggregate["excluded_unpriced_reviewer_calls"], 0)
        self.assertEqual(aggregate["carry_cost"], partial["cost_trajectory"]["result"]["carry_cost"])
        self.assertEqual(len(fixture.primaries), 2)
        self.assertEqual(fixture.reviews, [])

    def test_successful_request_savings_after_failure_and_restart(self):
        fixture = self.fixture(lambda _body, n: (429, {"error": "fixture rate limit"})
                               if n == 2 else (200, completed(response_id=f"resp_{n}")))
        options = ["--review-every-requests", "100"]
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send([{"role": "user", "content": "initial task"}])[0], 200)
        proxy.close()
        # Small owned scenario forces a later threshold crossing without large
        # requests. Real provider endpoints are all loopback fixtures.
        path = next(self.state.glob("*.json"))
        saved = json.loads(path.read_text())
        saved["cost_trajectory"]["scenario"] = {
            "context_window": 2000, "reserve": 100, "keep_recent": 200, "summary": 50}
        path.write_text(json.dumps(saved))
        proxy = self.proxy(fixture, options)
        delta = [{"role": "user", "content": "evidence " * 3000}]
        self.assertEqual(proxy.send(delta, previous_response_id="resp_1")[0], 429)
        self.assertEqual(proxy.send(delta, previous_response_id="resp_1")[0], 200)
        proxy.close()
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send([{"role": "user", "content": "after restart"}],
                                    previous_response_id="resp_3")[0], 200)
        status, stats = proxy.request("GET", "/carry/dashboard/stats")
        self.assertEqual(status, 200)
        session = stats["sessions"][0]
        self.evidence.update(stats=stats, checkpoint=proxy.saved())
        estimate = session["cost_estimate"]
        self.assertEqual(session["completed_requests"], 3)
        self.assertEqual(estimate["coverage"]["primary_responses"], 3)
        self.assertEqual(estimate["coverage"]["failed_calls"], 1)
        self.assert_estimate_available(estimate, True, "one failure must not hide later successful-request savings")
        self.assertEqual(estimate["traditional_compactions"], 1,
                         "the successful-only conventional scenario must keep advancing after a failure")
        self.assertGreater(estimate["summary_cost"], 0)
        self.assertEqual(len(fixture.primaries), 4)
        self.assertEqual(fixture.reviews, [])
        self.assertEqual(session["primary"]["unavailable_cost_calls"], 1,
                         "failed-call spend remains unknown, not a zero-cost success")

    def test_legacy_checkpoint_keeps_fail_closed_savings_policy(self):
        fixture = self.fixture(lambda _body, n: (429, {"error": "fixture rate limit"})
                               if n == 2 else (200, completed(response_id=f"resp_{n}")))
        options = ["--review-every-requests", "100"]
        proxy = self.proxy(fixture, options)
        seed = [{"role": "user", "content": "legacy successful task"}]
        self.assertEqual(proxy.send(seed)[0], 200)
        proxy.close()
        path = next(self.state.glob("*.json"))
        saved = json.loads(path.read_text())
        saved["cost_trajectory"].pop("successful_requests_only", None)
        saved["cost_trajectory"]["result"].pop("excluded_failed_calls", None)
        path.write_text(json.dumps(saved))
        proxy = self.proxy(fixture, options)
        self.assert_estimate_available(proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]["cost_estimate"], True)
        self.assertEqual(proxy.send(seed)[0], 429)
        proxy.close()
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send(seed)[0], 200)
        session = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]
        self.evidence.update(session=session, checkpoint=proxy.saved())
        estimate = session["cost_estimate"]
        self.assert_estimate_available(estimate, False, "legacy failures cannot be retroactively reinterpreted")
        self.assertEqual(estimate["coverage"]["primary_responses"], 2)
        self.assertEqual(estimate["coverage"]["failed_calls"], 1)
        self.assertEqual(estimate.get("excluded_failed_calls", 0), 0)
        self.assertFalse(proxy.saved()["cost_trajectory"].get("successful_requests_only", False))

    def test_failed_primary_terminals_do_not_mutate_successful_scenario(self):
        self.evidence["cases"] = []
        for terminal in ("failed", "incomplete", "transport", "http_error"):
            with self.subTest(terminal=terminal):
                self.state = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))

                def primary(_body, n):
                    if n != 2:
                        return 200, completed(response_id=f"resp_{n}")
                    if terminal == "transport":
                        return None
                    if terminal == "http_error":
                        return 429, {"error": "fixture rate limit"}
                    value = completed(response_id="resp_failed")
                    value["status"] = terminal
                    return 200, value

                fixture = self.fixture(primary)
                options = ["--review-every-requests", "100"]
                proxy = self.proxy(fixture, options)
                seed = [{"role": "user", "content": "successful prefix " * 1500}]
                self.assertEqual(proxy.send(seed)[0], 200)
                before = proxy.saved()["cost_trajectory"]
                retry = seed + [{"role": "user", "content": "retry delta"}]
                status, _ = proxy.send(retry)
                self.assertEqual(status, 502 if terminal == "transport" else
                                 429 if terminal == "http_error" else 200)
                after = proxy.saved()["cost_trajectory"]
                proxy.close()
                proxy = self.proxy(fixture, options)
                self.assertEqual(proxy.send(retry)[0], 200)
                stats = proxy.request("GET", "/carry/dashboard/stats")[1]
                session = stats["sessions"][0]
                self.evidence["cases"].append({"terminal": terminal, "before": before,
                                               "after": after, "session": session})
                proxy.close()
                fixture.close()
                self.assertEqual(fixture.errors, [])
                for field in ("previous", "restored", "retired", "summary", "last_traditional_input",
                              "carry_edited", "traditional_edited"):
                    self.assertEqual(before[field], after[field], f"failed primary changed {field}")
                estimate = session["cost_estimate"]
                self.assertEqual(estimate["coverage"]["primary_responses"], 2)
                self.assertEqual(estimate["coverage"]["failed_calls"], 1)
                self.assert_estimate_available(estimate, True, "failed primary terminal must not poison successful scenario")
                self.assertEqual(estimate["excluded_failed_calls"], 1)
                self.assertEqual(session["primary"]["calls"], 3)
                self.assertEqual(session["primary"]["unavailable_cost_calls"],
                                 1 if terminal in ("transport", "http_error") else 0)

    def test_reviewer_completion_classification_survives_background_merge_and_restart(self):
        self.evidence["cases"] = []
        for background in (False, True):
            for terminal in ("failed", "incomplete", "transport", "http_error",
                             "invalid_advice", "refusal", "missing_usage"):
                with self.subTest(background=background, terminal=terminal):
                    self.state = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))

                    def review(_body, _n):
                        if terminal == "transport":
                            return None
                        if terminal == "http_error":
                            return 429, {"error": "fixture rate limit"}
                        value = completed([{"type": "message", "role": "assistant", "content": [
                            {"type": "output_text", "text": "not advice JSON"}]}])
                        if terminal in ("failed", "incomplete"):
                            value["status"] = terminal
                        elif terminal == "refusal":
                            value["output"][0]["content"] = [{"type": "refusal", "refusal": "fixture refusal"}]
                        elif terminal == "missing_usage":
                            value.pop("usage")
                        return 200, value

                    fixture = self.fixture(lambda _body, n: (200, completed(response_id=f"resp_{n}")), review)
                    options = [f"--background-reviews={'true' if background else 'false'}"]
                    proxy = self.proxy(fixture, options)
                    seed = [{"role": "user", "content": "goal"},
                            {"role": "assistant", "content": "eligible evidence"},
                            {"role": "user", "content": "current"}]
                    self.assertEqual(proxy.send(seed)[0], 200)
                    self.assertEqual(proxy.send(seed)[0], 200)
                    saved = self.wait_saved(proxy, lambda s: s["shadow"]["calls"] == 1
                                            and not s.get("background_review_pending", False))
                    proxy.close()
                    proxy = self.proxy(fixture, options)
                    stats = proxy.request("GET", "/carry/dashboard/stats")[1]
                    session = stats["sessions"][0]
                    self.evidence["cases"].append({"background": background, "terminal": terminal,
                                                   "saved": saved, "session": session,
                                                   "review_requests": fixture.reviews})
                    proxy.close()
                    fixture.close()
                    self.assertEqual(fixture.errors, [])
                    self.assertEqual(len(fixture.reviews), 1)
                    self.assertEqual(session["shadow"]["calls"], 1, "review spend merges exactly once")
                    estimate = session["cost_estimate"]
                    excluded = terminal in ("failed", "incomplete", "transport", "http_error")
                    self.assertEqual(estimate["coverage"]["reviewer_responses"], 0 if excluded else 1,
                                     "failed reviewer terminal with usage is not a successful response")
                    self.assertEqual(estimate["coverage"]["failed_calls"], 1 if excluded else 0)
                    self.assertEqual(estimate["coverage"]["missing_usage"], 1 if terminal == "missing_usage" else 0)
                    self.assert_estimate_available(estimate, True,
                                     "omit failed or unpriced reviewer calls without hiding priced primary pairs")
                    aggregate = stats["cost_estimate"]
                    self.assertEqual(aggregate["included_sessions"], 1)
                    self.assertEqual(aggregate["excluded_sessions"], 0)
                    self.assertEqual(estimate["excluded_unpriced_calls"], int(terminal == "missing_usage"))
                    self.assertEqual(estimate["excluded_unpriced_reviewer_calls"], int(terminal == "missing_usage"))
                    self.assertEqual(aggregate["excluded_unpriced_calls"], int(terminal == "missing_usage"))
                    for field in ("carry_cost", "traditional_cost", "savings", "savings_percent"):
                        if estimate[field] is None:
                            self.assertIsNone(aggregate[field])
                        else:
                            self.assertAlmostEqual(aggregate[field], estimate[field], delta=1e-12,
                                                   msg="real completed background reviewer spend belongs in backend totals")
                    if terminal == "missing_usage":
                        self.assertEqual(estimate["reviewer_cost"], 0, "known reviewer expense only; missing expense is separately counted")
                    else:
                        self.assertEqual(estimate["reviewer_cost"] > 0,
                                         terminal in ("invalid_advice", "refusal"))
                    # Observed metering remains independent, including metered failures.
                    self.assertEqual(session["shadow"]["unavailable_cost_calls"],
                                     1 if terminal in ("transport", "http_error", "missing_usage") else 0)

    def test_successful_reviewer_spend_survives_failed_primary_exactly_once(self):
        self.evidence["cases"] = []
        for background in (False, True):
            with self.subTest(background=background):
                self.state = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))

                def review(_body, _n):
                    if background:
                        fixture.held.set()
                        if not fixture.release.wait(8):
                            raise AssertionError("successful reviewer was not released")
                    return 200, advice_response({"protected": [], "removable": [], "memories": []})

                fixture = self.fixture(lambda _body, n: (429, {"error": "fixture rate limit"})
                                       if n == 2 else (200, completed(response_id=f"resp_{n}")), review)
                proxy = self.proxy(fixture, [f"--background-reviews={'true' if background else 'false'}"])
                seed = [{"role": "user", "content": "goal"},
                        {"role": "assistant", "content": "eligible evidence"},
                        {"role": "user", "content": "current"}]
                self.assertEqual(proxy.send(seed)[0], 200)
                self.assertEqual(proxy.send(seed)[0], 429)
                if background:
                    self.assertTrue(fixture.held.wait(3))
                    fixture.release.set()
                saved = self.wait_saved(proxy, lambda s: s["shadow"]["calls"] == 1
                                        and not s.get("background_review_pending", False))
                proxy.close()
                proxy = self.proxy(fixture, ["--review-every-requests", "100"])
                self.assertEqual(proxy.send(seed)[0], 200)
                session = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]
                self.evidence["cases"].append({"background": background, "saved": saved, "session": session})
                proxy.close()
                fixture.close()
                self.assertEqual(fixture.errors, [])
                estimate = session["cost_estimate"]
                self.assertEqual(len(fixture.reviews), 1)
                self.assertEqual(session["shadow"]["calls"], 1)
                self.assertEqual(estimate["coverage"]["reviewer_responses"], 1)
                self.assertEqual(estimate["coverage"]["primary_responses"], 2)
                self.assertGreater(estimate["reviewer_cost"], 0)
                self.assertEqual(estimate["reviewer_cost"], saved["cost_trajectory"]["result"]["reviewer_cost"])
                self.assert_estimate_available(estimate, True, "successful reviewer spend must survive a failed primary")

    def test_restart_orphan_reviewer_is_untracked_not_proven_failed(self):
        def review(_body, _n):
            fixture.held.set()
            if not fixture.release.wait(8):
                raise AssertionError("held reviewer was not released")
            return 200, advice_response({"protected": [], "removable": [], "memories": []})

        fixture = self.fixture(lambda _body, n: (200, completed(response_id=f"resp_{n}")), review)
        options = ["--background-reviews=true"]
        proxy = self.proxy(fixture, options)
        seed = [{"role": "user", "content": "goal"},
                {"role": "assistant", "content": "eligible evidence"},
                {"role": "user", "content": "current"}]
        self.assertEqual(proxy.send(seed)[0], 200)
        self.assertEqual(proxy.send(seed)[0], 200)
        self.assertTrue(fixture.held.wait(3))
        pending = proxy.saved()
        self.assertTrue(pending["background_review_pending"])
        self.assert_estimate_available(proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]["cost_estimate"], False)
        proxy.close()
        fixture.release.set()
        proxy = self.proxy(fixture, ["--review-every-requests", "100"])
        self.assertEqual(proxy.send(seed)[0], 200)
        session = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]
        self.evidence.update(pending=pending, session=session, checkpoint=proxy.saved())
        estimate = session["cost_estimate"]
        self.assert_estimate_available(estimate, False, "unobserved reviewer may have completed with spend")
        self.assertEqual(estimate["coverage"]["prior_untracked_calls"], 1,
                         "restart orphan is censored, not a proven failed call")
        self.assertEqual(estimate["coverage"]["failed_calls"], 0)
        self.assertEqual(session["shadow"]["unavailable_cost_calls"], 1)

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
        # A next-request plan may first prune source/control history. The next
        # incremental update can then retire the shared fact within the budget.
        for _ in range(2):
            self.assertEqual(proxy.send(seed)[0], 200)
            if len(fixture.reviews) >= 2:
                break
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
