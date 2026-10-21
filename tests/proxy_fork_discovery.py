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

    def request(self, proxy, body, headers=None, raw=None):
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
            connection.request("POST", self.endpoint, body=wire(body) if raw is None else raw,
                               headers=identity)
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
                for value in (None, "", "x" * 257):
                    self.assertEqual(self.request(proxy, body, {key: value})[0], 400)
        for auth in (None, "Bearer wrong"):
            self.assertEqual(self.request(proxy, body, {"Authorization": auth})[0], 401)
        self.assertEqual(self.request(proxy, body, {"Origin": "http://localhost"})[0], 403)
        for key in ("x-carry-fork-parent", "x-carry-fork-point"):
            self.assertEqual(self.request(proxy, body, {key: "other"})[0], 400)
        for invalid in ([], {}, {**body, "input": "text"}, {**body, "model": None},
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


if __name__ == "__main__":
    unittest.main()
