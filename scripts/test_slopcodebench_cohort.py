"""Model-free production-boundary tests; synthetic processes are NOT gold qualification."""
import contextlib
import copy
import io
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from scripts import slopcodebench as scb
from scripts import slopcodebench_prepare as preparation
from scripts import slopcodebench_runtime as runtime
from scripts import benchmark_comparison as comparison
from scripts.test_benchmark_comparison import contract
from scripts.test_swebench_comparison import events


class CohortPreparationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='scb cohort ')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.upstream, self.problems = self.root/'upstream', self.root/'problems'
        self.upstream.mkdir(); self.problems.mkdir()
        self.carry = self.root/'carry'; self.carry.write_bytes(b'synthetic binary')
        self.prompt = self.upstream/'prompt.jinja'; self.prompt.write_text('{{ spec }}')
        self.selection = json.loads(preparation.SELECTION.read_text())
        self.selection.pop('problem'); self.selection.pop('checkpoints')
        self.selection.pop('problem_tree_sha'); self.selection.pop('expected_checkpoint_arm_outcomes')
        self.selection['problems'] = [
            {'id': 'first', 'problem_tree_sha': 'a'*40, 'checkpoints': ['checkpoint_1', 'checkpoint_2']},
            {'id': 'last', 'problem_tree_sha': 'b'*40, 'checkpoints': ['checkpoint_1']}]
        self.selection_path = self.root/'selection.json'
        self.selection_path.write_text(json.dumps(self.selection))
        for task in self.selection['problems']:
            directory = self.problems/task['id']; directory.mkdir()
            (directory/'config.yaml').write_text('synthetic official input')
        self.calls = []
        self.warm_commands = []

    def definition(self, upstream, problems, selection):
        task_id = selection.get('id', selection.get('problem'))
        task = next(t for t in self.selection['problems'] if t['id'] == task_id)
        return {'prompt': self.prompt, 'pass_policy': 'any', 'timeout_seconds': 7200,
                'environment': {'type': 'docker', 'docker': {'image': 'unprepared'}},
                'test_dependencies': ['pytest', task_id+'-dependency==1'],
                'checkpoints': task['checkpoints']}

    def execute(self, args, **kwargs):
        self.calls.append((args, kwargs))
        if args[0] == 'git':
            if args[-1].startswith('HEAD:'):
                task = next(t for t in self.selection['problems'] if t['id'] == args[-1][5:])
                return subprocess.CompletedProcess(args, 0, task['problem_tree_sha']+'\n', '')
            return subprocess.CompletedProcess(args, 0, '3'*40+'\n', '')
        if args[:2] == ['docker', 'build']:
            context = Path(args[-1])
            # Execute the generated dependency warmer with a trusted uvx seam.
            tools = self.root/'bin'; tools.mkdir(exist_ok=True)
            uvx = tools/'uvx'
            uvx.write_text('#!'+sys.executable+'\nimport json,os,sys\nwith open(os.environ["WARM_LOG"],"a") as f: f.write(json.dumps(sys.argv[1:])+"\\n")\n')
            uvx.chmod(0o755)
            log = self.root/'warm.jsonl'
            subprocess.run(['/bin/sh', str(context/'warm-cache.sh')], check=True,
                env={'PATH': str(tools), 'WARM_LOG': str(log)}, timeout=5)
            self.warm_commands = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertEqual(sorted(p.name for p in context.iterdir()), ['Dockerfile', 'carry', 'warm-cache.sh'])
        if args[:3] == ['docker', 'image', 'inspect']:
            return subprocess.CompletedProcess(args, 0, 'sha256:'+'1'*64+'\n', '')
        if args[:2] == ['docker', 'run']:
            return subprocess.CompletedProcess(args, 0, 'codex-cli 0.147.0\n'+'2'*64+'  /bin/codex\n', '')
        if '--prepare-readiness' in args:
            frozen = json.loads(Path(args[args.index('--contract')+1]).read_text())
            self.assertEqual(frozen['repetitions'], 3)
            self.assertEqual([t['id'] for t in frozen['selection']], ['first', 'last'])
            self.assertNotIn('OPENAI_API_KEY', kwargs['env'])
            self.assertNotIn('--allow-paid', args)
        return subprocess.CompletedProcess(args, 0, '', '')

    def test_invalid_selection_or_repetitions_never_reach_image_build(self):
        variants = []
        for repetitions in (0, 101, True, 1.5):
            variants.append((str(repetitions), copy.deepcopy(self.selection), repetitions))
        for name, mutate in [
                ('empty', lambda s: s.update(problems=[])),
                ('duplicate', lambda s: s['problems'].append(copy.deepcopy(s['problems'][0]))),
                ('unsafe', lambda s: s['problems'][1].update(id='../last')),
                ('bad-tree', lambda s: s['problems'][1].update(problem_tree_sha='bad')),
                ('duplicate-checkpoint', lambda s: s['problems'][1].update(checkpoints=['cp', 'cp']))]:
            selection = copy.deepcopy(self.selection); mutate(selection)
            variants.append((name, selection, 3))
        for name, selection, repetitions in variants:
            with self.subTest(name=name), patch.dict(os.environ, {'GITHUB_ACTIONS': 'true'}, clear=True), \
                    patch.object(preparation, 'official_definition', side_effect=self.definition), \
                    patch.object(runtime.evaluator, 'install', return_value=runtime.evaluator.identity()), \
                    patch.object(runtime, 'verify_checkout'):
                self.calls.clear(); self.selection_path.write_text(json.dumps(selection))
                try:
                    preparation.prepare(self.root/name, self.upstream, self.problems, self.carry,
                        selection_path=self.selection_path, repetitions=repetitions, execute=self.execute)
                except (ValueError, KeyError, IndexError, StopIteration):
                    pass
                else:
                    self.fail('invalid selection/repetition was accepted')
                self.assertFalse(any(args[0] == 'docker' for args, _ in self.calls),
                                 'invalid cohort reached image build')

    def test_last_problem_must_match_full_ordered_definition_before_build(self):
        original = self.definition
        for mode in ('order', 'settings', 'tree'):
            def definition(*args):
                result = original(*args)
                if args[2]['problem'] == 'last':
                    if mode == 'order': result['checkpoints'] = ['different']
                    if mode == 'settings': result['timeout_seconds'] = 1
                return result
            def execute(args, **kwargs):
                result = self.execute(args, **kwargs)
                if mode == 'tree' and args[-1] == 'HEAD:last':
                    result.stdout = 'c'*40+'\n'
                return result
            with self.subTest(mode=mode), patch.dict(os.environ, {'GITHUB_ACTIONS': 'true'}, clear=True), \
                    patch.object(preparation, 'official_definition', side_effect=definition), \
                    patch.object(runtime.evaluator, 'install', return_value=runtime.evaluator.identity()), \
                    patch.object(runtime, 'verify_checkout'):
                self.calls.clear()
                with self.assertRaises(ValueError):
                    preparation.prepare(self.root/mode, self.upstream, self.problems, self.carry,
                        selection_path=self.selection_path, repetitions=3, execute=execute)
                self.assertFalse(any(args[0] == 'docker' for args, _ in self.calls))

    def test_cli_prepares_every_problem_and_dependency_with_explicit_repetitions(self):
        real_prepare = preparation.prepare
        def injected(*args, **kwargs):
            return real_prepare(*args, **kwargs, execute=self.execute)
        argv = ['prepare', '--output', str(self.root/'prepared'), '--upstream', str(self.upstream),
                '--problems', str(self.problems), '--carry', str(self.carry),
                '--selection', str(self.selection_path), '--repetitions', '3']
        stderr = io.StringIO()
        with patch.dict(os.environ, {'GITHUB_ACTIONS': 'true'}, clear=True), \
                patch.object(preparation, 'official_definition', side_effect=self.definition), \
                patch.object(runtime.evaluator, 'install', return_value=runtime.evaluator.identity()), \
                patch.object(runtime, 'verify_checkout'), patch.object(preparation, 'prepare', side_effect=injected), \
                patch.object(sys, 'argv', argv), contextlib.redirect_stderr(stderr):
            try:
                code = preparation.main()
            except SystemExit as error:
                code = error.code
        self.assertEqual(code, 0, stderr.getvalue())
        frozen = json.loads((self.root/'prepared/contract.json').read_text())
        prepared = json.loads((self.root/'prepared/prepared.json').read_text())
        self.assertEqual(frozen['repetitions'], 3)
        self.assertEqual(frozen['selection'], [{'id': t['id'], 'checkpoints': t['checkpoints']}
                                              for t in self.selection['problems']])
        self.assertEqual(set(prepared['tasks']), {'first', 'last'})
        self.assertEqual(scb.plan(frozen)['expected_records'], 18)
        self.assertEqual(self.warm_commands, [
            ['--with=pytest', '--with=first-dependency==1', 'pytest', '--version'],
            ['--with=pytest', '--with=last-dependency==1', 'pytest', '--version']])
        self.assertEqual(json.loads((self.root/'prepared/selection.json').read_text()), self.selection)
        self.assertEqual(frozen.get('study_selection_sha256'), comparison.fingerprint(self.selection),
                         'preparation did not bind immutable study metadata')


