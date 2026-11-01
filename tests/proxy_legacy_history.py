"""Current history through real explicit and canonical-index Session loaders.

Synthetic native totals, loopback-only providers and private state. Explicitly
absent modeled write events cannot gain warmth from an observation timestamp.
"""
import copy
import http.client
import json
import unittest

import proxy_state_regressions as support


class OwnerProxy(support.Proxy):
    def __init__(self, *args, shared=False, **kwargs):
        self.shared = shared
        super().__init__(*args, **kwargs)

    def request(self, method, path, body=None):
        connection = http.client.HTTPConnection(*self.address, timeout=30)
        headers = {"Content-Type": "application/json"}
        if not self.shared:
            headers["x-carry-session"] = "state-regression"
        try:
            connection.request(method, path,
                               body=None if body is None else support.wire(body),
                               headers=headers)
            response = connection.getresponse()
            return response.status, json.loads(response.read())
        finally:
            connection.close()


class LegacyHistoryLoads(unittest.TestCase):
    def setUp(self):
        self.harness = support.StateRegressions()
        self.harness.setUp()
        self.harness._testMethodName = self._testMethodName
        self.evidence = self.harness.evidence
        self.addCleanup(lambda: self.assertTrue(self.harness.doCleanups(),
                                               "nested fixture cleanup failed"))

    def lifecycle(self, *, shared, initialized):
        inputs = ([] if initialized else [10000]) + [300000, 300100, 300200, 300300,
                                                              300400, 300500]
        seed_count = len(inputs) - 2

        def primary(_body, n):
            response = support.completed(response_id=f"legacy-session-{n}")
            response["usage"] = {"input_tokens": inputs[n - 1], "output_tokens": 0,
                                 "input_tokens_details": {"cached_tokens": 0,
                                                          "cache_write_tokens": 0}}
            return 200, response

        def review(_body, _n):
            response = support.advice_response({"protected": [], "removable": [], "memories": []})
            response["usage"] = {"input_tokens": 2000, "output_tokens": 7,
                                 "input_tokens_details": {"cached_tokens": 1000,
                                                          "cache_write_tokens": 0}}
            return 200, response

        fixture = self.harness.fixture(primary, review)
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))

        def start(review_enabled=False):
            proxy = OwnerProxy(self.harness.state, fixture, shared=shared, mode="audit",
                               extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS":
                                          "0" if review_enabled else "1000000"})
            self.harness.stack.callback(proxy.close)
            return proxy

        def snapshot(proxy):
            status, stats = proxy.request("GET", "/carry/dashboard/stats")
            self.assertEqual(status, 200)
            self.assertEqual(len(stats["sessions"]), 1)
            return copy.deepcopy(stats["sessions"][0]["cost_estimate"])

        history = [{"role": "assistant", "content": "owned older history"},
                   {"role": "user", "content": "current request"}]
        wires = []
        for index in range(seed_count):
            if index:
                history.append({"role": "user", "content": f"seed growth {index}"})
            proxy = start(review_enabled=index == seed_count - 1)
            self.assertEqual(proxy.send(history, model="gpt-6.1-sol", max_output_tokens=1)[0], 200)
            wires.append(copy.deepcopy(history))
            before = snapshot(proxy)
            saved = proxy.saved()
            proxy.close()
        trajectory = saved["cost_trajectory"]
        self.assertEqual(saved["history_shared"], shared)
        self.assertEqual(trajectory["retired"], 280000)
        self.assertEqual(trajectory["summary"], 1000)
        self.assertEqual(trajectory["last_traditional_input"], 21300)
        self.assertEqual(before.get("initial_compactions", 0), int(initialized))
        self.assertEqual(before["traditional_compactions"], int(not initialized))
        self.assertEqual(before["summary_cost"] > 0, not initialized)
        self.assertGreater(before["observed_reviewer_cost"], 0)
        self.assertEqual(len(fixture.reviews), 1)
        self.assertEqual(saved["compactions"], 0)
        self.assertEqual(before["excluded_unpriced_calls"], 0)
        paid_before = sum(n * (4 if n > 272000 else 2) / 1e6 for n in inputs[:seed_count])
        self.assertAlmostEqual(before["observed_primary_cost"], paid_before, places=12)

        # Keep the current schema and exact cursor. Explicitly discard only
        # hypothetical write evidence to exercise cold-once/rewarm behavior.
        # Shared snapshots, not mirrors, are the authoritative load target.
        checkpoint = next(self.harness.state.glob("*.json"))
        owner = checkpoint.stem
        def without_cache(target):
            target["cost_trajectory"]["receipts"].pop("modeled_cache", None)
            return target
        staged = without_cache(copy.deepcopy(saved))
        checkpoint.write_text(json.dumps(staged))
        index_path = self.harness.state / "history.index"
        index = json.loads(index_path.read_text())
        if shared:
            self.assertIn(owner, index["snapshots"])
            index["snapshots"][owner] = without_cache(index["snapshots"][owner])
            self.assertEqual(index["snapshots"][owner]["cost_trajectory"], staged["cost_trajectory"])
            index_path.write_text(json.dumps(index))
        self.evidence.update(shared=shared, initialized=initialized,
                             before=before, staged=staged, owner=owner,
                             staged_index=index if shared else None)

        snapshots, states = [], []
        paid = paid_before
        for step in range(2):
            # Reading the dashboard exercises the actual loader before any new
            # paid request. A second independent process repeats that load.
            proxy = start()
            loaded = snapshot(proxy)
            for key in ("observed_primary_cost", "observed_reviewer_cost", "carry_cost",
                        "traditional_cost", "summary_cost", "initial_compactions",
                        "traditional_compactions", "cache_policy_boundaries"):
                expected = snapshots[-1] if snapshots else before
                self.assertEqual(loaded.get(key), expected.get(key), f"load changed {key}")
            history.append({"role": "user", "content": f"after restarted load {step}"})
            self.assertEqual(proxy.send(history, model="gpt-6.1-sol", max_output_tokens=1)[0], 200)
            wires.append(copy.deepcopy(history))
            after = snapshot(proxy)
            state = proxy.saved()
            proxy.close()
            paid += inputs[seed_count + step] * 4 / 1e6
            # Grade measured money and complete real wire/call counts before
            # legacy-comparison assertions; unknown comparison is not zero spend.
            self.assertAlmostEqual(after["observed_primary_cost"], paid, places=12)
            self.assertEqual(after["observed_reviewer_cost"], before["observed_reviewer_cost"])
            self.assertEqual(after["reviewer_cost"], before["reviewer_cost"])
            self.assertEqual(after["unavailable_primary_cost_calls"], 0)
            self.assertEqual(after["coverage"]["primary_responses"], seed_count + step + 1)
            self.assertEqual(state["compactions"], 0)
            self.assertEqual(after.get("cache_policy_boundaries", []),
                             before.get("cache_policy_boundaries", []))
            self.assertEqual(fixture.primaries[-1]["input"], history)
            self.assertEqual(fixture.primaries[-1]["max_output_tokens"], 1)
            snapshots.append(after)
            states.append(state)
        self.evidence.update(snapshots=snapshots, checkpoints=states,
                             primaries=fixture.primaries, reviews=fixture.reviews)
        self.assertEqual([b["input"] for b in fixture.primaries], wires)
        self.assertEqual(len(fixture.primaries), len(inputs))
        self.assertEqual(len(fixture.reviews), 1)
        self.assertEqual(fixture.errors, [])

        expected_comparison = before["traditional_cost"]
        for step, (after, state) in enumerate(zip(snapshots, states)):
            t = state["cost_trajectory"]
            with self.subTest(step=step, shared=shared):
                self.assertEqual(after.get("initial_compactions", 0), int(initialized),
                                 "CURRENT_SESSION_HISTORY: load must not grant another initialization")
                self.assertEqual(after["traditional_compactions"], int(not initialized))
                self.assertEqual(after["summary_cost"], before["summary_cost"])
                self.assertEqual((t["retired"], t["summary"]), (280000, 1000))
                incoming = 21400 + step * 100
                # Complete history proof is not cache-write evidence. The
                # first eventless continuation must be cold; the next
                # has a genuinely new modeled event from that continuation.
                expected_comparison += (incoming * 2.5 if step == 0 else
                                        21400 * .1 + 100 * 2.5) / 1e6
                self.assertEqual(t["last_traditional_input"], incoming)
                self.assertEqual(after["matched_primary_responses"],
                                 before["matched_primary_responses"] + step + 1)
                self.assertEqual(after["excluded_unpriced_calls"], 0)
                self.assertAlmostEqual(after["traditional_cost"], expected_comparison, places=12,
                                       msg="CURRENT_SESSION_CACHE: no write may be inferred from old observation time")


def install_cases():
    for shared in (False, True):
        for initialized in (False, True):
            def case(self, shared=shared, initialized=initialized):
                self.lifecycle(shared=shared, initialized=initialized)
            owner = "indexed" if shared else "explicit"
            history = "free" if initialized else "paid"
            setattr(LegacyHistoryLoads, f"test_{owner}_{history}_current_eventless_restart", case)


install_cases()
if __name__ == "__main__":
    unittest.main(verbosity=2)
