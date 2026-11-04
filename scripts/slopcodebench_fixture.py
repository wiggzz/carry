#!/usr/bin/env python3
"""Hosted-only synthetic SCB fixture preparation (never a benchmark result)."""
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import uuid
from unittest.mock import patch

from scripts import slopcodebench_runtime as runtime
from scripts import swebench_smoke
from scripts.benchmark_comparison import fingerprint

PYTHON_BASE = 'python@sha256:593bd06efe90efa80dc4eee3948be7c0fde4134606dd40d8dd8dbcade98e669c'
NODE_BASE = 'node@sha256:1c18d9ab3af4585870b92e4dbc5cac5a0dc77dd13df1a5905cea89fc720eb05b'


def command(args, **kwargs):
    return subprocess.run(args, check=True, text=True, **kwargs)


def prepare(root, upstream, carry):
    if os.environ.get('GITHUB_ACTIONS') != 'true' or os.environ.get('CARRY_SCB_HOSTED_FIXTURE') != '1':
        raise RuntimeError('SCB Docker fixture preparation is restricted to disposable hosted CI')
    root.mkdir(parents=True, exist_ok=False)
    source = Path(__file__).resolve().parents[1]
    context = root/'image-context'; context.mkdir()
    for name in ('codex.py', 'provider.py'):
        shutil.copyfile(source/'benchmarks/slopcodebench/fixtures'/name, context/name)
    shutil.copyfile(carry, context/'carry')
    command(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
             '-subj', '/CN=api.openai.com', '-addext', 'subjectAltName=DNS:api.openai.com',
             '-keyout', str(context/'fixture-key.pem'), '-out', str(context/'fixture-cert.pem')], capture_output=True)
    dockerfile = context/'Dockerfile'
    dockerfile.write_text(f'''FROM {PYTHON_BASE}
RUN pip install --no-cache-dir uv==0.9.7
ENV UV_CACHE_DIR=/opt/scb-uv-cache UV_PYTHON_DOWNLOADS=never SSL_CERT_FILE=/opt/fixture-cert.pem
RUN uvx --with=pytest --with=pytest-json-ctrf --with=pytest-json-report --with=pytest-timeout --with=jsonschema --with=deepdiff pytest --version && chmod -R a+rX /opt/scb-uv-cache
COPY codex.py /usr/local/bin/codex
COPY carry /opt/swebench-harness/bin/carry
COPY fixture-cert.pem /opt/fixture-cert.pem
RUN chmod 755 /usr/local/bin/codex /opt/swebench-harness/bin/carry
''')
    iid = root/'agent.iid'
    # Separate build contexts keep provider TLS private keys/program out of the agent image.
    public_context = root/'agent-context'; public_context.mkdir()
    for name in ('codex.py', 'carry', 'fixture-cert.pem', 'Dockerfile'):
        shutil.copyfile(context/name, public_context/name)
    command(['docker', 'build', '--iidfile', str(iid), str(public_context)])
    agent = iid.read_text().strip()
    # BuildKit treats a config ID in FROM as a registry repository. A unique
    # preparation-only local tag binds the already-built bytes; paid runtimes
    # still consume only inspected config IDs and never build on a miss.
    parent_tag = 'scb-fixture-parent:'+uuid.uuid4().hex
    command(['docker', 'tag', agent, parent_tag])
    parent_id = subprocess.check_output(['docker', 'image', 'inspect', '--format', '{{.Id}}', parent_tag], text=True).strip()
    if parent_id != agent:
        raise ValueError('fixture provider parent image identity mismatch')
    dockerfile.write_text(f'''FROM {parent_tag}
COPY provider.py /opt/provider.py
COPY fixture-key.pem /opt/fixture-key.pem
''')
    try:
        command(['docker', 'build', '--pull=false', '--iidfile', str(root/'provider.iid'), str(context)])
    finally:
        command(['docker', 'image', 'rm', parent_tag], capture_output=True)
    provider_image = (root/'provider.iid').read_text().strip()
    gateway_context = root/'gateway-context'; gateway_context.mkdir()
    shutil.copyfile(context/'fixture-cert.pem', gateway_context/'fixture-cert.pem')
    (gateway_context/'Dockerfile').write_text(f'''FROM {NODE_BASE}
COPY fixture-cert.pem /opt/fixture-cert.pem
ENV NODE_EXTRA_CA_CERTS=/opt/fixture-cert.pem
''')
    command(['docker', 'build', '--iidfile', str(root/'gateway.iid'), str(gateway_context)])
    gateway = (root/'gateway.iid').read_text().strip()
    problem = root/'problems'/'synthetic_evolution'; (problem/'tests').mkdir(parents=True)
    (problem/'config.yaml').write_text('''name: synthetic_evolution
version: 1
description: Owned synthetic evolving specification; not a benchmark task
entry_file: main.py
timeout: 30
checkpoints:
  checkpoint_1: {version: 1, order: 1, state: Full Tests}
  checkpoint_2: {version: 1, order: 2, state: Full Tests}
''')
    (problem/'tests'/'conftest.py').write_text('''import pytest
def pytest_addoption(parser):
    parser.addoption('--entrypoint')
    parser.addoption('--checkpoint')
@pytest.fixture
def entrypoint(request):
    return request.config.getoption('--entrypoint')
''')
    for n in (1, 2):
        cp = f'checkpoint_{n}'
        (problem/f'{cp}.md').write_text(f'{cp}: make main.py print {n}. Preserve the workspace and HOME.\n')
        (problem/'tests'/f'test_{cp}.py').write_text(f'''import subprocess
import shlex
def test_owned_spec(entrypoint):
    result = subprocess.run(shlex.split(entrypoint), capture_output=True, text=True)
    assert result.returncode == 0
    assert result.stdout.strip() == '{n}'
''')
        gold = problem/'solutions'/cp; gold.mkdir(parents=True)
        (gold/'main.py').write_text(f'print({n})\n')
    environment = root/'environment.yaml'
    environment.write_text(f'''type: docker
name: synthetic-protected
commands: {{command: python, entry_file: "{{entry_file}}"}}
docker:
  image: {agent}
  workdir: /workspace
''')
    prompt = root/'prompt.j2'
    prompt.write_text('{{ spec }}\n')
    lock = json.loads((source/'benchmarks/slopcodebench/upstream.json').read_text())
    contract = {'schema_version': 1, 'benchmark': 'slopcodebench', 'evidence_kind': 'synthetic-fixture',
                'source_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
                'runner_commit': lock['runner_commit'], 'dataset_commit': lock['problems_commit'], 'repetitions': 1,
                'selection': [{'id': 'synthetic_evolution', 'checkpoints': ['checkpoint_1', 'checkpoint_2']}],
                'settings': {'codex_version': '0.0.0-synthetic',
                    'codex_sha256': hashlib.sha256((context/'codex.py').read_bytes()).hexdigest(),
                    'prompt_sha256': hashlib.sha256(prompt.read_bytes()).hexdigest(),
                    'environment_digest': agent, 'harness_digest': agent, 'gateway_digest': gateway, 'carry_digest': agent,
                    'model': 'gpt-6-luna', 'reasoning': 'low', 'auth_mode': 'api-key', 'native_config': {},
                    'timeout_seconds': 30, 'retries': 1, 'concurrency': 1, 'scb_pass_policy': 'any-case'}, 'proxy': {}}
    prepared = {'source_root': str(source), 'upstream_root': str(upstream), 'problems_root': str(root/'problems'),
                'agent_image': agent, 'evaluator_image': agent, 'gateway_image': gateway, 'carry_image': agent,
                'prompt': str(prompt), 'environment': str(environment), 'tasks': {'synthetic_evolution': {
                    'definition_sha256': runtime.tree_digest(problem), 'readiness': str(root/'readiness'/'synthetic_evolution.json')}}}
    runtime.activate_upstream(prepared)
    (root/'contract.json').write_text(json.dumps(contract, indent=2)+'\n')
    (root/'prepared.json').write_text(json.dumps(prepared, indent=2)+'\n')
    print(json.dumps({'evidence_kind': 'synthetic-fixture', 'source_commit': contract['source_commit'],
                      'runner_commit': contract['runner_commit'], 'fixture_dataset_override': True,
                      'agent_image': agent, 'gateway_image': gateway,
                      'carry_binary_sha256': hashlib.sha256(carry.read_bytes()).hexdigest()}, sort_keys=True), flush=True)
    return contract, prepared, provider_image


