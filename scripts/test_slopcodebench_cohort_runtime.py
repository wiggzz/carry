"""Real run_slot/dispatch with synthetic official/native boundaries, NOT Docker or official gold."""
import contextlib
import json
import os
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

from scripts import benchmark_comparison as comparison
from scripts import slopcodebench as scb
from scripts import slopcodebench_runtime as runtime
from scripts import test_slopcodebench_cohort as cohort_fixture


class CanonicalCohortRuntimeTests(unittest.TestCase):
    def test_whole_problem_chains_share_one_worker_and_repetitions_get_fresh_namespaces(self):
        config = cohort_fixture.CohortOutcomeTests().configuration()
        config['settings']['timeout_seconds'] = 7200
        run_plan = scb.plan(config)
        invocations, networks, cleanups, agent_settings, runners = [], [], [], [], []
        callback = None
        native_events = []
        current_network = None
        class Settings:
            def __init__(self, **kwargs): self.__dict__.update(kwargs)
        class Agent(Settings):
            def __init__(self, **kwargs): super().__init__(**kwargs); agent_settings.append(kwargs)
        class Policy:
            value = 'any'
            def __init__(self, value): self.value = value
            def check(self, passed, totals): return passed == totals
        class Report:
            infrastructure_failure = False
            pytest_collected = 1
            tests = [types.SimpleNamespace(status='passed')]
            pass_counts = total_counts = {'Core': 1}
            @classmethod
            def from_dir(cls, directory):
                if not (directory/'synthetic-evaluated').exists(): raise FileNotFoundError(directory)
                return cls()
        def evaluate(checkpoint, save_dir, snapshot_dir, problem, environment):
            save_dir.mkdir(); (save_dir/'synthetic-evaluated').write_text('synthetic')
            return Report()
        official = types.ModuleType('slop_code.agent_runner.runner')
        official.evaluate_agent_snapshot = evaluate
        class AgentRunner:
            def __init__(self, spec, agent, output, queue):
                self.spec, self.agent, self.output = spec, agent, output
                runners.append((spec.problem.name, id(self), id(agent), output))
            def run(self):
                # The trusted fake official executor emits sequential checkpoints
                # through the actual production callback/evaluation adapter.
                for cp in self.spec.problem.checkpoints:
                    callback(False)
                    invocations.append((current_network['identity'], cp.name, id(self)))
                    rows = cohort_fixture.CohortOutcomeTests().records(run_plan,
                        [next(s for s in run_plan['slots'] if s['namespace'] == current_network['identity'])])
                    native_events.extend(e for row in rows if row['checkpoint'] == cp.name for e in row['events'])
                    state = Path(current_network['state_dir'])/'benchmark-events.jsonl'
                    state.write_text(''.join(json.dumps(e)+'\n' for e in native_events))
                    official.evaluate_agent_snapshot(cp, self.output/cp.name, None, self.spec.problem, self.spec.environment)
        official.AgentRunner = AgentRunner
        modules = {}
        for name, values in {
                'slop_code.agent_runner.agents.codex.agent': {'CodexAgent': Agent, 'CodexModelProvider': Settings},
                'slop_code.agent_runner.credentials': {'ProviderCredential': Settings,
                    'CredentialType': types.SimpleNamespace(ENV_VAR='env')},
                'slop_code.agent_runner.models': {'AgentCostLimits': Settings, 'AgentRunSpec': Settings},
                'slop_code.agent_runner': {'runner': official},
                'slop_code.evaluation': {'CorrectnessResults': Report, 'PassPolicy': Policy}}.items():
            module = types.ModuleType(name); module.__dict__.update(values); modules[name] = module
        def components(prepared, contract, task_id):
            task = next(t for t in contract['selection'] if t['id'] == task_id)
            problem = types.SimpleNamespace(name=task_id, checkpoints=[types.SimpleNamespace(name=cp) for cp in task['checkpoints']])
            return problem, None, Policy('any')
        def start(**kwargs):
            nonlocal current_network
            native_events.clear()
            Path(kwargs['state_dir']).mkdir()
            (Path(kwargs['state_dir'])/'benchmark-events.jsonl').write_text('')
            current_network = {**kwargs, 'client_token': 'synthetic-only', 'api_base': 'http://synthetic.invalid'}
            networks.append(current_network)
            return current_network
        @contextlib.contextmanager
        def protected(network, prepared, *, on_invocation):
            nonlocal callback
            callback = on_invocation
            yield
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {}, clear=True), \
                patch.dict(sys.modules, modules), patch.object(runtime, 'official_components', side_effect=components), \
                patch.object(runtime, 'protected_runtimes', side_effect=protected), \
                patch.object(runtime.swebench_smoke, 'start_agent_network', side_effect=start), \
                patch.object(runtime.swebench_smoke, 'cleanup_agent_network', side_effect=lambda n: cleanups.append(n['identity'])), \
                patch.object(runtime.proxy_benchmark, 'capture_evidence', return_value={
                    'ledger_complete': True, 'dispatched_rewrites': 0, 'native_compactions': 0}):
            root = Path(directory); prompt = root/'prompt'; prompt.write_text('{{ spec }}')
            prepared = {'prompt': str(prompt), 'source_root': str(root),
                        'agent_image': 'synthetic-agent', 'gateway_image': 'synthetic-gateway', 'carry_image': 'synthetic-carry'}
            result = comparison.dispatch(run_plan, config, prepared, root/'run', runtime.run_slot, budget_usd=10)
            self.assertTrue(result['comparison_valid'])
            self.assertTrue(result['cost_comparison_complete'])
            self.assertEqual(len(runners), 12)
            self.assertEqual(len(networks), 12)
            self.assertEqual(cleanups, [s['namespace'] for s in run_plan['slots']])
            self.assertEqual(len(set(cleanups)), 12)
            for slot, network, runner in zip(run_plan['slots'], networks, runners):
                calls = [i for i in invocations if i[0] == slot['namespace']]
                self.assertEqual([i[1] for i in calls], slot['checkpoints'])
                self.assertEqual(len({i[2] for i in calls}), 1, 'checkpoint chain reset its official runner')
                self.assertEqual(network['identity'], slot['namespace'])
                self.assertEqual(network['expected_primary_model'], config['settings']['model'])
            self.assertTrue(all(a['timeout'] == 7200 and a['thinking'] == config['settings']['reasoning'] for a in agent_settings))
            self.assertTrue(all(a['cost_limits'].max_retries == 0 and a['cost_limits'].cost_limit == 0 for a in agent_settings))
            rows = [json.loads(line) for line in (root/'run/records.jsonl').read_text().splitlines()]
            self.assertEqual(len(rows), 18)
            for slot in run_plan['slots']:
                selected = [r for r in rows if r['namespace'] == slot['namespace']]
                self.assertEqual([r['checkpoint'] for r in selected], slot['checkpoints'])
                for row in selected:
                    self.assertEqual(sum(e['event'] == 'completed' and e['actor'] == 'primary' for e in row['events']), 1)
            self.assertIs(official.evaluate_agent_snapshot, evaluate)
