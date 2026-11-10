"""Real native failure/unknown population versus successful monthly comparison."""
import hashlib
import json
from pathlib import Path
import sqlite3
import shutil
import tempfile
import time
import unittest

from proxy_state_regressions import Fixture
from proxy_transparent_history import TransparentProxy


class NativeFailureWindow(unittest.TestCase):
    def test_real_failures_and_unknown_usage_survive_restart_and_raw_expiry(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        state = Path(temporary.name)

        def primary(_body, n):
            if n == 2:
                return 500, {"error": {"message": "scripted unpriced native failure"}}
            # Every call submits identical complete input/output, so native
            # quantities stay equal rather than staging inferred history growth.
            return 200, {"id": f"failure-window-{n}",
                         "status": "failed" if n == 3 else "completed", "output": [],
                         "usage": {"input_tokens": 100, "output_tokens": 1,
                                   "input_tokens_details": {"cached_tokens": 0}}}

        def reviewer(_body, _n):
            self.fail("read-only accounting cannot dispatch reviewers")

        fixture = Fixture(primary, reviewer)
        self.addCleanup(fixture.close)
        extra = ("--review-min-primary-tokens", "2000000", "--review-min-new-tokens", "2000000")
        proxy = TransparentProxy(state, fixture, extra=extra)
        self.addCleanup(proxy.close)
        body = [{"role": "user", "content": "private real failure monthly lifecycle"}]
        self.assertEqual(proxy.send(body)[0], 200)
        status, first = proxy.request("GET", "/carry/dashboard/accounting?limit=100")
        self.assertEqual(status, 200)
        self.assertEqual(len(first["records"]), 1)
        opening = first["records"][0]
        # Real later observation second, not a rewrite of paid receipt clocks.
        deadline = time.monotonic() + 3
        while int(time.time()) <= opening["at"]:
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.02)
        self.assertEqual(proxy.send(body)[0], 500)
        status, failed = proxy.send(body)
        self.assertEqual(status, 200)
        self.assertEqual(failed["status"], "failed")
        proxy.close()
        proxy = TransparentProxy(state, fixture, extra=extra)
        self.addCleanup(proxy.close)
        self.assertEqual(proxy.send(body)[0], 200)
        status, records = proxy.request("GET", "/carry/dashboard/accounting?limit=100")
        self.assertEqual(status, 200)
        rows = records["records"]
        self.assertEqual(len(rows), 4)
        self.assertEqual(rows[0], opening)
        last = rows[-1]
        self.assertEqual(last["primary"]["calls"], 4)
        self.assertEqual(last["primary"]["unavailable_cost_calls"], 1)
        self.assertAlmostEqual(last["primary"]["cost_usd"], 0.0000315, places=12)
        self.assertEqual(last["completed_requests"], 2)
        self.assertEqual(last["failed_primaries"], 2)
        self.assertEqual(len(fixture.primaries), 4)
        self.assertEqual(fixture.reviews, [])
        for sent in fixture.primaries:
            self.assertEqual(sent["input"], body)
        self.assertFalse(fixture.errors, fixture.errors)
        query = (f'/carry/dashboard/accounting?start_at={opening["at"]}'
                 f'&end_at={last["at"]}')
        status, before = proxy.request("GET", query)
        self.assertEqual(status, 200)
        window = before["window"]
        self.assertEqual(window["coverage"]["included_owners"], 1)
        self.assertEqual(window["usage"]["primary"]["calls"], 3)
        self.assertEqual(window["usage"]["primary"]["unavailable_cost_calls"], 1)
        self.assertAlmostEqual(window["usage"]["primary"]["cost_usd"], 0.000021, places=12)
        self.assertEqual(window["completed_requests"], 1)
        self.assertEqual(window["failed_primaries"], 2)
        self.assertEqual(window["comparison"]["included_owners"], 1)
        self.assertEqual(window["comparison"]["primary_responses"], 1)
        self.assertAlmostEqual(window["comparison"]["carry_cost"], 0.0000105, places=12)
        self.assertAlmostEqual(window["comparison"]["traditional_cost"], 0.0000105, places=12)
        proxy.close()
        # Staged raw-age seam, not seven elapsed days. Paid observations, their
        # original prices, numeric provenance/order and owner IDs are untouched.
        db_path = state / "history.index"
        with sqlite3.connect(db_path) as db:
            changed = db.execute(
                "UPDATE records SET at=? WHERE kind IN "
                "('nodes','tips','reviews','transcripts','snapshots','owners')",
                (opening["at"] - 8 * 86400,),
            ).rowcount
            self.assertGreater(changed, 0)
        frozen = hashlib.sha256(db_path.read_bytes()).hexdigest()
        for _ in range(2):
            proxy = TransparentProxy(state, fixture, extra=extra)
            self.addCleanup(proxy.close)
            read_started = int(time.time())
            status, after = proxy.request("GET", query)
            read_finished = int(time.time())
            self.assertEqual(status, 200)
            # The advertised rolling retention horizon is read-time metadata,
            # not a frozen accounting fact. Validate its own clock domain and
            # compare every other field without altering any paid observation.
            horizon = after["window"]["retention_start_at"]
            retention = records["accounting_retention_secs"]
            self.assertGreaterEqual(horizon, read_started - retention)
            self.assertLessEqual(horizon, read_finished - retention)
            comparable = json.loads(json.dumps(after))
            comparable["window"]["retention_start_at"] = before["window"]["retention_start_at"]
            self.assertEqual(comparable, before)
            status, population = proxy.request("GET", "/carry/dashboard/stats?limit=1")
            self.assertEqual(status, 200)
            self.assertEqual(population["sessions"], [])
            self.assertEqual(population["usage"]["primary"]["calls"], 4)
            self.assertEqual(population["usage"]["primary"]["unavailable_cost_calls"], 1)
            self.assertAlmostEqual(population["usage"]["primary"]["cost_usd"], 0.0000315, places=12)
            status, retained = proxy.request("GET", "/carry/dashboard/accounting?limit=100")
            self.assertEqual(status, 200)
            self.assertEqual(retained, records)
            proxy.close()
            self.assertEqual(hashlib.sha256(db_path.read_bytes()).hexdigest(), frozen)
        # Reader compatibility controls on private stopped copies. These are
        # deliberately staged frozen metadata, not a claim that a live producer
        # changed policies or recovered a historical prefix. Native money and
        # all successful estimates remain byte-for-byte the same values.
        variants = tempfile.TemporaryDirectory()
        self.addCleanup(variants.cleanup)
        for case in ("scenario", "policy", "historical-prefix", "intermediate-scenario"):
            with self.subTest(incompatible_boundary=case):
                variant = Path(variants.name) / case
                shutil.copytree(state, variant)
                variant_db = variant / "history.index"
                with sqlite3.connect(variant_db) as db:
                    observations = db.execute(
                        "SELECT key,data FROM records WHERE kind='accounting' ORDER BY key"
                    ).fetchall()
                    self.assertEqual(len(observations), 4)
                    key, raw = observations[-2 if case == "intermediate-scenario" else -1]
                    value = json.loads(raw)
                    unchanged = {name: value[name] for name in
                                 ("primary", "reviewer", "completed_requests", "failed_primaries", "estimate")}
                    if case == "policy":
                        value["estimator_policy"]["quantity_policy"] = "staged_incompatible_policy"
                    elif case == "historical-prefix":
                        value["numeric_provenance"]["historical_success_prefix"] = {"staged": "different-prefix"}
                    else:
                        value["numeric_provenance"]["scenario"]["summary"] += 1
                    self.assertEqual({name: value[name] for name in unchanged}, unchanged)
                    db.execute("UPDATE records SET data=? WHERE kind='accounting' AND key=?",
                               (json.dumps(value).encode(), key))
                before_read = hashlib.sha256(variant_db.read_bytes()).hexdigest()
                proxy = TransparentProxy(variant, fixture, extra=extra)
                self.addCleanup(proxy.close)
                status, incompatible = proxy.request("GET", query)
                self.assertEqual(status, 200)
                result = incompatible["window"]
                self.assertEqual(result["coverage"], window["coverage"])
                self.assertEqual(result["usage"], window["usage"])
                self.assertEqual(result["completed_requests"], window["completed_requests"])
                self.assertEqual(result["failed_primaries"], window["failed_primaries"])
                self.assertEqual(result["comparison"]["included_owners"], 0)
                self.assertEqual(result["comparison"]["unavailable_owners"], 1)
                self.assertIsNone(result["comparison"]["carry_cost"])
                self.assertIsNone(result["comparison"]["traditional_cost"])
                proxy.close()
                self.assertEqual(hashlib.sha256(variant_db.read_bytes()).hexdigest(), before_read)
        self.assertEqual(len(fixture.primaries), 4)
        self.assertEqual(fixture.reviews, [])
        self.assertFalse(fixture.errors, fixture.errors)


if __name__ == "__main__":
    unittest.main(verbosity=2)
