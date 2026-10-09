"""Opt-in, installed Codex 0.147.0 + actual benchmark entrypoint, loopback only.

No downloads, installs, Carry binary, real credentials, or provider inference.
CARRY_HOSTED_TOOL_FIXTURE_CODEX selects the already installed pinned executable;
CARRY_HOSTED_TOOL_FIXTURE_OUTPUT optionally retains synthetic wire evidence.
"""
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
from unittest import mock
import sys
import tempfile
import unittest

from scripts.proxy_native_fixture import Fixture
from scripts.proxy_gateway_fixture_support import Gateway

ROOT = Path(__file__).resolve().parents[1]
CODEX = os.environ.get('CARRY_HOSTED_TOOL_FIXTURE_CODEX')


def stage_node_runtime(bin_dir):
    # setup-node places its pinned runtime outside os.defpath on hosted runners.
    # Preserve only that executable, not the ambient PATH or credentials.
    node = shutil.which('node')
    if node is None:
        raise RuntimeError('installed Codex fixture requires an available Node runtime')
    (bin_dir / 'node').symlink_to(Path(node).resolve())


class NodeRuntimePathTests(unittest.TestCase):
    def test_isolated_path_preserves_selected_node_outside_default_system_path(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            runtime = root / 'selected-node'; runtime.mkdir()
            binary = runtime / 'node'
            binary.write_text('#!/bin/sh\nprintf "SELECTED_NODE_RUNTIME_OK"\n'); binary.chmod(0o755)
            bin_dir = root / 'bin'; bin_dir.mkdir()
            client = root / 'npm-wrapper'
            client.write_text('#!/usr/bin/env node\nnot-real-javascript\n'); client.chmod(0o755)
            with mock.patch.dict(os.environ, {'PATH': str(runtime)}, clear=True):
                stage_node_runtime(bin_dir)
            result = subprocess.run([str(client)], env={'PATH': str(bin_dir) + ':' + os.defpath},
                capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 0)
            self.assertEqual(result.stdout, 'SELECTED_NODE_RUNTIME_OK')


@unittest.skipUnless(CODEX, 'opt in with an installed pinned Codex executable')
class InstalledCodexHostedToolTests(unittest.TestCase):
    def test_fresh_resume_and_custom_command_omit_hosted_tools_on_actual_wire(self):
        assert CODEX is not None
        # Deliberately do not inherit credentials, native auth, config, or proxies.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(os.environ.get('CARRY_HOSTED_TOOL_FIXTURE_OUTPUT', temporary))
            if root != Path(temporary):
                root.mkdir(mode=0o700)
            home = root / 'home'; home.mkdir(mode=0o700)
            bin_dir = root / 'bin'; bin_dir.mkdir()
            scratch = root / 'tmp'; scratch.mkdir(mode=0o700)
            (bin_dir / 'codex').symlink_to(Path(CODEX).resolve())
            stage_node_runtime(bin_dir)
            repo = root / 'workspace'; repo.mkdir()
            # Reuse the immutable base object, without creating any Git commits.
            subprocess.run(['git', 'init', '-q', str(repo)], check=True)
            object_dir = subprocess.check_output(['git', 'rev-parse', '--path-format=absolute',
                '--git-path', 'objects'], cwd=ROOT, text=True).strip()
            (repo / '.git/objects/info/alternates').write_text(object_dir + '\n')
            baseline = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
            subprocess.run(['git', 'update-ref', 'HEAD', baseline], cwd=repo, check=True)
            subprocess.run(['git', 'read-tree', 'HEAD'], cwd=repo, check=True)
            tracked = subprocess.check_output(['git', 'ls-files', '-z'], cwd=repo)
            subprocess.run(['git', 'update-index', '--skip-worktree', '-z', '--stdin'],
                input=tracked, cwd=repo, check=True)
            env = {'PATH': str(bin_dir) + ':' + os.defpath,
                   'HOME': str(home), 'TMPDIR': str(scratch),
                   'OPENAI_API_KEY': 'credential-free-fixture-key',
                   'PREPARED_HARNESS_ROOT': str(root), 'BENCHMARK_WORKSPACE': str(repo),
                   'AGENT_TIMEOUT_SECONDS': '45', 'PYTHONDONTWRITEBYTECODE': '1'}
            version = subprocess.check_output([CODEX, '--version'], env=env, text=True).strip()
            self.assertEqual(version, 'codex-cli 0.147.0')
            prompt = root / 'prompt.txt'
            prompt.write_text('Scripted offline fixture: execute the supplied local tool and finish.')
            results = []
            for custom in (False, True):
                session = root / ('custom-session' if custom else 'native-session')
                session.mkdir(mode=0o700)
                command_env = dict(env)
                if custom:
                    # Attempt an operator-template override; the benchmark policy wins.
                    command_env['AGENT_COMMAND'] = (shlex.quote(str(bin_dir / 'codex')) +
                        ' exec --strict-config --dangerously-bypass-approvals-and-sandbox '
                        '--model {model} --config web_search="live" --json {prompt_text}')
                with Fixture('codex') as fixture:
                    # Fresh native: direct loopback capture (also yields useful RED).
                    # Resume/custom: actual trusted gateway + loopback provider.
                    gateway = Gateway(fixture.server.server_port, fixture.server.server_port)
                    try:
                        thread = None
                        for phase in ('fresh', 'resume'):
                            label = ('custom-' if custom else 'native-') + phase
                            if phase == 'fresh':
                                (repo / 'proxy-fixture.txt').unlink(missing_ok=True)
                            before = len(fixture.calls)
                            through_gateway = custom or phase == 'resume'
                            command_env['OPENAI_BASE_URL'] = (gateway.url if through_gateway else fixture.url) + '/v1'
                            command_env['OPENAI_API_KEY'] = ('fixture-client' if through_gateway
                                                             else 'credential-free-fixture-key')
                            output = root / label
                            command = [sys.executable, str(ROOT / 'containers/swebench-harness/entrypoint.py'),
                                'run', '--harness', 'codex', '--model', 'gpt-6-luna',
                                '--reasoning', 'medium', '--prompt', str(prompt), '--output', str(output),
                                '--codex-session', str(session)]
                            if thread:
                                command += ['--codex-thread', thread]
                            run = subprocess.run(command, cwd=repo, env=command_env,
                                text=True, capture_output=True, timeout=60, stdin=subprocess.DEVNULL)
                            (root / (label + '-entrypoint.log')).write_text(run.stdout + run.stderr)
                            trace = (output / 'trace.log').read_text()
                            events = [json.loads(line) for line in trace.splitlines() if line.startswith('{')]
                            started = [event for event in events if event.get('type') == 'thread.started']
                            if started:
                                thread = started[0]['thread_id']
                            calls = fixture.calls[before:]
                            (root / (label + '-requests.json')).write_text(json.dumps(calls, indent=2) + '\n')
                            tools = [tool for call in calls for tool in call.get('tools', [])]
                            observed_types = set()
                            while tools:
                                tool = tools.pop()
                                observed_types.add(tool['type'])
                                if tool['type'] == 'namespace':
                                    tools.extend(tool['tools'])
                            tool_types = sorted(observed_types)
                            result = {'case': label, 'exit_code': run.returncode,
                                      'requests': len(calls), 'tool_types': tool_types,
                                      'through_gateway': through_gateway,
                                      'auth_removed': not (session / 'auth.json').exists(),
                                      'local_tool_effect': (repo / 'proxy-fixture.txt').is_file(),
                                      'native_final': 'FIXTURE_COMPLETE' in trace,
                                      'provider_errors': list(fixture.errors)}
                            results.append(result)
                            (root / 'results.json').write_text(json.dumps({'version': version,
                                'fixture_only': True, 'results': results}, indent=2) + '\n')
                            with self.subTest(case=label):
                                self.assertEqual(run.returncode, 0, trace[-3000:])
                                self.assertTrue(calls, 'actual model-bearing request required')
                                self.assertTrue(set(tool_types) <= {'function', 'custom', 'namespace'}, tool_types)
                                self.assertTrue(result['auth_removed'])
                                self.assertTrue(result['native_final'])
                                self.assertEqual(fixture.errors, [])
                                self.assertEqual((repo / 'proxy-fixture.txt').read_text(), 'FIXTURE_TOOL_OK')
                            if not thread:
                                self.fail('native fresh session did not yield a resumable thread')
                    finally:
                        gateway.close()
                        (root / (('custom' if custom else 'native') + '-gateway.log')).write_text(
                            gateway.stdout + gateway.stderr)
            print(json.dumps({'version': version, 'fixture_only': True, 'results': results}, sort_keys=True))


if __name__ == '__main__':
    unittest.main(verbosity=2)
