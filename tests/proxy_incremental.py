"""Incremental shared-history reviewer behavior against the real proxy binary."""
import json
import concurrent.futures
import unittest
import proxy_state_regressions as support
from proxy_state_regressions import advice_response, completed, ledger


class IncrementalReviewer(unittest.TestCase):
    def setUp(self):
        self.harness = support.StateRegressions()
        self.harness.setUp()
        self.addCleanup(self.harness.doCleanups)

    def test_implicit_reviewer_appends_prior_output_before_new_sources(self):
        def review(body, n):
            return 200, advice_response({"protected": ledger(body)["eligible_group_ids"],
                                         "removable": [], "memories": []})
        fixture = self.harness.fixture(lambda _b, n: (200, completed(response_id=f"r{n}")), review)
        proxy = self.harness.proxy(fixture, ["--classifier-cache-policy", "disabled",
                                           "--min-payback-percent", "100"])
        history = [{"role": "user", "content": "durable goal"}]
        for n in range(3):
            self.assertEqual(proxy.send(history)[0], 200)
            history += [{"role": "assistant", "content": f"new evidence {n}"},
                        {"role": "user", "content": f"next step {n}"}]
        self.assertEqual(len(fixture.reviews), 2)
        first, second = fixture.reviews
        self.assertEqual(second["input"][:len(first["input"])], first["input"],
                         "without markers the previous nudge must remain in the exact prefix")
        self.assertEqual(second["input"][len(first["input"])]["role"], "assistant",
                         "replay the reviewer's previous response before newly appended sources")

    def test_bootstrap_keeps_protected_sources_across_chunks(self):
        def review(body, n):
            ids = ledger(body)["eligible_group_ids"]
            return 200, advice_response({"protected": [i for i in ids if i == "g1"],
                                         "removable": [i for i in ids if i != "g1"], "memories": []})
        fixture = self.harness.fixture(lambda _b, _n: (200, completed()), review)
        proxy = self.harness.proxy(fixture, ["--review-replayed-history",
            "--classifier-max-input-tokens", "6000", "--min-payback-percent", "100"])
        history = [{"role": "user", "content": "DURABLE_IMPORT_REQUIREMENT"}]
        for n in range(18):
            history += [{"role": "assistant", "content": (f"old output {n} " * 300)},
                        {"role": "user", "content": f"old task {n}"}]
        history += [{"role": "user", "content": "current task"}]
        result = proxy.send(history)
        self.assertEqual(result[0], 200, {
            "response": result[1], "review_count": len(fixture.reviews),
            "review_wire_bytes": [len(support.wire(b)) for b in fixture.reviews],
            "eligible_ids": [ledger(b)["eligible_group_ids"] for b in fixture.reviews],
            "feedback": proxy.saved().get("review_feedback"),
        })
        self.assertGreater(len(fixture.reviews), 1)
        self.assertTrue(all("DURABLE_IMPORT_REQUIREMENT" in json.dumps(b["input"])
                            for b in fixture.reviews), "protected source survives every admission chunk")
        self.assertGreater(proxy.saved()["compactions"], 0,
                           "bootstrap makes actual room in both retained histories")
        self.assertLess(len(fixture.primaries[0]["input"]), len(history))

    def test_primary_capacity_compacts_shared_memory_and_discards_old_review_turns(self):
        def review(body, n):
            ids = ledger(body)["eligible_group_ids"]
            return 200, advice_response({"protected": [i for i in ids if i != "g2"],
                "removable": [i for i in ids if i == "g2"],
                "memories": [{"source_ids":["g2"],"text":"Preserved useful finding"}] if n == 1 else []})
        fixture = self.harness.fixture(lambda _b, n: (200, completed(response_id=f"r{n}")), review)
        proxy = self.harness.proxy(fixture, ["--primary-context-tokens","7000",
            "--primary-output-reserve-tokens","64","--min-payback-percent","100"])
        history = [{"role":"user","content":"durable goal"},
            {"role":"assistant","content":"OLD_SOURCE_OUTPUT " * 1000},
            {"role":"user","content":"current task"}]
        self.assertEqual(proxy.send(history)[0],200)
        self.assertEqual(proxy.send(history)[0],200)
        history += [{"role":"user","content":"new incoming material " * 1500}]
        self.assertEqual(proxy.send(history)[0],200)
        sent=json.dumps(fixture.primaries[-1]["input"])
        self.assertNotIn("OLD_SOURCE_OUTPUT",sent)
        self.assertIn("Preserved useful finding",sent)
        self.assertTrue(proxy.saved()["last_plan"]["forced"])
        self.assertEqual(proxy.send(history)[0],200)
        reviewer=fixture.reviews[-1]["input"]
        self.assertNotIn("OLD_SOURCE_OUTPUT",json.dumps(reviewer))
        self.assertIn("Preserved useful finding",json.dumps(reviewer))
        self.assertFalse(any(item.get("role")=="assistant" for item in reviewer),
                         "old review answers are disposable when both histories compact")

    def test_oversized_protected_request_never_reaches_primary(self):
        fixture=self.harness.fixture(lambda _b,_n:(200,completed()))
        proxy=self.harness.proxy(fixture,["--primary-context-tokens","3000",
            "--primary-output-reserve-tokens","64"])
        status,_=proxy.send([{"role":"user","content":"protected new request " * 3000}])
        self.assertEqual(status,413)
        self.assertEqual(fixture.primaries,[])


    @staticmethod
    def large_history():
        history=[{"role":"user","content":"DURABLE_IMPORT_REQUIREMENT"}]
        for n in range(18):
            history += [{"role":"assistant","content":f"old output {n} " * 300},
                        {"role":"user","content":f"old task {n}"}]
        return history+[{"role":"user","content":"current task"}]

    def import_fixture(self):
        def review(body,n):
            ids=ledger(body)["eligible_group_ids"]
            return 200,advice_response({"protected":[i for i in ids if i=="g1"],
                "removable":[i for i in ids if i!="g1"],"memories":[]})
        return self.harness.fixture(lambda _b,_n:(200,completed()),review)

    def test_import_call_cap_resumes_across_restart_before_any_primary(self):
        fixture=self.import_fixture()
        options=["--review-replayed-history","--classifier-max-input-tokens","6000","--review-max-calls","1"]
        proxy=self.harness.proxy(fixture,options)
        history=self.large_history()
        self.assertEqual(proxy.send(history)[0],413)
        saved=proxy.saved()
        self.assertTrue(saved["bootstrap_reviewed_ids"])
        self.assertEqual(fixture.primaries,[])
        spent=saved["bootstrap_estimated_cost_usd"]
        proxy.close()
        proxy=self.harness.proxy(fixture,options)
        for _ in range(30):
            before=len(fixture.reviews)
            status,_=proxy.send(history)
            self.assertLessEqual(len(fixture.reviews)-before,1)
            self.assertGreaterEqual(proxy.saved()["bootstrap_estimated_cost_usd"],spent)
            if status==200:break
            self.assertEqual(status,413)
            self.assertEqual(fixture.primaries,[])
        else:self.fail("import did not resume to completion")
        self.assertEqual(len(fixture.primaries),1)

    def test_import_spend_cap_is_not_replenished_by_restart(self):
        fixture=self.import_fixture()
        options=["--review-replayed-history","--classifier-max-input-tokens","6000",
                 "--bootstrap-max-cost-usd","0.0017"]
        proxy=self.harness.proxy(fixture,options)
        history=self.large_history()
        self.assertEqual(proxy.send(history)[0],413)
        calls=len(fixture.reviews); self.assertGreater(calls,0)
        spent=proxy.saved()["bootstrap_estimated_cost_usd"]
        self.assertGreater(spent,0)
        proxy.close(); proxy=self.harness.proxy(fixture,options)
        self.assertEqual(proxy.send(history)[0],413)
        self.assertEqual(len(fixture.reviews),calls)
        self.assertAlmostEqual(proxy.saved()["bootstrap_estimated_cost_usd"],spent,places=12)
        self.assertEqual(fixture.primaries,[])

    def test_capacity_waits_for_background_advice_without_holding_merge_lock(self):
        def review(body,n):
            fixture.held.set()
            if not fixture.release.wait(4):raise AssertionError("review was not released")
            return 200,advice_response({"protected":["g1"],"removable":["g2"],"memories":[]})
        fixture=self.harness.fixture(lambda _b,_n:(200,completed()),review)
        proxy=self.harness.proxy(fixture,["--background-reviews","--primary-context-tokens","7000",
            "--primary-output-reserve-tokens","64","--min-payback-percent","100"])
        history=[{"role":"user","content":"durable goal"},
            {"role":"assistant","content":"OLD_SOURCE_OUTPUT " * 1000},
            {"role":"user","content":"current task"}]
        self.assertEqual(proxy.send(history)[0],200)
        self.assertEqual(proxy.send(history)[0],200)
        self.assertTrue(fixture.held.wait(2))
        history += [{"role":"user","content":"new incoming material " * 1500}]
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
            pending=pool.submit(proxy.send,history)
            try:
                with self.assertRaises(concurrent.futures.TimeoutError):pending.result(timeout=0.2)
                self.assertEqual(len(fixture.primaries),2)
            finally:fixture.release.set()
            self.assertEqual(pending.result(timeout=4)[0],200)
        self.assertNotIn("OLD_SOURCE_OUTPUT",json.dumps(fixture.primaries[-1]["input"]))


    def test_import_invalid_advice_preserves_progress_across_restart(self):
        def review(body,n):
            if n==2:return 200,advice_response({"invalid":True})
            ids=ledger(body)["eligible_group_ids"]
            return 200,advice_response({"protected":[i for i in ids if i=="g1"],
                "removable":[i for i in ids if i!="g1"],"memories":[]})
        fixture=self.harness.fixture(lambda _b,_n:(200,completed()),review)
        options=["--review-replayed-history","--classifier-max-input-tokens","6000"]
        proxy=self.harness.proxy(fixture,options)
        history=self.large_history()
        self.assertEqual(proxy.send(history)[0],413)
        saved=proxy.saved()
        self.assertTrue(saved["bootstrap_reviewed_ids"])
        self.assertEqual(saved["invalid_reviews"],1)
        self.assertEqual(fixture.primaries,[])
        proxy.close();proxy=self.harness.proxy(fixture,options)
        self.assertEqual(proxy.send(history)[0],200)
        self.assertEqual(proxy.saved()["invalid_reviews"],1)
        self.assertEqual(len(fixture.primaries),1)
        self.assertIn("DURABLE_IMPORT_REQUIREMENT",json.dumps(fixture.primaries[0]["input"]))



if __name__ == "__main__":
    unittest.main()
