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
        self.addCleanup(self.cleanup_harness)
        self.evidence = self.harness.evidence

    def cleanup_harness(self):
        self.assertTrue(self.harness.doCleanups(), "nested fixture cleanup failed")

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
            return 200, advice_response({"protected": [2] if observed else [],
                "removable": [] if observed else [2], "memories": []})
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
        self.assertEqual(proxy.advice()["opinions"].get("2"), "keep")
        self.assertIn("UNIQUE_LIVE_SOURCE", json.dumps(fixture.primaries[-1]["input"]))

    def reviewer_prefix_capture(self, review=None):
        fixture = self.fixture(review)
        proxy = self.harness.proxy(fixture, ["--min-payback-percent", "100"])
        history = [{"role": "user", "content": "Preserve the source prefix exactly."}]
        for n in range(32):
            history.extend([{"role": "assistant", "content": f"Source {n}: " + "evidence " * 32},
                            {"role": "user", "content": f"Continue {n}."}])
        for n in range(3):
            self.assertEqual(proxy.send(history, instructions="Fixed task direction.")[0], 200)
            history.extend([{"role": "assistant", "content": f"Appended outcome {n}."},
                            {"role": "user", "content": f"Next operation {n}."}])
        self.assertGreaterEqual(len(fixture.reviews), 2)
        before, after = fixture.reviews[-2:]
        self.assertGreater(len(before["input"]), 48)
        # Only the final opinion ledger is mutable; every previously transmitted
        # observation must survive byte-for-byte, including native wrappers.
        stable = before["input"][:-1]
        self.assertEqual(wire(after["input"][:len(stable)]), wire(stable))
        self.assertEqual({k: v for k, v in before.items() if k != "input"},
                         {k: v for k, v in after.items() if k != "input"})
        events = [json.loads(line) for path in (proxy.state / "diagnostics").glob("*.jsonl")
                  for line in path.read_text().splitlines()]
        records = [e["data"] for e in events if e["event"] == "review_wire"]
        self.evidence.update(reviews=fixture.reviews, wire_diagnostics=records)
        return records

    def test_reviewer_implicit_write_boundary_is_reusable_after_ledger_changes(self):
        # Documented GPT-5.6+ implicit behavior: WRITE only the latest
        # eligible message ending; LOOKUP up to 20 earlier eligible endings.
        # This is a credential-free protocol simulator, not provider evidence.
        cached_prefixes, hits = set(), []
        def review(body, _n):
            import hashlib
            import sys
            from pathlib import Path
            sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
            from proxy_reviewer_cache_fixture import implicit_eligible_endings
            base = {k: v for k, v in body.items() if k not in ("input", "stream")}
            eligible = implicit_eligible_endings(body["input"])
            def identity(end):
                return hashlib.sha256(wire([base, body["input"][:end]])).hexdigest()
            hit = any(identity(end) in cached_prefixes for end in eligible[-21:])
            hits.append(hit)
            cached_prefixes.add(identity(eligible[-1]))
            response = advice_response({"protected": ledger(body)["eligible_group_ids"],
                                        "removable": [], "memories": []})
            response["usage"]["input_tokens"] = 6000
            response["usage"]["input_tokens_details"]["cached_tokens"] = 4000 if hit else 0
            return 200, response
        self.reviewer_prefix_capture(review)
        self.evidence["synthetic_implicit_hits"] = hits
        self.assertFalse(hits[0], "first request must really start cold")
        self.assertTrue(hits[-1],
                        "unchanged observations must retain the actual implicit WRITE endpoint, not merely some earlier bytes")

    def test_long_reviewer_prefix_is_exact_on_the_wire(self):
        self.reviewer_prefix_capture()

    def test_long_reviewer_stable_prefix_has_complete_diagnostic_identity(self):
        records = self.reviewer_prefix_capture()
        self.assertTrue(records[-1].get("prior_stable_prefix_equal"),
                        "complete stable-prefix identity must not depend on the 48-item detail cap")
        self.assertGreater(records[-1]["prior_stable_prefix_items"], 48)
        self.assertGreater(records[-1]["prior_stable_prefix_bytes"], 1024)

    def test_image_receipt_preserves_known_observed_cost(self):
        fixture = self.fixture()
        proxy = self.harness.proxy(fixture, ["--review-every-requests", "100"])
        history = [{"role": "user", "content": [
            {"type": "input_text", "text": "Read this synthetic image."},
            {"type": "input_image", "image_url": "data:image/png;base64,TEST_ONLY_NOT_DECODED"}]}]
        self.assertEqual(proxy.send(history)[0], 200)
        _, stats = proxy.request("GET", "/carry/dashboard/stats")
        row = stats["sessions"][0]
        self.evidence.update(stats=stats, saved=proxy.saved(), primaries=fixture.primaries)
        estimate = row["cost_estimate"]
        self.assertGreater(row["primary"]["cost_usd"], 0,
                           "raw native ledger is independently priceable")
        self.assertEqual(estimate["unavailable_primary_cost_calls"], 0,
                         "unsupported comparison must not erase a valid observed image receipt")
        self.assertAlmostEqual(estimate["observed_primary_cost"],
                               row["primary"]["cost_usd"])

    def test_unchanged_image_comparison_uses_receipt_input_and_survives_restart(self):
        fixture = self.fixture()
        options = ["--review-every-requests", "100"]
        proxy = self.harness.proxy(fixture, options)
        history = [{"role": "user", "content": [
            {"type": "input_text", "text": "Read this synthetic image."},
            {"type": "input_image", "image_url": "data:image/png;base64,TEST_ONLY_NOT_DECODED"}]}]
        self.assertEqual(proxy.send(history)[0], 200)
        proxy.close()
        proxy = self.harness.proxy(fixture, options)
        self.assertEqual(proxy.send(history)[0], 200)
        _, stats = proxy.request("GET", "/carry/dashboard/stats")
        estimate = stats["sessions"][0]["cost_estimate"]
        self.evidence.update(stats=stats, saved=proxy.saved(), primaries=fixture.primaries)
        self.assertEqual(estimate["matched_primary_responses"], 2,
                         "unchanged native image input has exact request-total receipt units")
        self.assertEqual(estimate["unavailable_primary_cost_calls"], 0)
        self.assertEqual(estimate["coverage"]["unmodeled_calls"], 0)
        self.assertGreater(estimate["carry_cost"], 0)

    def test_state01_top_level_instructions_observed_as_data(self):
        self.instructions(True)

    def test_state01_native_instruction_data_control(self):
        self.instructions(False)

    def successive_review(self, release, restart=False):
        """Scripted advice tests existing semantics, not model instruction-following."""
        constraint = ("OLDER_STILL_RELEVANT: do not change the on-disk format. "
                      "No compatible migrator was found; migration failed on legacy files.")
        fact = "Verification completed; the existing on-disk format is unchanged."
        cohort = [
            {"type": "reasoning", "encrypted_content": "SUPERSEDED_REASONING", "summary": []},
            {"type": "function_call", "call_id": "verify", "name": "shell",
             "arguments": "{\"command\":\"verify-format\"}"},
            {"type": "function_call_output", "call_id": "verify",
             "output": "SUPERSEDED_TOOL_DETAIL " * 1200 + fact}]
        history = [{"role": "user", "content": "verify the storage change"},
                   {"role": "assistant", "content": constraint},
                   {"role": "user", "content": "inspect and verify"},
                   *cohort, {"role": "user", "content": "finish verification"}]

        def review(body, n):
            state = ledger(body)
            self.assertIn(2, state["eligible_group_ids"])
            if n <= 2:
                self.assertIn(4, state["eligible_group_ids"])
                group = next(g for g in state["current_groups"] if g["group_id"] == 4)
                self.assertEqual(group["member_ids"], [4, 5, 6])
                for item in cohort:
                    self.assertEqual(body["input"].count(item), 1)
                if n == 2:
                    self.assertEqual(group["opinion"], "keep",
                                     "reconsider actual previously committed protection")
                    self.assertIn("VERIFIED_SUCCESSOR", json.dumps(body["input"]))
            releasing = release and n == 2
            return 200, advice_response({
                "protected": [2] if releasing or n > 2 else [2, 4],
                "removable": [4] if releasing else [],
                "memories": [{"source_ids": [4], "text": fact}] if releasing else []})

        fixture = self.fixture(review)
        options = ["--classifier-cache-policy", "disabled"]
        proxy = self.harness.proxy(fixture, options)
        for _ in range(2):
            self.assertEqual(proxy.send(history)[0], 200)
        protected = proxy.advice()
        self.assertEqual(protected["opinions"].get("2"), "keep")
        for member in (4, 5, 6):
            self.assertEqual(protected["opinions"].get(str(member)), "keep")
        history += [{"role": "assistant", "content": "VERIFIED_SUCCESSOR: moving to the next check."},
                    {"role": "user", "content": "continue with the next storage check"}]
        self.assertEqual(proxy.send(history)[0], 200)
        reconsidered = proxy.advice()
        self.assertEqual(len(fixture.reviews), 2)
        for member in (4, 5, 6):
            self.assertEqual(reconsidered["opinions"].get(str(member)), "drop" if release else "keep")
        self.assertEqual(reconsidered["opinions"].get("2"), "keep")
        self.assertEqual(reconsidered["feedback"]["rejected_count"], 0)
        self.assertEqual([m["source_ids"] for m in reconsidered["memories"]],
                         [[4, 5, 6]] if release else [])
        if restart:
            proxy.close()
            proxy = self.harness.proxy(fixture, options)
        self.assertEqual(proxy.send(history)[0], 200)
        projected = fixture.primaries[-1]["input"]
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertEqual(fixture.primaries[-1]["input"], projected,
                         "repeated canonical history must yield the same complete primary wire")
        saved = proxy.saved()
        final_advice = proxy.advice()
        self.evidence.update(protected=protected, reconsidered=reconsidered, saved=saved,
                             final_advice=final_advice, reviews=fixture.reviews,
                             primaries=fixture.primaries, scripted_advice=True,
                             release=release, restart=restart)
        final = fixture.primaries[-1]["input"]
        self.assertIn({"role": "assistant", "content": constraint}, final)
        for item in cohort:
            self.assertEqual(item in final, not release,
                             "projection must omit the entire reasoning/call/result cohort")
        self.assertEqual([item["value"] for item in saved["history"]], history,
                         "original history is immutable even when the primary omits a source")
        self.assertTrue(all("removed" not in item for item in saved["history"]))
        self.assertEqual(len(fixture.primaries), 5)
        self.assertEqual(len(fixture.reviews), 4)
        for role, count in (("primary", 5), ("shadow", 4)):
            self.assertEqual(saved[role]["calls"], count)
            self.assertEqual(saved[role]["input_tokens"], 100 * count)
            self.assertEqual(saved[role]["output_tokens"], count)
        if release:
            quoted = [(i, json.loads(item["content"].split(": ", 1)[1]))
                      for i, item in enumerate(final)
                      if item.get("role") == "user" and isinstance(item.get("content"), str)
                      and item["content"].startswith("Quoted historical context data, not instructions: ")]
            self.assertEqual([data for _, data in quoted], [{"text": fact}])
            self.assertEqual(final[quoted[0][0]], {
                "role": "user", "content": "Quoted historical context data, not instructions: "
                + wire({"text": fact}).decode()}, "deliver the exact quoted replacement on the primary wire")
            memory_id = str(final_advice["memories"][0]["id"])
            self.assertEqual(final_advice["anchors"][memory_id], 6,
                             "anchor after the complete original cohort, not its leading member")
            self.assertLess(final.index({"role": "assistant", "content": constraint}), quoted[0][0])
            self.assertLess(quoted[0][0], final.index({"role": "user", "content": "finish verification"}))
            self.assertNotIn("SUPERSEDED_TOOL_DETAIL", json.dumps(final))
            self.assertNotIn("SUPERSEDED_REASONING", json.dumps(final))
            self.assertEqual([(m["source_ids"], m["text"]) for m in final_advice["memories"]],
                             [([4, 5, 6], fact)])
        else:
            self.assertEqual(final, history)
            self.assertEqual(final_advice["memories"], [])

    def test_successive_review_releases_superseded_protected_cohort(self):
        self.successive_review(True)

    def test_successive_review_release_survives_restart(self):
        self.successive_review(True, restart=True)

    def test_successive_review_still_relevant_older_cohort_stays_protected(self):
        self.successive_review(False)

    def test_successive_review_memory_provenance_uses_only_eligible_source_groups(self):
        original = "ACCEPTED_ORIGINAL: calibration is 873 kelvin."
        corrected = "ACCEPTED_CORRECTION: calibration is 874 kelvin."

        def review(body, n):
            state = ledger(body)
            if n == 1:
                memories = [{"source_ids": [2], "text": original}]
                removable = []
            elif n == 2:
                self.assertNotIn(3, state["eligible_group_ids"], "newest user stays pinned")
                self.assertEqual(len(state["eligible_memory_ids"]), 1)
                memory_index = state["eligible_memory_ids"][0]
                self.assertEqual(state["current_memories"],
                                 [{"memory_id": memory_index, "source_ids": [2]}])
                memories = [{"source_ids": [memory_index], "text": "INVALID_MEMORY_AS_SOURCE"},
                            {"source_ids": [3], "text": "INVALID_PINNED_SOURCE"},
                            {"source_ids": [999999], "text": "INVALID_UNKNOWN_SOURCE"},
                            {"source_ids": [2], "text": corrected}]
                removable = [memory_index]
            else:
                memories, removable = [], []
            return 200, advice_response({"protected": [2],
                                        "removable": removable, "memories": memories})

        fixture = self.fixture(review)
        options = ["--min-payback-percent", "100"]
        proxy = self.harness.proxy(fixture, options)
        for _ in range(3):
            self.assertEqual(proxy.send(self.seed())[0], 200)
        accepted = proxy.advice()
        self.assertEqual(accepted["feedback"]["rejected_count"], 3)
        self.assertEqual(accepted["feedback"]["accepted_memories"], 1)
        self.assertEqual([(m["source_ids"], m["text"]) for m in accepted["memories"]],
                         [([2], corrected)])
        self.assertEqual(accepted["opinions"].get("2"), "keep")
        self.assertEqual(fixture.primaries[-1]["input"], self.seed())
        proxy.close()
        proxy = self.harness.proxy(fixture, options)
        self.assertEqual(proxy.send(self.seed())[0], 200)
        self.evidence.update(accepted=accepted, saved=proxy.saved(), final_advice=proxy.advice(),
                             reviews=fixture.reviews, primaries=fixture.primaries, scripted_advice=True)
        self.assertEqual(len(fixture.reviews), 3)
        self.assertNotIn(original, json.dumps(fixture.reviews[-1]["input"]))
        self.assertIn(corrected, json.dumps(fixture.reviews[-1]["input"]))
        self.assertNotIn("INVALID_", json.dumps(fixture.reviews[-1]["input"]))
        self.assertEqual([(m["source_ids"], m["text"]) for m in proxy.advice()["memories"]],
                         [([2], corrected)])

    def memory_retirement(self, restart, retire=True):
        fact = "RETIRED_UNIQUE_FACT_8e54: the derived calibration is 873 kelvin."
        def review(body, n):
            if n == 1:
                advice = {"protected": [2], "removable": [1],
                          "memories": [{"source_ids": [2], "text": fact}]}
            else:
                advice = {"protected": [], "removable": [ledger(body)["eligible_memory_ids"][0]] if retire and n == 2 else [],
                          "memories": []}
            return 200, advice_response(advice)
        fixture = self.fixture(review)
        options = ["--min-payback-percent", "100", "--classifier-cache-policy", "disabled"]
        proxy = self.harness.proxy(fixture, options)
        history = self.seed()
        self.assertNotIn(fact, json.dumps(history))
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertEqual(proxy.send(history)[0], 200)  # Review 1 accepts m1.
        accepted = proxy.advice()
        self.assertEqual([m["text"] for m in accepted["memories"]], [fact])
        self.assertEqual(proxy.send(history)[0], 200)  # Review 2 retires m1.
        retired = proxy.advice()
        self.assertEqual(len(fixture.reviews), 2)
        self.assertEqual(retired["memories"], [] if retire else accepted["memories"])
        self.assertEqual(retired["opinions"], accepted["opinions"])
        self.assertEqual(proxy.saved()["history"][1]["value"], history[1],
                         "fact retirement cannot edit original source")
        if restart:
            proxy.close()
            proxy = self.harness.proxy(fixture, options)
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertEqual(len(fixture.reviews), 3)
        saved = proxy.saved()
        self.evidence.update(accepted=accepted, retired=retired, saved=saved,
                             reviews=fixture.reviews, restart=restart)
        self.assertEqual(proxy.advice()["opinions"], accepted["opinions"],
                         "retirement/rebuild must retain accepted group decisions")
        self.assertEqual(saved["compactions"], 0)
        if retire:
            self.assertNotIn("active_shadow", saved)
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
        self.assertEqual(proxy.advice()["opinions"].get("2"), "keep")
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
        self.assertEqual(proxy.advice()["opinions"].get("2"), "keep")
        self.assertTrue(all("removed" not in item for item in after["history"]))
        for item in cohort:
            self.assertEqual(sum(i == item for i in second), 1,
                             "new reasoning/calls/results must appear once as complete native items")
        self.assertEqual(json.dumps(second).count("UNIQUE_LIVE_SOURCE"), 1)
        self.assertFalse(prefix, "fresh projection replaces its ledger rather than appending answers")
        self.assertNotIn("REVIEW_REASONING", json.dumps(second))
        self.assertNotIn(outputs[0][-1], second)

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
        self.assertEqual(proxy.saved()["shadow"]["calls"], len(fixture.reviews))

    def test_planner01_age_one_cadence_100_has_no_future_review(self):
        self.schedule(100, False)

    def test_planner01_age_one_cadence_100_no_future_review_after_restart(self):
        self.schedule(100, True)

    def test_planner01_due_dispatch_control(self):
        self.schedule(1, False)

    def test_planner01_due_without_eligible_sources_has_no_future_calls(self):
        self.schedule(1, False, eligible=False)

    def test_selected_source_removal_does_not_starve_due_review(self):
        def review(_body, _n):
            return 200, advice_response({"protected": [1], "removable": [2], "memories": []})
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
        self.assertEqual(actual, 1, "selected omission cannot starve a review otherwise due")
        self.assertNotIn("UNIQUE_LIVE_SOURCE", json.dumps(fixture.primaries[-1]["input"]))
        self.assertEqual(saved["shadow"]["calls"], len(fixture.reviews))

    def test_fresh_projection_discards_prior_reviewer_reasoning_and_still_progresses(self):
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
        self.assertNotIn("disposable reviewer reasoning", json.dumps(fixture.reviews[-1]))
        self.assertEqual(after["compactions"], 0)
        self.assertEqual(len(fixture.reviews), 2, "reviewer-only cleanup does not skip due dispatch")
        self.assertEqual(fixture.primaries[-1]["input"], fixture.primaries[-2]["input"])

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
        self.assertEqual(proxy.advice()["len"], len(self.seed()))
        proxy.close()
        proxy = self.harness.proxy(fixture, options)
        self.assertEqual(proxy.send(self.seed(), instructions="BOOTSTRAP_DIRECTION")[0], 200)
        after = proxy.saved()
        self.evidence.update(before=before, after=after, reviews=fixture.reviews)
        self.assertEqual(after["shadow"]["calls"], before["shadow"]["calls"])
        self.assertEqual(len(fixture.reviews), 1)
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
        blocked = known and forced
        self.assertEqual(status, 413 if blocked else 200)
        self.assertEqual(len(fixture.primaries), 1 if blocked else 2)
        if not known:
            self.assertEqual(fixture.primaries[-1]["input"], history,
                             "unknown models bypass Carry context admission unchanged")
            self.assertEqual(saved["primary"]["unavailable_cost_calls"], 2)
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
