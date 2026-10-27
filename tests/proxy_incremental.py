"""Fresh reviewer projections: no replay, bounded inputs, native source fidelity."""
import json
import unittest
import proxy_state_regressions as support
from proxy_state_regressions import advice_response, completed, ledger


class IncrementalReviewer(unittest.TestCase):
    def setUp(self):
        self.harness = support.StateRegressions()
        self.harness.setUp()
        self.addCleanup(self.harness.doCleanups)

    def fixture(self, review=None):
        fixture = self.harness.fixture(
            lambda _b, n: (200, completed(response_id=f"r{n}")),
            review or (lambda b, _n: (200, advice_response({
                "protected": ledger(b)["eligible_group_ids"], "removable": [], "memories": []}))))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        return fixture

    def test_fresh_projection_has_each_native_source_once_and_no_reviewer_answers(self):
        fixture = self.fixture()
        proxy = self.harness.proxy(fixture, ["--classifier-cache-policy", "disabled",
                                           "--min-payback-percent", "100"])
        history = [{"role": "user", "content": "durable goal"}]
        for n in range(4):
            self.assertEqual(proxy.send(history)[0], 200)
            history += [{"role": "assistant", "content": f"native evidence {n}"},
                        {"role": "user", "content": f"next direction {n}"}]
        self.assertGreaterEqual(len(fixture.reviews), 2)
        for body in fixture.reviews:
            items = body["input"]
            self.assertEqual(sum("eligible_group_ids" in json.dumps(i) for i in items), 1)
            for item in items:
                if item.get("role") == "assistant":
                    self.assertIn(item, history, "reviewer answers are not retained source")
            for source in history:
                self.assertLessEqual(items.count(source), 1)

    def test_primary_keeps_native_input_when_reviewer_does_not_fit(self):
        fixture = self.fixture()
        proxy = self.harness.proxy(fixture, ["--review-replayed-history",
            "--classifier-max-input-tokens", "1024", "--classifier-max-output-tokens", "64"])
        history = [{"role": "user", "content": "DURABLE_REQUIREMENT"},
                   {"role": "assistant", "content": "large evidence " * 6000},
                   {"role": "user", "content": "current direction"}]
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertEqual(fixture.primaries[0]["input"], history)
        self.assertEqual(fixture.reviews, [], "no hidden partial observation authority")
        self.assertEqual(proxy.saved()["compactions"], 0)

    def test_oversized_protected_request_never_reaches_primary_or_reviewer(self):
        fixture = self.fixture()
        proxy = self.harness.proxy(fixture, ["--primary-context-tokens", "3000",
            "--primary-output-reserve-tokens", "64"])
        status, _ = proxy.send([{"role": "user", "content": "protected new request " * 3000}])
        self.assertEqual(status, 413)
        self.assertEqual(fixture.primaries, [])
        self.assertEqual(fixture.reviews, [])


if __name__ == "__main__":
    unittest.main()
