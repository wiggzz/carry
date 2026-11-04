#!/usr/bin/env python3
"""Small protected runtime adapter for the pinned official SCB runner.

No catalog bootstrap, image builds/pulls, provider credentials in agent containers,
custom scorer, or replacement checkpoint loop. Invoke through slopcodebench.py.
"""
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import queue
import re
import shlex
import subprocess
import tempfile
import time
import uuid
import sys

try:
    from scripts import proxy_benchmark, swebench_smoke
    from scripts.benchmark_comparison import fingerprint
except ModuleNotFoundError:
    import proxy_benchmark
    import swebench_smoke
    from benchmark_comparison import fingerprint


def tree_digest(root):
    """Hash definitions/tests/gold/assets, not merely the selected spec text."""
    rows = []
    for path in sorted(root.rglob('*')):
        if '.git' in path.relative_to(root).parts:
            continue
        if path.is_symlink():
            raise ValueError('prepared problem trees cannot contain symlinks')
        if path.is_file():
            rows.append([path.relative_to(root).as_posix(), hashlib.sha256(path.read_bytes()).hexdigest()])
    return fingerprint(rows)


def image_pin(value):
    if not re.fullmatch(r'(?:[a-zA-Z0-9][a-zA-Z0-9._/:+-]*@)?sha256:[0-9a-f]{64}', value):
        raise ValueError('prepared images must be immutable image IDs or OCI digests')
    return value.rsplit('@', 1)[-1]


def verify_checkout(root, expected):
    actual = subprocess.check_output(['git', '-C', str(root), 'rev-parse', 'HEAD'], text=True).strip()
    dirty = subprocess.check_output(['git', '-C', str(root), 'status', '--porcelain', '--untracked-files=no'], text=True)
    if actual != expected or dirty:
        raise ValueError(f'checkout is not the clean pinned revision: {root}')


def preflight(contract, prepared, *, execute=subprocess.run, verify_sources=True, require_readiness=True):
    """Read-only readiness; never prepares an image on a measured run."""
    try:
        from scripts.slopcodebench import plan
    except ModuleNotFoundError:
        from slopcodebench import plan
    result = plan(contract)
    settings = contract['settings']
    if settings['concurrency'] != 1 or not 0 <= settings['retries'] <= 3:
        raise ValueError('protected SCB is serial with at most three native retries')
    if settings['native_config']:
        raise ValueError('initial SCB native_config must be empty (no ignored native settings)')
    if settings['auth_mode'] != 'api-key':
        raise ValueError('protected SCB supports scoped API-key transport only')
    for name, key in [('agent_image', 'harness_digest'), ('evaluator_image', 'environment_digest'),
                      ('gateway_image', 'gateway_digest'), ('carry_image', 'carry_digest')]:
        if image_pin(prepared[name]) != settings[key]:
            raise ValueError(f'{name} differs from the paired contract')
        execute(['docker', 'image', 'inspect', prepared[name]], check=True, capture_output=True, text=True, timeout=30)
    if verify_sources:
        if Path(prepared['source_root']).resolve() != Path(__file__).resolve().parents[1]:
            raise ValueError('runtime must execute from its pinned source checkout')
        for key, commit in [('source_root', 'source_commit'), ('upstream_root', 'runner_commit'),
                            ('problems_root', 'dataset_commit')]:
            verify_checkout(Path(prepared[key]), contract[commit])
    for key in ('prompt', 'environment'):
        if not Path(prepared[key]).is_file():
            raise ValueError(f'missing prepared {key}')
    if hashlib.sha256(Path(prepared['prompt']).read_bytes()).hexdigest() != settings['prompt_sha256']:
        raise ValueError('prompt differs from paired contract')
    version = execute(['docker', 'run', '--rm', '--pull=never', '--network=none', '--read-only',
                       '--cap-drop=ALL', '--security-opt=no-new-privileges', '--entrypoint=/bin/sh',
                       prepared['agent_image'], '-c',
                       'codex --version && sha256sum "$(command -v codex)"'],
                      check=True, capture_output=True, text=True, timeout=30).stdout.splitlines()
    if (len(version) != 2 or version[0] != 'codex-cli ' + settings['codex_version']
            or version[1].split()[0] != settings['codex_sha256']):
        raise ValueError('prepared native Codex binary/version differs from contract')
    for task in contract['selection']:
        record = prepared['tasks'][task['id']]
        definition = Path(prepared['problems_root']) / task['id']
        if tree_digest(definition) != record['definition_sha256']:
            raise ValueError('problem definition changed since preparation')
        if not require_readiness:
            continue
        readiness = json.loads(Path(record['readiness']).read_text())
        binding = {'runner_commit': contract['runner_commit'], 'dataset_commit': contract['dataset_commit'],
                   'definition_sha256': record['definition_sha256'], 'evaluator_image': prepared['evaluator_image'],
                   'environment_sha256': hashlib.sha256(Path(prepared['environment']).read_bytes()).hexdigest(),
                   'task_id': task['id'], 'checkpoints': task['checkpoints']}
        if readiness.get('binding') != binding or readiness.get('status') != 'ready':
            raise ValueError('missing or mismatched official gold readiness receipt')
    return result


