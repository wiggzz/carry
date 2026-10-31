"""Actual planner: compare an authorized warm projection with a tiny new edit.

Synthetic loopback receipts and a deliberately attested fixture-only cache
capability; not evidence of provider residency. No live state or paid calls.
"""
import contextlib
import copy
import json
import itertools
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

    def run_arm(self, name, drop_tiny, *, restart=False, followups=False,
                history=HISTORY, additional=(4,), fields=None, audit_guard=False):
        fields = fields or {}
        state = self.root / name
        state.mkdir()
        observations = []
        ledgers = []
        captured = {"observations": observations, "review_ledgers": ledgers}
        self.evidence[name] = captured

        def primary(_body, n):
            response = completed(response_id=f"resp_marginal_{n}")
            response["usage"]["input_tokens"] = 20000
            response["usage"]["input_tokens_details"]["cached_tokens"] = 20000 if n == 2 or n >= 4 else 0
            # The first actual projected wire (R3) populates a whole-input
            # synthetic write. Later read receipts do not invent write timing
            # or refresh the deadline; the production dispatch/terminal owner
            # records that interval. Not a claim about real provider residency.
            response["usage"]["input_tokens_details"]["cache_write_tokens"] = 20000 if n == 3 else 0
            return 200, response

        def review(body, n):
            actionable = ledger(body)
            ledgers.append(copy.deepcopy(actionable))
            removable = [2] if n == 1 else (list(additional) if n == 3 and drop_tiny else [])
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
                if n == 5 and restart:
                    before = proxy.saved()["primary"]
                    proxy.close()
                    proxy = Proxy(state, fixture, options)
                    self.stack.callback(proxy.close)
                    self.assertEqual(proxy.saved()["primary"], before)
                status, response = proxy.send(history, **fields)
                self.assertEqual(status, 200, response)
                saved = proxy.saved()
                observations.append({"n": n, "last_plan": copy.deepcopy(saved["last_plan"]),
                                     "last_submitted_ids": saved["last_submitted_ids"]})
                if n == 4:
                    captured["warm_cache"] = json.loads((state / "history.index").read_text()).get("upstream_cache", [])
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
            decision = copy.deepcopy(saved)
            captured["decision_checkpoint"] = decision
            if followups:
                previous = copy.deepcopy(fixture.primaries[-1]["input"])
                # Repeated full input, process restart, ordinary append and
                # actual response-ID expansion all reach the same planner and
                # final outbound boundary; no synthetic planner is involved.
                canonical = list(history)
                for action in ("repeat", "restart", "append", "response_id"):
                    before = proxy.saved()["primary"]
                    if action == "restart":
                        proxy.close()
                        proxy = Proxy(state, fixture, options)
                        self.stack.callback(proxy.close)
                        self.assertEqual(proxy.saved()["primary"], before)
                    delta = {"role": "user", "content": "CONTINUATION_" + action}
                    fields = {}
                    sent = canonical
                    if action in ("append", "response_id"):
                        canonical = canonical + [delta]
                        previous = previous + [delta]
                        sent = canonical
                        if action == "response_id":
                            sent = [delta]
                            fields["previous_response_id"] = saved["last_response_id"]
                    status, response = proxy.send(sent, **fields)
                    self.assertEqual(status, 200, response)
                    self.assertEqual(fixture.primaries[-1]["input"], previous, action)
                    self.assertNotIn("previous_response_id", fixture.primaries[-1])
                    saved = proxy.saved()
                    observations.append({"action": action, "last_plan": copy.deepcopy(saved["last_plan"]),
                                         "last_submitted_ids": saved["last_submitted_ids"]})
            if audit_guard:
                plan = decision["last_plan"]
                chosen = next(row for row in plan["candidates"] if row["candidate_id"] == plan["selected_candidate_id"])
                capacity = int(chosen["primary_retained_required_tokens"]) + 500
                bounded = options + ["--primary-context-tokens", str(capacity)]
                delta = {"role": "user", "content": "CAPACITY_APPEND " * 100}
                appended = list(history) + [delta]
                expected = copy.deepcopy(fixture.primaries[-1]["input"]) + [delta]
                proxy.close()
                proxy = Proxy(state, fixture, bounded)
                self.stack.callback(proxy.close)
                self.assertEqual(proxy.send(appended)[0], 200)
                saved = proxy.saved()
                captured["compact_capacity"] = copy.deepcopy(saved)
                # Capacity re-resolution can legitimately remove the staged
                # loopback cache capability. Either authorized compact wire
                # may then win; that is not the audit-capacity assertion.
                marker = next(item for item in expected if item not in appended)
                rewritten = [marker if item == history[3] else item for item in expected]
                actual = fixture.primaries[-1]["input"]
                self.assertIn(actual, [expected, rewritten])
                self.assertEqual(actual[-1], delta)
                compact_plan = saved["last_plan"]
                compact_choice = next(row for row in compact_plan["candidates"]
                                      if row["candidate_id"] == compact_plan["selected_candidate_id"])
                self.assertTrue(compact_choice["feasible"])
                self.assertLessEqual(compact_choice["primary_retained_required_tokens"], capacity)
                self.assertFalse(saved["last_plan"]["forced"], "a fitting continuation is not forced")
                self.assertGreater(saved["last_plan"]["canonical_primary_required_tokens"], capacity)
                proxy.close()
                proxy = Proxy(state, fixture, bounded, mode="audit")
                self.stack.callback(proxy.close)
                before = {role: copy.deepcopy(proxy.saved()[role]) for role in ("primary", "shadow")}
                counts = len(fixture.primaries), len(fixture.reviews)
                status, response = proxy.send(appended)
                captured["audit_capacity_response"] = {"status": status, "body": response}
                self.assertEqual(status, 413, "ACTUAL_AUDIT_CAPACITY: reject the actual oversized canonical wire")
                self.assertEqual((len(fixture.primaries), len(fixture.reviews)), counts)
                self.assertEqual({role: proxy.saved()[role] for role in before}, before)
                saved = proxy.saved()
            captured.update(primaries=copy.deepcopy(fixture.primaries), decision_checkpoint=decision,
                            reviews=copy.deepcopy(fixture.reviews),
                            fixture_errors=copy.deepcopy(fixture.errors), final_checkpoint=saved)
            self.assertEqual(fixture.errors, [])
            self.assertEqual(len(fixture.primaries), 9 if followups else (6 if audit_guard else 5))
            calls = len(fixture.primaries)
            self.assertEqual(saved["primary"]["calls"], calls)
            self.assertEqual(saved["primary"]["input_tokens"], 20000 * calls)
            self.assertEqual(saved["primary"]["cached_tokens"], 20000 * (calls - 2))
            self.assertEqual(saved["primary"]["cache_write_tokens"], 20000)
            self.assertEqual(saved["primary"]["output_tokens"], calls)
            self.assertEqual(saved["shadow"]["calls"], len(fixture.reviews))
            self.assertEqual(saved["shadow"]["input_tokens"], 100 * len(fixture.reviews))
            self.assertEqual(saved["shadow"]["output_tokens"], len(fixture.reviews))
            self.assertEqual(fixture.primaries[2]["input"], fixture.primaries[3]["input"],
                             "R3/R4 must actually transmit the same prior projection")
            self.assertNotIn(history[1], fixture.primaries[3]["input"])
            self.assertIn(history[3], fixture.primaries[3]["input"])
            # Both possible outgoing projections must retain every other input
            # in order. Do not require the tiny edit: retaining it may be better.
            prior = fixture.primaries[3]["input"]
            final = fixture.primaries[4]["input"]
            markers = [item for item in prior if item not in history]
            self.assertEqual(len(markers), 1, "prior projection must have one omission marker")
            allowed = []
            for length in range(len(additional) + 1):
                for ids in itertools.combinations(additional, length):
                    removed = [history[i - 1] for i in ids]
                    allowed.append([markers[0] if item in removed else item for item in prior])
            self.assertIn(final, allowed,
                          "only newly authorized sources may change, including their omission markers")
            self.assertFalse(decision["last_plan"]["forced"], "not a capacity-forced differential")
            plan = decision["last_plan"]
            selected = next(item for item in plan["candidates"]
                            if item["candidate_id"] == plan["selected_candidate_id"])
            self.assertTrue(selected["feasible"])
            self.assertIsInstance(selected["selection_joint"]["compact"], (int, float))
            self.assertGreater(selected["selection_joint"]["compact"], 0)
            captured["selected"] = selected
            return captured
        finally:
            captured.update(primaries=copy.deepcopy(fixture.primaries),
                            reviews=copy.deepcopy(fixture.reviews),
                            fixture_errors=copy.deepcopy(fixture.errors))
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
        self.assert_marginal_decision(change)

    def assert_marginal_decision(self, change):
        plan = change["decision_checkpoint"]["last_plan"]
        rows = plan["candidates"]
        continuation = [row for row in rows if row["omitted_members"] == [2]]
        self.assertEqual(len(continuation), 1,
                         "MARGINAL_BASELINE: missing the still-authorized prior projection")
        baseline = continuation[0]
        rewrite = next(row for row in rows if row["omitted_members"] == [2, 4])
        # Compare alternatives in ONE actual decision: new advice may change
        # the reviewer ledger by a few tokens versus the independent control.
        # That is not a primary-wire loss, and must not be hidden by an epsilon.
        self.assertEqual(plan["baseline_omitted_members"], [2])
        self.assertTrue(baseline["baseline"])
        self.assertEqual(rewrite["selection_joint"]["keep"], baseline["selection_joint"]["compact"])
        self.assertGreater(rewrite["selection_joint"]["compact"], baseline["selection_joint"]["compact"])
        self.assertFalse(rewrite["selection_joint"]["accepted"])
        self.assertEqual(plan["selected_candidate_id"], baseline["candidate_id"])
        self.assertEqual(change["primaries"][4]["input"], change["primaries"][3]["input"])

    def test_warm_continuation_survives_restart_repeat_append_and_response_id(self):
        change = self.run_arm("restarted_tiny_change", True, restart=True, followups=True)
        self.assert_marginal_decision(change)

    def test_actual_audit_wire_cannot_borrow_compact_capacity_after_restart_append(self):
        self.run_arm("audit_capacity", True, audit_guard=True)

    # Deferred per Will's scope clarification relayed in review5723638312.
    # Manual unittest selector retains the executed tests-first optimization
    # case; ordinary discovery must not gate the immediate baseline repair.
    # Synthetic explicit breakpoint only, NOT implicit-provider feasibility.
    def deferred_projected_cache_boundary_preserves_tiny_source_but_drops_profitable_suffix(self):
        history = copy.deepcopy(HISTORY)
        history[5]["content"] = [{"type": "output_text", "text": history[5]["content"],
                                  "prompt_cache_breakpoint": {"mode": "explicit"}}]
        history += [{"role": "assistant", "content": "LATE_SOURCE_C " + "late details " * 1200},
                    {"role": "user", "content": "Final suffix task."}]
        for restart in (False, True):
            with self.subTest(restart=restart):
                arm = self.run_arm(f"projected_boundary_{restart}", True, restart=restart,
                                   history=history, additional=(4, 8),
                                   fields={"prompt_cache_options": {"mode": "explicit"}})
                prior = arm["primaries"][3]["input"]
                boundary = prior.index(history[5]) + 1
                self.assertLess(boundary, prior.index(history[7]))
                self.assertTrue(any(e["input_len"] == boundary and e["native_cached_fraction"] == 1
                                    for e in arm["warm_cache"]), "actual receipt must locate the explicit projected prefix")
                plan = arm["decision_checkpoint"]["last_plan"]
                candidates = plan["candidates"]
                intermediate = [row for row in candidates if row["omitted_members"] == [2, 8]]
                self.assertEqual(len(intermediate), 1,
                                 "PROJECTED_PREFIX_ALTERNATIVE: missing warm-prefix-preserving suffix edit")
                keep = next(row for row in candidates if row["omitted_members"] == [2])
                full = next(row for row in candidates if row["omitted_members"] == [2, 4, 8])
                winner = intermediate[0]
                self.assertLess(winner["selection_joint"]["compact"], keep["selection_joint"]["compact"])
                self.assertLess(winner["selection_joint"]["compact"], full["selection_joint"]["compact"])
                self.assertEqual(plan["selected_candidate_id"], winner["candidate_id"])
                final = arm["primaries"][4]["input"]
                self.assertEqual(final[:boundary], prior[:boundary])
                self.assertIn(history[3], final)
                self.assertNotIn(history[7], final)


if __name__ == "__main__":
    unittest.main(verbosity=2)
