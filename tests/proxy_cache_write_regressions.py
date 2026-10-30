"""Issue188 economics controls: real CLI, synthetic receipts, no paid traffic.

No-write requests explicitly disable implicit caching and supply no breakpoints.
Default/implicit GPT-5.6+ caching can charge writes: separate positive controls
must preserve those charges. All receipts are synthetic, not live tariff proof.
"""
import copy
import json
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

    def summary_policy_control(self, *, migrate):
        """Real producer/restart; native totals deliberately differ from wire size."""
        def primary(_body, n):
            response = support.completed(response_id=f"summary-policy-{n}")
            response["usage"] = {
                "input_tokens": 300000 + (n - 1) * 100, "output_tokens": 0,
                "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0}}
            return 200, response

        fixture = self.harness.fixture(primary, lambda *_: self.fail("unexpected review"))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        history = [{"role": "user", "content": "synthetic large native receipt"}]
        snapshots = []
        for index in range(3):
            if index:
                if migrate and index == 1:
                    path = next(self.harness.state.glob("*.json"))
                    saved = json.loads(path.read_text())
                    # Future version is fixture metadata, not an implemented tariff.
                    saved["model_resolution"]["primary"]["scenario_cache_policy"]["version"] = 3
                    path.write_text(json.dumps(saved))
                history.append({"role": "user", "content": f"append {index}"})
            proxy = support.Proxy(self.harness.state, fixture, mode="audit",
                                  extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS": "1000000"})
            self.harness.stack.callback(proxy.close)
            self.assertEqual(proxy.send(history, model="gpt-6.1-sol")[0], 200)
            status, stats = proxy.request("GET", "/carry/dashboard/stats")
            self.assertEqual(status, 200)
            self.assertEqual(len(stats["sessions"]), 1)
            saved = proxy.saved()
            snapshots.append(copy.deepcopy({
                "estimate": stats["sessions"][0]["cost_estimate"],
                "trajectory": saved["cost_trajectory"]}))
            self.assertEqual(saved["compactions"], 0)
            self.assertEqual(fixture.primaries[-1]["input"], history)
            proxy.close()
        self.evidence.update(summary_policy_snapshots=snapshots,
                             primaries=fixture.primaries, reviews=fixture.reviews)
        self.assertEqual(len(fixture.primaries), 3)
        self.assertEqual(fixture.reviews, [])
        return snapshots

    def assert_summary_survives_policy(self, snapshots, *, migrate):
        first, second, third = snapshots
        self.assertEqual(first["trajectory"]["retired"], 280000)
        self.assertEqual(first["trajectory"]["summary"], 1000)
        self.assertEqual(first["trajectory"]["last_traditional_input"], 21000)
        for index, snapshot in enumerate(snapshots):
            estimate = snapshot["estimate"]
            self.assertEqual(estimate["traditional_compactions"], 1,
                             "cache-only policy changes must not resurrect retired history")
            self.assertEqual(estimate["summary_cost"], first["estimate"]["summary_cost"])
            self.assertEqual(snapshot["trajectory"]["retired"], 280000)
            self.assertEqual(snapshot["trajectory"]["summary"], 1000)
            self.assertEqual(estimate["matched_primary_responses"], index + 1)
            self.assertEqual(estimate["excluded_unpriced_calls"], 0)
        # A summary and a version transition both make the next comparison cold;
        # the following append becomes warm, without changing observed receipts.
        self.assertAlmostEqual(second["estimate"]["traditional_cost"] - first["estimate"]["traditional_cost"],
                               21100 * 2.5 / 1e6, places=12)
        self.assertAlmostEqual(third["estimate"]["traditional_cost"] - second["estimate"]["traditional_cost"],
                               (21100 * 0.1 + 100 * 2.5) / 1e6, places=12)
        boundaries = second["estimate"]["cache_policy_boundaries"]
        self.assertEqual(len(boundaries), int(migrate))
        if migrate:
            self.assertEqual(boundaries[0]["previous_version"], 2)
            self.assertEqual(boundaries[0]["next_version"], 3)
            self.assertEqual(boundaries[0]["cumulative"]["traditional_cost"], first["estimate"]["traditional_cost"])
            self.assertEqual(third["estimate"]["cache_policy_boundaries"], boundaries)

    def test_summary_continuity_unchanged_policy_restart(self):
        self.assert_summary_survives_policy(self.summary_policy_control(migrate=False), migrate=False)

    def test_summary_continuity_cache_policy_migration_restart(self):
        self.assert_summary_survives_policy(self.summary_policy_control(migrate=True), migrate=True)

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

    def test_breakpoint_override_and_mixed_lifetimes_keep_money_unknown_across_restart(self):
        cases = [(["30m"], 2000), (["12h"], 2000),
                 (["30m", "12h"], 2000), (["12h"], 0), (["30m"], 2000)]
        def primary(body, n):
            lifetimes, written = cases[n-1]
            self.assertEqual(body["prompt_cache_options"], {"mode": "explicit", "ttl": "30m"})
            self.assertEqual([b["prompt_cache_breakpoint"]["ttl"]
                              for b in body["input"][-1]["content"]], lifetimes)
            long_written = written // len(lifetimes) if "12h" in lifetimes else 0
            response = support.completed(response_id=f"breakpoint-ttl-{n}")
            response["usage"] = {"input_tokens": 2000, "output_tokens": 10,
                "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": written,
                    "cache_write_tokens_details": {"ttl_30m_tokens": written-long_written,
                                                    "ttl_12h_tokens": long_written}}}
            return 200, response
        fixture = self.harness.fixture(primary, lambda *_: self.fail("off mode reviewed"))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        history, snapshots, expected = [], [], 0.0
        for index, (lifetimes, written) in enumerate(cases):
            proxy = support.Proxy(self.harness.state, fixture, mode="off")
            self.harness.stack.callback(proxy.close)
            history.append({"role": "user", "content": [
                {"type": "input_text", "text": f"breakpoint {index}",
                 "prompt_cache_breakpoint": {"mode": "explicit", "ttl": ttl}}
                for ttl in lifetimes]})
            self.assertEqual(proxy.send(history, model="gpt-6.1-sol",
                prompt_cache_options={"mode": "explicit", "ttl": "30m"})[0], 200)
            saved = proxy.saved()
            estimate = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]["cost_estimate"]
            # Earlier unsupported breakpoints remain applicable in growing history.
            if index == 0 or not written:
                expected += (2000 * (2.5 if written else 2) + 100) / 1e6
            unknown = sum(bool(w) and n > 0 for n, (_, w) in enumerate(cases[:index+1]))
            snapshots.append({"saved": saved, "estimate": estimate, "known": expected})
            self.evidence.update(snapshots=snapshots, primaries=fixture.primaries)
            self.assertEqual(fixture.primaries[-1]["input"], history)
            self.assertEqual(saved["primary"]["calls"], index+1)
            self.assertEqual(saved["primary"]["input_tokens"], (index+1)*2000)
            self.assertEqual(saved["primary"]["cached_tokens"], 0)
            self.assertEqual(saved["primary"]["cache_write_tokens"], sum(w for _, w in cases[:index+1]))
            self.assertEqual(saved["primary"]["unavailable_cost_calls"], unknown)
            self.assertAlmostEqual(saved["primary"]["cost_usd"], expected, places=12)
            self.assertEqual(estimate["unavailable_primary_cost_calls"], unknown)
            self.assertAlmostEqual(estimate["observed_primary_cost"], expected, places=12)
            self.assertIsNone(estimate["traditional_cost"])
            self.assertIsNone(estimate["savings"])
            proxy.close()

    def test_typed_tool_output_lifetimes_preserve_money_across_restart(self):
        cases = [(["30m"], 2000), (["12h"], 2000),
                 (["30m", "12h"], 2000), (["12h"], 0), (["30m"], 2000)]
        def primary(body, n):
            lifetimes, written = cases[n-1]
            self.assertEqual(body["prompt_cache_options"], {"mode": "explicit", "ttl": "30m"})
            self.assertEqual([b["prompt_cache_breakpoint"]["ttl"]
                              for b in body["input"][-1]["output"]], lifetimes)
            long_written = written // len(lifetimes) if "12h" in lifetimes else 0
            response = support.completed(response_id=f"tool-output-ttl-{n}")
            response["usage"] = {"input_tokens": 2000, "output_tokens": 10,
                "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": written,
                    "cache_write_tokens_details": {"ttl_30m_tokens": written-long_written,
                                                    "ttl_12h_tokens": long_written}}}
            return 200, response
        fixture = self.harness.fixture(primary, lambda *_: self.fail("off mode reviewed"))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        history, snapshots, expected = [], [], 0.0
        for index, (lifetimes, written) in enumerate(cases):
            proxy = support.Proxy(self.harness.state, fixture, mode="off")
            self.harness.stack.callback(proxy.close)
            history.extend([
                {"type": "function_call", "call_id": f"call_191_{index}",
                 "name": "lookup", "arguments": "{}"},
                {"type": "function_call_output", "call_id": f"call_191_{index}", "output": [
                    {"type": "input_text", "text": f"tool result {index}",
                     "prompt_cache_breakpoint": {"mode": "explicit", "ttl": ttl}}
                    for ttl in lifetimes]}])
            self.assertEqual(proxy.send(history, model="gpt-6.1-sol",
                prompt_cache_options={"mode": "explicit", "ttl": "30m"})[0], 200)
            saved = proxy.saved()
            estimate = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]["cost_estimate"]
            if index == 0 or not written:
                expected += (2000 * (2.5 if written else 2) + 100) / 1e6
            unknown = sum(bool(w) and n > 0 for n, (_, w) in enumerate(cases[:index+1]))
            snapshots.append({"saved": saved, "estimate": estimate, "known": expected})
            self.evidence.update(snapshots=snapshots, primaries=fixture.primaries)
            self.assertEqual(fixture.primaries[-1]["input"], history)
            self.assertEqual(saved["primary"]["calls"], index+1)
            self.assertEqual(saved["primary"]["input_tokens"], (index+1)*2000)
            self.assertEqual(saved["primary"]["cache_write_tokens"], sum(w for _, w in cases[:index+1]))
            self.assertEqual(saved["primary"]["unavailable_cost_calls"], unknown)
            self.assertAlmostEqual(saved["primary"]["cost_usd"], expected, places=12)
            self.assertEqual(estimate["unavailable_primary_cost_calls"], unknown)
            self.assertAlmostEqual(estimate["observed_primary_cost"], expected, places=12)
            self.assertIsNone(estimate["traditional_cost"])
            self.assertIsNone(estimate["savings"])
            proxy.close()

    def test_branch_specific_summary_tiers_and_cache_survive_restart(self):
        # The actual full input stays large; only the hypothetical branch summarizes.
        # Staging changes this owned fixture's threshold, never production state.
        import json
        import tempfile
        from pathlib import Path
        snapshots = []
        for mode in ("implicit", "explicit"):
            with self.subTest(mode=mode):
                self.harness.state = Path(self.harness.stack.enter_context(tempfile.TemporaryDirectory()))
                inputs = [1000, 300000, 300100, 300200]
                def primary(body, n):
                    incoming = inputs[n-1]
                    cached = 300000 if mode == "implicit" and n > 2 else 0
                    written = incoming-cached if mode == "implicit" and incoming >= 1024 else 0
                    response = support.completed(response_id=f"summary-{n}")
                    response["usage"] = {"input_tokens": incoming, "output_tokens": 100,
                        "input_tokens_details": {"cached_tokens": cached, "cache_write_tokens": written}}
                    self.assertEqual(body["prompt_cache_options"], {"mode": mode, "ttl": "30m"})
                    return 200, response
                fixture = self.harness.fixture(primary, lambda *_: self.fail("off mode reviewed"))
                self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
                history = [{"role": "user", "content": "owned summary scenario"}]
                actual, traditional, summary = 0.0, 0.0, 0.0
                for index, incoming in enumerate(inputs):
                    proxy = support.Proxy(self.harness.state, fixture, mode="off")
                    self.harness.stack.callback(proxy.close)
                    if index:
                        history.append({"role": "user", "content": f"continue {index}"})
                    self.assertEqual(proxy.send(history, model="gpt-6.1-sol",
                        prompt_cache_options={"mode": mode, "ttl": "30m"},
                        prompt_cache_key="summary-tiers")[0], 200)
                    cached = 300000 if mode == "implicit" and index > 1 else 0
                    written = incoming-cached if mode == "implicit" and incoming >= 1024 else 0
                    ordinary, read, write, output = (4, .2, 5, 15) if incoming > 272000 else (2, .1, 2.5, 10)
                    actual += ((incoming-cached-written)*ordinary+cached*read+written*write+100*output)/1e6
                    alt = [1000, 300000, 1700, 1800][index]
                    alt_cached = 1700 if mode == "implicit" and index == 3 else 0
                    alt_written = alt-alt_cached if mode == "implicit" and alt >= 1024 else 0
                    ordinary, read, write, output = (4, .2, 5, 15) if alt > 272000 else (2, .1, 2.5, 10)
                    traditional += ((alt-alt_cached-alt_written)*ordinary+alt_cached*read+alt_written*write+100*output)/1e6
                    if index == 1:
                        summary = (298500*4+100*15)/1e6
                        traditional += summary
                    saved = proxy.saved()
                    estimate = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]["cost_estimate"]
                    snapshots.append({"mode": mode, "estimate": estimate, "saved": saved})
                    self.evidence.update(snapshots=snapshots)
                    self.assertEqual(fixture.primaries[-1]["input"], history)
                    self.assertEqual(estimate["matched_primary_responses"], index+1)
                    self.assertEqual(estimate["traditional_compactions"], int(index > 0))
                    self.assertEqual(saved["compactions"], 0)
                    self.assertAlmostEqual(estimate["observed_primary_cost"], actual, places=12)
                    self.assertAlmostEqual(saved["primary"]["cost_usd"], actual, places=12)
                    self.assertAlmostEqual(estimate["carry_cost"], actual, places=12)
                    self.assertAlmostEqual(estimate["traditional_cost"], traditional, places=12)
                    self.assertAlmostEqual(estimate["summary_cost"], summary, places=12)
                    self.assertAlmostEqual(estimate["savings"], traditional-actual, places=12)
                    proxy.close()
                    if index == 0:
                        path = next(self.harness.state.glob("*.json"))
                        saved["cost_trajectory"]["scenario"] = {"context_window":4000,
                            "reserve":100,"keep_recent":1500,"summary":100}
                        path.write_text(json.dumps(saved))
                self.assertEqual(fixture.reviews, [])

    def test_cache_ineligible_input_ordinary_price_control(self):
        self.assert_no_write_premium(self.run_control([(512, 0)], paid_write=True))


if __name__ == "__main__":
    unittest.main()