@contextmanager
def scoped_env(values):
    before = dict(os.environ)
    try:
        for key in list(os.environ):
            if key.startswith(('CARRY_PROXY_', 'BENCHMARK_')):
                os.environ.pop(key)
        os.environ.update(values)
        yield
    finally:
        os.environ.clear()
        os.environ.update(before)


def new_conversation(network, *, execute=subprocess.run):
    if 'carry_proxy' not in network:
        return
    script = """fetch('http://127.0.0.1:8080/benchmark/new-conversation', {
      method:'POST', headers:{authorization:'Bearer '+process.env.BENCHMARK_CONTROL_TOKEN},
      signal:AbortSignal.timeout(3000)}).then(async r=>{if(r.status!==200)process.exit(1); console.log(await r.text())})
      .catch(()=>process.exit(1));"""
    result = execute(['docker', 'exec', network['proxy'], 'node', '-e', script],
                     check=True, capture_output=True, text=True, timeout=10)
    network['active_session_id'] = json.loads(result.stdout)['session_id']


@contextmanager
def protected_runtimes(network, prepared, *, execute=subprocess.run, on_invocation=None):
    """Use SCB's runtime registries and volume builder, not its unrestricted launcher.

    The official streaming runtime, workspace, HOME and .codex mount persist
    through checkpoints/retry within one slot. Native exec is fresh per spec.
    The evaluator has no network or credentials and uses the official command.
    """
    from slop_code.execution import runtime as registry
    from slop_code.execution.docker_runtime.exec import DockerExecRuntime
    from slop_code.agent_runner.agents.utils import HOME_PATH
    registry._ensure_runtimes_registered()
    old_stream, old_exec = registry.STREAMING_RUNTIME_REGISTRY.copy(), registry.EXEC_RUNTIME_REGISTRY.copy()
    with tempfile.TemporaryDirectory(prefix='scb-native-home-') as home:
        class ProtectedExec:
            @classmethod
            def spawn(cls, environment, **kwargs):
                return cls(environment, **kwargs)

            def __init__(self, environment, working_dir, command=None, **kwargs):
                self.environment, self.working_dir, self.command = environment, working_dir, command
                self.kwargs = kwargs

            def execute(self, env=None, stdin=None, timeout=None):
                raise RuntimeError('use the specialized agent/evaluator runtime')

            def invoke(self, command, env, timeout, stdin=None):
                from slop_code.execution.runtime import RuntimeResult
                if (self.environment.type != 'docker' or self.environment.docker.extra_mounts
                        or self.environment.environment.include_os_env
                        or self.environment.docker.binary != 'docker'):
                    raise ValueError('protected SCB requires Docker without environment extra_mounts')
                if self.kwargs.get('ports'):
                    raise ValueError('protected SCB does not expose agent or evaluator ports')
                image = prepared['evaluator_image']
                if self.kwargs.get('image', image) != image:
                    raise ValueError('official runner attempted to replace the prepared image')
                kwargs = {k: v for k, v in self.kwargs.items()
                          if k in {'mounts', 'static_assets', 'env_vars', 'is_evaluation'}}
                mounts = dict(kwargs.get('mounts') or {})
                kwargs.update(mounts=mounts, disable_setup=True, image=image, ports={}, setup_command=None)
                builder = DockerExecRuntime(spec=self.environment, working_dir=self.working_dir,
                                            command=command, **kwargs)
                builder._network_mode = 'none'
                clean = dict(env or {})
                clean['UV_OFFLINE'] = '1'
                clean['UV_PYTHON_DOWNLOADS'] = 'never'
                clean['UV_CACHE_DIR'] = '/tmp/uv-cache'
                clean.update(HOME='/tmp/scb-home', XDG_DATA_HOME='/tmp/scb-data',
                             UV_TOOL_DIR='/tmp/scb-tools', UV_TOOL_BIN_DIR='/tmp/scb-bin')
                limit = prepared.get('evaluation_timeout_seconds', 300)
                if type(limit) is not int or not 1 <= limit <= 3600:
                    raise ValueError('evaluation_timeout_seconds must be 1..3600')
                timeout = min(timeout or limit, limit)
                command = 'cp -R /opt/scb-uv-cache /tmp/uv-cache && ' + command
                merged = self.environment.get_full_env({**(kwargs.get('env_vars') or {}), **clean})
                for key, value in merged.items():
                    if any(word in key.upper() for word in ('KEY', 'TOKEN', 'SECRET', 'PASSWORD', 'CREDENTIAL')):
                        raise ValueError('credential-bearing environment is forbidden in evaluator runtime')
                builder._command = command
                args = builder._build_docker_run_command(clean)
                name = 'carry-scb-' + uuid.uuid4().hex
                flags = ['--name', name, '--pull=never', '--read-only', '--cap-drop=ALL',
                         '--security-opt=no-new-privileges', '--pids-limit=512', '--memory=4g',
                         '--cpus=2', '--tmpfs=/tmp:rw,nosuid,nodev,size=512m']
                args[2:2] = flags
                started = time.monotonic()
                timed_out = False
                try:
                    result = execute(args, check=False, capture_output=True, text=True,
                                     input=stdin, timeout=timeout)
                    code, stdout, stderr = result.returncode, result.stdout, result.stderr
                except subprocess.TimeoutExpired as error:
                    timed_out, code = True, 124
                    def text(value):
                        return value.decode(errors='replace') if isinstance(value, bytes) else value or ''
                    stdout, stderr = text(error.stdout), text(error.stderr)
                finally:
                    swebench_smoke.force_remove_container(name, exact_name=True)
                return RuntimeResult(exit_code=code, stdout=stdout or '', stderr=stderr or '',
                                     setup_stdout='', setup_stderr='', elapsed=time.monotonic()-started,
                                     timed_out=timed_out)

            def cleanup(self):
                pass  # Every synchronous invocation proves its container absent in finally.

        from slop_code.execution.docker_runtime.streaming import DockerStreamingRuntime
        agents = []
        class AgentRuntime(DockerStreamingRuntime):
            def __init__(self, *args, **kwargs):
                super().__init__(*args, **kwargs)
                self.protected_name = 'carry-scb-' + uuid.uuid4().hex
                agents.append(self)

            def validate(self, env):
                if (self.spec.environment.include_os_env or self.spec.docker.extra_mounts
                        or self.spec.docker.binary != 'docker' or self._ports
                        or self._image != prepared['agent_image']):
                    raise ValueError('unprepared or unrestricted agent runtime configuration')
                merged = self.spec.get_full_env(self._merge_env(env))
                for key, value in merged.items():
                    if any(word in key.upper() for word in ('KEY', 'TOKEN', 'SECRET', 'PASSWORD', 'CREDENTIAL')):
                        if key != 'OPENAI_API_KEY' or value != network['client_token']:
                            raise ValueError('provider credentials cannot enter the agent runtime')
                return merged

            def _create_base_container(self):
                env = self.validate({})
                self._mounts[home] = {'bind': str(HOME_PATH), 'mode': 'rw'}
                # SDK create never pulls an image on miss. No mutable upstream
                # get_base_image/build path or unrestricted upstream create is used.
                container = self.client.containers.create(self._image, command='sleep infinity',
                    name=self.protected_name, tty=False, detach=True, stdin_open=False,
                    environment=env, volumes=self._build_volumes(), user=self.user,
                    network_mode=network['internal'], working_dir=self._container_workdir(),
                    extra_hosts={'openai-proxy': network['proxy_ip']}, dns=['127.0.0.1'],
                    read_only=True, cap_drop=['ALL'], security_opt=['no-new-privileges'],
                    pids_limit=512, mem_limit='4g', nano_cpus=2000000000,
                    tmpfs={'/tmp': 'rw,nosuid,nodev,size=512m'})
                self._container = container
                container.start()
                return container

            def stream(self, command, env, timeout):
                self.validate(env)
                if env.get('OPENAI_API_KEY') != network['client_token']:
                    raise ValueError('native Codex must use its scoped client token')
                tokens = shlex.split(command)
                if tokens[:2] != ['codex', 'exec']:
                    raise ValueError('only the official native Codex exec contract is allowed')
                resume = tokens[2:4] == ['resume', '--last']
                if not resume:
                    new_conversation(network, execute=execute)
                if on_invocation:
                    on_invocation(resume)
                for event in super().stream(command, env, timeout):
                    if event.kind == 'finished' and event.result.timed_out:
                        # Killing docker exec alone does not kill its children.
                        self.kill()
                        swebench_smoke.force_remove_container(self.protected_name, exact_name=True)
                    yield event

            def cleanup(self):
                try:
                    super().cleanup()
                finally:
                    swebench_smoke.force_remove_container(self.protected_name, exact_name=True)

        class EvalRuntime(ProtectedExec):
            def execute(self, env=None, stdin=None, timeout=None):
                return self.invoke(self.command, env, timeout, stdin)

        registry.STREAMING_RUNTIME_REGISTRY['docker'] = AgentRuntime
        registry.EXEC_RUNTIME_REGISTRY['docker'] = EvalRuntime
        try:
            yield
        finally:
            cleanup_errors = []
            for agent_runtime in agents:
                try:
                    agent_runtime.cleanup()
                except Exception as error:
                    cleanup_errors.append(str(error))
            registry.STREAMING_RUNTIME_REGISTRY.clear(); registry.STREAMING_RUNTIME_REGISTRY.update(old_stream)
            registry.EXEC_RUNTIME_REGISTRY.clear(); registry.EXEC_RUNTIME_REGISTRY.update(old_exec)
            if cleanup_errors:
                raise RuntimeError('protected agent cleanup failed: ' + '; '.join(cleanup_errors))


