"""Neutral retained-cut matching over real HTTP; no native adapter claims."""
import copy
import hashlib
import http.client
import json
import threading
import time
import unittest

from proxy_fork_continuity import BranchProxy, ForkContinuity
from proxy_state_regressions import StateRegressions, advice_response, completed, wire


class ForkDiscovery(unittest.TestCase):
    setUp = StateRegressions.setUp
    fixture = StateRegressions.fixture
    record_evidence = StateRegressions.record_evidence
    seed = ForkContinuity.seed
    token = "synthetic-discovery-token"
    endpoint = "/carry/fork-points/match"

    def request(self, proxy, body, headers=None, raw=None, path=None):
        identity = {"x-carry-tenant": "local", "x-carry-session": "state-regression",
                    "x-carry-branch": "main", "Authorization": "Bearer " + self.token,
                    "Content-Type": "application/json"}
        for key, value in (headers or {}).items():
            if value is None:
                identity.pop(key, None)
            else:
                identity[key] = value
        connection = http.client.HTTPConnection(*proxy.address, timeout=5)
        try:
            payload = wire(body) if raw is None else raw
            connection.putrequest("POST", path or self.endpoint)
            for key, value in identity.items():
                for entry in value if isinstance(value, tuple) else (value,):
                    connection.putheader(key, entry)
            connection.putheader("Content-Length", str(len(payload)))
            connection.endheaders(payload)
            response = connection.getresponse()
            data = response.read()
            return response.status, json.loads(data) if data else {}
        finally:
            connection.close()

    def restart(self, proxy, fixture, options, env=None):
        proxy.close()
        proxy = BranchProxy(self.state, fixture, options,
                            {"CARRY_PROXY_AUTH_TOKEN": self.token, **(env or {})})
        self.stack.callback(proxy.close)
        return proxy

    def snapshot(self):
        return {p.name: p.read_bytes() for p in self.state.iterdir()
                if p.suffix in (".json", ".tmp")}

    def test_committed_matching_is_content_free_scoped_and_read_only(self):
        history = [{"role": "user", "content": "PROTECTED_GOAL"},
                   {"role": "assistant", "content": "RAW_A " + "old payload " * 3000,
                    "opaque_fixture": {"fraction": 1.0, "exponent": 1e-7,
                                       "negative_zero": -0.0, "large": 9007199254740993,
                                       "text": "quote\" slash\\ newline\n snowman ☃ 漢字"}},
                   {"role": "user", "content": "A_CUT"}]
        proxy, fixture, history, _fixture_handle, options = self.seed(history=history)
        # The authority is an actually committed reviewer result, NOT cut() or
        # a staged accepted checkpoint. Python/JS numeric hashes are irrelevant.
        points = proxy.saved()["fork_points"]
        self.assertEqual(len(points), 1)
        handle = points[0]["id"]
        proxy = self.restart(proxy, fixture, options)
        body = {"model": "gpt-6-luna", "input": history + [{"role": "user", "content": "C"}]}
        before, counts = self.snapshot(), (len(fixture.primaries), len(fixture.reviews))
        status, result = self.request(proxy, body)
        self.assertEqual(status, 200, "LINEAGE_DISCOVERY_RED: committed cut must be remotely matchable")
        self.assertEqual(result["schema_version"], 1)
        self.assertEqual(result["outcome"], "matched")
        self.assertEqual(result["matching"], "ordered-prefix-cohort-scope-v1")
        self.assertEqual(result["horizon"], {"publication": "first-fully-accepted-raw-cut-v1",
                         "max_age_secs": 86400, "max_nodes": 8, "max_bytes": 262144})
        self.assertEqual(len(result["points"]), 1)
        point = result["points"][0]
        self.assertEqual(set(point), {"id", "input_items", "expires_at"})
        self.assertEqual(point["id"], handle)
        self.assertEqual(point["input_items"], len(history))
        self.assertIsInstance(point["expires_at"], int)
        self.assertGreater(point["expires_at"], time.time())
        self.assertEqual(set(result), {"schema_version", "outcome", "matching", "horizon", "points"})
        encoded = json.dumps(result)
        for private in ("RAW_A", "A_FACT", "PROTECTED_GOAL", "sha256", "opinions", "memories",
                        "source_ids", str(self.state), "fixture-primary", "fixture-shadow"):
            self.assertNotIn(private, encoded)
        # Equivalent numeric spellings, key order, escaping and Unicode cross
        # the actual HTTP/serde boundary. Never hash them client-side.
        reordered = json.dumps(body, ensure_ascii=True, sort_keys=True).replace('1.0', '1e0')
        self.assertEqual(self.request(proxy, body, raw=reordered.encode())[1], result)
        self.assertEqual(self.request(proxy, {**body, "prompt_cache_key": "child-affinity"})[1], result)
        for key in ("x-carry-tenant", "x-carry-session", "x-carry-branch"):
            with self.subTest(namespace=key):
                status, empty = self.request(proxy, body, {key: "other"})
                self.assertEqual(status, 200)
                self.assertEqual(empty["outcome"], "no_match")
                self.assertEqual(empty["points"], [])
                for value in (None, "", "x" * 257, b"\xff", ("main", "other")):
                    self.assertEqual(self.request(proxy, body, {key: value})[0], 400)
        for auth in (None, "Bearer wrong"):
            self.assertEqual(self.request(proxy, body, {"Authorization": auth})[0], 401)
        self.assertEqual(self.request(proxy, body, {"Origin": "http://localhost"})[0], 403)
        for key in ("x-carry-fork-parent", "x-carry-fork-point"):
            self.assertEqual(self.request(proxy, body, {key: "other"})[0], 400)
        for invalid in ([], {}, {**body, "input": "text"}, {**body, "input": [None]}, {**body, "model": None},
                        {**body, "previous_response_id": "resp_old"}, {**body, "background": True},
                        {**body, "conversation": "remote"}):
            self.assertEqual(self.request(proxy, invalid)[0], 400)
        self.assertEqual(self.request(proxy, body, raw=b'{broken')[0], 400)
        for change in ({"instructions": "different"}, {"model": "different"},
                       {"tools": []}, {"input": history[:1]},
                       {"input": [history[0], {**history[1], "content": "different"}, history[2]]},
                       {"input": list(reversed(history))}):
            status, empty = self.request(proxy, {**body, **change})
            self.assertEqual(status, 200)
            self.assertEqual(empty["points"], [])
            self.assertEqual(empty["outcome"], "no_match")
        # Malformed decoys and orphan temporary publication never participate.
        (self.state / "decoy.json").write_text("malformed unrelated checkpoint")
        unknown_id = hashlib.sha256(wire(["local", "state-regression", "orphan"])).hexdigest()
        (self.state / ("." + unknown_id + ".tmp")).write_bytes(next(iter(before.values())))
        self.assertEqual(self.request(proxy, body, {"x-carry-branch": "orphan"})[1]["points"], [])
        after_decoys = self.snapshot()
        self.assertEqual(self.request(proxy, body)[1], result)
        self.assertEqual(self.snapshot(), after_decoys)
        for name, data in before.items():
            self.assertEqual(self.snapshot()[name], data, "discovery must not rewrite saved bytes")
        self.assertEqual((len(fixture.primaries), len(fixture.reviews)), counts)
        proxy = self.restart(proxy, fixture, options)
        self.assertEqual(self.request(proxy, body)[1], result, "same committed handle across restart")
        proxy = self.restart(proxy, fixture, options + ["--classifier-reasoning-effort", "high"])
        self.assertEqual(self.request(proxy, body)[1]["points"], [])
        self.assertEqual(self.snapshot(), after_decoys)
        self.assertEqual((len(fixture.primaries), len(fixture.reviews)), counts)


    def test_retention_is_read_only_and_unavailable_never_becomes_fresh(self):
        proxy, fixture, history, _handle, options = self.seed()
        # A second genuinely reviewed raw epoch is not a staged authority.
        proxy.history_policy = "reset-on-divergence"
        changed = copy.deepcopy(history)
        changed[0]["content"] = "SECOND_EPOCH_GOAL"
        self.assertEqual(proxy.send(changed)[0], 200)
        proxy = self.restart(proxy, fixture, options, {"CARRY_PROXY_FORK_MAX_NODES": "1"})
        before, counts = self.snapshot(), (len(fixture.primaries), len(fixture.reviews))
        for items, expected in ((history, "no_match"), (changed, "matched")):
            status, result = self.request(proxy, {"model": "gpt-6-luna", "input": items})
            self.assertEqual(status, 200)
            self.assertEqual(result["outcome"], expected)
            self.assertEqual(result["horizon"]["max_nodes"], 1)
        self.assertEqual(self.snapshot(), before, "read-time node eviction is local only")
        proxy = self.restart(proxy, fixture, options, {"CARRY_PROXY_FORK_MAX_BYTES": "2"})
        self.assertEqual(self.request(proxy, {"model": "gpt-6-luna", "input": changed})[1]["points"], [])
        self.assertEqual(self.snapshot(), before)
        proxy = self.restart(proxy, fixture, options, {"CARRY_PROXY_FORK_MAX_AGE_SECS": "1"})
        time.sleep(2.05)
        self.assertEqual(self.request(proxy, {"model": "gpt-6-luna", "input": changed})[1]["points"], [])
        self.assertEqual(self.snapshot(), before, "expiry cannot rewrite checkpoint bytes")
        self.assertEqual((len(fixture.primaries), len(fixture.reviews)), counts)

    def test_matching_reuses_hydration_cohort_rules_and_returned_handle(self):
        history = [{"role": "user", "content": "PROTECTED_GOAL"},
                   {"role": "assistant", "content": "RAW_A " + "old payload " * 3000},
                   {"type": "function_call", "call_id": "a1", "name": "one", "arguments": "{}"},
                   {"type": "function_call_output", "call_id": "a1", "output": "OLD_RESULT"},
                   {"role": "user", "content": "A_CUT"}]
        proxy, fixture, history, _handle, options = self.seed(history=history)
        proxy = self.restart(proxy, fixture, options)
        suffix = [{"role": "user", "content": "CHILD_C"}]
        body = {"model": "gpt-6-luna", "input": history + suffix}
        status, matched = self.request(proxy, body)
        self.assertEqual(status, 200)
        self.assertEqual(matched["outcome"], "matched")
        before, counts = self.snapshot(), (len(fixture.primaries), len(fixture.reviews))
        incompatible = {**body, "input": history + [
            {"type": "function_call_output", "call_id": "a1", "output": "LATER_RESULT"}] + suffix}
        self.assertEqual(self.request(proxy, incompatible)[1]["points"], [])
        self.assertEqual(self.snapshot(), before)
        self.assertEqual((len(fixture.primaries), len(fixture.reviews)), counts)
        # Consume the server-returned handle through the existing HTTP fork
        # contract; no native-client acceptance is implied.
        status, result = self.request(proxy, body, {"x-carry-branch": "matched-child",
            "x-carry-fork-parent": "main", "x-carry-fork-point": matched["points"][0]["id"]},
            path="/v1/responses")
        self.assertEqual(status, 200, result)
        self.assertNotIn("RAW_A", json.dumps(fixture.primaries[-1]))
        self.assertIn("A_FACT", json.dumps(fixture.primaries[-1]))
        self.assertEqual(len(fixture.reviews), counts[1])

    def test_held_unrelated_primary_lock_and_review_do_not_block_matching(self):
        proxy, fixture, history, _handle, options = self.seed()
        proxy = self.restart(proxy, fixture, options)
        body = {"model": "gpt-6-luna", "input": history}
        expected = self.request(proxy, body)[1]
        self.assertEqual(expected["outcome"], "matched")
        review_held, review_release = threading.Event(), threading.Event()
        primary_held, primary_release = threading.Event(), threading.Event()
        def review(_body, _n):
            review_held.set()
            review_release.wait(15)
            return 200, advice_response({"protected": [], "removable": [], "memories": []})
        def primary(request, _n):
            if "HOLD_PRIMARY" in json.dumps(request):
                primary_held.set()
                primary_release.wait(15)
            return 200, completed()
        fixture.review, fixture.primary = review, primary
        self.stack.callback(review_release.set)
        self.stack.callback(primary_release.set)
        status, _ = self.request(proxy, body, {"x-carry-branch": "held-review"}, path="/v1/responses")
        self.assertEqual(status, 200)
        self.assertTrue(review_held.wait(3))
        responses, errors = [], []
        def hold_primary():
            try:
                responses.append(self.request(proxy,
                    {"model": "gpt-6-luna", "input": [{"role": "user", "content": "HOLD_PRIMARY"}]},
                    {"x-carry-branch": "held-primary"}, path="/v1/responses"))
            except Exception as error:
                errors.append(repr(error))
        thread = threading.Thread(target=hold_primary, daemon=True)
        thread.start()
        try:
            self.assertTrue(primary_held.wait(3))
            before, counts = self.snapshot(), (len(fixture.primaries), len(fixture.reviews))
            started = time.monotonic()
            self.assertEqual(self.request(proxy, body)[1], expected)
            self.assertLess(time.monotonic() - started, 3)
            self.assertFalse(review_release.is_set())
            self.assertFalse(primary_release.is_set())
            self.assertEqual(self.snapshot(), before)
            self.assertEqual((len(fixture.primaries), len(fixture.reviews)), counts)
            self.assertEqual(self.request(proxy, body, {"x-carry-branch": "held-review"})[1]["points"], [],
                             "uncommitted review is not discovery authority")
        finally:
            primary_release.set()
            review_release.set()
            thread.join(5)
        self.assertFalse(errors)
        self.assertTrue(responses)
        self.assertEqual(responses[0][0], 200)

    def test_no_gateway_token_and_corrupt_exact_checkpoint_fail_closed(self):
        proxy, fixture, history, _handle, options = self.seed()
        body = {"model": "gpt-6-luna", "input": history}
        self.assertEqual(self.request(proxy, body)[0], 401,
                         "discovery requires configured gateway auth even on loopback")
        proxy = self.restart(proxy, fixture, options)
        before = self.snapshot()
        path = next(self.state.glob("*.json"))
        path.write_text("malformed exact checkpoint")
        corrupt = self.snapshot()
        counts = (len(fixture.primaries), len(fixture.reviews))
        status, result = self.request(proxy, body)
        self.assertEqual(status, 500)
        self.assertNotIn(str(self.state), json.dumps(result))
        self.assertEqual(self.snapshot(), corrupt)
        path.write_bytes(before[path.name])
        self.assertEqual(self.request(proxy, body)[1]["outcome"], "matched")
        self.assertEqual((len(fixture.primaries), len(fixture.reviews)), counts)
        proxy.close()
        for mode in ("audit", "off"):
            proxy = BranchProxy(self.state, fixture, options,
                                {"CARRY_PROXY_AUTH_TOKEN": self.token}, mode=mode)
            self.stack.callback(proxy.close)
            status, result = self.request(proxy, body)
            self.assertEqual(status, 409)
            self.assertEqual(result["error"]["code"], "fork_unsupported")
            proxy.close()
        self.assertEqual(self.snapshot(), before)


if __name__ == "__main__":
    unittest.main()
