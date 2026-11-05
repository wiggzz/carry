"""Model-free paid-admission regressions through the actual gateway."""
import json
from pathlib import Path
import tempfile
import unittest

from scripts.proxy_gateway_fixture_support import Gateway
from scripts.test_proxy_gateway import Provider


class DispatchBudgetTests(unittest.TestCase):
    def test_escrow_survives_missing_capture_and_shares_cross_lane_remainder(self):
        import importlib.util
        self.assertIsNotNone(importlib.util.find_spec('scripts.benchmark_budget'),
                             'durable shared spending owner is missing')
        from scripts.benchmark_budget import SpendingAuthority
        with tempfile.TemporaryDirectory() as root:
            state = Path(root) / 'authority.json'
            SpendingAuthority.create(state, cap_usd='10', cloud_reserve_usd='1',
                cloud_bound={'evidence_kind': 'synthetic-fixture', 'rate_usd_per_hour': '1',
                             'maximum_seconds': 3600, 'cleanup_owner': 'owned-fixture'})
            owner = SpendingAuthority(state)
            lease = owner.reserve('swe-direct')
            self.assertEqual(lease['pico_usd'], '9000000000000')
            with self.assertRaisesRegex(ValueError, 'outstanding'):
                SpendingAuthority(state).reserve('scb-compact')
            with self.assertRaises((ValueError, FileNotFoundError)):
                owner.settle(lease, Path(root) / 'missing.jsonl')
            with self.assertRaisesRegex(ValueError, 'outstanding'):
                SpendingAuthority(state).reserve('swe-compact')
            journal = Path(root) / 'journal.jsonl'
            usage = {'input_tokens': 100, 'output_tokens': 2,
                     'input_tokens_details': {'cached_tokens': 0}}
            journal.write_text('\n'.join(json.dumps(e) for e in [
                {'event': 'escrow', 'pico_usd': lease['pico_usd'], 'lease_id': lease['id']},
                {'event': 'reserved', 'request_id': 'x', 'model': 'gpt-6-luna',
                 'pico_usd': '358500000000'},
                {'event': 'completed', 'request_id': 'x', 'model': 'gpt-6-luna',
                 'service_tier': None, 'usage': usage}]) + '\n')
            owner.settle(lease, journal)
            next_lease = SpendingAuthority(state).reserve('scb-compact')
            self.assertEqual(next_lease['pico_usd'], '8999989000000')

    def test_actual_gateway_journals_settle_once_across_lane_restart(self):
        from scripts.benchmark_budget import SpendingAuthority
        with tempfile.TemporaryDirectory() as root:
            state = Path(root)/'authority'
            SpendingAuthority.create(state, cap_usd='10', cloud_reserve_usd='1',
                cloud_bound={'evidence_kind': 'synthetic-fixture', 'rate_usd_per_hour': '1',
                             'maximum_seconds': 3600, 'cleanup_owner': 'owned-fixture'})
            usage = {'input_tokens': 100, 'output_tokens': 2,
                     'input_tokens_details': {'cached_tokens': 0}}
            for lane in ('swe-direct', 'scb-compact'):
                owner = SpendingAuthority(state)
                lease = owner.reserve(lane)
                journal = Path(root)/(lane+'.jsonl')
                with Provider(value={'model': 'gpt-6-luna', 'output': [], 'usage': usage}) as provider:
                    gateway = Gateway(provider.server.server_port, provider.server.server_port, direct=True,
                        expected_models=('gpt-6-luna', 'gpt-6.1-sol'),
                        budget={'id': lease['id'], 'limit': lease['pico_usd'], 'journal': journal})
                    try:
                        self.assertEqual(gateway.request(b'{"model":"gpt-6-luna","input":[]}')[0], 200)
                        self.assertEqual(len(provider.calls), 1)
                    finally:
                        gateway.close()
                    self.assertEqual(gateway.process.returncode, 0)
                owner.settle(lease, journal)
                with self.assertRaisesRegex(ValueError, 'lease mismatch'):
                    owner.settle(lease, journal)
            result = json.loads(state.read_text())
            self.assertEqual(result['settled_pico_usd'], '22000000')
            self.assertEqual(result['available_pico_usd'], '8999978000000')
            self.assertIsNone(result['active'])
            self.assertEqual([row['identity'] for row in result['history']], ['swe-direct', 'scb-compact'])

    def test_previous_gateway_journal_cannot_refund_new_lease(self):
        from scripts.benchmark_budget import SpendingAuthority
        with tempfile.TemporaryDirectory() as root:
            state, journal = Path(root)/'authority', Path(root)/'old-journal'
            SpendingAuthority.create(state, cap_usd='10', cloud_reserve_usd='0',
                cloud_bound={'evidence_kind': 'synthetic-fixture', 'rate_usd_per_hour': '0',
                             'maximum_seconds': 3600, 'cleanup_owner': 'owned-fixture'})
            owner = SpendingAuthority(state)
            first = owner.reserve('swe-direct')
            journal.write_text(json.dumps({'event': 'escrow', 'pico_usd': first['pico_usd'], 'lease_id': first['id']})+'\n')
            owner.settle(first, journal)
            second = owner.reserve('scb-direct')
            with self.assertRaisesRegex(ValueError, 'escrow mismatch'):
                owner.settle(second, journal)
            self.assertEqual(json.loads(state.read_text())['active'], second)

    def test_network_wires_same_escrow_and_primary_backhaul_before_provider_calls(self):
        import os
        import subprocess
        from unittest.mock import patch
        from scripts import swebench_smoke as worker
        from scripts.benchmark_budget import SpendingAuthority
        for mode in ('disabled', 'compact'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as root:
                state = Path(root) / 'authority.json'
                SpendingAuthority.create(state, cap_usd='10', cloud_reserve_usd='1',
                    cloud_bound={'evidence_kind': 'synthetic-fixture', 'rate_usd_per_hour': '1',
                                 'maximum_seconds': 3600, 'cleanup_owner': 'owned-fixture'})
                calls = []
                def execute(argv, **kwargs):
                    calls.append((argv, kwargs))
                    if 'inspect' in argv and '--format' not in argv:
                        kind = 'network' if 'network' in argv else 'container'
                        return subprocess.CompletedProcess(argv, 1, '', 'Error: No such '+kind+': '+argv[-1])
                    return subprocess.CompletedProcess(argv, 0, '172.20.0.2\n', '')
                with patch.dict(os.environ, {'CARRY_PROXY_MODE': mode, 'BENCHMARK_HARNESS': 'codex',
                        'BENCHMARK_TRUSTED_DIRECT': '1' if mode == 'disabled' else '0',
                        'OPENAI_API_KEY': 'fixture-secret', 'BENCHMARK_BUDGET_STATE': str(state)}), \
                        patch.object(worker.proxy_benchmark, 'read_metrics', return_value={'mode': mode}):
                    network = worker.start_agent_network(identity='owned-budget-'+mode,
                        proxy_image='sha256:'+'1'*64, carry_image='sha256:'+'2'*64,
                        proxy_script=Path(worker.__file__).with_name('openai_proxy.js'),
                        state_dir=Path(root)/'capture', execute=execute, expected_primary_model='gpt-6-luna')
                    gateway = next((a, k) for a, k in calls if 'node' in a and '--detach' in a)
                    self.assertEqual(gateway[1]['env'].get('BENCHMARK_BUDGET_PICO_USD'), '9000000000000')
                    self.assertIn('dst=/budget', ' '.join(gateway[0]))
                    self.assertIsNotNone(json.loads(state.read_text())['active'])
                    if mode == 'compact':
                        sidecar = next((a, k) for a, k in calls if '--upstream-url' in a)
                        self.assertEqual(sidecar[0][sidecar[0].index('--upstream-url')+1],
                                         'http://openai-proxy:8080/v1/responses')
                        self.assertNotEqual(sidecar[1]['env']['CARRY_PROXY_UPSTREAM_KEY'], 'fixture-secret')
                    # A missing journal after proven cleanup must retain accepted exposure.
                    with self.assertRaises((ValueError, FileNotFoundError)):
                        worker.cleanup_agent_network(network, execute=execute)
                    self.assertIsNotNone(json.loads(state.read_text())['active'])

    def test_paid_dispatch_without_shared_authority_never_calls_runner(self):
        from unittest.mock import patch
        from scripts import benchmark_comparison as comparison
        from scripts.test_benchmark_comparison import contract
        calls = []
        frozen = contract()
        plan = comparison.build_plan(frozen)
        with tempfile.TemporaryDirectory() as root, patch.dict('os.environ', {}, clear=True):
            with self.assertRaisesRegex(ValueError, 'shared spending authority'):
                comparison.dispatch(plan, frozen, {}, Path(root) / 'run',
                    lambda *args: calls.append(args) or [], budget_usd=10)
            self.assertEqual(calls, [])


class GatewayBudgetTests(unittest.TestCase):
    def test_unobserved_remote_input_is_not_authorized_as_text(self):
        with tempfile.TemporaryDirectory() as root, Provider() as provider:
            gateway = Gateway(provider.server.server_port, provider.server.server_port, direct=True,
                expected_models=('gpt-6-luna', 'gpt-6.1-sol'),
                budget={'limit': 1_000_000_000_000, 'journal': Path(root)/'journal'})
            try:
                for change in ({'previous_response_id': 'unobserved'},
                               {'conversation': 'unobserved'},
                               {'input': [{'type': 'item_reference', 'id': 'unobserved'}]}):
                    body = json.dumps({'model': 'gpt-6-luna', 'input': [], **change}).encode()
                    self.assertEqual(gateway.request(body)[0], 400)
                self.assertEqual(provider.calls, [])
            finally:
                gateway.close()

    def test_unpriced_requests_and_one_pico_short_limit_never_forward(self):
        with tempfile.TemporaryDirectory() as root, Provider() as provider:
            gateway = Gateway(provider.server.server_port, provider.server.server_port, direct=True,
                expected_models=('gpt-6-luna', 'gpt-6.1-sol'),
                budget={'limit': 358_499_999_999, 'journal': Path(root)/'journal'})
            try:
                for change in ({'service_tier': 'priority'}, {'service_tier': 'auto'},
                               {'max_output_tokens': 128001}, {'max_output_tokens': 0},
                               {'background': True}, {'tools': [{'type': 'web_search'}]},
                               {'model': 'unknown'}):
                    body = json.dumps({'model': 'gpt-6-luna', 'input': [], **change}).encode()
                    self.assertIn(gateway.request(body)[0], (400, 403))
                self.assertEqual(gateway.request(b'{"model":"gpt-6-luna","input":[]}')[0], 402)
                self.assertEqual(provider.calls, [])
            finally:
                gateway.close()

    def test_overlapping_roles_reserve_before_either_receipt(self):
        import threading
        arrived, release = threading.Event(), threading.Event()
        usage = {'input_tokens': 10, 'output_tokens': 2, 'input_tokens_details': {'cached_tokens': 0}}
        with tempfile.TemporaryDirectory() as root, Provider(value={
                'model': 'gpt-6-luna', 'output': [], 'usage': usage}) as provider:
            original = provider.server.RequestHandlerClass.send_response
            def held(handler, *args, **kwargs):
                arrived.set()
                if not release.wait(2):
                    raise AssertionError('fixture release deadline')
                return original(handler, *args, **kwargs)
            provider.server.RequestHandlerClass.send_response = held
            gateway = Gateway(provider.server.server_port, provider.server.server_port,
                expected_models=('gpt-6-luna', 'gpt-6.1-sol'),
                budget={'limit': 7_200_000_000_000, 'journal': Path(root)/'journal'})
            results = []
            first = threading.Thread(target=lambda: results.append(gateway.request(
                b'{"model":"gpt-6-luna","input":[]}', token='fixture-upstream')[0]))
            try:
                first.start()
                self.assertTrue(arrived.wait(2))
                self.assertEqual(gateway.request(b'{"model":"gpt-6.1-sol","input":[]}', token='fixture-shadow')[0], 402)
                self.assertEqual(len(provider.calls), 1)
            finally:
                release.set(); first.join(3); gateway.close()
            self.assertEqual(results, [200])

    def test_each_retry_consumes_reservation_and_restart_cannot_refund(self):
        usage = {'input_tokens': 10, 'output_tokens': 2, 'input_tokens_details': {'cached_tokens': 0}}
        with tempfile.TemporaryDirectory() as root, Provider(value={
                'model': 'gpt-6-luna', 'output': [], 'usage': usage}) as provider:
            budget = {'limit': 1_000_000_000_000, 'journal': Path(root)/'journal'}
            gateway = Gateway(provider.server.server_port, provider.server.server_port, direct=True,
                expected_models=('gpt-6-luna', 'gpt-6.1-sol'), budget=budget)
            try:
                body = b'{"model":"gpt-6-luna","input":[]}'
                self.assertEqual([gateway.request(body)[0] for _ in range(3)], [200, 200, 402])
                self.assertEqual(len(provider.calls), 2)
            finally:
                gateway.close()
            with self.assertRaisesRegex(RuntimeError, 'EEXIST'):
                Gateway(provider.server.server_port, provider.server.server_port, direct=True,
                    expected_models=('gpt-6-luna', 'gpt-6.1-sol'), budget=budget)

    def test_nonterminal_transport_keeps_exposure_and_stops_retries(self):
        with tempfile.TemporaryDirectory() as root, Provider(status=503) as provider:
            gateway = Gateway(provider.server.server_port, provider.server.server_port, direct=True,
                expected_models=('gpt-6-luna', 'gpt-6.1-sol'),
                budget={'limit': 2_000_000_000_000, 'journal': Path(root)/'journal'})
            try:
                body = b'{"model":"gpt-6-luna","input":[]}'
                self.assertEqual(gateway.request(body)[0], 503)
                self.assertEqual(gateway.request(body)[0], 402)
                self.assertEqual(len(provider.calls), 1)
            finally:
                gateway.close()

    def test_unknown_usage_freezes_new_paid_attempts(self):
        with tempfile.TemporaryDirectory() as root, Provider(value={
                'model': 'gpt-6-luna', 'output': [], 'usage': {'input_tokens': 10}}) as provider:
            gateway = Gateway(provider.server.server_port, provider.server.server_port, direct=True,
                expected_models=('gpt-6-luna', 'gpt-6.1-sol'),
                budget={'limit': 2_000_000_000_000, 'journal': Path(root)/'journal'})
            try:
                body = b'{"model":"gpt-6-luna","input":[]}'
                self.assertEqual(gateway.request(body)[0], 200)
                self.assertEqual(gateway.request(body)[0], 402)
                self.assertEqual(len(provider.calls), 1)
            finally:
                gateway.close()

    def test_escaped_multimodal_request_is_denied_before_spend(self):
        with tempfile.TemporaryDirectory() as root, Provider() as provider:
            gateway = Gateway(provider.server.server_port, provider.server.server_port, direct=True,
                expected_models=('gpt-6-luna', 'gpt-6.1-sol'),
                budget={'limit': 1_000_000_000_000, 'journal': Path(root)/'journal'})
            try:
                body = b'{"model":"gpt-6-luna","input":[{"type":"message","content":[{"type":"input_\\u0069mage","image_url":"https://invalid/"}]}]}'
                self.assertEqual(gateway.request(body)[0], 400)
                self.assertEqual(provider.calls, [])
            finally:
                gateway.close()

    def test_primary_and_shadow_share_conservative_admission_before_transport(self):
        usage = {'input_tokens': 100, 'output_tokens': 2,
                 'input_tokens_details': {'cached_tokens': 0}}
        with tempfile.TemporaryDirectory() as root, Provider(value={
                'model': 'gpt-6-luna', 'output': [], 'usage': usage}) as provider:
            journal = Path(root) / 'attempts.jsonl'
            gateway = Gateway(provider.server.server_port, provider.server.server_port,
                expected_models=('gpt-6-luna', 'gpt-6.1-sol'),
                budget={'limit': 7_200_000_000_000, 'journal': journal})
            try:
                self.assertEqual(gateway.request(b'{"model":"gpt-6-luna","input":[]}', token='fixture-upstream')[0], 200)
                status, _ = gateway.request(b'{"model":"gpt-6.1-sol","input":[]}', token='fixture-shadow')
                self.assertEqual(status, 402, 'combined reservations must deny before paid transport')
                self.assertEqual(len(provider.calls), 1)
            finally:
                gateway.close()
            events = [json.loads(line) for line in journal.read_text().splitlines()]
            self.assertEqual(events[0]['event'], 'escrow')
            self.assertEqual([e['event'] for e in events[1:]], ['reserved', 'completed'])
            self.assertEqual(events[1]['pico_usd'], '358500000000')


if __name__ == '__main__':
    unittest.main()
