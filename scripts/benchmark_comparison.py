#!/usr/bin/env python3
"""Frozen Codex direct/Carry paired plans and native-receipt reports; no dispatch."""
import argparse
import hashlib
import json
import math
import re
from collections import Counter
from decimal import Decimal
from pathlib import Path
from statistics import median

try:
    from proxy_benchmark import summarize_events, validate_config
except ModuleNotFoundError:
    from scripts.proxy_benchmark import summarize_events, validate_config

ARMS = ({'name': 'direct', 'proxy_mode': 'disabled'},
        {'name': 'compact', 'proxy_mode': 'compact'})


def fingerprint(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':'),
                                     allow_nan=False).encode()).hexdigest()


def validate_contract(value):
    if value['schema_version'] != 1 or value['benchmark'] not in {'swebench', 'slopcodebench'}:
        raise ValueError('unsupported comparison contract')
    if value['evidence_kind'] not in {'synthetic-fixture', 'measured-benchmark'}:
        raise ValueError('evidence_kind must distinguish fixtures from measurements')
    for key in ('source_commit', 'runner_commit', 'dataset_commit'):
        if not re.fullmatch('[0-9a-f]{40}', value[key]):
            raise ValueError(f'{key} must be an immutable full commit')
    if type(value['repetitions']) is not int or not 1 <= value['repetitions'] <= 100:
        raise ValueError('repetitions must be 1..100')
    selection = value['selection']
    if not selection or len({task['id'] for task in selection}) != len(selection):
        raise ValueError('selection must contain unique tasks')
    for task in selection:
        if not re.fullmatch('[a-zA-Z0-9][a-zA-Z0-9_.-]{0,199}', task['id']):
            raise ValueError('invalid task id')
        checkpoints = task['checkpoints']
        if not checkpoints or len(set(checkpoints)) != len(checkpoints):
            raise ValueError('checkpoint order must be nonempty and unique')
        if any(not re.fullmatch('[a-zA-Z0-9][a-zA-Z0-9_.-]{0,199}', cp) for cp in checkpoints):
            raise ValueError('invalid checkpoint id')
        if value['benchmark'] == 'swebench' and checkpoints != ['final']:
            raise ValueError('SWE-bench has one official final outcome')
    settings = value['settings']
    for key in ('codex_sha256', 'prompt_sha256'):
        if not re.fullmatch('[0-9a-f]{64}', settings[key]):
            raise ValueError(f'{key} must be SHA256')
    for key in ('environment_digest', 'harness_digest', 'gateway_digest', 'carry_digest'):
        if not re.fullmatch('sha256:[0-9a-f]{64}', settings[key]):
            raise ValueError(f'{key} must be an immutable OCI digest')
    for key in ('codex_version', 'model', 'reasoning'):
        if not isinstance(settings[key], str) or not settings[key].strip():
            raise ValueError(f'{key} must be explicit')
    if settings['auth_mode'] != 'api-key':
        raise ValueError('this protected comparison contract supports API-key auth only')
    if not isinstance(settings['native_config'], dict):
        raise ValueError('native_config must explicitly freeze native settings')
    for key, minimum in (('timeout_seconds', 1), ('retries', 0), ('concurrency', 1)):
        if type(settings[key]) is not int or settings[key] < minimum:
            raise ValueError(f'invalid {key}')
    proxy = value.get('proxy', {})
    if set(proxy) - set(validate_config({})) or 'CARRY_PROXY_MODE' in proxy:
        raise ValueError('proxy settings cannot change paired treatment ownership')
    validate_config({**proxy, 'CARRY_PROXY_MODE': 'compact', 'BENCHMARK_HARNESS': 'codex'})
    return value


def build_plan(contract, *, include_off=False):
    validate_contract(contract)
    identity = fingerprint(contract)
    arms = [*ARMS]
    if include_off:
        arms.append({'name': 'forward-only', 'proxy_mode': 'off'})
    slots = []
    for repetition in range(contract['repetitions']):
        for task in contract['selection']:
            for arm in arms:
                key = [identity, repetition, task['id'], arm['name']]
                slots.append({'task_id': task['id'], 'repetition': repetition, **arm,
                              'checkpoints': task['checkpoints'], 'namespace': fingerprint(key)})
    return {'schema_version': 1, 'contract_sha256': identity,
            'selection_sha256': fingerprint(contract['selection']),
            'benchmark': contract['benchmark'], 'evidence_kind': contract['evidence_kind'],
            'arms': arms, 'slots': slots,
            'expected_records': sum(len(s['checkpoints']) for s in slots),
            'paid_dispatch': False}


def measured_sum(rows, key):
    values = [row.get(key) for row in rows]
    if any(value is None for value in values):
        return None
    if any(type(value) not in (int, float) or not math.isfinite(value) or value < 0
           for value in values):
        raise ValueError(f'invalid measurement: {key}')
    return float(sum(Decimal(str(value)) for value in values))