class CohortOutcomeTests(unittest.TestCase):
    def configuration(self):
        config = contract('slopcodebench')
        config.update(runner_commit='af5a9cbc311605b5cceb80200ca9832bdff2d7b1',
                      dataset_commit='9cd9ca3a51c3d3e2a99d2488a25baf73a2204451', repetitions=3)
        config['settings']['budget_policy'] = 'between-slot-soft'
        config['selection'] = [{'id': 'first', 'checkpoints': ['cp1', 'cp2']},
                               {'id': 'last', 'checkpoints': ['cp1']}]
        return config

    def records(self, run_plan, slots=None):
        rows = []
        for slot in slots or run_plan['slots']:
            for cp in slot['checkpoints']:
                native = []
                for actor in ('primary', 'shadow') if slot['name'] == 'compact' else ('primary',):
                    native.extend({**e, 'request_id': slot['namespace']+cp+actor, 'actor': actor}
                                  for e in events())
                rows.append({'namespace': slot['namespace'], 'contract_sha256': run_plan['contract_sha256'],
                    'checkpoint': cp, 'task_id': slot['task_id'], 'arm': slot['name'],
                    'repetition': slot['repetition'], 'status': 'evaluated', 'resolved': True,
                    'events': native, 'quality': {'owned': slot['repetition']}, 'agent_seconds': 1,
                    'task_wall_seconds': 2})
        return rows

    def test_exact_slot_identity_is_required_not_just_namespace(self):
        run_plan = scb.plan(self.configuration())
        for field, value in [('task_id', 'other'), ('arm', 'compact'), ('repetition', 99)]:
            with self.subTest(field=field):
                rows = self.records(run_plan); rows[0][field] = value
                with self.assertRaises(ValueError):
                    comparison.summarize(run_plan, rows)
        for mutate in (lambda p: p.update(expected_records=1),
                       lambda p: p['slots'].append(copy.deepcopy(p['slots'][0]))):
            broken = copy.deepcopy(run_plan); mutate(broken)
            with self.assertRaises(ValueError):
                comparison.summarize(broken, self.records(run_plan))

    def test_malformed_worker_outcomes_stop_before_next_slot_and_remain_diagnostic(self):
        config = self.configuration(); run_plan = scb.plan(config)
        for mode in ('identity', 'missing', 'duplicate'):
            calls = []
            def runner(config, slot, prepared, output):
                calls.append(slot['namespace'])
                rows = self.records(run_plan, [slot])
                if mode == 'identity': rows[0]['arm'] = 'wrong'
                if mode == 'missing': rows.pop()
                if mode == 'duplicate': rows.append(copy.deepcopy(rows[0]))
                return rows
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory, \
                    patch.dict(os.environ, {}, clear=True):
                output = Path(directory)/'run'
                try:
                    result = comparison.dispatch(run_plan, config, {}, output, runner, budget_usd=10)
                except ValueError as error:
                    self.fail(f'worker diagnostics lost instead of fixed-denominator report: {error}; calls={len(calls)}')
                self.assertEqual(len(calls), 1, 'malformed slot permitted further spend')
                self.assertEqual(result['expected_records'], 18)
                self.assertFalse(result['comparison_valid'])
                self.assertEqual(result['arms']['direct']['denominator'], 9)
                self.assertTrue(list(output.glob('invalid-records-*.json')))

    def test_reports_whole_problem_attempts_and_coarse_paired_spread_without_dropping_failures(self):
        config = self.configuration(); run_plan = scb.plan(config)
        paths = []
        def runner(config, slot, prepared, output):
            output.mkdir(); paths.append(output)
            rows = self.records(run_plan, [slot])
            for row in rows:
                # One timed-out checkpoint remains unknown and in the denominator.
                if slot['task_id'] == 'last' and slot['repetition'] == 1 and slot['name'] == 'compact':
                    row.update(status='agent-timeout', resolved=None)
                elif slot['name'] == 'compact' and slot['repetition'] == 0:
                    row['resolved'] = False
                for event in row['events']:
                    if event['event'] == 'completed':
                        event['usage'] = copy.deepcopy(event['usage'])
                        event['usage']['input_tokens'] *= slot['repetition']+1
            return rows
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {}, clear=True):
            result = comparison.dispatch(run_plan, config, {}, Path(directory)/'run', runner, budget_usd=10)
            self.assertEqual(len(paths), 12)
            self.assertEqual(len(set(paths)), 12)
            self.assertEqual(result['expected_records'], 18)
            self.assertEqual(result['arms']['compact']['denominator'], 9)
            self.assertEqual(result['arms']['compact']['terminal_statuses']['agent-timeout'], 1)
            self.assertEqual(len(result.get('problem_attempts', [])), 6,
                             'missing whole-problem/repetition paired reporting')
            attempts = result['problem_attempts']
            self.assertEqual([(a['task_id'], a['repetition']) for a in attempts],
                             [('first', 0), ('last', 0), ('first', 1), ('last', 1), ('first', 2), ('last', 2)])
            for attempt in attempts:
                direct = attempt['arms']['direct']; compact = attempt['arms']['compact']
                self.assertAlmostEqual(compact['estimated_api_usd'], 2*direct['estimated_api_usd'])
                self.assertAlmostEqual(attempt['compact_minus_direct_estimated_api_usd'], direct['estimated_api_usd'])
                self.assertEqual(direct['checkpoint_solve_rate'], 1)
            self.assertIsNone(attempts[3]['arms']['compact']['whole_problem_resolved'])
            self.assertFalse(attempts[0]['arms']['compact']['whole_problem_resolved'])
            self.assertEqual(attempts[0]['arms']['compact']['checkpoint_solve_rate'], 0)
            spread = result['per_problem'][0]['paired_cost_delta_usd']
            values = [a['compact_minus_direct_estimated_api_usd'] for a in attempts if a['task_id'] == 'first']
            self.assertEqual(spread['count'], 3)
            self.assertEqual(spread['unknown'], 0)
            self.assertAlmostEqual(spread['mean'], sum(values)/3)
            self.assertEqual(spread['median'], sorted(values)[1])
            self.assertEqual(spread['min'], min(values)); self.assertEqual(spread['max'], max(values))
            self.assertAlmostEqual(spread['range'], max(values)-min(values))
            self.assertEqual(result['per_problem'][1]['paired_checkpoint_solve_rate_delta']['unknown'], 1)
            self.assertIsNone(result['per_problem'][1]['paired_checkpoint_solve_rate_delta']['mean'])
            self.assertEqual(len(result['checkpoints']), 9)
            self.assertEqual(result['checkpoints'][0]['compact_quality'], {'owned': 0})
            self.assertEqual(json.loads((Path(directory)/'run/report.json').read_text()), result)

    def test_paired_spread_preserves_negative_zero_positive_and_unknown_costs(self):
        config = self.configuration(); run_plan = scb.plan(config)
        rows = self.records(run_plan)
        for row in rows:
            if row['task_id'] != 'first':
                continue
            if row['repetition'] == 0 and row['arm'] == 'direct':
                for event in row['events']:
                    if event['event'] == 'completed': event['usage']['input_tokens'] = 10000
            if row['repetition'] == 1 and row['arm'] == 'compact':
                row['events'] = [e for e in row['events'] if e['actor'] == 'primary']
        result = comparison.summarize(run_plan, rows)
        values = [a['compact_minus_direct_estimated_api_usd'] for a in result['problem_attempts'] if a['task_id'] == 'first']
        self.assertLess(values[0], 0); self.assertEqual(values[1], 0); self.assertGreater(values[2], 0)
        spread = result['per_problem'][0]['paired_cost_delta_usd']
        self.assertEqual(spread['count'], 3); self.assertEqual(spread['unknown'], 0)
        self.assertAlmostEqual(spread['mean'], sum(values)/3)
        self.assertEqual(spread['median'], 0)
        next(r for r in rows if r['task_id'] == 'first' and r['repetition'] == 2 and r['arm'] == 'compact')['events'] = []
        incomplete = comparison.summarize(run_plan, rows)
        self.assertEqual(incomplete['per_problem'][0]['paired_cost_delta_usd']['unknown'], 1)
        self.assertIsNone(incomplete['per_problem'][0]['paired_cost_delta_usd']['mean'])
        self.assertFalse(incomplete['cost_comparison_complete'])
        self.assertGreater(incomplete['arms']['compact']['observed_api_usd_lower_bound'], 0)
        self.assertEqual(incomplete['arms']['compact']['denominator'], 9)

    def test_dispatch_rejects_plan_that_no_longer_matches_contract_before_worker(self):
        config = self.configuration(); run_plan = scb.plan(config)
        run_plan['slots'][0]['checkpoints'] = ['cp1']
        calls = []
        def runner(config, slot, prepared, output):
            calls.append(slot['namespace'])
            return self.records(run_plan, [slot])
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {}, clear=True):
            with self.assertRaises(ValueError):
                comparison.dispatch(run_plan, config, {}, Path(directory)/'run', runner, budget_usd=10)
        self.assertEqual(calls, [])


