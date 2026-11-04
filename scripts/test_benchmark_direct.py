"""Opt-in matched direct lane: synthetic process boundary, no Docker/model."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from scripts import swebench_smoke as worker


class TrustedDirectTests(unittest.TestCase):
    def test_direct_uses_scoped_token_and_trusted_ledger_without_sidecar(self):
        calls = []
        def execute(argv, **kwargs):
            calls.append((argv, kwargs))
            return subprocess.CompletedProcess(argv, 0, '172.20.0.2\n', '')
        with tempfile.TemporaryDirectory() as root, patch.dict(os.environ, {
                'CARRY_PROXY_MODE': 'disabled', 'BENCHMARK_TRUSTED_DIRECT': '1',
                'OPENAI_API_KEY': 'synthetic-provider-private'}):
            network = worker.start_agent_network(identity='synthetic-direct',
                proxy_image='sha256:' + '1' * 64, proxy_script=Path(root) / 'proxy.js',
                state_dir=Path(root) / 'ledger', execute=execute)
            self.assertIn('client_token', network, 'matched direct must not give agents provider keys')
            self.assertNotEqual(network['client_token'], 'synthetic-provider-private')
            self.assertNotIn('carry_proxy', network)
            gateway = next((argv, kw) for argv, kw in calls if 'node' in argv and '--detach' in argv)
            self.assertEqual(gateway[1]['env']['BENCHMARK_TRUSTED_DIRECT'], '1')
            self.assertEqual(gateway[1]['env']['BENCHMARK_PROVIDER_KEY'], 'synthetic-provider-private')
            self.assertNotIn('synthetic-provider-private', str(gateway[0]))
            self.assertTrue(Path(network['state_dir']).is_dir())


    def test_direct_handler_forwards_raw_bytes_and_prices_native_receipts(self):
        import json
        from scripts.test_proxy_gateway import Provider
        from scripts.proxy_gateway_fixture_support import Gateway
        from scripts import proxy_benchmark
        usage = {'input_tokens': 100, 'output_tokens': 5,
                 'input_tokens_details': {'cached_tokens': 20, 'cache_write_tokens': 0}}
        body = b' {"model":"gpt-6-luna", "input":[]} '
        with Provider(value={'model': 'gpt-6-luna', 'output': [], 'usage': usage}) as provider:
            gateway = Gateway(provider.server.server_port, provider.server.server_port, direct=True)
            try:
                status, _ = gateway.request(body, token='wrong')
                self.assertEqual(status, 401, 'direct gateway must reject unscoped credentials')
                self.assertFalse(provider.calls)
                status, _ = gateway.request(body, headers={'x-carry-session': 'caller-owner'})
                self.assertEqual(status, 200)
                self.assertEqual(provider.calls[0]['body'], body)
                self.assertEqual(provider.calls[0]['headers']['authorization'], 'Bearer fixture-direct-provider')
                self.assertNotIn('x-carry-session', provider.calls[0]['headers'])
            finally:
                gateway.close()
            summary = proxy_benchmark.summarize_events(gateway.events)
            self.assertTrue(summary['ledger_complete'])
            self.assertEqual(summary['estimated_total_cost_usd'], proxy_benchmark.native_cost('gpt-6-luna', usage))
            self.assertEqual(summary['shadow']['requests'], 0)
            self.assertNotIn('fixture-direct-provider', json.dumps(gateway.events))


if __name__ == '__main__':
    unittest.main()
