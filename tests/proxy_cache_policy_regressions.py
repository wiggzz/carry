"""Immutable comparison-policy boundaries through the real CLI and restart.

The future version numbers below are owned checkpoint fixtures, not claims that
write-pricing v2/v3 is implemented. Rates and receipts stay fixed; the test
isolates migration from the still-failing #188 write-pricing controls.
"""
import copy
import json
import unittest

import proxy_state_regressions as support


class CachePolicyRegressions(unittest.TestCase):
    def setUp(self):
        self.harness = support.StateRegressions()
        self.harness.setUp()
        self.harness._testMethodName = self._testMethodName
        self.evidence = self.harness.evidence
        self.addCleanup(self.cleanup_harness)

    def cleanup_harness(self):
        self.assertTrue(self.harness.doCleanups(), "nested fixture cleanup failed")

    def test_changed_present_policy_freezes_cumulative_boundary_once_across_restart(self):
        inputs = [(2000, 0), (2100, 2000), (2200, 2100), (2300, 2200)]

        def primary(_body, n):
            incoming, cached = inputs[n - 1]
            response = support.completed(response_id=f"policy-boundary-{n}")
            response["usage"] = {"input_tokens": incoming, "output_tokens": 0,
                                 "input_tokens_details": {"cached_tokens": cached,
                                                          "cache_write_tokens": 0}}
            return 200, response

        def review(_body, _n):
            self.fail("below-review-threshold audit control must never dispatch a reviewer")

        fixture = self.harness.fixture(primary, review)
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        history = [{"role": "user", "content": "immutable policy boundary"}]
        versions = [1, 2, 2, 3]
        snapshots = []
        checkpoints = []
        paid = 0.0
        for index, version in enumerate(versions):
            if index:
                # Only an isolated fixture's prospective resolution is changed.
                # Keep the previous ledger version, tariff, history and money.
                path = next(self.harness.state.glob("*.json"))
                saved = json.loads(path.read_text())
                saved["model_resolution"]["primary"]["scenario_cache_policy"]["version"] = version
                path.write_text(json.dumps(saved))
                history.append({"role": "user", "content": f"append {index}"})
            proxy = support.Proxy(self.harness.state, fixture, mode="audit",
                                  extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS": "100000"})
            self.harness.stack.callback(proxy.close)
            self.assertEqual(proxy.send(history, model="gpt-6.1-sol")[0], 200)
            status, stats = proxy.request("GET", "/carry/dashboard/stats")
            self.assertEqual(status, 200)
            self.assertEqual(len(stats["sessions"]), 1)
            estimate = stats["sessions"][0]["cost_estimate"]
            checkpoint = proxy.saved()
            proxy.close()
            snapshots.append(copy.deepcopy(estimate))
            checkpoints.append(checkpoint)
            incoming, cached = inputs[index]
            paid += (incoming - cached) * 2.0 / 1e6 + cached * 0.1 / 1e6
            self.assertAlmostEqual(estimate["observed_primary_cost"], paid, places=12)
            self.assertAlmostEqual(estimate["carry_cost"], paid, places=12)
            self.assertEqual(estimate["matched_primary_responses"], index + 1)
            self.assertEqual(estimate["excluded_unpriced_calls"], 0)
            self.assertEqual(checkpoint["compactions"], 0)
            self.assertEqual(estimate["traditional_compactions"], 0)
            self.assertEqual(fixture.primaries[-1]["input"], history)
        self.evidence.update(snapshots=snapshots, checkpoints=checkpoints,
                             primaries=fixture.primaries, reviews=fixture.reviews,
                             scope="synthetic version changes; write-pricing repair remains separate")
        self.assertEqual(fixture.reviews, [])
        before, changed, same, changed_again = snapshots
        self.assertEqual(before.get("cache_policy_boundaries", []), [])
        boundaries = changed.get("cache_policy_boundaries", [])
        self.assertEqual(len(boundaries), 1,
                         "CACHE_POLICY_188: present v1 -> present v2 must freeze the old ledger")
        self.assertEqual(boundaries[0]["previous_version"], 1)
        self.assertEqual(boundaries[0]["next_version"], 2)
        cumulative = boundaries[0]["cumulative"]
        for field in ("carry_cost", "traditional_cost", "reviewer_cost", "summary_cost",
                      "traditional_compactions", "coverage", "matched_primary_responses"):
            self.assertEqual(cumulative[field], before[field], field)
        self.assertEqual(changed["counterfactual_cache_policy"], "mixed_versioned_cache_policies")
        self.assertEqual(same["cache_policy_boundaries"], boundaries,
                         "same-version restart must not duplicate or reprice the boundary")
        later = changed_again["cache_policy_boundaries"]
        self.assertEqual(len(later), 2)
        self.assertEqual(later[0], boundaries[0], "never overwrite an earlier boundary")
        self.assertEqual((later[1]["previous_version"], later[1]["next_version"]), (2, 3))
        for field in cumulative:
            self.assertEqual(later[1]["cumulative"][field], same[field], field)
        # Monetary snapshots are cumulative, never additive disjoint segments.
        # Do not bake the unsupported write premium into this migration test.
        # The separate #188 controls own ordinary/paid-write absolute pricing.
        for earlier, later in zip(snapshots, snapshots[1:]):
            self.assertGreater(later["traditional_cost"], earlier["traditional_cost"])
        self.assertEqual(stats["cost_estimate"]["mixed_cache_policy_sessions"], 1)
        self.assertEqual(stats["cost_estimate"]["legacy_cache_policy_sessions"], 0)


if __name__ == "__main__":
    unittest.main()