class CohortWorkflowTests(unittest.TestCase):
    def shell(self, name, env, **kwargs):
        from scripts.test_slopcodebench_workflow import WorkflowShellTests
        fixture = WorkflowShellTests()
        return fixture.shell(name, env, **kwargs)

    def test_workflow_passes_selection_and_repetition_to_real_preparation_boundary(self):
        with tempfile.TemporaryDirectory(prefix='workflow cohort ') as directory:
            root = Path(directory)
            python = root/'python'
            python.write_text('#!'+sys.executable+'\nimport json,os,sys\nfrom pathlib import Path\nPath(os.environ["ARGS"]).write_text(json.dumps(sys.argv[1:]))\n')
            python.chmod(0o755)
            selection = root/'chosen cohort.json'; selection.write_text('{}')
            result = self.shell('Real native images and ALL selected official offline gold evaluations (no secrets)',
                {'SCB_PYTHON': str(python), 'SCB_OUTPUT': str(root/'output'), 'ARGS': str(root/'args'),
                 'SCB_SELECTION': str(selection), 'SCB_REPETITIONS': '3'})
            self.assertEqual(result.returncode, 0, result.stderr)
            args = json.loads((root/'args').read_text())
            self.assertIn('--selection', args, 'workflow dropped explicit selected cohort')
            self.assertEqual(args[args.index('--selection')+1], str(selection))
            self.assertIn('--repetitions', args, 'workflow silently reverted to pilot repetition')
            self.assertEqual(args[args.index('--repetitions')+1], '3')

    def test_terminal_shell_uses_canonical_plan_and_rejects_frozen_provenance_drift(self):
        config = CohortOutcomeTests().configuration()
        selection = {'owned': 'frozen selection'}
        config['study_selection_sha256'] = comparison.fingerprint(selection)
        run_plan = scb.plan(config)
        rows = CohortOutcomeTests().records(run_plan)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); (root/'paired').mkdir(); (root/'prepared').mkdir()
            for name, value in [('paired/plan', run_plan), ('paired/contract', config),
                                ('prepared/contract', config), ('prepared/selection', selection)]:
                (root/(name+'.json')).write_text(json.dumps(value))
            (root/'paired/records.jsonl').write_text(''.join(json.dumps(r)+'\n' for r in rows))
            result = self.shell('Reject incomplete or unpriced comparison (no secrets)', {'SCB_OUTPUT': str(root)})
            self.assertEqual(result.returncode, 0, 'valid replicated denominator rejected: '+result.stderr)
            for name, mutate in [
                    ('paired/plan', lambda v: v.update(expected_records=12)),
                    ('paired/contract', lambda v: v.update(repetitions=1)),
                    ('prepared/selection', lambda v: v.update(owned='changed'))]:
                path = root/(name+'.json'); before = path.read_text(); value = json.loads(before); mutate(value)
                path.write_text(json.dumps(value))
                result = self.shell('Reject incomplete or unpriced comparison (no secrets)', {'SCB_OUTPUT': str(root)})
                self.assertNotEqual(result.returncode, 0, name+' drift accepted')
                path.write_text(before)
