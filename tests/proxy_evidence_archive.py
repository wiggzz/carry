"""Repair G1: staged semantic saturation, real append/save/process restart.

Saturation is NOT 4096 executed calls. Only the two loopback receipts count.
The numeric archive (when implemented) is never staged or manufactured.
"""
import copy
import hashlib
from history_store import read_index, write_index
import json
import unittest
import proxy_state_regressions as support


def canonical_saved(proxy):
    saved = proxy.saved()
    if saved.get('history_shared'):
        owner = next(proxy.state.glob('*.json')).stem
        return read_index(proxy.state / 'history.index')['snapshots'][owner]
    return saved


def selected_archive(proxy):
    mark = canonical_saved(proxy)['request_evidence']['archive']
    suffix = '-g'+mark['generation'] if mark.get('generation') else ''
    namespace = hashlib.sha256(json.dumps(mark['namespace'], separators=(',', ':')).encode()).hexdigest()
    return proxy.state / f".request-evidence-{mark['owner']}-{namespace}{suffix}.jsonl"


class EvidenceArchive(unittest.TestCase):
    setUp = support.StateRegressions.setUp
    record_evidence = support.StateRegressions.record_evidence
    fixture = support.StateRegressions.fixture

    def rollover(self, bound):
        fixture = self.fixture(lambda _b, n: (200, support.completed(response_id=f'archive-{n}', input_tokens=1000)))
        options = ['--review-every-requests','100','--review-min-primary-tokens','1000000','--primary-context-tokens','1000000']
        history = [{'role':'user','content':'raw-body-storage-sentinel'}]
        proxy = support.Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(history)[0], 200)
        original = proxy.saved()
        before = support.cost_estimate(proxy)
        proxy.close()
        checkpoint = next(self.state.glob('*.json'))
        owner = checkpoint.stem
        index_path = self.state / 'history.index'
        index = read_index(index_path)
        saturated = copy.deepcopy(original)
        observations = saturated['request_evidence']['observations']
        if bound == 'observations':
            observations.extend(['Boundary'] * (4096 - len(observations)))
        else:
            facts = next(o['Primary']['facts'] for o in observations if isinstance(o, dict) and 'Primary' in o)
            facts.update(input=['f'*64]*32768, semantic_input=['e'*64]*32768,
                         semantic_output=[], reused_input=0)
        checkpoint.write_text(json.dumps(saturated))
        if owner in index['snapshots']:
            index['snapshots'][owner]['request_evidence'] = saturated['request_evidence']
            write_index(index_path, index)
        proxy = support.Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(history)[0], 200)
        saved = proxy.saved()
        after = support.cost_estimate(proxy)
        proxy.close()
        proxy = support.Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        restarted = support.cost_estimate(proxy)
        self.evidence.update(bound=bound, staged_saturation=True, before=before,
                             after=after, restarted=restarted, saved=saved,
                             primaries=fixture.primaries)
        self.assertEqual(len(fixture.primaries), 2)
        self.assertEqual(saved['primary']['calls'], 2)
        self.assertEqual(saved['shadow']['calls'], 0)
        self.assertAlmostEqual(saved['primary']['cost_usd'], 2*original['primary']['cost_usd'], places=12)
        self.assertEqual(after, restarted)
        self.assertEqual(after, support.cost_estimate(proxy))
        self.assertLess(len(saved['request_evidence']['observations']), 4096)
        self.assertIsNotNone(after['whole_window'], 'G1 captured chronology must survive semantic rollover')
        self.assertEqual(after['whole_window']['primary_responses'], 2)
        self.assertAlmostEqual(after['whole_window']['traditional_cost'], 2*before['whole_window']['traditional_cost'], places=12)

    def test_rebase_threshold_crossing_keeps_protected_fixed_floor(self):
        output = [{'type':'message','role':'assistant','content':[{'type':'output_text','text':' result'*10000}]}]
        def primary(_body, n):
            response = support.completed(output if n == 3 else [], f'floor-{n}', input_tokens=[200000,250000,200000][n-1])
            response['usage']['output_tokens'] = 0
            return 200, response
        fixture = self.fixture(primary)
        proxy = support.Proxy(self.state, fixture, ['--primary-context-tokens','1000000','--review-min-primary-tokens','2000000'], mode='off')
        self.stack.callback(proxy.close)
        history = [{'role':'assistant','content':'old context'}]
        for n in range(3):
            history += [{'role':'user','content':f'append {n}'}]
            self.assertEqual(proxy.send(history, model='gpt-6.1-sol', max_output_tokens=1,
                                        instructions=' fixed'*25000, prompt_cache_options={'mode':'explicit'})[0], 200)
        saved = proxy.saved()
        estimate = support.cost_estimate(proxy)
        self.evidence.update(saved=saved, estimate=estimate, primaries=fixture.primaries)
        # Local visible-output units include a small positive framing charge.
        # This upper bound is deliberately loose, but distinguishes an illegally
        # reduced 20k floor from the >25k fixed instruction floor.
        trajectory = estimate['whole_window']['trajectory']
        self.assertEqual(trajectory['traditional_compactions'], 1)
        self.assertLess(trajectory['summary_cost'], 0.482,
                        'L1 replay-only threshold crossing must not retire the fixed floor')

    def test_native_v1_summary_then_growth_has_two_chronological_primaries(self):
        def primary(_body, n):
            response = support.completed([{'type':'compaction','encrypted_content':'native-opaque-storage-sentinel'}] if n == 1 else [],
                                         f'native-{n}', input_tokens=2000 if n == 1 else 300000)
            if n == 1:
                response.pop('status')  # Real V1 terminal seam, one anchor.
            return 200, response
        fixture = self.fixture(primary)
        options = ['--primary-context-tokens','1000000','--review-min-primary-tokens','2000000']
        proxy = support.Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        history = [{'role':'user','content':'summarize'}, {'type':'compaction_trigger'}]
        fields = dict(model='gpt-6.1-sol', max_output_tokens=1, prompt_cache_options={'mode':'explicit'})
        self.assertEqual(proxy.send(history, **fields)[0], 200)
        first = support.cost_estimate(proxy)
        saved_first = proxy.saved()
        self.assertEqual(saved_first['native_compactions'], 1)
        followup = saved_first['native_checkpoint'] + [{'role':'user','content':'continue'}]
        self.assertEqual(proxy.send(followup, **fields)[0], 200)
        after = support.cost_estimate(proxy)
        saved = proxy.saved()
        proxy.close()
        proxy = support.Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        restarted = support.cost_estimate(proxy)
        self.evidence.update(first=first, after=after, restarted=restarted, saved=saved, wires=fixture.primaries)
        self.assertEqual(len(fixture.primaries), 2)
        self.assertEqual(first['whole_window']['primary_responses'], 1)
        self.assertEqual(after['whole_window']['primary_responses'], 2)
        self.assertEqual(after['whole_window']['trajectory']['initial_compactions'], 0)
        self.assertEqual(after['whole_window']['trajectory']['traditional_compactions'], 1)
        expected = (2000*2+10 + 300000*4+15 + (300000-20000)*4+1000*15)/1e6
        self.assertAlmostEqual(after['whole_window']['traditional_cost'], expected, places=12)
        self.assertEqual(after, restarted)
        self.assertEqual(saved['primary']['calls'], 2)
        self.assertEqual(saved['shadow']['calls'], 0)
        for archive in self.state.glob('.request-evidence-*.jsonl'):
            self.assertNotIn(b'native-opaque-storage-sentinel', archive.read_bytes())

    def test_archive_negatives_are_unknown_but_forwarding_and_paid_ledgers_survive(self):
        self.evidence['cases'] = {}
        for case in ('missing', 'corrupt', 'foreign', 'truncated', 'foreign-watermark', 'symlink', 'uncommitted-tail'):
            with self.subTest(case=case):
                state = self.state / case
                state.mkdir()
                fixture = self.fixture(lambda _b,n: (200,support.completed(response_id=f'negative-{n}',input_tokens=1000)))
                options = ['--review-every-requests','100','--review-min-primary-tokens','1000000','--primary-context-tokens','1000000']
                proxy = support.Proxy(state, fixture, options)
                self.stack.callback(proxy.close)
                history = [{'role':'user','content':'raw-body-storage-sentinel'}]
                self.assertEqual(proxy.send(history)[0], 200)
                saved = proxy.saved()
                before = support.cost_estimate(proxy)
                proxy.close()
                archive = selected_archive(proxy)
                raw = archive.read_bytes()
                for sentinel in (b'raw-body-storage-sentinel',b'traditional_cost',b'summary_cost',b'canonical_offset'):
                    self.assertNotIn(sentinel, raw)
                records = [json.loads(line) for line in raw.splitlines()]
                self.assertEqual(len(records), 2)
                self.assertIn('Dispatch', records[0]['event'])
                facts = records[1]['event']['Primary']['facts']
                self.assertIsNone(facts['input'])
                self.assertIsNone(facts['semantic_input'])
                self.assertIsNone(facts['semantic_output'])
                if case == 'missing':
                    archive.unlink()
                elif case == 'corrupt':
                    archive.write_bytes(b'not json\n')
                elif case == 'foreign':
                    records[0]['owner'] = 'a'*64
                    archive.write_text(json.dumps(records[0])+'\n')
                elif case == 'truncated':
                    archive.write_bytes(raw[:-1])
                elif case == 'symlink':
                    target = state / 'foreign-target.bin'
                    target.write_bytes(b'foreign-target-must-survive')
                    archive.unlink()
                    archive.symlink_to(target)
                elif case == 'foreign-watermark':
                    checkpoint = next(state.glob('*.json'))
                    saved['request_evidence']['archive']['owner'] = 'a'*64
                    checkpoint.write_text(json.dumps(saved))
                else:
                    archive.write_bytes(raw + b'orphan bytes must not be replayed\n')
                proxy = support.Proxy(state, fixture, options)
                self.stack.callback(proxy.close)
                loaded = support.cost_estimate(proxy)
                if case == 'uncommitted-tail':
                    self.assertEqual(loaded, before)
                else:
                    self.assertIsNone(loaded['whole_window'])
                    self.assertEqual(loaded['coverage']['prior_untracked_calls'], 0,
                                     'archive integrity is not an invented untracked request')
                    self.assertTrue(loaded['quantity_evidence']['numeric_archive_unavailable'])
                self.assertEqual(proxy.send(history)[0], 200, 'archive faults cannot block provider forwarding')
                current = proxy.saved()
                after = support.cost_estimate(proxy)
                self.assertEqual(len(fixture.primaries), 2)
                self.assertEqual(current['primary']['calls'], 2)
                self.assertEqual(current['shadow']['calls'], 0)
                self.assertAlmostEqual(current['primary']['cost_usd'], 2*saved['primary']['cost_usd'], places=12)
                proxy.close()
                proxy = support.Proxy(state, fixture, options)
                self.stack.callback(proxy.close)
                restarted = support.cost_estimate(proxy)
                self.assertEqual(after, restarted)
                self.assertEqual(after, support.cost_estimate(proxy))
                if case == 'symlink':
                    self.assertEqual(target.read_bytes(), b'foreign-target-must-survive')
                if case == 'uncommitted-tail':
                    self.assertEqual(after['whole_window']['primary_responses'], 2)
                    # A stale caller cannot distinguish an orphan from another
                    # committed publication. Preserve bytes, replay only the
                    # checkpoint watermark and genuine bounded pending facts.
                    self.assertEqual(archive.read_bytes(), raw + b'orphan bytes must not be replayed\n')
                    self.assertEqual(current['request_evidence']['archive']['events'], 4)
                    self.assertFalse(current['request_evidence'].get('numeric_pending'))
                    self.assertNotEqual(selected_archive(proxy), archive)
                else:
                    self.assertIsNone(after['whole_window'])
                    self.assertEqual(len(current['request_evidence']['numeric_pending']), 2)
                self.evidence['cases'][case] = dict(before=before, loaded=loaded, after=after, restarted=restarted,
                                                   primary=current['primary'], archive_bytes=len(raw))
                proxy.close()

    def test_orphan_advances_archive_and_drains_pending_after_restart(self):
        fixture = self.fixture(lambda _b,n: (200,support.completed(response_id=f'orphan-{n}',input_tokens=1000)))
        options = ['--review-min-primary-tokens','2000000','--primary-context-tokens','1000000']
        history = [{'role':'user','content':'raw-body-storage-sentinel'}]
        proxy = support.Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        self.assertEqual(proxy.send(history)[0], 200)
        before = proxy.saved()
        proxy.close()
        archive = selected_archive(proxy)
        orphan = archive.read_bytes() + b'orphan bytes must not be replayed\n'
        archive.write_bytes(orphan)
        for n in range(3):
            proxy = support.Proxy(self.state, fixture, options)
            self.stack.callback(proxy.close)
            self.assertEqual(proxy.send(history)[0], 200)
            saved = proxy.saved()
            after = support.cost_estimate(proxy)
            self.assertEqual(saved['primary']['calls'], n+2)
            self.assertAlmostEqual(saved['primary']['cost_usd'], (n+2)*before['primary']['cost_usd'], places=12)
            self.assertEqual(after['whole_window']['primary_responses'], n+2)
            self.assertEqual(saved['request_evidence']['archive']['events'], 2*(n+2))
            self.assertFalse(saved['request_evidence'].get('numeric_pending'))
            self.assertFalse(saved['request_evidence'].get('numeric_lost', False))
            self.assertEqual(archive.read_bytes(), orphan)
            proxy.close()
        proxy = support.Proxy(self.state, fixture, options)
        self.stack.callback(proxy.close)
        self.assertEqual(support.cost_estimate(proxy), after)
        self.assertEqual(len(fixture.primaries), 4)
        self.evidence.update(saved=saved, estimate=after, wires=fixture.primaries)

    def test_actual_reference_pressure_growing_cache_frontier_survives_restart(self):
        # Each real receipt contributes semantic OUTPUT references even when its
        # visible retained text and billed output are zero. Sixteen receipts
        # exceed 65536 references, without exceeding Session's 16384-item input
        # cap. The caller appends to its admitted input prefix, not pending output.
        # Bound each receipt's transient JSON work under the unchanged guard.
        output_items = 4096
        calls = 17
        receipts = []
        def primary(_body, n):
            output = [{'type':'message','role':'assistant','content':[],
                       'id':f'empty-{n}-{j}'} for j in range(output_items)]
            response = support.completed(output, f'frontier-{n}', input_tokens=300000 + (n-1)*100)
            response['usage']['output_tokens'] = 0
            receipts.append(response)
            return 200, response
        fixture = self.fixture(primary, lambda *_: self.fail('unexpected reviewer'))
        options = ['--primary-context-tokens','1000000','--review-min-primary-tokens','2000000']
        proxy = support.Proxy(self.state, fixture, options, mode='off')
        self.stack.callback(proxy.close)
        history = []
        fields = dict(model='gpt-6.1-sol', max_output_tokens=1,
                      prompt_cache_options={'mode':'implicit','ttl':'30m'})
        expected = 0.0
        snapshots, pressure, charges = [], [], []
        self.evidence.update(snapshots=snapshots, pressure=pressure, charges=charges,
                             receipts=receipts, wires=fixture.primaries, staged_saturation=False)
        owner = None
        retained_refs = 0
        evicted = False
        for i in range(calls):
            history.append({'role':'user','content':f'growth {i}'})
            if i == calls-1:
                self.assertTrue(evicted, 'restart must follow actual reference eviction')
                prior = support.cost_estimate(proxy)
                proxy.close()
                proxy = support.Proxy(self.state, fixture, options, mode='off')
                self.stack.callback(proxy.close)
                self.assertEqual(support.cost_estimate(proxy), prior)
            status, returned = proxy.send(history, **fields)
            self.assertEqual(status, 200)
            self.assertEqual(returned, receipts[-1], 'complete native receipt must survive forwarding')
            self.assertEqual(fixture.primaries[-1], dict(fields, input=history),
                             'complete actual input/settings must survive forwarding')
            saved = canonical_saved(proxy)
            current_owner = next(self.state.glob('*.json')).stem
            owner = current_owner if owner is None else owner
            self.assertEqual(current_owner, owner)
            self.assertLessEqual(len(history), 16384)
            self.assertEqual(len(saved['history']), len(history), 'every input item must be admitted')
            self.assertEqual([item['value'] for item in saved['history']], history)
            self.assertEqual(saved['history_rebases'], 0, 'no off-mode tracking fallback or rebase')
            self.assertEqual(saved['completed_requests'], i+1)
            self.assertEqual(saved['primary']['calls'], i+1)
            self.assertEqual(saved['shadow']['calls'], 0)
            paid = sum((300000+n*100)*4/1e6 for n in range(i+1))
            self.assertAlmostEqual(saved['primary']['cost_usd'], paid, places=10)
            evidence = saved['request_evidence']
            observations = evidence['observations']
            self.assertTrue(all('Primary' in o or 'Dispatch' in o for o in observations), 'no hidden epoch boundary')
            facts = [o['Primary']['facts'] for o in observations if 'Primary' in o]
            actual_refs = sum(2*len(f['input'])+len(f['semantic_output']) for f in facts)
            demanded_refs = retained_refs + 2 + output_items
            crossed = demanded_refs > 65536
            pressure.append(dict(call=i+1, before=retained_refs, demanded=demanded_refs,
                                 retained=actual_refs, prior_untracked=evidence['prior_untracked'],
                                 input_items=len(history), history_items=len(saved['history']),
                                 history_rebases=saved['history_rebases']))
            self.assertLessEqual(actual_refs, 65536)
            self.assertTrue(all(len(f['semantic_output']) == output_items for f in facts))
            self.assertTrue(all(f['retained'] == 0 for f in facts))
            if crossed:
                self.assertEqual(len(facts), 1, 'actual reference pressure must evict the semantic prefix')
                self.assertEqual(evidence['prior_untracked'], i)
                self.assertEqual(facts[0]['reused_input'], 0)
                self.assertEqual(actual_refs, 2*len(history)+output_items)
                evicted = True
            elif not evicted:
                self.assertEqual(len(facts), i+1)
                self.assertEqual(evidence['prior_untracked'], 0)
                self.assertEqual(actual_refs, demanded_refs)
            retained_refs = actual_refs
            self.assertEqual(evidence['archive']['events'], 2*(i+1))
            self.assertFalse(evidence.get('numeric_pending'))
            self.assertFalse(evidence.get('numeric_lost', False))
            snapshot = support.cost_estimate(proxy)
            snapshots.append(snapshot)
            # Independent price oracle: one free initialization, then implicit
            # previous-input reads and only the 100 newly appended tokens written.
            incoming = 21000 + i*100
            charge = (incoming*2.5 if i == 0 else (incoming-100)*0.1+100*2.5)/1e6
            charges.append(dict(input=incoming, cached=0 if i == 0 else incoming-100,
                                cost=charge, paid=paid))
            expected += charge
            self.evidence.update(expected=expected, saved=saved, actual_input_items=len(history))
            self.assertEqual(snapshot['whole_window']['primary_responses'], i+1)
            self.assertEqual(snapshot['whole_window']['trajectory']['initial_compactions'], 1)
            self.assertEqual(snapshot['whole_window']['trajectory']['traditional_compactions'], 0)
            self.assertAlmostEqual(snapshot['whole_window']['traditional_cost'], expected, places=10,
                msg='R2 semantic reference eviction must not erase the numeric append/cache frontier')
        self.assertTrue(evicted)
        self.assertEqual(len(fixture.primaries), calls)
        self.assertEqual(len(receipts), calls)
        self.assertEqual(fixture.reviews, [])
        proxy.close()
        proxy = support.Proxy(self.state, fixture, options, mode='off')
        self.stack.callback(proxy.close)
        self.assertEqual(support.cost_estimate(proxy), snapshots[-1])
        self.assertEqual(support.cost_estimate(proxy), snapshots[-1])

    def test_observation_rollover_retains_numeric_chronology(self):
        self.rollover('observations')

    def test_reference_rollover_retains_numeric_chronology(self):
        self.rollover('references')


if __name__ == '__main__':
    unittest.main()
