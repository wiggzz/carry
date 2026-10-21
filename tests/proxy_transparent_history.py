"""Transparent, caller-owned history matching through real loopback review."""
import http.client
import json
import time
import unittest
import proxy_state_regressions as regressions
from proxy_state_regressions import Proxy, advice_response, completed, ledger, wire


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

    def checkpoint(self, items):
        # The server's bounded index owns file identity; never pick the first JSON.
        index = json.loads((self.state / "history.index").read_text())
        found = []
        for reference in index["tips"].values():
            path = self.state / (reference["session"] + ".json")
            state = index.get("snapshots", {}).get(reference["session"]) or json.loads(path.read_text())
            values = [i["value"] for i in state["history"]]
            if state.get("history_frontier"):
                values = []
                frontier = state["history_frontier"]
                while frontier in index["nodes"]:
                    node = index["nodes"][frontier]
                    values.append(node["value"])
                    frontier = node["parent"]
                values.reverse()
            if values == items:
                found.append((path, state))
        assert len(found) == 1, (items, len(found))
        return found[0]

    def saved(self):
        # Settle all fixture-owned workers; never pick an arbitrary checkpoint.
        path = self.state / "history.index"
        if not path.exists():
            return {"background_review_pending": False}
        states = json.loads(path.read_text()).get("snapshots", {}).values()
        return {"background_review_pending": any(s.get("background_review_pending") for s in states)}


