#!/usr/bin/env python3
"""Official SCB paired planning and credential-free result inspection.

Live dispatch is deliberately unavailable until the official runner is connected
at its runtime boundary to the protected pull-only agent/evaluator machinery.
Do not invoke the unrestricted upstream CLI as a benchmark worker.
"""
import argparse
import hashlib
import importlib.util
import json
import logging
from pathlib import Path
import sys
import types

try:
    from benchmark_comparison import build_plan
except ModuleNotFoundError:
    from scripts.benchmark_comparison import build_plan

ROOT = Path(__file__).resolve().parents[1]
LOCK = ROOT / 'benchmarks/slopcodebench/upstream.json'
LIFECYCLE = {
    'workspace': 'per-problem-arm-repetition',
    'native_home': 'per-problem-arm-repetition',
    'checkpoint_conversation': 'fresh-exec',
    'retry_conversation': 'resume-last',
    'carry_owner': 'per-native-conversation',
}


def plan(contract, *, include_off=False):
    lock = json.loads(LOCK.read_text())
    if (contract['benchmark'] != 'slopcodebench'
            or contract['runner_commit'] != lock['runner_commit']
            or contract['dataset_commit'] != lock['problems_commit']):
        raise ValueError('SCB contract must match both pinned upstream revisions')
    return {**build_plan(contract, include_off=include_off), 'upstream': lock,
            'lifecycle': LIFECYCLE,
            'live_blockers': ['prepared isolated SCB runtime',
                              'symmetric trusted direct native receipts',
                              'official gold/readiness and cleanup integration']}


def official_report(module_path):
    """Execute the complete hash-verified upstream report component unchanged.

    The component's only non-Pydantic package dependency is an unused logger.
    Substitute stdlib logging in this offline reader, not any scoring behavior.
    This is NOT the upstream pytest/Docker evaluator or full runner integration.
    """
    lock = json.loads(LOCK.read_text())
    raw = module_path.read_bytes()
    if hashlib.sha256(raw).hexdigest() != lock['report_sha256']:
        raise ValueError('official report module SHA256 does not match pinned source')
    shim = types.ModuleType('slop_code.logging')
    shim.get_logger = logging.getLogger
    prior = sys.modules.get('slop_code.logging')
    sys.modules['slop_code.logging'] = shim
    try:
        spec = importlib.util.spec_from_loader('_carry_scb_official_report', loader=None)
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        # Execute the verified buffer itself; do not reopen after checking it.
        exec(compile(raw, str(module_path), 'exec'), module.__dict__)
        return module.CorrectnessResults
    finally:
        if prior is None:
            sys.modules.pop('slop_code.logging', None)
        else:
            sys.modules['slop_code.logging'] = prior


def inspect_scores(run_plan, contract, root, module_path):
    report_type = official_report(module_path)
    policy = contract['settings']['scb_pass_policy']
    # Validate even when every selected checkpoint is missing.
    from_policy = sys.modules[report_type.__module__].PassPolicy
    from_policy(policy)
    checkpoints = []
    for slot in run_plan['slots']:
        for checkpoint in slot['checkpoints']:
            row = {'namespace': slot['namespace'], 'task_id': slot['task_id'],
                   'checkpoint': checkpoint, 'arm': slot['name'],
                   'repetition': slot['repetition'], 'contract_sha256': run_plan['contract_sha256'],
                   'resolved': None, 'status': 'evaluation-error',
                   'evaluation_seconds': None, 'quality': None}
            directory = root / slot['namespace'] / slot['task_id'] / checkpoint
            try:
                official = report_type.from_dir(directory)
                if (official.problem_name != slot['task_id']
                        or official.checkpoint_name != checkpoint or official.schema_version != 1):
                    raise ValueError('official report identity/schema mismatch')
                row.update(evaluation_seconds=official.duration,
                           official_pass_counts=official.pass_counts,
                           official_total_counts=official.total_counts)
                # Collection-only results are evaluator unknown, not failed solves.
                if not official.infrastructure_failure and official.pytest_collected > 0 and official.tests:
                    row.update(resolved=official.passes_policy(policy), status='evaluated')
            except (OSError, ValueError, TypeError, KeyError):
                pass  # Keep this exact denominator slot unknown, never replace it.
            quality = directory / 'quality_analysis/overall_quality.json'
            if quality.is_file():
                try:
                    value = json.loads(quality.read_text())
                    if isinstance(value, dict):
                        row['quality'] = value
                except (OSError, ValueError):
                    pass
            checkpoints.append(row)
    return {'schema_version': 1, 'contract_sha256': run_plan['contract_sha256'],
            'official_report_sha256': json.loads(LOCK.read_text())['report_sha256'],
            'checkpoint_denominator': run_plan['expected_records'], 'checkpoints': checkpoints,
            'paid_dispatch': False, 'test_execution_performed': False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--contract', type=Path, required=True)
    parser.add_argument('--include-off', action='store_true')
    parser.add_argument('--allow-paid', action='store_true', help='explicit consent is necessary, not sufficient')
    parser.add_argument('--score-root', type=Path, help='read existing official checkpoint artifacts only')
    parser.add_argument('--official-report-module', type=Path, help='exact pinned upstream report.py')
    args = parser.parse_args()
    try:
        contract = json.loads(args.contract.read_text())
        result = plan(contract, include_off=args.include_off)
        if args.allow_paid:
            raise ValueError('paid dispatch blocked: ' + '; '.join(result['live_blockers']))
        if bool(args.score_root) != bool(args.official_report_module):
            raise ValueError('score-root and official-report-module must be supplied together')
        if args.score_root:
            result = inspect_scores(result, contract, args.score_root, args.official_report_module)
    except (OSError, ImportError, KeyError, TypeError, ValueError) as error:
        parser.error(str(error))
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
