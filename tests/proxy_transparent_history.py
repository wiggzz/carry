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

    def send(self, input_items, *, wait=True, **fields):
        response = self.request("POST", "/v1/responses",
                                {"model": "gpt-6-luna", "input": input_items, **fields})
        self.response_id = response[1].get("id")
        if wait and self.wait_reviews:
            deadline = time.monotonic() + 15
            while self.saved().get("background_review_pending"):
                assert time.monotonic() < deadline, "post-response reviewer did not settle"
                time.sleep(0.01)
        return response

    def saved(self):
        # Settle the exact completed response, not an unrelated/orphaned worker.
        path = self.state / "history.index"
        if path.exists():
            index = json.loads(path.read_text())
            for reference in index["transcripts"].values():
                if reference["external_id"] == getattr(self, "response_id", None):
                    return index.get("snapshots", {}).get(reference["session"], {})
        return {"background_review_pending": False}


class TransparentHistory(unittest.TestCase):
    setUp = regressions.StateRegressions.setUp
    record_evidence = regressions.StateRegressions.record_evidence
    fixture = regressions.StateRegressions.fixture

    def proxy(self, fixture, options):
        proxy = TransparentProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        return proxy

    def test_request_projection_can_restore_genuinely_rewarmed_original_source(self):
        """A past cold omission is an audit event, never semantic removal authority."""
        def primary(body, n):
            value = completed(response_id=f"reversible_{n}")
            value["usage"]["input_tokens"] = max(100, len(wire(body["input"])) // 4)
            value["usage"]["input_tokens_details"]["cached_tokens"] = (
                value["usage"]["input_tokens"] * 99 // 100)
            return 200, value

        def review(_body, _n):
            return 200, advice_response({"protected": [], "removable": ["g2"], "memories": []})

        fixture = self.fixture(primary, review)
        options = ["--payoff-requests", "1", "--classifier-cache-policy", "disabled"]
        proxy = self.proxy(fixture, [*options, "--review-replayed-history"])
        history = [{"role": "user", "content": "PROTECTED_ORIGINAL_GOAL " + "direction " * 10000},
                   {"role": "assistant", "content": "REVERSIBLE_ORIGINAL_SOURCE " + "payload " * 6000},
                   {"role": "user", "content": "CURRENT_H_DIRECTION"}]
        self.assertEqual(proxy.send(history)[0], 200)
        proxy.close()
        # The loopback destination cannot advertise official provider semantics.
        # Attest only this synthetic fixture's cache capability on the frozen
        # pair; native cache activity still comes solely from actual HTTP usage.
        path = self.state / "history.index"
        index = json.loads(path.read_text())
        for snapshot in index["snapshots"].values():
            snapshot["model_resolution"]["primary"]["cache"] = {
                "minimum_prefix_tokens": 1024, "max_read_breakpoints": 50,
                "max_write_breakpoints": 4, "implicit_breakpoint_uses_write_slot": True}
        path.write_bytes(wire(index))
        options += ["--review-every-requests", "100"]
        # Advertised fixture capability precedes a real canonical dispatch.
        # No saved cache receipt is manufactured by the attestation above.
        warm = TransparentProxy(self.state, fixture, options, mode="audit")
        self.stack.callback(warm.close)
        self.assertEqual(warm.send(history)[0], 200)
        warm.close()
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertIn(history[1], fixture.primaries[-1]["input"], "warm prefix favors original source")
        proxy.close()
        # Stage elapsed provider TTL only, not semantic advice, a receipt or a
        # planner result. Every outgoing body is still rendered by the real CLI.
        path = self.state / "history.index"
        index = json.loads(path.read_text())
        for entry in index.get("upstream_cache", []):
            entry["at"] = 1
        for snapshot in index["snapshots"].values():
            for field in ("primary_cache", "reviewer_cache", "upstream_cache"):
                for entry in snapshot.get(field, []):
                    entry["at"] = 1
                    if "read_confirmed_at" in entry:
                        entry["read_confirmed_at"] = 1
        path.write_bytes(wire(index))
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertNotIn(history[1], fixture.primaries[-1]["input"], "expired prefix permits cold omission")
        cold = fixture.primaries[-1]
        proxy.close()
        # If both shapes are warm the smaller compact view can remain cheaper.
        # Expire its new receipt before genuinely re-warming canonical H alone.
        index = json.loads(path.read_text())
        for entry in index["upstream_cache"]:
            entry["at"] = 1
        path.write_bytes(wire(index))
        # Audit mode genuinely dispatches canonical H. Only its terminal provider
        # receipt may re-warm that exact shape; no invented saved cache entries.
        proxy = TransparentProxy(self.state, fixture, options, mode="audit")
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(history)[0], 200)
        rewarmed = fixture.primaries[-1]
        self.evidence.update(cold=cold, actual_rewarm=rewarmed, upstream=fixture.primaries)
        self.assertIn(history[1], rewarmed["input"], "audit must dispatch canonical original H after prior omission")
        proxy.close()
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send(history)[0], 200)
        self.evidence["restored"] = fixture.primaries[-1]
        self.assertIn(history[1], fixture.primaries[-1]["input"],
                      "genuine compatible cache evidence must make the same original H retainable again")
        self.assertIn(history[0], fixture.primaries[-1]["input"])
        self.assertEqual(len(fixture.reviews), 1, "reversibility needs no paid semantic recovery")


    def test_repeated_projections_do_not_starve_async_review(self):
        def review(body, n):
            eligible = ledger(body)["eligible_group_ids"]
            return 200, advice_response({"protected": [], "removable": eligible, "memories": []})
        fixture = self.fixture(lambda _b,n: (200,completed(response_id=f"starve_{n}")), review)
        proxy = self.proxy(fixture,["--review-replayed-history","--payoff-requests","1","--classifier-cache-policy","disabled"])
        h = [{"role":"user","content":"goal"},
             {"role":"assistant","content":"old output " * 6000},
             {"role":"user","content":"next"}]
        self.assertEqual(proxy.send(h)[0],200)
        self.assertEqual(len(fixture.reviews),1)
        for n in range(3):
            h += [{"role":"assistant","content":f"later {n} " * 3000},
                  {"role":"user","content":f"continue {n}"}]
            self.assertEqual(proxy.send(h)[0],200)
        self.evidence.update(primaries=fixture.primaries,reviews=fixture.reviews)
        self.assertGreater(len(fixture.reviews),1,"selected projections must not suppress ordinary async review")

    def test_completed_advice_survives_two_days_and_more_than_eight_cuts(self):
        fixture = self.fixture(lambda _b,n: (200,completed(response_id=f"retained_{n}")),
            lambda body,n: (200,advice_response({"protected":[],"removable":["g2"],"memories":[]})))
        proxy = self.proxy(fixture,["--review-replayed-history","--payoff-requests","1","--classifier-cache-policy","disabled"])
        h = [{"role":"user","content":"goal"},{"role":"assistant","content":"reviewed source " * 6000},
             {"role":"user","content":"cut 0"}]
        self.assertEqual(proxy.send(h)[0],200)
        original = list(h)
        for n in range(10):
            h += [{"role":"user","content":f"cut {n+1}"}]
            self.assertEqual(proxy.send(h)[0],200)
        proxy.close()
        path = self.state / "history.index"
        index = json.loads(path.read_text())
        # Clock seam only: no manufactured advice, wire/cache receipt or plan.
        for records in index["reviews"].values():
            for record in (records if isinstance(records,list) else [records]):
                record["at"] = int(time.time()) - 2*86400
        path.write_bytes(wire(index))
        proxy = self.proxy(fixture,["--review-every-requests","100","--payoff-requests","1","--classifier-cache-policy","disabled"])
        self.assertEqual(proxy.send(original+[{"role":"user","content":"sibling"}])[0],200)
        self.evidence.update(index=index,primaries=fixture.primaries)
        self.assertNotIn(original[1],fixture.primaries[-1]["input"],"seven-day review retention must not expire after one day")

    def test_no_fit_acquires_one_bounded_review_before_primary(self):
        fixture = self.fixture(lambda _b,n: (200,completed(response_id=f"fit_{n}")),
            lambda _b,n: (200,advice_response({"protected":[],"removable":["g2"],"memories":[]})))
        h = [{"role":"user","content":"goal"},{"role":"assistant","content":"bulky output " * 12000},
             {"role":"user","content":"current task"}]
        proxy = self.proxy(fixture,["--review-every-requests","100","--classifier-cache-policy","disabled"])
        self.assertEqual(proxy.send(h)[0],200)
        self.assertEqual(len(fixture.reviews),0)
        proxy.close()
        proxy = self.proxy(fixture,["--review-every-requests","100","--classifier-cache-policy","disabled",
            "--primary-context-tokens","5000","--primary-output-reserve-tokens","1","--context-margin-percent","0"])
        self.assertEqual(proxy.send(h)[0],200,"genuine no-fit may acquire one useful review before primary")
        self.assertEqual(len(fixture.reviews),1)
        self.assertNotIn(h[1],fixture.primaries[-1]["input"])
        self.assertEqual(proxy.saved()["shadow"]["calls"],1)
        self.evidence.update(primaries=fixture.primaries,reviews=fixture.reviews,state=proxy.saved())

    def test_no_fit_exception_failure_no_help_and_timeout_never_send_primary(self):
        original_state = self.state
        for outcome in ("failure", "no_help", "timeout"):
            with self.subTest(outcome=outcome):
                self.state = original_state / outcome
                def review(_body, _n):
                    if outcome == "timeout":
                        time.sleep(3)
                    if outcome == "failure":
                        return 503, {"error": "synthetic failure"}
                    return 200, advice_response({"protected": [], "removable": [], "memories": []})
                fixture = self.fixture(lambda _b,n: (200, completed(response_id=f"nohelp_{n}")), review)
                history = [{"role":"user","content":"goal"},
                           {"role":"assistant","content":"UNREVIEWED_SOURCE " * 12000},
                           {"role":"user","content":"current"}]
                options = ["--review-every-requests","100","--classifier-cache-policy","disabled"]
                proxy = self.proxy(fixture, options)
                self.assertEqual(proxy.send(history)[0], 200)
                proxy.close()
                proxy = self.proxy(fixture, options + ["--primary-context-tokens","5000",
                    "--primary-output-reserve-tokens","1","--context-margin-percent","0",
                    "--classifier-timeout-secs","1"])
                started = time.monotonic()
                self.assertEqual(proxy.send(history)[0], 413)
                self.assertLess(time.monotonic() - started, 2.5, "one bounded exceptional attempt")
                self.assertEqual(len(fixture.reviews), 1)
                self.assertEqual(len(fixture.primaries), 1, "no unqualified source removal or overflowing primary")
                saved = proxy.saved()
                self.assertEqual(saved["shadow"]["calls"], 1)
                self.assertEqual(saved["shadow"]["unavailable_cost_calls"], int(outcome != "no_help"))
                self.assertEqual([i["value"] for i in saved["history"]], history)
                self.evidence[outcome] = {"saved": saved, "reviews": fixture.reviews}
                proxy.close()
        self.state = original_state

    def test_projected_merged_members_publish_under_original_cut_after_advancement(self):
        import threading
        held, release = threading.Event(), threading.Event()
        self.stack.callback(release.set)
        def review(body,n):
            if n == 1:
                return 200,advice_response({"protected":[],"removable":["g3"],"memories":[]})
            if n == 2:
                group = next(g for g in ledger(body)["current_groups"] if g["group_id"] == "g2")
                self.assertEqual(group["member_ids"],[2,4])
                held.set()
                self.assertTrue(release.wait(10))
                return 200,advice_response({"protected":[],"removable":["g2"],"memories":[{"source_ids":["g2"],"text":"MERGED_ORIGINAL_FACT"}]})
            self.fail("no extra review while captured projection is held")
        fixture = self.fixture(lambda _b,n:(200,completed(response_id=f"merged_{n}")),review)
        h = [{"role":"user","content":"original goal"},
             {"role":"assistant","content":"LEFT_SOURCE " * 3000},
             {"role":"user","content":"OLD_SEPARATOR " * 10000},
             {"role":"assistant","content":"RIGHT_SOURCE " * 3000},
             {"role":"user","content":"original H boundary"}]
        options = ["--review-replayed-history","--payoff-requests","1","--classifier-cache-policy","disabled"]
        proxy = self.proxy(fixture,options)
        self.assertEqual(proxy.send(h)[0],200)
        proxy.wait_reviews = False
        self.assertEqual(proxy.send(h)[0],200)
        self.assertTrue(held.wait(3),"selected source projection must still dispatch the next async review")
        advanced = h + [{"role":"user","content":"A_ONLY"}]
        self.assertEqual(proxy.send(advanced)[0],200)
        release.set()
        deadline = time.monotonic()+5
        while proxy.checkpoint(advanced)[1].get("background_review_pending"):
            self.assertLess(time.monotonic(),deadline)
            time.sleep(.01)
        proxy.close()
        proxy = self.proxy(fixture,["--review-every-requests","100","--payoff-requests","1","--classifier-cache-policy","disabled"])
        for suffix in ("B_ONLY","C_ONLY"):
            child = h + [{"role":"user","content":suffix}]
            self.assertEqual(proxy.send(child)[0],200)
            sent = fixture.primaries[-1]["input"]
            for source in h[1:4]: self.assertNotIn(source,sent)
            self.assertIn("MERGED_ORIGINAL_FACT",json.dumps(sent))
            self.assertNotIn("A_ONLY",json.dumps(sent))
            self.assertIn(h[0],sent)
            fact_at = next(i for i,x in enumerate(sent) if "MERGED_ORIGINAL_FACT" in json.dumps(x))
            self.assertLess(fact_at,sent.index(h[4]),"fact belongs after original member 4, before original H direction")
        self.evidence.update(primaries=fixture.primaries,reviews=fixture.reviews)
        self.assertEqual(len(fixture.reviews),2)

    def test_exact_original_review_survives_advanced_owner_and_restart(self):
        """Completed H advice belongs to H, not its worker's now-advanced owner."""
        import threading
        held, release = threading.Event(), threading.Event()
        self.stack.callback(release.set)

        def review(body, n):
            self.assertEqual(n, 1, "one captured-H worker, no synchronous rescue")
            held.set()
            self.assertTrue(release.wait(10), "release the actual held HTTP review")
            self.assertIn("g2", ledger(body)["eligible_group_ids"])
            return 200, advice_response({"protected": [], "removable": ["g2"],
                "memories": [{"source_ids": ["g2"], "text": "EXACT_H_FACT"}]})

        fixture = self.fixture(lambda _b, n: (200, completed(response_id=f"exact_h_{n}")), review)
        options = ["--review-replayed-history", "--payoff-requests", "1",
                   "--classifier-cache-policy", "disabled", "--background-reviews=true"]
        proxy = self.proxy(fixture, options)
        history = [{"role": "user", "content": "KEEP_ORIGINAL_GOAL"},
                   {"role": "assistant", "content": "EXACT_H_SOURCE " + "payload " * 6000},
                   {"role": "user", "content": "H_BOUNDARY"}]
        self.assertEqual(proxy.send(history)[0], 200, "ordinary primary cannot wait for review")
        self.assertTrue(held.wait(3), "real reviewer must be held before advancement")
        advanced = history + [{"role": "user", "content": "A_ONLY"}]
        self.assertEqual(proxy.send(advanced)[0], 200)
        self.assertEqual(len(fixture.primaries), 2, "H+A completed before H review release")
        release.set()
        deadline = time.monotonic() + 5
        while proxy.checkpoint(advanced)[1].get("background_review_pending"):
            self.assertLess(time.monotonic(), deadline, "captured H completion must settle")
            time.sleep(.01)
        settled = proxy.checkpoint(advanced)[1]
        self.assertEqual(settled["shadow"]["calls"], 1, "paid review settles once")
        proxy.close()
        options = ["--review-every-requests", "100", "--payoff-requests", "1",
                   "--classifier-cache-policy", "disabled"]
        proxy = self.proxy(fixture, options)
        wires = []
        for suffix in ("B_ONLY", "C_ONLY"):
            child = history + [{"role": "user", "content": suffix}]
            self.assertEqual(proxy.send(child)[0], 200)
            sent = fixture.primaries[-1]["input"]
            wires.append(sent)
            self.evidence.update(settled=settled, primary_inputs=wires,
                                 reviewer_requests=fixture.reviews)
            self.assertNotIn(history[1], sent,
                "exact original H advice must be reusable after H+A advancement/restart")
            self.assertIn("EXACT_H_FACT", json.dumps(sent))
            self.assertNotIn("A_ONLY", json.dumps(sent))
            self.assertIn(history[0], sent)
            _, state = proxy.checkpoint(child)
            self.assertEqual(state["primary"]["calls"], 1)
            self.assertEqual(state["shadow"]["calls"], 0, "no sibling billed receipt inheritance")
        self.assertEqual(len(fixture.reviews), 1)


    def test_empty_selection_retires_historical_memory_authority(self):
        def review(_body, n):
            return 200, advice_response({"protected": [], "removable": ["m1"] if n == 2 else [],
                "memories": [{"source_ids": ["g2"], "text": "RETIRED_FACT"}] if n == 1 else []})

        fixture = self.fixture(lambda _b, n: (200, completed(response_id=f"retire_{n}")), review)
        options = ["--review-replayed-history", "--payoff-requests", "1",
                   "--classifier-cache-policy", "disabled"]
        proxy = self.proxy(fixture, options)
        history = [{"role": "user", "content": "GOAL"},
                   {"role": "assistant", "content": "SOURCE"},
                   {"role": "user", "content": "BOUNDARY"}]
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertEqual(proxy.advice()["memories"][0]["text"], "RETIRED_FACT")
        self.assertEqual(proxy.send(history)[0], 200)
        owner, retired = proxy.checkpoint(history)
        retired_advice = proxy.advice()
        self.assertEqual(retired_advice["memories"], [])
        self.assertFalse(any(v == "drop" for v in retired_advice["opinions"].values()))
        self.assertTrue(all("removed" not in i for i in retired["history"]))
        auxiliary = history + [{"role": "user", "content": "AUX"}]
        self.assertEqual(proxy.send(auxiliary)[0], 200)
        sibling_bytes = owner.read_bytes()
        proxy.close()
        proxy = self.proxy(fixture, options)
        foreground = history + [{"role": "user", "content": "FOREGROUND"}]
        self.assertEqual(proxy.send(foreground)[0], 200)
        child_owner, child = proxy.checkpoint(foreground)
        self.evidence.update(retired=retired, child=child, primary=fixture.primaries[-1])
        self.assertEqual(proxy.advice()["memories"], [], "empty committed selection must supersede retired facts")
        self.assertNotIn("RETIRED_FACT", json.dumps(fixture.primaries[-1]["input"]))
        self.assertNotEqual(child_owner, owner)
        self.assertEqual(child["primary"]["calls"], 1)
        self.assertEqual(child["shadow"]["calls"], 0)
        self.assertEqual(owner.read_bytes(), sibling_bytes)


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


    def seed(self, env=None, review=None, extra=(), history=None):
        history = history or [{"role": "user", "content": "PROTECTED_GOAL"},
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


    def test_expired_original_input_starts_cold_with_and_without_restart(self):
        root = self.state
        for restart in (False, True):
            with self.subTest(restart=restart):
                self.state = root / ("restart" if restart else "same_process")
                self.state.mkdir()
                proxy, fixture, history, _ = self.seed(extra=["--history-max-age-secs", "1",
                                                             "--review-every-requests", "100"])
                old_path, old = proxy.checkpoint(history)
                self.assertEqual(old["primary"]["calls"], 1)
                self.assertTrue(json.loads((self.state / "history.index").read_text())["reviews"], "real completed advice supplies old authority")
                time.sleep(2.05)
                if restart:
                    proxy.close()
                    proxy = self.proxy(fixture, ["--history-max-age-secs", "1",
                        "--review-every-requests", "100", "--classifier-cache-policy", "disabled"])
                status, result = proxy.send(history)
                self.assertEqual(status, 200, result)
                new_path, saved = proxy.checkpoint(history)
                self.assertNotEqual(new_path, old_path,
                                    "a lookup miss must not pin/revive the expired owner")
                self.assertEqual(saved["primary"]["calls"], 1,
                                 "the new lineage must not inherit expired paid receipts")
                self.assertNotIn("A_FACT", json.dumps(fixture.primaries[-1]))
                if restart:
                    self.assertEqual(saved["shadow"]["calls"], 0)
                    self.assertEqual(fixture.primaries[-1]["input"], history)
                proxy.close()
        self.state = root

    def test_same_input_concurrency_keeps_live_owner_past_age_and_restart(self):
        import concurrent.futures
        import threading
        held, release = threading.Event(), threading.Event()
        self.stack.callback(release.set)
        def primary(_body, n):
            if n == 1:
                held.set()
                assert release.wait(15), "fresh primary release"
            return 200, completed(response_id=f"same_input_{n}")
        fixture = self.fixture(primary)
        options = ["--history-max-age-secs", "1", "--review-every-requests", "100",
                   "--request-timeout-secs", "30"]
        proxy = self.proxy(fixture, options)
        history = [{"role": "user", "content": "CONCURRENT_FRESH_INPUT"}]
        with concurrent.futures.ThreadPoolExecutor() as pool:
            first = pool.submit(proxy.send, history)
            try:
                self.assertTrue(held.wait(3))
                owner, initial = proxy.checkpoint(history)
                self.assertEqual(initial["primary"]["calls"], 0)
                time.sleep(2.05)
                second = pool.submit(proxy.send, history)
                done, _ = concurrent.futures.wait([second], timeout=.2)
                self.assertNotIn(second, done, "same input must queue behind the selected primary")
                self.assertEqual(len(fixture.primaries), 1, "no concurrent duplicate owner/dispatch")
                self.assertEqual(proxy.checkpoint(history)[0], owner)
            finally:
                release.set()
            self.assertEqual(first.result()[0], 200)
            self.assertEqual(second.result()[0], 200)
        _, saved = proxy.checkpoint(history)
        self.assertEqual(saved["primary"]["calls"], 2)
        self.assertEqual(saved["shadow"]["calls"], 0)
        self.assertEqual(len(list(self.state.glob("*.json"))), 1)
        proxy.close()
        proxy = self.proxy(fixture, ["--review-every-requests", "100"])
        self.assertEqual(proxy.send(history)[0], 200)
        resumed_owner, resumed = proxy.checkpoint(history)
        self.assertEqual(resumed_owner, owner)
        self.assertEqual(resumed["primary"]["calls"], 3)
        self.assertEqual(fixture.reviews, [])

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
        self.assertTrue(any(m["text"] == "B_DEPENDENT_FACT_CLAIMS_A" for m in proxy.advice()["memories"]))
        index = json.loads((self.state / "history.index").read_text())
        self.assertTrue(any(any(m["text"] == "B_DEPENDENT_FACT_CLAIMS_A" for m in p["memories"])
                            for p in index["reviews"].values()),
                        "later compatible reviews must be committed too, not first-raw-only: "
                        + json.dumps({"feedback": parent.get("review_feedback"),
                                      "versions": parent.get("fork_points"),
                                      "stderr": (self.state / f"fixture-{proxy.process.pid}-stderr.log").read_text()[-3000:]}))
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

    def test_native_checkpoint_archives_observed_input_and_actual_output(self):
        history = [{"role": "user", "content": "BEFORE_NATIVE_CHECKPOINT"},
                   {"type": "compaction_trigger"}]
        checkpoint = {"type": "compaction", "encrypted_content": "actual_observed_anchor"}
        fixture = self.fixture(lambda _b, _n: (200, completed([checkpoint], "native_1")))
        proxy = TransparentProxy(self.state, fixture, ["--review-every-requests", "100"])
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(history)[0], 200)
        archive = json.loads((self.state / "history.index").read_text())
        references = [r for r in archive["transcripts"].values() if r["external_id"] == "native_1"]
        self.assertEqual(len(references), 1, "native reset must not erase the observed canonical transcript")
        frontier, observed = references[0]["frontier"], []
        while frontier in archive["nodes"]:
            node = archive["nodes"][frontier]
            observed.append(node["value"])
            frontier = node["parent"]
        self.assertEqual(list(reversed(observed)), history + [checkpoint])
        proxy.close()
        proxy = TransparentProxy(self.state, fixture, ["--review-every-requests", "100"])
        self.stack.callback(proxy.close)
        fresh = [{"role": "user", "content": "UNRELATED_AFTER_NATIVE"}]
        self.assertEqual(proxy.send(fresh)[0], 200)
        self.assertEqual(fixture.primaries[-1]["input"], fresh)
        self.assertEqual(len(list(self.state.glob("*.json"))), 2)

    def test_history_empty_envelope_limits_two_requests_and_restart(self):
        fixture = self.fixture(lambda _body, n: (200, completed(response_id=f"minimum_{n}")))
        minimum = len(wire({"format": "carry-prefix-history-v3", "nodes": {},
                            "upstream_cache": [], "tips": {}, "reviews": {},
                            "transcripts": {}, "snapshots": {}}))
        for limit in (2, minimum - 1):
            with self.subTest(rejected_limit=limit):
                with self.assertRaises(AssertionError, msg="impossible history byte limits must reject startup"):
                    unexpected = self.proxy(fixture, ["--history-max-bytes", str(limit)])
                    unexpected.close()
        # The envelope is accepted, but cannot reserve an active selected owner.
        # Backpressure must not publish an unreadable generation or dispatch work.
        options = ["--history-max-bytes", str(minimum), "--review-every-requests", "100"]
        proxy = self.proxy(fixture, options)
        for attempt in range(2):
            status, result = proxy.send([{"role": "user", "content": f"minimum request {attempt}"}])
            self.assertEqual(status, 507, result)
            self.assertNotIn("id", result)
            index = proxy.state / "history.index"
            if index.exists():
                self.assertLessEqual(index.stat().st_size, minimum)
                json.loads(index.read_bytes())
        self.assertEqual(fixture.primaries, [])
        proxy.close()
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send([{"role": "user", "content": "minimum after restart"}])[0], 507)
        self.assertEqual(fixture.primaries, [])

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

    def test_empty_input_repeat_reuses_retained_owner_after_restart(self):
        fixture = self.fixture(lambda _b, n: (200, completed(response_id=f"empty_retry_{n}")))
        proxy = self.proxy(fixture, ["--review-every-requests", "100"])
        self.assertEqual(proxy.send([])[0], 200)
        owner, initial = proxy.checkpoint([])
        self.assertEqual(initial["primary"]["calls"], 1)
        self.assertEqual(proxy.send([])[0], 200)
        selected, retried = proxy.checkpoint([])
        self.assertEqual(selected, owner, "empty live input still selects its retained tip")
        self.assertEqual(retried["primary"]["calls"], 2)
        proxy.close()
        proxy = self.proxy(fixture, ["--review-every-requests", "100"])
        self.assertEqual(proxy.send([])[0], 200)
        selected, restored = proxy.checkpoint([])
        self.assertEqual(selected, owner)
        self.assertEqual(restored["primary"]["calls"], 3)
        self.assertEqual(fixture.reviews, [])

    def test_empty_input_and_output_still_publish_observed_response_id(self):
        fixture = self.fixture(lambda _b, n: (200, completed([], f"zero_{n}")))
        options = ["--review-every-requests", "100"]
        proxy = TransparentProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send([])[0], 200)
        proxy.close()
        proxy = TransparentProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        followup = {"role": "user", "content": "AFTER_ZERO_OUTPUT"}
        status, response = proxy.send([followup], previous_response_id="zero_1")
        self.assertEqual(status, 200, response)
        self.assertEqual(fixture.primaries[-1]["input"], [followup])

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

    def test_explicit_settings_change_cannot_republish_old_observed_ids(self):
        import concurrent.futures
        import threading
        held, release = threading.Event(), threading.Event()
        self.stack.callback(release.set)
        def primary(_body, n):
            if n == 2:
                held.set()
                assert release.wait(5), "held settings-B primary release"
                return 503, {"error": "B did not complete"}
            value = completed(response_id=f"scope_{n}")
            if n == 1:
                value["conversation"] = {"id": "conversation_A"}
            return 200, value
        fixture = self.fixture(primary)
        options = ["--review-every-requests", "100"]
        proxy = Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        a = [{"role": "user", "content": "EXPLICIT_A"}]
        b = a + [{"role": "user", "content": "EXPLICIT_B"}]
        self.assertEqual(proxy.send(a, instructions="A")[0], 200)
        with concurrent.futures.ThreadPoolExecutor() as pool:
            pending = pool.submit(proxy.send, b, instructions="B")
            self.assertTrue(held.wait(2))
            lookup = pool.submit(proxy.send, [], previous_response_id="scope_1", instructions="B")
            try:
                done, _ = concurrent.futures.wait([lookup], timeout=1)
                self.assertIn(lookup, done, "foreign-setting ID must reject before waiting on the held branch")
                status, result = lookup.result()
                self.assertEqual(status, 409, result)
                self.assertEqual(len(fixture.primaries), 2, "reject B ancestry before lock/upstream")
            finally:
                release.set()
            self.assertEqual(pending.result()[0], 503)
        count = (len(fixture.primaries), len(fixture.reviews))
        for selector in ({"previous_response_id": "scope_1"}, {"conversation": "conversation_A"}):
            self.assertEqual(proxy.send([], instructions="B", **selector)[0], 409)
        self.assertEqual((len(fixture.primaries), len(fixture.reviews)), count)
        proxy.close()
        proxy = Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        for selector in ({"previous_response_id": "scope_1"}, {"conversation": "conversation_A"}):
            self.assertEqual(proxy.send([], instructions="B", **selector)[0], 409)
            self.assertEqual(proxy.send([b[-1]], instructions="A", **selector)[0], 200)
        self.assertEqual(proxy.send(b, instructions="B")[0], 200)
        observed_b = f"scope_{len(fixture.primaries)}"
        self.assertEqual(proxy.send([], instructions="B", previous_response_id=observed_b)[0], 200)
        self.assertEqual(proxy.send([], instructions="B", conversation="conversation_A")[0], 409,
                         "a genuine B response cannot inherit an unobserved A conversation")

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
        points = [p for ps in json.loads((self.state / "history.index").read_text())["reviews"].values()
                  for p in ps]
        self.assertEqual(points, [], "failed review cannot publish semantic authority")
        for point in points:
            self.assertEqual(point["memories"], [])
            self.assertEqual(point["drop_members"], {})
            self.assertNotIn("drop", point["opinions"].values())
            self.assertFalse(any(source["removed"] for source in point["sources"]))

    def test_index_byte_node_age_limits_and_evicted_ids_fail_closed(self):
        env = {"CARRY_PROXY_HISTORY_MAX_BYTES": "4096", "CARRY_PROXY_HISTORY_MAX_NODES": "4",
               "CARRY_PROXY_HISTORY_MAX_REFERENCES": "4",
               "CARRY_PROXY_HISTORY_MAX_AGE_SECS": "1"}
        fixture = self.fixture(lambda _b, n: (200, completed(response_id=f"resp_{n}")))
        proxy = TransparentProxy(self.state, fixture, ["--review-every-requests", "100"], env)
        self.stack.callback(proxy.close)
        # 4 KiB cannot hold the real selected session snapshot. Protecting a
        # live owner now backpressures instead of deleting its authority.
        self.assertEqual(proxy.send([{"role": "user", "content": "TOO_SMALL_OWNER"}])[0], 507)
        self.assertEqual(fixture.primaries, [])
        self.assertFalse((self.state / "history.index").exists())
        self.assertEqual(list(self.state.glob("*.json")), [])
        proxy.close()
        env["CARRY_PROXY_HISTORY_MAX_BYTES"] = "16384"
        proxy = TransparentProxy(self.state, fixture, ["--review-every-requests", "100"], env)
        self.stack.callback(proxy.close)
        for n in range(8):
            self.assertEqual(proxy.send([{"role": "user", "content": f"UNIQUE_{n}"}])[0], 200)
            raw = (self.state / "history.index").read_bytes()
            index = json.loads(raw)
            self.assertLessEqual(len(raw), int(env["CARRY_PROXY_HISTORY_MAX_BYTES"]))
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
                "memories": [{"source_ids": ["g2"] if n == 1 else [i for i in ids if i != "g1"][:1],
                              "text": "A_FACT" if n == 1 else "UNCOMMITTED_B_FACT"}]})
        proxy, fixture, history, options = self.seed(review=review)
        proxy.close()
        proxy = TransparentProxy(self.state, fixture, [*options, "--background-reviews=true"])
        self.stack.callback(proxy.close)
        b = [*history, {"role": "assistant", "content": "B_ONLY " + "payload " * 6000},
             {"role": "user", "content": "B_BOUNDARY"}]
        self.assertEqual(proxy.send(b, wait=False)[0], 200)
        # A newly submitted source must first be exposed by a real primary
        # response. A later request can then schedule its retirement review.
        if not held.wait(.1):
            self.assertEqual(proxy.send([*b, {"role": "user", "content": "B_REVIEW_BOUNDARY"}], wait=False)[0], 200)
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

    def test_live_reviewer_survives_each_history_gc_bound_and_restart(self):
        import threading
        original_state = self.state
        for pressure in ("nodes", "references", "bytes", "age"):
            with self.subTest(pressure=pressure):
                self.state = original_state / pressure
                self.state.mkdir()
                held, release = threading.Event(), threading.Event()
                self.stack.callback(release.set)
                def review(body, _n):
                    held.set()
                    assert release.wait(45), "held reviewer release"
                    ids = ledger(body)["eligible_group_ids"]
                    return 200, advice_response({"protected": ["g1"],
                        "removable": [i for i in ids if i != "g1"],
                        "memories": [{"source_ids": ["g2"], "text": "PINNED_FACT"}]})
                fixture = self.fixture(lambda _b, n: (200, completed(response_id=f"pin_{pressure}_{n}")), review)
                env = {"CARRY_PROXY_HISTORY_MAX_NODES": "8"} if pressure == "nodes" else (
                    {"CARRY_PROXY_HISTORY_MAX_REFERENCES": "6"} if pressure == "references" else (
                    {"CARRY_PROXY_HISTORY_MAX_BYTES": "300000"} if pressure == "bytes" else
                    {"CARRY_PROXY_HISTORY_MAX_AGE_SECS": "1"}))
                options = ["--review-replayed-history", "--payoff-requests", "1",
                           "--classifier-cache-policy", "disabled", "--background-reviews=true",
                           "--classifier-timeout-secs", "60"]
                proxy = TransparentProxy(self.state, fixture, options, env)
                self.stack.callback(proxy.close)
                history = [{"role": "user", "content": "PINNED_GOAL"},
                           {"role": "assistant", "content": "PINNED_RAW " + "payload " * 2000},
                           {"role": "user", "content": "PINNED_BOUNDARY"}]
                self.assertEqual(proxy.send(history, wait=False)[0], 200)
                self.assertTrue(held.wait(3), "real background worker must own the branch")
                path, before = proxy.checkpoint(history)
                owner = path.stem
                self.assertEqual(before["primary"]["calls"], 1)
                if pressure == "age":
                    time.sleep(2.05)
                for n in range(12):
                    # A quota may backpressure this unrelated reservation; it
                    # may never satisfy the quota by deleting a live owner.
                    status, _ = proxy.send([{"role": "user", "content":
                        f"UNRELATED_{n} " + ("x" * 40000 if pressure == "bytes" else "x")}], wait=False)
                    self.assertIn(status, (200, 500, 507))
                index = json.loads((self.state / "history.index").read_text())
                try:
                    self.assertIn(owner, index["snapshots"],
                                  "GC must not delete an in-flight review's accounting/removal owner")
                    self.assertTrue(path.exists())
                finally:
                    release.set()
                deadline = time.monotonic() + 5
                while True:
                    index = json.loads((self.state / "history.index").read_text())
                    settled = index["snapshots"].get(owner, {})
                    if not settled.get("background_review_pending", True):
                        break
                    self.assertLess(time.monotonic(), deadline, "held review must durably settle")
                    time.sleep(.01)
                self.assertEqual(settled["primary"]["calls"], 1)
                self.assertEqual(settled["shadow"]["calls"], 1, "paid receipt settles exactly once")
                # Capacity may evict advice, never an in-flight paid owner.
                records = list(index["reviews"].values())
                self.evidence[pressure] = {"settled": settled, "reviews": records}
                proxy.close()
                # The age policy may legitimately expire a settled branch. The
                # durable receipt is checked above; non-age restart must retain it.
                if pressure != "age":
                    proxy = TransparentProxy(self.state, fixture, options, env)
                    self.stack.callback(proxy.close)
                    followup = history + [{"role": "user", "content": "PINNED_CONTINUE"}]
                    self.assertEqual(proxy.send(followup)[0], 200)
                    _, resumed = proxy.checkpoint(followup)
                    self.assertEqual(resumed["primary"]["calls"], 2)
                    self.assertEqual(resumed["shadow"]["calls"], 1)
                    sent = fixture.primaries[-1]["input"]
                    self.assertIn(history[0], sent, "direction survives advice eviction")
                    self.assertEqual(history[1] in sent, not any(
                        m["text"] == "PINNED_FACT" for r in records for m in r["memories"]))
                proxy.close()
        self.state = original_state

    def test_held_primary_keeps_selected_removals_and_receipts_under_gc(self):
        import concurrent.futures
        import threading
        held, release = threading.Event(), threading.Event()
        self.stack.callback(release.set)
        proxy, fixture, history, options = self.seed()
        def primary(_b, n):
            if n == 2:
                held.set()
                assert release.wait(20), "held primary release"
            return 200, completed(response_id=f"primary_pin_{n}")
        fixture.primary = primary
        proxy.close()
        env = {"CARRY_PROXY_HISTORY_MAX_REFERENCES": "6"}
        proxy = TransparentProxy(self.state, fixture, [*options, "--request-timeout-secs", "30"], env)
        self.stack.callback(proxy.close)
        parent = history + [{"role": "user", "content": "HELD_PRIMARY"}]
        with concurrent.futures.ThreadPoolExecutor() as pool:
            pending = pool.submit(proxy.send, parent)
            self.assertTrue(held.wait(2))
            path, before = proxy.checkpoint(parent)
            owner = path.stem
            for n in range(12):
                self.assertIn(proxy.send([{"role": "user", "content": f"PRIMARY_PRESSURE_{n}"}])[0], (200, 500, 507))
            try:
                index = json.loads((self.state / "history.index").read_text())
                self.assertIn(owner, index["snapshots"], "primary transport owns its selected snapshot")
            finally:
                release.set()
            self.assertEqual(pending.result()[0], 200)
        _, after = proxy.checkpoint(parent)
        self.assertEqual(after["primary"]["calls"], 2)
        self.assertEqual(after["shadow"]["calls"], before["shadow"]["calls"])
        self.assertNotIn("RAW_A", json.dumps(fixture.primaries[-1]["input"]))
        proxy.close()
        proxy = TransparentProxy(self.state, fixture, options, env)
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(parent)[0], 200)
        _, resumed = proxy.checkpoint(parent)
        self.assertEqual(resumed["primary"]["calls"], 3)
        sent = fixture.primaries[-1]["input"]
        self.assertIn(history[0], sent)
        self.assertEqual(history[1] in sent, not any(r["drops"] for r in json.loads(
            (self.state / "history.index").read_text())["reviews"].values()),
            "evicted advice cannot confer permanent omission authority")

    def test_bounded_many_sibling_storage_observation_is_not_latency_evidence(self):
        proxy, fixture, history, _ = self.seed(extra=("--history-max-references", "32"))
        self.assertEqual(proxy.send(history + [{"role": "user", "content": "STORAGE_PARENT"}])[0], 200)
        observations = []
        for n in range(20):
            child = history + [{"role": "user", "content": f"STORAGE_CHILD_{n}"}]
            self.assertEqual(proxy.send(child)[0], 200)
            path = self.state / "history.index"
            index = json.loads(path.read_text())
            mirrors = list(self.state.glob("*.json"))
            observation = {"index_bytes": path.stat().st_size,
                "diagnostic_mirror_bytes": sum(p.stat().st_size for p in mirrors),
                "diagnostic_mirror_count": len(mirrors), "node_count": len(index["nodes"]),
                "reference_count": len(index["tips"]) + len(index["reviews"]) + len(index["transcripts"]),
                "snapshot_count": len(index["snapshots"]),
                "shared_payload_copies": sum(node["value"] == history[1] for node in index["nodes"].values())}
            observations.append(observation)
            self.assertLessEqual(observation["reference_count"], 32)
            self.assertLessEqual(observation["index_bytes"], 33554432)
            self.assertEqual(observation["shared_payload_copies"], 1)
        summary = {"case": "20_siblings_shared_42k_prefix_reference_pressure", "samples": len(observations),
            "last": observations[-1],
            "maximum_observed_index_plus_mirrors_bytes": max(o["index_bytes"] + o["diagnostic_mirror_bytes"] for o in observations),
            "temporary_peak_bytes_measured": False, "lookup_serialization_counts_measured": False,
            "latency_claim": False}
        self.evidence.update(storage=summary)
        import sys
        print("TRANSPARENT_HISTORY_STORAGE=" + json.dumps(summary, sort_keys=True), file=sys.stderr, flush=True)

    def test_malformed_combined_and_unknown_selectors_never_dispatch(self):
        fixture = self.fixture(lambda _b, n: (200, completed(response_id=f"selector_{n}")))
        proxy = self.proxy(fixture, ["--review-every-requests", "100"])
        self.assertEqual(proxy.send([{"role": "user", "content": "SELECTOR_SEED"}])[0], 200)
        counts = (len(fixture.primaries), len(fixture.reviews))
        malformed = [{"previous_response_id": value} for value in (0, [], {}, "")]
        malformed += [{"conversation": value} for value in (0, [], {}, {"id": 0}, "")]
        malformed += [{"previous_response_id": "selector_1", "conversation": "same"}]
        for selector in malformed:
            with self.subTest(selector=selector):
                self.assertEqual(proxy.send([], **selector)[0], 400)
                self.assertEqual((len(fixture.primaries), len(fixture.reviews)), counts)
        for selector in ({"previous_response_id": "unknown"}, {"conversation": "unknown"}):
            self.assertEqual(proxy.send([], **selector)[0], 409)
        self.assertEqual((len(fixture.primaries), len(fixture.reviews)), counts)

    def test_cold_pair_freezes_current_capacity_without_reviewer_recovery(self):
        proxy, fixture, history, options = self.seed()
        self.assertEqual(proxy.send(history + [{"role": "user", "content": "PAIR_PARENT"}])[0], 200)
        proxy.close()
        options += ["--primary-context-tokens", "60000", "--classifier-max-input-tokens", "1024"]
        proxy = TransparentProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        child = history + [{"role": "user", "content": "COLD_PAIR_CHILD"}]
        reviews = len(fixture.reviews)
        self.assertEqual(proxy.send(child)[0], 200)
        _, saved = proxy.checkpoint(child)
        pair = saved["model_resolution"]
        for role, capacity in (("primary", 60000), ("reviewer", 1024)):
            self.assertEqual(pair[role]["capacity_tokens"], capacity)
            self.assertEqual(pair[role]["provenance"]["capacity"]["operator_override"], capacity)
            self.assertNotEqual(pair[role]["provenance"]["capacity"]["status"], "legacy_snapshot_missing")
        self.assertEqual(saved.get("model_resolution_history", []), [])
        self.assertEqual(saved["primary"]["calls"], 1)
        self.assertEqual(saved["shadow"]["calls"], 0)
        self.assertTrue(saved["last_plan"]["primary_fits"])
        self.assertFalse(saved["last_plan"]["reviewer_fits"])
        self.assertFalse(saved["last_plan"]["forced"])
        self.assertEqual(len(fixture.reviews), reviews)
        sent = fixture.primaries[-1]["input"]
        self.assertNotIn("RAW_A", json.dumps(sent))
        self.assertIn("A_FACT", json.dumps(sent))
        proxy.close()
        proxy = TransparentProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(child)[0], 200)
        _, restored = proxy.checkpoint(child)
        self.assertEqual(restored["model_resolution"], pair)
        self.assertEqual(restored["primary"]["calls"], 2)
        self.assertEqual(restored["shadow"]["calls"], 0)
        self.assertEqual(len(fixture.reviews), reviews)
        self.assertEqual(fixture.primaries[-1]["input"], sent)

    def test_real_primary_overflow_never_dispatches_recovery(self):
        proxy, fixture, history, options = self.seed()
        self.assertEqual(proxy.send(history + [{"role": "user", "content": "CAPACITY_PARENT"}])[0], 200)
        proxy.close()
        options += ["--primary-context-tokens", "1024", "--primary-output-reserve-tokens", "1",
                    "--context-margin-percent", "0"]
        proxy = TransparentProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(history + [{"role": "user", "content": "FITTING_CHILD"}])[0], 200)
        self.assertNotIn("RAW_A", json.dumps(fixture.primaries[-1]["input"]))
        counts = (len(fixture.primaries), len(fixture.reviews))
        self.assertEqual(proxy.send(history + [{"role": "user", "content": "PROTECTED_SUFFIX " * 10000}])[0], 413)
        self.assertEqual((len(fixture.primaries), len(fixture.reviews)), counts)
        fresh = [{"role": "user", "content": "FRESH_NO_ADVICE"}]
        self.assertEqual(proxy.send(fresh)[0], 200)
        self.assertEqual(fixture.primaries[-1]["input"], fresh)

    def test_cold_child_has_no_parent_receipts_before_held_primary_completes(self):
        import concurrent.futures
        import threading
        proxy, fixture, history, _ = self.seed()
        self.assertEqual(proxy.send(history + [{"role": "user", "content": "RECEIPT_PARENT"}])[0], 200)
        held, release = threading.Event(), threading.Event()
        self.stack.callback(release.set)
        def primary(_body, n):
            held.set()
            assert release.wait(10), "cold child primary release"
            return 200, completed(response_id=f"cold_receipt_{n}")
        fixture.primary = primary
        child = history + [{"role": "user", "content": "HELD_COLD_CHILD"}]
        with concurrent.futures.ThreadPoolExecutor() as pool:
            pending = pool.submit(proxy.send, child)
            try:
                self.assertTrue(held.wait(2))
                _, saved = proxy.checkpoint(child)
                self.assertEqual(saved["primary"]["calls"], 0)
                self.assertEqual(saved["shadow"]["calls"], 0)
                self.assertEqual(saved["completed_requests"], 0)
                self.assertFalse(saved["background_review_pending"])
                self.assertEqual(saved["review_cache_key"], "", "cold child has no reviewer affinity yet")
            finally:
                release.set()
            self.assertEqual(pending.result()[0], 200)


    def test_extended_tool_cohort_cannot_inherit_partial_review_authority(self):
        history = [{"role": "user", "content": "PROTECTED_GOAL"},
            {"role": "assistant", "content": "RAW_A " + "payload " * 6000},
            {"type": "function_call", "call_id": "a1", "name": "one", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "a1", "output": "OLD_COHORT_RESULT"},
            {"role": "user", "content": "A_BOUNDARY"}]
        proxy, fixture, history, options = self.seed(history=history)
        self.assertEqual(proxy.send(history + [{"role": "user", "content": "COHORT_PARENT"}])[0], 200)
        proxy.close()
        options.remove("--review-replayed-history")
        options += ["--review-every-requests", "100"]
        proxy = TransparentProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        grown = history + [{"type": "function_call_output", "call_id": "a1", "output": "NEW_COHORT_RESULT"},
                           {"role": "user", "content": "GROWN_COHORT_CONTEXT"}]
        reviews = len(fixture.reviews)
        self.assertEqual(proxy.send(grown)[0], 200)
        sent = fixture.primaries[-1]["input"]
        self.assertIn(history[1], sent)
        self.assertIn(history[2], sent)
        self.assertIn(history[3], sent)
        self.assertNotIn("A_FACT", json.dumps(sent))
        self.assertEqual(len(fixture.reviews), reviews)

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