class TransparentHistory(unittest.TestCase):
    setUp = regressions.StateRegressions.setUp
    record_evidence = regressions.StateRegressions.record_evidence
    fixture = regressions.StateRegressions.fixture

    def test_no_header_fork_reuses_committed_ancestor_after_cleanup_and_restart(self):
        history = [{"role": "user", "content": "ANCESTOR_GOAL"},
                   {"role": "assistant", "content": "RAW_A " + "payload " * 6000},
                   {"role": "user", "content": "A_BOUNDARY"}]
        def review(body, n):
            ids = ledger(body)["eligible_group_ids"]
            return 200, advice_response({"protected": [i for i in ids if i == "g1"], "removable": [i for i in ids if i != "g1"],
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


    def seed(self, env=None, review=None, extra=()):
        history = [{"role": "user", "content": "PROTECTED_GOAL"},
                   {"role": "assistant", "content": "RAW_A " + "payload " * 6000},
                   {"role": "user", "content": "A_BOUNDARY"}]
        def default_review(body, n):
            ids = ledger(body)["eligible_group_ids"]
            return 200, advice_response({"protected": [i for i in ids if i == "g1"],
                "removable": [i for i in ids if i != "g1"],
                "memories": [{"source_ids": ["g2"], "text": "A_FACT"}] if n == 1 else []})
        fixture = self.fixture(lambda _b, n: (200, completed(response_id=f"resp_{n}")), review or default_review)
        options = ["--review-replayed-history", "--payoff-requests", "1",
                   "--classifier-cache-policy", "disabled", *extra]
        proxy = TransparentProxy(self.state, fixture, options, env)
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(history)[0], 200)
        return proxy, fixture, history, options

    def test_parallel_cohorts_and_cold_child_receipts(self):
        proxy, fixture, history, options = self.seed()
        # First seed proves review; a separate actual complete cohort is then reviewed.
        cohort = [{"type": "reasoning", "summary": [{"type": "summary_text", "text": "COHORT_REASON"}]},
                  {"type": "function_call", "call_id": "one", "name": "one", "arguments": "{}"},
                  {"type": "function_call", "call_id": "two", "name": "two", "arguments": "{}"},
                  {"type": "function_call_output", "call_id": "one", "output": "PARALLEL_ONE"},
                  {"type": "function_call_output", "call_id": "two", "output": "PARALLEL_TWO"}]
        ancestor = history + cohort + [{"role": "user", "content": "AFTER_PARALLEL"}]
        self.assertEqual(proxy.send(ancestor)[0], 200)
        parent = ancestor + [{"role": "user", "content": "B_ONLY"}]
        self.assertEqual(proxy.send(parent)[0], 200)
        path, _ = proxy.checkpoint(parent)
        original = path.read_bytes()
        child = ancestor + [{"role": "user", "content": "C_ONLY"}]
        before = len(fixture.reviews)
        self.assertEqual(proxy.send(child)[0], 200)
        sent = fixture.primaries[-1]["input"]
        present = [v in sent for v in cohort]
        self.assertTrue(all(present) or not any(present), "all parallel reasoning/calls/results travel together")
        self.assertNotIn("B_ONLY", json.dumps(sent))
        self.assertIn("A_FACT", json.dumps(sent))
        _, state = proxy.checkpoint(child)
        self.assertEqual(state["primary"]["calls"], 1)
        self.assertEqual(state["shadow"]["calls"], 0)
        self.assertEqual(state["reviewer_cache"], [])
        self.assertEqual(len(fixture.reviews), before)
        self.assertEqual(path.read_bytes(), original)

    def test_later_review_closure_never_exports_b_dependent_fact_to_a_c(self):
        def review(body, n):
            ids = ledger(body)["eligible_group_ids"]
            return 200, advice_response({"protected": [i for i in ids if i == "g2"],
                "removable": [], "memories": [{"source_ids": ["g2"],
                "text": "A_FACT" if n == 1 else "B_DEPENDENT_FACT_CLAIMS_A"}]})
        proxy, fixture, history, _ = self.seed(review=review, extra=["--min-payback-percent", "100"])
        b = history + [{"role": "assistant", "content": "B_OBSERVED " + "payload " * 100},
                       {"role": "user", "content": "B_BOUNDARY"}]
        self.assertEqual(proxy.send(b)[0], 200)
        self.assertGreaterEqual(len(fixture.reviews), 2)
        _, parent = proxy.checkpoint(b)
        self.assertTrue(any(m["text"] == "B_DEPENDENT_FACT_CLAIMS_A" for m in parent["memories"]))
        index = json.loads((self.state / "history.index").read_text())
        self.assertTrue(any(any(m["text"] == "B_DEPENDENT_FACT_CLAIMS_A" for m in p["memories"])
                            for versions in index["reviews"].values() for p in versions),
                        "later compatible reviews must be committed too, not first-raw-only: "
                        + json.dumps({"feedback": parent.get("review_feedback"),
                                      "versions": parent.get("fork_points"),
                                      "stderr": proxy.stderr.read_text()[-3000:]}))
        c = history + [{"role": "user", "content": "C_NOT_B"}]
        self.assertEqual(proxy.send(c)[0], 200)
        _, child = proxy.checkpoint(c)
        self.assertTrue(any(m["text"] == "A_FACT" for m in child["memories"]))
        self.assertFalse(any("B_DEPENDENT" in m["text"] for m in child["memories"]))
        # The same later frontier IS reusable when the complete B dependency
        # remains. Advance its mutable tip first so this must hydrate B's review.
        self.assertEqual(proxy.send(b + [{"role": "user", "content": "B_NEXT"}])[0], 200)
        d = b + [{"role": "user", "content": "B_FORK_D"}]
        self.assertEqual(proxy.send(d)[0], 200)
        _, compatible = proxy.checkpoint(d)
        self.assertTrue(any(m["text"] == "B_DEPENDENT_FACT_CLAIMS_A" for m in compatible["memories"]))
        self.assertNotIn("B_NEXT", json.dumps(fixture.primaries[-1]["input"]))

    def test_empty_input_response_is_observed_and_resumable(self):
        answer = {"role": "assistant", "content": "EMPTY_INPUT_ACTUAL_OUTPUT"}
        fixture = self.fixture(lambda _b, n: (200, completed([answer], f"empty_{n}")))
        proxy = TransparentProxy(self.state, fixture, ["--review-every-requests", "100"])
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send([])[0], 200)
        proxy.close()
        proxy = TransparentProxy(self.state, fixture, ["--review-every-requests", "100"])
        self.stack.callback(proxy.close)
        followup = {"role": "user", "content": "AFTER_EMPTY"}
        status, response = proxy.send([followup], previous_response_id="empty_1")
        self.assertEqual(status, 200, response)
        self.assertEqual(fixture.primaries[-1]["input"], [answer, followup])

    def test_active_checkpoints_reference_shared_canonical_ancestry(self):
        proxy, fixture, history, options = self.seed()
        self.assertEqual(proxy.send(history + [{"role": "user", "content": "B_ONLY"}])[0], 200)
        child = history + [{"role": "user", "content": "C_ONLY"}]
        self.assertEqual(proxy.send(child)[0], 200)
        archive = json.loads((self.state / "history.index").read_text())
        checkpoints = [json.loads(p.read_text()) for p in self.state.glob("*.json")]
        self.assertGreaterEqual(len(checkpoints), 2)
        for state in checkpoints:
            self.assertTrue(all(item["value"] is None for item in state["history"]),
                            "mutable checkpoints must reference, not copy, canonical wire history")
            frontier = state.get("history_frontier")
            restored = []
            while frontier in archive["nodes"]:
                node = archive["nodes"][frontier]
                restored.append(node["value"])
                frontier = node["parent"]
            self.assertEqual(len(restored), len(state["history"]))
            self.assertIn(history[1], restored)
        self.assertEqual(sum(n["value"] == history[1] for n in archive["nodes"].values()), 1)
        proxy.close()
        proxy = TransparentProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(child + [{"role": "user", "content": "D_ONLY"}])[0], 200)
        self.assertNotIn("RAW_A", json.dumps(fixture.primaries[-1]["input"]))
        self.assertIn("A_FACT", json.dumps(fixture.primaries[-1]["input"]))

    def test_unknown_settings_and_client_rewrites_do_not_reuse_advice(self):
        proxy, fixture, history, _ = self.seed()
        changed = history + [{"role": "user", "content": "CHANGED_SETTINGS"}]
        self.assertEqual(proxy.send(changed, instructions="different policy context")[0], 200)
        self.assertIn("RAW_A", json.dumps(fixture.primaries[-1]))
        self.assertNotIn("A_FACT", json.dumps(fixture.primaries[-1]))
        rewrite = [{"role": "user", "content": "CLIENT_SUMMARY_DIFFERENT_HISTORY"}]
        self.assertEqual(proxy.send(rewrite)[0], 200)
        self.assertEqual(fixture.primaries[-1]["input"], rewrite)
        count = len(fixture.primaries)
        self.assertEqual(proxy.send([], previous_response_id="resp_1", instructions="wrong settings")[0], 409)
        self.assertEqual(len(fixture.primaries), count)

    def test_failed_review_does_not_publish_portable_advice(self):
        proxy, fixture, history, _ = self.seed(review=lambda _b, _n: (500, {"error": "scripted_failure"}))
        parent = history + [{"role": "user", "content": "B_ONLY"}]
        self.assertEqual(proxy.send(parent)[0], 200)
        child = history + [{"role": "user", "content": "C_ONLY"}]
        self.assertEqual(proxy.send(child)[0], 200)
        self.assertIn("RAW_A", json.dumps(fixture.primaries[-1]))
        self.assertEqual(json.loads((self.state / "history.index").read_text())["reviews"], {})

    def test_index_byte_node_age_limits_and_evicted_ids_fail_closed(self):
        env = {"CARRY_PROXY_HISTORY_MAX_BYTES": "4096", "CARRY_PROXY_HISTORY_MAX_NODES": "4",
               "CARRY_PROXY_HISTORY_MAX_REFERENCES": "4",
               "CARRY_PROXY_FORK_MAX_AGE_SECS": "1"}
        fixture = self.fixture(lambda _b, n: (200, completed(response_id=f"resp_{n}")))
        proxy = TransparentProxy(self.state, fixture, ["--review-every-requests", "100"], env)
        self.stack.callback(proxy.close)
        for n in range(8):
            self.assertEqual(proxy.send([{"role": "user", "content": f"UNIQUE_{n}"}])[0], 200)
            raw = (self.state / "history.index").read_bytes()
            index = json.loads(raw)
            self.assertLessEqual(len(raw), 4096)
            self.assertLessEqual(len(index["nodes"]), 4)
            self.assertLessEqual(len(index["tips"]) + len(index["transcripts"]), 4)
        self.assertEqual(proxy.send([], previous_response_id="resp_1")[0], 409)
        proxy.close()
        time.sleep(2.05)
        # Orphan publication is never authoritative.
        (self.state / "history.tmp").write_text('{"fake":"uncommitted"}')
        proxy = TransparentProxy(self.state, fixture, ["--review-every-requests", "100"], env)
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send([], previous_response_id="resp_8")[0], 409)

    def test_killed_uncommitted_review_does_not_replace_committed_ancestor(self):
        import threading
        held, release = threading.Event(), threading.Event()
        self.stack.callback(release.set)
        def review(body, n):
            ids = ledger(body)["eligible_group_ids"]
            if n == 2:
                held.set()
                assert release.wait(10), "held review release"
            return 200, advice_response({"protected": [i for i in ids if i == "g1"],
                "removable": [i for i in ids if i != "g1"],
                "memories": [{"source_ids": ["g2"], "text": "A_FACT" if n == 1 else "UNCOMMITTED_B_FACT"}]})
        proxy, fixture, history, options = self.seed(review=review)
        proxy.close()
        proxy = TransparentProxy(self.state, fixture, [*options, "--background-reviews=true"])
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(history + [{"role": "assistant", "content": "B_ONLY " + "payload " * 100},
                                               {"role": "user", "content": "B_BOUNDARY"}], wait=False)[0], 200)
        self.assertTrue(held.wait(5), "actual dispatched reviewer must be in flight")
        proxy.close()
        release.set()
        (self.state / "orphan.json.tmp").write_text('{"memories":[{"text":"UNCOMMITTED_B_FACT"}]}')
        proxy = TransparentProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        child = history + [{"role": "user", "content": "C_ONLY"}]
        self.assertEqual(proxy.send(child)[0], 200)
        sent = json.dumps(fixture.primaries[-1])
        self.assertIn("A_FACT", sent)
        self.assertNotIn("RAW_A", sent)
        self.assertNotIn("UNCOMMITTED_B_FACT", sent)
        _, state = proxy.checkpoint(child)
        self.assertEqual(state["shadow"]["calls"], 0)

    def test_concurrent_siblings_keep_independent_selection_and_restart(self):
        import concurrent.futures
        proxy, fixture, history, options = self.seed()
        # Retire the common live tip so both children use the immutable review.
        self.assertEqual(proxy.send(history + [{"role": "user", "content": "B_ONLY"}])[0], 200)
        children = [history + [{"role": "user", "content": f"SIBLING_{n}"}] for n in range(4)]
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            results = list(pool.map(proxy.send, children))
        self.assertTrue(all(s == 200 for s, _ in results), results)
        paths = [proxy.checkpoint(c)[0] for c in children]
        self.assertEqual(len(set(paths)), 4)
        for c in children:
            _, state = proxy.checkpoint(c)
            self.assertEqual(state["primary"]["calls"], 1)
            self.assertEqual(state["shadow"]["calls"], 0)
        proxy.close()
        proxy = TransparentProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        for c in children:
            self.assertEqual(proxy.send(c)[0], 200)
            self.assertNotIn("RAW_A", json.dumps(fixture.primaries[-1]))


if __name__ == "__main__":
    unittest.main()
