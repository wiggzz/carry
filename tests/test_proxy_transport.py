"""Model-free HTTP regressions against the actual Carry binary.

Run directly: python3 tests/test_proxy_transport.py --binary /path/to/carry
Cargo's proxy_transport integration target invokes each case with its built binary.
All listeners are loopback; credentials are dedicated, fake and future-expiring.
"""

import argparse
import base64
import copy
import http.client
import json
import os
from pathlib import Path
import queue
import socket
import stat
import subprocess
import urllib.parse
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


LIMIT = 4 * 1024 * 1024
FRAME_COUNT = 2048
ADVICE = {"protected": [], "removable": [], "memories": []}
BINARY = None


def encoded(value):
    return json.dumps(value, separators=(",", ":")).encode()


def completed(text="fixture answer"):
    return {
        "id": "resp_transport_fixture",
        "status": "completed",
        "output": [{"type": "message", "role": "assistant", "content": [
            {"type": "output_text", "text": text}
        ]}],
        "usage": {"input_tokens": 100, "output_tokens": 10,
                  "input_tokens_details": {"cached_tokens": 0}},
    }


def frame(value):
    return b"data: " + encoded(value) + b"\n\n"


# Reuse one small frame, never concatenate the multi-MiB fixture in memory.
DELTA = frame({"type": "response.output_text.delta", "item_id": "msg_fixture",
               "output_index": 0, "content_index": 0, "delta": "x" * 2048})
PROGRESS = frame({"type": "response.in_progress", "response": {
    "id": "resp_transport_fixture", "status": "in_progress"}})
assert len(DELTA) < 4096
assert FRAME_COUNT * len(DELTA) > LIMIT


class Fixture:
    def __init__(self, primary="tiny", reviewer="tiny", codex=True):
        self.primary = primary
        self.reviewer = reviewer
        self.codex = codex
        self.requests = []
        self.errors = []
        self.lock = threading.Lock()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *args):
                pass

            def do_POST(self):
                try:
                    length = int(self.headers["Content-Length"])
                    if length > 65536:
                        raise AssertionError("fixture only accepts tiny request bodies")
                    body = json.loads(self.rfile.read(length))
                    review = body.get("prompt_cache_key") != "transport-main"
                    with fixture.lock:
                        fixture.requests.append((review, body))
                    value = completed(encoded(ADVICE).decode() if review else "fixture answer")
                    if not fixture.codex:
                        data = encoded(value)
                        self.send_response(200)
                        self.send_header("Content-Type", "application/json")
                        self.send_header("Content-Length", str(len(data)))
                        self.send_header("Connection", "close")
                        self.end_headers()
                        self.wfile.write(data)
                        self.close_connection = True
                        return
                    assert body["stream"] is True
                    assert self.headers["ChatGPT-Account-Id"] == "fixture-account"
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Transfer-Encoding", "chunked")
                    self.send_header("Connection", "close")
                    self.end_headers()

                    def chunk(data):
                        self.wfile.write(("%x\r\n" % len(data)).encode())
                        self.wfile.write(data)
                        self.wfile.write(b"\r\n")

                    shape = fixture.reviewer if review else fixture.primary
                    if shape == "terminal_then_oversized":
                        chunk(frame({"type": "response.completed", "response": value}))
                    if shape == "many":
                        for i in range(FRAME_COUNT):
                            chunk(DELTA)
                            if i % 128 == 0:
                                chunk(PROGRESS)
                    elif shape in ("oversized", "terminal_then_oversized"):
                        # One valid JSON delta frame exceeds the per-frame bound.
                        # Even a later tiny terminal must not rehabilitate it.
                        chunk(b'data: {"type":"response.output_text.delta","delta":"')
                        block = b"x" * 4096
                        for _ in range(LIMIT // len(block) + 1):
                            chunk(block)
                        chunk(b'"}\n\n')
                    if shape == "failed_terminal":
                        chunk(frame({"type": "response.failed", "response": {
                            "status": "failed", "usage": value["usage"],
                            "error": {"type": "fixture_error", "code": "fixture_failed"}}}))
                    if shape == "done_items":
                        for index, item in enumerate(value["output"]):
                            chunk(frame({"type": "response.output_item.done",
                                         "output_index": index, "item": item}))
                        value["output"] = []
                    if shape != "no_terminal":
                        terminal = frame({"type": "response.completed", "response": value})
                        if shape == "fragmented":
                            for offset in range(0, len(terminal), 7):
                                chunk(terminal[offset:offset + 7])
                        else:
                            chunk(terminal)
                    if shape == "truncated":
                        self.wfile.flush()
                        self.close_connection = True
                        return  # No chunked EOF, despite the completed terminal.
                    self.wfile.write(b"0\r\n\r\n")
                    self.close_connection = True
                except (BrokenPipeError, ConnectionResetError):
                    # Expected when old conversion closes at its aggregate cap.
                    self.close_connection = True
                except Exception as error:
                    with fixture.lock:
                        fixture.errors.append(repr(error))
                    self.close_connection = True

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def endpoint(self):
        return "http://127.0.0.1:%s/v1/responses" % self.server.server_port

    def captured(self):
        with self.lock:
            return copy.deepcopy(self.requests)

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)
        if self.errors:
            raise AssertionError("upstream fixture failed: " + repr(self.errors))


