"""Loopback lease failure/restart controls, with explicitly staged maturity.

Protection and target dispatch are genuinely admitted by Carry. Only completed
round counters are advanced in an owned, stopped synthetic checkpoint to keep the
matrix bounded. The separate original tracer executes all eight real rounds.
"""
import json
import tempfile
import unittest
from pathlib import Path

from proxy_state_regressions import StateRegressions, advice_response, completed, ledger


class LeaseControls(StateRegressions):
    def seed(self, outcome, extra=()):
        self.state = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
        evidence = {"role":"assistant","content":"CONTROL_EVIDENCE " * 2000}
        history = [{"role":"user","content":"durable human goal"}, evidence,
                   {"role":"user","content":"current task"}]
        def review(body, n):
            if n == 1:
                return 200, advice_response({"protected":["g2"],"removable":[],"memories":[]})
            self.assertIn("g2", ledger(body).get("keep_lease_review", {}).get("target_ids", []))
            fixture.held.set()
            if not fixture.release.wait(20):
                raise AssertionError("controlled target was not released")
            if outcome == "failed":
                return 503, {"error":{"code":"fixture_unavailable"}}
            if outcome == "invalid":
                return 200, advice_response({"invalid":True})
            if outcome == "partial":
                return 200, advice_response({"protected":["g1", "g99999"],"removable":[],"memories":[]})
            if outcome == "renew":
                return 200, advice_response({"protected":["g2"],"removable":[],"memories":[]})
            return 200, advice_response({"protected":[],"removable":[],"memories":[]})
        fixture = self.fixture(lambda _b, _n:(200, completed()), review)
        proxy = self.proxy(fixture, ["--background-reviews=false"])
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertEqual(proxy.send(history)[0], 200)
        protected = proxy.saved()
        self.assertEqual(protected["opinions"].get("2"), "keep")
        proxy.close()
        # Staged maturity only; do not stage model advice or a released lease.
        files = list(self.state.glob("*.json"))
        self.assertEqual(len(files), 1)
        protected["completed_requests"] = protected["keep_leases"]["entries"]["g2"]["renewed_at_round"] + 8
        files[0].write_text(json.dumps(protected))
        proxy = self.proxy(fixture, ["--background-reviews=true", *extra])
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertTrue(fixture.held.wait(5), "real targeted review must be observed")
        self.assertIn(evidence, fixture.primaries[-1]["input"])
        held = proxy.saved()
        self.assertEqual(held["opinions"].get("2"), "keep")
        self.assertIsNone(held["keep_leases"]["entries"]["g2"]["released_at_round"])
        return fixture, proxy, history, evidence, held

    def test_failed_invalid_partial_and_timeout_target_reviews_never_release(self):
        for outcome in ("failed", "invalid", "partial", "timeout"):
            with self.subTest(outcome=outcome):
                fixture, proxy, history, evidence, held = self.seed(outcome,
                    ["--classifier-timeout-secs", "1"] if outcome == "timeout" else [])
                if outcome != "timeout":
                    fixture.release.set()
                saved = self.wait_saved(proxy, lambda s:not s["background_review_pending"])
                fixture.release.set()
                self.assertEqual(saved["opinions"].get("2"), "keep")
                self.assertIsNone(saved["keep_leases"]["entries"]["g2"]["released_at_round"])
                self.assertFalse(saved["history"][1]["removed"])
                self.assertGreater(saved["review_feedback"]["rejected_count"], 0)
                proxy.close()
                # No new source: an additional economic retry must be bounded
                # across restart, even after failure or a partial accepted batch.
                proxy = self.proxy(fixture, ["--background-reviews=false", "--review-min-new-tokens", "25000"])
                self.assertEqual(proxy.send(history)[0], 200)
                self.assertEqual(len(fixture.reviews), 2, "no paid hot loop for unchanged failed target generations")
                self.assertIn(evidence, fixture.primaries[-1]["input"])
                proxy.close()

    def test_pending_restart_is_conservative_and_counts_are_content_free(self):
        fixture, proxy, history, evidence, held = self.seed("empty")
        policy = proxy.request("GET", "/carry/metrics")[1]["keep_lease_policy"]
        self.assertEqual(policy["pending_targets"], 1)
        self.assertEqual(policy["due_targets"], 1)
        self.assertEqual(policy["released_targets"], 0)
        self.assertNotIn("CONTROL_EVIDENCE", json.dumps(policy))
        proxy.close()
        fixture.release.set()
        proxy = self.proxy(fixture, ["--review-min-new-tokens", "25000", "--review-every-requests", "100"])
        self.assertEqual(proxy.send(history)[0], 200)
        restarted = proxy.saved()
        self.assertFalse(restarted["background_review_pending"])
        self.assertIsNone(restarted["keep_leases"]["pending"])
        self.assertEqual(restarted["opinions"].get("2"), "keep")
        self.assertIn(evidence, fixture.primaries[-1]["input"])
        self.assertEqual(len(fixture.reviews), 2)

    def test_target_renewal_reanchors_at_current_round_and_survives_restart(self):
        fixture, proxy, history, evidence, held = self.seed("renew")
        fixture.release.set()
        renewed = self.wait_saved(proxy, lambda s:not s["background_review_pending"])
        old = held["keep_leases"]["entries"]["g2"]
        new = renewed["keep_leases"]["entries"]["g2"]
        self.assertGreater(new["generation"], old["generation"])
        self.assertEqual(new["renewed_at_round"], renewed["completed_requests"])
        self.assertIsNone(new["released_at_round"])
        self.assertEqual(renewed["opinions"].get("2"), "keep")
        proxy.close()
        proxy = self.proxy(fixture, ["--review-min-new-tokens", "25000", "--review-every-requests", "100"])
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertEqual(proxy.saved()["keep_leases"]["entries"]["g2"], new)
        self.assertIn(evidence, fixture.primaries[-1]["input"])

    def test_released_restart_retains_uneconomic_source_then_removes_profitably(self):
        fixture, proxy, history, evidence, _ = self.seed("empty")
        fixture.release.set()
        released = self.wait_saved(proxy, lambda s:not s["background_review_pending"])
        self.assertIsNone(released["opinions"].get("2"))
        self.assertFalse(released["history"][1]["removed"])
        proxy.close()
        proxy = self.proxy(fixture, ["--min-payback-percent", "100", "--review-min-new-tokens", "25000"])
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertIn(evidence, fixture.primaries[-1]["input"], "released eligibility is not forced deletion")
        self.assertEqual(proxy.request("GET", "/carry/metrics")[1]["keep_lease_policy"]["released_targets"], 1)
        proxy.close()
        proxy = self.proxy(fixture, ["--min-payback-percent", "0", "--review-min-new-tokens", "25000"])
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertNotIn(evidence, fixture.primaries[-1]["input"])
        self.assertEqual(len(fixture.reviews), 2)
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertNotIn(evidence, fixture.primaries[-1]["input"], "retired raw input cannot resurrect on full-history echo")


if __name__ == "__main__":
    suite = unittest.TestSuite(LeaseControls(name) for name in LeaseControls.__dict__ if name.startswith("test_"))
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(not result.wasSuccessful())
