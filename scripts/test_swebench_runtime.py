"""Hosted paired SWE acceptance: real isolated worker/native launch + official 4.1.0 grader.

Only the selected task/TestSpec, native executable and TLS provider are owned
synthetic fixtures. No real dataset/model calls or economic inference.
"""
import contextlib
import copy
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import unittest
from unittest.mock import patch

from scripts import swebench_comparison as lane, slopcodebench_fixture as fixture
from scripts import swebench_evaluator_compat as compat


@unittest.skipUnless(os.environ.get('CARRY_SCB_HOSTED_FIXTURE') == '1', 'disposable hosted Docker fixture only')
class NativeOfficialSWEAcceptance(unittest.TestCase):
    def test_paired_command_native_containment_and_official_grading(self):
        import docker
        from swebench.harness import run_evaluation as official, docker_build, reporting
        from swebench.harness.test_spec.test_spec import TestSpec
        from swebench.harness.constants import MAP_REPO_VERSION_TO_SPECS
        root = Path(os.environ['SCB_FIXTURE_OUTPUT'])
        scb_contract = json.loads((root/'contract.json').read_text())
        scb_prepared = json.loads((root/'prepared.json').read_text())
        work = root/'swe-native'; work.mkdir()
        context = work/'context'; context.mkdir()
        repo = context/'repo'; repo.mkdir()
        (repo/'main.py').write_text('print(0)\n')
        for args in (['init'], ['config', 'user.email', 'synthetic@example.invalid'],
                     ['config', 'user.name', 'synthetic'], ['add', '.'], ['commit', '-m', 'synthetic base']):
            subprocess.run(['git', '-C', str(repo), *args], check=True, capture_output=True)
        base = subprocess.check_output(['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        adapter = Path(__file__).resolve().parents[1]/'containers/swebench-harness/entrypoint.py'
        bundle = work/'bundle/bin'; bundle.mkdir(parents=True)
        shutil.copyfile(adapter, bundle/'adapter')
        (bundle/'adapter').chmod(0o755)
        # Invoke the actual native Codex adapter, but supply a clearly synthetic
        # executable that asserts the production sandbox and calls only our TLS fixture.
        (bundle/'codex').write_text('''#!/usr/bin/env python3
import http.client,json,os,socket,sys
from pathlib import Path
auth=Path(os.environ['HOME'])/'.codex/auth.json'
if sys.argv[1]=='login':
    key=sys.stdin.read(); assert not key.startswith('synthetic-provider-')
    auth.parent.mkdir(parents=True,exist_ok=True); auth.write_text(json.dumps({'OPENAI_API_KEY':key}))
    raise SystemExit(0)
assert sys.argv[1]=='exec'
assert not Path('/var/run/docker.sock').exists()
assert not Path('/proxy-state').exists()
assert not Path('test_owned.py').exists()
assert 'OPENAI_API_KEY' not in os.environ
assert not any(k in os.environ for k in ('CARRY_PROXY_UPSTREAM_KEY','AWS_SECRET_ACCESS_KEY'))
try:
    socket.create_connection(('1.1.1.1',443),timeout=0.2).close()
except OSError: pass
else: raise AssertionError('external egress')
c=http.client.HTTPConnection('openai-proxy',8080,timeout=15)
c.request('POST','/v1/responses',json.dumps({'model':'gpt-6-luna','input':[{'role':'user','content':'synthetic SWE task'}],'stream':False}),{'Authorization':'Bearer '+json.loads(auth.read_text())['OPENAI_API_KEY'],'Content-Type':'application/json'})
r=c.getresponse(); data=r.read(); assert r.status==200,(r.status,data)
Path('main.py').write_text('print(1)\\n')
print(json.dumps({'type':'turn.completed','usage':{'input_tokens':30,'cached_input_tokens':0,'output_tokens':3}}))
''')
        (bundle/'codex').chmod(0o755)
        # Reuse the preparation-only local parent tag whose inspected bytes are
        # pinned by the shared SCB fixture. No image builds in measured slots.
        parent = 'swe-fixture-parent:'+base[:20]
        subprocess.run(['docker', 'tag', scb_prepared['agent_image'], parent], check=True)
        (context/'Dockerfile').write_text(f'''FROM {parent}
RUN apt-get update && apt-get install -y --no-install-recommends git patch && rm -rf /var/lib/apt/lists/*
RUN python -m pip install --no-cache-dir pytest==8.4.2
COPY repo /testbed
ENV PATH=/opt/swebench-harness/bin:$PATH
WORKDIR /testbed
ENTRYPOINT ["python3", "/opt/swebench-harness/bin/adapter"]
''')
        spec = TestSpec(instance_id='owned__synthetic-1', repo='pytest-dev/pytest',
            version=next(iter(MAP_REPO_VERSION_TO_SPECS['pytest-dev/pytest'])),
            repo_script_list=[], env_script_list=[], arch='x86_64', FAIL_TO_PASS=['test_owned.py::test_owned'], PASS_TO_PASS=[],
            language='py', docker_specs={}, namespace='owned-synthetic', eval_script_list=[
                'cd /testbed', "echo '>>>>> Start Test Output'",
                "python - <<'PY'\nfrom pathlib import Path\nPath('test_owned.py').write_text(\"import subprocess\\ndef test_owned():\\n    assert subprocess.check_output(['python', 'main.py'], text=True).strip() == '1'\\n\")\nPY",
                'python -m pytest -rA test_owned.py',
                "echo '>>>>> End Test Output'"])
        subprocess.run(['docker', 'build', '--tag', spec.instance_image_key, str(context)], check=True)
        image = subprocess.check_output(['docker', 'image', 'inspect', '--format', '{{.Id}}', spec.instance_image_key], text=True).strip()
        dataset = work/'dataset.json'
        dataset.write_text(json.dumps([{'instance_id': spec.instance_id, 'repo': spec.repo,
            'version': spec.version, 'base_commit': base, 'problem_statement': 'synthetic: make main.py print 1',
            'test_patch': 'withheld owned test', 'patch': 'withheld owned gold'}]))
        contract = copy.deepcopy(scb_contract)
        contract.update(benchmark='swebench', runner_commit=contract['source_commit'],
                        dataset_commit=lane.swe.DATASET_REVISION,
                        selection=[{'id': spec.instance_id, 'checkpoints': ['final']}])
        contract['settings'].pop('scb_pass_policy')
        contract['settings']['retries'] = 0
        config = work/'contract.json'; config.write_text(json.dumps(contract))
        prepared = {'source_root': scb_prepared['source_root'], 'canonical_dataset': str(dataset),
                    'config': {}, 'harness_bundle': str(bundle.parent), 'gateway_image': scb_prepared['gateway_image'],
                    'carry_image': scb_prepared['carry_image'], 'evaluation_timeout_seconds': 60,
                    'tasks': {spec.instance_id: {'source_task_image': spec.instance_image_key,
                        'agent_image': {'image_id': image}, 'evaluator_image': {'image_id': image}}}}
        prepared_file = work/'prepared.json'; prepared_file.write_text(json.dumps(prepared))
        original_materialize = lane.swe.materialize
        def materialize(**kwargs):
            def clone(_repo, _base, destination):
                shutil.copytree(repo, destination)
            return original_materialize(**kwargs, clone=clone)
        official_reports = []
        def evaluate(**kwargs):
            # Keep the official grading implementation unmodified. This owned
            # TestSpec is the only task override; runtime image boundary is the
            # same prepared-only/no-egress spawn used by the production CLI.
            prediction = json.loads(kwargs['predictions'].read_text())
            compat.install_prepared_runtime({spec.instance_image_key: image})
            with patch.object(official, 'build_container', docker_build.build_container), \
                 patch.object(official, 'RUN_EVALUATION_LOG_DIR', kwargs['output']/'logs/run_evaluation'):
                official.run_instance(spec, prediction, False, False, docker.from_env(), kwargs['run_id'], timeout=30)
            report = next((kwargs['output']/'logs/run_evaluation').rglob('report.json'))
            official_reports.append(json.loads(report.read_text()))
            with patch.object(reporting, 'RUN_EVALUATION_LOG_DIR', kwargs['output']/'logs/run_evaluation'), \
                 contextlib.chdir(kwargs['output']):
                reporting.make_run_report({spec.instance_id: prediction}, json.loads(dataset.read_text()), kwargs['run_id'])
        # Preparation source/dataset verification intentionally cannot accept
        # this owned task as the public dataset. Only this fixture injects it.
        # Reuse the same authority already consumed by SCB, never a new lane allowance.
        budget_state = root/'synthetic-combined-budget.json'
        self.assertTrue(budget_state.is_file())
        previous_leases = json.loads(budget_state.read_text())['history']
        with fixture.FixtureNetwork(scb_prepared['fixture_provider_image']) as network, \
             patch.dict(os.environ, {'BENCHMARK_BUDGET_STATE': str(budget_state)}), \
             patch.object(lane.swe, 'start_agent_network', wraps=lane.swe.start_agent_network) as networks, \
             patch.object(lane, 'preflight', side_effect=lambda c,p: lane.comparison.build_plan(c)), \
             patch.object(lane.swe, 'materialize', side_effect=materialize), \
             patch.object(lane.swe, 'run_official_evaluation', side_effect=evaluate), contextlib.redirect_stdout(io.StringIO()):
            lane.main(['--contract', str(config), '--prepared', str(prepared_file), '--run', '--allow-paid',
                       '--output', str(work/'paired'), '--budget-usd', '10'])
        authority = json.loads(budget_state.read_text())
        self.assertIsNone(authority['active'])
        self.assertEqual(authority['history'][:len(previous_leases)], previous_leases)
        self.assertEqual(len(networks.call_args_list), len(lane.comparison.build_plan(contract)['slots']))
        self.assertEqual([lease['identity'] for lease in authority['history'][len(previous_leases):]],
                         [call.kwargs['identity'] for call in networks.call_args_list])
        (work/'provider-synthetic.log').write_text(network.provider_log)
        report = json.loads((work/'paired/report.json').read_text())
        self.assertTrue(report['comparison_valid'], (work/'paired/records.jsonl').read_text())
        self.assertTrue(report['cost_comparison_complete'], report)
        self.assertEqual(report['evidence_kind'], 'synthetic-fixture')
        self.assertEqual(report['paired']['both_resolved'], 1)
        self.assertEqual(len(official_reports), 2)
        for arm in ('direct', 'compact'):
            self.assertEqual(report['arms'][arm]['native']['primary']['completed_requests'], 1)
        self.assertTrue(all(r[spec.instance_id]['resolved'] for r in official_reports))
        self.assertEqual(subprocess.check_output(['docker', 'ps', '-aq', '--filter', 'name=sweb.eval.owned__synthetic'], text=True).strip(), '')
        (work/'acceptance.json').write_text(json.dumps({'evidence_kind': 'synthetic-fixture',
            'official_evaluator': 'swebench==4.1.0', 'official_reports': official_reports,
            'preflight_override': 'owned synthetic dataset only; paid public dataset still pinned',
            'source_commit': contract['source_commit']}, indent=2)+'\n')