class Proxy:
    def __init__(self, fixture, mode="off", replay=False):
        self.temp = tempfile.TemporaryDirectory(prefix="carry-transport-")
        home = Path(self.temp.name)
        home.chmod(0o700)
        payload = base64.urlsafe_b64encode(encoded({
            "https://api.openai.com/auth": {"chatgpt_account_id": "fixture-account"}
        })).rstrip(b"=").decode()
        auth = home / "auth.json"
        auth.write_bytes(encoded({"version": 1,
                                 "access_token": "header." + payload + ".signature",
                                 "refresh_token": "fixture-never-refresh",
                                 "expires_at_ms": 4102444800000}))
        auth.chmod(0o600)
        args = [BINARY, "proxy", "--listen", "127.0.0.1:0",
                "--upstream-url", fixture.endpoint, "--state-dir", str(home / "state"),
                "--no-open-dashboard", "--mode", mode,
                "--request-timeout-secs", "60", "--classifier-timeout-secs", "60",
                "--bootstrap-min-input-bytes", "8388608", "--review-max-calls", "1"]
        if fixture.codex:
            args += ["--codex-login", "--codex-home", str(home)]
        if replay:
            args += ["--review-replayed-history"]
        # Do not inherit provider credentials, OAuth homes, proxy URLs or runtime
        # config. The binary can send only fake auth to our explicit loopback URL.
        env = {"HOME": str(home), "CARRY_HOME": str(home), "CODEX_HOME": str(home),
               "XDG_CONFIG_HOME": str(home / "config"),
               "XDG_CACHE_HOME": str(home / "cache"), "TMPDIR": str(home),
               "PATH": os.defpath, "NO_PROXY": "127.0.0.1,localhost",
               "CARRY_PROXY_UPSTREAM_KEY": "fixture-primary",
               "CARRY_PROXY_CLASSIFIER_KEY": "fixture-shadow"}
        self.stderr = tempfile.TemporaryFile(dir=home)
        self.process = subprocess.Popen(args, env=env, stdin=subprocess.DEVNULL,
                                        stdout=subprocess.PIPE, stderr=self.stderr)
        self.lines = queue.Queue()

        def read_stdout():
            for line in self.process.stdout:
                self.lines.put(line.decode().strip())

        self.reader = threading.Thread(target=read_stdout, daemon=True)
        self.reader.start()
        try:
            banner = self.lines.get(timeout=5)
            if not banner.startswith("CARRY_PROXY_LISTEN "):
                raise AssertionError("missing bound-listener banner: " + banner)
            self.address = banner.split(" ", 1)[1]
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                if self.process.poll() is not None:
                    raise AssertionError("Carry exited during startup")
                try:
                    if self.request("GET", "/health")[0] == 200:
                        return
                except OSError:
                    pass
                time.sleep(0.02)
            raise AssertionError("Carry health did not become ready")
        except BaseException:
            self.close()
            raise

    def request(self, method, path, body=None, headers=None, stream=False):
        host, port = self.address.rsplit(":", 1)
        connection = http.client.HTTPConnection(host, int(port), timeout=120)
        try:
            connection.request(method, path, body=body, headers=headers or {})
            response = connection.getresponse()
            if stream:
                size = 0
                tail = b""
                while True:
                    chunk = response.read(8192)
                    if not chunk:
                        break
                    size += len(chunk)
                    tail = (tail + chunk)[-4096:]
                return response.status, (size, tail)
            # Downstream JSON/error must be tiny, never capture SSE wholesale.
            data = response.read(65537)
            if len(data) > 65536:
                raise AssertionError("nonstream response unexpectedly exceeds 64KiB")
            return response.status, json.loads(data)
        finally:
            connection.close()

    def send(self, body, headers=None, stream=False):
        return self.request("POST", "/v1/responses", encoded(body),
                            {"Content-Type": "application/json",
                             "x-carry-session": "transport-fixture", **(headers or {})}, stream)

    def metrics(self):
        status, value = self.request("GET", "/carry/metrics", headers={
            "x-carry-session": "transport-fixture"})
        if status != 200 or not isinstance(value, dict):
            raise AssertionError("metrics request failed: " + repr((status, value)))
        return value

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
        try:
            self.process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=3)
        self.reader.join(timeout=2)
        self.process.stdout.close()
        self.stderr.seek(0)
        self.stderr_text = self.stderr.read(4096).decode(errors="replace")
        self.stderr.close()
        self.temp.cleanup()


