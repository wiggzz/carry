"""Current history through real explicit and canonical-index Session loaders.

Synthetic native totals, loopback-only providers and private state. Explicitly
expired dispatch evidence cannot gain warmth from restart or dashboard polling.
"""
import copy
import http.client
from history_store import read_index, write_index
import json
import unittest

import proxy_state_regressions as support


def expire_dispatches(target):
    """Age real synthetic dispatches together, not a removed prediction cache.

    Relative chronology, native usage, frozen rates and settled money are intact.
    Replaying old requests is identical; the next real dispatch is cold once.
    """
    for observation in target["request_evidence"]["observations"]:
        if "Primary" in observation:
            observation["Primary"]["facts"]["at"] -= 7200
    return target


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


class LegacyHistoryLoads(support.FixtureCase):
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

        fixture = self.fixture(primary, review)
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))

        def start(review_enabled=False):
            proxy = OwnerProxy(self.state, fixture, shared=shared, mode="audit",
                               extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS":
                                          "0" if review_enabled else "1000000"})
            self.stack.callback(proxy.close)
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
        self.assertEqual(saved["history_shared"], shared)
        self.assertEqual(saved["primary_input"]["anchor"]["input"], 300300)
        self.assertEqual(before.get("initial_compactions", 0), int(initialized))
        self.assertEqual(before["traditional_compactions"], int(not initialized))
        self.assertEqual(before["summary_cost"] > 0, not initialized)
        self.assertGreater(before["observed_reviewer_cost"], 0)
        self.assertEqual(len(fixture.reviews), 1)
        self.assertEqual(saved["compactions"], 0)
        self.assertEqual(before["excluded_unpriced_calls"], 0)
        paid_before = sum(n * (4 if n > 272000 else 2) / 1e6 for n in inputs[:seed_count])
        self.assertAlmostEqual(before["observed_primary_cost"], paid_before, places=12)

        # Keep the current evidence and age its dispatch clock to exercise
        # cold-once/rewarm behavior without injecting prediction state.
        # Shared snapshots, not mirrors, are the authoritative load target.
        checkpoint = next(self.state.glob("*.json"))
        owner = checkpoint.stem
        staged = expire_dispatches(copy.deepcopy(saved))
        checkpoint.write_text(json.dumps(staged))
        index_path = self.state / "history.index"
        index = read_index(index_path)
        if shared:
            self.assertIn(owner, index["snapshots"])
            index["snapshots"][owner] = expire_dispatches(index["snapshots"][owner])
            self.assertEqual(index["snapshots"][owner]["request_evidence"], staged["request_evidence"])
            write_index(index_path, index)
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
            with self.subTest(step=step, shared=shared):
                self.assertEqual(after.get("initial_compactions", 0), int(initialized),
                                 "CURRENT_SESSION_HISTORY: load must not grant another initialization")
                self.assertEqual(after["traditional_compactions"], int(not initialized))
                self.assertEqual(after["summary_cost"], before["summary_cost"])
                incoming = 21400 + step * 100
                # Complete history proof is not cache-write evidence. The
                # first eventless continuation must be cold; the next
                # has a genuinely new modeled event from that continuation.
                expected_comparison += (incoming * 2.5 if step == 0 else
                                        21400 * .1 + 100 * 2.5) / 1e6
                self.assertEqual(state["primary_input"]["anchor"]["input"], inputs[seed_count + step])
                old = staged["request_evidence"]["observations"]
                self.assertEqual(state["request_evidence"]["observations"][:len(old)], old)
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
