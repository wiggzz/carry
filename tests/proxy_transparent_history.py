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


if __name__ == "__main__":
    unittest.main()
