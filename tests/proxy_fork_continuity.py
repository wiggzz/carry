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
    intent: tuple[str, str] | None = None
    history_policy = None

    def request(self, method, path, body=None):
        headers = {"Content-Type": "application/json", "x-carry-session": "state-regression",
                   "x-carry-branch": self.branch}
        if self.history_policy:
            headers["x-carry-history-policy"] = self.history_policy
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


    def seed(self, extra_env=None):
        history = [{"role": "user", "content": "PROTECTED_GOAL"},
                   {"role": "assistant", "content": "RAW_A " + "old payload " * 3000},
                   {"role": "user", "content": "A_CUT"}]
        def review(body, n):
            ids = ledger(body)["eligible_group_ids"]
            return 200, advice_response({"protected": [i for i in ids if i == "g1"],
                "removable": [i for i in ids if i != "g1"],
                "memories": [{"source_ids": ["g2" if n == 1 else ids[0]], "text": "A_FACT" if n == 1 else "SIBLING_ONLY_FACT"}]
                    if ids else []})
        fixture = self.fixture(lambda _b, n: (200, completed(response_id=f"resp_{n}")), review)
        options = ["--review-replayed-history", "--payoff-requests", "1",
                   "--classifier-cache-policy", "disabled"]
        proxy = BranchProxy(self.state, fixture, options, extra_env)
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(history)[0], 200)
        saved = proxy.saved()
        self.assertTrue(saved["drop_members"])
        self.assertTrue(saved["memories"])
        return proxy, fixture, history, cut(history, saved["review_epoch"]), options

    def rejected(self, proxy, fixture, history, code):
        counts = (len(fixture.primaries), len(fixture.reviews))
        status, body = proxy.send(history)
        self.assertEqual(status, 409, "declared unavailable fork cannot silently fall back")
        self.assertEqual(body["error"]["code"], code)
        self.assertEqual((len(fixture.primaries), len(fixture.reviews)), counts)

    def test_expired_ancestor_rejected_after_restart(self):
        env = {"CARRY_PROXY_FORK_MAX_AGE_SECS": "1"}
        proxy, fixture, history, handle, options = self.seed(env)
        proxy.close()
        time.sleep(2.05)
        proxy = BranchProxy(self.state, fixture, options, env)
        self.stack.callback(proxy.close)
        proxy.branch, proxy.intent = "expired-child", ("main", handle)
        self.rejected(proxy, fixture, history, "fork_expired")

    def test_byte_retention_refuses_unpublishable_guarantee(self):
        proxy, fixture, history, handle, _ = self.seed({"CARRY_PROXY_FORK_MAX_BYTES": "256"})
        saved = proxy.saved()
        self.assertLessEqual(len(wire(saved["fork_points"])), 256,
                             "actual serialized evidence must obey the byte budget")
        proxy.branch, proxy.intent = "byte-child", ("main", handle)
        self.rejected(proxy, fixture, history, "fork_expired")

    def test_node_retention_evicts_oldest_without_erasing_live_authority(self):
        proxy, fixture, history, handle, _ = self.seed({"CARRY_PROXY_FORK_MAX_NODES": "1"})
        for n in range(2):
            proxy.history_policy = "reset-on-divergence"
            changed = [dict(item) for item in history]
            changed[0]["content"] = f"NEW_EPOCH_{n}"
            self.assertEqual(proxy.send(changed)[0], 200)
        saved = proxy.saved()
        self.assertLessEqual(len(saved["fork_points"]), 1, "bounded nodes across native history epochs")
        self.assertTrue(saved["drop_members"], "archive eviction cannot delete active review authority")
        proxy.history_policy = None
        proxy.branch, proxy.intent = "node-child", ("main", handle)
        self.rejected(proxy, fixture, history, "fork_expired")

    def test_restart_reset_and_sibling_memories_do_not_mutate_ancestor(self):
        proxy, fixture, history, handle, options = self.seed()
        parent = history + [{"role": "user", "content": "B_CONTEXT"}]
        self.assertEqual(proxy.send(parent)[0], 200)
        self.assertEqual(proxy.send(parent)[0], 200)
        self.assertTrue(any(m["text"] == "SIBLING_ONLY_FACT" for m in proxy.saved()["memories"]))
        proxy.history_policy = "reset-on-divergence"
        self.assertEqual(proxy.send([{"role": "user", "content": "NEW_PARENT_EPOCH"}])[0], 200)
        proxy.close()
        proxy = BranchProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        proxy.branch, proxy.intent = "restart-child", ("main", handle)
        child = history + [{"role": "user", "content": "C_CONTEXT"}]
        calls = len(fixture.reviews)
        self.assertEqual(proxy.send(child)[0], 200)
        sent = json.dumps(fixture.primaries[-1])
        self.assertIn("A_FACT", sent)
        self.assertNotIn("SIBLING_ONLY_FACT", sent)
        self.assertNotIn("RAW_A", sent)
        self.assertNotIn("B_CONTEXT", sent)
        self.assertEqual(len(fixture.reviews), calls)
        # Repeated intent is idempotent; never rehydrate an already live child.
        self.assertEqual(proxy.send(child)[0], 200)
        self.assertEqual(len(fixture.reviews), calls)
        self.assertEqual(len(proxy.saved()["memories"]), 1)

    def test_incompatible_and_unknown_forks_reject_before_upstream(self):
        proxy, fixture, history, handle, _ = self.seed()
        for n, mutation in enumerate(["content", "order", "instructions", "model", "unknown"]):
            with self.subTest(mutation=mutation):
                child = [dict(item) for item in history]
                fields = {}
                proxy.branch, proxy.intent = f"bad-{n}", ("main", handle)
                if mutation == "content": child[1]["content"] += "tampered"
                if mutation == "order": child[0], child[1] = child[1], child[0]
                if mutation == "instructions": fields["instructions"] = "different policy"
                if mutation == "model": fields["model"] = "different-model"
                if mutation == "unknown": proxy.intent = ("main", "0:" + "f" * 64)
                counts = (len(fixture.primaries), len(fixture.reviews))
                status, body = proxy.send(child, **fields)
                self.assertEqual(status, 409)
                self.assertEqual(body["error"]["code"], "fork_unknown" if mutation == "unknown" else "fork_incompatible")
                self.assertEqual((len(fixture.primaries), len(fixture.reviews)), counts)


    def test_fork_primary_does_not_depend_on_reviewer_capacity(self):
        proxy, fixture, history, handle, options = self.seed()
        proxy.close()
        # Reviewer capacity is not review identity or primary feasibility.
        proxy = BranchProxy(self.state, fixture, options + ["--classifier-max-input-tokens", "1024"])
        self.stack.callback(proxy.close)
        proxy.branch, proxy.intent = "review-pressure-child", ("main", handle)
        calls = len(fixture.reviews)
        status, body = proxy.send(history + [{"role": "user", "content": "FIT_C"}])
        self.assertEqual(status, 200, "inherited authority is not a new reviewer request")
        self.assertNotIn("RAW_A", json.dumps(fixture.primaries[-1]))
        self.assertEqual(len(fixture.reviews), calls)

    def test_fork_genuine_primary_overflow_fails_before_upstream(self):
        proxy, fixture, history, handle, options = self.seed()
        proxy.close()
        options += ["--primary-context-tokens", "1024", "--primary-output-reserve-tokens", "1",
                    "--context-margin-percent", "0"]
        proxy = BranchProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        proxy.branch, proxy.intent = "fit-capacity-child", ("main", handle)
        counts = len(fixture.reviews)
        self.assertEqual(proxy.send(history + [{"role": "user", "content": "FIT_C"}])[0], 200)
        self.assertNotIn("RAW_A", json.dumps(fixture.primaries[-1]))
        primaries = len(fixture.primaries)
        proxy.branch = "overflow-child"
        status, _ = proxy.send(history + [{"role": "user", "content": "PROTECTED_C " * 10000}])
        self.assertEqual(status, 413, "genuine primary overflow is not reviewer rescue")
        self.assertEqual(len(fixture.primaries), primaries)
        self.assertEqual(len(fixture.reviews), counts)
        # Undeclared fresh fitting histories still forward unchanged.
        proxy.branch, proxy.intent = "fresh-control", None
        fresh = [{"role": "user", "content": "FRESH_WITHOUT_ADVICE"}]
        self.assertEqual(proxy.send(fresh)[0], 200)
        self.assertEqual(fixture.primaries[-1]["input"], fresh)

    def test_held_parent_reviewer_does_not_block_child(self):
        proxy, fixture, history, handle, _ = self.seed()
        def held_review(_body, _n):
            fixture.held.set()
            fixture.release.wait(15)
            return 200, advice_response({"protected": [], "removable": [], "memories": []})
        fixture.review = held_review
        proxy.wait_reviews = False
        parent = history + [{"role": "user", "content": "HELD_B"}]
        self.assertEqual(proxy.send(parent)[0], 200)
        self.assertEqual(proxy.send(parent)[0], 200)
        self.assertTrue(fixture.held.wait(3))
        try:
            proxy.branch, proxy.intent = "concurrent-child", ("main", handle)
            started = time.monotonic()
            self.assertEqual(proxy.send(history + [{"role": "user", "content": "LIVE_C"}])[0], 200)
            self.assertLess(time.monotonic() - started, 3)
            self.assertFalse(fixture.release.is_set())
            sent = json.dumps(fixture.primaries[-1])
            self.assertNotIn("RAW_A", sent)
            self.assertNotIn("HELD_B", sent)
            self.assertEqual(proxy.saved()["shadow"]["calls"], 0)
        finally:
            fixture.release.set()


if __name__ == "__main__":
    unittest.main(defaultTest="ForkContinuity")
