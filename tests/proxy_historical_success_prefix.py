"""Bounded numeric prefix through the real CLI; synthetic state/loopback only."""
import copy
import hashlib
from history_store import read_index, write_index
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


class HistoricalSuccessPrefix(support.FixtureCase):

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
        staged['request_evidence'].pop('archive', None)  # pre-journal legacy loss, not suffix-only rollover
        staged['request_evidence'].pop('numeric_pending', None)
        staged['request_evidence']['prior_untracked'] = 2  # success + failed review
        checkpoint.write_text(json.dumps(staged))
        index_path = self.state / 'history.index'
        index = read_index(index_path)
        index['snapshots'][owner]['request_evidence'] = staged['request_evidence']
        write_index(index_path, index)
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
        self.assertIsNone(whole, 'successful expense proof cannot invent missing chronological inputs')
        expected = 3 * original['primary']['cost_usd']
        for snapshot in (stats, restarted):
            for estimate in (snapshot['sessions'][0]['cost_estimate'], snapshot['cost_estimate']):
                self.assertIsNone(estimate['whole_window'])
            self.assertAlmostEqual(snapshot['sessions'][0]['cost_estimate']['observed_primary_cost'], expected, places=12)
            row = snapshot['sessions'][0]['cost_estimate']
            self.assertEqual(row['coverage']['failed_calls'], 2)
            self.assertEqual(row['coverage']['prior_untracked_calls'], 0)
            self.assertEqual(row['matched_primary_responses'], 2,
                             'frozen historical strict reference is retained, not re-used')
        self.assertEqual(saved['request_evidence']['historical_success_prefix'], prefix)
        self.assertIsNone(seeded['sessions'][0]['cost_estimate']['whole_window'])
        proxy.close()
        self.evidence['loader_controls'] = {}
        # Exercise the real no-evidence legacy loader as well as hard refusal;
        # every reset below is a stopped synthetic fixture, never live data.
        for case in ('legacy-no-evidence', 'unknown-success-money', 'missing-seed', 'count-mismatch'):
            legacy = copy.deepcopy(original)
            legacy.pop('request_evidence', None)
            checkpoint.write_text(json.dumps(legacy))
            index['snapshots'][owner] = copy.deepcopy(legacy)
            write_index(index_path, index)
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
                    self.assertIsNone(dashboard['cost_estimate']['whole_window'])
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

    def test_bounded_rollover_preserves_forwarding_and_restart_not_comparison(self):
        for bound in ('observations', 'references'):
            for keep_seed in (True, False):
                case = f'{bound}-seed-{keep_seed}'
                with self.subTest(case=case):
                    state = self.state / case
                    state.mkdir()
                    fixture = self.fixture(lambda _b, n: (200, support.completed(
                        response_id=f'rollover-{n}', input_tokens=1000)))
                    options = ['--review-every-requests', '100',
                               '--review-min-primary-tokens', '1000000',
                               '--primary-context-tokens', '1000000']
                    history = [{'role': 'user', 'content': 'bounded prefix continuity'}]
                    proxy = IndexedProxy(state, fixture, options)
                    self.stack.callback(proxy.close)
                    self.assertEqual(proxy.send(history)[0], 200)
                    original = proxy.saved()
                    estimate = copy.deepcopy(proxy.request('GET', '/carry/dashboard/stats')[1]
                                             ['sessions'][0]['cost_estimate'])
                    estimate['priced'] = True
                    estimate.setdefault('quantity_policy_version', None)
                    for key in ('whole_window', 'quantity_evidence', 'counterfactual_basis',
                                'in_flight_costs_excluded'):
                        estimate.pop(key, None)
                    for key in ('observed_reviewer_cost', 'legacy_quantity_segment',
                                'legacy_initialization_segment', 'legacy_cache_policy_segment',
                                'cache_policy_boundaries'):
                        if estimate.get(key) is None or estimate.get(key) == []:
                            estimate.pop(key, None)
                    proxy.close()
                    checkpoint = next(state.glob('*.json'))
                    owner = checkpoint.stem
                    index_path = state / 'history.index'
                    index = read_index(index_path)
                    prefix = dict(source_sha256='a' * 64, owner=owner,
                                  history_namespace=original['history_namespace'],
                                  primary_attempts=1, reviewer_attempts=0, estimate=estimate)
                    seed_path = state / 'historical-success.seed'
                    seed_path.write_text(json.dumps(dict(
                        format='carry-historical-success-seed-v1', prefixes=[prefix])))
                    imported = copy.deepcopy(original)
                    imported['request_evidence']['observations'] = []
                    imported['request_evidence'].pop('archive', None)
                    imported['request_evidence'].pop('numeric_pending', None)
                    imported['request_evidence']['prior_untracked'] = 1

                    def stage(saved):
                        # This stopped fixture declares a legacy, never-journaled
                        # suffix. Remove its earlier synthetic journal as well as
                        # the watermark; immutable-prefix saves must not rewrite
                        # an unrelated generation left by fixture setup.
                        saved['request_evidence'].pop('numeric_primary_items', None)
                        for archive in state.glob('.request-evidence-*.jsonl'):
                            archive.unlink()
                        self.assertEqual(list(state.glob('.request-evidence-*.jsonl')), [])
                        checkpoint.write_text(json.dumps(saved))
                        index['snapshots'][owner] = copy.deepcopy(saved)
                        write_index(index_path, index)

                    stage(imported)
                    # First bind is exact and the proof is persisted by a REAL
                    # request, before any synthetic saturation is constructed.
                    with IndexedProxy(state, fixture, options) as proxy:
                        seeded = proxy.request('GET', '/carry/dashboard/stats')[1]
                        self.assertIsNone(seeded['cost_estimate']['whole_window'],
                            'aggregate-only proof preserves money, not lost chronological inputs')
                        self.assertEqual(proxy.send(history)[0], 200)
                        bound_state = proxy.saved()
                    self.assertEqual(bound_state['request_evidence']['historical_success_prefix'], prefix)
                    saturated = copy.deepcopy(bound_state)
                    if bound == 'observations':
                        # Stopped SYNTHETIC suffix at documented 4096 bound,
                        # not 4096 executed HTTP requests. Counts/paid receipts
                        # are reconciled; the next real append crosses the cap.
                        suffix_count = 4096
                        observations = [{'Unmodeled': {'cost': original['primary']['cost_usd']}}
                                        for _ in range(suffix_count)]
                    else:
                        suffix_count = 1
                        observation = copy.deepcopy(next(o for o in bound_state['request_evidence']['observations'] if isinstance(o, dict) and 'Primary' in o))
                        facts = observation['Primary']['facts']
                        facts['reused_input'] = 0
                        facts['input'] = ['f' * 64] * 32768
                        facts['semantic_input'] = ['f' * 64] * 32768
                        facts['semantic_output'] = []
                        observations = [observation]  # exactly 65536 stored references
                    saturated['request_evidence']['observations'] = observations
                    # Stage a legacy suffix that has never had numeric capture;
                    # the real next append must journal its available raw facts.
                    saturated['request_evidence'].pop('archive', None)
                    saturated['request_evidence'].pop('numeric_pending', None)
                    saturated['completed_requests'] = 1 + suffix_count
                    # Scale only the synthetic native ledger, never an oracle
                    # copied from the candidate's later result.
                    def scale(value, factor):
                        if isinstance(value, dict):
                            return {k: scale(v, factor) for k, v in value.items()}
                        if isinstance(value, (int, float)) and not isinstance(value, bool):
                            return value * factor
                        return value
                    saturated['primary'] = scale(original['primary'], 1 + suffix_count)
                    stage(saturated)
                    if not keep_seed:
                        seed_path.unlink()
                    before_hashes = {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                                     for p in (checkpoint, index_path)}
                    record = self.evidence[case] = dict(
                        staging='stopped synthetic saturated suffix; not thousands of requests',
                        suffix_count=suffix_count, before_hashes=before_hashes,
                        prefix=prefix, before_primary=saturated['primary'], statuses=[])
                    with IndexedProxy(state, fixture, options) as proxy:
                        before = proxy.request('GET', '/carry/dashboard/stats')[1]
                        self.assertIsNone(before['cost_estimate']['whole_window'],
                            'saturated synthetic evidence is not a chronological replay')
                        self.assertEqual(before_hashes, {
                            p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                            for p in (checkpoint, index_path)})
                        for _ in range(2):
                            status, response = proxy.send(history)
                            record['statuses'].append(dict(status=status, response=response,
                                                           upstream_calls=len(fixture.primaries)))
                        saved = proxy.saved()
                        record['after_primary'] = saved['primary']
                        record['after_evidence'] = saved['request_evidence']
                        record['after_stats'] = proxy.request('GET', '/carry/dashboard/stats')[1]
                    # Remove the seed as well for the removed-seed arm; no
                    # in-process cache can hide a persisted-loader failure.
                    with IndexedProxy(state, fixture, options) as proxy:
                        record['restart_stats'] = proxy.request('GET', '/carry/dashboard/stats')[1]
                        status, response = proxy.send(history)
                        record['statuses'].append(dict(status=status, response=response,
                                                       upstream_calls=len(fixture.primaries)))
                        final = proxy.saved()
                        record['final_primary'] = final['primary']
                        record['final_evidence'] = final['request_evidence']
                        final_index = read_index(index_path)
                    record['wires'] = fixture.primaries
                    # First finish all reachable accounting/authority controls,
                    # then grade NEXT request and real-process restart liveness.
                    successful_appends = sum(r['status'] == 200 for r in record['statuses'])
                    self.assertEqual(len(fixture.primaries), 2 + successful_appends)
                    self.assertEqual(fixture.reviews, [])
                    self.assertTrue(all(w['input'] == history for w in fixture.primaries))
                    self.assertEqual(final['completed_requests'], 1 + suffix_count + successful_appends)
                    self.assertEqual(final['primary']['calls'], 1 + suffix_count + successful_appends)
                    expected_primary = scale(original['primary'], 1 + suffix_count + successful_appends)
                    actual_primary = copy.deepcopy(final['primary'])
                    self.assertAlmostEqual(actual_primary.pop('cost_usd'),
                                           expected_primary.pop('cost_usd'), places=10)
                    self.assertEqual(actual_primary, expected_primary,
                                     'all native paid/cache/token counters must continue exactly')
                    self.assertEqual(final['shadow'], original['shadow'])
                    self.assertEqual(final['history_namespace'], original['history_namespace'])
                    self.assertEqual(set(final_index['snapshots']), {owner})
                    self.assertEqual(final_index['snapshots'][owner]['primary'], final['primary'])
                    for key in ('history', 'history_frontier', 'compaction_count'):
                        self.assertEqual(final.get(key), bound_state.get(key), key)
                    before_input = copy.deepcopy(bound_state['primary_input'])
                    after_input = copy.deepcopy(final['primary_input'])
                    self.assertGreaterEqual(after_input['anchor'].pop('at'),
                                            before_input['anchor'].pop('at'))
                    self.assertEqual(after_input, before_input,
                                     'native quantity/history must continue without initialization')
                    for dashboard in (record['after_stats'], record['restart_stats']):
                        self.assertIsNone(dashboard['cost_estimate']['whole_window'])
                    record['accounting_authority_controls_complete'] = True
                    self.assertEqual([r['status'] for r in record['statuses']], [200, 200, 200],
                                     'normal retention must not turn optional proof into load/HTTP failure')
                    for dashboard in (record['after_stats'], record['restart_stats']):
                        self.assertEqual(len(dashboard['sessions']), 1)
                        row = dashboard['sessions'][0]['cost_estimate']
                        self.assertIsNone(row['whole_window'])
                        self.assertEqual(row['coverage']['prior_untracked_calls'], 0,
                                         'numeric capture preserves future money; seed proves only original legacy money')
                        self.assertEqual(row['coverage']['primary_responses'], 1 + suffix_count + 2)
                        self.assertAlmostEqual(row['observed_primary_cost'],
                                               (1 + suffix_count + 2) * original['primary']['cost_usd'], places=10,
                                               msg='captured future successful receipts survive semantic rollover; legacy input chronology is still missing')
                    self.assertEqual(final['request_evidence']['historical_success_prefix'], prefix)
                    self.assertEqual(final['request_evidence']['prior_untracked'], 1 + suffix_count)


if __name__ == '__main__':
    unittest.main()
