"""Synthetic executable SWE paired boundary; no provider, Docker or dataset download."""
import contextlib
import copy
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from scripts import swebench_comparison as lane
from scripts import benchmark_comparison as comparison
from scripts.test_benchmark_comparison import contract


def events():
    identity = {'actor': 'primary', 'request_id': 'synthetic-request', 'model': 'gpt-6.1-sol', 'service_tier': 'default'}
    return [{**identity, 'event': 'started'}, {**identity, 'event': 'completed', 'latency_ms': 10,
            'usage': {'input_tokens': 100, 'output_tokens': 10, 'input_tokens_details': {'cached_tokens': 20}}}]


class PairedSWEExecutionTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        from scripts.benchmark_budget import SpendingAuthority
        self.budget_state = self.root/'synthetic-budget.json'
        SpendingAuthority.create(self.budget_state, cap_usd='1', cloud_reserve_usd='0',
            cloud_bound={'evidence_kind': 'synthetic-fixture', 'rate_usd_per_hour': '0',
                         'maximum_seconds': 3600, 'cleanup_owner': 'owned-no-cloud-fixture'})
        self.config = contract()
        self.config.update(benchmark='swebench', selection=[{'id': 'owned__synthetic-1', 'checkpoints': ['final']}])
        self.records = [{'instance_id': 'owned__synthetic-1', 'repo': 'owned/synthetic',
                         'base_commit': 'a'*40, 'problem_statement': 'Owned synthetic task',
                         'patch': 'PRIVATE GOLD', 'test_patch': 'PRIVATE TEST'}]
        dataset = self.root/'dataset.json'; dataset.write_text(json.dumps(self.records))
        self.prepared = {'canonical_dataset': str(dataset), 'source_root': str(Path(__file__).resolve().parents[1]),
                         'config': {}, 'harness_bundle': str(self.root/'bundle'), 'gateway_image': 'sha256:'+'1'*64,
                         'carry_image': 'sha256:'+'2'*64, 'evaluation_timeout_seconds': 30,
                         'tasks': {'owned__synthetic-1': {'agent_image': {'image_id': 'sha256:'+'3'*64},
                             'source_task_image': 'owned-synthetic:prepared', 'evaluator_image': {'image_id': 'sha256:'+'4'*64}}}}

    def execute(self, *, evaluator_failure=False, receipt_missing=False, cleanup_failure=False, capture_failure=False, artifact_attack=None):
        launches = []
        clones = []
        original_materialize = lane.swe.materialize
        def clone(repo, commit, destination):
            clones.append(str(destination)); destination.mkdir(parents=True)
        def materialize(**kwargs):
            return original_materialize(**kwargs, clone=clone)
        def agent(**kwargs):
            launches.append((os.environ['CARRY_PROXY_MODE'], os.environ['BENCHMARK_TRUSTED_DIRECT'], kwargs))
            public = json.loads((kwargs['task_input']/'task.json').read_text())
            self.assertNotIn('patch', public); self.assertNotIn('test_patch', public)
            if not receipt_missing:
                state = kwargs['output'].with_name('agent-proxy-state'); state.mkdir()
                state.joinpath('benchmark-events.jsonl').write_text(''.join(json.dumps(e)+'\n' for e in events()))
            if capture_failure:
                import subprocess
                from scripts import proxy_benchmark
                log = ''.join('BENCHMARK_CONTEXT_EVENT '+json.dumps(e)+'\n' for e in events())
                network = {'proxy': 'owned', 'state_dir': str(state)}
                summary = proxy_benchmark.capture_evidence(network,
                    execute=lambda *a, **kw: subprocess.CompletedProcess(a, 1, log, 'synthetic log loss'))
                self.assertFalse(summary['ledger_complete'])
                # A later snapshot is not proof that the missing prefix was recovered.
                proxy_benchmark.capture_evidence(network,
                    execute=lambda *a, **kw: subprocess.CompletedProcess(a, 0, log, ''))
            if artifact_attack is not None:
                import subprocess
                sentinel = self.root/'host-only-sentinel'
                sentinel.write_text('HARMLESS_HOST_SENTINEL_NOT_A_CREDENTIAL')
                def worker(argv, **kw):
                    (kwargs['output']/'final.patch').symlink_to(sentinel)
                    if artifact_attack:
                        raise subprocess.CalledProcessError(1, argv)
                    return subprocess.CompletedProcess(argv, 0)
                with patch.object(lane.swe, 'agent_docker_command', return_value=['owned-fixture']), \
                     patch.object(lane.swe.subprocess, 'run', side_effect=worker):
                    return lane.swe.run_agent(instance_id=kwargs['instance_id'], harness='codex', image='owned',
                        repo=kwargs['repo'], harness_bundle=kwargs['harness_bundle'], task_input=kwargs['task_input'],
                        output=kwargs['output'], model=kwargs['model'], reasoning=kwargs['reasoning'],
                        network='owned', proxy_ip='127.0.0.1')
            if cleanup_failure:
                raise RuntimeError('synthetic fail-closed cleanup after native billing')
            return {'instance_id': kwargs['instance_id'], 'harness': 'codex', 'status': 'agent-completed',
                    'patch': 'SYNTHETIC GENERATED PATCH', 'agent_seconds': 0.01, 'setup_seconds': 0.01}
        def evaluate(**kwargs):
            self.assertFalse(any(k.startswith('OPENAI_') for k in kwargs['environment']))
            prediction = json.loads(kwargs['predictions'].read_text())
            self.assertEqual(prediction['model_patch'], 'SYNTHETIC GENERATED PATCH')
            self.assertEqual(kwargs['max_workers'], 1)
            self.assertIsNotNone(kwargs['process_timeout_seconds'])
            if evaluator_failure:
                raise RuntimeError('synthetic evaluator lost')
            task = kwargs['instance_ids'][0]
            kwargs['output'].joinpath('report.json').write_text(json.dumps({
                'completed_ids': [task], 'resolved_ids': [task], 'unresolved_ids': [],
                'empty_patch_ids': [], 'error_ids': [], 'incomplete_ids': []}))
        config = self.root/'contract.json'; config.write_text(json.dumps(self.config))
        prepared = self.root/'prepared.json'; prepared.write_text(json.dumps(self.prepared))
        with patch.object(lane, 'preflight', side_effect=lambda c,p: comparison.build_plan(c)), \
             patch.object(lane.swe, 'materialize', side_effect=materialize), \
             patch.object(lane.swe, 'run_isolated_agent', side_effect=agent), \
             patch.object(lane.swe, 'run_official_evaluation', side_effect=evaluate), \
             patch.dict(os.environ, {'OPENAI_API_KEY': 'synthetic-secret', 'OPENAI_SECRET_FILE': '/synthetic/private',
                                     'BENCHMARK_BUDGET_STATE': str(self.budget_state)}), \
             contextlib.redirect_stdout(io.StringIO()):
            lane.main(['--contract', str(config), '--prepared', str(prepared), '--run', '--allow-paid',
                       '--budget-usd', '1', '--output', str(self.root/'paired')])
        report = json.loads((self.root/'paired/report.json').read_text())
        return report, launches, clones

    def test_production_command_dispatches_both_arms_and_grades_patches(self):
        report, launches, clones = self.execute()
        self.assertEqual([(mode, direct) for mode, direct, _ in launches], [('disabled', '1'), ('compact', '0')])
        self.assertEqual(len(set(clones)), 2)
        self.assertEqual(report['paired']['both_resolved'], 1)
        self.assertTrue(report['comparison_valid'])
        self.assertTrue(report['cost_comparison_complete'])
        self.assertEqual(report['evidence_kind'], 'synthetic-fixture')
        self.assertEqual(launches[0][2]['model'], launches[1][2]['model'])
        self.assertNotEqual(launches[0][2]['output'], launches[1][2]['output'])

    def test_evaluator_loss_stops_further_dispatch_with_fixed_denominator(self):
        report, launches, _ = self.execute(evaluator_failure=True)
        self.assertEqual(len(launches), 1)
        self.assertEqual(report['expected_records'], 2)
        self.assertFalse(report['comparison_valid'])
        self.assertEqual(report['arms']['compact']['terminal_statuses'], {'not-run': 1})
        self.assertIsNone(report['arms']['compact']['estimated_api_usd'])

    def test_missing_native_receipts_are_unknown_and_stop_pair(self):
        report, launches, _ = self.execute(receipt_missing=True)
        self.assertEqual(len(launches), 1)
        self.assertFalse(report['cost_comparison_complete'])
        self.assertIsNone(report['arms']['direct']['estimated_api_usd'])

    def test_cleanup_failure_retains_native_spend_and_stops_pair(self):
        report, launches, _ = self.execute(cleanup_failure=True)
        self.assertEqual(len(launches), 1)
        self.assertGreater(report['arms']['direct']['estimated_api_usd'], 0)
        self.assertFalse(report['comparison_valid'])
        self.assertEqual(report['arms']['compact']['terminal_statuses'], {'not-run': 1})

    def test_successful_worker_symlink_never_enters_predictions(self):
        self.check_artifact_attack(False)

    def test_failed_worker_symlink_never_enters_predictions(self):
        self.check_artifact_attack(True)

    def check_artifact_attack(self, failed):
        report, launches, _ = self.execute(artifact_attack=failed)
        published = [p for p in (self.root/'paired').rglob('*') if p.is_file() and not p.is_symlink()]
        self.assertFalse(any('HARMLESS_HOST_SENTINEL_NOT_A_CREDENTIAL' in p.read_text() for p in published))
        self.assertEqual(len(launches), 1)
        self.assertEqual(report['arms']['direct']['terminal_statuses'], {'infrastructure-error': 1})
        self.assertGreater(report['arms']['direct']['observed_api_usd_lower_bound'], 0)
        self.assertFalse(list((self.root/'paired').rglob('predictions.jsonl')))

    def test_failed_capture_prefix_stops_dispatch_and_retains_known_billing(self):
        report, launches, _ = self.execute(capture_failure=True)
        self.assertEqual(len(launches), 1)
        self.assertIsNone(report['arms']['direct']['estimated_api_usd'])
        self.assertGreater(report['arms']['direct']['observed_api_usd_lower_bound'], 0)

    def test_preflight_accepts_qualified_inputs_and_rejects_prompt_drift(self):
        config = copy.deepcopy(self.config)
        config.update(runner_commit=config['source_commit'], dataset_commit=lane.swe.DATASET_REVISION)
        config['settings'].update(retries=0, concurrency=1, native_config={},
            prompt_sha256=comparison.fingerprint([[r['instance_id'], lane.swe.task_prompt(r)] for r in self.records]),
            gateway_digest=self.prepared['gateway_image'], carry_digest=self.prepared['carry_image'])
        bundle = Path(self.prepared['harness_bundle']); (bundle/'bin').mkdir(parents=True)
        for name in ('adapter', 'codex'):
            (bundle/'bin'/name).write_text('synthetic')
        prepared = copy.deepcopy(self.prepared)
        prepared['harness_bundle_sha256'] = lane.bundle_digest(bundle)
        prepared['config'].update(PREPARED_HARNESS_IMAGES='synthetic-manifest',
            TASK_IMAGE_CATALOG='owned.invalid/catalog@'+config['settings']['environment_digest'])
        def output(args, **kwargs):
            if args[:3] == ['docker', 'image', 'inspect']:
                return prepared['tasks']['owned__synthetic-1']['evaluator_image']['image_id']+'\n'
            return 'codex-cli '+config['settings']['codex_version']+'\n'+config['settings']['codex_sha256']+'  codex\n'
        with patch.object(lane, 'version', return_value='4.1.0'), patch.object(lane, 'verify_checkout'), \
             patch.object(lane, 'canonical_records', return_value=self.records), \
             patch.object(lane.swe, 'build_images', return_value={'codex': {'image_id': config['settings']['harness_digest']}}), \
             patch.object(lane.subprocess, 'check_output', side_effect=output), patch.object(lane.subprocess, 'run'):
            self.assertEqual(lane.preflight(config, prepared)['expected_records'], 2)
            config['settings']['prompt_sha256'] = '0'*64
            with self.assertRaisesRegex(ValueError, 'prompts differ'):
                lane.preflight(config, prepared)

    def test_missing_consent_rejects_before_preflight(self):
        config = self.root/'contract.json'; config.write_text(json.dumps(self.config))
        with patch.object(lane, 'preflight') as check, contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                lane.main(['--contract', str(config), '--run'])
            check.assert_not_called()


