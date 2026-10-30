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

    def output_allowance_history_control(self, *, initialized, change_cap):
        # Native-total tracers, not provider/tokenizer measurements. The seeded
        # path preserves a genuine charged summary on both baseline and candidate.
        inputs = ([] if initialized else [10000]) + [300000, 300100, 300200, 300300, 300400]
        changed_at = len(inputs) - 2

        def primary(_body, n):
            response = support.completed(response_id=f"history-contract-{n}")
            response["usage"] = {"input_tokens": inputs[n - 1], "output_tokens": 0,
                                 "input_tokens_details": {"cached_tokens": 0}}
            return 200, response

        fixture = self.harness.fixture(primary, lambda *_: self.fail("unexpected reviewer"))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        snapshots, wires, caps = [], [], []
        history = [{"role": "assistant", "content": "owned native-input history contract"},
                   {"role": "user", "content": "current request"}]
        for index, _incoming in enumerate(inputs):
            if index:
                history.append({"role": "user", "content": f"append {index}"})
            cap = 2 if change_cap and index >= changed_at else 1
            proxy = support.Proxy(self.harness.state, fixture, mode="audit",
                                  extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS": "1000000"})
            self.harness.stack.callback(proxy.close)
            if index:
                self.assertEqual(proxy.saved()["cost_trajectory"], snapshots[-1],
                                 "restart must preserve settled quantities and money")
            wires.append(copy.deepcopy(history))
            caps.append(cap)
            self.assertEqual(proxy.send(history, model="gpt-6.1-sol", max_output_tokens=cap)[0], 200)
            snapshots.append(copy.deepcopy(proxy.saved()["cost_trajectory"]))
            proxy.close()
        self.evidence.update(history_contract_snapshots=snapshots, primaries=fixture.primaries,
                             reviews=fixture.reviews, initialized=initialized, change_cap=change_cap,
                             basis="native receipt-domain contract; no inferred per-message quantities")
        self.assertEqual(len(fixture.primaries), len(inputs))
        self.assertEqual(fixture.reviews, [])
        for index, snapshot in enumerate(snapshots):
            self.assertEqual(fixture.primaries[index]["input"], wires[index])
            self.assertEqual(fixture.primaries[index]["max_output_tokens"], caps[index])
            expected_paid = sum(n * (4.0 if n > 272000 else 2.0) / 1e6
                                for n in inputs[:index + 1])
            self.assertAlmostEqual(snapshot["result"]["observed_primary_cost"], expected_paid, places=12)
            self.assertAlmostEqual(snapshot["result"]["carry_cost"], expected_paid, places=12)
            self.assertEqual(snapshot["result"]["excluded_unpriced_calls"], 0)
        before = snapshots[changed_at - 1]
        self.assertEqual(before["retired"], 280000)
        self.assertEqual(before["summary"], 1000)
        self.assertEqual(before["last_traditional_input"], 21200)
        self.assertEqual(before["result"]["traditional_compactions"], int(not initialized))
        self.assertEqual(before["result"]["summary_cost"] > 0.0, not initialized)
        if initialized:
            self.assertEqual(before["result"]["initial_compactions"], 1)
        for offset, after in enumerate(snapshots[changed_at:]):
            with self.subTest(boundary="output allowance is not history identity", append=offset):
                self.assertEqual(after["result"]["summary_cost"], before["result"]["summary_cost"],
                                 "fitting output allowance cannot charge another summary")
                self.assertEqual(after["result"]["traditional_compactions"],
                                 before["result"]["traditional_compactions"])
                # Omitted legacy schema means zero initializations, not an error;
                # the free-initialized arm explicitly proves its own new-schema precondition.
                self.assertEqual(after["result"].get("initial_compactions", 0),
                                 before["result"].get("initial_compactions", 0),
                                 "fitting output allowance cannot grant another free initialization")
                self.assertEqual(after["retired"], 280000)
                self.assertEqual(after["summary"], 1000)
                self.assertEqual(after["last_traditional_input"], 21300 + offset * 100,
                                 "all accumulated growth must survive the allowance change")

    def test_history_contract_unchanged_output_preserves_paid_growth(self):
        self.output_allowance_history_control(initialized=False, change_cap=False)

    def test_history_contract_changed_output_preserves_paid_growth(self):
        self.output_allowance_history_control(initialized=False, change_cap=True)

    def test_history_contract_changed_output_preserves_free_initialized_growth(self):
        self.output_allowance_history_control(initialized=True, change_cap=True)

    def test_unknown_small_history_needs_no_initial_compaction(self):
        estimate = self.first_primary(10000)
        self.assertEqual(estimate["traditional_compactions"], 0)
        self.assertEqual(estimate["summary_cost"], 0.0)
        self.assertAlmostEqual(estimate["traditional_cost"], 10000 * 2 / 1e6, places=12)

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

    def test_cache_only_failure_retains_recognition_but_changed_settings_reinitialize(self):
        inputs = [260000, None, 260100, 260200]

        def primary(_body, n):
            incoming = inputs[n - 1]
            if incoming is None:
                return 429, {"error": "owned cache-only recognition fixture"}
            response = support.completed(response_id=f"recognition186-{n}")
            response["usage"] = {"input_tokens": incoming, "output_tokens": 0,
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
        history = [{"role": "assistant", "content": "owned synthetic receipt tracer"},
                   {"role": "user", "content": "current"}]
        self.assertEqual(proxy.send(history, model="gpt-6.1-sol", max_output_tokens=1)[0], 200)
        first = copy.deepcopy(proxy.saved()["cost_trajectory"])
        appended = history + [{"role": "user", "content": "append"}]
        self.assertEqual(proxy.send(appended, model="gpt-6.1-sol", max_output_tokens=1)[0], 429)
        failed = copy.deepcopy(proxy.saved()["cost_trajectory"])
        proxy.close()
        proxy = start()
        restarted = copy.deepcopy(proxy.saved()["cost_trajectory"])
        self.assertEqual(proxy.send(appended, model="gpt-6.1-sol", max_output_tokens=1)[0], 200)
        retry = copy.deepcopy(proxy.saved()["cost_trajectory"])
        self.assertEqual(proxy.send(appended, model="gpt-6.1-sol", max_output_tokens=1,
                                    instructions="changed fixed settings")[0], 200)
        changed = copy.deepcopy(proxy.saved()["cost_trajectory"])
        self.evidence.update(first=first, failed=failed, restarted=restarted, retry=retry,
                             changed=changed, primaries=fixture.primaries, reviews=fixture.reviews,
                             basis="synthetic receipt accounting; explicit owner restart and settings fence")
        self.assertEqual(fixture.reviews, [])
        self.assertEqual(len(fixture.primaries), 4)
        self.assertEqual(fixture.primaries[1]["input"], fixture.primaries[2]["input"])
        self.assertEqual(fixture.primaries[3]["instructions"], "changed fixed settings")
        self.assertAlmostEqual(changed["result"]["observed_primary_cost"], 1.5606, places=12)
        self.assertEqual(failed["result"]["traditional_cost"], first["result"]["traditional_cost"])
        self.assertAlmostEqual(first["result"]["traditional_cost"], 0.0525, places=12)
        self.assertEqual(first["result"]["initial_compactions"], 1)
        self.assertIsNotNone(first["history_continuity"])
        self.assertIsNone(failed["receipts"]["anchor"])
        self.assertIsNone(failed["receipts"]["traditional_anchor"])
        self.assertEqual(failed["history_continuity"], first["history_continuity"])
        self.assertEqual(restarted, failed, "restart preserves the failed-attempt boundary")
        self.assertEqual(retry["result"]["initial_compactions"], 1,
                         "cache-only invalidation cannot repeat free initialization")
        self.assertAlmostEqual(retry["result"]["traditional_cost"] - first["result"]["traditional_cost"],
                               21100 * 2.5 / 1e6, places=12)
        self.assertEqual(changed["result"]["initial_compactions"], 2,
                         "genuinely unrecognized changed settings require a new prospective assumption")
        self.assertAlmostEqual(changed["result"]["traditional_cost"] - retry["result"]["traditional_cost"],
                               0.0525, places=12)
        self.assertEqual(changed["result"]["summary_cost"], 0.0)
        self.assertEqual(changed["result"]["traditional_compactions"], 0)

    def initialized_policy_restart_control(self, *, migrate):
        import json

        def primary(_body, n):
            response = support.completed(response_id=f"initialized-policy186-{n}")
            response["usage"] = {"input_tokens": 300000 + (n - 1) * 100,
                                 "output_tokens": 0,
                                 "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0}}
            return 200, response

        fixture = self.harness.fixture(primary, lambda *_: self.fail("unexpected reviewer"))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        history = [{"role": "assistant", "content": "owned synthetic receipt history"},
                   {"role": "user", "content": "current"}]
        snapshots = []
        wires = []
        for index in range(5):
            if index:
                if migrate and index == 3:
                    checkpoint = next(self.harness.state.glob("*.json"))
                    saved = json.loads(checkpoint.read_text())
                    # Synthetic future policy identity only, not a new tariff.
                    # The recorded ledger and complete old digest stay intact.
                    resolution = saved["model_resolution"]["primary"]
                    self.assertEqual(resolution["scenario_cache_policy"]["version"], 2)
                    resolution["scenario_cache_policy"]["version"] = 3
                    checkpoint.write_text(json.dumps(saved))
                history.append({"role": "user", "content": f"append {index}"})
            proxy = support.Proxy(self.harness.state, fixture, mode="audit",
                                  extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS": "1000000"})
            self.harness.stack.callback(proxy.close)
            if index:
                self.assertEqual(proxy.saved()["cost_trajectory"], snapshots[-1],
                                 "restart must not rewrite prior trajectory or money")
            wires.append(copy.deepcopy(history))
            self.assertEqual(proxy.send(history, model="gpt-6.1-sol", max_output_tokens=1)[0], 200)
            snapshots.append(copy.deepcopy(proxy.saved()["cost_trajectory"]))
            proxy.close()
        self.evidence.update(policy_snapshots=snapshots, primaries=fixture.primaries,
                             reviews=fixture.reviews, migrate=migrate,
                             basis="initialized receipt-domain history; synthetic policy identity and usage")
        self.assertEqual(len(fixture.primaries), 5)
        self.assertEqual(fixture.reviews, [])
        first = snapshots[0]
        self.assertIsNotNone(first["history_continuity"])
        self.assertEqual(first["retired"], 280000)
        self.assertEqual(first["summary"], 1000)
        self.assertEqual(first["result"]["initial_compactions"], 1)
        self.assertAlmostEqual(first["result"]["traditional_cost"], 0.0525, places=12)
        for index, snapshot in enumerate(snapshots):
            result = snapshot["result"]
            self.assertEqual(fixture.primaries[index]["input"], wires[index])
            self.assertEqual(fixture.primaries[index]["max_output_tokens"], 1)
            self.assertAlmostEqual(result["observed_primary_cost"],
                                   sum(300000 + n * 100 for n in range(index + 1)) * 4 / 1e6,
                                   places=12)
            self.assertEqual(result["summary_cost"], 0.0)
            self.assertEqual(result["traditional_compactions"], 0)
            self.assertEqual(result["excluded_unpriced_calls"], 0)
            with self.subTest(boundary="initialization is not repeated", request=index + 1):
                self.assertEqual(result["initial_compactions"], 1,
                                 "policy-only cache coldness must not erase known traditional history")
                self.assertEqual(snapshot["retired"], 280000)
                self.assertEqual(snapshot["summary"], 1000)
        # The history is warm before migration. A policy change is cold once,
        # but retains every appended token rather than freely compacting again.
        for index in range(1, 5):
            previous_input = 21000 + (index - 1) * 100
            expected = ((21000 + index * 100) * 2.5 if migrate and index == 3
                        else previous_input * 0.1 + 100 * 2.5) / 1e6
            with self.subTest(boundary="cold transition or warm append", request=index + 1):
                self.assertAlmostEqual(snapshots[index]["result"]["traditional_cost"]
                                       - snapshots[index - 1]["result"]["traditional_cost"],
                                       expected, places=12)
        boundaries = snapshots[3]["result"].get("cache_policy_boundaries", [])
        self.assertEqual(len(boundaries), int(migrate))
        if migrate:
            self.assertEqual(boundaries[0]["previous_version"], 2)
            self.assertEqual(boundaries[0]["next_version"], 3)
            for key in ["carry_cost", "traditional_cost", "summary_cost", "traditional_compactions"]:
                self.assertEqual(boundaries[0]["cumulative"][key], snapshots[2]["result"][key])
            self.assertEqual(snapshots[2]["history_continuity"]["contract"],
                             snapshots[3]["history_continuity"]["contract"],
                             "cache policy must not rekey traditional history")
        self.assertEqual(snapshots[4]["result"].get("cache_policy_boundaries", []), boundaries)

    def test_initialized_history_unchanged_policy_stays_warm_across_restarts(self):
        self.initialized_policy_restart_control(migrate=False)

    def test_initialized_history_policy_transition_is_cold_once_without_reinitialization(self):
        self.initialized_policy_restart_control(migrate=True)

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
        # Rendered-product acceptance, not source inspection. Keep the existing
        # numeric columns and money assertions independently exercised above.
        with self.subTest(boundary="native report initialization disclosure"):
            self.assertIn("1 free pre-window summaries", row[9])
            self.assertIn("0 charged in-window summaries", row[9])
            self.assertIn("counterfactual initialization assumption", row[9])
        with self.subTest(boundary="native report does not contradict initialization"):
            self.assertNotIn("No compaction is assumed before the first observed prompt", html)

    def test_native_report_later_paid_compaction_preserves_fixed_floor(self):
        import json
        import os
        import subprocess
        from html.parser import HTMLParser

        class Cells(HTMLParser):
            def __init__(self):
                super().__init__()
                self.cells, self.cell = [], None

            def handle_starttag(self, tag, _attrs):
                if tag == "td":
                    self.cell = ""

            def handle_data(self, data):
                if self.cell is not None:
                    self.cell += data

            def handle_endtag(self, tag):
                if tag == "td" and self.cell is not None:
                    self.cells.append(self.cell)
                    self.cell = None

        session = self.harness.state / "sessions" / "fixed-floor186"
        session.mkdir(parents=True)
        body = {"model": "gpt-6.1-sol", "instructions": " fixed" * 40000,
                "tools": [{"type": "function", "name": "owned", "description": "fixed tool",
                           "parameters": {"type": "object", "properties": {}}}],
                "input": [{"role": "assistant", "content": " evidence" * 260000},
                          {"role": "user", "content": "current"}]}
        events = [{"event": "run_started", "data": {"model": "gpt-6.1-sol", "context_window": 272000,
                   "cache_ttl_seconds": 1800, "implicit_cache_minimum_prefix_tokens": 1024}}]
        rows = []
        for step, incoming in enumerate([300000, 560000, 560100], 1):
            if step == 2:
                body["input"].append({"role": "assistant", "content": " later" * 260000})
            elif step == 3:
                body["input"].append({"role": "user", "content": "continue"})
            events.extend([
                {"event": "model_request", "run_id": "fixed-floor186", "timestamp_ms": step * 10,
                 "data": {"step": step, "transport": {"api_base": "http://127.0.0.1:1"},
                          "request": copy.deepcopy(body)}},
                {"event": "model_response", "run_id": "fixed-floor186", "timestamp_ms": step * 10 + 1,
                 "data": {"step": step, "usage": {"input_tokens": incoming, "output_tokens": 0},
                          "raw": {"output": []}}}])
            if step == 1:
                continue
            (session / "trace.jsonl").write_text("".join(json.dumps(e) + "\n" for e in events))
            output = self.harness.state / f"fixed-floor-{step}.html"
            env = {"PATH": "", "HOME": str(self.harness.state)}
            if "TMPDIR" in os.environ:
                env["TMPDIR"] = os.environ["TMPDIR"]
            result = subprocess.run([os.environ["CARRY_TEST_BINARY"], "report", "cost", "--sessions",
                                     str(session.parent), "--output", str(output)],
                                    env=env, capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            parser = Cells()
            parser.feed(output.read_text())
            self.assertEqual(len(parser.cells), 10)
            rows.append(parser.cells)
        self.evidence.update(trace=events, rows=rows,
                             basis="real offline CLI; synthetic receipts; fixed tools/instructions floor")
        self.assertEqual(rows[0][2], "2/2")
        self.assertEqual(rows[1][2], "3/3")
        self.assertEqual(rows[0][3], "$3.4400")
        self.assertEqual(rows[1][3], "$5.6804")
        self.assertGreater(int(rows[0][7]), 0, "later in-window summary remains charged")
        self.assertEqual(rows[1][7], rows[0][7], "no extra summary on third primary")
        marginal = float(rows[1][5].removeprefix("$")) - float(rows[0][5].removeprefix("$"))
        self.assertGreaterEqual(marginal, 0.1,
                                "cold third primary must retain at least 40000 fixed input tokens")

    def hard_capacity_comparison(self, cap):
        def primary(_body, _n):
            response = support.completed()
            response["usage"] = {"input_tokens": 60000, "output_tokens": 0,
                                 "input_tokens_details": {"cached_tokens": 0}}
            return 200, response

        fixture = self.harness.fixture(primary, lambda *_: self.fail("unexpected reviewer"))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        proxy = support.Proxy(self.harness.state, fixture, mode="audit",
                              extra=["--primary-context-tokens", "50000"],
                              extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS": "1000000"})
        self.harness.stack.callback(proxy.close)
        history = [{"role": "assistant", "content": "old synthetic history"},
                   {"role": "user", "content": "current"}]
        self.assertEqual(proxy.send(history, model="gpt-6.1-sol", max_output_tokens=cap)[0], 200)
        status, stats = proxy.request("GET", "/carry/dashboard/stats")
        self.assertEqual(status, 200)
        estimate = stats["sessions"][0]["cost_estimate"]
        saved = proxy.saved()
        self.evidence.update(estimate=estimate, saved=saved, primaries=fixture.primaries,
                             reviews=fixture.reviews,
                             basis="synthetic receipt-domain hard-capacity tracer, not realistic oversized wire")
        self.assertEqual(len(fixture.primaries), 1)
        self.assertEqual(fixture.primaries[0]["input"], history)
        self.assertEqual(fixture.primaries[0]["max_output_tokens"], cap)
        self.assertEqual(fixture.reviews, [])
        self.assertAlmostEqual(estimate["observed_primary_cost"], 0.12, places=12)
        self.assertEqual(saved["primary"]["calls"], 1)
        self.assertEqual(saved["cost_trajectory"]["result"]["summary_cost"], 0.0)
        self.assertEqual(estimate["traditional_compactions"], 0)
        return estimate

    def test_hard_capacity_triggers_initialization_below_economic_budget(self):
        estimate = self.hard_capacity_comparison(1)
        self.assertAlmostEqual(estimate["traditional_cost"], 0.0525, places=12,
                               msg="first compared primary must fit resolved hard capacity")
        self.assertEqual(estimate["initial_compactions"], 1)
        self.assertEqual(estimate["matched_primary_responses"], 1)

    def test_hard_capacity_and_output_exclude_infeasible_retained_floor(self):
        estimate = self.hard_capacity_comparison(40000)
        self.assertIsNone(estimate["traditional_cost"],
                          "21000 retained input plus 40000 output cannot fit 50000 hard capacity")
        self.assertIsNone(estimate["savings"])
        self.assertEqual(estimate["initial_compactions"], 0)
        self.assertEqual(estimate["matched_primary_responses"], 0)
        self.assertEqual(estimate["excluded_unpriced_calls"], 1)

    def omitted_output_comparison(self, reserve=None, cap=None):
        def primary(_body, _n):
            response = support.completed()
            response["usage"] = {"input_tokens": 40000, "output_tokens": 0,
                                 "input_tokens_details": {"cached_tokens": 0}}
            return 200, response

        fixture = self.harness.fixture(primary, lambda *_: self.fail("unexpected reviewer"))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        extra = ["--primary-context-tokens", "50000"]
        if reserve is not None:
            extra.extend(["--primary-output-reserve-tokens", str(reserve)])
        proxy = support.Proxy(self.harness.state, fixture, mode="audit", extra=extra,
                              extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS": "1000000"})
        self.harness.stack.callback(proxy.close)
        history = [{"role": "assistant", "content": "old synthetic history"},
                   {"role": "user", "content": "current"}]
        fields = {} if cap is None else {"max_output_tokens": cap}
        self.assertEqual(proxy.send(history, model="gpt-6.1-sol", **fields)[0], 200)
        status, stats = proxy.request("GET", "/carry/dashboard/stats")
        self.assertEqual(status, 200)
        estimate = stats["sessions"][0]["cost_estimate"]
        saved = proxy.saved()
        self.evidence.update(estimate=estimate, saved=saved, primaries=fixture.primaries,
                             reviews=fixture.reviews,
                             basis="synthetic native receipt, omitted versus explicit output capacity tracer")
        self.assertEqual(len(fixture.primaries), 1)
        self.assertEqual(fixture.primaries[0]["input"], history)
        if cap is None:
            self.assertNotIn("max_output_tokens", fixture.primaries[0], "do not invent a wire cap")
        else:
            self.assertEqual(fixture.primaries[0]["max_output_tokens"], cap)
        self.assertEqual(fixture.reviews, [])
        self.assertAlmostEqual(estimate["observed_primary_cost"], 0.08, places=12)
        self.assertEqual(saved["primary"]["calls"], 1)
        resolution = saved["model_resolution"]["primary"]
        self.assertEqual(resolution["capacity_tokens"], 50000)
        self.assertEqual(resolution["ordinary"]["output_reserve"], 16384 if reserve is None else reserve)
        self.assertEqual(saved["cost_trajectory"]["result"]["summary_cost"], 0.0)
        self.assertEqual(estimate["traditional_compactions"], 0)
        return estimate

    def test_explicit_output_overrides_large_configured_capacity_reserve(self):
        estimate = self.omitted_output_comparison(reserve=40000, cap=1)
        self.assertAlmostEqual(estimate["traditional_cost"], estimate["observed_primary_cost"], places=12)
        self.assertEqual(estimate["initial_compactions"], 0)
        self.assertEqual(estimate["matched_primary_responses"], 1)

    def test_omitted_output_reserves_default_frozen_allowance(self):
        estimate = self.omitted_output_comparison()
        self.assertAlmostEqual(estimate["traditional_cost"], 0.0525, places=12,
                               msg="40000 input plus default output reserve cannot fit 50000")
        self.assertEqual(estimate["initial_compactions"], 1)
        self.assertEqual(estimate["matched_primary_responses"], 1)

    def test_omitted_output_excludes_custom_reserve_infeasible_floor(self):
        estimate = self.omitted_output_comparison(reserve=40000)
        self.assertIsNone(estimate["traditional_cost"], "21000 retained floor plus 40000 fallback cannot fit")
        self.assertIsNone(estimate["savings"])
        self.assertEqual(estimate["initial_compactions"], 0)
        self.assertEqual(estimate["matched_primary_responses"], 0)
        self.assertEqual(estimate["excluded_unpriced_calls"], 1)

    def image_output_comparison(self, cap):
        def primary(_body, _n):
            response = support.completed()
            response["usage"] = {"input_tokens": 201000, "output_tokens": 0,
                                 "input_tokens_details": {"cached_tokens": 0}}
            return 200, response

        fixture = self.harness.fixture(primary, lambda *_: self.fail("unexpected reviewer"))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        proxy = support.Proxy(self.harness.state, fixture, mode="audit",
                              extra=["--primary-context-tokens", "1048576"],
                              extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS": "1000000"})
        self.harness.stack.callback(proxy.close)
        history = [{"role": "assistant", "content": " evidence" * 200000},
                   {"role": "user", "content": [
                       {"type": "input_text", "text": "current image request"},
                       {"type": "input_image", "image_url": "data:image/png;base64,fixture"}]}]
        self.assertEqual(proxy.send(history, model="gpt-6.1-sol", max_output_tokens=cap)[0], 200)
        status, stats = proxy.request("GET", "/carry/dashboard/stats")
        self.assertEqual(status, 200)
        estimate = stats["sessions"][0]["cost_estimate"]
        self.evidence.update(estimate=estimate, trajectory=proxy.saved()["cost_trajectory"],
                             primaries=fixture.primaries, reviews=fixture.reviews,
                             basis="synthetic image receipt; no projected-image allocation")
        self.assertEqual(len(fixture.primaries), 1)
        self.assertEqual(fixture.primaries[0]["input"], history)
        self.assertEqual(fixture.primaries[0]["max_output_tokens"], cap)
        self.assertEqual(fixture.reviews, [])
        self.assertAlmostEqual(estimate["observed_primary_cost"], 0.402, places=12)
        self.assertEqual(proxy.saved()["primary"]["calls"], 1)
        self.assertEqual(estimate["traditional_compactions"], 0)
        self.assertEqual(self.evidence["trajectory"]["result"]["summary_cost"], 0.0)
        return estimate

    def test_unchanged_image_with_small_output_remains_priceable(self):
        estimate = self.image_output_comparison(1)
        self.assertEqual(estimate["summary_cost"], 0.0)
        self.assertAlmostEqual(estimate["traditional_cost"], estimate["observed_primary_cost"], places=12)
        self.assertEqual(estimate["coverage"]["unmodeled_calls"], 0)
        self.assertEqual(estimate["matched_primary_responses"], 1)
        self.assertEqual(estimate.get("initial_compactions", 0), 0)

    def test_output_triggered_image_initialization_is_unavailable(self):
        estimate = self.image_output_comparison(100000)
        self.assertIsNone(estimate["traditional_cost"],
                          "initialization cannot allocate native image tokens to a projected slice")
        self.assertIsNone(estimate["savings"])
        self.assertIsNone(estimate["summary_cost"])
        self.assertEqual(estimate["coverage"]["unmodeled_calls"], 1)
        self.assertEqual(estimate["excluded_unpriced_calls"], 1)
        self.assertEqual(estimate["matched_primary_responses"], 0)
        self.assertEqual(estimate["initial_compactions"], 0)

    def test_requested_output_triggers_fitting_initialization_before_first_primary(self):
        def primary(_body, _n):
            response = support.completed()
            response["usage"] = {"input_tokens": 201000, "output_tokens": 0,
                                 "input_tokens_details": {"cached_tokens": 0}}
            return 200, response

        fixture = self.harness.fixture(primary, lambda *_: self.fail("unexpected reviewer"))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        proxy = support.Proxy(self.harness.state, fixture, mode="audit",
                              extra=["--primary-context-tokens", "1048576"],
                              extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS": "1000000"})
        self.harness.stack.callback(proxy.close)
        history = [{"role": "assistant", "content": " evidence" * 200000},
                   {"role": "user", "content": "current request"}]
        self.assertEqual(proxy.send(history, model="gpt-6.1-sol", max_output_tokens=100000)[0], 200)
        status, stats = proxy.request("GET", "/carry/dashboard/stats")
        self.assertEqual(status, 200)
        estimate = stats["sessions"][0]["cost_estimate"]
        self.evidence.update(estimate=estimate, trajectory=proxy.saved()["cost_trajectory"],
                             primaries=fixture.primaries, reviews=fixture.reviews,
                             basis="input alone fits; requested output triggers fitting initialization")
        self.assertEqual(fixture.primaries[0]["input"], history)
        self.assertEqual(fixture.primaries[0]["max_output_tokens"], 100000)
        self.assertEqual(fixture.reviews, [])
        self.assertAlmostEqual(estimate["observed_primary_cost"], 0.402, places=12)
        self.assertAlmostEqual(estimate["traditional_cost"], 0.0525, places=12)
        self.assertEqual(estimate["initial_compactions"], 1)
        self.assertEqual(estimate["traditional_compactions"], 0)
        self.assertEqual(estimate["summary_cost"], 0.0)

    def test_requested_output_that_cannot_fit_with_current_input_is_unavailable(self):
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
        history = [{"role": "assistant", "content": " evidence" * 60000},
                   {"role": "user", "content": " current" * 200000}]
        # Protected input fits the ordinary economic input budget by itself,
        # but not alongside this requested output allowance. The actual route
        # has ample capacity, so this must not fail primary wire admission.
        self.assertEqual(proxy.send(history, model="gpt-6.1-sol", max_output_tokens=100000)[0], 200)
        status, stats = proxy.request("GET", "/carry/dashboard/stats")
        self.assertEqual(status, 200)
        estimate = stats["sessions"][0]["cost_estimate"]
        self.evidence.update(estimate=estimate, trajectory=proxy.saved()["cost_trajectory"],
                             primaries=fixture.primaries, reviews=fixture.reviews,
                             basis="output reservation versus protected current input; synthetic receipt")
        self.assertEqual(len(fixture.primaries), 1)
        self.assertEqual(fixture.primaries[0]["input"], history)
        self.assertEqual(fixture.primaries[0]["max_output_tokens"], 100000)
        self.assertEqual(fixture.reviews, [])
        self.assertAlmostEqual(estimate["observed_primary_cost"], 261000 * 2.0 / 1e6, places=12)
        self.assertEqual(proxy.saved()["primary"]["calls"], 1)
        self.assertIsNone(estimate["traditional_cost"],
                          "requested output and protected input cannot fit the economic context together")
        self.assertIsNone(estimate["savings"])

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

    def test_fixed_tools_that_cannot_fit_are_explicitly_unavailable(self):
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
        tools = [{"type": "function", "name": "owned", "description": " fixed" * 256000,
                  "parameters": {"type": "object", "properties": {}}}]
        self.assertEqual(proxy.send(history, model="gpt-6.1-sol", tools=tools,
                                    max_output_tokens=1)[0], 200)
        status, stats = proxy.request("GET", "/carry/dashboard/stats")
        self.assertEqual(status, 200)
        estimate = stats["sessions"][0]["cost_estimate"]
        self.evidence.update(estimate=estimate, trajectory=proxy.saved()["cost_trajectory"],
                             primaries=fixture.primaries, reviews=fixture.reviews,
                             basis="fixed tools exceed economic budget; synthetic receipt")
        self.assertEqual(fixture.primaries[0]["tools"], tools)
        self.assertEqual(fixture.primaries[0]["input"], history)
        self.assertEqual(fixture.reviews, [])
        self.assertAlmostEqual(estimate["observed_primary_cost"], 0.522, places=12)
        self.assertEqual(proxy.saved()["primary"]["calls"], 1)
        self.assertIsNone(estimate["traditional_cost"], "fixed tools cannot be hypothetically retired")
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
