"""Reviewer-independent forwarding through the production HTTP handler.

Synthetic loopback providers only. No checkpoint fabrication or model calls.
"""
import concurrent.futures
import json
import unittest

import proxy_state_regressions as support
from proxy_state_regressions import advice_response, completed, ledger


class ReviewerIndependence(unittest.TestCase):
    def setUp(self):
        self.harness = support.StateRegressions()
        self.harness.setUp()
        self.addCleanup(self.harness.doCleanups)

    @staticmethod
    def seed():
        return [{"role": "user", "content": "durable goal"},
                {"role": "assistant", "content": "AUTHORIZED_OLD_RESULT " * 1000},
                {"role": "user", "content": "latest protected direction"}]

    def proxy(self, fixture, options):
        proxy = self.harness.proxy(fixture, options)
        proxy.wait_reviews = False  # These assertions measure primary admission itself.
        return proxy

    def fixture(self, review):
        fixture = self.harness.fixture(
            lambda _body, n: (200, completed(response_id=f"primary_{n}")), review)
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        return fixture

    def held(self, bootstrap):
        def review(body, _n):
            fixture.held.set()
            fixture.release.wait(15)
            return 200, advice_response({"protected": [], "removable": [], "memories": []})
        fixture = self.fixture(review)
        options = ["--background-reviews=false", "--min-payback-percent", "100"]
        if bootstrap:
            options += ["--review-replayed-history"]
        proxy = self.proxy(fixture, options)
        history = self.seed()
        if not bootstrap:
            self.assertEqual(proxy.send(history)[0], 200)
        before = len(fixture.primaries)
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
            pending = pool.submit(proxy.send, history)
            try:
                self.assertTrue(fixture.held.wait(5), "exercise a real held reviewer")
                try:
                    status, _ = pending.result(timeout=3)
                except concurrent.futures.TimeoutError:
                    self.fail("fitting primary waited for reviewer completion")
                self.assertEqual(status, 200)
                self.assertEqual(len(fixture.primaries), before + 1)
                self.assertEqual(fixture.primaries[-1]["input"], history)
                self.assertFalse(fixture.release.is_set())
            finally:
                fixture.release.set()
                pending.result(timeout=15)

    def test_ordinary_review_never_holds_primary_even_in_legacy_sync_mode(self):
        self.held(False)

    def test_bootstrap_review_never_holds_primary(self):
        self.held(True)

    def test_reviewer_capacity_is_not_primary_capacity(self):
        fixture = self.fixture(lambda _b, _n: (503, {"error": "review unavailable"}))
        proxy = self.proxy(fixture, ["--classifier-max-input-tokens", "1024",
            "--classifier-max-output-tokens", "64", "--min-payback-percent", "0"])
        history = self.seed()
        self.assertEqual(proxy.send(history)[0], 200,
                         "reviewer overflow cannot reject a fitting primary")
        self.assertEqual(fixture.primaries[-1]["input"], history)
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertEqual(fixture.primaries[-1]["input"], history)
        self.assertFalse(proxy.saved()["last_plan"]["forced"])
        self.assertEqual(fixture.reviews, [])

    def failed_review(self, scenario):
        def review(_body, _n):
            if scenario == "timeout":
                fixture.held.set()
                fixture.release.wait(10)
            return 503, {"error": "unavailable"}
        fixture = self.fixture(review)
        proxy = self.proxy(fixture, ["--classifier-timeout-secs", "1",
            "--background-reviews=false", "--min-payback-percent", "100"])
        history = self.seed()
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertEqual(proxy.send(history)[0], 200)
        if scenario == "timeout":
            self.assertTrue(fixture.held.wait(2))
            self.assertFalse(fixture.release.is_set())
            self.assertEqual(proxy.send(history)[0], 200)
        saved = self.harness.wait_saved(proxy,
            lambda s: s["shadow"]["calls"] == 1 and not s["background_review_pending"])
        self.assertEqual(saved["invalid_reviews"], 1)
        self.assertEqual(saved["shadow"]["unavailable_cost_calls"], 1)
        self.assertEqual(saved["compactions"], 0)
        self.assertIsNone(proxy.advice())
        self.assertTrue(all(p["input"] == history for p in fixture.primaries))
        self.assertEqual(saved["completed_requests"], len(fixture.primaries))
        fixture.release.set()

    def test_reviewer_failure_preserves_primary_progress_and_unknown_spend(self):
        self.failed_review("failure")

    def test_reviewer_timeout_preserves_primary_progress_and_unknown_spend(self):
        self.failed_review("timeout")

    @staticmethod
    def imported_history():
        history = [{"role": "user", "content": "DURABLE_IMPORT_REQUIREMENT"}]
        for n in range(18):
            history += [{"role": "assistant", "content": f"import evidence {n} " * 300},
                        {"role": "user", "content": f"old direction {n}"}]
        return history + [{"role": "user", "content": "UNREVIEWED_SUFFIX_DIRECTION"}]




if __name__ == "__main__":
    unittest.main()
