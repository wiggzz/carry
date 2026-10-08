"""Execute the actual workflow bootstrap writer, not source assertions."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[1]


class WorkflowProxyTests(unittest.TestCase):
    def test_workflow_bootstrap_preserves_opt_in_configuration(self):
        source = (ROOT / '.github/workflows/run-swebench.yml').read_text()
        start = source.index('          import base64, os, shlex, sys\n')
        stop = source.index('          PY\n', start)
        writer = textwrap.dedent(source[start:stop])
        env = dict(os.environ)
        for key in ('SOURCE_URL', 'KEY_URL', 'DOCKER_AUTH_URL', 'REGISTRY_AUTH_URL', 'RESULT_URL', 'CONTROL_URL',
            'SOURCE_SHA256', 'SOURCE_COMMIT', 'BENCHMARK_MODE', 'BENCHMARK_HARNESS',
            'BENCHMARK_ATTEMPT', 'BENCHMARK_ATTEMPTS', 'CARRY_COMPACTION_POLICY', 'CARRY_KEEP_LEASE_TURNS',
            'CARRY_COMPACTION_PAYOFF_REQUESTS', 'CARRY_COMPACTION_MIN_PAYBACK_PERCENT',
            'CARRY_COMPACTION_ROLLOUT_SAMPLES', 'CARRY_COMPACTION_ROLLOUT_STOP_PROBABILITY_PERCENT',
            'CARRY_COMPACTION_NEUTRAL_HIGH_WATERMARK_TOKENS', 'CARRY_COMPACTION_NEUTRAL_LOW_WATERMARK_TOKENS',
            'BOOTSTRAP_WAIT_SECONDS', 'RUN_ID', 'MODEL', 'REASONING', 'TASK_IMAGE_REPOSITORY', 'TASK_IMAGE_CATALOG'):
            env[key] = 'fixture'
        env.update(BENCHMARK_HARNESS='codex', BENCHMARK_MODE='smoke-5', CARRY_PROXY_MODE='compact',
            CARRY_PROXY_CLASSIFIER_MODEL='gpt-6-luna', CARRY_PROXY_CLASSIFIER_EFFORT='low',
            CARRY_PROXY_PAYOFF_REQUESTS='5', CARRY_PROXY_MIN_PAYBACK_PERCENT='3',
            CARRY_PROXY_HISTORY_POLICY='reset-on-divergence')
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'bootstrap.env'
            result = subprocess.run(['python3', '-c', writer, str(output)], cwd=ROOT, env=env,
                                    text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            # Remove the original configuration: only exports made by the real
            # writer and strict-shell consumer may reach the child.
            clean = {key:value for key,value in env.items() if not key.startswith('CARRY_PROXY_')}
            result = subprocess.run(['bash', '-uc', 'set -a; source "$1"; set +a; python3 -c '
                + "'import os,json; print(json.dumps({k:v for k,v in os.environ.items() if k.startswith(\"CARRY_PROXY_\")}))'",
                'fixture', str(output)], env=clean, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            values = json.loads(result.stdout)
            self.assertEqual(values.get('CARRY_PROXY_MODE'), 'compact')
            self.assertEqual(values.get('CARRY_PROXY_CLASSIFIER_EFFORT'), 'low')
            self.assertEqual(values.get('CARRY_PROXY_PAYOFF_REQUESTS'), '5')
            self.assertEqual(values.get('CARRY_PROXY_MIN_PAYBACK_PERCENT'), '3')
            self.assertEqual(values.get('CARRY_PROXY_HISTORY_POLICY'), 'reset-on-divergence')


    def test_parsed_mode_choices_reach_actual_configuration_validator(self):
        import yaml
        from scripts.proxy_benchmark import validate_config
        workflow=yaml.safe_load((ROOT/'.github/workflows/run-swebench.yml').read_text())
        inputs=workflow.get('on',workflow.get(True))['workflow_dispatch']['inputs']
        choices=inputs['proxy_mode']['options']
        self.assertEqual(choices,['disabled','off','audit','compact'])
        for mode in choices:
            resolved=validate_config({'BENCHMARK_HARNESS':'pi','CARRY_PROXY_MODE':mode})
            self.assertEqual(resolved['CARRY_PROXY_MODE'],mode)

    def test_parsed_workflow_policy_choice_reaches_real_validator(self):
        import yaml
        from scripts.proxy_benchmark import validate_config
        workflow=yaml.safe_load((ROOT/'.github/workflows/run-swebench.yml').read_text())
        inputs=workflow.get('on',workflow.get(True))['workflow_dispatch']['inputs']
        setting=inputs.get('proxy_history_policy',{})
        self.assertEqual(setting.get('default'),'strict')
        self.assertEqual(setting.get('options'),['strict','reset-on-divergence'])
        binding=workflow['jobs']['bootstrap-worker']['env'].get('CARRY_PROXY_HISTORY_POLICY')
        self.assertEqual(binding,'${{ inputs.proxy_history_policy }}')
        for policy in setting['options']:
            resolved=validate_config({'BENCHMARK_HARNESS':'pi','CARRY_PROXY_MODE':'compact',
                'CARRY_PROXY_HISTORY_POLICY':policy})
            self.assertEqual(resolved['CARRY_PROXY_HISTORY_POLICY'],policy)

    def test_ci_native_gateway_command_uses_built_binary_and_pinned_pi_prefix(self):
        import yaml
        workflow=yaml.safe_load((ROOT/'.github/workflows/ci.yml').read_text())
        step=next(s for s in workflow['jobs']['test']['steps'] if s.get('name')==
                  'Pinned Pi checkpoint policy through production gateway and integrated Carry')
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); binary=root/'bin'; binary.mkdir()
            fake=binary/'python3'
            fake.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$RECORDED_ARGV"\n')
            fake.chmod(0o755)
            record=root/'argv'
            result=subprocess.run(['bash','-eu','-c',step['run']],cwd=ROOT,
                env={**os.environ,'PATH':str(binary)+':'+os.environ['PATH'],
                     'RUNNER_TEMP':str(root),'RECORDED_ARGV':str(record)},
                text=True,capture_output=True,timeout=5)
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual(record.read_text().splitlines(),['scripts/proxy_gateway_native_fixture.py',
                '--carry',str(ROOT/'target/x86_64-unknown-linux-musl/release/carry'),
                '--pi',str(root/'proxy-clients/node_modules/.bin/pi'),
                '--output',str(root/'proxy-gateway-native-fixture')])

    def test_actual_result_gate_rejects_missing_and_mismatched_effective_modes(self):
        import tarfile
        from scripts.proxy_benchmark import provenance
        source=(ROOT/'.github/workflows/run-swebench.yml').read_text()
        section=source.split('      - name: Validate fixed-denominator results\n',1)[1]
        script=textwrap.dedent(section.split('        run: |\n',1)[1].split('\n      - name:',1)[0])
        tasks=json.loads((ROOT/'benchmarks/swe-bench-verified-smoke-5.json').read_text())['instance_ids']
        for effective in ('compact','off',None):
            with self.subTest(effective=effective), tempfile.TemporaryDirectory() as directory:
                root=Path(directory); artifact=root/'benchmark-artifact'; artifact.mkdir()
                (root/'scripts').symlink_to(ROOT/'scripts',target_is_directory=True)
                (root/'benchmarks').symlink_to(ROOT/'benchmarks',target_is_directory=True)
                env={k:v for k,v in os.environ.items() if not k.startswith('CARRY_PROXY_')}
                env.update(BENCHMARK_MODE='smoke-5',BENCHMARK_HARNESS='pi',BENCHMARK_ATTEMPT='1',
                           BENCHMARK_ATTEMPTS='1',CARRY_PROXY_MODE='compact')
                records=[{'instance_id':task,'harness':'pi','attempt':1,'status':'evaluated',
                          'proxy_summary':{'effective_mode':effective}} for task in tasks]
                report={'denominator':5,'attempt_numbers':[1],'provenance':{'phase':'complete','proxy':provenance(env)}}
                (artifact/'records.json').write_text(json.dumps(records))
                (artifact/'report.json').write_text(json.dumps(report))
                (artifact/'worker-exit-status').write_text('0\n')
                with tarfile.open(artifact/'results.tar.gz','w:gz') as archive:
                    for name in ('records.json','report.json','worker-exit-status'):
                        archive.add(artifact/name,arcname=name)
                run=subprocess.run(['bash','-c',script],cwd=root,env=env,text=True,capture_output=True,timeout=15)
                self.assertEqual(run.returncode==0,effective=='compact',run.stderr)


if __name__ == '__main__':
    unittest.main()
