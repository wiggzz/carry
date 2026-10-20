"""Incremental reviewer behavior; primary responses precede reviewer settlement."""
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

    def fixture(self, review=None):
        fixture = self.harness.fixture(lambda _b, n: (200, completed(response_id=f"r{n}")),
            review or (lambda _b, _n: (200, advice_response(
                {"protected": [], "removable": [], "memories": []}))))
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        return fixture

    def test_implicit_reviewer_appends_prior_output_before_new_sources(self):
        def review(body, n):
            return 200, advice_response({"protected": ledger(body)["eligible_group_ids"],
                                         "removable": [], "memories": []})
        fixture = self.fixture(review)
        proxy = self.harness.proxy(fixture, ["--classifier-cache-policy", "disabled",
                                           "--min-payback-percent", "100"])
        history = [{"role": "user", "content": "durable goal"}]
        for n in range(3):
            self.assertEqual(proxy.send(history)[0], 200)
            history += [{"role": "assistant", "content": f"new evidence {n}"},
                        {"role": "user", "content": f"next step {n}"}]
        self.assertEqual(len(fixture.reviews), 2)
        first, second = fixture.reviews
        self.assertEqual(second["input"][:len(first["input"])], first["input"])
        self.assertEqual(second["input"][len(first["input"])]["role"], "assistant")

    def test_bootstrap_keeps_protected_sources_without_primary_capacity_rewrites(self):
        fixture = self.import_fixture()
        proxy = self.harness.proxy(fixture, ["--review-replayed-history",
            "--classifier-max-input-tokens", "6000", "--min-payback-percent", "100"])
        history = self.large_history()
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertGreater(len(fixture.reviews), 0)
        self.assertTrue(all("DURABLE_IMPORT_REQUIREMENT" in json.dumps(b["input"])
                            for b in fixture.reviews))
        self.assertEqual(proxy.saved()["compactions"], 0,
                         "reviewer import capacity cannot authorize primary retirement")
        self.assertEqual(fixture.primaries[0]["input"], history)
        self.assertTrue(all(not i["removed"] for i in proxy.saved()["history"]))

    def test_primary_capacity_compacts_shared_memory_and_discards_old_review_turns(self):
        def review(body, n):
            ids = ledger(body)["eligible_group_ids"]
            return 200, advice_response({"protected": [i for i in ids if i != "g2"],
                "removable": [i for i in ids if i == "g2"],
                "memories": [{"source_ids":["g2"],"text":"Preserved useful finding"}] if n == 1 else []})
        fixture = self.fixture(review)
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
        self.assertFalse(any(item.get("role")=="assistant" for item in reviewer))

    def test_oversized_protected_request_never_reaches_primary(self):
        fixture=self.fixture()
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
        return self.fixture(review)

    def test_import_call_cap_resumes_across_restart_after_primary_progress(self):
        fixture=self.import_fixture()
        options=["--review-replayed-history","--classifier-max-input-tokens","6000","--review-max-calls","1"]
        proxy=self.harness.proxy(fixture,options)
        history=self.large_history()
        self.assertEqual(proxy.send(history)[0],200)
        saved=proxy.saved()
        self.assertTrue(saved["bootstrap_reviewed_ids"])
        self.assertEqual(fixture.primaries[0]["input"],history)
        spent=saved["bootstrap_estimated_cost_usd"]
        covered=set(saved["bootstrap_reviewed_ids"])
        proxy.close(); proxy=self.harness.proxy(fixture,options)
        for _ in range(3):
            before=len(fixture.reviews)
            self.assertEqual(proxy.send(history)[0],200)
            self.assertLessEqual(len(fixture.reviews)-before,1)
            self.assertGreaterEqual(proxy.saved()["bootstrap_estimated_cost_usd"],spent)
            self.assertTrue(covered.issubset(proxy.saved()["bootstrap_reviewed_ids"]))
        self.assertEqual(len(fixture.primaries),4)
        self.assertIn(history[0],fixture.primaries[-1]["input"])
        self.assertIn(history[-1],fixture.primaries[-1]["input"])

    def test_import_spend_cap_is_not_replenished_by_restart(self):
        fixture=self.import_fixture()
        options=["--review-replayed-history","--classifier-max-input-tokens","6000",
                 "--bootstrap-max-cost-usd","0.0017","--min-payback-percent","100"]
        proxy=self.harness.proxy(fixture,options)
        history=self.large_history()
        self.assertEqual(proxy.send(history)[0],200)
        calls=len(fixture.reviews); self.assertGreater(calls,0)
        spent=proxy.saved()["bootstrap_estimated_cost_usd"]
        self.assertGreater(spent,0)
        proxy.close(); proxy=self.harness.proxy(fixture,options)
        self.assertEqual(proxy.send(history)[0],200)
        self.assertEqual(len(fixture.reviews),calls)
        self.assertAlmostEqual(proxy.saved()["bootstrap_estimated_cost_usd"],spent,places=12)
        self.assertEqual([p["input"] for p in fixture.primaries],[history,history])

    def test_real_primary_overflow_rejects_without_waiting_for_background_rescue(self):
        def review(body,n):
            fixture.held.set()
            if not fixture.release.wait(10):raise AssertionError("review was not released")
            return 200,advice_response({"protected":["g1"],"removable":["g2"],"memories":[]})
        fixture=self.fixture(review)
        proxy=self.harness.proxy(fixture,["--background-reviews","--primary-context-tokens","7000",
            "--primary-output-reserve-tokens","64","--min-payback-percent","100"])
        history=[{"role":"user","content":"durable goal"},
            {"role":"assistant","content":"OLD_SOURCE_OUTPUT " * 1000},
            {"role":"user","content":"current task"}]
        self.assertEqual(proxy.send(history)[0],200)
        self.assertEqual(proxy.send(history)[0],200)
        self.assertTrue(fixture.held.wait(2))
        history += [{"role":"user","content":"new incoming material " * 1500}]
        try:
            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                self.assertEqual(pool.submit(proxy.send,history).result(timeout=3)[0],413)
            self.assertEqual(len(fixture.primaries),2)
            self.assertFalse(fixture.release.is_set())
        finally:fixture.release.set()
        self.harness.wait_saved(proxy,lambda s:not s["background_review_pending"])
        self.assertEqual(proxy.send(history)[0],200)
        self.assertNotIn("OLD_SOURCE_OUTPUT",json.dumps(fixture.primaries[-1]["input"]))

    def test_import_invalid_advice_preserves_progress_across_restart(self):
        def review(body,n):
            if n==2:return 200,advice_response({"invalid":True})
            ids=ledger(body)["eligible_group_ids"]
            return 200,advice_response({"protected":[i for i in ids if i=="g1"],
                "removable":[i for i in ids if i!="g1"],"memories":[]})
        fixture=self.fixture(review)
        options=["--review-replayed-history","--classifier-max-input-tokens","6000"]
        proxy=self.harness.proxy(fixture,options)
        history=self.large_history()
        self.assertEqual(proxy.send(history)[0],200)
        saved=proxy.saved()
        self.assertTrue(saved["bootstrap_reviewed_ids"])
        self.assertEqual(saved["invalid_reviews"],1)
        self.assertEqual(fixture.primaries[0]["input"],history)
        proxy.close();proxy=self.harness.proxy(fixture,options)
        self.assertEqual(proxy.send(history)[0],200)
        self.assertEqual(proxy.saved()["invalid_reviews"],1)
        self.assertEqual(len(fixture.primaries),2)
        self.assertIn("DURABLE_IMPORT_REQUIREMENT",json.dumps(fixture.primaries[-1]["input"]))


if __name__ == "__main__":
    unittest.main()
