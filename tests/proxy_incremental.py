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

    def test_estimated_oversized_protected_request_is_tried_upstream(self):
        fixture = self.fixture()
        proxy = self.harness.proxy(fixture, ["--primary-context-tokens", "3000",
            "--primary-output-reserve-tokens", "64"])
        status, _ = proxy.send([{"role": "user", "content": "protected new request " * 3000}])
        self.assertEqual(status, 200)
        self.assertEqual(len(fixture.primaries), 1)
        self.assertEqual(fixture.reviews, [])

    def test_screenshot_request_is_tried_and_upstream_413_is_preserved(self):
        upstream_error = {"error": {"code": "provider_context_limit", "message": "fixture limit"}}
        fixture = self.harness.fixture(lambda *_: (413, upstream_error),
                                       lambda *_: self.fail("unexpected reviewer"))
        proxy = self.harness.proxy(fixture, ["--primary-context-tokens", "3000",
            "--primary-output-reserve-tokens", "64"])
        history = [{"role": "user", "content": [
            {"type": "input_text", "text": "Inspect this screenshot"},
            {"type": "input_image", "image_url": "data:image/png;base64," + "aB3+cD4/eF5=" * 8000}
        ]}]
        status, response = proxy.send(history)
        self.assertEqual(len(fixture.primaries), 1,
                         "encoded screenshots must reach the provider, not trigger a local 413")
        self.assertEqual(fixture.primaries[0]["input"], history)
        self.assertEqual(status, 413)
        self.assertEqual(response, upstream_error)
        self.assertEqual(fixture.reviews, [])

    def test_screenshot_and_encrypted_receipt_anchor_survives_restart(self):
        def primary(_body, n):
            response = completed(response_id=f"native-input-{n}")
            response["usage"]["input_tokens"] = 55888
            # Missing cache usage prevents pricing, not native input measurement.
            del response["usage"]["input_tokens_details"]
            return 200, response

        fixture = self.harness.fixture(primary,
            lambda *_: self.fail("protected native input must not require a reviewer"))
        options = ["--primary-context-tokens", "1000000",
                   "--primary-output-reserve-tokens", "64",
                   "--review-min-primary-tokens", "1000000"]
        history = [{"type": "reasoning", "summary": [],
                    "encrypted_content": "ENCRYPTED+opaque/" * 2000},
                   {"role": "user", "content": [
                       {"type": "input_text", "text": "Inspect the screenshot"},
                       {"type": "input_image", "image_url":
                        "data:image/png;base64," + "aB3+cD4/eF5=" * 8000}]}]
        proxy = self.harness.proxy(fixture, options)
        self.assertEqual(proxy.send(history)[0], 200)
        first = proxy.saved()["last_plan"]
        self.assertIsNone(first["primary_estimated_tokens"])
        self.assertIsNone(first["primary_input_basis"])
        self.assertEqual(proxy.saved()["primary"]["unavailable_cost_calls"], 1)
        for restarted in (False, True):
            with self.subTest(restarted=restarted):
                if restarted:
                    proxy.close()
                    proxy = self.harness.proxy(fixture, options)
                self.assertEqual(proxy.send(history)[0], 200)
                plan = proxy.saved()["last_plan"]
                self.assertEqual(plan["primary_estimated_tokens"], 55888)
                self.assertEqual(plan["primary_input_basis"], "provider_receipt")
                self.assertEqual(fixture.primaries[-1]["input"], history,
                                 "measurement must not rewrite native image or reasoning payloads")
        self.assertEqual(fixture.reviews, [])

    def test_unmeasured_native_input_stays_unknown_and_is_forwarded_unchanged(self):
        def primary(_body, n):
            response = completed(response_id=f"unmeasured-native-{n}")
            del response["usage"]
            return 200, response

        fixture = self.harness.fixture(primary,
            lambda *_: self.fail("unknown input quantity cannot authorize a paid review"))
        proxy = self.harness.proxy(fixture, ["--primary-context-tokens", "3000",
            "--primary-output-reserve-tokens", "64"])
        history = [{"type": "reasoning", "summary": [],
                    "encrypted_content": "opaqueCiphertext+" * 8000},
                   {"role": "user", "content": [
                       {"type": "input_image", "image_url":
                        "data:image/png;base64," + "aB3+cD4/eF5=" * 8000},
                       {"type": "input_text", "text": "Explain this screenshot"}]}]
        for _ in range(2):
            self.assertEqual(proxy.send(history)[0], 200)
            self.assertEqual(fixture.primaries[-1]["input"], history)
            plan = proxy.saved()["last_plan"]
            self.assertIsNone(plan["primary_estimated_tokens"])
            self.assertIsNone(plan["primary_input_basis"])
            self.assertEqual(plan["reason"], "input_quantity_unknown")
            self.assertEqual(plan["primary_admission"], "provider_enforced")
            for candidate in plan["candidates"]:
                self.assertIsNone(candidate["primary_retained_estimated_tokens"])
        self.assertEqual(fixture.reviews, [])


if __name__ == "__main__":
    unittest.main()
