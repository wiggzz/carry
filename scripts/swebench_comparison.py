#!/usr/bin/env python3
"""Paired Codex SWE execution using the existing isolated worker and official grader.

Preparation resolves the existing catalog and exports a prepared harness; measured
slots never build an image. All accounting/denominators belong to benchmark_comparison.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time
from importlib.metadata import version

try:
    from scripts import benchmark_comparison as comparison, swebench_smoke as swe
    from scripts.slopcodebench_runtime import image_pin, scoped_env, verify_checkout
except ModuleNotFoundError:
    import benchmark_comparison as comparison
    import swebench_smoke as swe
    from slopcodebench_runtime import image_pin, scoped_env, verify_checkout


def bundle_digest(root):
    rows = []
    for path in sorted(root.rglob('*')):
        if path.is_symlink():
            if not path.resolve().is_relative_to(root.resolve()):
                raise ValueError('prepared harness symlink escapes bundle')
            rows.append([path.relative_to(root).as_posix(), 'symlink', os.readlink(path)])
        elif path.is_file():
            digest = hashlib.sha256()
            with path.open('rb') as stream:
                for block in iter(lambda: stream.read(1024*1024), b''):
                    digest.update(block)
            rows.append([path.relative_to(root).as_posix(), 'file', digest.hexdigest()])
    return comparison.fingerprint(rows)


def prepare(contract, config, output):
    """Use the canonical pull-only task resolver; never build a harness here."""
    comparison.build_plan(contract)
    settings = contract['settings']
    source = Path(config['SOURCE_ROOT']).resolve()
    verify_checkout(source, contract['source_commit'])
    if not config.get('PREPARED_HARNESS_IMAGES'):
        raise ValueError('export requires already prepared harness images')
    output.mkdir(parents=True, exist_ok=False)
    records = canonical_records(contract)
    dataset = output/'canonical-dataset.json'
    dataset.write_text(json.dumps(records, indent=2, sort_keys=True)+'\n')
    config = {**config, 'SOURCE_COMMIT': contract['source_commit'], 'BENCHMARK_HARNESS': 'codex',
              'CODEX_VERSION': settings['codex_version'], 'MODEL': settings['model'], 'REASONING': settings['reasoning']}
    images = swe.build_images(source=source, run_id='paired-preparation', config=config, harnesses=('codex',))
    bundles = swe.export_harness_bundles(images, output/'harness-bundles')
    tasks = swe.resolve_task_environments(records=records, source=source, repository=config['TASK_IMAGE_REPOSITORY'],
        output=output/'preparation', trusted_ca_image=config['BASE_IMAGE'], max_workers=1,
        catalog_reference=config['TASK_IMAGE_CATALOG'])
    prepared = {'source_root': str(source), 'canonical_dataset': str(dataset.resolve()), 'config': config,
                'harness_bundle': str(bundles['codex']), 'harness_bundle_sha256': bundle_digest(bundles['codex']),
                'gateway_image': config['BASE_IMAGE'], 'carry_image': config['CARRY_IMAGE'],
                'evaluation_timeout_seconds': int(config.get('EVALUATOR_TIMEOUT_SECONDS', '300')), 'tasks': tasks}
    preflight(contract, prepared)
    (output/'prepared.json').write_text(json.dumps(prepared, indent=2, sort_keys=True)+'\n')
    return {'status': 'ready', 'prepared': str(output/'prepared.json'), 'model_requests': 0}


def canonical_records(contract):
    from datasets import load_dataset
    dataset = load_dataset(swe.DATASET, split='test', revision=swe.DATASET_REVISION)
    by_id = {row['instance_id']: dict(row) for row in dataset}
    return [by_id[task['id']] for task in contract['selection']]


def preflight(contract, prepared):
    plan = comparison.build_plan(contract)
    settings = contract['settings']
    if contract['benchmark'] != 'swebench':
        raise ValueError('SWE command requires swebench contract')
    if contract['runner_commit'] != contract['source_commit'] or contract['dataset_commit'] != swe.DATASET_REVISION:
        raise ValueError('SWE pins must name this adapter source and the canonical dataset revision')
    if settings['concurrency'] != 1 or settings['retries'] != 0 or settings['native_config']:
        raise ValueError('existing SWE worker is serial, no outer retry, default native config')
    if version('swebench') != '4.1.0':
        raise ValueError('official evaluator requires swebench==4.1.0')
    source = Path(prepared['source_root'])
    if source.resolve() != Path(__file__).resolve().parents[1]:
        raise ValueError('runtime must execute from its pinned source checkout')
    verify_checkout(source, contract['source_commit'])
    records = json.loads(Path(prepared['canonical_dataset']).read_text())
    if records != canonical_records(contract):
        raise ValueError('prepared records differ from canonical pinned dataset selection')
    prompts = [[r['instance_id'], swe.task_prompt(r)] for r in records]
    if comparison.fingerprint(prompts) != settings['prompt_sha256']:
        raise ValueError('paired SWE prompts differ from contract')
    config = {**prepared['config'], **contract.get('proxy', {}), 'SOURCE_COMMIT': contract['source_commit'],
              'BENCHMARK_HARNESS': 'codex', 'CODEX_VERSION': settings['codex_version'],
              'MODEL': settings['model'], 'REASONING': settings['reasoning']}
    if not config.get('PREPARED_HARNESS_IMAGES'):
        raise ValueError('prepared harness manifest is required; builds are forbidden')
    images = swe.build_images(source=source, run_id='paired-preflight', config=config, harnesses=('codex',))
    if images['codex']['image_id'] != settings['harness_digest']:
        raise ValueError('prepared Codex harness differs from paired contract')
    bundle = Path(prepared['harness_bundle'])
    if not (bundle/'bin/adapter').is_file() or not (bundle/'bin/codex').is_file():
        raise ValueError('missing prepared native harness bundle')
    # Bind the exported bundle as well as its image; otherwise an editable host
    # bundle could silently replace the native client after image verification.
    if bundle_digest(bundle) != prepared['harness_bundle_sha256']:
        raise ValueError('prepared harness bundle changed')
    native = subprocess.check_output(['docker', 'run', '--rm', '--pull=never', '--network=none',
        '--entrypoint=/bin/sh', settings['harness_digest'], '-c',
        '/opt/swebench-harness/bin/codex --version && sha256sum "$(readlink -f /opt/swebench-harness/bin/codex)"'], text=True).splitlines()
    if len(native) != 2 or native[0] != 'codex-cli '+settings['codex_version'] or native[1].split()[0] != settings['codex_sha256']:
        raise ValueError('prepared native Codex binary/version mismatch')
    if image_pin(config['TASK_IMAGE_CATALOG']) != settings['environment_digest']:
        raise ValueError('prepared catalog differs from paired contract')
    for key, field in [('gateway_image', 'gateway_digest'), ('carry_image', 'carry_digest')]:
        if image_pin(prepared[key]) != settings[field]:
            raise ValueError('prepared trusted image differs from paired contract')
        subprocess.run(['docker', 'image', 'inspect', prepared[key]], check=True, capture_output=True, timeout=30)
    for record in records:
        task = prepared['tasks'][record['instance_id']]
        # The existing resolver already validated recipe/cache-key/readiness and
        # aliased the official remote TestSpec key to these immutable bytes.
        inspected = subprocess.check_output(['docker', 'image', 'inspect', '--format', '{{.Id}}', task['source_task_image']], text=True).strip()
        if inspected != task['evaluator_image']['image_id']:
            raise ValueError('prepared official evaluator alias changed')
        subprocess.run(['docker', 'image', 'inspect', task['agent_image']['image_id']], check=True, capture_output=True, timeout=30)
    return plan


def run_slot(contract, slot, prepared, output):
    settings = contract['settings']
    output.mkdir(parents=True, exist_ok=False)
    started = time.monotonic()
    records = json.loads(Path(prepared['canonical_dataset']).read_text())
    task = prepared['tasks'][slot['task_id']]
    swe.materialize(records=records, selected_ids=[slot['task_id']], root=output/'work', harnesses=('codex',))
    root = output/'work/tasks'/slot['task_id']
    materialization_seconds = time.monotonic()-started
    agent_output = output/'agent'; agent_output.mkdir()
    values = {**prepared['config'], **contract.get('proxy', {}), 'CARRY_PROXY_MODE': slot['proxy_mode'],
              'BENCHMARK_HARNESS': 'codex', 'BENCHMARK_TRUSTED_DIRECT': '1' if slot['proxy_mode'] == 'disabled' else '0'}
    with scoped_env(values):
        record = swe.run_isolated_agent(instance_id=slot['task_id'], harness='codex',
            image=task['agent_image']['image_id'], repo=root/'codex/repo',
            harness_bundle=Path(prepared['harness_bundle']), task_input=root/'input', output=agent_output,
            model=settings['model'], reasoning=settings['reasoning'], proxy_image=prepared['gateway_image'],
            proxy_script=Path(prepared['source_root'])/'scripts/openai_proxy.js', carry_image=prepared['carry_image'],
            timeout_seconds=settings['timeout_seconds'])
    agent_done = time.monotonic()
    row = {'namespace': slot['namespace'], 'contract_sha256': comparison.fingerprint(contract),
           'checkpoint': 'final', 'task_id': slot['task_id'], 'arm': slot['name'],
           'status': 'evaluation-error', 'resolved': None, 'quality': {},
           'events': [], 'agent_seconds': record.get('agent_seconds'),
           'setup_seconds': materialization_seconds+record['setup_seconds'] if record.get('setup_seconds') is not None else None,
           'selected_rewrites': None, 'committed_rewrites': None,
           'dispatched_rewrites': record.get('proxy_summary', {}).get('dispatched_rewrites'), 'native_compactions': None}
    event_path = output/'agent-proxy-state/benchmark-events.jsonl'
    # run_isolated_agent retains the authoritative gateway/native events here.
    if event_path.is_file():
        row['events'] = [json.loads(line) for line in event_path.read_text().splitlines()]
    else:
        row['events'] = [{'ledger_capture_incomplete': True}]
    official = output/'official'; official.mkdir()
    predictions = official/'predictions.jsonl'
    predictions.write_text(json.dumps({'instance_id': slot['task_id'], 'model_name_or_path': 'codex',
                                       'model_patch': record['patch']})+'\n')
    evaluation_started = time.monotonic()
    # No provider credential reaches the official evaluator, even via an ambient
    # secret-file path. Native spend is retained before grading failure.
    evaluator_env = {k: v for k, v in os.environ.items() if not k.startswith(('OPENAI_', 'CARRY_PROXY_'))}
    evaluator_env['CARRY_SWEBENCH_PREPARED_IMAGES'] = json.dumps({task['source_task_image']: task['evaluator_image']['image_id']})
    try:
        swe.run_official_evaluation(predictions=predictions, canonical_dataset=output/'work/canonical-dataset.json',
            instance_ids=[slot['task_id']], run_id='paired-'+slot['namespace'][:24], output=official,
            environment=evaluator_env, max_workers=1, process_timeout_seconds=int(prepared['evaluation_timeout_seconds']))
        outcomes = swe.load_official_report(official)
        swe.validate_official_outcomes(outcomes, [slot['task_id']])
        graded = [dict(record)]
        swe.apply_official_outcomes(graded, outcomes)
        if not swe.official_evaluation_unknowns(graded):
            row.update(status='evaluated', resolved=graded[0]['resolved'],
                       quality={'official_outcome': graded[0]['status']})
    except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired) as error:
        row['error'] = f'{type(error).__name__}: {error}'
    row['evaluation_seconds'] = time.monotonic()-evaluation_started
    row['task_wall_seconds'] = time.monotonic()-started
    (output/'record.json').write_text(json.dumps(row, indent=2)+'\n')
    return [row]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--contract', type=Path, required=True)
    parser.add_argument('--prepared', type=Path)
    execution = parser.add_mutually_exclusive_group()
    execution.add_argument('--run', action='store_true')
    execution.add_argument('--prepare-runtime', action='store_true')
    parser.add_argument('--config', type=Path, help='existing SWE worker config; no secrets')
    parser.add_argument('--allow-paid', action='store_true')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--budget-usd', type=float)
    args = parser.parse_args(argv)
    try:
        contract = json.loads(args.contract.read_text())
        result = comparison.build_plan(contract)
        if contract['benchmark'] != 'swebench':
            raise ValueError('SWE command requires swebench contract')
        if args.allow_paid and not args.run:
            raise ValueError('--allow-paid requires --run')
        if args.prepare_runtime:
            if not args.config or not args.output:
                raise ValueError('--prepare-runtime requires --config and --output')
            result = prepare(contract, json.loads(args.config.read_text()), args.output)
        if args.run:
            if not args.allow_paid or not args.prepared or not args.output or args.budget_usd is None:
                raise ValueError('--run requires --allow-paid, --prepared, --output and --budget-usd')
            prepared = json.loads(args.prepared.read_text())
            result = preflight(contract, prepared)
            result = comparison.dispatch(result, contract, prepared, args.output, run_slot, budget_usd=args.budget_usd)
    except (OSError, ImportError, ValueError, TypeError, KeyError) as error:
        parser.error(str(error))
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
