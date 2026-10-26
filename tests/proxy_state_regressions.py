"""Model-free regressions against the actual Carry CLI (stdlib only).

CARRY_TEST_BINARY=/path/to/carry python3 -B tests/proxy_state_regressions.py -v
The Rust integration selectors run this same fixture with CARGO_BIN_EXE_carry.
All provider endpoints are ephemeral loopback listeners; no auth files are read.
"""

import contextlib
import http.client
import http.server
import json
import math
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
        self.review_auth_hashes = []
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
                        import hashlib
                        credential = self.headers.get("Authorization", "").removeprefix("Bearer ")
                        fixture.review_auth_hashes.append(hashlib.sha256(credential.encode()).hexdigest())
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
    def __init__(self, state, fixture, extra=(), extra_env=None, mode="compact", environment=None, *, wait_reviews=True):
        binary = os.environ["CARRY_TEST_BINARY"]
        # Explicit minimal environment prevents ambient provider/auth/refresh
        # configuration from entering even this loopback-only fixture.
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"),
               "HOME": str(state), "XDG_CONFIG_HOME": str(state),
               "CARRY_HOME": str(state), "NO_PROXY": "127.0.0.1,localhost",
               "CARRY_PROXY_UPSTREAM_KEY": "fixture-primary",
               "CARRY_PROXY_CLASSIFIER_KEY": "fixture-shadow",
               "CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS": "0",
               "CARRY_PROXY_REVIEW_MIN_NEW_TOKENS": "0",
               "CARRY_PROXY_REVIEW_BACKOFF_FACTOR": "1",
               "CARRY_PROXY_REVIEW_EVERY_REQUESTS": "1"}
        env.update(extra_env or {})
        env.update(environment or {})
        args = [binary, "proxy", "--listen", "127.0.0.1:0", "--no-open-dashboard",
                "--state-dir", str(state), "--mode", mode,
                "--upstream-url", fixture.url + "/v1/responses",
                "--classifier-url", fixture.url + "/review",
                "--classifier-model", "gpt-6-luna",
                *([] if "--min-payback-percent" in extra else ["--min-payback-percent", "0"]),
                "--bootstrap-min-input-bytes", "8388608",
                *([] if "--request-timeout-secs" in extra else ["--request-timeout-secs", "5"]),
                *([] if "--classifier-timeout-secs" in extra else ["--classifier-timeout-secs", "10"]),
                *extra]
        if "--payoff-requests" not in args:
            args += ["--payoff-requests", "5"]
        self._captures = []
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
        # Fixture settlement happens only after consuming the primary response.
        # Held-review tests opt out; production admission is always asynchronous.
        self.wait_reviews = wait_reviews
        # Long loopback tracers must not deadlock the actual binary on a full
        # unread diagnostic pipe. Spool both streams; never discard evidence.
        for name, stream in (("stdout", self.process.stdout), ("stderr", self.process.stderr)):
            capture = (self.state / f"fixture-{self.process.pid}-{name}.log").open("ab", buffering=0)
            drain = threading.Thread(target=self.drain_output, args=(stream, capture), daemon=True)
            self._captures.append((drain, capture))
            drain.start()
        status, _ = self.request("GET", "/health")
        assert status == 200, "real Carry health route is required"

    @staticmethod
    def drain_output(stream, capture):
        while chunk := stream.read1(16384):
            capture.write(chunk)

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
        response = self.request("POST", "/v1/responses",
                                {"model": "gpt-6-luna", "input": input_items, **fields})
        if self.wait_reviews and response[0] == 200:
            deadline = time.monotonic() + 15
            while not self.reviewers_settled():
                assert time.monotonic() < deadline, "post-response reviewer did not settle"
                time.sleep(0.01)
        return response

    def reviewers_settled(self):
        # Paid receipt persistence deliberately precedes H publication. This is
        # only a worker-completion barrier; every authority/receipt assertion
        # still reads the canonical index, never this fail-open diagnostic lane.
        if self.saved().get("background_review_pending"):
            return False
        dispatched, merged = set(), set()
        for path in (self.state / "diagnostics").glob("*.jsonl"):
            for line in path.read_text().splitlines():
                try:
                    event = json.loads(line)
                except ValueError:
                    continue  # concurrent append has not finished this line
                request = event.get("request_id", "")
                if not request.startswith(f"{self.process.pid}-"):
                    continue  # an orphan from another process cannot settle here
                if event.get("event") == "background_review_dispatched":
                    dispatched.add(request)
                elif event.get("event") == "background_review_merged":
                    merged.add(request)
        return dispatched <= merged

    def saved(self):
        files = list(self.state.glob("*.json"))
        assert len(files) == 1, f"expected exactly one session checkpoint, got {files}"
        return json.loads(files[0].read_text())

    def advice(self):
        """Read the actual completed prefix record, not the paid-owner mirror."""
        import hashlib
        state = self.saved()
        index = json.loads((self.state / "history.index").read_text())
        namespace = state["history_namespace"]
        values = [item["value"] for item in state["history"]]
        if state.get("history_frontier"):
            values, key = [], state["history_frontier"]
            while key in index["nodes"]:
                node = index["nodes"][key]
                values.append(node["value"])
                key = node["parent"]
            values.reverse()
        key, found = namespace, None
        for value in values:
            key = hashlib.sha256(wire([key, value])).hexdigest()
            if key in index["reviews"]:
                found = index["reviews"][key]
        return found

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(timeout=5)
        for drain, capture in self._captures:
            drain.join(timeout=5)
            if drain.is_alive():
                raise AssertionError("fixture diagnostic stream did not reach EOF")
            capture.close()
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

    def proxy(self, fixture, extra=(), environment=None, *, wait_reviews=True):
        proxy = Proxy(self.state, fixture, extra, environment, wait_reviews=wait_reviews)
        self.stack.callback(proxy.close)
        return proxy

    def wait_saved(self, proxy, predicate):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            saved = proxy.saved()
            if predicate(saved) and (saved.get("background_review_pending") or proxy.reviewers_settled()):
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

    def test_accounting_basis_matches_current_and_retained_legacy_policy(self):
        fixture = self.fixture(lambda _body, _n: (200, completed()))
        proxy = self.proxy(fixture)
        status, stats = proxy.request("GET", "/carry/dashboard/stats")
        self.assertEqual(status, 200)
        self.assertIn("receipt_anchored_v2", stats["cost_estimate_basis"])
        self.assertIn("legacy_json_v1", stats["cost_estimate_basis"])
        self.assertNotIn("reviewer_uncached", stats["cost_estimate_basis"])
        policy = stats["cost_estimate_policy"]
        self.assertEqual(policy["default_basis"], "receipt_anchored_v2")
        self.assertEqual(policy["retained_legacy_basis"], "legacy_json_v1")
        self.assertEqual(policy["observed_cost_basis"], "provider_native_partitions_with_dispatch_frozen_tariffs")
        self.assertEqual(proxy.send([{"role":"user","content":"policy fixture"}])[0], 200)
        _, stats = proxy.request("GET", "/carry/dashboard/stats")
        estimate = stats["sessions"][0]["cost_estimate"]
        self.assertEqual(estimate["accounting_basis"], policy["default_basis"])
        self.assertEqual(estimate["counterfactual_basis"], policy["counterfactual_basis"])
        self.evidence.update(policy=policy, estimate=estimate)

    def test_reviewer_control_observations_once_and_restart(self):
        fixture = self.fixture(lambda _body, _n: (200, completed()))
        options = ["--min-payback-percent", "100"]
        proxy = self.proxy(fixture, options)
        history = [{"role": "user", "content": "fixture goal"},
                   {"role": "assistant", "content": "source once " + "s" * 8192},
                   {"role": "user", "content": "continue"}]
        instruction = "UNIQUE_DIRECTION_" + "d" * 32768
        for n in range(5):
            if n == 3:
                proxy.close()
                proxy = self.proxy(fixture, options)
            self.assertEqual(proxy.send(history, instructions=instruction)[0], 200)
        self.assertEqual(len(fixture.reviews), 4)
        body = fixture.reviews[-1]
        for captured in fixture.reviews:
            direction = next(i for i, item in enumerate(captured["input"])
                             if b"UNIQUE_DIRECTION_" in wire(item))
            source = next(i for i, item in enumerate(captured["input"])
                          if b"fixture goal" in wire(item))
            self.assertLess(direction, source,
                            "quoted primary direction must precede conversation sources")
        self.assertEqual(wire(body).count(b"UNIQUE_DIRECTION_"), 1,
                         "unchanged primary instructions must be ingested once, not per ledger")
        for before, after in zip(fixture.reviews, fixture.reviews[1:]):
            self.assertEqual(after["input"][:-1], before["input"][:-1],
                             "fresh source prefix is stable; prior ledgers/answers are not replayed")
        self.assertEqual(proxy.send(history, instructions="NEW_DIRECTION")[0], 200)
        changed = fixture.reviews[-1]
        self.assertEqual(wire(changed).count(b"NEW_DIRECTION"), 1)
        self.assertNotIn(b"UNIQUE_DIRECTION_", wire(changed))
        self.assertEqual(proxy.send(history, instructions="")[0], 200)
        cleared = fixture.reviews[-1]
        self.assertNotIn(b"NEW_DIRECTION", wire(cleared))
        self.assertNotIn(b"UNIQUE_DIRECTION_", wire(cleared))
        self.assertNotIn("primary_instruction_version", ledger(cleared))
        proxy.close()
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertEqual(fixture.reviews[-1]["input"][:-1], cleared["input"][:-1])
        self.evidence.update(reviews=fixture.reviews, checkpoint=proxy.saved())

    def test_direction_changes_reject_held_advice_and_preserve_paid_receipt(self):
        for change in ("unchanged", "changed", "clear_then_restore"):
            with self.subTest(change=change):
                def review(_body, _n):
                    fixture.held.set()
                    if not fixture.release.wait(8):
                        raise AssertionError("held reviewer was not released")
                    return 200, advice_response({"protected": [2], "removable": [],
                        "memories": [{"text": "HELD_DIRECTION_FACT", "source_ids": [2]}]})

                fixture = self.fixture(lambda _body, n: (200, completed(response_id=f"resp_{n}")), review)
                options = ["--min-payback-percent", "100"]
                proxy = Proxy(self.state / change, fixture, options, wait_reviews=False)
                self.addCleanup(proxy.close)
                seed = [{"role": "user", "content": "goal"},
                        {"role": "assistant", "content": "eligible evidence"},
                        {"role": "user", "content": "current"}]
                self.assertEqual(proxy.send(seed, instructions="ORIGINAL_DIRECTION")[0], 200)
                self.assertEqual(proxy.send(seed, instructions="ORIGINAL_DIRECTION")[0], 200)
                self.assertTrue(fixture.held.wait(3))
                if change == "changed":
                    self.assertEqual(proxy.send(seed, instructions="CHANGED_DIRECTION")[0], 200)
                elif change == "clear_then_restore":
                    self.assertEqual(proxy.send(seed, instructions="")[0], 200)
                    self.assertEqual(proxy.send(seed, instructions="ORIGINAL_DIRECTION")[0], 200)
                fixture.release.set()
                saved = self.wait_saved(proxy, lambda s: not s["background_review_pending"])
                self.assertEqual(saved["shadow"]["calls"], 1,
                                 "even stale advice must settle its actual paid receipt once")
                self.assertEqual(len(fixture.reviews), 1)
                accepted = change in ("unchanged", "clear_then_restore")
                self.assertEqual(any(m["text"] == "HELD_DIRECTION_FACT" for m in (proxy.advice() or {}).get("memories", [])), accepted)
                self.assertEqual((proxy.advice() or {}).get("opinions", {}).get("2") == "keep", accepted)
                proxy.close()
                proxy = Proxy(self.state / change, fixture, ["--review-every-requests", "100"])
                self.addCleanup(proxy.close)
                direction = "CHANGED_DIRECTION" if change == "changed" else "ORIGINAL_DIRECTION"
                self.assertEqual(proxy.send(seed, instructions=direction)[0], 200)
                restarted = proxy.saved()
                self.assertEqual(restarted["shadow"]["calls"], 1)
                self.assertEqual(any(m["text"] == "HELD_DIRECTION_FACT" for m in (proxy.advice() or {}).get("memories", [])), accepted)
                self.evidence[change] = {"settled": saved, "restarted": restarted, "reviews": fixture.reviews}
                proxy.close()

    def capacity_rejection_direction(self, change, publication_failure=False):
        def review(body, n):
            if n == 1:
                self.assertIn(2, ledger(body)["eligible_group_ids"])
                fixture.held.set()
                if not fixture.release.wait(8):
                    raise AssertionError("capacity waiter did not release the held A reviewer")
                return 200, advice_response({"protected": [], "removable": [2],
                    "memories": [{"text": "CAPACITY_HELD_A_FACT", "source_ids": [2]}]})
            # A rejected old review may need a fresh blocking capacity review.
            # That actual B review grants no removal or memory authority.
            return 200, advice_response({"protected": [], "removable": [], "memories": []})

        fixture = self.fixture(lambda _body, n: (200, completed(response_id=f"resp_{n}")), review)
        options = ["--primary-context-tokens", "4096",
                   "--min-payback-percent", "100", "--payoff-requests", "1",
                   "--review-replayed-history", "--review-every-requests", "100"]
        proxy = self.proxy(fixture, options, wait_reviews=False)
        seed = [{"role": "user", "content": "goal"},
                {"role": "assistant", "content": "old evidence " * 1500},
                {"role": "user", "content": "current"}]
        fields = {"instructions": "ORIGINAL_DIRECTION", "max_output_tokens": 1}
        self.assertEqual(proxy.send(seed, **fields)[0], 200)
        self.assertEqual(proxy.send(seed, **fields)[0], 200)
        self.assertTrue(fixture.held.wait(3), "an actual A-direction reviewer must be in flight")
        before = proxy.saved()
        self.assertTrue(before["background_review_pending"])
        self.assertNotIn("2", before.get("opinions", {}))
        self.assertEqual(before.get("memories", []), [])
        growing = seed + [{"role": "user", "content": "new material " * 1500}]
        direction = "ORIGINAL_DIRECTION" if change == "unchanged" else (
            "CHANGED_DIRECTION" if change == "changed" else "")
        result, errors = [], []
        checkpoint = next(self.state.glob("*.json"))
        original_bytes = checkpoint.read_bytes()
        blocked_temporary = self.state / f".{checkpoint.stem}.tmp"
        if publication_failure:
            # Fail the real explicit-authority staging open, not its reader or
            # the provider. The old checkpoint remains readable byte-for-byte.
            blocked_temporary.mkdir()

        def send_pressure():
            try:
                result.append(proxy.send(growing, instructions=direction, max_output_tokens=1))
            except Exception as error:
                errors.append(repr(error))

        waiter = threading.Thread(target=send_pressure, daemon=True)
        waiter.start()
        try:
            # #163 rejects overflow using already accepted authority; it never
            # waits for a reviewer or buys rescue advice on the primary path.
            waiter.join(timeout=3)
            self.assertFalse(waiter.is_alive(), "capacity rejection must not wait for held review")
            self.assertEqual(errors, [])
            self.assertEqual(len(result), 1)
            self.assertEqual(result[0][0], 413)
            self.assertEqual(len(fixture.primaries), 2, "no capacity-overflow input may reach upstream")
            at_wait = proxy.saved()
            if change == "clear_then_restore":
                # Even two rejected requests must publish both invalidations.
                self.assertEqual(proxy.send(growing, **fields)[0], 413)
                direction = "ORIGINAL_DIRECTION"
            before_release = proxy.saved()
            if publication_failure:
                self.assertEqual(checkpoint.read_bytes(), original_bytes,
                                 "fault must preserve the old readable authority")
                self.evidence["failed_publication"] = {
                    "checkpoint_unchanged": True, "temporary": blocked_temporary.name,
                    "old_epoch": before["review_epoch"],
                    "readable_epoch": before_release["review_epoch"]}
                blocked_temporary.rmdir()
                if change == "clear_then_restore":
                    # Recover storage and persist A again before releasing A's
                    # old worker. A same-input ingress must not erase the fence.
                    self.assertEqual(proxy.send(growing, **fields)[0], 413)
                    before_release = proxy.saved()
        finally:
            if publication_failure and blocked_temporary.is_dir():
                blocked_temporary.rmdir()
            fixture.release.set()
            waiter.join(timeout=20)
        settled = self.wait_saved(proxy, lambda s: not s["background_review_pending"])
        if change in ("unchanged", "clear_then_restore"):
            # Compatible advice may help only the next request, never turn the
            # earlier rejected primary into a blocking reviewer-dependent call.
            self.assertEqual(proxy.send(growing, instructions=direction, max_output_tokens=1)[0], 200)
            settled = proxy.saved()
        # Checkpoint replacement precedes its settlement journal append. Wait
        # for that actual event rather than racing the worker's final logging.
        deadline = time.monotonic() + 3
        while True:
            entries = [json.loads(line) for path in (self.state / "diagnostics").glob("*.jsonl")
                       for line in path.read_bytes().splitlines()]
            merges = [e for e in entries if e["event"] in (
                "background_review_merged", "background_review_discarded")]
            if merges or time.monotonic() >= deadline:
                break
            time.sleep(0.01)
        self.evidence.update(change=change, before=before, at_wait=at_wait,
                             before_release=before_release, settled=settled, result=result,
                             reviews=fixture.reviews, primaries=fixture.primaries, events=entries)
        self.assertEqual(len(merges), 1, "held review has exactly one paid settlement")
        self.assertEqual(merges[0]["data"]["receipt_count"], 1)
        self.assertEqual(settled["shadow"]["calls"], len(fixture.reviews))
        self.assertEqual(settled["shadow"]["input_tokens"], 100 * len(fixture.reviews))
        accepted = change in ("unchanged", "clear_then_restore")
        if not publication_failure or accepted:
            self.assertEqual(any(m["text"] == "CAPACITY_HELD_A_FACT"
                                 for m in (proxy.advice() or {}).get("memories", [])), accepted)
        self.assertEqual(result[0][0], 413, "an in-flight review cannot hold/rescue that request")
        if accepted:
            self.assertIn(b"CAPACITY_HELD_A_FACT", wire(fixture.primaries[-1]))
            self.assertNotIn(b"old evidence", wire(fixture.primaries[-1]))
        else:
            self.assertNotIn(b"CAPACITY_HELD_A_FACT", wire(fixture.primaries))
        proxy.close()
        proxy = self.proxy(fixture, ["--review-every-requests", "100",
                                     "--primary-context-tokens", "16384",
                                     "--min-payback-percent", "100"])
        self.assertEqual(proxy.send(growing, instructions=direction, max_output_tokens=1)[0], 200)
        restarted = proxy.saved()
        # Changed direction may legitimately acquire a fresh review: compare
        # paid totals with the fixture's actual calls, never forbid that work.
        self.assertEqual(restarted["shadow"]["calls"], len(fixture.reviews))
        self.assertEqual(restarted["shadow"]["input_tokens"], 100 * len(fixture.reviews))
        self.assertEqual(restarted["shadow"]["output_tokens"], len(fixture.reviews))
        self.assertEqual(restarted["shadow"]["unavailable_cost_calls"], 0)
        self.assertGreaterEqual(restarted["shadow"]["cost_usd"], settled["shadow"]["cost_usd"])
        self.assertEqual(any(m["text"] == "CAPACITY_HELD_A_FACT"
                             for m in (proxy.advice() or {}).get("memories", [])), accepted)
        self.assertEqual(len(fixture.reviews), 1 if accepted or publication_failure else 2)
        self.evidence["restarted"] = restarted
        print("CAPACITY_EPOCH_RESTART " + json.dumps({"change": change,
              "shadow": restarted["shadow"], "review_calls": len(fixture.reviews),
              "memory_accepted": any(m["text"] == "CAPACITY_HELD_A_FACT"
                                     for m in (proxy.advice() or {}).get("memories", []))}), flush=True)

    def test_capacity_rejection_unchanged_direction_accepts_held_advice(self):
        self.capacity_rejection_direction("unchanged")

    def test_capacity_rejection_changed_direction_rejects_held_advice(self):
        self.capacity_rejection_direction("changed")

    def test_capacity_rejection_cleared_direction_rejects_held_advice(self):
        self.capacity_rejection_direction("cleared")

    def test_capacity_rejection_clear_restore_rejects_held_advice(self):
        self.capacity_rejection_direction("clear_then_restore")

    def test_capacity_rejection_failed_save_unchanged_accepts_held_advice(self):
        self.capacity_rejection_direction("unchanged", publication_failure=True)

    def test_capacity_rejection_failed_save_changed_rejects_held_advice(self):
        self.capacity_rejection_direction("changed", publication_failure=True)

    def test_capacity_rejection_failed_save_cleared_rejects_held_advice(self):
        self.capacity_rejection_direction("cleared", publication_failure=True)

    def test_capacity_rejection_failed_save_restore_rejects_held_advice(self):
        self.capacity_rejection_direction("clear_then_restore", publication_failure=True)

    def test_reviewer_wire_fingerprints_are_content_free_and_join_usage(self):
        def review(_body, n):
            value = advice_response({"protected": [], "removable": [], "memories": []})
            if n == 2:
                del value["usage"]["input_tokens_details"]
            if n == 3:
                del value["usage"]
            return 200, value

        fixture = self.fixture(lambda _body, _n: (200, completed()), review)
        proxy = self.proxy(fixture, ["--min-payback-percent", "100"])
        history = [{"role": "user", "content": "PRIVATE_FIXTURE_GOAL"},
                   {"role": "assistant", "content": "PRIVATE_TOOL_BYTES" + "x" * 8192},
                   {"role": "user", "content": "continue"}]
        for _ in range(4):
            self.assertEqual(proxy.send(history, instructions="PRIVATE_INSTRUCTION")[0], 200)
        raw = b"".join(p.read_bytes() for p in (self.state / "diagnostics").glob("*.jsonl"))
        entries = [json.loads(line) for line in raw.splitlines()]
        requests = [e for e in entries if e["event"] == "review_wire"]
        self.assertEqual(len(requests), 3, "every final transport request needs bounded item-order diagnostics")
        for sentinel in (b"PRIVATE_FIXTURE_GOAL", b"PRIVATE_TOOL_BYTES", b"PRIVATE_INSTRUCTION",
                         b"fixture-primary", b"fixture-shadow", fixture.url.encode()):
            self.assertNotIn(sentinel, raw)
        for n, (entry, body) in enumerate(zip(requests, fixture.reviews), 1):
            data = entry["data"]
            self.assertEqual(data["format_version"], 1)
            self.assertEqual(data["item_count"], len(body["input"]))
            self.assertTrue(data["coverage_complete"])
            self.assertEqual(len(data["items"]), len(body["input"]))
            self.assertLessEqual(len(wire(entry)), 16384)
            receipts = [e for e in entries if e["event"] == "usage" and
                        e.get("review_attempt_id") == entry["review_attempt_id"]]
            self.assertEqual(len(receipts), 1)
            usage = receipts[0]["data"]["native_usage"]
            if n == 3:
                self.assertEqual(usage, {}, "absent native usage must remain unknown")
            else:
                self.assertEqual(usage["input_tokens"], 100)
                if n == 2:
                    self.assertNotIn("input_tokens_details", usage,
                                     "unknown cache receipt must not become a fabricated zero")
                else:
                    self.assertEqual(usage["input_tokens_details"]["cached_tokens"], 0)
        for entry in requests[1:]:
            self.assertIsInstance(entry["data"]["prior_prefix_equal"], bool)
            self.assertTrue(entry["data"]["settings_equal"])

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
             1, (789, 1012, 223, 100.0 * 223 / 1012)),
            ("pending_with_legacy_peer", [(999, 1000, "legacy"), (888, 1000, "pending")],
             1, (888, 1000, 112, 11.2)),
            ("no_eligible", [(999, 1000, "legacy")], 1, None),
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
                        # Old unknown-cache costs were accumulated but were not
                        # comparable; they have no trustworthy priced-pair count.
                        trajectory.pop("priced_primary_responses", None)
                        result["coverage"]["unknown_cache_requests"] = 1
                    elif invalid == "pending":
                        saved["background_review_pending"] = True
                    elif invalid == "no_trajectory":
                        saved.pop("cost_trajectory")
                    (self.state / (f"{index:064x}.json")).write_text(json.dumps(saved))
                if unreadable:
                    (self.state / ("f" * 64 + ".json")).write_text("not a checkpoint")
                checkpoint_bytes = {path.name: path.read_bytes() for path in self.state.glob("*.json")}
                proxy = self.proxy(fixture, wait_reviews=False)
                status, stats = proxy.request("GET", "/carry/dashboard/stats")
                proxy.close()
                self.evidence["cases"].append({"case": name, "status": status, "stats": stats})
                self.assertEqual(status, 200)
                self.assertTrue(stats["background_reviews"])
                self.assertEqual(stats["unavailable_sessions"], unreadable)
                self.assertEqual(len(stats["sessions"]), len(entries))
                self.assertIn("cost_estimate", stats, "backend must supply dashboard totals")
                aggregate = stats["cost_estimate"]
                included = sum(invalid in (None, "pending") for _, _, invalid in entries)
                self.assertEqual(aggregate["included_sessions"], included)
                self.assertEqual(aggregate["excluded_sessions"], len(entries) - included + unreadable)
                self.assertEqual(aggregate["in_flight_sessions"],
                                 sum(invalid == "pending" for _, _, invalid in entries))
                self.assertEqual(aggregate["excluded_failed_calls"], included,
                                 "failed-call scope belongs only to included comparisons")
                for field, value in zip(("carry_cost", "traditional_cost", "savings", "savings_percent"),
                                        expected or (None, None, None, None)):
                    self.assertEqual(aggregate[field], value, field)
                by_id = {session["id"]: session for session in stats["sessions"]}
                for index, (carry, traditional, invalid) in enumerate(entries, 1):
                    estimate = by_id[f"{index:064x}"]["cost_estimate"]
                    self.assert_estimate_available(estimate, invalid in (None, "pending"))
                    self.assertEqual(estimate["in_flight_costs_excluded"], invalid == "pending")
                    if invalid in (None, "pending"):
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
                if variant == "unknown_model":
                    # Unknown price remains unknown; capacity is explicitly
                    # declared instead of silently borrowed from a role default.
                    options += ["--primary-context-tokens", "272000"]
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
                if variant == "unknown_cache":
                    # Actuals use the zero-cache receipt; the traditional branch
                    # assumes uncached when cache capability is unknown. No
                    # projection is substituted for observed provider input.
                    result = omitted["result"]
                    delta = result["carry_cost"] - before["carry_cost"]
                    self.assertGreater(delta, 0)
                    self.assertAlmostEqual(result["traditional_cost"] - before["traditional_cost"],
                                           delta, delta=1e-12, msg="matched uncached branches, no invented savings")
                    self.assertIsNotNone(omitted["receipts"]["anchor"])
                    self.assertEqual(result["excluded_unpriced_calls"], 0)
                    self.assertGreater(result["coverage"]["unknown_cache_requests"],
                                       before["coverage"]["unknown_cache_requests"])
                    estimate = stats["sessions"][0]["cost_estimate"]
                    self.assert_estimate_available(estimate, True)
                    self.assertEqual(estimate["excluded_unpriced_calls"], 0)
                    self.assertAlmostEqual(estimate["carry_cost"], 2 * before["carry_cost"] + delta,
                                           delta=1e-12, msg="restart preserves frozen totals without repricing")
                    self.assertEqual(stats["cost_estimate"]["excluded_unpriced_calls"], 0)
                    continue
                self.assertEqual(omitted["result"]["carry_cost"], before["carry_cost"], "omit both branches, never a partial pair")
                self.assertEqual(omitted["result"]["traditional_cost"], before["traditional_cost"])
                self.assertIsNone(omitted["receipts"]["anchor"], "an omitted primary cannot preserve a stale receipt anchor")
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
        proxy = self.proxy(fixture, wait_reviews=False)
        stats = proxy.request("GET", "/carry/dashboard/stats")[1]
        self.evidence.update(all_unpriced=null_stats, aggregate=stats, partial=partial)
        estimate = null_stats["sessions"][0]["cost_estimate"]
        self.assert_estimate_available(estimate, False, "all-unpriced must remain null, not zero")
        self.assertEqual(estimate.get("excluded_unpriced_calls"), 1)
        self.assertIsNone(null_stats["cost_estimate"]["carry_cost"])
        aggregate = stats["cost_estimate"]
        self.assertEqual(aggregate["included_sessions"], 2)
        self.assertEqual(aggregate["excluded_sessions"], 2)
        self.assertEqual(aggregate["in_flight_sessions"], 1)
        self.assertEqual(aggregate["excluded_unpriced_calls"], 4)
        self.assertEqual(aggregate["excluded_unpriced_reviewer_calls"], 0)
        self.assertEqual(aggregate["carry_cost"], 2 * partial["cost_trajectory"]["result"]["carry_cost"])
        self.assertEqual(len(fixture.primaries), 2)
        self.assertEqual(fixture.reviews, [])

    def test_successful_request_savings_after_failure_and_restart(self):
        def primary(_body, n):
            if n == 2:
                return 429, {"error": "fixture rate limit"}
            value = completed(response_id=f"resp_{n}")
            value["usage"]["input_tokens"] = {1: 100, 3: 2000, 4: 2100}[n]
            return 200, value
        fixture = self.fixture(primary)
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

    def test_traditional_summary_between_tool_requests_survives_restart(self):
        tool = {"type": "function_call", "name": "lookup", "call_id": "call_2",
                "arguments": "{}"}
        def primary(_body, n):
            value = completed([tool] if n == 2 else [], response_id=f"resp_{n}")
            value["usage"]["input_tokens"] = {1: 100, 2: 2000, 3: 2100}[n]
            return 200, value
        fixture = self.fixture(primary)
        options = ["--review-every-requests", "100"]
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send([{"role": "user", "content": "initial task"}])[0], 200)
        proxy.close()
        # Owned checkpoint only; force the modeled threshold, not live policy.
        path = next(self.state.glob("*.json"))
        saved = json.loads(path.read_text())
        saved["cost_trajectory"]["scenario"] = {
            "context_window": 2000, "reserve": 100, "keep_recent": 1500, "summary": 100}
        path.write_text(json.dumps(saved))
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send([{"role": "user", "content": "evidence " * 2000}],
                                    previous_response_id="resp_1")[0], 200)
        estimate = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]["cost_estimate"]
        self.evidence["after_tool_call"] = estimate
        self.assertEqual(estimate["traditional_compactions"], 1,
                         "compact the scenario before the tool-result request, not at turn end")
        self.assertGreater(estimate["summary_cost"], 0)
        self.assertEqual(proxy.saved()["cost_trajectory"]["last_traditional_input"], 1600)
        proxy.close()
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send([{"type": "function_call_output", "call_id": "call_2", "output": "ok"}],
                                    previous_response_id="resp_2")[0], 200)
        after = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]["cost_estimate"]
        self.evidence["after_restart"] = after
        self.assertEqual(after["traditional_compactions"], 1)
        self.assertEqual(after["summary_cost"], estimate["summary_cost"])
        self.assertEqual(len(fixture.primaries), 3)
        self.assertEqual(fixture.reviews, [])
        self.assertEqual(fixture.primaries[2]["input"][-2:], [tool, {
            "type": "function_call_output", "call_id": "call_2", "output": "ok"}],
            "hypothetical summary must not modify real Carry tool continuation")

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
                    options = []
                    proxy = self.proxy(fixture, options, wait_reviews=not background)
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
                    self.assertEqual(estimate.get("excluded_unpriced_calls"), int(terminal == "missing_usage"))
                    self.assertEqual(estimate.get("excluded_unpriced_reviewer_calls"), int(terminal == "missing_usage"))
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
                proxy = self.proxy(fixture, wait_reviews=not background)
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
                # Rust/Python JSON roundtrip can differ by one IEEE-754 ULP;
                # permit only that representation error, not a money tolerance.
                saved_cost = saved["cost_trajectory"]["result"]["reviewer_cost"]
                self.assertAlmostEqual(estimate["reviewer_cost"], saved_cost,
                                       delta=max(math.ulp(estimate["reviewer_cost"]),
                                                 math.ulp(saved_cost)))
                self.assert_estimate_available(estimate, True, "successful reviewer spend must survive a failed primary")

    def test_reviewer_only_failures_preserve_primary_cache_continuity(self):
        self.evidence["cases"] = []
        for background in (False, True):
            for restart in (False, True):
                controls = None
                for outcome in ("no-review", "completed", "failed", "incomplete", "429", "transport"):
                    with self.subTest(background=background, restart=restart, outcome=outcome):
                        self.state = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))

                        def primary(_body, n):
                            response = completed(response_id=f"resp_{n}")
                            response["usage"]["input_tokens"] = 2000 + 100 * n
                            response["usage"]["output_tokens"] = 0
                            return 200, response

                        def review(_body, n):
                            if n == 1 and outcome == "transport":
                                return None
                            if n == 1 and outcome == "429":
                                return 429, {"error": "fixture reviewer rate limit"}
                            response = advice_response({"protected": [], "removable": [], "memories": []})
                            if n == 1 and outcome in ("failed", "incomplete"):
                                response["status"] = outcome
                            return 200, response

                        fixture = self.fixture(primary, review)
                        options = []
                        if outcome == "no-review":
                            options += ["--review-every-requests", "100"]
                        proxy = self.proxy(fixture, options, wait_reviews=not background)
                        seed = [{"role": "user", "content": "goal"},
                                {"role": "assistant", "content": "eligible evidence"},
                                {"role": "user", "content": "current"}]
                        costs = []
                        for n in range(3):
                            self.assertEqual(proxy.send(seed + [
                                {"role": "user", "content": f"continue {i}"} for i in range(n)
                            ])[0], 200)
                            saved = self.wait_saved(proxy, lambda s: not s.get("background_review_pending", False))
                            costs.append(saved["cost_trajectory"]["result"]["traditional_cost"])
                            if restart and n == 1:
                                proxy.close()
                                proxy = self.proxy(fixture, ["--review-every-requests", "100"])
                        estimate = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]["cost_estimate"]
                        proxy.close()
                        fixture.close()
                        self.assertEqual(fixture.errors, [])
                        self.evidence["cases"].append({"background": background, "restart": restart,
                                                       "outcome": outcome, "costs": costs, "estimate": estimate})
                        if outcome == "no-review":
                            self.assertEqual(len(fixture.reviews), 0)
                            controls = (costs, estimate["observed_primary_cost"])
                        else:
                            self.assertGreaterEqual(len(fixture.reviews), 1)
                        self.assertEqual(estimate["coverage"]["failed_calls"],
                                         0 if outcome in ("no-review", "completed") else 1)
                        self.assertEqual(estimate["coverage"]["primary_responses"], 3)
                        self.assertAlmostEqual(estimate["observed_primary_cost"], controls[1], places=12)
                        for actual, expected in zip(costs, controls[0]):
                            self.assertAlmostEqual(actual, expected, places=12,
                                                   msg="reviewer-only failures must not cool the untouched primary scenario")

    def test_restart_orphan_reviewer_is_untracked_not_proven_failed(self):
        def review(_body, _n):
            fixture.held.set()
            if not fixture.release.wait(8):
                raise AssertionError("held reviewer was not released")
            return 200, advice_response({"protected": [], "removable": [], "memories": []})

        fixture = self.fixture(lambda _body, n: (200, completed(response_id=f"resp_{n}")), review)
        proxy = self.proxy(fixture, wait_reviews=False)
        seed = [{"role": "user", "content": "goal"},
                {"role": "assistant", "content": "eligible evidence"},
                {"role": "user", "content": "current"}]
        self.assertEqual(proxy.send(seed)[0], 200)
        self.assertEqual(proxy.send(seed)[0], 200)
        self.assertTrue(fixture.held.wait(3))
        pending = proxy.saved()
        self.assertTrue(pending["background_review_pending"])
        held = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]["cost_estimate"]
        self.assert_estimate_available(held, True, "completed costs remain visible while reviewer is held")
        self.assertTrue(held["in_flight_costs_excluded"])
        self.assertEqual(held["carry_cost"], pending["cost_trajectory"]["result"]["carry_cost"])
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
                return 200, advice_response({"protected": [], "removable": [2], "memories": []})
            fixture.held.set()
            if not fixture.release.wait(8):
                raise AssertionError("held reviewer was not released during teardown")
            return 200, advice_response({"protected": [2], "removable": [], "memories": []})

        fixture = self.fixture(lambda _body, n: (200, completed(response_id=f"resp_{n}")), review)
        proxy = self.proxy(fixture, ["--payoff-requests", "1",
                                     "--min-payback-percent", "99"], wait_reviews=False)
        seed = [{"role": "user", "content": "goal"},
                {"role": "assistant", "content": "old"}]
        self.assertEqual(proxy.send(seed)[0], 200)
        self.assertEqual(proxy.send(seed)[0], 200)
        saved = self.wait_saved(proxy, lambda s: s["shadow"]["calls"] == 1)
        self.assertEqual((proxy.advice() or {}).get("opinions", {}).get("2"), "drop", "setup requires actual accepted old advice")
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
        facts = [{"source_ids": [2], "text": text_a},
                 {"source_ids": [2], "text": text_b}]

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
        accepted = len((proxy.advice() or {}).get("memories", []))
        feedback = (proxy.advice() or {}).get("feedback", saved["review_feedback"])
        if accepted < 2:
            self.assertIsInstance(feedback, dict,
                "completed known-source replacement rejection must retain H-scoped feedback and source protection")
            self.assertGreater(feedback["rejected_count"], 0,
                               "budget admission may reject facts only with explicit feedback")
        self.evidence.update(limit=limit, accepted_memories=accepted,
                             feedback=feedback)
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
        self.assertEqual((proxy.advice() or {}).get("memories", []), [], "in-budget advice retires accepted memories")
        self.assertNotIn(text_a, json.dumps(fixture.reviews[-1]))
        self.assertNotIn(text_b, json.dumps(fixture.reviews[-1]))

    def test_small_memory_budget_rejects_large_fact_preserves_small_sibling_and_opinions(self):
        large = ("a0b1c2d3e4f5g6h7i8j9" * 205)[:4096]
        small = "The useful result is 42."

        def review(body, n):
            data = ledger(body)
            advice = {"protected": [1], "removable": [2], "memories": [
                {"source_ids": [2], "text": large},
                {"source_ids": [2], "text": small}]} if n == 1 else {
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
        self.assertEqual([m["text"] for m in (proxy.advice() or {}).get("memories", [])], [small])
        self.assertEqual((proxy.advice() or {}).get("opinions", {}).get("1"), "keep")
        self.assertEqual((proxy.advice() or {}).get("opinions", {}).get("2"), "keep",
                         "a small sibling does not prove it replaces the rejected fact")
        self.assertEqual(proxy.advice()["feedback"]["rejected_count"], 1)
        self.assertEqual(proxy.advice()["feedback"]["rejected"][0]["index"], 0)
        proxy.close()
        proxy = self.proxy(fixture, options)
        # A next-request plan may first prune source/control history. The next
        # incremental update can then retire the shared fact within the budget.
        for _ in range(2):
            self.assertEqual(proxy.send(seed)[0], 200)
            if len(fixture.reviews) >= 2:
                break
        self.assertGreaterEqual(len(fixture.reviews), 2)
        self.assertEqual(proxy.advice()["memories"], [])
        self.evidence.update(reviewer_requests=fixture.reviews,
                             reviewer_limits={"bytes": 10000, "tokens": 4000})

    def test_small_byte_budget_memory_admission_preserves_review_progress(self):
        self.memory_progress("bytes")

    def test_small_token_budget_memory_admission_preserves_review_progress(self):
        self.memory_progress("tokens")


if __name__ == "__main__":
    unittest.main()
