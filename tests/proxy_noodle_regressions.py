"""PR184 review regressions against the actual CLI; synthetic loopback only."""
import json
import time
import unittest
from proxy_state_regressions import advice_response, completed, wire
import proxy_transparent_history as history_fixture
from proxy_transparent_history import TransparentProxy


class NoodleRegressions(unittest.TestCase):
    setUp = history_fixture.TransparentHistory.setUp
    record_evidence = history_fixture.TransparentHistory.record_evidence
    fixture = history_fixture.TransparentHistory.fixture
    proxy = history_fixture.TransparentHistory.proxy

    def history(self):
        return [{"role": "user", "content": "NOODLE_GOAL"},
                {"role": "assistant", "content": "NOODLE_SOURCE " + "payload " * 3000},
                {"role": "user", "content": "NOODLE_TASK"}]

    def events(self):
        return [json.loads(line) for p in (self.state / "diagnostics").glob("*.jsonl")
                for line in p.read_text().splitlines()]

    def attest_cache(self):
        # Only synthetic route capability; never manufacture a native receipt.
        path = self.state / "history.index"
        index = json.loads(path.read_text())
        for snapshot in index["snapshots"].values():
            snapshot["model_resolution"]["primary"]["cache"] = {
                "minimum_prefix_tokens": 1024, "max_read_breakpoints": 50,
                "max_write_breakpoints": 4, "implicit_breakpoint_uses_write_slot": True}
        path.write_bytes(wire(index))

    def test_changed_cap_preserves_current_projection_and_summary_threshold_after_restart(self):
        fixture = self.fixture(lambda _b,n: (200, completed(response_id=f"cap_{n}")),
            lambda _b,_n: (200, advice_response({"protected":[], "removable":[2], "memories":[]})))
        h = self.history()
        options = ["--payoff-requests","1","--classifier-cache-policy","disabled"]
        proxy = self.proxy(fixture, [*options,"--review-replayed-history"])
        self.assertEqual(proxy.send(h, max_output_tokens=1)[0], 200)
        proxy.close()
        options += ["--review-every-requests","100"]
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send(h, max_output_tokens=1)[0], 200)
        _, before = proxy.checkpoint(h)
        restored = before["cost_trajectory"]["restored"]
        self.assertGreater(restored, 0)
        self.assertNotIn(h[1], fixture.primaries[-1]["input"])
        proxy.close()
        # Place the alternative's summary boundary between native and restored
        # input. A receipt-config change must invalidate old summary assumptions,
        # not the already measured current canonical-minus-sent quantity.
        path = self.state / "history.index"
        index = json.loads(path.read_text())
        for state in index["snapshots"].values():
            state["cost_trajectory"]["scenario"] = {
                "context_window": 200 + restored // 2, "reserve":100,
                "keep_recent":100, "summary":50}
        path.write_bytes(wire(index))
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send(h, max_output_tokens=2)[0], 200)
        _, after = proxy.checkpoint(h)
        self.evidence.update(before=before, after=after, primary=fixture.primaries, reviews=fixture.reviews)
        self.assertNotIn(h[1], fixture.primaries[-1]["input"])
        self.assertEqual(after["primary"]["calls"], 3)
        self.assertEqual(after["primary"]["input_tokens"], 300)
        self.assertEqual(after["shadow"]["calls"], 1)
        self.assertEqual(after["cost_trajectory"]["restored"], restored,
                         "receipt epoch reset must preserve this request's projection")
        self.assertEqual(after["cost_trajectory"]["result"]["traditional_compactions"], 1)
        self.assertGreater(after["cost_trajectory"]["result"]["summary_cost"], 0)
        self.assertGreater(after["cost_trajectory"]["result"]["observed_primary_cost"],
                           before["cost_trajectory"]["result"]["observed_primary_cost"])

    def test_cold_extension_revokes_old_warm_prefix_through_repeat_restart_and_rewarm(self):
        def primary(_body, n):
            response = completed(response_id=f"extension_{n}")
            response["usage"]["input_tokens"] = 4000
            response["usage"]["input_tokens_details"]["cached_tokens"] = 3900 if n in (2,6) else 0
            return 200, response
        fixture = self.fixture(primary)
        h = [{"role":"user","content":"WARM_ORIGINAL " + "prefix " * 3000}]
        extended = h + [{"role":"user","content":"COLD_SUFFIX"}]
        options = ["--review-every-requests","100","--classifier-cache-policy","disabled"]
        proxy = TransparentProxy(self.state,fixture,options,mode="audit")
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(h)[0],200)
        proxy.close(); self.attest_cache()
        proxy = TransparentProxy(self.state,fixture,options,mode="audit")
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(h)[0],200)  # actual warm H terminal
        self.assertEqual(proxy.send(extended)[0],200)  # actual cold H+suffix terminal
        _, cold = proxy.checkpoint(extended)
        self.assertGreater(cold["last_plan"]["primary_cached_estimated_tokens"],0,
                           "first extension legitimately forecasts the prior warm H")
        self.assertEqual(proxy.send(extended)[0],200)
        _, repeated = proxy.checkpoint(extended)
        proxy.close()
        proxy = TransparentProxy(self.state,fixture,options,mode="audit")
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(extended)[0],200)
        _, restarted = proxy.checkpoint(extended)
        self.assertEqual(proxy.send(h)[0],200)  # genuine later rewarm H
        self.assertEqual(proxy.send(extended)[0],200)
        _, rewarmed = proxy.checkpoint(extended)
        self.evidence.update(cold=cold,repeated=repeated,restarted=restarted,rewarmed=rewarmed,
                             actual_wires=fixture.primaries,events=self.events())
        self.assertEqual(repeated["last_plan"]["primary_cached_estimated_tokens"],0,
                         "newer cold extension supersedes old covered warmth")
        self.assertEqual(restarted["last_plan"]["primary_cached_estimated_tokens"],0)
        self.assertGreater(rewarmed["last_plan"]["primary_cached_estimated_tokens"],0)
        self.assertEqual(rewarmed["primary"]["calls"],7)
        self.assertEqual(rewarmed["primary"]["input_tokens"],28000)
        self.assertEqual(rewarmed["primary"]["cached_tokens"],7800)
        self.assertFalse(fixture.reviews)

    def test_planning_and_dispatch_share_metadata_stripped_floor(self):
        fixture = self.fixture(lambda _b,n:(200,completed(response_id=f"floor_{n}")),
            lambda _b,_n:(200,advice_response({"protected":[],"removable":[2],"memories":[]})))
        h = self.history()
        options = ["--review-replayed-history","--payoff-requests","1","--classifier-cache-policy","disabled",
                   "--review-min-primary-tokens","5000"]
        proxy = self.proxy(fixture,options)
        fields = {"metadata":{"diagnostic":"metadata " * 6000}}
        self.assertEqual(proxy.send(h,**fields)[0],200)
        self.assertEqual(proxy.send(h,**fields)[0],200)
        _, saved = proxy.checkpoint(h)
        decisions = [e["data"] for e in self.events() if e["event"] == "review_admission"]
        self.evidence.update(saved=saved,decisions=decisions,primaries=fixture.primaries,reviews=fixture.reviews)
        self.assertEqual(len(fixture.reviews),1)
        self.assertEqual(decisions[-1]["reason"],"primary_floor")
        self.assertLess(decisions[-1]["primary_estimated_tokens"],5000)
        self.assertGreater(saved["last_plan"]["primary_estimated_tokens"],5000)
        candidates = saved["last_plan"]["candidates"]
        self.assertTrue(candidates)
        for candidate in candidates:
            joint = candidate["selection_joint"]
            self.assertEqual(joint["future_shadow_keep"],0,
                             "an undispatchable ordinary review cannot subsidize omission")
            self.assertEqual(joint["future_shadow_compact"],0)

    def bootstrap_retry(self, rejected):
        def review(_body,n):
            if n == 1:
                return (200,advice_response({"protected":[999],"removable":[],"memories":[]})) if rejected else None
            return 200,advice_response({"protected":[],"removable":[],"memories":[]})
        fixture = self.fixture(lambda _b,n:(200,completed(response_id=f"retry_{n}")),review)
        h = self.history()
        options = ["--review-replayed-history","--classifier-cache-policy","disabled",
                   "--review-min-primary-tokens","100000","--review-every-requests","3"]
        proxy = self.proxy(fixture,options)
        self.assertEqual(proxy.send(h)[0],200)
        _, failed = proxy.checkpoint(h)
        index = json.loads((self.state / "history.index").read_text())
        self.assertFalse(index["reviews"])
        self.assertEqual(failed["shadow"]["calls"],1)
        self.assertEqual(failed["shadow"]["unavailable_cost_calls"],int(not rejected))
        self.assertEqual(failed["shadow"]["input_tokens"],100 if rejected else 0)
        # Failed bootstrap must honor normal request cadence on its retries,
        # even though the initial replay acquisition bypassed ordinary floors.
        self.assertEqual(proxy.send(h)[0],200)
        self.assertEqual(len(fixture.reviews),1)
        proxy.close()
        proxy = self.proxy(fixture,options)
        self.assertEqual(proxy.send(h)[0],200)
        self.assertEqual(len(fixture.reviews),1)
        self.assertEqual(proxy.send(h)[0],200)
        _, accepted = proxy.checkpoint(h)
        self.evidence.update(failed=failed,accepted=accepted,events=self.events(),reviews=fixture.reviews)
        self.assertEqual(len(fixture.reviews),2,"below-floor intent must retry after failure/rejection and restart")
        self.assertFalse(accepted["bootstrap_review_pending"])
        self.assertEqual(accepted["shadow"]["calls"],2)
        self.assertEqual(accepted["shadow"]["input_tokens"],200 if rejected else 100)
        self.assertEqual(accepted["shadow"]["unavailable_cost_calls"],int(not rejected))
        self.assertTrue(json.loads((self.state / "history.index").read_text())["reviews"],
                        "accepted empty advice is a durable semantic completion")
        proxy.close()
        proxy = self.proxy(fixture,options)
        for _ in range(4): self.assertEqual(proxy.send(h)[0],200)
        self.assertEqual(len(fixture.reviews),2,"accepted empty completion closes below-floor retries")
        self.assertEqual(proxy.checkpoint(h)[1]["shadow"],accepted["shadow"])

    def test_failed_bootstrap_retries_below_floor_with_cadence_and_once_only_censorship(self):
        self.bootstrap_retry(False)

    def test_all_rejected_bootstrap_retries_below_floor_with_cadence_and_once_only_payment(self):
        self.bootstrap_retry(True)


if __name__ == "__main__":
    unittest.main()
