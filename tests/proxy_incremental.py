"""Fresh reviewer projections: no replay, bounded inputs, native source fidelity."""
import json
import unittest
import proxy_state_regressions as support
from proxy_state_regressions import advice_response, completed, ledger


def assert_source_projection(test, body, history):
    items = body["input"]
    test.assertEqual(sum("eligible_group_ids" in json.dumps(i) for i in items), 1)
    # The final quoted control ledger is not a native source or a reviewer answer.
    control = items[-1]
    test.assertEqual(control.get("role"), "assistant")
    test.assertEqual(json.loads(control["content"]), ledger(body))
    for item in items[:-1]:
        if item.get("role") == "assistant":
            test.assertIn(item, history, "reviewer answers are not retained source")
    for source in history:
        test.assertLessEqual(items.count(source), 1)


class ProjectionAssertionControls(unittest.TestCase):
    def test_control_ledger_is_not_mistaken_for_a_reviewer_answer(self):
        history = [{"role": "user", "content": "task"},
                   {"role": "assistant", "content": "native evidence"}]
        control = {"role": "assistant", "content": json.dumps({
            "eligible_group_ids": [1], "current_groups": []})}
        assert_source_projection(self, {"input": history + [control]}, history)
        with self.assertRaises(AssertionError):
            assert_source_projection(self, {"input": history + [
                {"role": "assistant", "content": "reviewer answer"}, control]}, history)
        with self.assertRaises(AssertionError):
            assert_source_projection(self, {"input": history + [history[-1], control]}, history)


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
            assert_source_projection(self, body, history)
        eligible = ledger(fixture.reviews[-1])["eligible_group_ids"]
        self.assertTrue(eligible, "exercise actionable source IDs, not just ledger presence")
        advice = proxy.advice()
        self.assertIsNotNone(advice)
        for group_id in eligible:
            self.assertEqual(advice["opinions"].get(str(group_id)), "keep")

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
