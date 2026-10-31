"""Actual planner: compare an authorized warm projection with a tiny new edit.

Synthetic loopback receipts and a deliberately attested fixture-only cache
capability; not evidence of provider residency. No live state or paid calls.
"""
import contextlib
import copy
import json
import os
from pathlib import Path
import tempfile
import unittest

from proxy_state_regressions import Fixture, Proxy, advice_response, completed, ledger


HISTORY = [
    {"role": "user", "content": "Preserve this goal."},
    {"role": "assistant", "content": "OBSOLETE_BIG_SOURCE " + "old material " * 12000},
    {"role": "user", "content": "Continue."},
    {"role": "assistant", "content": "TINY_SOURCE ready."},
    {"role": "user", "content": "Keep the following useful reference."},
    {"role": "assistant", "content": "RETAINED_REFERENCE " + "useful details " * 8000},
    {"role": "user", "content": "Current task."},
]


class PlannerMarginalBaseline(unittest.TestCase):
    def setUp(self):
        self.stack = contextlib.ExitStack()
        self.addCleanup(self.stack.close)
        self.root = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
        self.evidence = {"capability": "SYNTHETIC loopback test contract, not provider proof"}
        self.addCleanup(self.record_evidence)

    def record_evidence(self):
        directory = os.environ.get("CARRY_TEST_EVIDENCE_DIR")
        if directory:
            path = Path(directory)
            path.mkdir(parents=True, exist_ok=True)
            (path / (self._testMethodName + ".json")).write_text(
                json.dumps(self.evidence, indent=2) + "\n")

    def run_arm(self, name, drop_tiny):
        state = self.root / name
        state.mkdir()
        observations = []
        ledgers = []
        captured = {"observations": observations, "review_ledgers": ledgers}
        self.evidence[name] = captured

        def primary(_body, n):
            response = completed(response_id=f"resp_marginal_{n}")
            response["usage"]["input_tokens"] = 20000
            response["usage"]["input_tokens_details"]["cached_tokens"] = 20000 if n in (2, 4) else 0
            return 200, response

        def review(body, n):
            actionable = ledger(body)
            ledgers.append(copy.deepcopy(actionable))
            removable = [2] if n == 1 else ([4] if n == 3 and drop_tiny else [])
            self.assertTrue(all(i in actionable["eligible_group_ids"] for i in removable),
                            (n, removable, actionable))
            return 200, advice_response({"protected": [], "removable": removable, "memories": []})

        fixture = Fixture(primary, review)
        self.stack.callback(fixture.close)
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        options = ["--payoff-requests", "5", "--classifier-cache-policy", "disabled"]
        proxy = Proxy(state, fixture, options)
        self.stack.callback(proxy.close)
        try:
            for n in range(1, 6):
                status, response = proxy.send(HISTORY)
                self.assertEqual(status, 200, response)
                saved = proxy.saved()
                observations.append({"n": n, "last_plan": copy.deepcopy(saved["last_plan"]),
                                     "last_submitted_ids": saved["last_submitted_ids"]})
                if n == 1:
                    # The ordinary loopback route is correctly unknown. Attest
                    # only this disposable route, in BOTH authority locations.
                    proxy.close()
                    capability = {"minimum_prefix_tokens": 1024, "max_read_breakpoints": 50,
                                  "max_write_breakpoints": 4,
                                  "implicit_breakpoint_uses_write_slot": True}
                    checkpoint = next(state.glob("*.json"))
                    snap = json.loads(checkpoint.read_text())
                    snap["model_resolution"]["primary"]["cache"] = capability
                    checkpoint.write_text(json.dumps(snap))
                    index_path = state / "history.index"
                    index = json.loads(index_path.read_text())
                    for owner in index["snapshots"].values():
                        owner["model_resolution"]["primary"]["cache"] = capability
                    index_path.write_text(json.dumps(index))
                    proxy = Proxy(state, fixture, options)
                    self.stack.callback(proxy.close)
                    self.assertEqual(proxy.saved()["model_resolution"]["primary"]["cache"], capability)
            captured.update(primaries=copy.deepcopy(fixture.primaries),
                            reviews=copy.deepcopy(fixture.reviews),
                            fixture_errors=copy.deepcopy(fixture.errors), final_checkpoint=saved)
            self.assertEqual(fixture.errors, [])
            self.assertEqual(len(fixture.primaries), 5)
            self.assertEqual(fixture.primaries[2]["input"], fixture.primaries[3]["input"],
                             "R3/R4 must actually transmit the same prior projection")
            self.assertNotIn(HISTORY[1], fixture.primaries[3]["input"])
            self.assertIn(HISTORY[3], fixture.primaries[3]["input"])
            # Both possible outgoing projections must retain every other input
            # in order. Do not require the tiny edit: retaining it may be better.
            prior = fixture.primaries[3]["input"]
            final = fixture.primaries[4]["input"]
            markers = [item for item in prior if item not in HISTORY]
            self.assertEqual(len(markers), 1, "prior projection must have one omission marker")
            edited = [markers[0] if item == HISTORY[3] else item for item in prior]
            self.assertIn(final, [prior, edited],
                          "only the newly authorized tiny source may change, including its marker")
            self.assertFalse(saved["last_plan"]["forced"], "not a capacity-forced differential")
            plan = saved["last_plan"]
            selected = next(item for item in plan["candidates"]
                            if item["candidate_id"] == plan["selected_candidate_id"])
            self.assertTrue(selected["feasible"])
            self.assertIsInstance(selected["selection_joint"]["compact"], (int, float))
            self.assertGreater(selected["selection_joint"]["compact"], 0)
            captured["selected"] = selected
            return captured
        finally:
            proxy.close()

    def test_tiny_edit_cannot_beat_a_cheaper_valid_warm_continuation(self):
        control = self.run_arm("keep_warm_control", False)
        change = self.run_arm("tiny_change", True)
        self.assertEqual(control["primaries"][:4], change["primaries"][:4],
                         "arms need identical actual histories before the decision")
        self.assertEqual(control["primaries"][-1]["input"], control["primaries"][-2]["input"])
        control_cost = control["selected"]["selection_joint"]["compact"]
        change_cost = change["selected"]["selection_joint"]["compact"]
        self.evidence["comparison"] = {"valid_continuation": control_cost,
                                       "selected_after_new_advice": change_cost}
        self.assertLessEqual(change_cost, control_cost + 1e-12,
                             "MARGINAL_BASELINE: selected edit costs more than the valid prior warm projection")


if __name__ == "__main__":
    unittest.main(verbosity=2)
