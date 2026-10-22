"""PR144 behavioral regressions; real CLI, synthetic receipts/auth, loopback only.

CARRY_TEST_BINARY=/owned/carry python3 -B tests/proxy_review_regressions.py -v
Economic private helpers additionally require the source-bound Rust unit target.
"""
import base64
import json
import unittest

import proxy_state_regressions as support
from proxy_state_regressions import advice_response, completed, ledger, wire


class ReviewRegressions(unittest.TestCase):
    def setUp(self):
        self.harness = support.StateRegressions()
        self.harness.setUp()
        # Use the actual selector for per-case durable loopback evidence.
        self.harness._testMethodName = self._testMethodName
        self.addCleanup(self.harness.doCleanups)
        self.evidence = self.harness.evidence

    @staticmethod
    def seed():
        return [{"role": "user", "content": "durable goal"},
                {"role": "assistant", "content": "UNIQUE_LIVE_SOURCE calibration result"},
                {"role": "user", "content": "continue task"}]

    def fixture(self, review=None, primary=None):
        if primary is None:
            def primary(_body, n):
                return 200, completed(response_id=f"r{n}")
        if review is None:
            def review(body, _n):
                return 200, advice_response({"protected": ledger(body)["eligible_group_ids"],
                                             "removable": [], "memories": []})
        fixture = self.harness.fixture(primary, review)
        # Child TestCase cleanup outcomes do not propagate to this TestCase.
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        return fixture

    def instructions(self, top_level):
        requirement = "ONLY_TASK_DIRECTION preserve calibration delta ZQK7"
        def review(body, _n):
            observed = requirement in json.dumps(body["input"])
            return 200, advice_response({"protected": ["g2"] if observed else [],
                "removable": [] if observed else ["g2"], "memories": []})
        fixture = self.fixture(review)
        proxy = self.harness.proxy(fixture, ["--min-payback-percent", "100"])
        history = self.seed()
        fields = {"instructions": requirement} if top_level else {}
        if not top_level:
            history[0]["content"] += " " + requirement
        for _ in range(3):
            self.assertEqual(proxy.send(history, **fields)[0], 200)
        self.evidence.update(reviews=fixture.reviews, primaries=fixture.primaries,
                             saved=proxy.saved())
        self.assertEqual(len(fixture.reviews), 2)
        if top_level:
            self.assertTrue(all(b["instructions"] == requirement for b in fixture.primaries))
        for body in fixture.reviews:
            self.assertNotIn(requirement, body["instructions"],
                             "primary directions must not become reviewer authority")
            self.assertTrue(any(i.get("role") == "user" and requirement in json.dumps(i)
                                for i in body["input"]),
                            "STATE01: top-level task instructions must be observed as data")
        self.assertEqual(proxy.saved()["opinions"].get("2"), "keep")
        self.assertIn("UNIQUE_LIVE_SOURCE", json.dumps(fixture.primaries[-1]["input"]))

    def test_state01_top_level_instructions_observed_as_data(self):
        self.instructions(True)

    def test_state01_native_instruction_data_control(self):
        self.instructions(False)

    def memory_retirement(self, restart, retire=True):
        fact = "RETIRED_UNIQUE_FACT_8e54: the derived calibration is 873 kelvin."
        def review(body, n):
            if n == 1:
                advice = {"protected": ["g2"], "removable": ["g1"],
                          "memories": [{"source_ids": ["g2"], "text": fact}]}
            else:
                advice = {"protected": [], "removable": ["m1"] if retire and n == 2 else [],
                          "memories": []}
            return 200, advice_response(advice)
        fixture = self.fixture(review)
        options = ["--min-payback-percent", "100", "--classifier-cache-policy", "disabled"]
        proxy = self.harness.proxy(fixture, options)
        history = self.seed()
        self.assertNotIn(fact, json.dumps(history))
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertEqual(proxy.send(history)[0], 200)  # Review 1 accepts m1.
        accepted = proxy.saved()
        self.assertEqual([m["text"] for m in accepted["memories"]], [fact])
        self.assertEqual(proxy.send(history)[0], 200)  # Review 2 retires m1.
        retired = proxy.saved()
        self.assertEqual(len(fixture.reviews), 2)
        self.assertEqual(retired["memories"], [] if retire else accepted["memories"])
        self.assertEqual(retired["opinions"], accepted["opinions"])
        self.assertTrue(all(not item["removed"] for item in retired["history"]),
                        "memory sources must remain live; removal must not mask stale replay")
        if restart:
            proxy.close()
            proxy = self.harness.proxy(fixture, options)
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertEqual(len(fixture.reviews), 3)
        saved = proxy.saved()
        self.evidence.update(accepted=accepted, retired=retired, saved=saved,
                             reviews=fixture.reviews, restart=restart)
        self.assertEqual(saved["opinions"], accepted["opinions"],
                         "retirement/rebuild must retain accepted group decisions")
        self.assertEqual(saved["compactions"], 0)
        if retire:
            self.assertFalse(any(r["source_id"] == 0 for r in saved["active_shadow"]))
            self.assertNotIn(fact, json.dumps(fixture.reviews[2]),
                             "STATE02: retirement removes fact from ENTIRE subsequent replay")
        else:
            self.assertIn(fact, json.dumps(fixture.reviews[2]["input"]))

    def test_state02_retirement_prunes_entire_replay_with_live_sources(self):
        self.memory_retirement(False)

    def test_state02_retirement_prunes_entire_replay_after_restart(self):
        self.memory_retirement(True)

    def test_state02_unretired_fact_and_decisions_survive_restart_control(self):
        self.memory_retirement(True, False)

    def test_state03_growing_cohort_preserves_prefix_or_accounts_rewrite(self):
        outputs = []
        def review(body, n):
            answer = advice_response({"protected": ledger(body)["eligible_group_ids"],
                                      "removable": [], "memories": []})
            answer["output"].insert(0, {"type": "reasoning", "encrypted_content": f"REVIEW_REASONING_{n}",
                                         "summary": []})
            answer["output"][-1]["phase"] = "final_answer"
            outputs.append(answer["output"])
            return 200, answer
        fixture = self.fixture(review)
        proxy = self.harness.proxy(fixture, ["--min-payback-percent", "100",
                                             "--classifier-cache-policy", "disabled"])
        seed = self.seed()[:2]
        for _ in range(2):
            self.assertEqual(proxy.send(seed)[0], 200)
        before = proxy.saved()
        self.assertEqual(before["opinions"].get("2"), "keep")
        cohort = [
            {"type": "reasoning", "encrypted_content": "NEW_NATIVE_REASONING", "summary": []},
            {"type": "function_call", "call_id": "grow", "name": "inspect", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "grow", "output": "NEW_TOOL_RESULT"}]
        grown = seed + cohort + [{"role": "user", "content": "next after tool"}]
        self.assertEqual(proxy.send(grown)[0], 200)
        self.assertEqual(len(fixture.reviews), 2)
        after = proxy.saved()
        first, second = [b["input"] for b in fixture.reviews]
        prefix = second[:len(first)] == first
        self.evidence.update(reviews=fixture.reviews, outputs=outputs, before=before,
                             after=after, previous_input_preserved=prefix)
        self.assertEqual(after["compactions"], 0)
        self.assertEqual(after["opinions"].get("2"), "keep")
        self.assertTrue(all(not item["removed"] for item in after["history"]))
        for item in cohort:
            self.assertEqual(sum(i == item for i in second), 1,
                             "new reasoning/calls/results must appear once as complete native items")
        self.assertEqual(json.dumps(second).count("UNIQUE_LIVE_SOURCE"), 1)
        if prefix:
            self.assertEqual(second[len(first):len(first) + len(outputs[0])], outputs[0],
                             "append exact reviewer reasoning and phase-tagged answer before new sources")
            self.assertEqual(after["reviewer_compactions"], before["reviewer_compactions"])
        else:
            self.assertGreater(after["reviewer_compactions"], before["reviewer_compactions"],
                               "STATE03: a native cohort rebuild must be an explicit accounted rewrite")

    def schedule(self, cadence, restart, eligible=True):
        fixture = self.fixture()
        history = self.seed() if eligible else self.seed()[:1]
        options = ["--payoff-requests", "1", "--review-every-requests", str(cadence),
                   "--min-payback-percent", "100"]
        proxy = self.harness.proxy(fixture, options)
        self.assertEqual(proxy.send(history)[0], 200)
        if restart:
            proxy.close()
            proxy = self.harness.proxy(fixture, options)
        before = len(fixture.reviews)
        self.assertEqual(proxy.send(history)[0], 200)
        actual = len(fixture.reviews) - before
        report = proxy.saved()["last_plan"]
        self.evidence.update(report=report, actual_calls=actual, saved=proxy.saved())
        self.assertEqual(actual, int(cadence == 1 and eligible))
        self.assertEqual(report["future_reviewer_calls"], actual,
                         "PLANNER01: one-request forecast must match actual age/eligibility dispatch")
        for candidate in report["candidates"]:
            if actual == 0 and candidate["joint"] is not None:
                self.assertEqual(candidate["joint"]["future_shadow_keep"], 0)
                self.assertEqual(candidate["joint"]["future_shadow_compact"], 0)

    def test_planner01_age_one_cadence_100_has_no_future_review(self):
        self.schedule(100, False)

    def test_planner01_age_one_cadence_100_no_future_review_after_restart(self):
        self.schedule(100, True)

    def test_planner01_due_dispatch_control(self):
        self.schedule(1, False)

    def test_planner01_due_without_eligible_sources_has_no_future_calls(self):
        self.schedule(1, False, eligible=False)

    def test_planner01_source_removal_has_no_immediate_review_cost(self):
        def review(_body, _n):
            return 200, advice_response({"protected": ["g1"], "removable": ["g2"], "memories": []})
        fixture = self.fixture(review)
        proxy = self.harness.proxy(fixture, ["--payoff-requests", "1"])
        history = self.seed()
        history[1]["content"] *= 500
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertEqual(proxy.send(history)[0], 200)
        before = len(fixture.reviews)
        self.assertEqual(proxy.send(history)[0], 200)
        saved = proxy.saved()
        actual = len(fixture.reviews) - before
        self.evidence.update(actual_calls=actual, saved=saved, reviews=fixture.reviews)
        self.assertGreater(saved["compactions"], 0, "exercise actual source-removal branch")
        self.assertEqual(actual, 0, "source-removal branch deliberately skips ordinary dispatch")
        self.assertNotIn("UNIQUE_LIVE_SOURCE", json.dumps(fixture.primaries[-1]["input"]))
        rows = [r for r in saved["last_plan"]["candidates"] if 2 in r["removed_groups"]]
        self.assertTrue(rows)
        for row in rows:
            self.assertEqual(row["joint"]["future_shadow_compact"], 0,
                             "PLANNER01: source-removal branch must not charge a skipped review")

    def test_planner01_reviewer_only_rewrite_still_dispatches_due_review_control(self):
        def review(body, n):
            answer = advice_response({"protected": ledger(body)["eligible_group_ids"],
                                      "removable": [], "memories": []})
            # Native reasoning is disposable reviewer state, not an invented
            # checkpoint or unsupported advice field.
            if n == 1:
                answer["output"].insert(0, {"type": "reasoning", "summary": [],
                    "encrypted_content": "disposable reviewer reasoning " * 500})
            return 200, answer
        fixture = self.fixture(review)
        proxy = self.harness.proxy(fixture, ["--payoff-requests", "1"])
        for _ in range(2):
            self.assertEqual(proxy.send(self.seed())[0], 200)
        before = proxy.saved()
        self.assertEqual(proxy.send(self.seed())[0], 200)
        after = proxy.saved()
        self.evidence.update(before=before, after=after, reviews=fixture.reviews)
        self.assertGreater(after["reviewer_compactions"], before["reviewer_compactions"])
        self.assertEqual(after["compactions"], 0)
        self.assertEqual(len(fixture.reviews), 2, "reviewer-only cleanup does not skip due dispatch")
        self.assertEqual(fixture.primaries[-1]["input"], fixture.primaries[-2]["input"])
        for row in after["last_plan"]["candidates"]:
            self.assertEqual(row["removed_groups"], [])
            self.assertGreater(row["joint"]["future_shadow_compact"], 0)

    def test_planner01_completed_bootstrap_not_charged_again_after_restart(self):
        fixture = self.fixture()
        options = ["--review-replayed-history", "--payoff-requests", "1",
                   "--review-every-requests", "100", "--min-payback-percent", "100"]
        proxy = self.harness.proxy(fixture, options)
        self.assertEqual(proxy.send(self.seed(), instructions="BOOTSTRAP_DIRECTION")[0], 200)
        before = proxy.saved()
        self.assertEqual(len(fixture.reviews), 1, "setup executes a real completed bootstrap")
        body = fixture.reviews[0]
        self.assertEqual(wire(body).count(b"BOOTSTRAP_DIRECTION"), 1)
        self.assertIn(b"BOOTSTRAP_DIRECTION", wire(body["input"][1]),
                      "disposable bootstrap/import must carry primary direction before sources")
        self.assertNotIn("primary_instruction_version", ledger(body))
        self.assertTrue(before["bootstrap_review_complete"])
        proxy.close()
        proxy = self.harness.proxy(fixture, options)
        self.assertEqual(proxy.send(self.seed(), instructions="BOOTSTRAP_DIRECTION")[0], 200)
        after = proxy.saved()
        self.evidence.update(before=before, after=after, reviews=fixture.reviews)
        self.assertEqual(after["shadow"]["calls"], before["shadow"]["calls"])
        self.assertEqual(len(fixture.reviews), 1)
        self.assertEqual(after["last_plan"]["future_reviewer_calls"], 0,
                         "PLANNER01: completed bootstrap receipt is sunk, not future spend")

    def primary_price(self, marker, cached=0):
        def primary(_body, n):
            response = completed(response_id=f"price_{n}")
            response["usage"]["input_tokens_details"]["cached_tokens"] = cached
            return 200, response
        fixture = self.fixture(primary=primary)
        proxy = self.harness.proxy(fixture, ["--payoff-requests", "1",
            "--min-payback-percent", "100", "--classifier-cache-policy", "disabled"])
        history = self.seed()
        block = {"type": "input_text", "text": history[0]["content"]}
        if marker != "absent":
            block["prompt_cache_breakpoint"] = marker
        history[0]["content"] = [block]
        for _ in range(3):
            self.assertEqual(proxy.send(history)[0], 200)
        saved = proxy.saved()
        self.evidence.update(saved=saved, primaries=fixture.primaries, marker=marker, cached=cached)
        self.assertEqual(saved["compactions"], 0)
        self.assertEqual(saved["reviewer_compactions"], 0)
        self.assertEqual(fixture.primaries[-1], fixture.primaries[-2],
                         "actual primary body is identical, not merely the same token estimate")
        rows = saved["last_plan"]["candidates"]
        self.assertTrue(rows, "actual replay turns must yield reviewer-only candidates")
        for row in rows:
            self.assertEqual(row["removed_groups"], [])
            self.assertAlmostEqual(row["joint"]["primary_keep"], row["joint"]["primary_compact"],
                places=12, msg="PLANNER02: identical explicit/null-marked primary must have one price")

    def test_planner02_identical_explicit_primary_prices(self):
        self.primary_price({"mode": "explicit"})

    def test_planner02_implicit_body_symmetric_control(self):
        self.primary_price("absent")

    def test_planner02_compatible_explicit_receipt_still_has_one_price(self):
        self.primary_price({"mode": "explicit"}, cached=100)

    def output_reserve(self, declared, fallback, expected):
        # Same supported auth.json fixture as subscription_auth/proxy tests;
        # expiry is future and tokens are synthetic, so refresh is unreachable.
        auth = self.harness.state / "synthetic-codex"
        auth.mkdir()
        payload = base64.urlsafe_b64encode(wire({"https://api.openai.com/auth": {
            "chatgpt_account_id": "fixture-account"}})).decode().rstrip("=")
        (auth / "auth.json").write_bytes(wire({"version": 1,
            "access_token": f"header.{payload}.signature", "refresh_token": "fixture-refresh",
            "expires_at_ms": 4102444800000}))
        (auth / "auth.json").chmod(0o600)
        def primary(_body, n):
            response = completed(response_id=f"codex_{n}")
            return 200, b"data: " + wire({"type": "response.completed", "response": response}) + b"\n\n"
        fixture = self.fixture(primary=primary)
        proxy = self.harness.proxy(fixture, ["--codex-login", "--codex-home", str(auth),
            "--primary-context-tokens", "2000", "--primary-output-reserve-tokens", str(fallback),
            "--min-payback-percent", "100"])
        fields = {} if declared is None else {"max_output_tokens": declared}
        status, response = proxy.send([{"role": "user", "content": "small protected task"}], **fields)
        self.evidence.update(declared=declared, fallback=fallback, status=status, response=response,
                             primaries=fixture.primaries, saved=proxy.saved())
        for body in fixture.primaries:
            self.assertNotIn("max_output_tokens", body, "Codex wire remains native-compatible")
            self.assertEqual(body["stream"], True)
            self.assertEqual(body["store"], False)
        self.assertEqual(status, expected,
                         "SURFACE-L2: use client reserve for planning before Codex strips cap")
        self.assertEqual(len(fixture.primaries), int(expected == 200))

    def test_surface_l2_above_fallback_reserve_blocks_before_codex_wire(self):
        self.output_reserve(3000, 512, 413)

    def test_surface_l2_below_fallback_reserve_fits_codex_wire(self):
        self.output_reserve(512, 3000, 200)

    def test_surface_l2_absent_cap_uses_fitting_fallback_control(self):
        self.output_reserve(None, 512, 200)

    def test_surface_l2_absent_cap_uses_oversized_fallback_control(self):
        self.output_reserve(None, 3000, 413)

    def unknown_capacity(self, known=False, forced=True):
        fixture = self.fixture()
        proxy = self.harness.proxy(fixture, ["--primary-context-tokens", "3000",
            "--primary-output-reserve-tokens", "64", "--min-payback-percent", "100"])
        model = "gpt-6-luna" if known else "unknown-fixture-model"
        history = self.seed()
        self.assertEqual(proxy.send(history, model=model)[0], 200)
        self.assertEqual(len(fixture.reviews), 0)
        if forced:
            history += [{"role": "user", "content": "protected new material " * 2000}]
        status, _ = proxy.send(history, model=model)
        saved = proxy.saved()
        self.evidence.update(status=status, saved=saved, reviews=fixture.reviews,
                             primaries=fixture.primaries)
        self.assertEqual(status, 413 if forced else 200)
        self.assertEqual(len(fixture.primaries), 1 if forced else 2)
        expected_reviews = int(known and not forced)
        self.assertEqual(len(fixture.reviews), expected_reviews,
                         "no capacity rescue review; unknown prices never authorize paid review")
        self.assertEqual(saved["shadow"]["calls"], expected_reviews)

    def test_transport_sug06_forced_unknown_primary_never_spends_review(self):
        self.unknown_capacity()

    def test_transport_sug06_ordinary_unknown_primary_control(self):
        self.unknown_capacity(forced=False)

    def test_transport_sug06_forced_known_primary_never_spends_rescue_review(self):
        self.unknown_capacity(known=True)

    def test_transport_sug06_ordinary_known_primary_review_control(self):
        self.unknown_capacity(known=True, forced=False)


if __name__ == "__main__":
    unittest.main()
