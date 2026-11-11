"""PR184 review regressions against the actual CLI; synthetic loopback only."""
from history_store import read_index, write_index
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

    def explicit_paid_pressure(self, *, pressure):
        """Hold a real first Off receipt while preserving every promised byte."""
        import threading
        import http.client

        held, release = threading.Event(), threading.Event()
        def primary(_body, n):
            if n == 1:
                held.set()
                self.assertTrue(release.wait(15), "release the actual held paid receipt")
            return 200, completed(response_id=f"explicit_paid_{n}")

        fixture = self.fixture(primary)
        self.stack.callback(release.set)
        options = ["--request-timeout-secs", "20"]
        proxy = Proxy(self.state, fixture, options, mode="off", wait_reviews=False)
        self.stack.callback(proxy.close)
        body = {"model": "gpt-6-luna", "input": [
            {"role": "user", "content": "EXPLICIT_OFF_FIRST_PAID_RECEIPT"}]}
        result = {}
        def send():
            try:
                result["response"] = proxy.request("POST", "/v1/responses", body)
            except (http.client.HTTPException, OSError) as error:
                result["error"] = type(error).__name__
        worker = threading.Thread(target=send, daemon=True)
        worker.start()
        self.assertTrue(held.wait(10), "real primary must reach upstream before pressure")
        pending = proxy.saved()
        self.assertTrue(pending["primary_pending"])
        self.assertEqual(pending["primary"]["calls"], 0)
        self.assertFalse(pending["history_shared"])
        self.assertIsNone(pending.get("history_namespace"))
        self.assertEqual(fixture.primaries, [body], "Off forwarding preserves the entire wire")
        self.assertEqual(fixture.reviews, [])
        # Default policy: one paid owner plus independent maintenance. Sparse
        # synthetic external pressure consumes ordinary headroom, not promises.
        max_record, disk_limit = 32 * 1024**2, 1024**3
        charge = lambda size: size * 4 + 16384
        paid = charge(max_record) * 2 + charge(128) * 2 + max_record * 2 + 65536
        maintenance = min(charge(max_record) + 65536 + charge(0), disk_limit // 2)
        before_bytes = sum(p.stat().st_size for p in self.state.rglob("*"))
        pressure_file = self.state / "ordinary-pressure.bin"
        if pressure:
            extent = disk_limit - paid - maintenance - 65536 - before_bytes
            self.assertGreater(extent, 0)
            with pressure_file.open("wb") as file:
                file.truncate(extent)
            self.assertEqual(sum(p.stat().st_size for p in self.state.rglob("*")),
                             disk_limit - paid - maintenance - 65536)
        schema_before_receipt = (self.state / "history.index").exists()
        release.set()
        worker.join(timeout=25)
        self.assertFalse(worker.is_alive(), "held request must finish within its declared deadline")
        settled = proxy.saved()
        proxy.close()
        if pressure:
            pressure_file.unlink()
        # A genuine process restart plus another real dispatch exercises pending
        # recovery; inspecting only an in-memory worker would miss lost receipts.
        restarted = Proxy(self.state, fixture, options, mode="off", wait_reviews=False)
        self.stack.callback(restarted.close)
        self.assertEqual(restarted.request("POST", "/v1/responses", body)[0], 200)
        cold = restarted.saved()
        self.assertEqual(fixture.primaries, [body, body])
        self.assertEqual(fixture.reviews, [])
        restarted.close()
        outcome = {"result": result, "pending": pending, "settled": settled,
                   "restarted": cold, "primaries": fixture.primaries,
                   "reviews": fixture.reviews, "schema_before_receipt": schema_before_receipt,
                   "paid_promise": paid, "maintenance_promise": maintenance,
                   "disk_limit": disk_limit, "ordinary_slack": 65536,
                   "logs": {p.name: p.read_text() for p in self.state.glob("fixture-*.log")}}
        self.evidence[str(pressure)] = outcome
        return outcome

    def test_first_explicit_off_receipt_survives_default_limit_pressure_and_restart(self):
        root = self.state
        for pressure in (False, True):
            with self.subTest(pressure=pressure):
                self.state = root / ("pressure" if pressure else "control")
                self.state.mkdir()
                outcome = self.explicit_paid_pressure(pressure=pressure)
                known = (100 * .10 + .50) / 1e6
                # Ordinary history publication may fail closed while the paid
                # accounting transaction succeeds. Grade native receipt facts
                # independently of that deliberate client-delivery failure.
                with self.subTest(phase="settled"):
                    self.assertEqual(outcome["settled"]["primary"]["calls"], 1)
                    self.assertEqual(outcome["settled"]["primary"]["unavailable_cost_calls"], 0)
                    self.assertAlmostEqual(outcome["settled"]["primary"]["cost_usd"], known, places=12)
                    self.assertFalse(outcome["settled"]["primary_pending"])
                with self.subTest(phase="delivery"):
                    if pressure:
                        self.assertEqual(outcome["result"], {"error": "IncompleteRead"})
                        self.assertIsNone(outcome["settled"].get("last_response_id"),
                                          "paid facts must not publish the rejected response ancestry")
                    else:
                        self.assertNotIn("error", outcome["result"])
                        self.assertEqual(outcome["result"]["response"][0], 200)
                with self.subTest(phase="restart"):
                    self.assertEqual(outcome["restarted"]["primary"]["calls"], 2)
                    self.assertEqual(outcome["restarted"]["primary"]["unavailable_cost_calls"], 0)
                    self.assertAlmostEqual(outcome["restarted"]["primary"]["cost_usd"], known * 2, places=12)
                    self.assertFalse(outcome["restarted"]["primary_pending"])

    def test_explicit_checkpoint_bound_refuses_oversize_before_paid_dispatch(self):
        fixture = self.fixture(lambda _b, n: (200, completed(response_id=f"bounded_explicit_{n}")))
        options = ["--history-max-bytes", "65536", "--history-disk-bytes", "8388608"]
        proxy = Proxy(self.state, fixture, options, mode="off", wait_reviews=False)
        self.stack.callback(proxy.close)
        small = {"model": "gpt-6-luna", "input": [
            {"role": "user", "content": "ESTABLISHED_PAID_AUTHORITY"}]}
        large = {"model": "gpt-6-luna", "input": [
            {"role": "user", "content": "EXPLICIT_CHECKPOINT_BOUND_" + "x" * 1024**2}]}
        self.assertEqual(proxy.request("POST", "/v1/responses", small)[0], 200)
        seed = proxy.saved()
        path, = self.state.glob("*.json")
        seed_bytes = path.read_bytes()
        result = proxy.request("POST", "/v1/responses", large)
        after = proxy.saved()
        proxy.close()
        self.evidence["oversize"] = {"result": result, "seed": seed, "after": after,
                                     "primaries": fixture.primaries, "reviews": fixture.reviews}
        # A safe pre-dispatch rejection is not a successfully settled paid call.
        # Keep it separate from the admitted-large pressure controls below.
        with self.subTest(phase="admission"):
            self.assertIn(result[0], (500, 507))
            self.assertEqual(fixture.primaries, [small])
        with self.subTest(phase="authority"):
            self.assertEqual(path.read_bytes(), seed_bytes)
            self.assertEqual(after["primary"], seed["primary"])
            self.assertFalse(after["primary_pending"])
            self.assertEqual(fixture.reviews, [])
            self.assertEqual(list(self.state.glob(".*.tmp")), [])
        restarted = Proxy(self.state, fixture, options, mode="off", wait_reviews=False)
        self.stack.callback(restarted.close)
        self.assertEqual(restarted.request("POST", "/v1/responses", small)[0], 200)
        cold = restarted.saved()
        self.evidence["oversize"]["restarted"] = cold
        self.evidence["oversize"]["primaries"] = fixture.primaries
        with self.subTest(phase="restart"):
            self.assertEqual(fixture.primaries, [small, small])
            self.assertEqual(cold["primary"]["calls"], 2)
            self.assertEqual(cold["primary"]["unavailable_cost_calls"], 0)
            self.assertAlmostEqual(cold["primary"]["cost_usd"], 2 * (100 * .10 + .50) / 1e6, places=12)
        restarted.close()

    def test_admitted_large_explicit_checkpoint_settles_protected_pressure_and_restart(self):
        self.admitted_large_explicit_checkpoint()

    def test_near_bound_explicit_checkpoint_never_loses_known_paid_receipt(self):
        root = self.state
        self.state = root / "calibration"
        self.state.mkdir()
        self.admitted_large_explicit_checkpoint()
        calibration = dict(self.evidence)
        # Resolve a fresh configured bound BEFORE dispatch, against the actual
        # pending representation emitted by this exact binary. No admitted
        # record or policy changes while its receipt is held upstream.
        bound = calibration["False"]["checkpoint_bytes"] + 16
        self.evidence.clear()
        self.state = root / "boundary"
        self.state.mkdir()
        try:
            self.admitted_large_explicit_checkpoint(max_record=bound, near_bound=True)
        finally:
            self.evidence["calibration"] = calibration
            self.state = root

    def admitted_large_explicit_checkpoint(self, *, max_record=2 * 1024**2, near_bound=False):
        import threading
        import http.client

        root = self.state
        for pressure in (False, True):
            with self.subTest(pressure=pressure):
                self.state = root / ("large-pressure" if pressure else "large-control")
                self.state.mkdir()
                held, release = threading.Event(), threading.Event()
                def primary(_body, n):
                    if n == 2:
                        held.set()
                        self.assertTrue(release.wait(15), "release actual admitted-large receipt")
                    return 200, completed(response_id=f"admitted_large_{n}")
                fixture = self.fixture(primary)
                self.stack.callback(release.set)
                # The SAME record bound owns SQL, explicit checkpoint admission
                # and the paid promise. This declares a legal large record; it
                # does not raise the configured cap while the request is paid.
                disk_limit = 64 * 1024**2
                options = ["--request-timeout-secs", "20", "--history-max-bytes", str(max_record),
                           "--history-disk-bytes", str(disk_limit)]
                proxy = Proxy(self.state, fixture, options, mode="off", wait_reviews=False)
                self.stack.callback(proxy.close)
                small = {"model": "gpt-6-luna", "input": [
                    {"role": "user", "content": "ESTABLISHED_PAID_AUTHORITY"}]}
                large = {"model": "gpt-6-luna", "input": [
                    {"role": "user", "content": "ADMITTED_LARGE_CHECKPOINT_" + "x" * 1024**2}]}
                self.assertEqual(proxy.request("POST", "/v1/responses", small)[0], 200)
                seed = proxy.saved()
                self.assertTrue((self.state / "history.index").is_file())
                known = (100 * .10 + .50) / 1e6
                self.assertEqual(seed["primary"]["calls"], 1)
                self.assertEqual(seed["primary"]["unavailable_cost_calls"], 0)
                self.assertAlmostEqual(seed["primary"]["cost_usd"], known, places=12)
                result = {}
                finished = threading.Event()
                seed_path, = self.state.glob("*.json")
                seed_bytes = seed_path.read_bytes()
                def send():
                    try:
                        result["response"] = proxy.request("POST", "/v1/responses", large)
                    except (http.client.HTTPException, OSError) as error:
                        result["error"] = type(error).__name__
                    finally:
                        finished.set()
                worker = threading.Thread(target=send, daemon=True)
                worker.start()
                if near_bound:
                    deadline = time.monotonic() + 10
                    while not held.is_set() and not finished.is_set() and time.monotonic() < deadline:
                        held.wait(.02)
                    if finished.is_set() and not held.is_set():
                        # Refusing an impossible settlement obligation before
                        # billing is safe, not proof that a paid receipt settled.
                        self.assertNotIn("error", result)
                        self.assertIn(result["response"][0], (500, 507))
                        self.assertEqual(fixture.primaries, [small])
                        self.assertEqual(fixture.reviews, [])
                        self.assertEqual(seed_path.read_bytes(), seed_bytes)
                        self.assertEqual(list(self.state.glob(".*.tmp")), [])
                        release.set()  # The restarted small request is now fixture arrival two.
                        worker.join(timeout=1)
                        self.assertFalse(worker.is_alive())
                        proxy.close()
                        restarted = Proxy(self.state, fixture, options, mode="off", wait_reviews=False)
                        self.stack.callback(restarted.close)
                        self.assertEqual(restarted.request("POST", "/v1/responses", small)[0], 200)
                        cold = restarted.saved()
                        restarted.close()
                        self.assertEqual(fixture.primaries, [small, small])
                        self.assertEqual(cold["primary"]["calls"], 2)
                        self.assertEqual(cold["primary"]["unavailable_cost_calls"], 0)
                        self.assertAlmostEqual(cold["primary"]["cost_usd"], 2 * known, places=12)
                        self.assertFalse(cold["primary_pending"])
                        self.evidence[str(pressure)] = {
                            "classification": "safe_before_billing_refusal", "result": result,
                            "seed": seed, "restarted": cold, "record_bound": max_record,
                            "primaries": fixture.primaries, "reviews": fixture.reviews}
                        continue
                self.assertTrue(held.wait(10), "legal large checkpoint must reach upstream")
                pending = proxy.saved()
                path, = self.state.glob("*.json")
                checkpoint_bytes = path.stat().st_size
                self.assertGreater(checkpoint_bytes, 1024**2)
                self.assertLessEqual(checkpoint_bytes, max_record)
                if near_bound:
                    self.assertLessEqual(max_record - checkpoint_bytes, 32,
                                         "exercise actual admitted pending-record boundary")
                self.assertFalse(pending["history_shared"])
                self.assertIsNone(pending.get("history_namespace"))
                self.assertTrue(pending["primary_pending"])
                self.assertEqual(pending["primary"], seed["primary"])
                self.assertEqual(fixture.primaries, [small, large])
                self.assertEqual(fixture.reviews, [])
                charge = lambda size: 4 * size + 16384
                paid = 2 * charge(max_record) + 2 * charge(128) + 2 * max_record + 65536
                maintenance = min(charge(max_record) + 65536 + charge(0), disk_limit // 2)
                before = sum(p.stat().st_size for p in self.state.rglob("*"))
                pressure_file = self.state / "ordinary-pressure.bin"
                if pressure:
                    extent = disk_limit - paid - maintenance - 65536 - before
                    self.assertGreater(extent, 0)
                    with pressure_file.open("wb") as file:
                        file.truncate(extent)
                    self.assertEqual(sum(p.stat().st_size for p in self.state.rglob("*")),
                                     disk_limit - paid - maintenance - 65536)
                release.set()
                worker.join(timeout=25)
                self.assertFalse(worker.is_alive())
                settled = proxy.saved()
                proxy.close()
                if pressure:
                    pressure_file.unlink()
                restarted = Proxy(self.state, fixture, options, mode="off", wait_reviews=False)
                self.stack.callback(restarted.close)
                self.assertEqual(restarted.request("POST", "/v1/responses", small)[0], 200)
                cold = restarted.saved()
                restarted.close()
                self.assertEqual(fixture.primaries, [small, large, small])
                self.assertEqual(fixture.reviews, [])
                self.evidence[str(pressure)] = {
                    "result": result, "seed": seed, "pending": pending, "settled": settled,
                    "restarted": cold, "primaries": fixture.primaries, "reviews": fixture.reviews,
                    "checkpoint_bytes": checkpoint_bytes, "record_bound": max_record,
                    "paid_promise": paid, "maintenance_promise": maintenance,
                    "disk_limit": disk_limit, "ordinary_slack": 65536,
                    "logs": {p.name: p.read_text() for p in self.state.glob("fixture-*.log")}}
                with self.subTest(phase="settled"):
                    self.assertEqual(settled["primary"]["calls"], 2)
                    self.assertEqual(settled["primary"]["unavailable_cost_calls"], 0)
                    self.assertAlmostEqual(settled["primary"]["cost_usd"], 2 * known, places=12)
                    self.assertFalse(settled["primary_pending"])
                with self.subTest(phase="delivery"):
                    if pressure:
                        self.assertEqual(result, {"error": "IncompleteRead"})
                        self.assertEqual(settled.get("last_response_id"), pending.get("last_response_id"),
                                         "paid fallback must not publish rejected new ancestry")
                    elif near_bound and "error" in result:
                        self.assertEqual(result, {"error": "IncompleteRead"})
                        self.assertEqual(settled.get("last_response_id"), pending.get("last_response_id"),
                                         "record-bound rejection must not publish new ancestry")
                    else:
                        self.assertNotIn("error", result)
                        self.assertEqual(result["response"][0], 200)
                with self.subTest(phase="restart"):
                    self.assertEqual(cold["primary"]["calls"], 3)
                    self.assertEqual(cold["primary"]["unavailable_cost_calls"], 0)
                    self.assertAlmostEqual(cold["primary"]["cost_usd"], 3 * known, places=12)
                    self.assertFalse(cold["primary_pending"])
        self.state = root

    def test_reviewer_receipt_growth_preserves_known_money(self):
        self.reviewer_receipt_growth()

    def test_near_bound_reviewer_receipt_growth_preserves_known_money(self):
        root = self.state
        self.state = root / "calibration"
        self.state.mkdir()
        self.reviewer_receipt_growth()
        calibration = dict(self.evidence)
        self.evidence.clear()
        self.state = root / "boundary"
        self.state.mkdir()
        try:
            self.reviewer_receipt_growth(max_record=calibration["extension-False-pressure-False"]["snapshot_bytes"] + 16,
                                        near_bound=True)
        finally:
            self.evidence["calibration"] = calibration
            self.state = root

    def reviewer_receipt_growth(self, *, max_record=256 * 1024, near_bound=False):
        import threading

        root = self.state
        for extension in ((False,) if near_bound else (False, True)):
            for pressure in (False, True):
                with self.subTest(extension=extension, pressure=pressure):
                    self.state = root / f"extension-{extension}-pressure-{pressure}"
                    self.state.mkdir()
                    held, release = threading.Event(), threading.Event()
                    replies = []
                    def reviewer(_body, _n):
                        response = advice_response({"protected": [2], "removable": [], "memories": []})
                        if extension:
                            # Valid native usage remains identical. An unrelated
                            # provider extension must not expand mandatory state
                            # beyond what was admitted before this paid call.
                            response["usage"]["provider_extension"] = "x" * (300 * 1024)
                        replies.append(response)
                        held.set()
                        if not release.wait(15):
                            raise AssertionError("release actual paid reviewer")
                        return 200, response
                    fixture = self.fixture(lambda _b, n: (200, completed(response_id=f"reviewer_growth_{n}")), reviewer)
                    self.stack.callback(release.set)
                    disk_limit = 64 * 1024**2
                    options = ["--history-max-bytes", str(max_record), "--history-disk-bytes", str(disk_limit),
                               "--review-replayed-history", "--payoff-requests", "1",
                               "--classifier-cache-policy", "disabled"]
                    proxy = TransparentProxy(self.state, fixture, options, mode="audit", wait_reviews=False)
                    self.stack.callback(proxy.close)
                    h = [{"role": "user", "content": "REVIEWER_GROWTH_GOAL"},
                         {"role": "assistant", "content": "REVIEWER_GROWTH_SOURCE " + "payload " * 20},
                         {"role": "user", "content": "REVIEWER_GROWTH_TASK"}]
                    body = {"model": "gpt-6-luna", "input": h}
                    initial = proxy.send(h, wait=False)
                    if near_bound and initial[0] in (500, 507) and not fixture.primaries and not fixture.reviews:
                        self.assertEqual(list(self.state.glob(".*.tmp")), [])
                        self.evidence[f"extension-{extension}-pressure-{pressure}"] = {
                            "classification": "safe_before_any_billing_refusal", "result": initial,
                            "record_bound": max_record, "primaries": fixture.primaries,
                            "reviews": fixture.reviews}
                        proxy.close()
                        continue
                    self.assertEqual(initial[0], 200)
                    arrived = held.wait(1 if near_bound else 10)
                    if near_bound and not arrived:
                        self.assertTrue(proxy.reviewers_settled())
                        safe = proxy.saved()
                        self.assertEqual(fixture.primaries, [body])
                        self.assertEqual(fixture.reviews, [])
                        self.assertEqual(safe["primary"]["calls"], 1)
                        self.assertEqual(safe["primary"]["unavailable_cost_calls"], 0)
                        self.assertAlmostEqual(safe["primary"]["cost_usd"], (100 * .10 + .50) / 1e6, places=12)
                        self.assertEqual(safe["shadow"]["calls"], 0)
                        self.assertFalse(safe["background_review_pending"])
                        proxy.close()
                        restarted_options = [o for o in options if o != "--review-replayed-history"]
                        restarted = TransparentProxy(self.state, fixture, restarted_options,
                                                     mode="audit", wait_reviews=False)
                        self.stack.callback(restarted.close)
                        self.assertEqual(restarted.request("GET", "/carry/dashboard/stats")[0], 200)
                        cold, = read_index(self.state / "history.index")["snapshots"].values()
                        self.assertEqual(cold["primary"], safe["primary"])
                        self.assertEqual(cold["shadow"], safe["shadow"])
                        self.assertFalse(cold["background_review_pending"])
                        restarted.close()
                        self.evidence[f"extension-{extension}-pressure-{pressure}"] = {
                            "classification": "safe_before_reviewer_billing_refusal", "settled": safe,
                            "restarted": cold, "record_bound": max_record,
                            "primaries": fixture.primaries, "reviews": fixture.reviews}
                        continue
                    self.assertTrue(arrived, "actual reviewer must dispatch before pressure")
                    pending = proxy.saved()
                    import sqlite3
                    from contextlib import closing
                    with closing(sqlite3.connect(self.state / "history.index")) as db:
                        snapshot_bytes, = db.execute("SELECT length(data) FROM records WHERE kind='snapshots'").fetchone()
                    self.assertLessEqual(snapshot_bytes, max_record)
                    if near_bound:
                        self.assertLessEqual(max_record - snapshot_bytes, 32)
                    self.assertTrue(pending["history_shared"])
                    self.assertTrue(pending["background_review_pending"])
                    self.assertEqual(pending["primary"]["calls"], 1)
                    self.assertEqual(pending["shadow"]["calls"], 0)
                    self.assertEqual(fixture.primaries, [body])
                    self.assertEqual(len(fixture.reviews), 1)
                    self.assertEqual(len(replies), 1)
                    self.assertEqual(replies[0]["usage"]["input_tokens"], 100)
                    self.assertEqual(replies[0]["usage"]["output_tokens"], 1)
                    charge = lambda size: 4 * size + 16384
                    paid = 2 * charge(max_record) + 2 * charge(128) + 2 * max_record + 65536
                    maintenance = min(charge(max_record) + 65536 + charge(0), disk_limit // 2)
                    pressure_file = self.state / "ordinary-pressure.bin"
                    before = sum(p.stat().st_size for p in self.state.rglob("*"))
                    if pressure:
                        extent = disk_limit - paid - maintenance - 65536 - before
                        self.assertGreater(extent, 0)
                        with pressure_file.open("wb") as file:
                            file.truncate(extent)
                        self.assertEqual(sum(p.stat().st_size for p in self.state.rglob("*")),
                                         disk_limit - paid - maintenance - 65536)
                    release.set()
                    deadline = time.monotonic() + 10
                    while not any(e["event"] == "background_review_merged" for e in self.events()):
                        self.assertLess(time.monotonic(), deadline, "actual reviewer worker must finish")
                        time.sleep(.02)
                    settled = proxy.saved()
                    completed_events = self.events()
                    index = read_index(self.state / "history.index")
                    proxy.close()
                    if pressure:
                        pressure_file.unlink()
                    restarted_options = [o for o in options if o != "--review-replayed-history"]
                    restarted_options += ["--review-every-requests", "100"]
                    restarted = TransparentProxy(self.state, fixture, restarted_options,
                                                 mode="audit", wait_reviews=False)
                    self.stack.callback(restarted.close)
                    restart_result = restarted.send(h, wait=False)
                    cold_index = read_index(self.state / "history.index")
                    cold, = cold_index["snapshots"].values()
                    restarted.close()
                    self.assertEqual(fixture.primaries, [body] if restart_result[0] == 507 else [body, body])
                    self.assertEqual(len(fixture.reviews), 1, "restart must not rebill reviewer")
                    key = f"extension-{extension}-pressure-{pressure}"
                    self.evidence[key] = {"pending": pending, "settled": settled, "restarted": cold,
                        "primaries": fixture.primaries, "reviews": fixture.reviews, "provider_responses": replies,
                        "restart_result": restart_result, "events": completed_events, "advice_records": index["reviews"],
                        "record_bound": max_record, "snapshot_bytes": snapshot_bytes, "disk_limit": disk_limit,
                        "paid_promise": paid, "maintenance_promise": maintenance,
                        "logs": {p.name: p.read_text() for p in self.state.glob("fixture-*.log")}}
                    known = (100 * .10 + .50) / 1e6
                    with self.subTest(phase="settled"):
                        self.assertEqual(settled["shadow"]["calls"], 1)
                        self.assertEqual(settled["shadow"]["unavailable_cost_calls"], 0)
                        self.assertAlmostEqual(settled["shadow"]["cost_usd"], known, places=12)
                        self.assertEqual(settled["shadow"]["input_tokens"], 100)
                        self.assertEqual(settled["shadow"]["output_tokens"], 1)
                        self.assertFalse(settled["background_review_pending"])
                    with self.subTest(phase="restart"):
                        # A fresh primary may itself be refused before billing
                        # at this unchanged bound; that cannot erase the already
                        # completed reviewer's known receipt.
                        self.assertIn(restart_result[0], (200, 507))
                        self.assertEqual(cold["shadow"]["calls"], 1)
                        self.assertEqual(cold["shadow"]["unavailable_cost_calls"], 0)
                        self.assertAlmostEqual(cold["shadow"]["cost_usd"], known, places=12)
                        primary_calls = len(fixture.primaries)
                        self.assertEqual(cold["primary"]["calls"], primary_calls)
                        self.assertEqual(cold["primary"]["unavailable_cost_calls"], 0)
                        self.assertAlmostEqual(cold["primary"]["cost_usd"], primary_calls * known, places=12)
                        self.assertFalse(cold["background_review_pending"])
        self.state = root

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
        index = read_index(path)
        for snapshot in index["snapshots"].values():
            snapshot["model_resolution"]["primary"]["cache"] = {
                "minimum_prefix_tokens": 1024, "max_read_breakpoints": 50,
                "max_write_breakpoints": 4, "implicit_breakpoint_uses_write_slot": True}
        write_index(path, index)

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
                index = read_index(path)
                for state in index["snapshots"].values():
                    state["request_evidence"]["scenario"] = {
                        "context_window": 200 + restored // 2, "reserve":100,
                        "keep_recent":100, "summary":50}
                write_index(path, index)
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
        index = read_index(self.state / "history.index")
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
        self.assertTrue(read_index(self.state / "history.index")["reviews"],
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