def request_body():
    return {"model": "gpt-6-sol", "prompt_cache_key": "transport-main",
            "input": [{"role": "user", "content": "fixture goal"}], "stream": False}


def replay_body():
    body = request_body()
    body["input"] += [
        {"type": "function_call", "call_id": "fixture-call", "name": "native", "arguments": "{}"},
        {"type": "function_call_output", "call_id": "fixture-call", "output": "fixture tool result"},
        {"role": "user", "content": "finish"},
    ]
    return body


class TransportTests(unittest.TestCase):
    def test_startup_stdout_redacts_gateway_token_on_mapped_loopback(self):
        # Mapped IPv6 remains loopback at the OS: no LAN listener is authorized.
        # Rust's IPv6 is_loopback check nevertheless exercises the token branch.
        with socket.socket(socket.AF_INET6, socket.SOCK_STREAM) as probe:
            probe.bind(("::ffff:127.0.0.1", 0))  # IPv6 unavailable is an error, not a skip.
            port = probe.getsockname()[1]
        token = "fixture-gateway+/=%:only"
        escaped = urllib.parse.quote_plus(token)
        with tempfile.TemporaryDirectory(prefix="carry-startup-") as directory:
            home = Path(directory)
            home.chmod(0o700)
            fixture = Fixture(codex=False)
            try:
                env = {"HOME": str(home), "CARRY_HOME": str(home),
                       "XDG_CONFIG_HOME": str(home / "config"),
                       "XDG_CACHE_HOME": str(home / "cache"), "TMPDIR": str(home),
                       "PATH": os.defpath, "NO_PROXY": "127.0.0.1,localhost",
                       "CARRY_PROXY_AUTH_TOKEN": token}
                args = [BINARY, "proxy", "--listen", "[::ffff:127.0.0.1]:%s" % port,
                        "--upstream-url", fixture.endpoint,
                        "--state-dir", str(home / "state"), "--no-open-dashboard"]
                with tempfile.TemporaryFile(dir=home) as stderr:
                    process = subprocess.Popen(args, env=env, stdin=subprocess.DEVNULL,
                                               stdout=subprocess.PIPE, stderr=stderr)
                    lines = queue.Queue()

                    def collect():
                        for line in process.stdout:
                            lines.put(line.decode().strip())

                    reader = threading.Thread(target=collect, daemon=True)
                    reader.start()
                    stdout = []
                    try:
                        deadline = time.monotonic() + 30
                        while time.monotonic() < deadline:
                            try:
                                line = lines.get(timeout=0.1)
                                stdout.append(line)
                                if line.startswith("CARRY_PROXY_DASHBOARD "):
                                    break
                            except queue.Empty:
                                if process.poll() is not None:
                                    stderr.seek(0)
                                    self.fail("Carry exited before dashboard readiness: " +
                                              stderr.read(4096).decode(errors="replace"))
                        else:
                            self.fail("no CARRY_PROXY_DASHBOARD marker within 30s")
                        private_url = home / "state" / "dashboard-url"
                        self.assertEqual(stat.S_IMODE(private_url.stat().st_mode), 0o600)
                        self.assertIn("#token=" + escaped, private_url.read_text())
                        self.assertEqual(fixture.captured(), [])
                    finally:
                        process.terminate()
                        try:
                            process.wait(timeout=3)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait(timeout=3)
                        reader.join(timeout=2)
                        process.stdout.close()
                        while not lines.empty():
                            stdout.append(lines.get_nowait())
                    output = "\n".join(stdout)
                    self.assertNotIn(token, output, "stdout leaked raw gateway token")
                    self.assertNotIn(escaped, output, "stdout leaked URL-encoded gateway token")
                    dashboard = next(line for line in stdout
                                     if line.startswith("CARRY_PROXY_DASHBOARD "))
                    self.assertNotIn("#", dashboard, "public dashboard marker contains auth fragment")
            finally:
                fixture.close()

    def start(self, primary="tiny", reviewer="tiny", codex=True, replay=False):
        fixture = Fixture(primary, reviewer, codex)
        self.addCleanup(fixture.close)
        proxy = Proxy(fixture, "audit" if replay else "off", replay)
        self.addCleanup(proxy.close)
        return fixture, proxy

    def schema_wire(self):
        fixture, proxy = self.start()
        schema = {"type": "object", "properties": {
            "prompt_cache_breakpoint": {"type": "string"}},
            "required": ["prompt_cache_breakpoint"], "additionalProperties": False}
        body = request_body()
        body["prompt_cache_breakpoint"] = True
        body["input"][0]["content"] = [{"type": "input_text", "text": "fixture goal",
                                        "prompt_cache_breakpoint": True}]
        body["input"][0]["prompt_cache_breakpoint"] = {"mode": "explicit"}
        body["input"][0]["content"][0]["metadata"] = {
            "prompt_cache_breakpoint": "native block data"}
        body["metadata"] = {"prompt_cache_breakpoint": "native request data"}
        body["input"].append({"type": "function_call_output", "call_id": "opaque",
                              "output": {"prompt_cache_breakpoint": "native tool data"}})
        body["tools"] = [{"type": "function", "name": "native", "strict": True,
                          "parameters": copy.deepcopy(schema)}]
        body["text"] = {"format": {"type": "json_schema", "name": "result", "strict": True,
                                   "schema": copy.deepcopy(schema)}}
        status, response = proxy.send(body)
        self.assertEqual(status, 200, response)
        captured = fixture.captured()
        self.assertEqual(len(captured), 1)
        wire = captured[0][1]
        self.assertNotIn("prompt_cache_breakpoint", wire)
        self.assertNotIn("prompt_cache_breakpoint", wire["input"][0])
        self.assertNotIn("prompt_cache_breakpoint", wire["input"][0]["content"][0])
        self.assertEqual(wire["stream"], True)
        self.assertEqual(wire["store"], False)
        return body, wire

    def test_codex_preserves_strict_tool_parameter_property(self):
        body, wire = self.schema_wire()
        self.assertEqual(wire["tools"], body["tools"],
                         "Codex wire adapter deleted a legitimate strict tool schema property")

    def test_codex_preserves_structured_output_schema_property(self):
        body, wire = self.schema_wire()
        self.assertEqual(wire["text"], body["text"],
                         "Codex wire adapter deleted a legitimate structured output schema property")

    def test_codex_preserves_native_cache_named_data(self):
        body, wire = self.schema_wire()
        self.assertEqual(wire["metadata"], body["metadata"])
        self.assertEqual(wire["input"][0]["content"][0]["metadata"],
                         body["input"][0]["content"][0]["metadata"])
        self.assertEqual(wire["input"][1], body["input"][1])

    def test_codex_streaming_accepts_many_small_frames(self):
        fixture, proxy = self.start(primary="many")
        body = request_body()
        body["stream"] = True
        status, (size, tail) = proxy.send(body, stream=True)
        self.assertEqual(status, 200)
        self.assertGreater(size, LIMIT)
        self.assertIn(frame({"type": "response.completed", "response": completed()}), tail)
        self.assertEqual(len(fixture.captured()), 1)
        # Proves the actual observer consumed the stream, not merely byte relay.
        deadline = time.monotonic() + 3
        while True:
            metrics = proxy.metrics()
            if metrics["completed_requests"] == 1 or time.monotonic() >= deadline:
                break
            time.sleep(0.02)
        self.assertEqual(metrics["completed_requests"], 1, metrics)

    def test_codex_nonstream_accepts_many_small_frames(self):
        fixture, proxy = self.start(primary="many")
        status, response = proxy.send(request_body())
        self.assertEqual(status, 200,
                         "small terminal after >4MiB small SSE frames must convert, got " + repr(response))
        self.assertEqual(response, completed())
        self.assertEqual(len(fixture.captured()), 1)
        metrics = proxy.metrics()
        self.assertEqual((metrics["completed_requests"], metrics["primary"]["calls"],
                          metrics["primary"]["unavailable_cost_calls"]), (1, 1, 0), metrics)
        self.assertEqual((metrics["primary"]["input_tokens"], metrics["primary"]["output_tokens"]),
                         (100, 10), "conversion must not double-observe or double-commit usage")

    def test_codex_reviewer_accepts_many_small_frames(self):
        fixture, proxy = self.start(reviewer="many", replay=True)
        status, response = proxy.send(replay_body())
        self.assertEqual(status, 200, response)
        captured = fixture.captured()
        self.assertEqual(sum(review for review, _ in captured), 1,
                         "fixture must exercise a real Codex reviewer request")
        metrics = proxy.metrics()
        self.assertEqual(metrics["shadow"]["calls"], 1, metrics)
        self.assertEqual((metrics["shadow"]["input_tokens"], metrics["shadow"]["output_tokens"]),
                         (100, 10), "reviewer must count the terminal usage once")
        self.assertEqual((metrics["primary"]["calls"], metrics["completed_requests"]), (1, 1))
        self.assertEqual((metrics["invalid_reviews"], metrics["shadow"]["unavailable_cost_calls"]),
                         (0, 0), "valid tiny advice after >4MiB small frames was rejected/censored")

    def test_codex_nonstream_eof_without_terminal_is_censored(self):
        fixture, proxy = self.start(primary="no_terminal")
        status, response = proxy.send(request_body())
        self.assertEqual(status, 502, response)
        self.assertEqual(len(fixture.captured()), 1)
        metrics = proxy.metrics()
        self.assertEqual((metrics["completed_requests"], metrics["failed_primaries"],
                          metrics["primary"]["calls"], metrics["primary"]["unavailable_cost_calls"]),
                         (0, 1, 1, 1), metrics)

    def test_codex_nonstream_terminal_then_transport_failure_keeps_one_receipt(self):
        fixture, proxy = self.start(primary="truncated")
        status, response = proxy.send(request_body())
        self.assertEqual(status, 502, response)
        self.assertEqual(len(fixture.captured()), 1)
        metrics = proxy.metrics()
        self.assertEqual((metrics["completed_requests"], metrics["failed_primaries"],
                          metrics["primary"]["calls"], metrics["primary"]["unavailable_cost_calls"]),
                         (1, 0, 1, 0), metrics)

    def test_codex_nonstream_failed_terminal_cannot_be_rehabilitated(self):
        fixture, proxy = self.start(primary="failed_terminal")
        status, response = proxy.send(request_body())
        self.assertEqual(status, 502, response)
        self.assertEqual(len(fixture.captured()), 1)
        metrics = proxy.metrics()
        self.assertEqual((metrics["completed_requests"], metrics["failed_primaries"],
                          metrics["primary"]["calls"], metrics["primary"]["unavailable_cost_calls"]),
                         (0, 1, 1, 0), metrics)

    def test_codex_nonstream_fragmented_terminal_and_done_item_fallback(self):
        for shape in ("fragmented", "done_items"):
            with self.subTest(shape=shape):
                fixture, proxy = self.start(primary=shape)
                status, response = proxy.send(request_body())
                self.assertEqual(status, 200, response)
                self.assertEqual(response, completed())
                self.assertEqual(len(fixture.captured()), 1)
                deadline = time.monotonic() + 3
                while True:
                    metrics = proxy.metrics()
                    if metrics["completed_requests"] == 1 or time.monotonic() >= deadline:
                        break
                    time.sleep(0.02)
                self.assertEqual((metrics["completed_requests"], metrics["primary"]["calls"]),
                                 (1, 1), metrics)

    def test_codex_oversized_tail_censors_terminal_observation(self):
        _, proxy = self.start(primary="terminal_then_oversized")
        status, response = proxy.send(request_body())
        self.assertEqual(status, 502, response)
        deadline = time.monotonic() + 3
        while True:
            metrics = proxy.metrics()
            if metrics["failed_primaries"] == 1 or time.monotonic() >= deadline:
                break
            time.sleep(0.02)
        self.assertEqual((metrics["completed_requests"], metrics["failed_primaries"],
                          metrics["primary"]["calls"], metrics["primary"]["unavailable_cost_calls"]),
                         (0, 1, 1, 1), "invalid observation must not retain stale terminal usage")

    def test_codex_nonstream_rejects_oversized_single_frame(self):
        _, proxy = self.start(primary="oversized")
        status, response = proxy.send(request_body())
        self.assertEqual(status, 502,
                         "oversized single frame must fail closed despite a later tiny completion: " + repr(response))

    def test_codex_reviewer_rejects_oversized_single_frame(self):
        fixture, proxy = self.start(reviewer="oversized", replay=True)
        status, response = proxy.send(replay_body())
        self.assertEqual(status, 200, response)
        self.assertEqual(sum(review for review, _ in fixture.captured()), 1)
        metrics = proxy.metrics()
        self.assertEqual(metrics["shadow"]["calls"], 1, metrics)
        self.assertEqual(metrics["invalid_reviews"], 1, metrics)
        self.assertEqual(metrics["shadow"]["unavailable_cost_calls"], 1, metrics)

    def test_browser_origin_plaintext_cannot_create_upstream_calls(self):
        fixture, proxy = self.start(codex=False)
        body = request_body()
        browser_status, browser_response = proxy.send(body, headers={
            "Origin": "http://127.0.0.1:3000", "Content-Type": "text/plain"})
        browser_calls = len(fixture.captured())
        for origin, content_type in [("null", "application/json"),
                                     ("https://untrusted.invalid", "application/json")]:
            status, response = proxy.send(body, headers={"Origin": origin,
                                                        "Content-Type": content_type})
            self.assertIn(status, (400, 401, 403, 415), response)
            self.assertEqual(len(fixture.captured()), browser_calls)
        dashboard_status, (size, _) = proxy.request("GET", "/carry/dashboard", stream=True)
        self.assertEqual(dashboard_status, 200)
        self.assertGreater(size, 0)
        native_status, native_response = proxy.send(body)
        self.assertEqual(native_status, 200, native_response)
        self.assertEqual(native_response, completed())
        self.assertEqual(len(fixture.captured()) - browser_calls, 1,
                         "ordinary native JSON client must still reach upstream exactly once")
        self.assertEqual(browser_calls, 0,
                         "browser-origin text/plain Responses POST reached upstream (HTTP %s)" % browser_status)
        self.assertIn(browser_status, (400, 401, 403, 415), browser_response)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    args, tests = parser.parse_known_args()
    BINARY = str(Path(args.binary).resolve(strict=True))
    unittest.main(argv=[__file__, *tests], verbosity=2)
