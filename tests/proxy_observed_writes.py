"""Actual dispatch/terminal write bounds, synthetic loopback only.

The fixture attests whole-input writes during its request, not measured vendor
cache retention. Partial native counts cannot locate a write in local units.
No saved write evidence is injected. JSON/SSE share the real terminal commit.
"""
import copy
import http.client
import json
import time
import unittest

import proxy_state_regressions as support


class ObservedWrites(unittest.TestCase):
    def setUp(self):
        self.harness = support.StateRegressions()
        self.harness.setUp()
        self.harness._testMethodName = self._testMethodName
        self.evidence = self.harness.evidence
        self.addCleanup(lambda: self.assertTrue(self.harness.doCleanups(),
                                               "nested fixture cleanup failed"))

    def lifecycle(self, *, partition, sse):
        # First dispatch establishes a real frozen route/owner. Only the fixture's
        # cache capability is attested afterward; actual observations must still
        # be produced by later real dispatches, not by checkpoint staging.
        first = {"full": (0, 2000), "partial": (1000, 1000), "read": (2000, 0)}[partition]
        receipts = [(0, 0), first, (2000, 0), (0, 0), (2000, 0), (0, 2000)]
        provider_times = []

        def primary(_body, n):
            provider_times.append(time.time_ns() // 1_000_000_000)
            cached, written = receipts[n - 1]
            value = support.completed(response_id=f"observed-write-{n}")
            value["usage"] = {"input_tokens": 2000, "output_tokens": 10,
                              "input_tokens_details": {"cached_tokens": cached,
                                                       "cache_write_tokens": written}}
            if sse:
                return 200, b"data: " + support.wire({"type": "response.completed",
                                                       "response": value}) + b"\n\n"
            return 200, value

        def review(*_):
            raise AssertionError("observed-write fixture must not dispatch reviewer work")

        fixture = self.harness.fixture(primary, review)
        self.addCleanup(lambda: self.assertEqual(fixture.errors, []))
        history = [{"role": "user", "content": "synthetic whole-prefix write " * 2000}]
        snapshots, brackets, wires = [], [], []
        paid = 0.0
        for step, (cached, written) in enumerate(receipts):
            proxy = support.Proxy(self.harness.state, fixture, mode="audit",
                extra_env={"CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS": "1000000"})
            self.harness.stack.callback(proxy.close)
            before = time.time_ns() // 1_000_000_000
            if sse:
                connection = http.client.HTTPConnection(*proxy.address, timeout=30)
                try:
                    connection.request("POST", "/v1/responses", body=support.wire({
                        "model": "gpt-6.1-sol", "input": history, "stream": True}),
                        headers={"Content-Type": "application/json", "x-carry-session": "state-regression"})
                    response = connection.getresponse()
                    raw = response.read()
                    self.assertEqual(response.status, 200)
                    self.assertIn(f'"observed-write-{step + 1}"'.encode(), raw)
                    self.assertIn(b'"response.completed"', raw)
                finally:
                    connection.close()
            else:
                self.assertEqual(proxy.send(history, model="gpt-6.1-sol")[0], 200)
            after = time.time_ns() // 1_000_000_000
            saved = proxy.saved()
            records = json.loads((self.harness.state / "history.index").read_text())["upstream_cache"]
            paid += ((2000 - cached - written) * 2 + cached * .1 + written * 2.5 + 10 * 10) / 1e6
            self.assertEqual(saved["primary"]["calls"], step + 1)
            self.assertEqual(saved["primary"]["cached_tokens"], sum(x[0] for x in receipts[:step + 1]))
            self.assertEqual(saved["primary"]["cache_write_tokens"], sum(x[1] for x in receipts[:step + 1]))
            self.assertAlmostEqual(saved["primary"]["cost_usd"], paid, places=12)
            self.assertEqual(saved["primary"]["unavailable_cost_calls"], 0)
            self.assertEqual(saved["compactions"], 0)
            self.assertEqual(saved["shadow"]["calls"], 0)
            self.assertEqual(fixture.primaries[-1]["input"], history)
            if step:
                # Immutable write proof and the latest raw read observation are
                # distinct facts; a read must not rewrite the original receipt.
                expected_records = 2 if partition == "full" and step == 2 else 1
                self.assertEqual(len(records), expected_records)
                self.assertEqual(records[-1]["native_cached_fraction"], cached / 2000)
                self.assertEqual(records[-1]["native_input_tokens"], 2000)
                self.assertEqual(records[-1]["native_write_tokens"], written)
            snapshots.append(copy.deepcopy(records))
            brackets.append((before, after))
            wires.append(copy.deepcopy(fixture.primaries[-1]))
            proxy.close()
            if step == 0:
                self.assertEqual(records, [], "unknown gateway has no cache entitlement")
                checkpoint = next(self.harness.state.glob("*.json"))
                staged = json.loads(checkpoint.read_text())
                staged["model_resolution"]["primary"]["cache"] = {
                    "minimum_prefix_tokens": 1024, "max_read_breakpoints": 50,
                    "max_write_breakpoints": 4, "implicit_breakpoint_uses_write_slot": True}
                checkpoint.write_text(json.dumps(staged))

        self.evidence.update(partition=partition, sse=sse, snapshots=snapshots,
                             dispatch_brackets=brackets, provider_times=provider_times,
                             primaries=fixture.primaries, reviews=fixture.reviews,
                             fixture_errors=fixture.errors, final=saved)
        self.assertEqual(fixture.errors, [])
        self.assertEqual(fixture.reviews, [])
        self.assertEqual(len(wires), len(receipts))
        self.assertTrue(all(wire == wires[0] for wire in wires), "whole native wire is unchanged")

        # Every wire/paid/raw-observation/restart control above runs before the
        # first missing producer assertion. A read cannot manufacture a write or
        # resurrect one revoked by a newer miss. A later full write can rewarm.
        first_window = snapshots[1][0].get("write_window")
        with self.subTest(stage="initial_write"):
            if partition == "full":
                self.assertIsInstance(first_window, dict,
                    "OBSERVED_WRITE_PRODUCER: real full write needs transport bounds, not terminal-time freshness")
                self.assertGreaterEqual(first_window["not_before"], brackets[1][0])
                self.assertLessEqual(first_window["not_before"], provider_times[1])
                self.assertGreaterEqual(first_window["not_after"], provider_times[1])
                self.assertLessEqual(first_window["not_after"], brackets[1][1])
                self.assertEqual(first_window["lifetime"], 1800)
            else:
                self.assertIsNone(first_window, "partial/unlocated writes and reads do not prove whole-prefix timing")
        with self.subTest(stage="read_not_refresh"):
            self.assertEqual(snapshots[2][0].get("write_window"), first_window)
            if partition == "full":
                self.assertEqual(snapshots[2][0], snapshots[1][0],
                                 "read cannot rewrite the original full-write fact")
                self.assertIsNone(snapshots[2][-1].get("write_window"),
                                  "latest read has no new write timing")
        for step in (3, 4):
            with self.subTest(stage="miss_then_unknown_hit", step=step):
                self.assertIsNone(snapshots[step][0].get("write_window"))
        with self.subTest(stage="actual_rewarm"):
            window = snapshots[5][0].get("write_window")
            self.assertIsInstance(window, dict, "OBSERVED_WRITE_PRODUCER: a new full write must rewarm")
            self.assertGreaterEqual(window["not_before"], brackets[5][0])
            self.assertLessEqual(window["not_before"], provider_times[5])
            self.assertGreaterEqual(window["not_after"], provider_times[5])
            self.assertLessEqual(window["not_after"], brackets[5][1])
            self.assertEqual(window["lifetime"], 1800)


def install_cases():
    for partition in ("full", "partial", "read"):
        for sse in (False, True):
            def case(self, partition=partition, sse=sse):
                self.lifecycle(partition=partition, sse=sse)
            setattr(ObservedWrites, f"test_{partition}_{'sse' if sse else 'json'}_write_bounds_restart", case)


install_cases()
if __name__ == "__main__":
    unittest.main(verbosity=2)