def official_components(prepared, contract, task_id):
    """Import installed pinned SCB directly; never bootstrap its mutable catalog."""
    from slop_code.evaluation import ProblemConfig, PassPolicy
    from slop_code.execution import DockerEnvironmentSpec
    import yaml
    problem = ProblemConfig.from_yaml(Path(prepared['problems_root']) / task_id)
    wanted = next(item['checkpoints'] for item in contract['selection'] if item['id'] == task_id)
    actual = [name for name, _ in problem.iterate_checkpoint_items()]
    if wanted != actual:
        raise ValueError('protected SCB runs the full ordered official problem, not independent checkpoints')
    environment = DockerEnvironmentSpec.model_validate(yaml.safe_load(Path(prepared['environment']).read_text()))
    if environment.docker.extra_mounts:
        raise ValueError('protected SCB environment cannot mount arbitrary host files')
    return problem, environment, PassPolicy(contract['settings']['scb_pass_policy'])


def activate_upstream(prepared):
    source = (Path(prepared['upstream_root'])/'src').resolve()
    sys.path.insert(0, str(source))
    import slop_code
    if not Path(slop_code.__file__).resolve().is_relative_to(source):
        raise ValueError('installed SCB does not come from the pinned upstream checkout')


def prepare_readiness(contract, prepared):
    """Generate receipts by actually evaluating prepared reference solutions offline."""
    from slop_code.evaluation import run_checkpoint
    for task in contract['selection']:
        problem, environment, policy = official_components(prepared, contract, task['id'])
        record = prepared['tasks'][task['id']]
        binding = {'runner_commit': contract['runner_commit'], 'dataset_commit': contract['dataset_commit'],
                   'definition_sha256': record['definition_sha256'], 'evaluator_image': prepared['evaluator_image'],
                   'environment_sha256': hashlib.sha256(Path(prepared['environment']).read_bytes()).hexdigest(),
                   'task_id': task['id'], 'checkpoints': task['checkpoints']}
        destination = Path(record['readiness'])
        destination.parent.mkdir(parents=True, exist_ok=True)
        reports = []
        # No network/agent factory is used by the official pytest evaluator.
        with protected_runtimes({}, prepared):
            for checkpoint in problem.iterate_checkpoints():
                gold = Path(prepared['problems_root'])/task['id']/'solutions'/checkpoint.name
                if not gold.is_dir():
                    raise ValueError(f'missing prepared gold snapshot for {task["id"]}/{checkpoint.name}')
                report = run_checkpoint(submission_path=gold, problem=problem, checkpoint=checkpoint, env_spec=environment)
                report.save(destination.parent/'gold-reports'/task['id']/checkpoint.name)
                if report.infrastructure_failure or not report.pytest_collected or not report.tests or not policy.check(report.pass_counts, report.total_counts):
                    raise ValueError('official gold readiness failed; no measured execution is allowed')
                reports.append({'checkpoint': checkpoint.name, 'tests': len(report.tests), 'passed': True})
        receipt = {'schema_version': 1, 'binding': binding, 'status': 'ready', 'reports': reports,
                   'evaluation_kind': 'official-offline-gold-no-model'}
        destination.write_text(json.dumps(receipt, indent=2, sort_keys=True)+'\n')
    return {'status': 'ready', 'tasks': len(contract['selection']), 'model_requests': 0}


