"""Preparation behavior at a fake external-process boundary; not Docker proof."""
import importlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from scripts import slopcodebench_runtime as runtime


class PreparationTests(unittest.TestCase):
    def test_prepares_real_contract_then_runs_only_gold_cli(self):
        self.assertIsNotNone(importlib.util.find_spec('scripts.slopcodebench_prepare'),
                             'missing hosted official SCB preparation helper')
        prepare = importlib.import_module('scripts.slopcodebench_prepare')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            upstream, problems = root/'upstream', root/'problems'
            upstream.mkdir(); problems.mkdir()
            carry = root/'carry'; carry.write_bytes(b'owned fake Carry binary')
            problem = problems/'cfgpipe'; problem.mkdir()
            (problem/'config.yaml').write_text('owned fake official component input')
            prompt = upstream/'prompt.jinja'; prompt.write_text('{{ spec }}\n')
            calls = []
            image = 'sha256:'+'1'*64
            native_hash = '2'*64

            def execute(args, **kwargs):
                calls.append((args, kwargs))
                if args[0] == 'git':
                    if args[-1] == 'HEAD:cfgpipe':
                        return subprocess.CompletedProcess(args, 0, '76e7313b7594277d6347fffb850d54e01a30bc45\n', '')
                    return subprocess.CompletedProcess(args, 0, '3'*40+'\n', '')
                if args[:2] == ['docker', 'build']:
                    context = Path(args[-1])
                    self.assertEqual((context/'carry').read_bytes(), carry.read_bytes())
                    self.assertFalse((context/'config.yaml').exists())
                    self.assertIn('--with=owned-dependency==1', (context/'warm-cache.sh').read_text())
                if args[:3] == ['docker', 'image', 'inspect']:
                    return subprocess.CompletedProcess(args, 0, image+'\n', '')
                if args[:2] == ['docker', 'run']:
                    self.assertIn('--network=none', args)
                    return subprocess.CompletedProcess(args, 0, 'codex-cli 0.147.0\n'+native_hash+'  /bin/codex\n', '')
                return subprocess.CompletedProcess(args, 0, '', '')

            selection = json.loads(prepare.SELECTION.read_text())
            definition = {'prompt': prompt, 'pass_policy': 'any', 'timeout_seconds': 7200,
                          'environment': {'type': 'docker', 'docker': {'image': 'unprepared'},
                              'commands': {'command': 'uv run', 'entry_file': '{entry_file}.py'}},
                          'test_dependencies': ['pytest', 'owned-dependency==1'],
                          'checkpoints': selection['checkpoints']}
            with patch.dict(os.environ, {'GITHUB_ACTIONS': 'true'}, clear=True), \
                    patch.object(prepare, 'official_definition', return_value=definition), \
                    patch.object(runtime, 'verify_checkout') as verify:
                contract, prepared = prepare.prepare(root/'prepared', upstream, problems, carry,
                                                     execute=execute)
            self.assertEqual(verify.call_count, 3)
            self.assertEqual(contract['selection'], [{'id': 'cfgpipe', 'checkpoints': selection['checkpoints']}])
            self.assertEqual(contract['settings']['model'], 'gpt-6-luna')
            self.assertEqual(contract['settings']['reasoning'], 'medium')
            self.assertEqual(contract['settings']['scb_pass_policy'], 'any')
            self.assertEqual(contract['settings']['scb_correctness_policy'], 'all-cases')
            self.assertEqual(contract['settings']['timeout_seconds'], 7200)
            self.assertEqual(contract['settings']['codex_version'], '0.147.0')
            self.assertEqual(contract['settings']['codex_sha256'], native_hash)
            self.assertEqual(contract['proxy']['CARRY_PROXY_CLASSIFIER_MODEL'], 'gpt-6-luna')
            self.assertEqual(contract['proxy']['CARRY_PROXY_CLASSIFIER_EFFORT'], 'low')
            self.assertEqual(prepared['agent_image'], image)
            self.assertEqual(json.loads((root/'prepared/contract.json').read_text()), contract)
            gold = calls[-1][0]
            self.assertIn('--prepare-readiness', gold)
            self.assertNotIn('--run', gold)
            self.assertNotIn('--allow-paid', gold)
            self.assertEqual(calls[-1][1]['env'].get('OPENAI_API_KEY'), None)
            self.assertEqual(len(contract['selection'][0]['checkpoints']), 6)
            self.assertEqual(json.loads(Path(prepared['environment']).read_text())['commands']['entry_file'],
                             '{entry_file}.py')
            def fail_gold(args, **kwargs):
                if '--prepare-readiness' in args:
                    raise subprocess.CalledProcessError(1, args)
                return execute(args, **kwargs)
            with patch.dict(os.environ, {'GITHUB_ACTIONS': 'true'}, clear=True), \
                    patch.object(prepare, 'official_definition', return_value=definition), \
                    patch.object(runtime, 'verify_checkout'):
                with self.assertRaises(subprocess.CalledProcessError):
                    prepare.prepare(root/'failed-gold', upstream, problems, carry, execute=fail_gold)
            self.assertFalse((root/'failed-gold/readiness/cfgpipe.json').exists())

    @unittest.skipUnless(os.environ.get('SCB_OFFICIAL_REPORT_MODULE'), 'explicit pinned report component required')
    def test_all_tests_correctness_is_not_progression_policy(self):
        from scripts import slopcodebench as scb
        from scripts.test_benchmark_comparison import contract
        official = scb.official_report(Path(os.environ['SCB_OFFICIAL_REPORT_MODULE']))
        config = contract('slopcodebench')
        config.update(runner_commit='af5a9cbc311605b5cceb80200ca9832bdff2d7b1',
                      dataset_commit='9cd9ca3a51c3d3e2a99d2488a25baf73a2204451')
        config['settings'].update(scb_pass_policy='any', scb_correctness_policy='all-cases')
        config['selection'] = [{'id': 'cfgpipe', 'checkpoints': ['checkpoint_'+str(i) for i in range(1, 7)]}]
        run_plan = scb.plan(config)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for slot in run_plan['slots']:
                for checkpoint in slot['checkpoints']:
                    report = dict(schema_version=1, problem_name='cfgpipe', problem_version=1,
                        checkpoint_name=checkpoint, checkpoint_version=1, duration=1,
                        entrypoint='python owned.py', tests={'Core': {'passed': ['one'], 'failed': ['two']}},
                        pass_counts={'Core': 1}, total_counts={'Core': 2}, pytest_exit_code=1,
                        pytest_collected=2, infrastructure_failure=False)
                    destination = root/slot['namespace']/'cfgpipe'/checkpoint
                    destination.mkdir(parents=True)
                    (destination/'evaluation.json').write_text(json.dumps(report))
            scored = scb.inspect_scores(run_plan, config, root, Path(os.environ['SCB_OFFICIAL_REPORT_MODULE']))
            self.assertEqual(len(scored['checkpoints']), 12)
            self.assertEqual([r['resolved'] for r in scored['checkpoints']], [False]*12,
                             'native ANY progression is not all-tests correctness')
            self.assertTrue(all(r['progression_passed'] for r in scored['checkpoints']))
            prepared = {'tasks': {'cfgpipe': {'definition_sha256': 'x'}}, 'evaluator_image': 'y',
                        'environment': str(root/'env.yaml')}
            (root/'env.yaml').write_text('owned')
            first = runtime.readiness_binding(config, prepared, config['selection'][0])
            config['settings']['scb_correctness_policy'] = 'any-case'
            second = runtime.readiness_binding(config, prepared, config['selection'][0])
            self.assertNotEqual(first, second)
            config['settings']['scb_correctness_policy'] = 'all-cases'
            destination = root/run_plan['slots'][0]['namespace']/'cfgpipe'/'checkpoint_1'/'evaluation.json'
            skipped = json.loads(destination.read_text())
            skipped.update(tests={'Core': {'skipped': ['one']}}, pass_counts={}, total_counts={},
                           pytest_exit_code=0, pytest_collected=1)
            destination.write_text(json.dumps(skipped))
            skipped_score = scb.inspect_scores(run_plan, config, root,
                                               Path(os.environ['SCB_OFFICIAL_REPORT_MODULE']))
            self.assertEqual(skipped_score['checkpoints'][0]['status'], 'evaluation-error',
                             'collection plus skips must not fabricate vacuous all-tests correctness')

    @unittest.skipUnless(os.environ.get('SCB_OFFICIAL_REPORT_MODULE'), 'explicit pinned report component required')
    def test_readiness_requires_every_gold_test_before_marking_ready(self):
        from contextlib import nullcontext
        import sys
        import types
        from scripts import slopcodebench as scb
        from scripts.test_benchmark_comparison import contract
        report_type = scb.official_report(Path(os.environ['SCB_OFFICIAL_REPORT_MODULE']))
        policy_type = sys.modules[report_type.__module__].PassPolicy
        config = contract('slopcodebench')
        config['settings'].update(scb_pass_policy='any', scb_correctness_policy='all-cases')
        config['selection'] = [{'id': 'cfgpipe', 'checkpoints': ['checkpoint_'+str(i) for i in range(1, 7)]}]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            prepared = {'problems_root': str(root/'problems'), 'environment': str(root/'env.yaml'),
                'evaluator_image': 'owned-image', 'tasks': {'cfgpipe': {'definition_sha256': 'owned',
                'readiness': str(root/'readiness/cfgpipe.json')}}}
            (root/'env.yaml').write_text('owned')
            checkpoints = [types.SimpleNamespace(name=cp) for cp in config['selection'][0]['checkpoints']]
            for cp in checkpoints:
                (root/'problems/cfgpipe/solutions'/cp.name).mkdir(parents=True)
            problem = types.SimpleNamespace(iterate_checkpoints=lambda: iter(checkpoints))
            calls = []
            passed = False
            def run_checkpoint(**kwargs):
                calls.append(kwargs['checkpoint'].name)
                destination = root/'owned-report'
                destination.mkdir(exist_ok=True)
                (destination/'evaluation.json').write_text(json.dumps(dict(schema_version=1,
                    problem_name='cfgpipe', problem_version=1, checkpoint_name=kwargs['checkpoint'].name,
                    checkpoint_version=1, duration=1, entrypoint='python owned.py',
                    tests={'Core': {'passed': ['one'], 'failed': [] if passed else ['two']}},
                    pass_counts={'Core': 1}, total_counts={'Core': 1 if passed else 2},
                    pytest_exit_code=0 if passed else 1, pytest_collected=1 if passed else 2,
                    infrastructure_failure=False)))
                return report_type.from_dir(destination)
            evaluation = types.ModuleType('slop_code.evaluation')
            evaluation.run_checkpoint = run_checkpoint
            evaluation.PassPolicy = policy_type
            with patch.dict(sys.modules, {'slop_code.evaluation': evaluation}), \
                    patch.object(runtime, 'official_components', return_value=(problem, None, policy_type('any'))), \
                    patch.object(runtime, 'protected_runtimes', side_effect=lambda *a: nullcontext()):
                with self.assertRaisesRegex(ValueError, 'gold readiness failed'):
                    runtime.prepare_readiness(config, prepared)
                self.assertFalse(Path(prepared['tasks']['cfgpipe']['readiness']).exists())
                passed = True; calls.clear()
                self.assertEqual(runtime.prepare_readiness(config, prepared)['model_requests'], 0)
                self.assertEqual(calls, config['selection'][0]['checkpoints'])
                receipt = json.loads(Path(prepared['tasks']['cfgpipe']['readiness']).read_text())
                self.assertEqual(len(receipt['reports']), 6)
                self.assertEqual(receipt['binding']['scb_correctness_policy'], 'all-cases')

    def test_soft_budget_runs_pair_without_escrow_and_preserves_slots_on_failure(self):
        from scripts import slopcodebench as scb
        from scripts import benchmark_comparison as comparison
        from scripts.test_benchmark_comparison import contract
        from scripts.test_swebench_comparison import events
        config = contract('slopcodebench')
        config.update(runner_commit='af5a9cbc311605b5cceb80200ca9832bdff2d7b1',
                      dataset_commit='9cd9ca3a51c3d3e2a99d2488a25baf73a2204451')
        config['settings']['budget_policy'] = 'between-slot-soft'
        config['selection'] = [{'id': 'cfgpipe', 'checkpoints': ['checkpoint_'+str(i) for i in range(1, 7)]}]
        run_plan = scb.plan(config)
        for fail in (False, True):
            calls = []
            def runner(contract, slot, prepared, output):
                calls.append(slot['name'])
                if fail:
                    raise ValueError('owned failed external worker')
                return [{'namespace': slot['namespace'], 'contract_sha256': run_plan['contract_sha256'],
                    'checkpoint': cp, 'task_id': slot['task_id'], 'arm': slot['name'], 'status': 'evaluated',
                    'resolved': True, 'quality': {},
                    'events': [{**event, 'request_id': slot['name']+'-'+cp} for event in events()]}
                    for i, cp in enumerate(slot['checkpoints'])]
            with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {}, clear=True):
                result = comparison.dispatch(run_plan, config, {}, Path(directory)/'run', runner, budget_usd=10)
                rows = [json.loads(line) for line in (Path(directory)/'run/records.jsonl').read_text().splitlines()]
                self.assertEqual(len(rows), 12)
                self.assertEqual(calls, ['direct'] if fail else ['direct', 'compact'])
                self.assertEqual(result['budget_kind'], 'between-slot-soft-not-hard-cap')
                if fail:
                    self.assertEqual([row['status'] for row in rows[6:]], ['not-run']*6)

    def test_rejects_provider_key_before_external_work(self):
        self.assertIsNotNone(importlib.util.find_spec('scripts.slopcodebench_prepare'),
                             'missing hosted official SCB preparation helper')
        prepare = importlib.import_module('scripts.slopcodebench_prepare')
        calls = []
        with patch.dict(os.environ, {'GITHUB_ACTIONS': 'true', 'OPENAI_API_KEY': 'owned-canary'}, clear=True):
            with self.assertRaisesRegex(ValueError, 'credential'):
                prepare.prepare(Path('/unused'), Path('/unused'), Path('/unused'), Path('/unused'),
                                execute=lambda *a, **kw: calls.append(a))
        self.assertEqual(calls, [])


if __name__ == '__main__':
    unittest.main()
