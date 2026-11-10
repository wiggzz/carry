"""PR184 review regressions against the actual CLI; synthetic loopback only."""
import json
import time
import unittest
from proxy_state_regressions import Proxy, advice_response, completed, wire
import proxy_transparent_history as history_fixture
import proxy_traditional_estimation as traditional_fixture
from proxy_transparent_history import TransparentProxy


class NoodleRegressions(unittest.TestCase):
    setUp = history_fixture.TransparentHistory.setUp
    record_evidence = history_fixture.TransparentHistory.record_evidence
    fixture = history_fixture.TransparentHistory.fixture
    proxy = history_fixture.TransparentHistory.proxy

    def history(self):
        return [{"role": "user", "content": "NOODLE_GOAL"},
                {"role": "assistant", "content": "NOODLE_SOURCE " + "payload " * 3000},
                {"role": "user", "content": "NOODLE_TASK"}]

    def events(self):
        return [json.loads(line) for p in (self.state / "diagnostics").glob("*.jsonl")
                for line in p.read_text().splitlines()]

    def attest_cache(self):
        # Only synthetic route capability; never manufacture a native receipt.
        path = self.state / "history.index"
        index = json.loads(path.read_text())
        for snapshot in index["snapshots"].values():
            snapshot["model_resolution"]["primary"]["cache"] = {
                "minimum_prefix_tokens": 1024, "max_read_breakpoints": 50,
                "max_write_breakpoints": 4, "implicit_breakpoint_uses_write_slot": True}
        path.write_bytes(wire(index))

    def snapshot(self, proxy, saved=None):
        # Join immutable request facts to their real HTTP projection, never to a
        # fixture-reimplemented estimator or a persisted prediction.
        saved = proxy.saved() if saved is None else saved
        status, stats = proxy.request("GET", "/carry/dashboard/stats")
        self.assertEqual(status, 200)
        self.assertEqual(len(stats["sessions"]), 1)
        result = stats["sessions"][0]["cost_estimate"]
        self.assertAlmostEqual(result["observed_primary_cost"], saved["primary"]["cost_usd"], places=12)
        if saved["shadow"]["calls"]:
            self.assertAlmostEqual(result["observed_reviewer_cost"], saved["shadow"]["cost_usd"], places=12)
        else:
            self.assertIsNone(result["observed_reviewer_cost"])
            self.assertEqual(saved["shadow"]["cost_usd"], 0)
        return {**saved, "estimate": result}

    def primary_observation(self, saved):
        return next(o["Primary"] for o in reversed(saved["request_evidence"]["observations"])
                    if "Primary" in o)

    # Keep threshold behavior independent of fitting cap continuity. These are
    # real registered HTTP seed/growth/restart controls, with declared native
    # quantities and marginal-rate oracles, not the obsolete future-only poke.
    test_paid_summary_threshold_after_restart = (
        traditional_fixture.TraditionalEstimation.test_opaque_continuous_summary_progresses_after_http_restart)
    test_free_initialization_once_after_restart = (
        traditional_fixture.TraditionalEstimation.test_unknown_retained_output_keeps_known_free_initialization_once)

    def test_changed_cap_preserves_current_projection_and_summary_threshold_after_restart(self):
        root = self.state
        controls = {}
        for label, cap in (("unchanged", 1), ("changed", 2)):
            with self.subTest(cap=label):
                self.state = root / label
                self.state.mkdir()
                fixture = self.fixture(lambda b,n: (200, completed(response_id=f"cap_{n}",
                    input_tokens=6000 if "NOODLE_SOURCE" in json.dumps(b) else 100)),
                    lambda _b,_n: (200, advice_response({"protected":[], "removable":[2], "memories":[]})))
                h = self.history()
                options = ["--payoff-requests","1","--classifier-cache-policy","disabled"]
                proxy = self.proxy(fixture, [*options,"--review-replayed-history"])
                self.assertEqual(proxy.send(h, max_output_tokens=1)[0], 200)
                proxy.close()
                options += ["--review-every-requests","100"]
                proxy = self.proxy(fixture, options)
                self.assertEqual(proxy.send(h, max_output_tokens=1)[0], 200)
                before = self.snapshot(proxy, proxy.checkpoint(h)[1])
                prior = self.primary_observation(before)
                restored = prior["restoration"]["tokens"]
                self.assertGreater(restored, 0)
                self.assertNotIn(h[1], fixture.primaries[-1]["input"])
                proxy.close()
                # This seam replays the ENTIRE immutable request history. Its
                # first 6000-token input exceeds the declared boundary, so one
                # pre-window initialization is due in BOTH arms before the cap
                # decision. A fitting allowance is not a new HistoryContract.
                path = self.state / "history.index"
                index = json.loads(path.read_text())
                for state in index["snapshots"].values():
                    state["request_evidence"]["scenario"] = {
                        "context_window": 200 + restored // 2, "reserve":100,
                        "keep_recent":100, "summary":50}
                path.write_bytes(wire(index))
                proxy = self.proxy(fixture, options)
                baseline = self.snapshot(proxy, proxy.checkpoint(h)[1])
                self.assertEqual(baseline["request_evidence"]["observations"],
                                 before["request_evidence"]["observations"])
                self.assertEqual(baseline["primary"], before["primary"])
                self.assertEqual(baseline["shadow"], before["shadow"])
                self.assertEqual(baseline["estimate"]["initial_compactions"], 1)
                self.assertEqual(baseline["estimate"]["matched_primary_responses"], 2)
                self.assertEqual(proxy.send(h, max_output_tokens=cap)[0], 200)
                after = self.snapshot(proxy, proxy.checkpoint(h)[1])
                self.evidence[label] = {"before":before, "baseline":baseline, "after":after,
                                        "primaries":fixture.primaries, "reviews":fixture.reviews}
                self.assertNotIn(h[1], fixture.primaries[-1]["input"])
                self.assertEqual(len(fixture.primaries), 3)
                self.assertEqual(len(fixture.reviews), 1)
                self.assertEqual(after["primary"]["calls"], 3)
                self.assertEqual(after["primary"]["input_tokens"], 6200)
                self.assertEqual(after["primary"]["output_tokens"], 3)
                self.assertEqual(after["shadow"], before["shadow"])
                current = self.primary_observation(after)
                self.assertEqual(current["restoration"], prior["restoration"])
                self.assertEqual(current["facts"]["contract"], prior["facts"]["contract"])
                self.assertEqual(prior["facts"]["allowance"], 1)
                self.assertEqual(current["facts"]["allowance"], cap)
                self.assertEqual(current["facts"]["configuration"] == prior["facts"]["configuration"], cap == 1)
                original = baseline["request_evidence"]["observations"]
                observations = after["request_evidence"]["observations"]
                self.assertEqual(observations[:len(original)], original)
                appended = observations[len(original):]
                self.assertEqual(len(appended), 2, 'one dispatch followed by one successful terminal')
                self.assertEqual(set(appended[0]), {'Dispatch'})
                dispatch = appended[0]['Dispatch']
                self.assertTrue(dispatch['request_id'])
                self.assertEqual(dispatch['selection'], dispatch['continuation'])
                self.assertEqual(dispatch['omitted_members'], 1)
                self.assertEqual(dispatch['newly_omitted_members'], 0,
                                 'repeating the authorized projection is not a new omission')
                self.assertIs(dispatch['forced'], False)
                self.assertEqual(dispatch['input_reduction_estimate'], 0)
                self.assertEqual(appended[1], {'Primary': current})
                result, staged = after["estimate"], baseline["estimate"]
                self.assertEqual(result["matched_primary_responses"], 3)
                self.assertEqual(result["coverage"]["unmodeled_calls"], 0)
                self.assertEqual(result["excluded_unpriced_calls"], 0)
                self.assertEqual(result["initial_compactions"], 1, "no duplicate free initialization")
                self.assertEqual(result["traditional_compactions"], 0)
                self.assertEqual(result["summary_cost"], 0)
                rates = after["model_resolution"]["primary"]["rates"]
                self.assertAlmostEqual(result["observed_primary_cost"] - staged["observed_primary_cost"],
                                       (100 * rates["input"] + rates["output"]) / 1e6, places=12)
                controls[label] = {key: result[key] - staged[key] for key in
                                   ("traditional_cost", "observed_primary_cost", "initial_compactions",
                                    "traditional_compactions", "summary_cost", "matched_primary_responses")}
                self.assertEqual(controls[label]["matched_primary_responses"], 1)
                proxy.close()
        self.assertEqual(controls["changed"], controls["unchanged"],
                         "fitting output allowance must preserve the same continuous history")
        self.state = root

    def test_off_tracking_fallback_has_zero_projection_after_repeat_and_restart(self):
        root = self.state
        for label, count in (("tracked", 100), ("tracker_limit", 16385)):
            with self.subTest(case=label):
                self.state = root / label
                self.state.mkdir()
                def primary(_body, n):
                    response = completed(response_id=f"off_{label}_{n}")
                    response["usage"]["input_tokens"] = 200000
                    return 200, response
                fixture = self.fixture(primary)
                h = [{"role":"user", "content":"x"} for _ in range(count)]
                def start():
                    proxy = Proxy(self.state, fixture, mode="off")
                    self.stack.callback(proxy.close)
                    return proxy
                proxy = start()
                snapshots = []
                for _ in range(2):
                    self.assertEqual(proxy.send(h, max_output_tokens=1)[0], 200)
                    self.assertEqual(fixture.primaries[-1]["input"], h)
                    snapshots.append(self.snapshot(proxy))
                proxy.close()
                proxy = start()
                self.assertEqual(proxy.send(h, max_output_tokens=1)[0], 200)
                self.assertEqual(fixture.primaries[-1]["input"], h)
                snapshots.append(self.snapshot(proxy))
                proxy.close()
                self.evidence[label] = {"snapshots":snapshots, "primaries":fixture.primaries,
                                        "reviews":fixture.reviews}
                self.assertFalse(fixture.reviews)
                for n, saved in enumerate(snapshots, 1):
                    result = saved["estimate"]
                    self.assertEqual(len(saved["history"]), count if label == "tracked" else 0)
                    self.assertEqual(saved["primary"]["calls"], n)
                    self.assertEqual(saved["primary"]["input_tokens"], 200000*n)
                    self.assertEqual(saved["primary"]["output_tokens"], n)
                    self.assertEqual(saved["shadow"]["calls"], 0)
                    self.assertEqual(result["matched_primary_responses"], n)
                    self.assertEqual(result["coverage"]["unmodeled_calls"], 0)
                    self.assertEqual(result["excluded_unpriced_calls"], 0)
                    self.assertAlmostEqual(result["observed_primary_cost"],
                                           saved["primary"]["cost_usd"], places=12)
                # Grade after collecting every request so RED retains repeat,
                # restart, complete forwarded input and paid receipt evidence.
                for label_request, saved in zip(("first", "repeat", "restart"), snapshots):
                    with self.subTest(case=label, request=label_request):
                        current = self.primary_observation(saved)
                        self.assertEqual(current["restoration"]["tokens"], 0,
                                         "pass-through does not edit input when tracking fails")
                        self.assertEqual(current["facts"]["native"]["input_tokens"], 200000)
                        # The unresolved off route keeps identical tracked
                        # execution on its native paid contribution. Tracker
                        # failure lacks ancestry: one ideal inherited prefix,
                        # then independent cold writes. Declare the fixture's
                        # gpt-6-luna API-equivalent rates, not observed prices.
                        n = ("first", "repeat", "restart").index(label_request) + 1
                        self.assertIsNone(current["facts"]["resolved"])
                        self.assertAlmostEqual(saved["primary"]["cost_usd"],
                                               n * (200000 * 0.1 + 0.5) / 1e6, places=12)
                        expected = (n * (200000 * 0.1 + 0.5) if label == "tracked" else
                                    200000 * (0.01 + (n-1) * 0.125) + n * 0.5) / 1e6
                        self.assertAlmostEqual(saved["estimate"]["traditional_cost"], expected, places=12)
                        if n > 1:
                            old = snapshots[n-2]["request_evidence"]["observations"]
                            self.assertEqual(saved["request_evidence"]["observations"][:len(old)], old)
        self.state = root

    def test_compact_to_passthrough_measures_current_projection_on_repeat_and_restart(self):
        root = self.state
        for mode in ("off", "audit"):
            with self.subTest(mode=mode):
                self.state = root / mode
                self.state.mkdir()
                # Full and projected wires receive distinct provider measurements.
                fixture = self.fixture(lambda b,n: (200, completed(response_id=f"mode_{mode}_{n}",
                    input_tokens=6000 if "NOODLE_SOURCE" in json.dumps(b) else 100)),
                    lambda _b,_n: (200, advice_response({"protected":[],"removable":[2],"memories":[]})))
                h = self.history()
                options = ["--payoff-requests","1","--classifier-cache-policy","disabled"]
                def start(mode, extra=()):
                    proxy = Proxy(self.state, fixture, [*options,*extra], mode=mode)
                    self.stack.callback(proxy.close)
                    return proxy
                proxy = start("compact", ["--review-replayed-history"])
                self.assertEqual(proxy.send(h, max_output_tokens=1)[0],200)
                proxy.close()
                options += ["--review-every-requests","100"]
                proxy = start("compact")
                self.assertEqual(proxy.send(h, max_output_tokens=1)[0],200)
                compact = self.snapshot(proxy)
                self.assertGreater(self.primary_observation(compact)["restoration"]["tokens"],0)
                self.assertNotIn(h[1],fixture.primaries[-1]["input"])
                proxy.close()
                # Execute every repeat/restart before grading restoration, so
                # a stale first receipt cannot hide the later lifetime seam.
                proxy = start(mode)
                snapshots = []
                for _ in range(2):
                    self.assertEqual(proxy.send(h, max_output_tokens=1)[0],200)
                    snapshots.append(self.snapshot(proxy))
                    self.assertEqual(fixture.primaries[-1]["input"],h)
                proxy.close()
                proxy = start(mode)
                self.assertEqual(proxy.send(h, max_output_tokens=1)[0],200)
                snapshots.append(self.snapshot(proxy))
                self.assertEqual(fixture.primaries[-1]["input"],h)
                proxy.close()
                # Returning to compact must measure a real current omission;
                # globally clearing restoration would fail this positive arm.
                proxy = start("compact")
                self.assertEqual(proxy.send(h, max_output_tokens=2)[0],200)
                resumed = self.snapshot(proxy)
                self.assertNotIn(h[1],fixture.primaries[-1]["input"])
                self.evidence[mode] = {"compact":compact,"passthrough":snapshots,"resumed":resumed,
                                       "primaries":fixture.primaries,"reviews":fixture.reviews}
                self.assertEqual(len(fixture.reviews),1)
                for n, saved in enumerate(snapshots,3):
                    self.assertEqual(saved["primary"]["calls"],n)
                    self.assertEqual(saved["primary"]["input_tokens"],6000*(n-1)+100)
                    self.assertEqual(saved["primary"]["output_tokens"],n)
                    self.assertEqual(saved["shadow"],compact["shadow"])
                    result = saved["estimate"]
                    self.assertEqual(result["matched_primary_responses"], n)
                    self.assertEqual(result["coverage"]["unmodeled_calls"], 0)
                    self.assertEqual(result["excluded_unpriced_calls"], 0)
                    self.assertAlmostEqual(result["observed_primary_cost"],
                        compact["estimate"]["observed_primary_cost"] + (n-2) *
                        (6000 * saved["model_resolution"]["primary"]["rates"]["input"] +
                         saved["model_resolution"]["primary"]["rates"]["output"]) / 1_000_000,places=12)
                    self.assertEqual(result["traditional_compactions"],0)
                # Grade each current quantity independently, retaining all wire
                # and paid evidence even on the intentionally failing baseline.
                for label, saved in zip(("first","repeat","restart"),snapshots):
                    with self.subTest(mode=mode,request=label):
                        current = self.primary_observation(saved)
                        self.assertEqual(current["restoration"]["tokens"],0,
                                         "full current wire must replace prior compact restoration")
                        self.assertEqual(current["facts"]["native"]["input_tokens"],6000)
                        self.assertFalse(current["edited"])
                # The initial full 6000-token wire was already modeled warm.
                # Audit retains that scope; off changes it and must write cold.
                # Subsequent identical full wires reuse all 6000 in both modes.
                rates = compact["model_resolution"]["primary"]["rates"]
                restored = self.primary_observation(compact)["restoration"]["tokens"]
                canonical = 100 + restored
                self.assertLess(canonical, 6000)
                compact_facts = self.primary_observation(compact)["facts"]
                full_facts = self.primary_observation(snapshots[0])["facts"]
                self.assertEqual(compact_facts["cache_scope"] == full_facts["cache_scope"], mode == "audit")
                expected = (6000 * rates["write" if mode == "off" else "cached"] + rates["output"]) / 1e6
                self.assertAlmostEqual(snapshots[0]["estimate"]["traditional_cost"] -
                                       compact["estimate"]["traditional_cost"], expected, places=12)
                for previous, current in zip(snapshots, snapshots[1:]):
                    self.assertAlmostEqual(current["estimate"]["traditional_cost"] -
                                           previous["estimate"]["traditional_cost"],
                                           (6000*rates["cached"] + rates["output"]) / 1e6, places=12)
                for previous, current in zip([compact, *snapshots], [*snapshots, resumed]):
                    old = previous["request_evidence"]["observations"]
                    self.assertEqual(current["request_evidence"]["observations"][:len(old)], old)
                self.assertEqual(self.primary_observation(resumed)["restoration"],
                                 self.primary_observation(compact)["restoration"])
                self.assertEqual(resumed["estimate"]["matched_primary_responses"],6)
                self.assertEqual(resumed["primary"]["calls"],6)
                self.assertEqual(resumed["primary"]["input_tokens"],24200)
                self.assertEqual(resumed["shadow"],compact["shadow"])
                proxy.close()
        self.state = root

    def test_cold_extension_revokes_old_warm_prefix_through_repeat_restart_and_rewarm(self):
        def primary(_body, n):
            response = completed(response_id=f"extension_{n}")
            response["usage"]["input_tokens"] = 4000
            response["usage"]["input_tokens_details"]["cached_tokens"] = 3900 if n in (2,7) else 0
            if n in (3,8):
                # A raw partial hit is not write-time/extent proof. Establish
                # warmth with a real whole-input write after capability attestation.
                response["usage"]["input_tokens_details"]["cache_write_tokens"] = 4000
            return 200, response
        fixture = self.fixture(primary)
        self.evidence.update(actual_wires=fixture.primaries,reviews=fixture.reviews)
        h = [{"role":"user","content":"WARM_ORIGINAL " + "prefix " * 3000}]
        extended = h + [{"role":"user","content":"COLD_SUFFIX"}]
        options = ["--review-every-requests","100","--classifier-cache-policy","disabled"]
        proxy = TransparentProxy(self.state,fixture,options,mode="audit")
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(h)[0],200)
        proxy.close(); self.attest_cache()
        proxy = TransparentProxy(self.state,fixture,options,mode="audit")
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(h)[0],200)  # actual partial read, unknown original write
        self.assertEqual(proxy.send(h)[0],200)  # actual timed whole H write
        _, written = proxy.checkpoint(h)
        self.evidence["written"] = written
        self.assertEqual(written["last_plan"]["primary_cached_estimated_tokens"],0,
                         "unlocated partial read alone cannot establish a future prefix")
        self.assertEqual(written["primary"]["cache_write_tokens"],4000)
        self.assertEqual(proxy.send(extended)[0],200)  # actual cold H+suffix terminal
        _, cold = proxy.checkpoint(extended)
        self.assertGreater(cold["last_plan"]["primary_cached_estimated_tokens"],0,
                           "first extension legitimately forecasts the prior warm H")
        self.assertEqual(proxy.send(extended)[0],200)
        _, repeated = proxy.checkpoint(extended)
        proxy.close()
        proxy = TransparentProxy(self.state,fixture,options,mode="audit")
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(extended)[0],200)
        _, restarted = proxy.checkpoint(extended)
        self.assertEqual(proxy.send(extended)[0],200)  # later partial read, not a new write
        _, positive = proxy.checkpoint(extended)
        self.assertEqual(positive["primary"]["cached_tokens"],7800,
                         "prove the provider's later positive receipt was observed")
        self.assertEqual(proxy.send(extended)[0],200)  # genuine later whole-input write
        _, rewritten = proxy.checkpoint(extended)
        self.evidence["rewritten"] = rewritten
        self.assertEqual(rewritten["last_plan"]["primary_cached_estimated_tokens"],0,
                         "a later partial read cannot resurrect revoked write evidence")
        self.assertEqual(rewritten["primary"]["cache_write_tokens"],8000)
        self.assertEqual(proxy.send(extended)[0],200)
        _, rewarmed = proxy.checkpoint(extended)
        self.evidence.update(cold=cold,repeated=repeated,restarted=restarted,positive=positive,rewarmed=rewarmed,
                             actual_wires=fixture.primaries,events=self.events())
        self.assertEqual(repeated["last_plan"]["primary_cached_estimated_tokens"],0,
                         "newer cold extension supersedes old covered warmth")
        self.assertEqual(restarted["last_plan"]["primary_cached_estimated_tokens"],0)
        self.assertGreater(rewarmed["last_plan"]["primary_cached_estimated_tokens"],0)
        self.assertEqual(rewarmed["primary"]["calls"],9)
        self.assertEqual(rewarmed["primary"]["input_tokens"],36000)
        self.assertEqual(rewarmed["primary"]["cached_tokens"],7800)
        self.assertFalse(fixture.reviews)

    def test_cold_extension_changes_actual_disposable_selection_after_restart(self):
        def primary(_body,n):
            value = completed(response_id=f"selection_{n}")
            value["usage"]["input_tokens"] = 40000
            value["usage"]["input_tokens_details"]["cached_tokens"] = 39600 if n == 2 else 0
            if n == 3:
                value["usage"]["input_tokens_details"]["cache_write_tokens"] = 40000
            return 200,value
        fixture = self.fixture(primary,lambda _b,_n:(200,advice_response({"protected":[],"removable":[2],"memories":[]})))
        self.evidence.update(primaries=fixture.primaries,reviews=fixture.reviews)
        h = self.history()
        h[0]["content"] += " protected direction " * 10000
        extended = h + [{"role":"user","content":"APPENDED_CURRENT_TASK"}]
        options = ["--payoff-requests","1","--classifier-cache-policy","disabled"]
        proxy = self.proxy(fixture,[*options,"--review-replayed-history"])
        self.assertEqual(proxy.send(h)[0],200)
        proxy.close(); self.attest_cache()
        options += ["--review-every-requests","100"]
        proxy = TransparentProxy(self.state,fixture,options,mode="audit")
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(h)[0],200)
        self.assertEqual(proxy.send(h)[0],200)  # real full write, not the preceding partial read
        self.evidence["written"] = proxy.checkpoint(h)[1]
        self.assertEqual(proxy.checkpoint(h)[1]["primary"]["cache_write_tokens"],40000)
        proxy.close()
        proxy = self.proxy(fixture,options)
        self.assertEqual(proxy.send(extended)[0],200)
        self.assertIn(h[1],fixture.primaries[-1]["input"],"confirmed warm H makes retaining original cheaper on first append")
        _, warm = proxy.checkpoint(extended)
        proxy.close()
        proxy = self.proxy(fixture,options)
        self.assertEqual(proxy.send(extended)[0],200)
        _, cold = proxy.checkpoint(extended)
        self.evidence.update(warm=warm,cold=cold,primaries=fixture.primaries,reviews=fixture.reviews)
        self.assertNotIn(h[1],fixture.primaries[-1]["input"],"new cold extension must change real selection, not only a diagnostic")
        self.assertFalse(warm["last_plan"]["selected"])
        self.assertTrue(cold["last_plan"]["selected"])
        self.assertEqual(len(fixture.reviews),1)
        self.assertEqual(cold["primary"]["calls"],5)
        self.assertEqual(cold["primary"]["cached_tokens"],39600)

    def test_planning_and_dispatch_share_metadata_stripped_floor(self):
        fixture = self.fixture(lambda _b,n:(200,completed(response_id=f"floor_{n}", input_tokens=4000)),
            lambda _b,_n:(200,advice_response({"protected":[],"removable":[2],"memories":[]})))
        h = self.history()
        options = ["--review-replayed-history","--payoff-requests","1","--classifier-cache-policy","disabled",
                   "--review-min-primary-tokens","5000"]
        proxy = self.proxy(fixture,options)
        fields = {"metadata":{"diagnostic":"metadata " * 6000}}
        self.assertEqual(proxy.send(h,**fields)[0],200)
        self.assertEqual(proxy.send(h,**fields)[0],200)
        _, saved = proxy.checkpoint(h)
        decisions = [e["data"] for e in self.events() if e["event"] == "review_admission"]
        self.evidence.update(saved=saved,decisions=decisions,primaries=fixture.primaries,reviews=fixture.reviews)
        self.assertEqual(len(fixture.reviews),1)
        self.assertEqual(decisions[-1]["reason"],"primary_floor")
        self.assertLess(decisions[-1]["primary_estimated_tokens"],5000)
        self.assertEqual(saved["last_plan"]["primary_estimated_tokens"],4000,
                         "planner reuses native input; diagnostic metadata adds no model input")
        self.assertEqual(decisions[-1]["primary_estimated_tokens"],4000)
        candidates = saved["last_plan"]["candidates"]
        self.assertTrue(candidates)
        for candidate in candidates:
            joint = candidate["selection_joint"]
            self.assertEqual(joint["future_shadow_keep"],0,
                             "an undispatchable ordinary review cannot subsidize omission")
            self.assertEqual(joint["future_shadow_compact"],0)

    def bootstrap_retry(self, rejected):
        def review(_body,n):
            if n == 1:
                return (200,advice_response({"protected":[999],"removable":[],"memories":[]})) if rejected else None
            return 200,advice_response({"protected":[],"removable":[],"memories":[]})
        fixture = self.fixture(lambda _b,n:(200,completed(response_id=f"retry_{n}")),review)
        h = self.history()
        options = ["--review-replayed-history","--classifier-cache-policy","disabled",
                   "--review-min-primary-tokens","100000","--review-every-requests","3"]
        proxy = self.proxy(fixture,options)
        self.assertEqual(proxy.send(h)[0],200)
        _, failed = proxy.checkpoint(h)
        index = json.loads((self.state / "history.index").read_text())
        self.assertFalse(index["reviews"])
        self.assertEqual(failed["shadow"]["calls"],1)
        self.assertEqual(failed["shadow"]["unavailable_cost_calls"],int(not rejected))
        self.assertEqual(failed["shadow"]["input_tokens"],100 if rejected else 0)
        self.assertTrue(failed["bootstrap_review_pending"])
        self.assertTrue(failed["bootstrap_review_attempted"])
        # Failed bootstrap must honor normal request cadence on its retries,
        # even though the initial replay acquisition bypassed ordinary floors.
        self.assertEqual(proxy.send(h)[0],200)
        self.assertEqual(len(fixture.reviews),1)
        proxy.close()
        proxy = self.proxy(fixture,options)
        self.assertEqual(proxy.send(h)[0],200)
        self.assertEqual(len(fixture.reviews),1)
        self.assertEqual(proxy.send(h)[0],200)
        _, accepted = proxy.checkpoint(h)
        self.evidence.update(failed=failed,accepted=accepted,events=self.events(),reviews=fixture.reviews)
        self.assertEqual(len(fixture.reviews),2,"below-floor intent must retry after failure/rejection and restart")
        # Settlement does not decide semantic completion. The next ingress
        # applies the indexed accepted-empty record and closes replay intent.
        self.assertTrue(accepted["bootstrap_review_pending"])
        self.assertEqual(accepted["shadow"]["calls"],2)
        self.assertEqual(accepted["shadow"]["input_tokens"],200 if rejected else 100)
        self.assertEqual(accepted["shadow"]["unavailable_cost_calls"],int(not rejected))
        self.assertTrue(json.loads((self.state / "history.index").read_text())["reviews"],
                        "accepted empty advice is a durable semantic completion")
        proxy.close()
        proxy = self.proxy(fixture,options)
        for _ in range(4): self.assertEqual(proxy.send(h)[0],200)
        self.assertEqual(len(fixture.reviews),2,"accepted empty completion closes below-floor retries")
        self.assertEqual(proxy.checkpoint(h)[1]["shadow"],accepted["shadow"])
        self.assertFalse(proxy.checkpoint(h)[1]["bootstrap_review_pending"])

    def test_failed_bootstrap_retries_below_floor_with_cadence_and_once_only_censorship(self):
        self.bootstrap_retry(False)

    def test_all_rejected_bootstrap_retries_below_floor_with_cadence_and_once_only_payment(self):
        self.bootstrap_retry(True)


if __name__ == "__main__":
    unittest.main()
