"""Current history continuity follows the final subscription wire.

Real CLI/SSE, synthetic auth and native usage, loopback only. The control holds
instructions/key fixed; independent arms add stripped options or fold instruction
roles without changing the adapted history. Paid-summary seed works on the frozen
pre-initialization baseline too; that execution is not candidate verification.
"""
import base64
import copy
import json
import unittest

import proxy_legacy_history as legacy
import proxy_state_regressions as support


class LegacyAdapterLoads(unittest.TestCase):
    def setUp(self):
        legacy.LegacyHistoryLoads.setUp(self)

    def lifecycle_adapter(self, *, shared, controls, folded):
        auth = self.harness.state / "synthetic-codex"
        auth.mkdir()
        payload = base64.urlsafe_b64encode(support.wire({"https://api.openai.com/auth": {
            "chatgpt_account_id": "fixture-account"}})).decode().rstrip("=")
        credential = auth / "auth.json"
        credential.write_bytes(support.wire({"version": 1,
            "access_token": f"header.{payload}.signature", "refresh_token": "fixture-refresh",
            "expires_at_ms": 4102444800000}))
        credential.chmod(0o600)
        inputs = [10000, 300000, 300100, 300200, 300300]

        def primary(_body, n):
            response = support.completed(response_id=f"legacy-adapter-{n}")
            response["usage"] = {"input_tokens": inputs[n - 1], "output_tokens": 0,
                                 "input_tokens_details": {"cached_tokens": 0,
                                                          "cache_write_tokens": 0}}
            return 200, b"data: " + support.wire({"type": "response.completed",
                                                   "response": response}) + b"\n\n"

        def review(*_):
            raise AssertionError("review threshold must suppress synthetic reviewer work")

        fixture = self.harness.fixture(primary, review)
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        fields = {"model": "gpt-6.1-sol", "instructions": "fixed native instruction",
                  "prompt_cache_key": "legacy-adapter-stable-key"}
        if controls:
            # All these disappear on subscription transport. They must remain
            # client-facing inputs to admission, not wire proof inputs.
            fields.update(max_output_tokens=1, max_completion_tokens=1,
                          prompt_cache_options={"mode": "implicit", "ttl": "30m"},
                          prompt_cache_retention="30m", stream_options={})
        instruction_roles = ([{"role": "system", "content": "fixed system instruction"},
                              {"role": "developer", "content": [
                                  {"type": "input_text", "text": "fixed developer instruction"}]}]
                             if folded else [])
        history = instruction_roles + [{"role": "assistant", "content": "older native history"},
                                       {"role": "user", "content": "current protected task"}]
        expected_instructions = fields["instructions"] + (
            "\nfixed system instruction\nfixed developer instruction" if folded else "")
        expected_wires = []

        def start():
            proxy = legacy.OwnerProxy(self.harness.state, fixture, shared=shared, mode="audit",
                extra=["--codex-login", "--codex-home", str(auth)],
                extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS": "1000000"})
            self.harness.stack.callback(proxy.close)
            return proxy

        def snapshot(proxy):
            status, stats = proxy.request("GET", "/carry/dashboard/stats")
            self.assertEqual(status, 200)
            self.assertEqual(len(stats["sessions"]), 1)
            return copy.deepcopy(stats["sessions"][0]["cost_estimate"])

        def send(proxy):
            self.assertEqual(proxy.send(history, **fields)[0], 200)
            expected = {"model": fields["model"], "instructions": expected_instructions,
                        "prompt_cache_key": fields["prompt_cache_key"], "stream": True,
                        "store": False, "input": copy.deepcopy(history[len(instruction_roles):])}
            self.assertEqual(fixture.primaries[-1], expected,
                             "complete adapted wire must remain unchanged except real append")
            expected_wires.append(expected)
            return snapshot(proxy), proxy.saved()

        # Real recognized small seed then in-window summary, followed by growth.
        # No new-style initialization is needed to validate the old binary arm.
        for step in range(3):
            if step:
                history.append({"role": "user", "content": f"native growth {step}"})
            proxy = start()
            before, saved = send(proxy)
            proxy.close()
        t = saved["cost_trajectory"]
        self.assertEqual(saved["history_shared"], shared)
        self.assertEqual((t["retired"], t["summary"], t["last_traditional_input"]),
                         (280000, 1000, 21100))
        self.assertEqual(before.get("initial_compactions", 0), 0)
        self.assertEqual(before["traditional_compactions"], 1)
        self.assertGreater(before["summary_cost"], 0)
        self.assertEqual(before["matched_primary_responses"], 3)
        self.assertEqual(before["excluded_unpriced_calls"], 0)
        self.assertEqual(saved["shadow"]["calls"], 0)
        self.assertEqual(saved["shadow"]["cost_usd"], 0)
        self.assertEqual(saved["compactions"], 0)
        paid_before = sum(n * (4 if n > 272000 else 2) / 1e6 for n in inputs[:3])
        self.assertAlmostEqual(before["observed_primary_cost"], paid_before, places=12)

        # Current cursor proof comes from real adapted dispatch. Remove only
        # modeled cache evidence; never rewrite the cursor or infer a write.
        checkpoint = next(self.harness.state.glob("*.json"))
        owner = checkpoint.stem
        def without_cache(target):
            trajectory = target["cost_trajectory"]
            trajectory["receipts"].pop("modeled_cache", None)
            self.assertEqual(trajectory["history_continuity"]["cursor"]["items"],
                             len(expected_wires[-1]["input"]))
            self.assertEqual(trajectory["history_continuity"]["cursor"]["input"], inputs[2])
            return target
        staged = without_cache(copy.deepcopy(saved))
        checkpoint.write_text(json.dumps(staged))
        index_path = self.harness.state / "history.index"
        index = json.loads(index_path.read_text())
        if shared:
            self.assertIn(owner, index["snapshots"])
            index["snapshots"][owner] = without_cache(index["snapshots"][owner])
            self.assertEqual(index["snapshots"][owner]["cost_trajectory"], staged["cost_trajectory"])
            index_path.write_text(json.dumps(index))
        self.evidence.update(shared=shared, controls=controls, folded=folded,
                             before=before, staged=staged, owner=owner,
                             staged_index=index if shared else None)

        snapshots, states = [], []
        paid = paid_before
        for step in range(2):
            proxy = start()
            loaded = snapshot(proxy)
            previous = snapshots[-1] if snapshots else before
            for key in ("observed_primary_cost", "observed_reviewer_cost", "carry_cost",
                        "traditional_cost", "summary_cost", "initial_compactions",
                        "traditional_compactions", "cache_policy_boundaries"):
                self.assertEqual(loaded.get(key), previous.get(key), f"load changed {key}")
            history.append({"role": "user", "content": f"continued native growth {step}"})
            after, state = send(proxy)
            proxy.close()
            paid += inputs[3 + step] * 4 / 1e6
            # Complete wires and immutable money grade before comparison proof.
            self.assertAlmostEqual(after["observed_primary_cost"], paid, places=12)
            self.assertEqual(after["coverage"]["primary_responses"], 4 + step)
            self.assertEqual(after["unavailable_primary_cost_calls"], 0)
            self.assertEqual(after["observed_reviewer_cost"], before["observed_reviewer_cost"])
            self.assertEqual(state["shadow"]["calls"], 0)
            self.assertEqual(state["shadow"]["cost_usd"], 0)
            self.assertEqual(state["compactions"], 0)
            self.assertEqual(after.get("cache_policy_boundaries", []),
                             before.get("cache_policy_boundaries", []))
            snapshots.append(after)
            states.append(state)
        self.evidence.update(snapshots=snapshots, checkpoints=states,
                             primaries=fixture.primaries, expected_wires=expected_wires,
                             fixture_errors=fixture.errors, reviews=fixture.reviews)
        self.assertEqual(fixture.primaries, expected_wires)
        self.assertEqual(len(fixture.primaries), len(inputs))
        self.assertEqual(fixture.reviews, [])
        self.assertEqual(fixture.errors, [])

        for step, (after, state) in enumerate(zip(snapshots, states)):
            trajectory = state["cost_trajectory"]
            with self.subTest(step=step, shared=shared, controls=controls, folded=folded):
                self.assertEqual(after["matched_primary_responses"], 4 + step,
                                 "CURRENT_ADAPTED_WIRE: unchanged native growth must remain comparable")
                self.assertEqual(after["excluded_unpriced_calls"], 0)
                self.assertEqual((trajectory["retired"], trajectory["summary"],
                                  trajectory["last_traditional_input"]),
                                 (280000, 1000, 21200 + step * 100))
                self.assertEqual(after.get("initial_compactions", 0), 0)
                self.assertEqual(after["traditional_compactions"], 1)
                self.assertEqual(after["summary_cost"], before["summary_cost"])
                self.assertGreater(after["traditional_cost"], snapshots[step - 1]["traditional_cost"]
                                   if step else before["traditional_cost"])
                previous_cost = snapshots[step - 1]["traditional_cost"] if step else before["traditional_cost"]
                # Eventless current history has no timed write: first continuation is
                # cold, then the new hypothetical write warms only its prefix.
                expected_increment = (21200 * 2.5 if step == 0 else
                                      21200 * .1 + 100 * 2.5) / 1e6
                self.assertAlmostEqual(after["traditional_cost"] - previous_cost,
                                       expected_increment, places=12,
                                       msg="CURRENT_ADAPTED_PRICE: cold once, then original prefix plus new suffix")


def install_cases():
    for shared in (False, True):
        for controls, folded in ((False, False), (True, False), (False, True), (True, True)):
            def case(self, shared=shared, controls=controls, folded=folded):
                self.lifecycle_adapter(shared=shared, controls=controls, folded=folded)
            owner = "indexed" if shared else "explicit"
            spelling = "stripped" if controls else "omitted"
            roles = "folded" if folded else "fixed"
            setattr(LegacyAdapterLoads, f"test_{owner}_{spelling}_{roles}_current_restart", case)


install_cases()
if __name__ == "__main__":
    unittest.main(verbosity=2)