class FixtureNetwork:
    """Only add a no-egress synthetic TLS provider; retain production network code."""
    def __init__(self, image):
        self.image, self.name = image, 'scb-fixture-provider-'+uuid.uuid4().hex
        self.secret = 'synthetic-provider-'+secrets.token_hex(16)
        self.original = swebench_smoke.start_agent_network

    def __enter__(self):
        env = {**os.environ, 'FIXTURE_EXPECTED_KEY': self.secret}
        command(['docker', 'run', '--detach', '--name', self.name, '--network=none', '--read-only',
                 '--cap-drop=ALL', '--security-opt=no-new-privileges', '--env', 'FIXTURE_EXPECTED_KEY',
                 '--entrypoint=python', self.image, '/opt/provider.py'], env=env)
        def execute(args, **kwargs):
            if args[:3] == ['docker', 'network', 'create'] and '--internal' not in args:
                args = args[:3]+['--internal']+args[3:]
                result = subprocess.run(args, **kwargs)
                if result.returncode == 0:
                    command(['docker', 'network', 'connect', '--alias', 'api.openai.com', args[-1], self.name], capture_output=True)
                return result
            return subprocess.run(args, **kwargs)
        def start(**kwargs):
            return self.original(**kwargs, execute=execute)
        self.patch = patch.object(swebench_smoke, 'start_agent_network', start)
        self.patch.start()
        original_stop = swebench_smoke.cleanup_agent_network
        def stop(network, **kwargs):
            command(['docker', 'network', 'disconnect', network['egress'], self.name], capture_output=True)
            return original_stop(network, **kwargs)
        self.stop_patch = patch.object(swebench_smoke, 'cleanup_agent_network', stop)
        self.stop_patch.start()
        self.envpatch = patch.dict(os.environ, {'OPENAI_API_KEY': self.secret})
        self.envpatch.start()
        return self

    def __exit__(self, *exc):
        self.envpatch.stop(); self.patch.stop(); self.stop_patch.stop()
        try:
            self.provider_log = subprocess.check_output(['docker', 'logs', self.name], text=True)
        finally:
            swebench_smoke.force_remove_container(self.name, exact_name=True)