def summarize(plan, records):
    """Reject missing slots; retain terminal errors in every paired denominator.

    The caller provides trusted checkpoint-local native events, not client token
    estimates. This is an offline reader, not an attestation of artifact origin.
    """
    expected = {(s['namespace'], cp): s for s in plan['slots'] for cp in s['checkpoints']}
    rows = {}
    for row in records:
        key = (row['namespace'], row['checkpoint'])
        if key not in expected or key in rows or row['contract_sha256'] != plan['contract_sha256']:
            raise ValueError('duplicate, unknown or source-mismatched record')
        if row['status'] not in {'evaluated', 'agent-failed', 'agent-timeout',
                                 'infrastructure-error', 'evaluation-error', 'not-run'}:
            raise ValueError('unknown terminal status')
        resolved = row['resolved']
        if resolved is not None and type(resolved) is not bool:
            raise ValueError('resolved must be official boolean or unknown')
        if row['status'] != 'evaluated' and resolved is True:
            raise ValueError('resolved requires official evaluation evidence')
        if row['status'] == 'evaluated' and resolved is None:
            raise ValueError('evaluated record requires an official score')
        if not isinstance(row['events'], list) or not isinstance(row['quality'], dict):
            raise ValueError('native events and official quality metrics are required')
        rows[key] = row
    if rows.keys() != expected.keys():
        raise ValueError('incomplete fixed denominator')
    arms = {}
    for arm in plan['arms']:
        selected = [rows[key] for key, slot in expected.items() if slot['name'] == arm['name']]
        native = summarize_events([event for row in selected for event in row['events']])
        # Empty events cannot establish a free failed/unlaunched checkpoint even
        # when a different checkpoint provides a complete priced ledger.
        complete = all(summarize_events(row['events'])['estimated_total_cost_usd'] is not None
                       for row in selected)
        total = native['estimated_total_cost_usd'] if complete else None
        counts = Counter(row['status'] for row in selected)
        arm_report = {'denominator': len(selected),
                      'resolved': sum(row['resolved'] is True for row in selected),
                      'unknown_scores': sum(row['resolved'] is None for row in selected),
                      'infrastructure_errors': sum(counts[k] for k in
                          ('infrastructure-error', 'evaluation-error', 'not-run')),
                      'terminal_statuses': dict(counts), 'native': native,
                      'estimated_api_usd': total,
                      'observed_api_usd_lower_bound': native['observed_cost_lower_bound_usd']}
        for metric in ('agent_seconds', 'setup_seconds', 'evaluation_seconds', 'task_wall_seconds',
                       'selected_rewrites', 'committed_rewrites', 'native_compactions'):
            arm_report[metric] = measured_sum(selected, metric)
        arm_report['median_agent_seconds'] = (None if arm_report['agent_seconds'] is None
                                               else median(row['agent_seconds'] for row in selected))
        arms[arm['name']] = arm_report
    paired = Counter()
    checkpoints = []
    by_task = {(s['task_id'], s['repetition'], s['name']): s for s in plan['slots']}
    for (task, rep, arm), slot in by_task.items():
        if arm != 'direct':
            continue
        other = by_task[(task, rep, 'compact')]
        for checkpoint in slot['checkpoints']:
            direct, compact = rows[(slot['namespace'], checkpoint)], rows[(other['namespace'], checkpoint)]
            a, b = direct['resolved'], compact['resolved']
            label = ('unknown' if None in (a, b) else 'both_resolved' if a and b
                     else 'direct_only_resolved' if a else 'compact_only_resolved' if b
                     else 'neither_resolved')
            paired[label] += 1
            checkpoints.append({'task_id': task, 'repetition': rep, 'checkpoint': checkpoint,
                                'direct_resolved': a, 'compact_resolved': b,
                                'direct_quality': direct['quality'], 'compact_quality': compact['quality']})
    for key in ('both_resolved', 'direct_only_resolved', 'compact_only_resolved', 'neither_resolved', 'unknown'):
        paired.setdefault(key, 0)
    for metric in ('agent_seconds', 'task_wall_seconds', 'estimated_api_usd'):
        a, b = arms['direct'][metric], arms['compact'][metric]
        paired['compact_minus_direct_' + metric] = None if a is None or b is None else b - a
    return {'schema_version': 1, 'contract_sha256': plan['contract_sha256'],
            'timing_basis': 'summed-checkpoint-intervals-not-cohort-wall',
            'benchmark': plan['benchmark'], 'evidence_kind': plan['evidence_kind'],
            'expected_records': len(expected),
            'cost_basis': 'modeled-standard-api-usd-not-invoice',
            'infrastructure_cost_included': False, 'arms': arms, 'paired': dict(paired),
            'checkpoints': checkpoints,
            'comparison_valid': all(not a['infrastructure_errors'] and not a['unknown_scores']
                                    for a in arms.values()),
            'cost_comparison_complete': all(a['estimated_api_usd'] is not None for a in arms.values()),
            'rewrite_efficacy_observed': bool(arms['compact']['committed_rewrites'])}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--contract', type=Path, required=True)
    parser.add_argument('--include-off', action='store_true', help='optional transport diagnostic')
    parser.add_argument('--records', type=Path, help='offline trusted checkpoint JSONL')
    args = parser.parse_args()
    try:
        result = build_plan(json.loads(args.contract.read_text()), include_off=args.include_off)
        if args.records:
            result = summarize(result, [json.loads(line) for line in args.records.read_text().splitlines()
                                        if line.strip()])
    except (ValueError, KeyError, TypeError) as error:
        parser.error(str(error))
    print(json.dumps(result, indent=2, sort_keys=True, allow_nan=False))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