def run_slot(contract, slot, prepared, output):
    """Run the official AgentRunner/CodexAgent, retaining every failed checkpoint."""
    from slop_code.agent_runner.agents.codex.agent import CodexAgent, CodexModelProvider
    from slop_code.agent_runner.credentials import ProviderCredential, CredentialType
    from slop_code.agent_runner.models import AgentCostLimits, AgentRunSpec
    from slop_code.agent_runner import runner as official
    from slop_code.evaluation import CorrectnessResults
    problem, environment, policy = official_components(prepared, contract, slot['task_id'])
    output.mkdir(parents=True, exist_ok=False)
    values = {**contract.get('proxy', {}), 'CARRY_PROXY_MODE': slot['proxy_mode'],
              'BENCHMARK_TRUSTED_DIRECT': '1' if slot['proxy_mode'] == 'disabled' else '0',
              'BENCHMARK_HARNESS': 'codex'}
    times, rows, seen = {}, [], set()
    started = time.monotonic()
    state = output / 'trusted-network'
    error = None
    network = None
    original_eval = official.evaluate_agent_snapshot
    active = None
    first = True
    with scoped_env(values):
        try:
            network = swebench_smoke.start_agent_network(identity=slot['namespace'],
                proxy_image=prepared['gateway_image'], proxy_script=Path(prepared['source_root'])/'scripts/openai_proxy.js',
                carry_image=prepared['carry_image'], state_dir=state)
            def invocation(resume):
                nonlocal active, first
                if not resume:
                    active = slot['checkpoints'][len(times)]
                    times[active] = {'started': time.monotonic(), 'setup_seconds': time.monotonic()-started if first else 0.0}
                    first = False
            def evaluate(checkpoint, save_dir, snapshot_dir, problem, environment):
                mark = time.monotonic()
                item = times.setdefault(checkpoint.name, {'started': started, 'setup_seconds': None})
                item['agent_seconds'] = mark-item['started']
                try:
                    return original_eval(checkpoint, save_dir, snapshot_dir, problem, environment)
                finally:
                    item['evaluation_seconds'] = time.monotonic()-mark
                    item['task_wall_seconds'] = time.monotonic()-item['started'] + (item['setup_seconds'] or 0)
                    summary = proxy_benchmark.capture_evidence(network)
                    item['dispatched_rewrites'] = summary.get('dispatched_rewrites')
                    item['native_compactions'] = summary.get('native_compactions')
                    events = [json.loads(line) for line in (state/'benchmark-events.jsonl').read_text().splitlines()]
                    item['events'] = [e for e in events if e.get('request_id') not in seen]
                    seen.update(e.get('request_id') for e in events)
            credential = ProviderCredential(provider='carry_benchmark', credential_type=CredentialType.ENV_VAR,
                                            value=network['client_token'], source='scoped', destination_key='OPENAI_API_KEY')
            settings = contract['settings']
            agent = CodexAgent(problem_name=problem.name, verbose=False, image=prepared['agent_image'],
                cost_limits=AgentCostLimits(cost_limit=0, net_cost_limit=0, max_retries=settings['retries']),
                pricing=None, credential=credential, binary='codex',
                model=settings['model'], timeout=settings['timeout_seconds'], thinking=settings['reasoning'],
                max_thinking_tokens=None, extra_args=['--config', shlex.quote('web_search="disabled"')], env={},
                model_provider=CodexModelProvider(provider_id='carry_benchmark', base_url=network['api_base'], env_key='OPENAI_API_KEY'))
            spec = AgentRunSpec(seed=0, template=Path(prepared['prompt']).read_text(), problem=problem,
                environment=environment, image=prepared['agent_image'], pass_policy=policy,
                skip_evaluation=False, concurrent_evaluation=False, agent_type='codex',
                agent_version=settings['codex_version'], model_name=settings['model'])
            official.evaluate_agent_snapshot = evaluate
            with protected_runtimes(network, prepared, on_invocation=invocation):
                official_output = output/'official'
                official_output.mkdir()
                official.AgentRunner(spec, agent, official_output, queue.Queue()).run()
        except Exception as caught:
            error = f'{type(caught).__name__}: {caught}'
        finally:
            official.evaluate_agent_snapshot = original_eval
            if network is not None:
                try:
                    summary = proxy_benchmark.capture_evidence(network)
                    events = [json.loads(line) for line in (state/'benchmark-events.jsonl').read_text().splitlines()]
                    remaining = [e for e in events if e.get('request_id') not in seen]
                    if active is not None:
                        item = times[active]
                        item.setdefault('events', []).extend(remaining)
                        if not summary['ledger_complete']:
                            item['events'].append({'ledger_capture_incomplete': True})
                        item.setdefault('agent_seconds', time.monotonic()-item['started'])
                        item.setdefault('task_wall_seconds', time.monotonic()-item['started']+(item['setup_seconds'] or 0))
                except Exception as caught:
                    error = f'evidence failure: {caught}; previous: {error}'
                finally:
                    try:
                        swebench_smoke.cleanup_agent_network(network)
                    except Exception as caught:
                        error = f'cleanup failure: {caught}; previous: {error}'
    for checkpoint in slot['checkpoints']:
        item = times.get(checkpoint, {})
        row = {'namespace': slot['namespace'], 'contract_sha256': fingerprint(contract), 'checkpoint': checkpoint,
               'task_id': slot['task_id'], 'arm': slot['name'], 'status': 'not-run', 'resolved': None,
               'quality': {}, 'events': item.get('events', []), 'error': error,
               **{k: item.get(k) for k in ('agent_seconds', 'setup_seconds', 'evaluation_seconds', 'task_wall_seconds')},
               'selected_rewrites': None, 'committed_rewrites': None,
               'dispatched_rewrites': 0 if slot['proxy_mode'] == 'disabled' else item.get('dispatched_rewrites'),
               'native_compactions': item.get('native_compactions')}
        directory = output/'official'/checkpoint
        try:
            report = CorrectnessResults.from_dir(directory)
            if not report.infrastructure_failure and report.pytest_collected > 0 and report.tests:
                row.update(status='evaluated', resolved=policy.check(report.pass_counts, report.total_counts))
            else:
                row['status'] = 'evaluation-error'
        except (OSError, ValueError, KeyError):
            if checkpoint in times:
                row['status'] = 'agent-failed'
        quality = directory/'quality_analysis/overall_quality.json'
        if quality.is_file():
            row['quality'] = json.loads(quality.read_text())
        if error:
            row.update(status='infrastructure-error', official_resolved=row['resolved'], resolved=None)
        rows.append(row)
    (output/'records.jsonl').write_text(''.join(json.dumps(r, sort_keys=True)+'\n' for r in rows))
    return rows
