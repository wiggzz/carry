"""Synthetic loopback keep-lease lifecycles against the real Carry binary."""
import json
import concurrent.futures
import os
import select
import subprocess
import tempfile
import threading
import time
import unittest
from pathlib import Path

from proxy_state_regressions import Proxy, StateRegressions, advice_response, completed, ledger


class KeepLeases(StateRegressions):
    # Do not inherit the unrelated fixture cases into this suite.
    def test_fixture_diagnostic_pipes_are_drained_without_losing_bytes(self):
        command = ["python3", "-c", "import sys; sys.stdout.buffer.write(b'x'*262144+b'out-end'); sys.stdout.flush(); sys.stderr.buffer.write(b'y'*262144+b'err-end'); sys.stderr.flush()"]
        env = {"PATH":os.environ.get("PATH", "/usr/bin:/bin"), "HOME":str(self.state)}
        blocked = subprocess.Popen(command, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            self.assertTrue(select.select([blocked.stdout], [], [], 5)[0], "negative control actually started writing")
            with self.assertRaises(subprocess.TimeoutExpired):
                blocked.wait(timeout=0.2)
        finally:
            blocked.kill()
            partial, _ = blocked.communicate(timeout=5)
        self.assertGreater(len(partial), 0)
        self.assertNotIn(b'out-end', partial, "undrained diagnostic output really blocks before completion")
        with (self.state / "stdout-probe.log").open("wb", buffering=0) as out, (self.state / "stderr-probe.log").open("wb", buffering=0) as err:
            process = subprocess.Popen(command, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            drains = [threading.Thread(target=Proxy.drain_output, args=(stream, capture))
                      for stream, capture in ((process.stdout, out), (process.stderr, err))]
            try:
                for drain in drains: drain.start()
                self.assertEqual(process.wait(timeout=5), 0)
            finally:
                if process.poll() is None: process.kill()
                process.wait(timeout=5)
                for drain in drains: drain.join(timeout=5)
                process.stdout.close(); process.stderr.close()
            self.assertTrue(all(not drain.is_alive() for drain in drains))
        self.assertEqual((self.state / "stdout-probe.log").read_bytes(), b'x'*262144+b'out-end')
        self.assertEqual((self.state / "stderr-probe.log").read_bytes(), b'y'*262144+b'err-end')
        self.evidence.update(negative_control_partial_stdout_bytes=len(partial),
            drained_stdout_bytes=(self.state / "stdout-probe.log").stat().st_size,
            drained_stderr_bytes=(self.state / "stderr-probe.log").stat().st_size)

    def test_cli_environment_values_and_effective_provenance(self):
        fixture = self.fixture(lambda _body, _n: (200, completed()))
        for environment, extra, expected, source in [
            ({}, [], 8, "default"),
            ({"CARRY_PROXY_KEEP_LEASE_TURNS": "3"}, [], 3, "environment"),
            ({"CARRY_PROXY_KEEP_LEASE_TURNS": "3"}, ["--keep-lease-turns", "5"], 5, "command_line"),
            ({}, ["--keep-lease-turns", "8"], 8, "command_line"),
        ]:
            with self.subTest(environment=environment, extra=extra):
                self.state = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
                proxy = self.proxy(fixture, extra, environment)
                self.assertEqual(proxy.send([{"role":"user","content":"configuration control"}])[0], 200)
                policy = proxy.request("GET", "/carry/metrics")[1].get("keep_lease_policy")
                self.assertIsNotNone(policy, "LEASE_CONFIG_RED: metrics must expose actual configured policy, not infer history")
                self.assertEqual(policy["turns"], expected)
                self.assertEqual(policy["source"], source)
                self.assertEqual(policy["round_basis"], "completed_primary_responses")
                self.assertEqual(policy["due_targets"], 0)
                self.assertEqual(policy["pending_targets"], 0)
                self.assertEqual(policy["released_targets"], 0)
                self.assertEqual(proxy.saved()["keep_leases"]["turns"], expected)
                proxy.close()

    def test_cli_and_environment_reject_invalid_lease_turns_before_network(self):
        binary = os.environ["CARRY_TEST_BINARY"]
        for bad in ("0", "-1", "18446744073709551616", "not-a-round"):
            for via_env in (False, True):
                with self.subTest(value=bad, environment=via_env):
                    env = {"PATH":os.environ.get("PATH", "/usr/bin:/bin"), "HOME":str(self.state),
                           "CARRY_HOME":str(self.state)}
                    args = [binary, "proxy"]
                    if via_env:
                        env["CARRY_PROXY_KEEP_LEASE_TURNS"] = bad
                    else:
                        args.append("--keep-lease-turns=" + bad)
                    result = subprocess.run(args, env=env, capture_output=True, text=True, timeout=5)
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertNotIn("CARRY_PROXY_LISTEN", result.stdout)

    def test_economic_lease_only_review_is_nonblocking_even_legacy_synchronous(self):
        evidence = {"role":"assistant","content":"DEMAND_EVIDENCE " * 6000}
        history = [{"role":"user","content":"goal"}, evidence,
                   {"role":"user","content":"current task"}]
        def review(body, n):
            if n == 1:
                return 200, advice_response({"protected":["g2"],"removable":[],"memories":[]})
            self.assertIn("g2", ledger(body).get("keep_lease_review", {}).get("target_ids", []))
            fixture.held.set()
            if not fixture.release.wait(8):
                raise AssertionError("targeted review was not released")
            return 200, advice_response({"protected":[],"removable":[],"memories":[]})
        fixture = self.fixture(lambda _b, _n: (200, completed()), review)
        options = ["--background-reviews=false", "--keep-lease-turns", "2"]
        proxy = self.proxy(fixture, options)
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertEqual(proxy.saved()["opinions"].get("2"), "keep")
        proxy.close()
        proxy = self.proxy(fixture, options + ["--review-min-new-tokens", "25000"])
        # The legacy flag no longer makes review a primary dependency. This
        # concurrency case must not wait in the fixture's post-response barrier.
        proxy.wait_reviews = False
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertEqual(proxy.send(history)[0], 200)
        before = len(fixture.primaries)
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
            pending = pool.submit(proxy.send, history)
            try:
                self.assertTrue(fixture.held.wait(3), "LEASE_ECON_HTTP_RED: no-growth due demand must dispatch bounded review")
                # RED4 proved status200/primary+1 with real review still pending
                # and unreleased. Test that causal independence, not a newly
                # invented one-second CPU latency guarantee.
                self.assertEqual(pending.result(timeout=5)[0], 200, "new lease admission cannot block primary")
                self.assertFalse(fixture.release.is_set())
                self.assertTrue(proxy.saved()["background_review_pending"])
                self.assertEqual(len(fixture.primaries), before + 1)
                held = proxy.saved()
                self.assertEqual(held["opinions"].get("2"), "keep")
                self.assertIn(evidence, fixture.primaries[-1]["input"])
            finally:
                fixture.release.set()
        released = self.wait_saved(proxy, lambda s: not s["background_review_pending"])
        self.assertIsNone(released["opinions"].get("2"))
        self.assertEqual(proxy.send(history)[0], 200)
        self.assertNotIn(evidence, fixture.primaries[-1]["input"])
        self.assertEqual(len(fixture.reviews), 2, "no immediate repeat paid loop after release")

    def test_expiry_target_omission_becomes_economic_not_timer_deletion(self):
        evidence = {"role": "assistant", "content": "LEASE_EVIDENCE " * 6000}
        transcript = [{"role": "user", "content": "preserve the task"}, evidence,
                      {"role": "user", "content": "work on next step"}]
        target_seen = threading.Event()
        requests_at_release = []

        def review(body, n):
            if n == 1:
                return 200, advice_response({"protected": ["g2"], "removable": [], "memories": []})
            if "g2" in ledger(body).get("keep_lease_review", {}).get("target_ids", []):
                target_seen.set()
                fixture.held.set()
                fixture.release.wait(20)
                requests_at_release.append(len(fixture.primaries))
            return 200, advice_response({"protected": [], "removable": [], "memories": []})

        fixture = self.fixture(lambda _body, n: (200, completed(response_id=f"resp_{n}")), review)
        proxy = self.proxy(fixture, ["--background-reviews=true"])

        def send():
            status, body = proxy.send(transcript)
            self.assertEqual(status, 200, body)
            return self.wait_saved(proxy, lambda s: s["completed_requests"] == len(fixture.primaries))

        send()  # First actual primary makes the initial evidence exposed.
        transcript.append({"role": "user", "content": "step 2"})
        send()
        protected = self.wait_saved(proxy, lambda s: not s["background_review_pending"])
        self.assertEqual(protected["opinions"].get("2"), "keep")
        anchor = protected["completed_requests"]
        # Seven later successful primary completions: ordinary omissions stay sticky.
        for turn in range(7):
            transcript.append({"role": "user", "content": f"before expiry {turn}"})
            send()
            saved = self.wait_saved(proxy, lambda s: not s["background_review_pending"])
            self.assertEqual(saved["opinions"].get("2"), "keep")
            self.assertFalse(target_seen.is_set(), "no target before eight later primary completions")
        self.assertEqual(saved["completed_requests"], anchor + 7)
        transcript.append({"role": "user", "content": "eighth completion"})
        send()
        saved = self.wait_saved(proxy, lambda s: not s["background_review_pending"])
        self.assertEqual(saved["completed_requests"], anchor + 8)
        self.assertFalse(target_seen.is_set())
        transcript.append({"role": "user", "content": "review due lease"})
        send()  # Must not wait for the held targeted reviewer.
        self.assertTrue(target_seen.wait(5), "LEASE_RED: due protected source must receive targeted revalidation")
        held = proxy.saved()
        self.assertEqual(held["opinions"].get("2"), "keep")
        self.assertFalse(next(i for i in held["history"] if i["id"] == 2)["removed"])
        self.assertIn(evidence, fixture.primaries[-1]["input"], "expiry itself cannot delete source")
        fixture.release.set()
        released = self.wait_saved(proxy, lambda s: not s["background_review_pending"])
        self.assertIsNone(released["opinions"].get("2"), "target omission is neutral, not fabricated drop")
        self.assertNotIn("2", released["drop_members"])
        self.assertFalse(next(i for i in released["history"] if i["id"] == 2)["removed"])
        transcript.append({"role": "user", "content": "use ordinary profitable compaction"})
        send()
        self.assertNotIn(evidence, fixture.primaries[-1]["input"], "accepted target omission must unlock actual paired planner removal")
        final = proxy.saved()
        self.assertTrue(next(i for i in final["history"] if i["id"] == 2)["removed"])
        self.assertTrue(requests_at_release, "held reviewer actually completed")
        self.evidence.update(protection=protected, held=held, released=released, final=final,
                             reviews=fixture.reviews, primaries=fixture.primaries)


if __name__ == "__main__":
    # Only this class's explicitly owned tests; shared fixture helpers stay reusable.
    names = [name for name in KeepLeases.__dict__ if name.startswith("test_")]
    suite = unittest.TestSuite(KeepLeases(name) for name in names)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(not result.wasSuccessful())
