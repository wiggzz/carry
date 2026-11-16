"""Offline dependency seams; native Docker/SQL qualification remains hosted-only."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

from scripts import slopcodebench_prepare as preparation
from scripts import slopcodebench_runtime as runtime
from scripts import test_slopcodebench_cohort as cohort_fixture


class RuntimeDependencyPreparationTests(unittest.TestCase):
    setUp = cohort_fixture.CohortPreparationTests.setUp
    definition = cohort_fixture.CohortPreparationTests.definition
    execute = cohort_fixture.CohortPreparationTests.execute

    def test_pinned_application_requirements_are_warmed_separately_from_pytest(self):
        manifests = {}
        for task in self.selection['problems']:
            for cp in task['checkpoints']:
                path = self.problems/task['id']/'solutions'/cp/'requirements.txt'
                path.parent.mkdir(parents=True)
                data = ('# not implementation code\nowned-app==1\n' if task['id'] == 'first'
                        else 'other-app==2; python_version >= "3.12"\n')
                path.write_text(data)
                manifests[path.relative_to(self.problems).as_posix()] = hashlib.sha256(data.encode()).hexdigest()
                (path.parent/'hidden.py').write_text('HIDDEN_IMPLEMENTATION_SENTINEL')
        tools = self.root/'bin'; tools.mkdir()
        uv = tools/'uv'
        uv.write_text('#!'+sys.executable+'\nimport json,os,sys\nwith open(os.environ["WARM_LOG"],"a") as f: f.write(json.dumps(["uv",*sys.argv[1:]])+"\\n")\n')
        uv.chmod(0o755)
        with patch.dict(os.environ, {'GITHUB_ACTIONS': 'true'}, clear=True), \
                patch.object(preparation, 'official_definition', side_effect=self.definition), \
                patch.object(runtime.evaluator, 'install', return_value=runtime.evaluator.identity()), \
                patch.object(runtime, 'verify_checkout'):
            contract, prepared = preparation.prepare(self.root/'prepared', self.upstream, self.problems,
                self.carry, selection_path=self.selection_path, repetitions=3, execute=self.execute)
        application_commands = [row for row in self.warm_commands if row[0] == 'uv']
        self.assertEqual(application_commands, [
            ['uv', 'run', '--isolated', '--no-project', '--with=owned-app==1', 'python', '-c', 'pass'],
            ['uv', 'run', '--isolated', '--no-project', '--with=other-app==2; python_version >= "3.12"',
             'python', '-c', 'pass']])
        provenance = {'policy': 'native-offline-requirements-v1', 'requirements_sha256': manifests}
        self.assertEqual(contract['dependency_preparation'], provenance)
        self.assertEqual(prepared['dependency_preparation'], provenance)
        for path in (self.root/'prepared/image-context').iterdir():
            self.assertNotIn(b'HIDDEN_IMPLEMENTATION_SENTINEL', path.read_bytes())
        self.assertEqual(len([r for r in self.warm_commands if r[0] != 'uv']), 2)


@unittest.skipUnless(os.environ.get('SCB_PINNED_RUNTIME_ROOT'), 'explicit exact pinned runtime source required')
class NativeEvaluatorSetupTests(unittest.TestCase):
    def test_native_setup_runs_after_cache_copy_and_output_is_split(self):
        # Execute complete checksum-bound native modules. Only Docker/import
        # boundaries are synthetic; native command/entry-script owners are real.
        root = Path(os.environ['SCB_PINNED_RUNTIME_ROOT'])
        modules = {}
        def module(name, **values):
            value = types.ModuleType(name); value.__dict__.update(values)
            modules[name] = value
            return value
        logger = types.SimpleNamespace(debug=lambda *a, **k: None)
        module('slop_code'); execution = module('slop_code.execution')
        registry = module('slop_code.execution.runtime', STREAMING_RUNTIME_REGISTRY={}, EXEC_RUNTIME_REGISTRY={},
            _ensure_runtimes_registered=lambda: None, RuntimeResult=lambda **kw: types.SimpleNamespace(**kw),
            SolutionRuntimeError=RuntimeError)
        execution.runtime = registry
        module('slop_code.logging', get_logger=lambda *a: logger)
        module('docker', from_env=lambda: None)
        module('slop_code.execution.assets', ResolvedStaticAsset=object)
        module('slop_code.execution.docker_runtime.models', DockerEnvironmentSpec=object)
        module('slop_code.execution.models', EnvironmentSpec=object)
        module('slop_code.execution.protocols', ExecRuntime=object)
        module('slop_code.execution.docker_runtime.streaming', DockerStreamingRuntime=object)
        module('slop_code.agent_runner.agents.utils', HOME_PATH=Path('/native-home'))
        class Codex:
            def _read_latest_trace_usage(self): pass
            def _save_codex_traces(self): pass
        module('slop_code.agent_runner.agents.codex.agent', CodexAgent=Codex)
        class Snapshot:
            @classmethod
            def from_directory(cls, *a, **kw): pass
        snapshot = module('slop_code.execution.snapshot', Snapshot=Snapshot, _walk_candidates=lambda *a: None)
        execution.snapshot = snapshot
        def load(name, path, expected):
            self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), expected)
            spec = importlib.util.spec_from_file_location(name, path)
            value = importlib.util.module_from_spec(spec)
            sys.modules[name] = value
            spec.loader.exec_module(value)
            return value
        with tempfile.TemporaryDirectory(prefix='native setup ') as directory, patch.dict(sys.modules, modules):
            load('slop_code.execution.shared', root/'src/slop_code/execution/shared.py',
                          '4f8a915a8557bb0f5e210dced32923c78192c152f59e2f05774e5a5a43faa1b4')
            load('slop_code.execution.docker_runtime.exec', root/'src/slop_code/execution/docker_runtime/exec.py',
                 'ffc6ef0a1aa72f6a2d2acfa9cb014bace3bf7d8cae598c0cc1be61c892105c41')
            cwd = Path(directory)
            cache = cwd/'prepared-cache'; cache.mkdir(); (cache/'ready').write_text('ready')
            mutable_cache = cwd/'mutable-cache'
            setup = ['test -f '+shlex.quote(str(mutable_cache/'ready'))+' && printf setup-output > setup-marker',
                     'printf native-setup-output; printf native-setup-error >&2']
            spec = types.SimpleNamespace(type='docker',
                docker=types.SimpleNamespace(binary='docker', image='sha256:'+'1'*64, extra_mounts={},
                    mount_workspace=True, workdir='/workspace'),
                environment=types.SimpleNamespace(include_os_env=False),
                get_setup_commands=lambda is_evaluation: setup if is_evaluation else [],
                get_full_env=lambda env: dict(env), get_container_user=lambda: None,
                effective_network_mode=lambda: 'bridge')
            calls, removals = [], []
            def execute(args, **kw):
                calls.append(args)
                self.assertIn('--network', args)
                self.assertEqual(args[args.index('--network')+1], 'none')
                self.assertIn('--read-only', args)
                self.assertIn('UV_OFFLINE=1', args)
                command = args[-1].replace('/opt/scb-uv-cache', shlex.quote(str(cache)))
                command = command.replace('/tmp/uv-cache', shlex.quote(str(mutable_cache)))
                # Login-profile side effects are outside this container seam.
                command = command.replace('bash -l ', 'bash --noprofile --norc ')
                return subprocess.run(['/bin/sh', '-c', command], cwd=cwd,
                    env={'PATH': os.defpath, 'HOME': str(cwd)}, text=True, capture_output=True, timeout=5)
            with runtime.protected_runtimes({}, {'evaluator_image': spec.docker.image}, execute=execute), \
                    patch.object(runtime.swebench_smoke, 'force_remove_container', side_effect=lambda *a, **k: removals.append(a)):
                runner = registry.EXEC_RUNTIME_REGISTRY['docker'].spawn(spec, working_dir=cwd,
                    command='test -f setup-marker && printf command-output', is_evaluation=True,
                    static_assets={}, env_vars={})
                result = runner.execute()
            self.assertEqual(result.exit_code, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), 'command-output')
            self.assertEqual(result.stderr.strip(), '')
            self.assertEqual(result.setup_stdout.strip(), 'native-setup-output')
            self.assertEqual(result.setup_stderr.strip(), 'native-setup-error')
            self.assertEqual((cwd/'setup-marker').read_text(), 'setup-output')
            self.assertEqual(len(calls), 1)
            self.assertEqual(len(removals), 1)
            (cwd/'setup-marker').rename(cwd/'original-setup-marker')
            with runtime.protected_runtimes({}, {'evaluator_image': spec.docker.image}, execute=execute), \
                    patch.object(runtime.swebench_smoke, 'force_remove_container', side_effect=lambda *a, **k: removals.append(a)):
                utility = registry.EXEC_RUNTIME_REGISTRY['docker'].spawn(spec, working_dir=cwd,
                    command='printf utility-output', is_evaluation=False, static_assets={}, env_vars={})
                utility_result = utility.execute()
                self.assertEqual(utility_result.exit_code, 0)
                self.assertEqual(utility_result.stdout, 'utility-output')
                self.assertEqual(utility_result.setup_stdout, '')
                self.assertFalse((cwd/'setup-marker').exists())
                with self.assertRaisesRegex(ValueError, 'credential-bearing'):
                    utility.execute({'OPENAI_API_KEY': 'synthetic-forbidden'})
                self.assertEqual(len(calls), 2)
                self.assertEqual(len(removals), 2)
            self.assertEqual(registry.EXEC_RUNTIME_REGISTRY, {})
