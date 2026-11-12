"""Synthetic actual-binary dashboard contract; no provider or live state access."""
import contextlib
import copy
from history_store import read_index, write_index
import json
import os
from pathlib import Path
import tempfile
import unittest

from proxy_state_regressions import Fixture, Proxy, advice_response, completed
import proxy_planner_marginal as marginal


class ImplicitProxy(Proxy):
    """Exercise normal implicit HTTP fork routing, not explicit-session divergence."""
    def request(self, method, path, body=None):
        import http.client
        connection = http.client.HTTPConnection(*self.address, timeout=30)
        try:
            connection.request(method, path, body=None if body is None else json.dumps(body).encode(), headers={"Content-Type": "application/json"})
            response = connection.getresponse()
            return response.status, json.loads(response.read())
        finally:
            connection.close()


class DashboardDispatchContract(unittest.TestCase):
    def test_pass_through_dispatch_does_not_count_input_only_for_dashboard(self):
        with contextlib.ExitStack() as stack:
            root = Path(stack.enter_context(tempfile.TemporaryDirectory()))
            fixture = Fixture(lambda body, n: (200, completed()),
                              lambda *_: self.fail("off mode must not dispatch a reviewer"))
            stack.callback(fixture.close)
            proxy = Proxy(root, fixture, mode="off", wait_reviews=False)
            stack.callback(proxy.close)
            source = [{"role": "user", "content": "Forward this unmeasured text unchanged."}]
            self.assertEqual(proxy.send(source)[0], 200)
            row = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]
            dispatch = row["last_primary_dispatch"]
            for field in ("caller_input_tokens", "canonical_input_tokens",
                          "continuation_input_tokens", "outgoing_input_tokens"):
                self.assertIsNone(dispatch[field], field)
            self.assertEqual(dispatch["receipt_input_tokens"], 100)
            self.assertEqual(dispatch["receipt_output_tokens"], 1)
            self.assertEqual(row["primary"]["input_tokens"], 100)
            self.assertEqual(fixture.primaries[0]["input"], source)
            self.assertEqual(fixture.errors, [])

    def test_dashboard_checkpoint_versions_and_snapshot_memories_are_not_advice(self):
        for shared in (False, True):
            with self.subTest(shared=shared), contextlib.ExitStack() as stack:
                root = Path(stack.enter_context(tempfile.TemporaryDirectory()))
                fixture = Fixture(lambda body, n: (200, completed()), lambda body, n: (200, advice_response({"protected": [], "removable": [], "memories": []})))
                stack.callback(fixture.close)
                proxy = (ImplicitProxy if shared else Proxy)(root, fixture, mode="audit", wait_reviews=False)
                stack.callback(proxy.close)
                source = [{"role": "user", "content": "Version and skipped semantic fields."}]
                self.assertEqual(proxy.send(source)[0], 200)
                owner = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]["id"]
                authority_path = root / ("history.index" if shared else owner + ".json")
                saved = read_index(authority_path)
                for version in (0, 3, 999):
                    authority = copy.deepcopy(saved)
                    session = authority["snapshots"][owner] if shared else authority
                    session["version"] = version
                    # These fields are serde-skipped: serialized snapshots are
                    # not semantic authority, even if stale facts are injected.
                    session["memories"] = [{"id": 1, "source_ids": [1], "text": "STALE_NOT_CURRENT"}]
                    frozen = json.dumps(authority).encode()
                    write_index(authority_path, authority) if shared else authority_path.write_bytes(frozen)
                    code, stats = proxy.request("GET", "/carry/dashboard/stats")
                    self.assertEqual(code, 200)
                    transcript_code, transcript = proxy.request("GET", "/carry/dashboard/transcript/" + owner)
                    if version in (0, 3):
                        self.assertEqual(stats["unavailable_sessions"], 0)
                        self.assertEqual(len(stats["sessions"]), 1)
                        self.assertEqual(transcript_code, 200)
                        self.assertEqual(transcript["canonical_input"], source)
                        self.assertEqual(transcript["current_replacement_memories"], [])
                    else:
                        self.assertEqual(stats["unavailable_sessions"], 1)
                        self.assertEqual(stats["sessions"], [])
                        self.assertNotEqual(transcript_code, 200)
                    self.assertEqual(read_index(authority_path), authority)
                    if not shared: self.assertEqual(authority_path.read_bytes(), frozen)
                self.assertEqual(fixture.errors, [])

    def test_pruned_or_unreadable_authority_cannot_resurrect_false_diagnostic_mirror(self):
        with contextlib.ExitStack() as stack:
            root = Path(stack.enter_context(tempfile.TemporaryDirectory()))
            fixture = Fixture(lambda body, n: (200, completed()), lambda body, n: (200, advice_response({"protected": [], "removable": [], "memories": []})))
            stack.callback(fixture.close)
            proxy = ImplicitProxy(root, fixture, mode="audit", wait_reviews=False)
            stack.callback(proxy.close)
            self.assertEqual(proxy.send([{"role": "user", "content": "Retained shared owner."}])[0], 200)
            owner = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]["id"]
            mirror = root / (owner + ".json")
            false_authority = json.loads(mirror.read_bytes())
            false_authority["history_shared"] = False
            false_authority["primary"]["input_tokens"] = 999999
            false_authority["history"][0]["value"] = {"role": "user", "content": "NEVER_RESURRECT_DIAGNOSTIC"}
            mirror_bytes = json.dumps(false_authority).encode()
            mirror.write_bytes(mirror_bytes)
            index_path = root / "history.index"
            expired = read_index(index_path)
            for reference in expired["tips"].values():
                reference["at"] = 0
            empty = {key: {} if isinstance(value, dict) else value for key, value in expired.items()}
            for authority in (json.dumps(expired).encode(), b"invalid actual authority", None, json.dumps(empty).encode()):
                with self.subTest(authority=None if authority is None else authority[:35]):
                    if authority is None:
                        index_path.unlink()
                    else:
                        write_index(index_path, json.loads(authority)) if authority.startswith(b"{") else index_path.write_bytes(authority)
                    code, stats = proxy.request("GET", "/carry/dashboard/stats")
                    self.assertEqual(code, 200)
                    self.assertEqual(stats["sessions"], [])
                    self.assertEqual(stats["unavailable_sessions"], 1)
                    self.assertNotEqual(proxy.request("GET", "/carry/dashboard/transcript/" + owner)[0], 200)
                    if authority is None:
                        self.assertFalse(index_path.exists())
                    else:
                        self.assertEqual(read_index(index_path), json.loads(authority)) if authority.startswith(b"{") else self.assertEqual(index_path.read_bytes(), authority)
                    self.assertEqual(mirror.read_bytes(), mirror_bytes)
            self.assertEqual(fixture.errors, [])

    def test_self_contained_explicit_checkpoint_remains_readable_without_index(self):
        with contextlib.ExitStack() as stack:
            root = Path(stack.enter_context(tempfile.TemporaryDirectory()))
            fixture = Fixture(lambda body, n: (200, completed()), lambda body, n: (200, advice_response({"protected": [], "removable": [], "memories": []})))
            stack.callback(fixture.close)
            proxy = Proxy(root, fixture, mode="audit", wait_reviews=False)
            stack.callback(proxy.close)
            source = [{"role": "user", "content": "Self-contained explicit authority."}]
            self.assertEqual(proxy.send(source)[0], 200)
            owner = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]["id"]
            checkpoint = root / (owner + ".json")
            saved = json.loads(checkpoint.read_bytes())
            self.assertFalse(saved.get("history_shared", False))
            self.assertIsNone(saved.get("history_frontier"))
            index_path = root / "history.index"
            index_path.unlink()
            for version in (0, 3, 999):
                with self.subTest(version=version):
                    saved["version"] = version
                    frozen = json.dumps(saved).encode()
                    checkpoint.write_bytes(frozen)
                    code, stats = proxy.request("GET", "/carry/dashboard/stats")
                    transcript_code, transcript = proxy.request("GET", "/carry/dashboard/transcript/" + owner)
                    self.assertEqual(code, 200)
                    if version in (0, 3):
                        self.assertEqual(stats["unavailable_sessions"], 0)
                        self.assertEqual(len(stats["sessions"]), 1)
                        self.assertEqual(transcript_code, 200)
                        self.assertEqual(transcript["canonical_input"], source)
                    else:
                        self.assertEqual(stats["sessions"], [])
                        self.assertEqual(stats["unavailable_sessions"], 1)
                        self.assertNotEqual(transcript_code, 200)
                    self.assertFalse(index_path.exists())
                    self.assertEqual(checkpoint.read_bytes(), frozen)
            self.assertEqual(fixture.errors, [])

    def test_shared_dashboard_uses_index_authority_not_a_corrupt_diagnostic_mirror(self):
        with contextlib.ExitStack() as stack:
            root = Path(stack.enter_context(tempfile.TemporaryDirectory()))
            fixture = Fixture(lambda body, n: (200, completed()), lambda body, n: (200, advice_response({"protected": [], "removable": [], "memories": []})))
            stack.callback(fixture.close)
            # This fixture corrupts authority on purpose. Join the legitimate
            # background publisher first so it cannot repair the injected bytes
            # between corruption and the read-only fail-closed assertions.
            proxy = ImplicitProxy(root, fixture, mode="audit", wait_reviews=True)
            stack.callback(proxy.close)
            source = [{"role": "user", "content": "Authoritative owned shared source."}]
            self.assertEqual(proxy.send(source)[0], 200)
            before = proxy.request("GET", "/carry/dashboard/stats")[1]
            owner = before["sessions"][0]
            mirror = root / (owner["id"] + ".json")
            saved = mirror.read_bytes()
            index = (root / "history.index").read_bytes()
            invalid_version = json.loads(saved)
            invalid_version["version"] = 999
            false_authority = json.loads(saved)
            false_authority["history_shared"] = False
            false_authority["primary"]["input_tokens"] = 999999
            for invalid in (b"not a valid diagnostic mirror", json.dumps(invalid_version).encode(), json.dumps(false_authority).encode()):
                with self.subTest(mirror=invalid[:35]):
                    mirror.write_bytes(invalid)
                    code, after = proxy.request("GET", "/carry/dashboard/stats")
                    self.assertEqual(code, 200)
                    self.assertEqual(after["unavailable_sessions"], 0)
                    self.assertEqual(after["cost_estimate"], before["cost_estimate"])
                    self.assertEqual(after["sessions"][0]["primary"], owner["primary"])
                    code, transcript = proxy.request("GET", "/carry/dashboard/transcript/" + owner["id"])
                    self.assertEqual(code, 200)
                    self.assertEqual(transcript["canonical_input"], source)
                    self.assertEqual((root / "history.index").read_bytes(), index)
            mirror.write_bytes(saved)
            # A WAL is committed authority too. With the publisher joined and
            # no outstanding readers, invalidate the entire SQLite generation,
            # not only a main file recoverable from its still-valid WAL pages.
            for suffix in ("-wal", "-shm"):
                (root / ("history.index" + suffix)).unlink(missing_ok=True)
            (root / "history.index").write_bytes(b"invalid actual authority")
            code, after = proxy.request("GET", "/carry/dashboard/stats")
            self.assertEqual(code, 200)
            self.assertEqual(after["unavailable_sessions"], 1)
            self.assertEqual(after["sessions"], [])
            self.assertNotEqual(proxy.request("GET", "/carry/dashboard/transcript/" + owner["id"])[0], 200)
            self.assertEqual((root / "history.index").read_bytes(), b"invalid actual authority")
            self.assertEqual(fixture.errors, [])

    def test_private_dashboard_transcript_needs_no_separate_login(self):
        import http.client
        with contextlib.ExitStack() as stack:
            root = Path(stack.enter_context(tempfile.TemporaryDirectory()))
            fixture = Fixture(lambda body, n: (200, completed()), lambda body, n: (200, advice_response({"protected": [], "removable": [], "memories": []})))
            stack.callback(fixture.close)
            proxy = Proxy(root, fixture, extra_env={"CARRY_PROXY_AUTH_TOKEN": "owned-transcript-token"}, wait_reviews=False)
            stack.callback(proxy.close)
            def request(method, path, token=None, body=None):
                headers = {"Content-Type": "application/json", "x-carry-session": "state-regression"}
                if token is not None:
                    headers["Authorization"] = "Bearer " + token
                connection = http.client.HTTPConnection(*proxy.address, timeout=30)
                try:
                    connection.request(method, path, body=None if body is None else json.dumps(body).encode(), headers=headers)
                    response = connection.getresponse()
                    return response.status, dict(response.getheaders()), json.loads(response.read())
                finally:
                    connection.close()
            source = [{"role": "user", "content": "Private owned transcript fixture."}]
            self.assertEqual(request("POST", "/v1/responses", "owned-transcript-token", {"model": "gpt-6-luna", "input": source})[0], 200)
            stats = proxy.request("GET", "/carry/dashboard/stats")[1]
            path = "/carry/dashboard/transcript/" + stats["sessions"][0]["id"]
            for token in (None, "invalid-owned-token"):
                with self.subTest(token=token):
                    self.assertEqual(request("GET", path, token)[0], 200)
            self.assertEqual(request("POST", "/v1/responses", "invalid-owned-token", {"model": "gpt-6-luna", "input": source})[0], 401)
            code, headers, transcript = request("GET", path, "owned-transcript-token")
            self.assertEqual(code, 200)
            self.assertEqual(transcript["canonical_input"], source)
            self.assertIsNone(transcript["last_primary_wire"])
            self.assertIsNone(transcript["last_reviewer_wire"])
            self.assertEqual(headers["cache-control"], "no-store")
            self.assertEqual(fixture.errors, [])

    def test_response_id_dispatch_reuses_only_known_caller_fragment_quantities(self):
        with contextlib.ExitStack() as stack:
            root = Path(stack.enter_context(tempfile.TemporaryDirectory()))
            fixture = Fixture(lambda body, n: (200, completed(response_id=f"resp_caller_{n}")), lambda body, n: (200, advice_response({"protected": [], "removable": [], "memories": []})))
            stack.callback(fixture.close)
            proxy = ImplicitProxy(root, fixture, wait_reviews=False)
            stack.callback(proxy.close)
            source = [{"role": "user", "content": "Retained ancestor material. " * 1000}]
            status, response = proxy.send(source)
            self.assertEqual(status, 200, response)
            first = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]["last_primary_dispatch"]
            status, response = proxy.send([{"role": "user", "content": "Continue."}], previous_response_id=response["id"])
            self.assertEqual(status, 200, response)
            row = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]["last_primary_dispatch"]
            self.assertGreater(first["caller_input_tokens"], 0)
            # The prior request counted the common overhead and this request
            # counted the append for planning. Their local quantities survive
            # reload, so telemetry can reuse them without another BPE pass or
            # copying the expanded prompt's native receipt into the fragment.
            self.assertGreater(row["caller_input_tokens"], 0)
            self.assertLess(row["caller_input_tokens"], row["outgoing_input_tokens"])
            self.assertEqual(row["canonical_input_tokens"], row["outgoing_input_tokens"])
            self.assertEqual(row["continuation_input_tokens"], row["outgoing_input_tokens"])
            self.assertGreater(row["outgoing_input_tokens"], first["receipt_input_tokens"])
            self.assertEqual(row["receipt_input_tokens"], 100)
            self.assertEqual(fixture.primaries[-1]["input"], source + [
                {"role": "user", "content": "Continue."}])
            self.assertEqual(fixture.errors, [])

    def test_private_dashboard_transcript_without_a_configured_bearer(self):
        with contextlib.ExitStack() as stack:
            root = Path(stack.enter_context(tempfile.TemporaryDirectory()))
            fixture = Fixture(lambda body, n: (200, completed()), lambda body, n: (200, advice_response({"protected": [], "removable": [], "memories": []})))
            stack.callback(fixture.close)
            proxy = Proxy(root, fixture, wait_reviews=False)
            stack.callback(proxy.close)
            self.assertEqual(proxy.send([{"role": "user", "content": "Private source."}])[0], 200)
            row = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]
            status, transcript = proxy.request("GET", "/carry/dashboard/transcript/" + row["id"])
            self.assertEqual(status, 200)
            self.assertEqual(transcript["canonical_input"], [{"role": "user", "content": "Private source."}])
            self.assertEqual(fixture.errors, [])

    def test_paid_fallback_keeps_terminal_dispatch_without_failed_response_material(self):
        import http.client
        with contextlib.ExitStack() as stack:
            root = Path(stack.enter_context(tempfile.TemporaryDirectory()))
            output = [{"type": "message", "role": "assistant", "content": [
                {"type": "output_text", "text": "UNPUBLISHED_RESPONSE " * 10000}]}]
            fixture = Fixture(lambda body, n: (200, completed(output, response_id="resp_unpublished")), lambda body, n: (200, advice_response({"protected": [], "removable": [], "memories": []})))
            stack.callback(fixture.close)
            proxy = ImplicitProxy(root, fixture, ["--history-max-bytes", "32768"], wait_reviews=False)
            stack.callback(proxy.close)
            source = [{"role": "user", "content": "Small admitted request."}]
            # Headers are already forwarded when persistence rejects completed
            # response material; Carry aborts the downstream chunked body.
            with self.assertRaises(http.client.IncompleteRead):
                proxy.send(source)
            events = [json.loads(line) for p in (root / "diagnostics").glob("*.jsonl") for line in p.read_text().splitlines()]
            failed = [event for event in events if event["event"] == "primary_checkpoint_failed"]
            self.assertEqual(len(failed), 1, events)
            self.assertEqual(failed[0]["data"], {"candidate_committed": False, "accounting_committed": True})
            saved = proxy.saved()
            self.assertEqual(saved["primary"]["calls"], 1)
            self.assertFalse(saved["primary_pending"])
            self.assertEqual(saved["last_primary_dispatch"]["outcome"], "completed")
            self.assertEqual(saved["last_primary_dispatch"]["receipt_input_tokens"], 100)
            self.assertNotEqual(saved.get("last_response_id"), "resp_unpublished")
            self.assertNotIn("UNPUBLISHED_RESPONSE", json.dumps(saved))
            proxy.close()
            proxy = ImplicitProxy(root, fixture, ["--history-max-bytes", "32768"], wait_reviews=False)
            stack.callback(proxy.close)
            row = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]
            self.assertEqual(row["last_primary_dispatch"], saved["last_primary_dispatch"])
            self.assertEqual(fixture.errors, [])
            if directory := os.environ.get("CARRY_TEST_EVIDENCE_DIR"):
                Path(directory, "paid-fallback.json").write_text(json.dumps({"saved": saved, "restarted": row, "events": events}, indent=2))

    def test_request_bound_measurements_survive_failure_and_restart(self):
        with contextlib.ExitStack() as stack:
            root = Path(stack.enter_context(tempfile.TemporaryDirectory()))
            def primary(body, n):
                return (500, {"error": {"type": "fixture_failure"}}) if n == 2 else (200, completed())
            fixture = Fixture(primary, lambda body, n: (200, advice_response({"protected": [], "removable": [], "memories": []})))
            stack.callback(fixture.close)
            proxy = Proxy(root, fixture)
            stack.callback(proxy.close)
            source = [{"role": "user", "content": "Bound measurement fixture."}]
            self.assertEqual(proxy.send(source)[0], 200)
            before = proxy.saved()
            stats = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]
            record = stats.get("last_primary_dispatch")
            self.assertIsInstance(record, dict, "actual primary boundary must produce a lane-bound numeric record")
            self.assertEqual(record["lane"], "primary")
            self.assertEqual(record.get("model"), "gpt-6-luna", "dispatch model must be frozen with the measured body")
            self.assertEqual(record["outcome"], "completed")
            self.assertGreater(record["outgoing_input_tokens"], 0)
            self.assertEqual(record["caller_input_tokens"], record["canonical_input_tokens"])
            self.assertEqual(record["receipt_input_tokens"], before["primary"]["input_tokens"])
            self.assertEqual(stats["rewrites"]["dispatched"], 0)
            self.assertFalse(stats["rewrites"]["prior_unknown"])
            self.assertEqual(proxy.send(source)[0], 500)
            failed = proxy.saved()["last_primary_dispatch"]
            self.assertEqual(failed["outcome"], "failed")
            self.assertIsNone(failed["receipt_input_tokens"])
            self.assertNotEqual(record["request_id"], failed["request_id"])
            proxy.close()
            proxy = Proxy(root, fixture)
            stack.callback(proxy.close)
            self.assertEqual(proxy.saved()["last_primary_dispatch"], failed)
            self.assertEqual(proxy.send(source)[0], 200)
            stats = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]
            self.assertEqual(stats["rewrites"]["dispatched"], 0)
            self.assertEqual(stats["planner"]["candidates"], proxy.saved()["last_plan"]["candidates"])
            status, transcript = proxy.request("GET", "/carry/dashboard/transcript/" + stats["id"])
            self.assertEqual(status, 200)
            self.assertEqual(transcript["canonical_input"], source)
            self.assertIsNone(transcript["last_primary_wire"])
            self.assertIsNone(transcript["last_reviewer_wire"])
            self.assertEqual(fixture.errors, [])
            directory = os.environ.get("CARRY_TEST_EVIDENCE_DIR")
            if directory:
                Path(directory, "dispatch-contract.json").write_text(json.dumps({"stats": stats, "failed": failed, "transcript": transcript}, indent=2))

    def test_gated_reviewer_measurement_cannot_overwrite_newer_primary(self):
        with contextlib.ExitStack() as stack:
            root = Path(stack.enter_context(tempfile.TemporaryDirectory()))
            def review(body, n):
                fixture.held.set()
                self.assertTrue(fixture.release.wait(10))
                return 200, advice_response({"protected": [], "removable": [], "memories": []})
            def primary(body, n):
                response = completed(response_id=f"resp_dashboard_{n}")
                response["usage"]["input_tokens"] = 100 * n
                return 200, response
            fixture = Fixture(primary, review)
            stack.callback(fixture.close)
            proxy = Proxy(root, fixture, wait_reviews=False)
            stack.callback(proxy.close)
            source = [{"role": "user", "content": "Concurrent boundary fixture."}, {"role": "assistant", "content": "Reviewable obsolete answer."}, {"role": "user", "content": "Continue."}]
            self.assertEqual(proxy.send(source)[0], 200)
            self.assertEqual(proxy.send(source)[0], 200)
            self.assertTrue(fixture.held.wait(5))
            first = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]
            self.assertTrue(first["background_review_pending"])
            self.assertEqual(first["shadow"]["calls"], 0)
            self.assertEqual(first["last_reviewer_dispatch"]["outcome"], "dispatched")
            self.assertIsNone(first["last_reviewer_dispatch"]["receipt_input_tokens"])
            self.assertIsNotNone(first["cost_estimate"]["savings"])
            self.assertEqual(proxy.send(source)[0], 200)
            second = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]
            self.assertEqual(second["last_primary_dispatch"]["receipt_input_tokens"], 300)
            # Dispatch quantities use the preceding receipt, and stay frozen
            # when this request's different native usage arrives afterwards.
            self.assertEqual(first["last_primary_dispatch"]["outgoing_input_tokens"], 100)
            self.assertEqual(first["last_primary_dispatch"]["receipt_input_tokens"], 200)
            self.assertEqual(second["last_primary_dispatch"]["outgoing_input_tokens"], 200)
            fixture.release.set()
            import time
            deadline = time.monotonic() + 10
            while not proxy.reviewers_settled():
                self.assertLess(time.monotonic(), deadline)
                time.sleep(.02)
            for _ in range(3):
                settled = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]
                self.assertEqual(settled["last_primary_dispatch"], second["last_primary_dispatch"])
                self.assertEqual(settled["last_reviewer_dispatch"]["outcome"], "completed")
                self.assertEqual(settled["last_reviewer_dispatch"]["receipt_input_tokens"], 100)
                self.assertEqual(settled["shadow"]["calls"], 1)
            self.assertEqual(fixture.errors, [])
            if directory := os.environ.get("CARRY_TEST_EVIDENCE_DIR"):
                Path(directory, "concurrent-dispatch.json").write_text(json.dumps({"held": first, "second": second, "settled": settled}, indent=2))

    def test_legacy_count_is_unknown_and_fork_does_not_inherit_sibling_measurement(self):
        with contextlib.ExitStack() as stack:
            root = Path(stack.enter_context(tempfile.TemporaryDirectory()))
            fixture = Fixture(lambda body, n: (200, completed(response_id=f"resp_fork_{n}")), lambda body, n: (200, advice_response({"protected": [], "removable": [], "memories": []})))
            stack.callback(fixture.close)
            proxy = ImplicitProxy(root, fixture, wait_reviews=False)
            stack.callback(proxy.close)
            source = [{"role": "user", "content": "Shared root"}, {"role": "user", "content": "Sibling A"}]
            self.assertEqual(proxy.send(source)[0], 200)
            proxy.close()
            def without_selection_history(saved):
                # Supported old evidence has successful receipts but no
                # applied-selection facts. Do not reconstruct them from sizes
                # or accept the obsolete accumulated counter as proof.
                saved["rewrites"] = {"dispatched": 99, "forced": 99, "prior_unknown": False}
                evidence = saved["request_evidence"]
                evidence["observations"] = [o for o in evidence["observations"]
                                            if not isinstance(o, dict) or "Dispatch" not in o]
                for key in ("archive", "numeric_pending", "numeric_primary_items"):
                    evidence.pop(key, None)
                return saved
            for checkpoint in root.glob("*.json"):
                saved = json.loads(checkpoint.read_text())
                without_selection_history(saved)
                checkpoint.write_text(json.dumps(saved))
            index_path = root / "history.index"
            index = read_index(index_path)
            for saved in index["snapshots"].values():
                without_selection_history(saved)
            write_index(index_path, index)
            proxy = ImplicitProxy(root, fixture, wait_reviews=False)
            stack.callback(proxy.close)
            self.assertEqual(proxy.send(source)[0], 200)
            old = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]
            self.assertTrue(old["rewrites"]["prior_unknown"])
            self.assertEqual(old["rewrites"]["dispatched"], 0)
            branch = [source[0], {"role": "user", "content": "Sibling B"}]
            self.assertEqual(proxy.send(branch)[0], 200)
            rows = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"]
            self.assertEqual(len(rows), 2)
            self.assertEqual(next(s for s in rows if s["id"] == old["id"])["last_primary_dispatch"], old["last_primary_dispatch"])
            fork = next(s for s in rows if s["id"] != old["id"])
            self.assertFalse(fork["rewrites"]["prior_unknown"])
            self.assertEqual(fork["completed_requests"], 1)
            self.assertNotEqual(fork["last_primary_dispatch"]["request_id"], old["last_primary_dispatch"]["request_id"])
            self.assertEqual(fixture.errors, [])

    def test_topline_compares_available_whole_session_windows_with_explicit_exclusions(self):
        for unknown_first in (False, True):
            with self.subTest(unknown_first=unknown_first), contextlib.ExitStack() as stack:
                root = Path(stack.enter_context(tempfile.TemporaryDirectory()))
                def primary(body, n):
                    response = completed(response_id=f"resp_scope_{n}", input_tokens=1000)
                    if (n == 1) == unknown_first:
                        response.pop("usage")
                    return 200, response
                fixture = Fixture(primary, lambda *_: self.fail("unexpected reviewer"))
                stack.callback(fixture.close)
                options = ["--review-min-primary-tokens", "1000000", "--review-every-requests", "100"]
                proxy = ImplicitProxy(root, fixture, options, wait_reviews=False)
                stack.callback(proxy.close)
                for label in ("first unrelated history", "second unrelated history"):
                    self.assertEqual(proxy.send([{"role": "user", "content": label}])[0], 200)
                stats = proxy.request("GET", "/carry/dashboard/stats")[1]
                self.assertEqual(len(stats["sessions"]), 2)
                known = [s for s in stats["sessions"] if s["cost_estimate"]["whole_window"] is not None]
                unknown = [s for s in stats["sessions"] if s["cost_estimate"]["whole_window"] is None]
                self.assertEqual(len(known), 1)
                self.assertEqual(len(unknown), 1)
                self.assertEqual(known[0]["primary"]["calls"], 1)
                self.assertEqual(unknown[0]["primary"]["unavailable_cost_calls"], 1)
                self.assertEqual(len(fixture.primaries), 2)
                self.assertEqual(fixture.errors, [])
                if directory := os.environ.get("CARRY_TEST_EVIDENCE_DIR"):
                    Path(directory, f"comparison-scope-{unknown_first}.json").write_text(json.dumps(stats, indent=2))
                total = stats["cost_estimate"]
                self.assertIsNotNone(total["whole_window"],
                                     "an unavailable peer must not erase complete session-window estimates")
                self.assertEqual(total["whole_window_included_sessions"], 1)
                self.assertEqual(total["whole_window_unavailable_sessions"], 1)
                self.assertEqual(total["whole_window_scope"], "available_session_windows_only")
                for field in ("carry_cost", "traditional_cost", "savings", "savings_percent"):
                    self.assertAlmostEqual(total["whole_window"][field],
                                           known[0]["cost_estimate"]["whole_window"][field], places=12)
                proxy.close()
                proxy = ImplicitProxy(root, fixture, options, wait_reviews=False)
                stack.callback(proxy.close)
                restarted = proxy.request("GET", "/carry/dashboard/stats")[1]["cost_estimate"]
                self.assertEqual(restarted, total)

    def test_opaque_cleanup_counts_selection_even_when_dispatch_size_is_unknown(self):
        with contextlib.ExitStack() as stack:
            root = Path(stack.enter_context(tempfile.TemporaryDirectory()))
            opaque = [{"type": "reasoning", "id": f"rs_selection_{i}", "summary": [],
                       "encrypted_content": f"synthetic-selection-{i}/" * 100} for i in range(2)]

            def primary(body, n):
                tokens = 3000 + sum(len(i.get("content", "")) // 3
                                    for i in body["input"]) + min(n - 1, 2) * 1000
                return 200, completed(opaque[n - 1:n] if n <= 2 else [],
                                      response_id=f"resp_selection_{n}", input_tokens=tokens)

            fixture = Fixture(primary, lambda body, n: (200, advice_response(
                {"protected": [], "removable": [2] if n == 1 else [], "memories": []})))
            stack.callback(fixture.close)
            options = ["--primary-context-tokens", "1000000", "--payoff-requests", "5",
                       "--classifier-cache-policy", "disabled"]
            proxy = Proxy(root, fixture, options)
            stack.callback(proxy.close)
            seed = [{"role": "user", "content": "Preserve the goal"},
                    {"role": "assistant", "content": "obsolete source " * 3000},
                    {"role": "user", "content": "Continue"}]
            self.assertEqual(proxy.send(seed)[0], 200)
            history = seed + [opaque[0], {"role": "user", "content": "Next"}]
            self.assertEqual(proxy.send(history)[0], 200)
            history += [opaque[1], {"role": "user", "content": "Next again"}]
            self.assertEqual(proxy.send(history)[0], 200)
            row = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]
            self.assertNotIn(seed[1], fixture.primaries[-1]["input"])
            self.assertIn(opaque[1], fixture.primaries[-1]["input"])
            self.assertIsNotNone(row["last_primary_dispatch"]["outgoing_input_tokens"])
            self.assertEqual(row["last_primary_dispatch"]["continuation_input_tokens"],
                             row["planner"]["primary_estimated_tokens"])
            self.assertGreater(row["last_primary_dispatch"]["continuation_input_tokens"],
                               row["last_primary_dispatch"]["outgoing_input_tokens"])
            self.assertGreater(row["context"]["removed_items"], 0)
            self.assertEqual(row["primary"]["calls"], 3)
            self.assertEqual(fixture.errors, [])
            if directory := os.environ.get("CARRY_TEST_EVIDENCE_DIR"):
                Path(directory, "opaque-selection.json").write_text(json.dumps(
                    {"row": row, "wires": fixture.primaries}, indent=2))
            self.assertEqual(row["rewrites"]["dispatched"], 1,
                             "a newly dispatched cleanup exists independently of token sizing")
            self.assertFalse(row["rewrites"]["prior_unknown"])
            # A newly appended image has no comparable native allocation or
            # reasoning-growth forecast. Unknown sizes must remain unknown,
            # without erasing an already dispatched, still-applied cleanup.
            history.append({"role": "user", "content": [
                {"type": "input_image", "image_url": "data:image/png;base64,synthetic"}]})
            self.assertEqual(proxy.send(history)[0], 200)
            row = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]
            self.assertNotIn(seed[1], fixture.primaries[-1]["input"])
            self.assertIsNone(row["last_primary_dispatch"]["outgoing_input_tokens"])
            self.assertIsNone(row["last_primary_dispatch"]["continuation_input_tokens"])
            self.assertEqual(row["rewrites"]["dispatched"], 1)
            self.assertEqual(row["primary"]["calls"], 4)
            before = copy.deepcopy(row["primary"])
            proxy.close()
            proxy = Proxy(root, fixture, options)
            stack.callback(proxy.close)
            for _ in range(2):
                self.assertEqual(proxy.send(history)[0], 200)
                row = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]
                self.assertEqual(row["rewrites"]["dispatched"], 1,
                                 "repeated applied omissions are not another cleanup")
            self.assertEqual(row["primary"]["calls"], before["calls"] + 2)
            self.assertEqual(fixture.errors, [])

    def test_forced_shrink_is_counted_at_failed_attempted_dispatch(self):
        with contextlib.ExitStack() as stack:
            root = Path(stack.enter_context(tempfile.TemporaryDirectory()))
            def primary(body, n):
                if n == 3:
                    return 500, {"error": "synthetic attempt failure"}
                response = completed(response_id=f"resp_forced_{n}")
                # The seed fits 4,096 tokens; its next ~2,400-token append
                # requires the authorized shrink. A 100-token fixture receipt
                # would contradict the source size and make subtraction unknown.
                response["usage"]["input_tokens"] = 2500
                return 200, response
            fixture = Fixture(primary, lambda body, n: (200, advice_response({"protected": [], "removable": [2], "memories": []})))
            stack.callback(fixture.close)
            proxy = Proxy(root, fixture, ["--primary-context-tokens", "4096", "--min-payback-percent", "100", "--payoff-requests", "1"])
            stack.callback(proxy.close)
            seed = [{"role": "user", "content": "Keep goal"}, {"role": "assistant", "content": "obsolete material " * 1200}, {"role": "user", "content": "Current task"}]
            self.assertEqual(proxy.send(seed, max_output_tokens=1)[0], 200)
            self.assertEqual(proxy.send(seed, max_output_tokens=1)[0], 200)
            self.assertEqual(proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]["rewrites"]["dispatched"], 0)
            growth = seed + [{"role": "user", "content": "additional source " * 1200}]
            status, result = proxy.send(growth, max_output_tokens=1)
            self.assertEqual(status, 500, result)
            saved = proxy.saved()
            self.assertTrue(saved["last_plan"]["forced"])
            rewrites = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]["rewrites"]
            self.assertNotIn("rewrites", saved, "counts must be derived, not a competing saved truth")
            self.assertEqual(rewrites["dispatched"], 1)
            self.assertEqual(rewrites["forced"], 1)
            self.assertEqual(saved["last_primary_dispatch"]["outcome"], "failed")
            self.assertLess(saved["last_primary_dispatch"]["outgoing_input_tokens"], saved["last_primary_dispatch"]["continuation_input_tokens"])
            self.assertEqual(fixture.errors, [])
            if directory := os.environ.get("CARRY_TEST_EVIDENCE_DIR"):
                Path(directory, "forced-failed-dispatch.json").write_text(json.dumps(saved, indent=2))

    def test_second_shrink_then_audit_restoration_does_not_add_a_rewrite(self):
        with contextlib.ExitStack() as stack:
            root = Path(stack.enter_context(tempfile.TemporaryDirectory()))
            def review(body, n):
                removable = [2] if n == 1 else ([4] if n == 2 else [])
                return 200, advice_response({"protected": [], "removable": removable, "memories": []})
            def primary(body, n):
                response = completed(response_id=f"resp_second_{n}")
                # Synthetic native metering varies with the actual projected
                # input, leaving a positive quantity after either source drops.
                response["usage"]["input_tokens"] = 1000 + sum(
                    len(item.get("content", "")) // 3 for item in body["input"])
                return 200, response
            fixture = Fixture(primary, review)
            stack.callback(fixture.close)
            options = ["--payoff-requests", "5", "--classifier-cache-policy", "disabled"]
            proxy = Proxy(root, fixture, options)
            stack.callback(proxy.close)
            history = [{"role": "user", "content": "Goal"},
                       {"role": "assistant", "content": "first obsolete material " * 3000},
                       {"role": "user", "content": "Next"},
                       {"role": "assistant", "content": "second obsolete material " * 3000},
                       {"role": "user", "content": "Current task"}]
            counts = []
            for _ in range(5):
                status, result = proxy.send(history)
                self.assertEqual(status, 200, result)
                counts.append(proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]["rewrites"]["dispatched"])
            self.assertEqual(counts, [0, 0, 1, 2, 2])
            self.assertNotIn(history[1], fixture.primaries[-1]["input"])
            self.assertNotIn(history[3], fixture.primaries[-1]["input"])
            proxy.close()
            proxy = Proxy(root, fixture, options, mode="audit")
            stack.callback(proxy.close)
            self.assertEqual(proxy.send(history)[0], 200)
            self.assertEqual(fixture.primaries[-1]["input"], history)
            restored = proxy.saved()
            rewrites = proxy.request("GET", "/carry/dashboard/stats")[1]["sessions"][0]["rewrites"]
            self.assertEqual(rewrites["dispatched"], 2, "restoring source is not a cleanup")
            self.assertEqual(fixture.errors, [])
            if directory := os.environ.get("CARRY_TEST_EVIDENCE_DIR"):
                Path(directory, "second-shrink-restoration.json").write_text(json.dumps({"counts": counts, "restored": restored}, indent=2))

    def test_distinct_rewrites_use_real_continuation_across_restart_and_append(self):
        tracer = marginal.PlannerMarginalBaseline("test_tiny_edit_cannot_beat_a_cheaper_valid_warm_continuation")
        tracer.setUp()
        try:
            evidence = tracer.run_arm("dashboard_rewrites", False, restart=True, followups=True)
            state = evidence["final_checkpoint"]
            rewrites = evidence["final_stats"]["rewrites"]
            self.assertNotIn("rewrites", state, "filtered requests are not a second saved counter")
            self.assertEqual(rewrites["dispatched"], 1, "initial cleanup only; repeated projection, restart and append add none")
            self.assertEqual(rewrites["forced"], 0)
            self.assertFalse(rewrites["prior_unknown"])
            self.assertEqual(state["last_reviewer_dispatch"]["lane"], "reviewer")
            self.assertEqual(state["last_reviewer_dispatch"]["outcome"], "completed")
            self.assertGreater(state["last_reviewer_dispatch"]["outgoing_input_tokens"], 0)
        finally:
            tracer.doCleanups()


if __name__ == "__main__":
    unittest.main()
