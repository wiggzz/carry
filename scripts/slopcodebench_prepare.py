#!/usr/bin/env python3
"""Hosted-only REAL official SCB preparation. No provider key or model calls.

Build public tool/dependency images, freeze the full selected problem, then invoke
only the existing official offline gold-readiness CLI. Never copy problem tests,
gold solutions, or credentials into an agent image.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import uuid

from scripts import slopcodebench_runtime as runtime
from scripts.slopcodebench import plan

SOURCE = Path(__file__).resolve().parents[1]
SELECTION = SOURCE/'benchmarks/slopcodebench/luna-cfgpipe.json'
PYTHON_BASE = 'python@sha256:593bd06efe90efa80dc4eee3948be7c0fde4134606dd40d8dd8dbcade98e669c'
NODE_BASE = 'node@sha256:1c18d9ab3af4585870b92e4dbc5cac5a0dc77dd13df1a5905cea89fc720eb05b'
CODEX_VERSION = '0.147.0'


def official_definition(upstream, problems, selection):
    runtime.activate_upstream({'upstream_root': str(upstream)})
    import yaml
    from slop_code.entrypoints.config.run_config import RunConfig
    from slop_code.evaluation import ProblemConfig
    from slop_code.evaluation.pytest_runner import PytestRunner
    defaults = RunConfig()
    problem = ProblemConfig.from_yaml(problems/selection['problem'])
    agent = yaml.safe_load((upstream/'configs/agents/codex.yaml').read_text())
    return {'prompt': upstream/'configs/prompts'/f'{defaults.prompt}.jinja',
            'environment': yaml.safe_load((upstream/'configs/environments'/f'{defaults.environment}.yaml').read_text()),
            'pass_policy': defaults.pass_policy.value,
            'timeout_seconds': agent['timeout'],
            'test_dependencies': list(PytestRunner.TEST_DEPENDENCIES)+list(problem.test_dependencies or []),
            'checkpoints': [name for name, _ in problem.iterate_checkpoint_items()]}


def prepare(root, upstream, problems, carry, *, execute=subprocess.run):
    # Trusted setup must never inherit provider authority, even on a manual run.
    if any(os.environ.get(key) for key in ('OPENAI_API_KEY', 'OPENAI_ADMIN_KEY',
                                         'CARRY_PROXY_UPSTREAM_KEY', 'BENCHMARK_BUDGET_STATE')):
        raise ValueError('preparation must not receive provider credentials or spend authority')
    if os.environ.get('GITHUB_ACTIONS') != 'true':
        raise ValueError('real image preparation requires a disposable GitHub-hosted runner')
    root, upstream, problems, carry = (Path(p).resolve() for p in (root, upstream, problems, carry))
    selection = json.loads(SELECTION.read_text())
    lock = json.loads((SOURCE/'benchmarks/slopcodebench/upstream.json').read_text())

    def command(args, **kwargs):
        return execute(args, check=True, text=True, **kwargs)

    source_commit = command(['git', '-C', str(SOURCE), 'rev-parse', 'HEAD'], capture_output=True).stdout.strip()
    for checkout, commit in [(SOURCE, source_commit), (upstream, lock['runner_commit']),
                             (problems, lock['problems_commit'])]:
        runtime.verify_checkout(checkout, commit)
    if (selection['runner_commit'], selection['problems_commit']) != (lock['runner_commit'], lock['problems_commit']):
        raise ValueError('study selection and upstream pins differ')
    problem_tree = command(['git', '-C', str(problems), 'rev-parse', 'HEAD:'+selection['problem']],
                           capture_output=True).stdout.strip()
    if problem_tree != selection['problem_tree_sha']:
        raise ValueError('study problem tree differs from the frozen selection')
    definition = official_definition(upstream, problems, selection)
    if definition['checkpoints'] != selection['checkpoints']:
        raise ValueError('study requires the entire pinned ordered checkpoint sequence')
    root.mkdir(parents=True, exist_ok=False)
    context = root/'image-context'; context.mkdir()
    shutil.copyfile(carry, context/'carry')
    warm = ['uvx', *['--with='+dep for dep in definition['test_dependencies']], 'pytest', '--version']
    (context/'warm-cache.sh').write_text('#!/bin/sh\nset -eu\n'+shlex.join(warm)+'\n')
    # Reuse the fixture's proven immutable bases/cache layout, not its fake Codex,
    # provider, TLS, task, or expensive synthetic role overrides.
    (context/'Dockerfile').write_text(f'''FROM {NODE_BASE} AS codex
RUN npm install --global --prefix /opt/scb-harness @openai/codex@{CODEX_VERSION}
FROM {PYTHON_BASE}
RUN apt-get update && apt-get install -y --no-install-recommends git ripgrep ca-certificates && rm -rf /var/lib/apt/lists/*
RUN pip install --no-cache-dir uv==0.9.7
COPY --from=codex /usr/local/bin/node /usr/local/bin/node
COPY --from=codex /opt/scb-harness /opt/scb-harness
ENV PATH=/opt/scb-harness/bin:$PATH UV_PYTHON_DOWNLOADS=never UV_CACHE_DIR=/tmp/uv-cache
COPY warm-cache.sh /opt/warm-cache.sh
RUN UV_CACHE_DIR=/opt/scb-uv-cache sh /opt/warm-cache.sh && chmod -R a+rX /opt/scb-uv-cache
COPY carry /opt/swebench-harness/bin/carry
RUN chmod 0555 /opt/swebench-harness/bin/carry
''')
    tag = 'carry-scb-prepared:'+uuid.uuid4().hex
    command(['docker', 'build', '--tag', tag, str(context)])
    image = command(['docker', 'image', 'inspect', '--format', '{{.Id}}', tag], capture_output=True).stdout.strip()
    runtime.image_pin(image)
    command(['docker', 'pull', NODE_BASE])
    gateway = command(['docker', 'image', 'inspect', '--format', '{{.Id}}', NODE_BASE], capture_output=True).stdout.strip()
    runtime.image_pin(gateway)
    native = command(['docker', 'run', '--rm', '--pull=never', '--network=none', '--read-only',
                      '--cap-drop=ALL', '--security-opt=no-new-privileges', '--entrypoint=/bin/sh', image,
                      '-c', 'codex --version && sha256sum "$(command -v codex)"'],
                     capture_output=True, timeout=30).stdout.splitlines()
    if len(native) != 2 or native[0] != 'codex-cli '+CODEX_VERSION:
        raise ValueError('prepared image does not contain the selected real native Codex')
    prompt = root/'prompt.jinja'; shutil.copyfile(definition['prompt'], prompt)
    environment = root/'environment.yaml'
    environment_spec = definition['environment']
    environment_spec['docker']['image'] = image
    environment.write_text(json.dumps(environment_spec, indent=2)+'\n')
    task = selection['problem']
    contract = {'schema_version': 1, 'benchmark': 'slopcodebench', 'evidence_kind': 'measured-benchmark',
                'source_commit': source_commit, 'runner_commit': lock['runner_commit'],
                'dataset_commit': lock['problems_commit'], 'repetitions': 1,
                'selection': [{'id': task, 'checkpoints': selection['checkpoints']}],
                'settings': {'codex_version': CODEX_VERSION, 'codex_sha256': native[1].split()[0],
                    'prompt_sha256': hashlib.sha256(prompt.read_bytes()).hexdigest(),
                    'environment_digest': image, 'harness_digest': image, 'gateway_digest': gateway, 'carry_digest': image,
                    'model': selection['primary_model'], 'reasoning': selection['primary_reasoning'],
                    'auth_mode': 'api-key', 'native_config': {}, 'timeout_seconds': definition['timeout_seconds'],
                    'retries': 0, 'concurrency': 1, 'budget_policy': 'between-slot-soft',
                    'scb_pass_policy': definition['pass_policy'],
                    'scb_correctness_policy': 'all-cases'},
                'proxy': {'CARRY_PROXY_CLASSIFIER_MODEL': selection['reviewer_model'],
                          'CARRY_PROXY_CLASSIFIER_EFFORT': selection['reviewer_reasoning']}}
    prepared = {'source_root': str(SOURCE), 'upstream_root': str(upstream), 'problems_root': str(problems),
                'agent_image': image, 'evaluator_image': image, 'gateway_image': gateway, 'carry_image': image,
                'prompt': str(prompt), 'environment': str(environment),
                'tasks': {task: {'definition_sha256': runtime.tree_digest(problems/task),
                                  'readiness': str(root/'readiness'/f'{task}.json')}}}
    run_plan = plan(contract)
    if run_plan['expected_records'] != selection['expected_checkpoint_arm_outcomes']:
        raise ValueError('study denominator mismatch')
    for name, value in [('contract', contract), ('prepared', prepared), ('selection', selection)]:
        (root/f'{name}.json').write_text(json.dumps(value, indent=2, sort_keys=True)+'\n')
    # Actual official grading, with its failure output retained by the workflow.
    command([sys.executable, '-B', '-m', 'scripts.slopcodebench', '--contract', str(root/'contract.json'),
             '--prepared', str(root/'prepared.json'), '--prepare-readiness'], env=dict(os.environ))
    return contract, prepared


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--upstream', type=Path, required=True)
    parser.add_argument('--problems', type=Path, required=True)
    parser.add_argument('--carry', type=Path, required=True)
    args = parser.parse_args()
    prepare(args.output, args.upstream, args.problems, args.carry)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
