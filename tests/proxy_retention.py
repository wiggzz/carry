"""Real wire/native expense and paginated durable accounting, no providers."""
import json
import tempfile
import unittest
from pathlib import Path
from urllib.parse import urlencode
from proxy_state_regressions import Fixture
from proxy_transparent_history import TransparentProxy


class RetainedAccounting(unittest.TestCase):
    def test_native_failures_and_restart_in_paginated_numeric_history(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = Fixture()
            self.addCleanup(fixture.close)
            def primary(body, n):
                if n == 2:
                    return 500, {"error": {"message": "scripted unknown-cost failure"}}
                return 200, {"id": f"receipt-{n}", "status": "completed", "output": [],
                             "usage": {"input_tokens": n * 100, "output_tokens": 1,
                                       "input_tokens_details": {"cached_tokens": 0}}}
            fixture.primary = primary
            extra = ("--review-min-primary-tokens", "4294967295", "--review-min-new-tokens", "4294967295")
            proxy = TransparentProxy(root, fixture, extra=extra)
            self.addCleanup(proxy.close)
            body = [{"role": "user", "content": "native receipt history"}]
            self.assertEqual(proxy.send(body)[0], 200)
            status, first = proxy.request("GET", "/carry/dashboard/accounting?limit=1")
            self.assertEqual(status, 200, "durable numeric history needs a bounded public read path")
            self.assertEqual(len(first["records"]), 1)
            self.assertEqual(first["records"][0]["primary"]["calls"], 1)
            # A failed attempt is an unknown cost, never a free measured receipt.
            self.assertEqual(proxy.send(body)[0], 500)
            self.assertEqual(proxy.send(body)[0], 200)
            saved = proxy.saved()
            frozen_paid = saved["primary"]
            self.assertEqual(frozen_paid["calls"], 3)
            self.assertEqual(frozen_paid["unavailable_cost_calls"], 1)
            self.assertEqual(len(fixture.primaries), 3)
            self.assertEqual(len(fixture.reviews), 0)
            for wire in fixture.primaries:
                self.assertEqual(wire["input"], body)
            proxy.close()
            proxy = TransparentProxy(root, fixture, extra=extra)
            self.addCleanup(proxy.close)
            records, query = [], {"limit": 1}
            while True:
                status, page = proxy.request("GET", "/carry/dashboard/accounting?" + urlencode(query))
                self.assertEqual(status, 200)
                self.assertLessEqual(len(page["records"]), 1)
                records.extend(page["records"])
                if not page["has_more"]:
                    break
                self.assertIsNotNone(page["next"])
                query = {"limit": 1, **page["next"]}
            self.assertEqual(len(records), 3, "checkpoint saves must not duplicate settled paid states")
            self.assertEqual(records[0], first["records"][0], "prior prices and provenance remain frozen")
            self.assertEqual(records[-1]["primary"], frozen_paid)
            self.assertNotIn("native receipt history", json.dumps(records))
            self.assertEqual(len(fixture.primaries), 3, "history reads cannot dispatch models")
            self.assertFalse(fixture.errors, fixture.errors)


if __name__ == "__main__":
    unittest.main(verbosity=2)
