"""Real implicit/explicit Session routing across cache TTL changes and restart.

Native receipt quantities are synthetic. No checkpoint edits or provider calls:
all input and zero-write receipts pass through the actual loopback Carry binary.
An unsupported hypothetical TTL stays unavailable, not a history reset.
"""
import copy
from history_store import read_index, write_index
import json
import unittest

import proxy_state_regressions as support
from proxy_legacy_history import OwnerProxy


class SessionHistory(unittest.TestCase):
    def setUp(self):
        self.harness = support.StateRegressions()
        self.harness.setUp()
        self.harness._testMethodName = self._testMethodName
        self.evidence = self.harness.evidence
        self.addCleanup(lambda: self.assertTrue(self.harness.doCleanups(),
                                               "nested fixture cleanup failed"))

    def lifecycle(self, *, shared, change_ttl, initialized):
        inputs = ([] if initialized else [10000]) + [300000, 300100, 300200,
                                                              300300, 300400]
        transition = len(inputs) - 2

        def primary(_body, n):
            response = support.completed(response_id=f"session-ttl-{n}")
            response["usage"] = {"input_tokens": inputs[n - 1], "output_tokens": 0,
                                 "input_tokens_details": {"cached_tokens": 0,
                                                          "cache_write_tokens": 0}}
            return 200, response

        fixture = self.harness.fixture(primary, lambda *_: self.fail("unexpected reviewer"))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        history = [{"role": "assistant", "content": "owned history before TTL transition"},
                   {"role": "user", "content": "current request"}]
        snapshots, estimates, owner_ids, indexes, wires, ttls = [], [], [], [], [], []
        previous_files = {}
        for index, incoming in enumerate(inputs):
            if index:
                history.append({"role": "user", "content": f"append {index}"})
            proxy = OwnerProxy(self.harness.state, fixture, shared=shared, mode="audit",
                               wait_reviews=False,
                               extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS": "1000000"})
            self.harness.stack.callback(proxy.close)
            # Real process restart may not rewrite any settled owner or index.
            for name, saved in previous_files.items():
                self.assertEqual(read_index(self.harness.state / name), saved,
                                 "restart must preserve frozen money and quantities")
            ttl = "12h" if change_ttl and index >= transition else "30m"
            status, response = proxy.send(history, model="gpt-6.1-sol", max_output_tokens=1,
                                          prompt_cache_options={"mode": "implicit", "ttl": ttl})
            self.assertEqual(status, 200)
            self.assertEqual(response["id"], f"session-ttl-{index + 1}")
            stats = proxy.request("GET", "/carry/dashboard/stats")[1]
            self.assertEqual(len(stats["sessions"]), 1)
            estimates.append(copy.deepcopy(stats["sessions"][0]["cost_estimate"]))
            proxy.close()
            index_state = read_index(self.harness.state / "history.index")
            matches = [ref for ref in index_state["transcripts"].values()
                       if ref["external_id"] == response["id"]]
            self.assertEqual(len(matches), 1, "successful wire must bind one canonical owner")
            owner = matches[0]["session"]
            if shared:
                saved = index_state["snapshots"][owner]
            else:
                saved = json.loads((self.harness.state / f"{owner}.json").read_text())
            self.assertEqual(saved["history_shared"], shared)
            snapshots.append(copy.deepcopy(saved))
            owner_ids.append(owner)
            indexes.append(index_state)
            wires.append(copy.deepcopy(history))
            ttls.append(ttl)
            previous_files = {path.name: json.loads(path.read_text())
                              for path in self.harness.state.glob("*.json")}
            previous_files["history.index"] = index_state
        self.evidence.update(snapshots=snapshots, estimates=estimates, owner_ids=owner_ids, indexes=indexes,
                             primaries=fixture.primaries, reviews=fixture.reviews,
                             shared=shared, change_ttl=change_ttl, initialized=initialized)
        self.assertEqual(len(fixture.primaries), len(inputs))
        self.assertEqual(fixture.reviews, [])
        # Finish all wire/native-money controls even if routing forked an owner.
        for index, (incoming, saved) in enumerate(zip(inputs, snapshots)):
            self.assertEqual(fixture.primaries[index]["input"], wires[index])
            self.assertEqual(fixture.primaries[index]["max_output_tokens"], 1)
            self.assertEqual(fixture.primaries[index]["prompt_cache_options"],
                             {"mode": "implicit", "ttl": ttls[index]})
            owners = (indexes[index]["snapshots"].values() if shared else [saved])
            expected_paid = sum(n * (4 if n > 272000 else 2) / 1e6 for n in inputs[:index + 1])
            self.assertAlmostEqual(estimates[index]["observed_primary_cost"], expected_paid, places=12)
            owners = (indexes[index]["snapshots"].values() if shared else [saved])
            self.assertEqual(sum(s["primary"]["calls"] for s in owners), index + 1)
            self.assertEqual(saved["compactions"], 0)
            self.assertEqual(saved["shadow"]["calls"], 0)
            self.assertEqual(saved["primary"]["unavailable_cost_calls"], 0)
        before = estimates[transition - 1]
        self.assertEqual(before.get("initial_compactions", 0), int(initialized))
        self.assertEqual(before["traditional_compactions"], int(not initialized))
        self.assertEqual(before["summary_cost"] > 0, not initialized)
        # The native actual charge is known at 12h because reported writes are
        # zero. Do not require a known counterfactual tariff or copy cache hits.
        differences = []
        for offset, saved in enumerate(snapshots[transition:]):
            after = estimates[transition + offset]
            facts = [o["Primary"]["facts"] for o in saved["request_evidence"]["observations"] if "Primary" in o]
            self.assertEqual(facts[-1]["native"]["input_tokens"], inputs[transition + offset])
            self.assertEqual(saved["request_evidence"]["observations"][:len(snapshots[transition - 1]["request_evidence"]["observations"])],
                             snapshots[transition - 1]["request_evidence"]["observations"],
                             "TTL/restart cannot reprice settled dispatch facts")
            observed = {
                "owner": owner_ids[transition + offset],
                "initial_compactions": after.get("initial_compactions", 0),
                "traditional_compactions": after["traditional_compactions"],
                "summary_cost": after["summary_cost"],
            }
            expected = {
                "owner": owner_ids[transition - 1],

                "initial_compactions": int(initialized),
                "traditional_compactions": int(not initialized),
                "summary_cost": before["summary_cost"],
            }
            if observed != expected:
                differences.append({"append": offset, "observed": observed, "expected": expected})
        self.assertEqual(differences, [],
                         "SESSION_TTL_HISTORY: routing must preserve summarized history, growth, "
                         "paid summary and once-only initialization through TTL changes/restart")

    def test_implicit_unchanged_ttl_preserves_paid_summary(self):
        self.lifecycle(shared=True, change_ttl=False, initialized=False)

    def test_implicit_changed_ttl_preserves_paid_summary(self):
        self.lifecycle(shared=True, change_ttl=True, initialized=False)

    def test_explicit_changed_ttl_preserves_paid_summary(self):
        self.lifecycle(shared=False, change_ttl=True, initialized=False)

    def test_implicit_unchanged_ttl_preserves_free_initialization(self):
        self.lifecycle(shared=True, change_ttl=False, initialized=True)

    def test_implicit_changed_ttl_preserves_free_initialization(self):
        self.lifecycle(shared=True, change_ttl=True, initialized=True)

    def test_explicit_changed_ttl_preserves_free_initialization(self):
        self.lifecycle(shared=False, change_ttl=True, initialized=True)


if __name__ == "__main__":
    unittest.main()
