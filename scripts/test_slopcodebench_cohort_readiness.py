"""Actual canonical readiness/preflight with synthetic external grading; no official gold proof."""
import contextlib
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

from scripts import slopcodebench as scb
from scripts import slopcodebench_runtime as runtime
from scripts import benchmark_comparison as comparison
from scripts import test_slopcodebench_cohort as cohort_fixture


PREFLIGHT = runtime.preflight


class CohortReadinessTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='cohort readiness ')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.config = cohort_fixture.CohortOutcomeTests().configuration()
        self.selection = {'problems': copy.deepcopy(self.config['selection'])}
        self.config['study_selection_sha256'] = comparison.fingerprint(self.selection)
        self.prepared = {'problems_root': str(self.root/'problems'), 'tasks': {}}
        self.calls = []
        for name, key in [('agent_image', 'harness_digest'), ('evaluator_image', 'environment_digest'),
                          ('gateway_image', 'gateway_digest'), ('carry_image', 'carry_digest')]:
            self.prepared[name] = self.config['settings'][key]
        for key, text in [('prompt', 'prompt'), ('environment', '{}'), ('selection', json.dumps(self.selection))]:
            path = self.root/(key+'.json'); path.write_text(text); self.prepared[key] = str(path)
        self.config['settings']['prompt_sha256'] = hashlib.sha256(b'prompt').hexdigest()
        for task in self.config['selection']:
            for cp in task['checkpoints']:
                directory = self.root/'problems'/task['id']/'solutions'/cp
                directory.mkdir(parents=True); (directory/'owned.py').write_text('synthetic gold')
            self.prepared['tasks'][task['id']] = {
                'definition_sha256': runtime.tree_digest(self.root/'problems'/task['id']),
                'readiness': str(self.root/'readiness'/f'{task["id"]}.json')}

    def execute(self, args, **kwargs):
        self.calls.append(args)
        out = ('codex-cli '+self.config['settings']['codex_version']+'\n'+
               self.config['settings']['codex_sha256']+'  /bin/codex\n') if args[:2] == ['docker', 'run'] else ''
        return subprocess.CompletedProcess(args, 0, out, '')

    def preflight(self, contract, prepared, **kwargs):
        return PREFLIGHT(contract, prepared, execute=self.execute, verify_sources=False, **kwargs)

    def test_changed_selection_is_rejected_before_external_runtime_or_model(self):
        Path(self.prepared['selection']).write_text(json.dumps({'changed': True}))
        with self.assertRaisesRegex(ValueError, 'selection'):
            self.preflight(self.config, self.prepared, require_readiness=False)
        self.assertEqual(self.calls, [])

    def test_last_problem_gold_failure_blocks_real_run_cli_before_runner(self):
        graded = []
        fail = True
        class Policy:
            def __init__(self, value): self.value = value
            def check(self, passed, totals): return passed == totals
        class Report:
            infrastructure_failure = False
            pytest_collected = 1
            tests = [types.SimpleNamespace(status='passed')]
            def __init__(self, passed):
                self.pass_counts = {'Core': int(passed)}; self.total_counts = {'Core': 1}
            def save(self, path):
                path.mkdir(parents=True, exist_ok=True); (path/'synthetic.json').write_text(json.dumps(self.pass_counts))
        def official_components(prepared, config, task_id):
            task = next(t for t in config['selection'] if t['id'] == task_id)
            checkpoints = [types.SimpleNamespace(name=cp) for cp in task['checkpoints']]
            problem = types.SimpleNamespace(name=task_id, iterate_checkpoints=lambda: iter(checkpoints))
            return problem, None, Policy('any')
        def grade(**kwargs):
            identity = (kwargs['problem'].name, kwargs['checkpoint'].name); graded.append(identity)
            return Report(not (fail and identity == ('last', 'cp1')))
        evaluation = types.ModuleType('slop_code.evaluation')
        evaluation.run_checkpoint = grade; evaluation.PassPolicy = Policy
        with patch.dict(sys.modules, {'slop_code.evaluation': evaluation}), \
                patch.object(runtime, 'official_components', side_effect=official_components), \
                patch.object(runtime, 'protected_runtimes', side_effect=lambda *a: contextlib.nullcontext()):
            with self.assertRaisesRegex(ValueError, 'gold readiness failed'):
                runtime.prepare_readiness(self.config, self.prepared)
            self.assertEqual(graded, [('first', 'cp1'), ('first', 'cp2'), ('last', 'cp1')])
            self.assertTrue(Path(self.prepared['tasks']['first']['readiness']).exists())
            self.assertFalse(Path(self.prepared['tasks']['last']['readiness']).exists())
            contract_path = self.root/'contract.json'; contract_path.write_text(json.dumps(self.config))
            prepared_path = self.root/'prepared.json'; prepared_path.write_text(json.dumps(self.prepared))
            argv = ['scb', '--contract', str(contract_path), '--prepared', str(prepared_path),
                    '--run', '--allow-paid', '--budget-usd', '10', '--output', str(self.root/'run')]
            with patch.object(sys, 'argv', argv), patch.dict(os.environ, {}, clear=True), \
                    patch.object(runtime, 'preflight', side_effect=self.preflight), \
                    patch.object(runtime, 'run_slot') as runner, contextlib.redirect_stderr(io.StringIO()):
                # Bind original preflight before patching, avoiding recursion.
                with self.assertRaises(SystemExit) as error:
                    scb.main()
                self.assertEqual(error.exception.code, 2)
                runner.assert_not_called()
            self.assertFalse((self.root/'run').exists())
            fail = False
            # Successful model-free readiness covers all ordered checkpoints once,
            # not once per repetition: immutable dependencies are shared setup.
            graded.clear()
            self.assertEqual(runtime.prepare_readiness(self.config, self.prepared)['model_requests'], 0)
            self.assertEqual(graded, [('first', 'cp1'), ('first', 'cp2'), ('last', 'cp1')])
            self.assertEqual(self.preflight(self.config, self.prepared)['expected_records'], 18)
