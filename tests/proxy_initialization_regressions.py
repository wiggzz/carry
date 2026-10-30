"""Issue186 tests-first initialization controls against real loopback Carry.

Small receipt tracers, large-wire restart/retry/growth, and fixed-overhead
infeasibility share the executable used by hosted native integration tests.
All native receipts are synthetic; these are not provider/tokenizer measurements.
Candidate execution is supplied by the hosted native integration wrapper.
"""
import copy
import unittest

import proxy_state_regressions as support


class InitializationRegressions(unittest.TestCase):
    def setUp(self):
        self.harness = support.StateRegressions()
        self.harness.setUp()
        self.harness._testMethodName = self._testMethodName
        self.evidence = self.harness.evidence
        self.addCleanup(self.cleanup_harness)

    def cleanup_harness(self):
        self.assertTrue(self.harness.doCleanups(), "nested fixture cleanup failed")

    def first_primary(self, incoming):
        def primary(_body, n):
            response = support.completed(response_id=f"initialization186-{n}")
            response["usage"] = {
                "input_tokens": incoming,
                "output_tokens": 0,
                "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0},
            }
            return 200, response

        fixture = self.harness.fixture(primary, lambda *_: self.fail("unexpected reviewer"))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        history = [{"role": "user", "content": "synthetic native receipt initialization tracer"}]
        proxy = support.Proxy(
            self.harness.state, fixture, mode="audit",
            extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS": "1000000"},
        )
        self.harness.stack.callback(proxy.close)
        self.assertEqual(proxy.send(history, model="gpt-6.1-sol")[0], 200)
        status, stats = proxy.request("GET", "/carry/dashboard/stats")
        self.assertEqual(status, 200)
        self.assertEqual(len(stats["sessions"]), 1)
        estimate = stats["sessions"][0]["cost_estimate"]
        saved = proxy.saved()
        self.evidence.update(
            estimate=estimate, trajectory=saved["cost_trajectory"],
            primaries=fixture.primaries, reviews=fixture.reviews,
            basis="synthetic native receipt; initialization tracer only",
        )
        self.assertEqual(len(fixture.primaries), 1)
        self.assertEqual(fixture.primaries[0]["input"], history)
        self.assertEqual(fixture.reviews, [])
        self.assertEqual(saved["compactions"], 0)
        self.assertEqual(estimate["matched_primary_responses"], 1)
        self.assertEqual(estimate["excluded_unpriced_calls"], 0)
        # Actual native receipts remain paid; implicit hypothetical cache writes
        # remain a distinct supported tariff, never a free-inference assumption.
        ordinary_rate = 4.0 if incoming > 272000 else 2.0
        self.assertAlmostEqual(estimate["observed_primary_cost"], incoming * ordinary_rate / 1e6, places=12)
        self.assertAlmostEqual(estimate["carry_cost"], incoming * ordinary_rate / 1e6, places=12)
        return estimate

    def test_unknown_small_history_needs_no_initial_compaction(self):
        estimate = self.first_primary(10000)
        self.assertEqual(estimate["traditional_compactions"], 0)
        self.assertEqual(estimate["summary_cost"], 0.0)
        self.assertAlmostEqual(estimate["traditional_cost"], 10000 * 2.5 / 1e6, places=12)

    def test_unknown_oversized_history_is_initialized_before_first_primary_pricing(self):
        estimate = self.first_primary(300000)
        with self.subTest(boundary="free pre-window summary"):
            self.assertEqual(estimate["summary_cost"], 0.0,
                             "unrecognized oversized history must initialize outside the billed window")
        with self.subTest(boundary="first primary uses initialized prompt"):
            self.assertAlmostEqual(
                estimate["traditional_cost"], 21000 * 2.5 / 1e6, places=12,
                msg="first primary must use retained recent content plus replacement summary, cold",
            )

    def test_legacy_initialization_upgrade_preserves_recorded_money(self):
        import json

        def primary(_body, n):
            response = support.completed(response_id=f"legacy-initialization186-{n}")
            response["usage"] = {"input_tokens": 10000 if n == 1 else 300000,
                                 "output_tokens": 0,
                                 "input_tokens_details": {"cached_tokens": 0}}
            return 200, response

        fixture = self.harness.fixture(primary, lambda *_: self.fail("unexpected reviewer"))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))

        def start():
            proxy = support.Proxy(self.harness.state, fixture, mode="audit",
                                  extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS": "1000000"})
            self.harness.stack.callback(proxy.close)
            return proxy

        proxy = start()
        history = [{"role": "user", "content": "owned legacy money fixture"}]
        self.assertEqual(proxy.send(history, model="gpt-6.1-sol")[0], 200)
        proxy.close()
        checkpoint = next(self.harness.state.glob("*.json"))
        saved = json.loads(checkpoint.read_text())
        # Owned synthetic checkpoint: remove only the new estimand fields,
        # retaining all historical receipts, native pricing and session state.
        trajectory = saved["cost_trajectory"]
        trajectory["receipts"].pop("initialization_policy_version", None)
        trajectory["receipts"].pop("history_anchor", None)
        for key in ["initialization_policy", "initial_compactions", "legacy_initialization_segment"]:
            trajectory["result"].pop(key, None)
        historical = copy.deepcopy(trajectory["result"])
        checkpoint.write_text(json.dumps(saved))
        proxy = start()
        self.assertEqual(proxy.saved()["cost_trajectory"]["result"]["traditional_cost"],
                         historical["traditional_cost"])
        history.append({"role": "assistant", "content": "new unrecognized input"})
        self.assertEqual(proxy.send(history, model="gpt-6.1-sol")[0], 200)
        status, stats = proxy.request("GET", "/carry/dashboard/stats")
        self.assertEqual(status, 200)
        estimate = stats["sessions"][0]["cost_estimate"]
        self.evidence.update(historical=historical, estimate=estimate, final=proxy.saved(),
                             primaries=fixture.primaries, reviews=fixture.reviews)
        self.assertAlmostEqual(estimate["observed_primary_cost"], 1.22, places=12)
        self.assertAlmostEqual(estimate["traditional_cost"] - historical["traditional_cost"],
                               0.0525, places=12)
        self.assertEqual(estimate["summary_cost"], historical["summary_cost"])
        self.assertEqual(estimate["initial_compactions"], 1)
        self.assertEqual(estimate["initialization_policy"], "mixed_legacy_and_free_pre_window_v1")
        boundary = estimate["legacy_initialization_segment"]
        for key in ["carry_cost", "traditional_cost", "summary_cost", "traditional_compactions"]:
            self.assertEqual(boundary[key], historical[key])


    def test_large_wire_initialization_survives_restart_retry_and_later_paid_compaction(self):
        # Real large serialized history, not a huge usage count on a tiny input.
        # Native receipts remain scripted, not a provider/tokenizer measurement.
        inputs = [260000, 260100, None, 260200, 510200]

        def primary(_body, n):
            incoming = inputs[n - 1]
            if incoming is None:
                return 429, {"error": "owned initialization retry fixture"}
            response = support.completed(response_id=f"large-initialization-{n}")
            response["usage"] = {"input_tokens": incoming, "output_tokens": 0,
                                 "input_tokens_details": {"cached_tokens": 0}}
            return 200, response

        fixture = self.harness.fixture(primary, lambda *_: self.fail("unexpected reviewer"))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        options = ["--primary-context-tokens", "1048576"]

        def start():
            proxy = support.Proxy(self.harness.state, fixture, extra=options, mode="audit",
                                  extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS": "1000000"})
            self.harness.stack.callback(proxy.close)
            return proxy

        def snapshot(proxy):
            status, stats = proxy.request("GET", "/carry/dashboard/stats")
            self.assertEqual(status, 200)
            self.assertEqual(len(stats["sessions"]), 1)
            return copy.deepcopy(stats["sessions"][0]["cost_estimate"])

        history = [{"role": "user", "content": "initial goal"},
                   {"role": "assistant", "content": " evidence" * 260000},
                   {"role": "user", "content": "current request"}]
        proxy = start()
        self.assertEqual(proxy.send(history, model="gpt-6.1-sol", max_output_tokens=1)[0], 200)
        first = snapshot(proxy)
        initialized = proxy.saved()
        proxy.close()
        proxy = start()
        self.assertEqual(proxy.saved()["cost_trajectory"], initialized["cost_trajectory"],
                         "restart must load the exact successful comparison before any new request")
        history.append({"role": "user", "content": "append after restart"})
        self.assertEqual(proxy.send(history, model="gpt-6.1-sol", max_output_tokens=1)[0], 200)
        continued = snapshot(proxy)
        retry_history = history + [{"role": "user", "content": "retry once"}]
        self.assertEqual(proxy.send(retry_history, model="gpt-6.1-sol", max_output_tokens=1)[0], 429)
        failed = snapshot(proxy)
        self.assertEqual(proxy.send(retry_history, model="gpt-6.1-sol", max_output_tokens=1)[0], 200)
        retried = snapshot(proxy)
        growing = retry_history + [{"role": "assistant", "content": " later" * 250000},
                                   {"role": "user", "content": "continue after real growth"}]
        self.assertEqual(proxy.send(growing, model="gpt-6.1-sol", max_output_tokens=1)[0], 200)
        later = snapshot(proxy)
        self.evidence.update(first=first, continued=continued, failed=failed, retried=retried,
                             later=later, initialized=initialized, final=proxy.saved(),
                             primaries=fixture.primaries, reviews=fixture.reviews,
                             basis="large wire history with synthetic native receipts; no provider claims")
        self.assertGreater(len(support.wire(fixture.primaries[0])), 2000000)
        self.assertEqual(fixture.primaries[2]["input"], retry_history)
        self.assertEqual(fixture.primaries[3]["input"], retry_history)
        self.assertEqual(fixture.reviews, [])
        self.assertEqual(proxy.saved()["completed_requests"], 4)
        self.assertEqual(later["matched_primary_responses"], 4)
        self.assertEqual(later["excluded_failed_calls"], 1)
        self.assertEqual(later["excluded_unpriced_calls"], 0)
        paid = (260000 + 260100 + 260200) * 2.0 / 1e6 + 510200 * 4.0 / 1e6
        self.assertAlmostEqual(later["observed_primary_cost"], paid, places=12)
        self.assertAlmostEqual(later["carry_cost"], paid, places=12)
        self.assertEqual(failed["traditional_cost"], continued["traditional_cost"])
        with self.subTest(boundary="free first initialization"):
            self.assertEqual(first["summary_cost"], 0.0)
            self.assertAlmostEqual(first["traditional_cost"], 21000 * 2.5 / 1e6, places=12)
        with self.subTest(boundary="initialized continuation warm after restart"):
            self.assertAlmostEqual(continued["traditional_cost"] - first["traditional_cost"],
                                   (21000 * 0.1 + 100 * 2.5) / 1e6, places=12)
        with self.subTest(boundary="failed attempt never duplicates initialization or money"):
            self.assertEqual(retried["summary_cost"], 0.0)
            self.assertAlmostEqual(retried["traditional_cost"] - continued["traditional_cost"],
                                   21200 * 2.5 / 1e6, places=12)
        with self.subTest(boundary="later in-window summary is still charged"):
            self.assertGreater(later["summary_cost"], 0.0)
            self.assertEqual(later["traditional_compactions"], 1,
                             "charged in-window count excludes pre-window initialization")

    def test_native_offline_report_prices_initialized_first_primary(self):
        import json
        import os
        import subprocess
        from html.parser import HTMLParser

        class Rows(HTMLParser):
            def __init__(self):
                super().__init__()
                self.rows, self.row, self.cell = [], None, None

            def handle_starttag(self, tag, _attrs):
                if tag == "tr":
                    self.row = []
                elif tag == "td":
                    self.cell = ""

            def handle_data(self, text):
                if self.cell is not None:
                    self.cell += text

            def handle_endtag(self, tag):
                if tag == "td" and self.cell is not None:
                    self.row.append(self.cell)
                    self.cell = None
                elif tag == "tr" and self.row:
                    self.rows.append(self.row)
                    self.row = None

        session = self.harness.state / "sessions" / "owned186"
        session.mkdir(parents=True)
        request = {"model": "gpt-6.1-sol", "input": [
            {"role": "assistant", "content": " evidence" * 260000},
            {"role": "user", "content": "current"}]}
        events = [
            {"event": "run_started", "data": {"model": "gpt-6.1-sol", "context_window": 272000,
                "cache_ttl_seconds": 1800, "implicit_cache_minimum_prefix_tokens": 1024}},
            {"event": "model_request", "run_id": "owned186", "timestamp_ms": 1,
             "data": {"step": 1, "transport": {"api_base": "http://127.0.0.1:1"}, "request": request}},
            {"event": "model_response", "run_id": "owned186", "timestamp_ms": 2,
             "data": {"step": 1, "usage": {"input_tokens": 260000, "output_tokens": 0},
                      "raw": {"output": []}}},
        ]
        (session / "trace.jsonl").write_text("".join(json.dumps(e) + "\n" for e in events))
        output = self.harness.state / "cost.html"
        # Report CLI normally opens a browser: an empty child PATH makes the
        # opener unavailable, without launching one or modifying host settings.
        env = {"PATH": "", "HOME": str(self.harness.state)}
        if "TMPDIR" in os.environ:
            env["TMPDIR"] = os.environ["TMPDIR"]
        result = subprocess.run([os.environ["CARRY_TEST_BINARY"], "report", "cost", "--sessions",
                                 str(session.parent), "--output", str(output)],
                                env=env, capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        html = output.read_text()
        parser = Rows()
        parser.feed(html)
        self.assertEqual(len(parser.rows), 1)
        row = parser.rows[0]
        self.evidence.update(trace=events, report=html, row=row, stdout=result.stdout, stderr=result.stderr)
        self.assertEqual(row[0], "owned186")
        self.assertEqual(row[2], "1/1")
        self.assertEqual(row[3], "$0.5200", "observed native receipt remains paid")
        with self.subTest(boundary="native report initialized first primary"):
            self.assertEqual(row[5], "$0.0525")
        with self.subTest(boundary="native report no charged initial summary"):
            self.assertEqual(row[7], "0")

    def test_current_user_request_that_cannot_fit_is_explicitly_unavailable(self):
        def primary(_body, _n):
            response = support.completed()
            response["usage"] = {"input_tokens": 261000, "output_tokens": 0,
                                 "input_tokens_details": {"cached_tokens": 0}}
            return 200, response

        fixture = self.harness.fixture(primary, lambda *_: self.fail("unexpected reviewer"))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        proxy = support.Proxy(self.harness.state, fixture, mode="audit",
                              extra=["--primary-context-tokens", "1048576"],
                              extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS": "1000000"})
        self.harness.stack.callback(proxy.close)
        history = [{"role": "assistant", "content": " evidence" * 6000},
                   {"role": "user", "content": " current" * 256000}]
        self.assertEqual(proxy.send(history, model="gpt-6.1-sol", max_output_tokens=1)[0], 200)
        status, stats = proxy.request("GET", "/carry/dashboard/stats")
        self.assertEqual(status, 200)
        estimate = stats["sessions"][0]["cost_estimate"]
        self.evidence.update(estimate=estimate, trajectory=proxy.saved()["cost_trajectory"],
                             primaries=fixture.primaries, reviews=fixture.reviews,
                             basis="large protected current user request; synthetic receipt")
        self.assertEqual(fixture.primaries[0]["input"], history)
        self.assertEqual(fixture.reviews, [])
        self.assertAlmostEqual(estimate["observed_primary_cost"], 261000 * 2.0 / 1e6, places=12)
        self.assertEqual(proxy.saved()["primary"]["calls"], 1)
        self.assertIsNone(estimate["traditional_cost"],
                          "initialization cannot retire the current user request to invent feasibility")
        self.assertIsNone(estimate["savings"])

    def test_fixed_instructions_that_cannot_fit_are_explicitly_unavailable(self):
        def primary(_body, _n):
            response = support.completed()
            response["usage"] = {"input_tokens": 261000, "output_tokens": 0,
                                 "input_tokens_details": {"cached_tokens": 0}}
            return 200, response

        fixture = self.harness.fixture(primary, lambda *_: self.fail("unexpected reviewer"))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        proxy = support.Proxy(self.harness.state, fixture, mode="audit",
                              extra=["--primary-context-tokens", "1048576"],
                              extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS": "1000000"})
        self.harness.stack.callback(proxy.close)
        history = [{"role": "assistant", "content": " evidence" * 6000},
                   {"role": "user", "content": "current protected request"}]
        instructions = " fixed" * 256000
        self.assertEqual(proxy.send(history, model="gpt-6.1-sol", instructions=instructions,
                                    max_output_tokens=1)[0], 200)
        status, stats = proxy.request("GET", "/carry/dashboard/stats")
        self.assertEqual(status, 200)
        estimate = stats["sessions"][0]["cost_estimate"]
        self.evidence.update(estimate=estimate, trajectory=proxy.saved()["cost_trajectory"],
                             primaries=fixture.primaries, reviews=fixture.reviews)
        self.assertEqual(fixture.primaries[0]["instructions"], instructions)
        self.assertEqual(fixture.primaries[0]["input"], history)
        self.assertAlmostEqual(estimate["observed_primary_cost"], 261000 * 2.0 / 1e6, places=12)
        self.assertEqual(proxy.saved()["primary"]["calls"], 1)
        self.assertIsNone(estimate["traditional_cost"],
                          "fixed instructions alone exceed the economic input budget; do not retire them")
        self.assertIsNone(estimate["savings"], "infeasible comparison cannot advertise savings")


if __name__ == "__main__":
    unittest.main(verbosity=2)
