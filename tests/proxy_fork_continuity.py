"""Explicit full-history fork HTTP contract; not native Pi/Codex acceptance."""
import hashlib
import http.client
import json
import time
import unittest

from proxy_state_regressions import StateRegressions, Proxy, advice_response, completed, ledger, wire


def cut(items, epoch=0):
    raw = json.dumps(items, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
    return f"{epoch}:{hashlib.sha256(raw).hexdigest()}"


class BranchProxy(Proxy):
    branch = "main"
    intent = None

    def request(self, method, path, body=None):
        headers = {"Content-Type": "application/json", "x-carry-session": "state-regression",
                   "x-carry-branch": self.branch}
        if self.intent:
            headers.update({"x-carry-fork-parent": self.intent[0],
                            "x-carry-fork-point": self.intent[1]})
        connection = http.client.HTTPConnection(*self.address, timeout=30)
        try:
            connection.request(method, path, body=None if body is None else wire(body), headers=headers)
            response = connection.getresponse()
            return response.status, json.loads(response.read())
        finally:
            connection.close()

    def saved(self):
        identity = hashlib.sha256(wire(["local", "state-regression", self.branch])).hexdigest()
        path = self.state / (identity + ".json")
        return json.loads(path.read_text()) if path.exists() else {}


class ForkContinuity(unittest.TestCase):
    setUp = StateRegressions.setUp
    record_evidence = StateRegressions.record_evidence
    fixture = StateRegressions.fixture
    # Do not inherit the unrelated regression selectors into this suite.
    def test_historical_full_history_fork_after_parent_cleanup(self):
        old = {"role": "assistant", "content": "ANCESTOR_RAW " + "discardable payload " * 5000}
        cohort = [
            {"type": "reasoning", "summary": [{"type": "summary_text", "text": "ANCESTOR_REASON"}]},
            {"type": "function_call", "call_id": "a1", "name": "one", "arguments": "{}"},
            {"type": "function_call", "call_id": "a2", "name": "two", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "a1", "output": "ANCESTOR_RESULT_ONE"},
            {"type": "function_call_output", "call_id": "a2", "output": "ANCESTOR_RESULT_TWO"},
        ]
        history = [{"role": "user", "content": "PROTECTED_ANCESTOR_GOAL"}, old, *cohort,
                   {"role": "user", "content": "ANCESTOR_BOUNDARY"}]

        def review(body, n):
            ids = ledger(body)["eligible_group_ids"]
            return 200, advice_response({"protected": [i for i in ids if i == "g1"],
                "removable": [i for i in ids if i != "g1"],
                "memories": [{"source_ids": ["g2"], "text": "ANCESTOR_FACT"}]
                    if n == 1 else []})

        fixture = self.fixture(lambda _body, n: (200, completed(response_id=f"resp_{n}")), review)
        proxy = BranchProxy(self.state, fixture, ["--review-replayed-history", "--payoff-requests", "1",
                            "--classifier-cache-policy", "disabled"])
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(history)[0], 200)
        seed = proxy.saved()
        self.assertTrue(seed["drop_members"], "seed real accepted reviewer authority")
        self.assertTrue(any(m["text"] == "ANCESTOR_FACT" for m in seed["memories"]))
        parent = history + [{"role": "user", "content": "SIBLING_B_ONLY"}]
        self.assertEqual(proxy.send(parent)[0], 200)
        advanced = proxy.saved()
        self.assertTrue(any(i["removed"] for i in advanced["history"]))
        self.assertGreater(advanced["reviewer_compactions"], 0)
        self.assertNotIn("ANCESTOR_RAW", json.dumps(fixture.primaries[-1]))
        self.assertEqual(proxy.send(parent)[0], 200)
        self.assertNotIn("ANCESTOR_RAW", json.dumps(fixture.primaries[-1]), "existing lineage never resurrects")
        reviews_before = len(fixture.reviews)
        proxy.branch = "child-c"
        proxy.intent = ("main", cut(history, seed["review_epoch"]))
        child = history + [{"role": "user", "content": "CHILD_C_ONLY"}]
        status, result = proxy.send(child)
        self.assertEqual(status, 200, result)
        sent = fixture.primaries[-1]["input"]
        encoded = json.dumps(sent)
        self.assertFalse("ANCESTOR_RAW" in encoded,
                         "explicit historical fork must recover accepted A authority after cleanup")
        self.assertIn("ANCESTOR_FACT", encoded)
        self.assertIn(history[0], sent)
        self.assertIn(child[-1], sent)
        self.assertNotIn("SIBLING_B_ONLY", encoded)
        present = [item in sent for item in cohort]
        self.assertTrue(all(present) or not any(present), "whole native cohort only")
        self.assertEqual(len(fixture.reviews), reviews_before, "no paid fork recovery review")
        saved = proxy.saved()
        self.assertEqual(saved["shadow"]["calls"], 0, "never copy parent spend")
        self.assertEqual(saved["reviewer_cache"], [], "never copy parent cache receipts")


if __name__ == "__main__":
    unittest.main()
