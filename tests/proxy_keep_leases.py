"""Synthetic loopback keep-lease lifecycles against the real Carry binary."""
import json
import threading
import time
import unittest

from proxy_state_regressions import StateRegressions, advice_response, completed, ledger


class KeepLeases(StateRegressions):
    # Do not inherit the unrelated fixture cases into this suite.
    def test_expiry_target_omission_becomes_economic_not_timer_deletion(self):
        evidence = {"role": "assistant", "content": "LEASE_EVIDENCE " * 6000}
        transcript = [{"role": "user", "content": "preserve the task"}, evidence,
                      {"role": "user", "content": "work on next step"}]
        target_seen = threading.Event()
        requests_at_release = []

        def review(body, n):
            if n == 1:
                return 200, advice_response({"protected": ["g2"], "removable": [], "memories": []})
            if "g2" in ledger(body).get("keep_lease_review", {}).get("target_ids", []):
                target_seen.set()
                fixture.held.set()
                fixture.release.wait(20)
                requests_at_release.append(len(fixture.primaries))
            return 200, advice_response({"protected": [], "removable": [], "memories": []})

        fixture = self.fixture(lambda _body, n: (200, completed(response_id=f"resp_{n}")), review)
        proxy = self.proxy(fixture, ["--background-reviews=true"])

        def send():
            status, body = proxy.send(transcript)
            self.assertEqual(status, 200, body)
            return self.wait_saved(proxy, lambda s: s["completed_requests"] == len(fixture.primaries))

        send()  # First actual primary makes the initial evidence exposed.
        transcript.append({"role": "user", "content": "step 2"})
        send()
        protected = self.wait_saved(proxy, lambda s: not s["background_review_pending"])
        self.assertEqual(protected["opinions"].get("2"), "keep")
        anchor = protected["completed_requests"]
        # Seven later successful primary completions: ordinary omissions stay sticky.
        for turn in range(7):
            transcript.append({"role": "user", "content": f"before expiry {turn}"})
            send()
            saved = self.wait_saved(proxy, lambda s: not s["background_review_pending"])
            self.assertEqual(saved["opinions"].get("2"), "keep")
            self.assertFalse(target_seen.is_set(), "no target before eight later primary completions")
        self.assertEqual(saved["completed_requests"], anchor + 7)
        transcript.append({"role": "user", "content": "eighth completion"})
        send()
        saved = self.wait_saved(proxy, lambda s: not s["background_review_pending"])
        self.assertEqual(saved["completed_requests"], anchor + 8)
        self.assertFalse(target_seen.is_set())
        transcript.append({"role": "user", "content": "review due lease"})
        send()  # Must not wait for the held targeted reviewer.
        self.assertTrue(target_seen.wait(5), "LEASE_RED: due protected source must receive targeted revalidation")
        held = proxy.saved()
        self.assertEqual(held["opinions"].get("2"), "keep")
        self.assertFalse(next(i for i in held["history"] if i["id"] == 2)["removed"])
        self.assertIn(evidence, fixture.primaries[-1]["input"], "expiry itself cannot delete source")
        fixture.release.set()
        released = self.wait_saved(proxy, lambda s: not s["background_review_pending"])
        self.assertIsNone(released["opinions"].get("2"), "target omission is neutral, not fabricated drop")
        self.assertNotIn("2", released["drop_members"])
        self.assertFalse(next(i for i in released["history"] if i["id"] == 2)["removed"])
        transcript.append({"role": "user", "content": "use ordinary profitable compaction"})
        send()
        self.assertNotIn(evidence, fixture.primaries[-1]["input"], "accepted target omission must unlock actual paired planner removal")
        final = proxy.saved()
        self.assertTrue(next(i for i in final["history"] if i["id"] == 2)["removed"])
        self.assertTrue(requests_at_release, "held reviewer actually completed")
        self.evidence.update(protection=protected, held=held, released=released, final=final,
                             reviews=fixture.reviews, primaries=fixture.primaries)


if __name__ == "__main__":
    # Only this class's explicitly owned tests; shared fixture helpers stay reusable.
    names = [name for name in KeepLeases.__dict__ if name.startswith("test_")]
    suite = unittest.TestSuite(KeepLeases(name) for name in names)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(not result.wasSuccessful())
