"""Issue188 economics controls: real CLI, synthetic receipts, no paid traffic.

No-write requests explicitly disable implicit caching and supply no breakpoints.
Default/implicit GPT-5.6+ caching can charge writes: separate positive controls
must preserve those charges. All receipts are synthetic, not live tariff proof.
"""
import unittest

import proxy_state_regressions as support


class CacheWriteRegressions(unittest.TestCase):
    def setUp(self):
        self.harness = support.StateRegressions()
        self.harness.setUp()
        self.harness._testMethodName = self._testMethodName
        self.evidence = self.harness.evidence
        self.addCleanup(self.cleanup_harness)

    def cleanup_harness(self):
        self.assertTrue(self.harness.doCleanups(), "nested fixture cleanup failed")

    def run_control(self, inputs, *, restart=False, omit_write=False, paid_write=False):
        options = {"mode": "implicit" if paid_write else "explicit", "ttl": "30m"}

        def primary(_body, n):
            incoming, cached = inputs[n - 1]
            self.assertEqual(_body["prompt_cache_options"], options)
            self.assertEqual(_body["prompt_cache_key"], "cache-write-188")
            written = incoming - cached if paid_write and incoming >= 1024 else 0
            response = support.completed(response_id=f"cache-control-{n}")
            response["usage"] = {
                "input_tokens": incoming, "output_tokens": 100,
                "input_tokens_details": {"cached_tokens": cached}}
            if not omit_write:
                response["usage"]["input_tokens_details"]["cache_write_tokens"] = written
            return 200, response

        def review(_body, _n):
            self.fail("off-mode negative control must never dispatch a reviewer")

        fixture = self.harness.fixture(primary, review)
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        history = [{"role": "user", "content": "unchanged below-budget cache control"}]
        snapshots = []
        paid = 0.0
        for index, (incoming, cached) in enumerate(inputs):
            if index == 0 or restart:
                if index:
                    proxy.close()
                proxy = support.Proxy(self.harness.state, fixture, mode="off")
                self.harness.stack.callback(proxy.close)
            if index:
                history.append({"role": "user", "content": f"continue {index}"})
            self.assertEqual(proxy.send(history, model="gpt-6.1-sol",
                                        prompt_cache_options=options,
                                        prompt_cache_key="cache-write-188")[0], 200)
            status, stats = proxy.request("GET", "/carry/dashboard/stats")
            self.assertEqual(status, 200)
            self.assertEqual(len(stats["sessions"]), 1)
            estimate = stats["sessions"][0]["cost_estimate"]
            saved = proxy.saved()
            # Independent frozen catalog expectations in dollars/token. The
            # local wire is deliberately tiny: shared receipt units must own
            # both branches, rather than inferring native input from JSON size.
            written = incoming - cached if paid_write and incoming >= 1024 else 0
            paid += ((incoming - cached - written) * 2.0 + cached * 0.10
                     + written * 2.50 + 100 * 10.0) / 1e6
            self.evidence.update(primaries=fixture.primaries, reviews=fixture.reviews,
                                 snapshots=snapshots, saved=saved,
                                 fixture_write_semantics=("paid_implicit_30m" if paid_write else
                                                          "explicit_only_no_breakpoints"))
            self.assertEqual(fixture.primaries[-1]["input"], history)
            self.assertEqual(fixture.primaries[-1]["prompt_cache_options"], options)
            self.assertEqual(saved["compactions"], 0)
            self.assertEqual(estimate["traditional_compactions"], 0)
            self.assertEqual(estimate["reviewer_cost"], 0)
            self.assertEqual(estimate["summary_cost"], 0)
            self.assertEqual(estimate["matched_primary_responses"], index + 1)
            self.assertEqual(estimate["unavailable_primary_cost_calls"], 0)
            self.assertEqual(estimate["excluded_unpriced_calls"], 0)
            self.assertAlmostEqual(estimate["observed_primary_cost"], paid, places=12)
            self.assertAlmostEqual(estimate["carry_cost"], paid, places=12)
            snapshots.append({"estimate": estimate, "expected_paid": paid})
        self.assertEqual(fixture.reviews, [])
        return snapshots

    def assert_no_write_premium(self, snapshots):
        for index, snapshot in enumerate(snapshots):
            estimate = snapshot["estimate"]
            with self.subTest(request=index + 1):
                self.assertAlmostEqual(
                    estimate["traditional_cost"], snapshot["expected_paid"], places=12,
                    msg="CACHE_WRITE_188: identical declared-cache branches must have equal cost")
                self.assertAlmostEqual(estimate["savings"], 0.0, places=12)

    def test_identical_uncached_zero_write_has_no_savings(self):
        self.assert_no_write_premium(self.run_control([(43298, 0)]))

    def test_identical_uncached_missing_optional_write_has_no_savings(self):
        self.assert_no_write_premium(self.run_control([(43298, 0)], omit_write=True))

    def test_explicit_without_breakpoints_stays_uncached_across_restart(self):
        self.assert_no_write_premium(self.run_control(
            [(43298, 0), (44298, 0), (45298, 0)], restart=True))

    def test_paid_implicit_cold_write_remains_charged(self):
        self.assert_no_write_premium(self.run_control([(43298, 0)], paid_write=True))

    def test_paid_implicit_append_reads_and_writes_survive_restart(self):
        # Matching scripted hits isolate prices, not divergent-history reuse.
        self.assert_no_write_premium(self.run_control(
            [(43298, 0), (44298, 43298), (45298, 44298)],
            restart=True, paid_write=True))

    def test_unsupported_lifetime_writes_keep_raw_receipts_and_known_subtotal_across_restart(self):
        receipts = [(2000, 0, 0), (2000, 0, 2000), (2100, 2000, 0)]
        def primary(body, n):
            self.assertEqual(body["prompt_cache_options"], {"mode": "implicit", "ttl": "12h"})
            incoming, cached, written = receipts[n-1]
            response = support.completed(response_id=f"unknown-ttl-{n}")
            response["usage"] = {"input_tokens": incoming, "output_tokens": 10,
                "input_tokens_details": {"cached_tokens": cached, "cache_write_tokens": written,
                    "cache_write_tokens_details": {"12h": written}}}
            return 200, response
        fixture = self.harness.fixture(primary, lambda *_: self.fail("off mode reviewed"))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        history = [{"role": "user", "content": "unsupported lifetime receipt"}]
        snapshots = []
        expected = 0.0
        for index, (incoming, cached, written) in enumerate(receipts):
            proxy = support.Proxy(self.harness.state, fixture, mode="off")
            self.harness.stack.callback(proxy.close)
            if index:
                history.append({"role": "user", "content": f"append {index}"})
            self.assertEqual(proxy.send(history, model="gpt-6.1-sol",
                prompt_cache_options={"mode": "implicit", "ttl": "12h"})[0], 200)
            saved = proxy.saved()
            status, stats = proxy.request("GET", "/carry/dashboard/stats")
            self.assertEqual(status, 200)
            estimate = stats["sessions"][0]["cost_estimate"]
            if not written:
                expected += ((incoming-cached)*2 + cached*0.1 + 100)/1e6
            snapshots.append({"saved": saved, "estimate": estimate, "known": expected})
            self.evidence.update(snapshots=snapshots, primaries=fixture.primaries)
            self.assertEqual(saved["primary"]["calls"], index+1)
            self.assertEqual(saved["primary"]["cache_write_tokens"], 2000 if index else 0)
            self.assertEqual(saved["primary"]["input_tokens"], sum(x[0] for x in receipts[:index+1]))
            self.assertEqual(saved["primary"]["cached_tokens"], sum(x[1] for x in receipts[:index+1]))
            self.assertEqual(saved["primary"]["unavailable_cost_calls"], int(index > 0))
            self.assertAlmostEqual(saved["primary"]["cost_usd"], expected, places=12)
            self.assertEqual(estimate["unavailable_primary_cost_calls"], int(index > 0))
            self.assertAlmostEqual(estimate["observed_primary_cost"], expected, places=12)
            self.assertIsNone(estimate["traditional_cost"])
            self.assertIsNone(estimate["savings"])
            proxy.close()

    def test_cache_ineligible_input_ordinary_price_control(self):
        self.assert_no_write_premium(self.run_control([(512, 0)], paid_write=True))


if __name__ == "__main__":
    unittest.main()
