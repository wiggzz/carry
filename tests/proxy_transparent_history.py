"""Transparent, caller-owned history matching through real loopback review."""
import http.client
import json
import time
import unittest
from proxy_state_regressions import StateRegressions, Proxy, advice_response, completed, ledger, wire


class TransparentProxy(Proxy):
    def request(self, method, path, body=None):
        connection = http.client.HTTPConnection(*self.address, timeout=30)
        try:
            connection.request(method, path, body=None if body is None else wire(body),
                               headers={"Content-Type": "application/json"})
            response = connection.getresponse()
            return response.status, json.loads(response.read())
        finally:
            connection.close()

    def saved(self):
        # Settle all fixture-owned workers; never pick an arbitrary checkpoint.
        states = [json.loads(p.read_text()) for p in self.state.glob("*.json")]
        return {"background_review_pending": any(s.get("background_review_pending") for s in states)}


class TransparentHistory(unittest.TestCase):
    setUp = StateRegressions.setUp
    record_evidence = StateRegressions.record_evidence
    fixture = StateRegressions.fixture

    def test_no_header_fork_reuses_committed_ancestor_after_cleanup_and_restart(self):
        history = [{"role": "user", "content": "ANCESTOR_GOAL"},
                   {"role": "assistant", "content": "RAW_A " + "payload " * 6000},
                   {"role": "user", "content": "A_BOUNDARY"}]
        def review(body, n):
            ids = ledger(body)["eligible_group_ids"]
            return 200, advice_response({"protected": [], "removable": ids,
                "memories": [{"source_ids": ["g2"], "text": "A_FACT"}] if n == 1 else []})
        fixture = self.fixture(lambda _b, n: (200, completed(response_id=f"resp_{n}")), review)
        options = ["--review-replayed-history", "--payoff-requests", "1",
                   "--classifier-cache-policy", "disabled"]
        proxy = TransparentProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertTrue(fixture.reviews, "actual accepted review is required")
        parent = history + [{"role": "user", "content": "B_ONLY"}]
        self.assertEqual(proxy.send(parent)[0], 200)
        self.assertNotIn("RAW_A", json.dumps(fixture.primaries[-1]))
        parent_bytes = {p.name: p.read_bytes() for p in self.state.glob("*.json")}
        reviews = len(fixture.reviews)
        proxy.close()
        proxy = TransparentProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        child = history + [{"role": "user", "content": "C_ONLY"}]
        status, result = proxy.send(child)
        self.assertEqual(status, 200, result)
        sent = fixture.primaries[-1]["input"]
        self.assertNotIn("RAW_A", json.dumps(sent),
                         "transparent fork must recover committed A without discovery/headers")
        self.assertIn("A_FACT", json.dumps(sent))
        self.assertNotIn("B_ONLY", json.dumps(sent))
        self.assertIn(history[0], sent)
        self.assertIn(history[-1], sent)
        self.assertIn(child[-1], sent)
        self.assertEqual(len(fixture.reviews), reviews, "no fork recovery call")
        for name, original in parent_bytes.items():
            self.assertEqual((self.state / name).read_bytes(), original, "parent stays byte-identical")
        self.assertEqual(proxy.send(child)[0], 200)
        self.assertNotIn("RAW_A", json.dumps(fixture.primaries[-1]))


    def test_response_id_expands_observed_outputs_and_can_rewind_after_restart(self):
        a = {"role": "user", "content": "INPUT_A"}
        oa = {"role": "assistant", "content": "ACTUAL_OUTPUT_A"}
        ob = {"role": "assistant", "content": "ACTUAL_OUTPUT_B"}
        b = {"role": "user", "content": "INPUT_B"}
        c = {"role": "user", "content": "INPUT_C"}
        fixture = self.fixture(lambda _body, n: (200, completed([oa if n == 1 else ob], f"resp_{n}")))
        options = ["--review-every-requests", "100"]
        proxy = TransparentProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send([a])[0], 200)
        status, result = proxy.send([b], previous_response_id="resp_1")
        self.assertEqual(status, 200, result)
        self.assertEqual(fixture.primaries[-1]["input"], [a, oa, b])
        proxy.close()
        proxy = TransparentProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        status, result = proxy.send([c], previous_response_id="resp_1")
        self.assertEqual(status, 200, result)
        self.assertEqual(fixture.primaries[-1]["input"], [a, oa, c], "older response is a transcript frontier, not latest state")
        self.assertNotIn("previous_response_id", fixture.primaries[-1])
        counts = len(fixture.primaries)
        self.assertEqual(proxy.send([c], previous_response_id="unobserved")[0], 409)
        self.assertEqual(len(fixture.primaries), counts)

    def test_conversation_id_expands_only_observed_transcript(self):
        a = {"role": "user", "content": "CONVERSATION_A"}
        out = {"role": "assistant", "content": "ACTUAL_CONVERSATION_OUTPUT"}
        b = {"role": "user", "content": "CONVERSATION_B"}
        def primary(_body, n):
            value = completed([out], f"conv_resp_{n}")
            value["conversation"] = {"id": "conv_observed"}
            return 200, value
        fixture = self.fixture(primary)
        proxy = TransparentProxy(self.state, fixture, ["--review-every-requests", "100"])
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send([a])[0], 200)
        status, result = proxy.send([b], conversation={"id": "conv_observed"})
        self.assertEqual(status, 200, result)
        self.assertEqual(fixture.primaries[-1]["input"], [a, out, b])
        self.assertNotIn("conversation", fixture.primaries[-1], "upstream must not re-expand selected context")
        count = len(fixture.primaries)
        self.assertEqual(proxy.send([b], conversation="unobserved")[0], 409)
        self.assertEqual(len(fixture.primaries), count)


if __name__ == "__main__":
    unittest.main()
