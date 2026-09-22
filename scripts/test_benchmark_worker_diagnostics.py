#!/usr/bin/env python3
"""Executable controller fixtures; no AWS credentials or network required."""
import importlib.util
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name("benchmark_worker_diagnostics.py")


def load_module(path):
    spec = importlib.util.spec_from_file_location(path.stem, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class DiagnosticsTests(unittest.TestCase):
    def test_missing_archive_retains_allowlisted_console_without_raw_secrets(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            aws = root / "aws"
            aws.write_text("#!/usr/bin/env python3\n" + """
import json, sys
if 'describe-instances' in sys.argv:
    print(json.dumps(['i-0123456789abcdef0', 'running', 16, None]))
else:
    print('SECRET raw console https://signed.invalid/?token=SECRET')
    print('BENCHMARK_WORKER ' + json.dumps({'phase':'finishing','status':'failed','exit_code':1}))
    print('BENCHMARK_PREPARATION ' + json.dumps({'phase':'building','status_counts':{'built':3},'build_log_bytes':42,'last_modified_age_seconds':2.5}))
    print('BENCHMARK_PROGRESS ' + json.dumps({'instance_id':'django__django-123','harness':'carry','state':'started'}))
    print('BENCHMARK_WORKER ' + json.dumps({'phase':'SECRET'}))
    print('BENCHMARK_WORKER ' + json.dumps({'phase':'finishing','secret':'SECRET'}))
""")
            aws.chmod(0o755)
            output = root / "diagnostics.json"
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "--output", str(output),
                 "--instance-id", "i-0123456789abcdef0", "--archive-exit-code", "22",
                 "--archive-http-status", "404"],
                env={**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"]},
                text=True, capture_output=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            evidence = json.loads(output.read_text())
            self.assertEqual(len(evidence["events"]), 3)
            self.assertEqual(evidence["polls"][-1]["worker_state"], "running")
            self.assertEqual(evidence["polls"][-1]["archive"], {"exit_code": 22, "http_status": 404})
            self.assertNotIn("SECRET", output.read_text() + result.stdout + result.stderr)


    def test_rejects_unknown_strings_nested_payloads_and_oversized_numbers(self):
        diagnostics = load_module(SCRIPT)
        for event in ({'elapsed_seconds': 10**500}, {'load1': float('nan')},
                      {'load1': float('inf')}, {'load1': True},
                      {'status_counts': {'SECRET': 1}}, {'status_counts': {'built': {'SECRET': 1}}},
                      {'phase': 'https://signed.invalid/SECRET'}, {'phase': ['building']},
                      {'schema': 'SECRET'}, {'resources': {'load1': 1}},
                      {'stage': 'SECRET'}, {'exit_code': 'SECRET'}):
            with self.subTest(event=event):
                self.assertIsNone(diagnostics.safe_event('BENCHMARK_PREPARATION', event))
        oversized = 'BENCHMARK_WORKER ' + json.dumps({'phase': 'building'}) + ' ' * 8192
        self.assertEqual(list(diagnostics.console_events(oversized)), [])

    def test_existing_graded_progress_keeps_exact_statuses(self):
        diagnostics = load_module(SCRIPT)
        for status in ('evaluated', 'evaluation-error', 'evaluation-incomplete',
                       'agent-budget-exhausted', 'agent-session-context-missing'):
            event = {'instance_id': 'django__django-123', 'harness': 'carry',
                     'state': 'graded', 'status': status}
            self.assertEqual(diagnostics.safe_event('BENCHMARK_PROGRESS', event),
                             {'kind': 'BENCHMARK_PROGRESS', **event})

    def test_real_preparation_schema_and_worker_stages_survive_sanitizing(self):
        diagnostics = load_module(SCRIPT)
        telemetry = load_module(SCRIPT.with_name('benchmark_preparation_telemetry.py'))
        with tempfile.TemporaryDirectory() as directory:
            snapshot = telemetry.snapshot(pathlib.Path(directory))
        worker = {'stage': 'upload_failed', 'exit_code': 28, 'elapsed_seconds': 120}
        events = list(diagnostics.console_events(
            'BENCHMARK_PREPARATION ' + json.dumps(snapshot) + '\n' +
            'BENCHMARK_WORKER ' + json.dumps(worker)))
        self.assertEqual(events, [{'kind': 'BENCHMARK_PREPARATION', **snapshot},
                                  {'kind': 'BENCHMARK_WORKER', **worker}])
        for stage in ('sanitize_failed', 'log_drain_failed', 'archive', 'upload',
                      'package_setup', 'source_fetch', 'source_ready', 'credentials',
                      'python_setup', 'preparation', 'benchmark'):
            event = {'stage': stage, 'exit_code': 0, 'elapsed_seconds': 1}
            self.assertEqual(diagnostics.safe_event('BENCHMARK_WORKER', event),
                             {'kind': 'BENCHMARK_WORKER', **event})


class WorkflowTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        self.repo = SCRIPT.parents[1]
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.root / "scripts").symlink_to(self.repo / "scripts", target_is_directory=True)
        self.workflow = (self.repo / '.github/workflows/run-swebench.yml').read_text()
        # Extract actual literal run blocks without adding a YAML package dependency.
        self.steps = []
        for block in self.workflow.split('      - name: ')[1:]:
            name = block.splitlines()[0]
            if '\n        run: |\n' in block:
                lines = block.split('\n        run: |\n', 1)[1].splitlines()
                script = []
                for line in lines:
                    if line and not line.startswith('          '):
                        break
                    script.append(line[10:])
                self.steps.append({'name': name, 'run': '\n'.join(script) + '\n'})
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                        RUNNER_TEMP=str(self.root), FIXTURE_ROOT=str(self.root),
                        BENCHMARK_MODE='prepare-long-50',
                        AWS_ACCESS_KEY_ID='dispatch', AWS_SECRET_ACCESS_KEY='private',
                        AWS_SESSION_TOKEN='private', WORKER_INSTANCE_ID='i-0123456789abcdef0',
                        RESULT_GET_URL='https://fixture.invalid/SECRET', PREFIX='runs/test',
                        RUN_ID='test', ARTIFACT_SESSION_ROLE_ARN='artifact', ARTIFACT_BUCKET='fixture',
                        GITHUB_RUN_ID='1', GITHUB_RUN_ATTEMPT='1')
        self.command('aws', '''
import json, os, pathlib, sys
root = pathlib.Path(os.environ['FIXTURE_ROOT'])
a = sys.argv[1:]
with (root / 'calls.jsonl').open('a') as f:
    f.write(json.dumps([a[:2], os.environ['AWS_ACCESS_KEY_ID']]) + '\\n')
if a[:2] == ['sts', 'assume-role']:
    print(json.dumps({'AccessKeyId':'artifact','SecretAccessKey':'private','SessionToken':'private'}))
elif a[:2] == ['s3', 'cp']:
    pass
elif a[:2] == ['ec2', 'describe-instances']:
    if os.environ.get('STATE_ERROR') or os.environ['AWS_ACCESS_KEY_ID'] != 'dispatch':
        print('An error occurred (UnauthorizedOperation) when calling DescribeInstances: SECRET', file=sys.stderr)
        sys.exit(254)
    count = int((root / 'gets').read_text()) if (root / 'gets').exists() else 0
    states = os.environ.get('STATES', 'terminated').split(',')
    state = states[min(max(count - 1, 0), len(states) - 1)]
    print(json.dumps(['i-0123456789abcdef0', state, {'running':16,'terminated':48,'stopped':80,'shutting-down':32}.get(state,0), None]))
elif a[:2] == ['ec2', 'get-console-output']:
    if os.environ.get('CONSOLE_ERROR'):
        print('An error occurred (AccessDenied) when calling GetConsoleOutput: SECRET', file=sys.stderr)
        sys.exit(253)
    print('SECRET raw environment https://fixture.invalid/SECRET')
    print('BENCHMARK_WORKER {"stage":"upload_failed","status":28}')
    print('BENCHMARK_PROGRESS {"instance_id":"task-1","harness":"carry","state":"started"}')
else:
    sys.exit(90)
''')
        self.command('curl', '''
import os, pathlib, sys
root = pathlib.Path(os.environ['FIXTURE_ROOT'])
f = root / 'gets'
n = int(f.read_text()) + 1 if f.exists() else 1
f.write_text(str(n))
ok = n >= int(os.environ.get('SUCCESS_AT', '999'))
a = sys.argv[1:]
pathlib.Path(a[a.index('-o') + 1]).write_text('archive' if ok else 'SECRET error body')
if '--write-out' in a or '-w' in a:
    print('200' if ok else '404', end='')
sys.exit(0 if ok else 22)
''')
        self.command('python3', f'''
import os, sys
if len(sys.argv) > 1 and sys.argv[1] == 'scripts/presign_s3.py':
    print('https://fixture.invalid/rotated')
else:
    os.execv({sys.executable!r}, [{sys.executable!r}, *sys.argv[1:]])
''')

    def command(self, name, body):
        path = self.bin / name
        path.write_text(f'#!{sys.executable}\n' + body)
        path.chmod(0o755)

    def run_step(self, name, **changes):
        script = next(step['run'] for step in self.steps if step.get('name') == name)
        # Advance the real Bash SECONDS clock without network, waits, or changed deadlines.
        preamble = 'sleep() { SECONDS=$((SECONDS + ${SECONDS_STEP:-1000})); };\n'
        return subprocess.run(['bash', '-c', preamble + script], cwd=self.root,
                              env=dict(self.env, **changes), text=True, capture_output=True, timeout=20)

    def evidence(self):
        return json.loads((self.root / 'benchmark-artifact/worker-diagnostics.json').read_text())

    def test_final_diagnostics_survive_denied_console_and_missing_result(self):
        result = self.run_step('Retain final worker diagnostics', CONSOLE_ERROR='1')
        self.assertEqual(result.returncode, 0, result.stderr)
        evidence = self.evidence()
        self.assertEqual(evidence['polls'][-1]['worker_state'], 'terminated')
        self.assertEqual(evidence['polls'][-1]['get_console_output'],
                         {'exit_code': 253, 'error_code': 'AccessDenied'})
        self.assertNotIn('SECRET', json.dumps(evidence) + result.stdout + result.stderr)
        self.assertFalse((self.root / 'benchmark-artifact/results.tar.gz').exists())

    def test_rotation_keeps_artifact_identity_but_ec2_reads_use_dispatch(self):
        result = self.run_step('Wait for protected benchmark result',
                               STATES='running,running,stopped', SECONDS_STEP='1501')
        self.assertEqual(result.returncode, 1, result.stderr)
        evidence = self.evidence()
        self.assertEqual(evidence['outcome'], 'worker-unavailable')
        calls = [json.loads(line) for line in (self.root / 'calls.jsonl').read_text().splitlines()]
        self.assertTrue(any(command == ['s3', 'cp'] and identity == 'artifact' for command, identity in calls))
        self.assertTrue(all(identity == 'dispatch' for command, identity in calls if command[0] == 'ec2'))
        self.assertTrue(all(identity == 'dispatch' for command, identity in calls if command[0] == 'sts'))

    def test_denied_state_is_unknown_not_terminal_and_deadline_stays_bounded(self):
        result = self.run_step('Wait for protected benchmark result',
                               STATE_ERROR='1', CONSOLE_ERROR='1', SECONDS_STEP='21000')
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(int((self.root / 'gets').read_text()), 1)
        evidence = self.evidence()
        self.assertEqual(evidence['outcome'], 'deadline-exceeded')
        self.assertEqual(evidence['polls'][-1]['worker_state'], 'unknown')
        self.assertEqual(evidence['polls'][-1]['describe_instances'],
                         {'exit_code': 254, 'error_code': 'UnauthorizedOperation'})
        self.assertEqual(evidence['unavailable_polls'], 0)
        self.assertNotIn('SECRET', json.dumps(evidence) + result.stdout + result.stderr)

    def test_workflow_preserves_human_readable_progress_once(self):
        result = self.run_step('Wait for protected benchmark result', SUCCESS_AT='3')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.count('[carry] task-1 started\n'), 1)
        events = [event for event in self.evidence()['events']
                  if event['kind'] == 'BENCHMARK_PROGRESS']
        self.assertEqual(len(events), 1)

    def test_archive_arriving_in_race_grace_is_only_success_signal(self):
        result = self.run_step('Wait for protected benchmark result', SUCCESS_AT='3')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(int((self.root / 'gets').read_text()), 3)
        self.assertEqual(self.evidence()['outcome'], 'archive-received')
        self.assertEqual((self.root / 'benchmark-artifact/results.tar.gz').read_text(), 'archive')
        self.assertEqual(self.evidence()['polls'][-1]['archive'], {'exit_code': 0, 'http_status': 200})

    def test_unrecognized_state_remains_unknown_until_original_deadline(self):
        result = self.run_step('Wait for protected benchmark result', STATES='None', SECONDS_STEP='21000')
        self.assertEqual(result.returncode, 1, result.stderr)
        evidence = self.evidence()
        self.assertEqual(evidence['outcome'], 'deadline-exceeded')
        self.assertEqual(evidence['polls'][-1]['worker_state'], 'unknown')
        self.assertEqual(evidence['polls'][-1]['describe_instances']['error_code'], 'InvalidResponse')

    def test_shutting_down_worker_gets_the_same_bounded_race_grace(self):
        result = self.run_step('Wait for protected benchmark result', STATES='shutting-down')
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(int((self.root / 'gets').read_text()), 3)
        self.assertEqual(self.evidence()['outcome'], 'worker-unavailable')

    def test_no_launched_worker_still_produces_safe_final_evidence(self):
        result = self.run_step('Retain final worker diagnostics', WORKER_INSTANCE_ID='')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.evidence()['polls'][-1]['describe_instances']['error_code'], 'WorkerNotLaunched')
        self.assertFalse((self.root / 'calls.jsonl').exists())

    def test_evidence_is_deduplicated_and_bounded_across_snapshots(self):
        directory = self.root / 'benchmark-artifact'
        directory.mkdir()
        output = directory / 'worker-diagnostics.json'
        output.write_text(json.dumps({'schema': 'carry.benchmark-worker-diagnostics.v1',
                                      'events': [{'kind': 'BENCHMARK_WORKER', 'elapsed_seconds': n}
                                                 for n in range(512)],
                                      'polls': [{'worker_state': 'running'} for _ in range(256)]}))
        result = self.run_step('Wait for protected benchmark result')
        self.assertEqual(result.returncode, 1, result.stderr)
        evidence = self.evidence()
        self.assertEqual(len(evidence['polls']), 256)
        self.assertEqual(len(evidence['events']), 512)
        progress = [event for event in evidence['events'] if event['kind'] == 'BENCHMARK_PROGRESS']
        self.assertEqual(len(progress), 1)
        self.assertLess(output.stat().st_size, 200000)

    def test_dead_worker_fails_after_two_race_grace_polls_with_diagnostics(self):
        result = self.run_step('Wait for protected benchmark result')
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(int((self.root / 'gets').read_text()), 3)
        evidence = self.evidence()
        self.assertEqual(evidence['outcome'], 'worker-unavailable')
        self.assertEqual(evidence['polls'][-1]['worker_state'], 'terminated')
        self.assertFalse((self.root / 'benchmark-artifact/results.tar.gz.tmp').exists())
        self.assertNotIn('SECRET', json.dumps(evidence) + result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
