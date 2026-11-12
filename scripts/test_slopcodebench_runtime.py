"""Real pinned SCB runner/evaluator and Docker isolation; owned fake native/provider.

Run ONLY in disposable hosted CI. All reported scores/usage are synthetic fixtures.
"""
import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest
from unittest.mock import Mock, patch

from scripts import slopcodebench_fixture as fixture
from scripts import slopcodebench_runtime as runtime
from scripts import benchmark_comparison as comparison


@unittest.skipUnless(os.environ.get('CARRY_SCB_HOSTED_FIXTURE') == '1', 'disposable hosted Docker fixture only')
class OfficialProtectedRuntimeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = Path(os.environ['SCB_FIXTURE_OUTPUT'])
        cls.contract, cls.prepared, cls.provider = fixture.prepare(
            cls.root, Path(os.environ['SCB_UPSTREAM_ROOT']), Path(os.environ['SCB_CARRY_BINARY']))

    def test_actual_official_gold_and_paired_native_checkpoint_lifecycle(self):
        contract, prepared = self.contract, self.prepared
        runtime.preflight(contract, prepared, verify_sources=False, require_readiness=False)
        readiness = runtime.prepare_readiness(contract, prepared)
        self.assertEqual(readiness['model_requests'], 0)
        run_plan = runtime.preflight(contract, prepared, verify_sources=False)
        from scripts.benchmark_budget import SpendingAuthority
        budget_state = self.root/'synthetic-combined-budget.json'
        SpendingAuthority.create(budget_state, cap_usd='10', cloud_reserve_usd='0',
            cloud_bound={'evidence_kind': 'synthetic-fixture', 'rate_usd_per_hour': '0',
                         'maximum_seconds': 3600, 'cleanup_owner': 'owned-hosted-no-cloud-fixture'})
        frozen_copies = []
        publish = runtime.swebench_smoke.publish_agent_artifacts
        def publish_while_frozen(source, destination, **kwargs):
            if Path(destination).parent.name.startswith('scb-owned-snapshot-'):
                ids = subprocess.check_output(['docker', 'ps', '-q', '--filter', 'name=carry-scb-'], text=True).split()
                for identity in ids:
                    self.assertEqual(subprocess.check_output(
                        ['docker', 'inspect', '--format', '{{.State.Paused}}', identity], text=True).strip(), 'true')
                frozen_copies.append(ids)
            return publish(source, destination, **kwargs)
        with fixture.FixtureNetwork(self.provider) as network, \
             patch.dict(os.environ, {'BENCHMARK_BUDGET_STATE': str(budget_state)}), \
             patch.object(runtime.swebench_smoke, 'publish_agent_artifacts', side_effect=publish_while_frozen):
            report = comparison.dispatch(run_plan, contract, prepared, self.root/'paired', runtime.run_slot, budget_usd=10.0)
        self.assertTrue(any(frozen_copies), 'no actual agent freeze was exercised')
        authority = json.loads(budget_state.read_text())
        self.assertIsNone(authority['active'])
        self.assertEqual([lease['identity'] for lease in authority['history']],
                         [slot['namespace'] for slot in run_plan['slots']])
        (self.root/'provider-synthetic.log').write_text(network.provider_log)
        rows = [json.loads(line) for line in (self.root/'paired'/'records.jsonl').read_text().splitlines()]
        self.assertEqual(len(rows), run_plan['expected_records'])
        self.assertTrue(report['comparison_valid'], json.dumps(rows, indent=2))
        self.assertTrue(report['cost_comparison_complete'], json.dumps(report, indent=2))
        self.assertEqual(report['evidence_kind'], 'synthetic-fixture')
        self.assertEqual(report['paired']['both_resolved'], 2)
        self.assertEqual(report['performance_qualifier'], 'rewrite-unproven')
        sessions = []
        for slot in run_plan['slots']:
            directory = self.root/'paired'/slot['namespace']
            official = directory/'official'
            final_snapshot = next((official/'checkpoint_2').rglob('synthetic-lifecycle.jsonl'))
            lifecycle = [json.loads(line) for line in final_snapshot.read_text().splitlines()]
            self.assertEqual([entry['checkpoint'] for entry in lifecycle], [1, 1, 2])
            self.assertEqual([entry['resume'] for entry in lifecycle], [False, True, False])
            self.assertEqual(lifecycle[0]['conversation'], lifecycle[1]['conversation'])
            self.assertNotEqual(lifecycle[1]['conversation'], lifecycle[2]['conversation'])
            self.assertEqual(len({entry['pid'] for entry in lifecycle}), 3)
            self.assertEqual(len({entry['home'] for entry in lifecycle}), 1)
            sessions.append(lifecycle[0]['conversation'])
            arm = report['arms'][slot['name']]
            # Initial failed Codex invocation + official retry + next checkpoint.
            self.assertEqual(arm['native']['primary']['requests'], 3)
            self.assertEqual(arm['native']['primary']['completed_requests'], 3)
            self.assertGreater(arm['estimated_api_usd'], 0)
            self.assertTrue(all(row['quality'] for row in rows if row['arm'] == slot['name']))
            gateway_log = (directory/'trusted-network'/'gateway-events.log').read_text()
            boundaries = [json.loads(line.split(' ', 1)[1])['session_id'] for line in gateway_log.splitlines()
                          if line.startswith('BENCHMARK_CONVERSATION ')]
            self.assertEqual(len(set(boundaries)), 2 if slot['name'] == 'compact' else 0)
        self.assertNotEqual(*sessions)
        leftovers = subprocess.check_output(['docker', 'ps', '-aq', '--filter', 'name=carry-scb-'], text=True)
        self.assertEqual(leftovers.strip(), '')
        networks = subprocess.check_output(['docker', 'network', 'ls', '--format', '{{.Name}}'], text=True).splitlines()
        self.assertFalse(any(name.startswith('carry-agent-') for name in networks))

    def test_actual_native_trace_helpers_use_owned_input_before_read_or_copy(self):
        runtime.activate_upstream(self.prepared)
        from slop_code.agent_runner.agents.codex.agent import CodexAgent
        old_usage, old_save = CodexAgent._read_latest_trace_usage, CodexAgent._save_codex_traces
        with tempfile.TemporaryDirectory(dir=self.root) as directory:
            root = Path(directory)
            raw = root/'regular'; raw.mkdir()
            payload = b'{"owned-fixture":true}\n'
            (raw/'trace.jsonl').write_bytes(payload)
            agent = object.__new__(CodexAgent)
            agent._trace_dir, agent._saved_trace_paths, agent.log = raw, set(), Mock()
            agent.pricing = None
            expected = old_usage(agent)
            destination = root/'native-copies'; destination.mkdir()
            with runtime.protected_runtimes({}, {}):
                self.assertEqual(agent._read_latest_trace_usage(), expected)
                agent._save_codex_traces(destination)
            copies = [p.read_bytes() for p in destination.rglob('*') if p.is_file()]
            self.assertEqual(copies, [payload])
            self.assertEqual(agent._trace_dir, raw)
            sentinel = root/'host-only'; sentinel.write_text('HARMLESS_HOST_ONLY_SENTINEL')
            for operation in ('usage', 'save'):
                with self.subTest(operation=operation):
                    bad = root/operation; bad.mkdir(); (bad/'bad.jsonl').symlink_to(sentinel)
                    agent._trace_dir, agent._saved_trace_paths = bad, set()
                    target = root/(operation+'-copies'); target.mkdir()
                    with runtime.protected_runtimes({}, {}):
                        with self.assertRaises(runtime.swebench_smoke.AgentArtifactError):
                            if operation == 'usage':
                                agent._read_latest_trace_usage()
                            else:
                                agent._save_codex_traces(target)
                    self.assertEqual(list(target.iterdir()), [])
                    self.assertEqual(agent._trace_dir, bad)
            self.assertIs(CodexAgent._read_latest_trace_usage, old_usage)
            self.assertIs(CodexAgent._save_codex_traces, old_save)

    def test_preflight_missing_or_changed_prepared_inputs_fails_without_model_network(self):
        config = copy.deepcopy(self.contract)
        prepared = copy.deepcopy(self.prepared)
        prepared['agent_image'] = 'mutable:latest'
        with self.assertRaisesRegex(ValueError, 'immutable'):
            runtime.preflight(config, prepared, verify_sources=False)
        config['settings']['codex_sha256'] = '0'*64
        with self.assertRaisesRegex(ValueError, 'native Codex binary'):
            runtime.preflight(config, self.prepared, verify_sources=False)
        prepared = copy.deepcopy(self.prepared)
        prepared['tasks']['synthetic_evolution']['definition_sha256'] = '0'*64
        with self.assertRaisesRegex(ValueError, 'problem definition'):
            runtime.preflight(self.contract, prepared, verify_sources=False)
        with patch.object(runtime, 'verify_checkout', side_effect=ValueError('wrong source')):
            with self.assertRaisesRegex(ValueError, 'wrong source'):
                runtime.preflight(self.contract, self.prepared)

    def test_readiness_cannot_authorize_a_different_official_policy(self):
        runtime.prepare_readiness(self.contract, self.prepared)
        changed = copy.deepcopy(self.contract)
        changed['settings']['scb_pass_policy'] = 'all-cases'
        with self.assertRaisesRegex(ValueError, 'readiness') as rejected:
            runtime.preflight(changed, self.prepared, verify_sources=False)
        (self.root/'readiness-policy-rejection.json').write_text(json.dumps({
            'evidence_kind': 'synthetic-fixture', 'performance_qualifier': 'rewrite-unproven',
            'source_commit': self.contract['source_commit'], 'prepared_policy': self.contract['settings']['scb_pass_policy'],
            'requested_policy': changed['settings']['scb_pass_policy'], 'error': str(rejected.exception)}, indent=2))

    def test_late_owned_reviewer_completion_reconciles_to_first_checkpoint(self):
        self.check_late_reviewer(1, 'late-reviewer')

    def test_final_capture_reconciles_owned_reviewer_completion_to_first_checkpoint(self):
        self.check_late_reviewer(2, 'late-reviewer-final')

    def check_late_reviewer(self, release_after, directory):
        contract, prepared = copy.deepcopy(self.contract), self.prepared
        contract['proxy']['CARRY_PROXY_CLASSIFIER_MODEL'] = 'gpt-6.1-sol'
        slot = next(s for s in comparison.build_plan(contract)['slots'] if s['name'] == 'compact')
        original_capture = runtime.proxy_benchmark.capture_evidence
        captures, child = [], None
        destination = self.root/directory
        def capture(network):
            nonlocal child
            if not captures:
                script = """fetch('http://127.0.0.1:8080/v1/responses', {
                  method:'POST', headers:{authorization:'Bearer '+process.env.BENCHMARK_SHADOW_TOKEN,
                    'content-type':'application/json','x-fixture-held-reviewer':'1'},
                  body:JSON.stringify({model:'gpt-6.1-sol',input:'owned delayed reviewer'})})
                  .then(async r=>{if(!r.ok)process.exit(1); console.log(await r.text())}).catch(()=>process.exit(1));"""
                child = subprocess.Popen(['docker', 'exec', network['proxy'], 'node', '-e', script],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                deadline = time.monotonic()+10
                while True:
                    summary = original_capture(network)
                    events = [json.loads(l) for l in (Path(network['state_dir'])/'benchmark-events.jsonl').read_text().splitlines()]
                    shadow = [e for e in events if e.get('actor') == 'shadow']
                    if shadow:
                        self.assertEqual([e['event'] for e in shadow], ['started'])
                        break
                    self.assertLess(time.monotonic(), deadline, 'owned shadow start not observed')
                    time.sleep(.05)
            elif len(captures) >= release_after:
                release = """fetch('https://api.openai.com/fixture/release-reviewer', {
                  headers:{authorization:'Bearer '+process.env.BENCHMARK_CLASSIFIER_KEY}})
                  .then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1));"""
                subprocess.run(['docker', 'exec', network['proxy'], 'node', '-e', release], check=True, timeout=10)
                stdout, stderr = child.communicate(timeout=10)
                self.assertEqual(child.returncode, 0, stderr)
                self.assertEqual(json.loads(stdout)['usage']['input_tokens'], 30)
                summary = original_capture(network)
            else:
                summary = original_capture(network)
            captures.append({'summary': summary, 'events': [json.loads(line) for line in
                (Path(network['state_dir'])/'benchmark-events.jsonl').read_text().splitlines()]})
            (destination/'capture-snapshots.json').write_text(json.dumps({
                'evidence_kind': 'synthetic-fixture', 'performance_qualifier': 'rewrite-unproven',
                'source_commit': contract['source_commit'], 'release_after': release_after,
                'captures': captures}, indent=2))
            return summary
        try:
            with fixture.FixtureNetwork(self.provider), patch.object(runtime.proxy_benchmark, 'capture_evidence', side_effect=capture):
                rows = runtime.run_slot(contract, slot, prepared, destination)
        finally:
            if child is not None and child.poll() is None:
                child.kill(); child.communicate(timeout=10)
        flattened = [e for row in rows for e in row['events']]
        summary = runtime.proxy_benchmark.summarize_events(flattened)
        self.assertTrue(summary['ledger_complete'], json.dumps(rows, indent=2))
        self.assertIsNotNone(summary['estimated_total_cost_usd'])
        self.assertEqual(summary['shadow']['completed_requests'], 1)
        self.assertGreater(summary['shadow']['observed_cost_usd'], 0)
        self.assertEqual(summary['primary']['completed_requests'], 3)
        self.assertEqual([e['event'] for e in rows[0]['events'] if e.get('actor') == 'shadow'], ['started', 'completed'])
        self.assertFalse([e for e in rows[1]['events'] if e.get('actor') == 'shadow'])


@unittest.skipUnless(os.environ.get('CARRY_SCB_HOSTED_FIXTURE') == '1', 'pinned official SCB source required')
class OfficialSnapshotBoundaryTests(unittest.TestCase):
    def test_actual_snapshot_never_reopens_an_agent_parent_after_discovery(self):
        from slop_code.execution import snapshot as native
        original = native.Snapshot.__dict__['from_directory']
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            raw = root/'agent'; (raw/'sub').mkdir(parents=True)
            (raw/'sub/payload').write_bytes(b'agent solution')
            outside = root/'host'; outside.mkdir()
            sentinel = b'HARMLESS_HOST_ONLY_SENTINEL'
            (outside/'payload').write_bytes(sentinel)
            walk = native._walk_candidates
            def replace_parent_after_discovery(cwd, *args, **kwargs):
                result = walk(cwd, *args, **kwargs)
                (raw/'sub').rename(raw/'previous')
                (raw/'sub').symlink_to(outside, target_is_directory=True)
                return result
            with runtime.protected_runtimes({}, {}), \
                 patch.object(native, '_walk_candidates', side_effect=replace_parent_after_discovery):
                captured = native.Snapshot.from_directory(raw, env={})
                contents = captured.extract_contents()
                self.assertNotIn(sentinel, contents.values())
                self.assertEqual(contents[Path('sub/payload')], b'agent solution')
                self.assertEqual(captured.path, raw)
                self.assertFalse(captured.archive.is_relative_to(raw))
            self.assertIs(native.Snapshot.__dict__['from_directory'], original)

    def test_snapshot_failure_resumes_every_successfully_frozen_runtime(self):
        from slop_code.execution import runtime as registry, snapshot as native
        from slop_code.execution.docker_runtime.streaming import DockerStreamingRuntime
        for failure in ('freeze', 'copy'):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory, \
                 patch.object(DockerStreamingRuntime, '__init__', return_value=None), \
                 patch.object(DockerStreamingRuntime, 'cleanup'), \
                 patch.object(runtime.swebench_smoke, 'force_remove_container'), \
                 patch.object(runtime.swebench_smoke, 'publish_agent_artifacts') as publish, \
                 patch.object(native, '_walk_candidates') as walk, runtime.protected_runtimes({}, {}):
                agents = [registry.STREAMING_RUNTIME_REGISTRY['docker']() for _ in range(2)]
                for agent in agents:
                    agent._container = Mock()
                if failure == 'freeze':
                    agents[1]._container.pause.side_effect = RuntimeError('owned freeze refused')
                else:
                    publish.side_effect = runtime.swebench_smoke.AgentArtifactError('owned copy refused')
                with self.assertRaisesRegex((RuntimeError, runtime.swebench_smoke.AgentArtifactError), 'owned .* refused'):
                    native.Snapshot.from_directory(Path(directory), env={})
                agents[0]._container.unpause.assert_called_once()
                if failure == 'freeze':
                    agents[1]._container.unpause.assert_not_called()
                    publish.assert_not_called()
                else:
                    agents[1]._container.unpause.assert_called_once()
                walk.assert_not_called()

    def test_actual_snapshot_rejects_existing_links_before_native_archive(self):
        from slop_code.execution import snapshot as native
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            raw = root/'agent'; raw.mkdir()
            outside = root/'host'; outside.write_text('HARMLESS_HOST_ONLY_SENTINEL')
            (raw/'payload').symlink_to(outside)
            with runtime.protected_runtimes({}, {}), patch.object(native, '_walk_candidates') as walk:
                with self.assertRaises(runtime.swebench_smoke.AgentArtifactError):
                    native.Snapshot.from_directory(raw, env={})
                walk.assert_not_called()


if __name__ == '__main__':
    unittest.main()
