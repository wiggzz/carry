"""Cache partition aliases through actual JSON/SSE terminal commits and restart.

Synthetic loopback only. Capability/rates are a fixture attestation after the
first real dispatch, never injected positive cache evidence. Cache facts must
survive unavailable money; contradictory partitions must not create write proof.
"""
import copy
import http.client
import json
import unittest

import proxy_state_regressions as support


def usage_for(case):
    usage = {"input_tokens": 2000, "output_tokens": 10,
             "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 2000}}
    details = usage["input_tokens_details"]
    if case == "conflicting_write":
        details["cache_creation_tokens"] = 1
    elif case == "conflicting_read":
        usage["cached_input_tokens"] = 1
    elif case == "malformed_write":
        usage["cache_creation_input_tokens"] = "2000"
    elif case in ("agreeing", "unpriced"):
        details["cache_creation_tokens"] = 2000
        usage.update(cached_input_tokens=0, cache_write_input_tokens=2000,
                     cache_creation_input_tokens=2000)
    elif case == "alternate_nested":
        details["cache_creation_tokens"] = details.pop("cache_write_tokens")
    elif case == "alternate_top":
        usage["cached_input_tokens"] = details.pop("cached_tokens")
        usage["cache_creation_input_tokens"] = details.pop("cache_write_tokens")
    else:
        assert case == "canonical", case
    return usage


class CachePartitions(unittest.TestCase):
    def setUp(self):
        self.harness = support.StateRegressions()
        self.harness.setUp()
        self.harness._testMethodName = self._testMethodName
        self.evidence = self.harness.evidence
        self.addCleanup(lambda: self.assertTrue(self.harness.doCleanups(),
                                               "nested fixture cleanup failed"))

    def lifecycle(self, *, case, sse):
        valid = case not in ("conflicting_write", "conflicting_read", "malformed_write")
        scripted = [
            {"input_tokens": 2000, "output_tokens": 10,
             "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0}},
            usage_for(case),
            {"input_tokens": 2000, "output_tokens": 10,
             "input_tokens_details": {"cached_tokens": 2000, "cache_write_tokens": 0}},
        ]

        def primary(_body, n):
            result = support.completed(response_id=f"cache-alias-{n}")
            result["usage"] = copy.deepcopy(scripted[n - 1])
            if sse:
                return 200, b"data: " + support.wire({"type": "response.completed",
                                                       "response": result}) + b"\n\n"
            return 200, result

        def review(*_):
            raise AssertionError("partition tracer must not dispatch reviewer work")

        fixture = self.harness.fixture(primary, review)
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        history = [{"role": "user", "content": "synthetic alias prefix " * 2000}]
        snapshots, states, estimates = [], [], []
        self.evidence.update(case=case, sse=sse, scripted_usage=scripted,
                             snapshots=snapshots, states=states, estimates=estimates,
                             primaries=fixture.primaries, reviews=fixture.reviews,
                             fixture_errors=fixture.errors)
        for step in range(3):
            proxy = support.Proxy(self.harness.state, fixture, mode="audit",
                extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS": "1000000"})
            self.harness.stack.callback(proxy.close)
            if sse:
                connection = http.client.HTTPConnection(*proxy.address, timeout=30)
                try:
                    connection.request("POST", "/v1/responses", body=support.wire({
                        "model": "gpt-6.1-sol", "input": history, "stream": True}),
                        headers={"Content-Type": "application/json", "x-carry-session": "state-regression"})
                    response = connection.getresponse()
                    raw = response.read()
                    self.assertEqual(response.status, 200)
                    self.assertIn(f'"cache-alias-{step + 1}"'.encode(), raw)
                    self.assertIn(b'"response.completed"', raw)
                finally:
                    connection.close()
            else:
                self.assertEqual(proxy.send(history, model="gpt-6.1-sol")[0], 200)
            saved = proxy.saved()
            records = json.loads((self.harness.state / "history.index").read_text())["upstream_cache"]
            status, dashboard = proxy.request("GET", "/carry/dashboard/stats")
            self.assertEqual(status, 200)
            self.assertEqual(len(dashboard["sessions"]), 1)
            estimate = dashboard["sessions"][0]["cost_estimate"]
            snapshots.append(copy.deepcopy(records))
            states.append(copy.deepcopy(saved))
            estimates.append(copy.deepcopy(estimate))
            self.assertEqual(fixture.primaries[-1]["input"], history)
            self.assertEqual(saved["completed_requests"], step + 1)
            self.assertEqual(saved["primary"]["calls"], step + 1)
            self.assertEqual(saved["primary"]["input_tokens"], (step + 1) * 2000)
            self.assertEqual(saved["shadow"]["calls"], 0)
            self.assertEqual(saved["compactions"], 0)
            if step == 0:
                self.assertEqual(records, [], "unknown loopback route has no cache entitlement")
                self.assertAlmostEqual(saved["primary"]["cost_usd"], .0041, places=12)
            proxy.close()
            if step == 0:
                checkpoint = next(self.harness.state.glob("*.json"))
                staged = json.loads(checkpoint.read_text())
                resolved = staged["model_resolution"]["primary"]
                resolved["cache"] = {"minimum_prefix_tokens": 1024, "max_read_breakpoints": 50,
                                     "max_write_breakpoints": 4, "implicit_breakpoint_uses_write_slot": True}
                if case == "unpriced":
                    resolved["rates"] = None
                checkpoint.write_text(json.dumps(staged))

        self.assertEqual(fixture.errors, [])
        self.assertEqual(fixture.reviews, [])
        self.assertEqual(len(fixture.primaries), 3)
        self.assertTrue(all(wire == fixture.primaries[0] for wire in fixture.primaries))
        # Grade each complete lifecycle independently: an alias settlement
        # defect must not hide the cache proof assertions on that same wire.
        with self.subTest(stage="frozen_money_and_usage"):
            if valid and case != "unpriced":
                self.assertAlmostEqual(states[-1]["primary"]["cost_usd"], .0095, places=12,
                                       msg="CACHE_PARTITION_SETTLEMENT")
                self.assertEqual(estimates[-1]["unavailable_primary_cost_calls"], 0)
            elif case == "unpriced":
                self.assertIsNone(states[-1]["model_resolution"]["primary"]["rates"])
                self.assertEqual(states[-1]["primary"]["unavailable_cost_calls"], 2)
                self.assertAlmostEqual(states[-1]["primary"]["cost_usd"], .0041, places=12)
            else:
                self.assertEqual(estimates[1]["unavailable_primary_cost_calls"], 1)
                self.assertEqual(states[1]["primary"]["unavailable_cost_calls"], 1,
                                 "CACHE_PARTITION_SETTLEMENT: contradictory aliases cannot mint money")
                self.assertAlmostEqual(states[-1]["primary"]["cost_usd"], .0044, places=12)
        for step in (1, 2):
            with self.subTest(stage="write_then_read_restart", step=step):
                windows = [record for record in snapshots[step] if record.get("write_window")]
                self.assertEqual(len(windows), int(valid),
                    "CACHE_PARTITION_PRODUCER: only an agreed whole write supplies reusable evidence")
                if valid:
                    self.assertEqual(windows[0]["native_input_tokens"], 2000)
                    self.assertEqual(windows[0]["native_write_tokens"], 2000)
                    self.assertEqual(windows[0]["native_cached_fraction"], 0)
                    self.assertEqual(windows[0]["write_window"]["lifetime"], 1800)
                    if step == 2:
                        self.assertIn(windows[0], snapshots[1], "a later read must not rewrite the original fact")


def install_cases():
    for case in ("canonical", "conflicting_write", "conflicting_read", "malformed_write",
                 "agreeing", "alternate_nested", "alternate_top", "unpriced"):
        for sse in (False, True):
            def test(self, case=case, sse=sse):
                self.lifecycle(case=case, sse=sse)
            setattr(CachePartitions, f"test_{case}_{'sse' if sse else 'json'}_restart", test)


install_cases()
if __name__ == "__main__":
    unittest.main(verbosity=2)
