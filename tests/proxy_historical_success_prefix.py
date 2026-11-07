"""Bounded numeric prefix through the real CLI; synthetic state/loopback only."""
import copy
import hashlib
import json
import unittest

import proxy_state_regressions as support
from proxy_transparent_history import TransparentProxy


class IndexedProxy(support.Proxy):
    request = TransparentProxy.request

    def __enter__(self):
        return self

    def __exit__(self, *_exc):
        self.close()


class HistoricalSuccessPrefix(unittest.TestCase):
    setUp = support.StateRegressions.setUp
    record_evidence = support.StateRegressions.record_evidence
    fixture = support.StateRegressions.fixture

    def test_seed_preserves_successful_window_across_failures_save_and_restart(self):
        def primary(_body, n):
            response = support.completed(response_id=f'prefix-{n}', input_tokens=1000)
            if n == 3:
                response['status'] = 'failed'
                response['error'] = {'code': 'fixture_failure', 'message': 'synthetic'}
            return 200, response

        def review(_body, _n):
            response = support.completed()
            response['status'] = 'failed'
            response['error'] = {'code': 'fixture_review_failure', 'message': 'synthetic'}
            del response['usage']  # unknown failed spend must not censor successes
            return 200, response

        fixture = self.fixture(primary, review)
        options = ['--review-replayed-history', '--review-every-requests', '100',
                   '--primary-context-tokens', '1000000']
        history = [{'role': 'user', 'content': 'prefix task'},
                   {'role': 'assistant', 'content': 'synthetic historical material ' * 500},
                   {'role': 'user', 'content': 'continue'}]
        proxy = IndexedProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(history)[0], 200)
        original = proxy.saved()
        initial = proxy.request('GET', '/carry/dashboard/stats')[1]
        self.assertEqual(len(fixture.reviews), 1)
        self.assertEqual(original['shadow']['unavailable_cost_calls'], 1)
        frozen = copy.deepcopy(initial['sessions'][0]['cost_estimate'])
        frozen['priced'] = True  # Estimate, rather than dashboard-only envelope
        frozen.setdefault('quantity_policy_version', None)
        for key in ('whole_window', 'quantity_evidence', 'counterfactual_basis',
                    'in_flight_costs_excluded'):
            frozen.pop(key, None)
        for key in ('observed_reviewer_cost', 'legacy_quantity_segment',
                    'legacy_initialization_segment', 'legacy_cache_policy_segment',
                    'cache_policy_boundaries'):
            if frozen.get(key) is None or frozen.get(key) == []:
                frozen.pop(key, None)  # canonical Estimate omits absent optionals
        self.assertEqual(frozen['coverage']['failed_calls'], 1)
        self.assertEqual(frozen['coverage']['reviewer_responses'], 0)
        self.assertEqual(frozen['unavailable_reviewer_cost_calls'], 0)
        proxy.close()

        checkpoint = next(self.state.glob('*.json'))
        owner = checkpoint.stem
        # Only a stopped SYNTHETIC fixture loses its old request observations.
        # Paid ledgers, authority graph, history and all other fields stay exact.
        staged = copy.deepcopy(original)
        staged['request_evidence']['observations'] = []
        staged['request_evidence']['prior_untracked'] = 2  # success + failed review
        checkpoint.write_text(json.dumps(staged))
        index_path = self.state / 'history.index'
        index = json.loads(index_path.read_text())
        index['snapshots'][owner]['request_evidence'] = staged['request_evidence']
        index_path.write_text(json.dumps(index))
        startup_hashes = {str(p): hashlib.sha256(p.read_bytes()).hexdigest()
                          for p in (checkpoint, index_path)}
        prefix = dict(source_sha256='a' * 64, owner=owner,
                      history_namespace=staged['history_namespace'],
                      primary_attempts=1, reviewer_attempts=1, estimate=frozen)
        seed = dict(format='carry-historical-success-seed-v1', prefixes=[prefix])
        (self.state / 'historical-success.seed').write_text(json.dumps(seed))
        self.assertEqual(startup_hashes, {str(p): hashlib.sha256(p.read_bytes()).hexdigest()
                                        for p in (checkpoint, index_path)})
        proxy = IndexedProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        seeded = proxy.request('GET', '/carry/dashboard/stats')[1]
        # A read-only seed import must not rewrite startup authority.
        self.assertEqual(startup_hashes, {str(p): hashlib.sha256(p.read_bytes()).hexdigest()
                                        for p in (checkpoint, index_path)})
        for _ in range(3):
            self.assertEqual(proxy.send(history)[0], 200)
        saved = proxy.saved()
        stats = proxy.request('GET', '/carry/dashboard/stats')[1]
        proxy.close()
        proxy = IndexedProxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        restarted = proxy.request('GET', '/carry/dashboard/stats')[1]
        self.evidence.update(original=original, prefix=prefix, staged=staged,
                             seeded=seeded, saved=saved, stats=stats, restarted=restarted,
                             wires=fixture.primaries, review_wires=fixture.reviews,
                             startup_hashes=startup_hashes)
        # Complete transport/paid/native/count/restart controls BEFORE RED.
        self.assertEqual(len(fixture.primaries), 4)
        self.assertEqual(len(fixture.reviews), 1)
        self.assertTrue(all(w['input'] == history for w in fixture.primaries))
        self.assertEqual(saved['completed_requests'], 3)
        self.assertEqual(saved['failed_primaries'], 1)
        self.assertEqual(saved['primary']['calls'], 4)
        self.assertEqual(saved['shadow'], original['shadow'])
        self.assertEqual(saved['primary']['unavailable_cost_calls'], 0)
        self.assertAlmostEqual(saved['primary']['cost_usd'],
                               4 * original['primary']['cost_usd'], places=12)
        self.assertEqual(proxy.saved()['primary'], saved['primary'])
        self.assertEqual(proxy.saved()['shadow'], saved['shadow'])
        self.assertEqual(restarted['cost_estimate'], stats['cost_estimate'])
        self.evidence['native_guards_complete'] = True
        whole = stats['sessions'][0]['cost_estimate']['whole_window']
        self.assertIsInstance(whole, dict, 'verified successful numeric prefix must restore whole-window estimate')
        expected = 3 * original['primary']['cost_usd']
        for snapshot in (stats, restarted):
            for estimate in (snapshot['sessions'][0]['cost_estimate'], snapshot['cost_estimate']):
                self.assertEqual(estimate['whole_window']['primary_responses'], 3)
                self.assertAlmostEqual(estimate['whole_window']['carry_cost'], expected, places=12)
                self.assertAlmostEqual(estimate['whole_window']['traditional_cost'], expected, places=12)
                self.assertAlmostEqual(estimate['whole_window']['savings'], 0, places=12)
            row = snapshot['sessions'][0]['cost_estimate']
            self.assertEqual(row['coverage']['failed_calls'], 2)
            self.assertEqual(row['coverage']['prior_untracked_calls'], 0)
            self.assertEqual(row['matched_primary_responses'], 2,
                             'frozen historical strict reference is retained, not re-used')
        self.assertEqual(saved['request_evidence']['historical_success_prefix'], prefix)
        self.assertEqual(seeded['sessions'][0]['cost_estimate']['whole_window']['primary_responses'], 1)
        proxy.close()
        self.evidence['loader_controls'] = {}
        # Exercise the real no-evidence legacy loader as well as hard refusal;
        # every reset below is a stopped synthetic fixture, never live data.
        for case in ('legacy-no-evidence', 'unknown-success-money', 'missing-seed', 'count-mismatch'):
            legacy = copy.deepcopy(original)
            legacy.pop('request_evidence', None)
            checkpoint.write_text(json.dumps(legacy))
            index['snapshots'][owner] = copy.deepcopy(legacy)
            index_path.write_text(json.dumps(index))
            candidate = copy.deepcopy(prefix)
            if case == 'unknown-success-money':
                candidate['estimate']['unavailable_primary_cost_calls'] = 1
            if case == 'count-mismatch':
                candidate['reviewer_attempts'] += 1
            seed_path = self.state / 'historical-success.seed'
            if case == 'missing-seed':
                seed_path.unlink()
            else:
                seed_path.write_text(json.dumps(dict(format=seed['format'], prefixes=[candidate])))
            before_bytes = {str(p): p.read_bytes() for p in (checkpoint, index_path)}
            with IndexedProxy(self.state, fixture, options) as probe:
                dashboard = probe.request('GET', '/carry/dashboard/stats')[1]
                self.assertEqual(before_bytes, {str(p): p.read_bytes() for p in (checkpoint, index_path)})
                self.evidence['loader_controls'][case] = dashboard
                if case == 'count-mismatch':
                    before_wire = len(fixture.primaries)
                    self.assertEqual(probe.send(history)[0], 500)
                    self.assertEqual(len(fixture.primaries), before_wire)
                    self.assertGreater(dashboard['cost_estimate']['whole_window_unavailable_sessions'], 0)
                elif case == 'legacy-no-evidence':
                    self.assertAlmostEqual(dashboard['cost_estimate']['whole_window']['carry_cost'],
                                           original['primary']['cost_usd'], places=12)
                    self.assertEqual(probe.send(history)[0], 200)
                    normal_save = probe.saved()
                    self.assertEqual(normal_save['request_evidence']['historical_success_prefix'], prefix)
                    self.assertEqual(normal_save['primary']['calls'], 2)
                    self.assertEqual(normal_save['shadow'], original['shadow'])
                    self.evidence['legacy_normal_save'] = normal_save
                else:
                    self.assertIsNone(dashboard['cost_estimate']['whole_window'])
                    self.assertEqual(probe.saved()['primary'], original['primary'])
                    self.assertEqual(probe.saved()['shadow'], original['shadow'])


if __name__ == '__main__':
    unittest.main()