class HostArtifactBoundaryTests(unittest.TestCase):
    def test_producer_never_follows_preexisting_trace_link(self):
        import subprocess
        import sys
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); repo = root/'repo'; repo.mkdir()
            subprocess.run(['git', 'init', '-q', str(repo)], check=True)
            subprocess.run(['git', '-C', str(repo), '-c', 'user.name=fixture', '-c',
                            'user.email=fixture@example.invalid', 'commit', '-qm', 'seed', '--allow-empty'], check=True)
            output = root/'output'; output.mkdir()
            sentinel = root/'host-only'; sentinel.write_text('HARMLESS HOST SENTINEL')
            (output/'trace.log').symlink_to(sentinel)
            prompt = root/'prompt'; prompt.write_text('owned synthetic fixture')
            env = {k:v for k,v in os.environ.items() if not k.startswith(('OPENAI_', 'CARRY_', 'BENCHMARK_', 'AGENT_'))}
            env.update(HOME=str(root), OPENAI_API_KEY='synthetic-not-a-key',
                       OPENAI_BASE_URL='http://127.0.0.1:1', BENCHMARK_WORKSPACE=str(repo),
                       AGENT_COMMAND=sys.executable+' -c "print(123)"')
            result = subprocess.run([sys.executable, str(lane.swe.pathlib.Path(lane.swe.__file__).parents[1]/
                'containers/swebench-harness/entrypoint.py'), 'run', '--model', 'gpt-6-luna',
                '--reasoning', 'low', '--prompt', str(prompt), '--output', str(output)],
                env=env, capture_output=True, timeout=10)
            self.assertEqual(sentinel.read_text(), 'HARMLESS HOST SENTINEL')
            self.assertNotEqual(result.returncode, 0)

    def test_reader_rejects_escape_special_hardlink_and_oversize(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); output = root/'output'; output.mkdir()
            sentinel = root/'host'; sentinel.write_text('HARMLESS HOST SENTINEL')
            (output/'link').symlink_to(sentinel)
            (output/'nested').symlink_to(root, target_is_directory=True)
            os.mkfifo(output/'fifo')
            os.link(sentinel, output/'hardlink')
            (output/'large').write_bytes(b'12345')
            for name in ('link', 'nested/host', 'fifo', 'hardlink', '../host'):
                with self.subTest(name=name), self.assertRaises(lane.swe.AgentArtifactError):
                    lane.swe.agent_artifact_bytes(output, name)
            with self.assertRaises(lane.swe.AgentArtifactError):
                lane.swe.agent_artifact_bytes(output, 'large', limit=4)
            self.assertEqual(lane.swe.agent_artifact_bytes(output, 'large', limit=5), b'12345')

    def test_reader_no_follow_at_open_and_stable_descriptor_after_open(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); output = root/'output'; output.mkdir()
            sentinel = root/'host'; sentinel.write_text('HARMLESS HOST SENTINEL')
            target = output/'final.patch'; target.write_text('OWNED PATCH')
            original = os.open
            def before(path, flags, *args, **kwargs):
                if path == 'final.patch':
                    target.unlink(); target.symlink_to(sentinel)
                return original(path, flags, *args, **kwargs)
            with patch.object(lane.swe.os, 'open', side_effect=before):
                with self.assertRaises(lane.swe.AgentArtifactError):
                    lane.swe.agent_artifact_bytes(output, 'final.patch')
            target.unlink(); target.write_text('OWNED PATCH')
            def after(path, flags, *args, **kwargs):
                fd = original(path, flags, *args, **kwargs)
                if path == 'final.patch':
                    target.rename(output/'original'); target.symlink_to(sentinel)
                return fd
            with patch.object(lane.swe.os, 'open', side_effect=after):
                self.assertEqual(lane.swe.agent_artifact_bytes(output, 'final.patch'), b'OWNED PATCH')


if __name__ == '__main__':
    unittest.main()
