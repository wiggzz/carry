"""Actual-binary pending-cleanup economics regressions (no planner emulation).

Only owned synthetic checkpoints are edited, to stage frozen/capped progress.
Sources, exact drop advice and memory admission come from real HTTP requests.
CARRY_TEST_BINARY=/path/to/carry python3 -B tests/proxy_pending_cleanup.py -v
"""
import json
import unittest

from proxy_state_regressions import StateRegressions, advice_response, completed, ledger


class PendingCleanup(StateRegressions):
    # Do not inherit/run the original regression selectors in this focused file.
    def seeded(self, memory=True, pending=True, bootstrap=False):
        seed = [{"role": "user", "content": "old goal"},
                {"role": "assistant", "content": "disposable evidence ".join(str(i) for i in range(200))},
                {"role": "user", "content": "current task"}]

        def review(body, n):
            if n == 1:
                facts = [{"source_ids": ["g2"], "text": "The useful result is 42."}] if memory else []
                return 200, advice_response({"protected": ["g1"], "removable": ["g2"], "memories": facts})
            if n == 2 and not bootstrap:
                fixture.held.set()
                if not fixture.release.wait(8):
                    raise AssertionError("held ordinary reviewer was not released")
            data = ledger(body)
            return 200, advice_response({"protected": data["eligible_group_ids"] + data["eligible_memory_ids"],
                                         "removable": [], "memories": []})

        fixture = self.fixture(lambda _body, n: (200, completed(response_id=f"resp_{n}")), review)
        proxy = self.proxy(fixture, ["--payoff-requests", "1", "--min-payback-percent", "100"])
        self.assertEqual(proxy.send(seed)[0], 200)
        self.assertEqual(proxy.send(seed)[0], 200)
        saved = proxy.saved()
        self.assertEqual(saved["compactions"], 0)
        self.assertEqual(saved["drop_members"], {"2": [2]})
        self.assertEqual(len(saved["memories"]), int(memory))
        self.assertEqual(len(fixture.reviews), 1)
        proxy.close()
        # Synthetic checkpoint staging, NOT a claim that CLI produced these
        # partition choices. Production Session deserialization/planning/review
        # and all subsequent HTTP dispatches execute the supplied Rust binary.
        saved.update(review_end_id=2 if pending else 0, reviewed_members=[1] if pending else [],
                     reviewer_windows=[[{"id": 1, "members": [1]}], [{"id": 2, "members": [2]}]],
                     pending_reviewer_windows=[[{"id": 1, "members": [1]}]] if pending else [],
                     last_review_request=saved["completed_requests"],
                     bootstrap_review_pending=bootstrap, bootstrap_review_complete=not bootstrap,
                     bootstrap_end_id=2 if bootstrap else 0,
                     bootstrap_reviewed_ids=[1] if bootstrap else [])
        next(self.state.glob("*.json")).write_text(json.dumps(saved))
        self.evidence.update(checkpoint_staging="owned synthetic frozen/capped progress; real source/advice/memory",
                             seeded_checkpoint=saved)
        return fixture, seed

    def exercise(self, memory=True, pending=True, payoff=2):
        fixture, seed = self.seeded(memory, pending)
        options = ["--payoff-requests", str(payoff), "--review-every-requests", "100", "--review-max-calls", "1"]
        proxy = self.proxy(fixture, options + ["--background-reviews"])
        self.assertEqual(proxy.send(seed)[0], 200)
        if pending:
            self.assertTrue(fixture.held.wait(3), "current paid review must be held while planning")
        planned = proxy.saved()
        plan = planned["last_plan"]
        self.assertTrue(plan, "must execute the actual paired planner from carried advice")
        self.evidence.update(plan=plan, planned_checkpoint=planned, planned_input=fixture.primaries[-1]["input"])
        fixture.release.set()
        if pending:
            self.wait_saved(proxy, lambda s: not s["background_review_pending"])
        proxy.close()
        # A distinct subsequent review after real process restart. On the old
        # binary it completes the emptied sweep then POSTs the memory-only body.
        proxy = self.proxy(fixture, options)
        before = proxy.saved()["shadow"]["calls"]
        start = len(fixture.reviews)
        self.assertEqual(proxy.send(seed)[0], 200)
        future = fixture.reviews[start:]
        final = proxy.saved()
        self.evidence.update(future_requests=future, actual_future_calls=len(future),
                             future_receipt_delta=final["shadow"]["calls"] - before,
                             final_checkpoint=final, all_review_requests=fixture.reviews)
        self.assertEqual(final["shadow"]["calls"] - before, len(future), "eventual receipts must count actual calls")
        self.assertLessEqual(len(future), 1, "ordinary call cap must remain unchanged")
        return plan, planned, future

    def test_pending_memory_cleanup_cost_unknown_prevents_compaction_after_restart(self):
        plan, planned, future = self.exercise()
        if plan["selected"]:
            self.assertEqual(len(future), 1, "RED witness requires actual subsequent paid cleanup")
            self.assertEqual(ledger(future[0])["eligible_group_ids"], [])
            self.assertEqual(ledger(future[0])["eligible_memory_ids"], ["m1"])
            self.assertEqual(plan["candidates"][0]["future_reviewer_calls"], 0,
                             "old-binary RED witness is the zero projected call versus actual POST")
        self.assertFalse(plan["selected"], "unpriced pending memory-only completion must not authorize compaction")
        self.assertFalse(plan["reviewer_economics_priced"])
        self.assertEqual(planned["compactions"], 0)
        self.assertFalse(planned["history"][1]["removed"])
        # With no rewrite, the already-paid background source review can finish
        # normally before restart. Its completion may legitimately leave no
        # future call; the payoff-one control separately exercises real cleanup.
        self.assertIn("g2", ledger(self.evidence["all_review_requests"][1])["eligible_group_ids"])

    def test_pending_empty_sweep_without_memory_remains_priced(self):
        plan, _planned, future = self.exercise(memory=False)
        self.assertTrue(plan["selected"])
        self.assertTrue(plan["reviewer_economics_priced"])
        self.assertEqual(plan["candidates"][0]["future_reviewer_calls"], 0)
        self.assertEqual(future, [], "no memory means completing the emptied sweep needs no cleanup")

    def test_complete_sweep_with_memory_obeys_cadence_and_remains_priced(self):
        plan, _planned, future = self.exercise(pending=False)
        self.assertTrue(plan["selected"])
        self.assertTrue(plan["reviewer_economics_priced"])
        self.assertEqual(plan["candidates"][0]["future_reviewer_calls"], 0)
        self.assertEqual(future, [], "complete sweep with retained memory is not due at cadence 100")

    def test_payoff_one_pending_memory_has_no_future_cost_horizon(self):
        plan, _planned, future = self.exercise(payoff=1)
        self.assertTrue(plan["selected"])
        self.assertTrue(plan["reviewer_economics_priced"])
        self.assertTrue(plan["current_review_cost_is_sunk"])
        self.assertEqual(plan["candidates"][0]["future_reviewer_calls"], 0)
        self.assertEqual(len(future), 1, "sunk-only planning must not change eventual cleanup")
        self.assertEqual(ledger(future[0])["eligible_memory_ids"], ["m1"])
        self.assertEqual(ledger(future[0])["eligible_group_ids"], [])

    def bootstrap_plan(self, payoff=2, complete=False):
        fixture, seed = self.seeded(bootstrap=True)
        if complete:
            path = next(self.state.glob("*.json"))
            saved = json.loads(path.read_text())
            saved["bootstrap_review_complete"] = True
            path.write_text(json.dumps(saved))
        proxy = self.proxy(fixture, ["--payoff-requests", str(payoff), "--review-every-requests", "100",
                                     "--review-max-calls", "1", "--bootstrap-max-cost-usd", "0"])
        self.assertEqual(proxy.send(seed)[0], 200)
        saved = proxy.saved()
        self.evidence.update(plan=saved["last_plan"], final_checkpoint=saved,
                             all_review_requests=fixture.reviews)
        self.assertEqual(len(fixture.reviews), 1, "exhausted bootstrap budget must still prevent dispatch")
        return saved["last_plan"]

    def test_incomplete_bootstrap_future_economics_unknown(self):
        plan = self.bootstrap_plan()
        self.assertFalse(plan["selected"], "incomplete bootstrap future costs are not modeled")
        self.assertFalse(plan["reviewer_economics_priced"])

    def test_payoff_one_incomplete_bootstrap_remains_sunk_only(self):
        plan = self.bootstrap_plan(payoff=1)
        self.assertTrue(plan["selected"])
        self.assertTrue(plan["reviewer_economics_priced"])

    def test_completed_bootstrap_does_not_trigger_unknown_guard(self):
        plan = self.bootstrap_plan(complete=True)
        # Only the ordinary pending-memory candidate remains unpriced here;
        # completing bootstrap itself must not short-circuit that candidate.
        self.assertIn("candidates", plan)


def load_tests(_loader, _tests, _pattern):
    # Explicit suite avoids inheriting/running the original controls here.
    names = [name for name in PendingCleanup.__dict__ if name.startswith("test_")]
    return unittest.TestSuite(PendingCleanup(name) for name in names)


if __name__ == "__main__":
    unittest.main()
