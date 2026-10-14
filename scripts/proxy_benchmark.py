"""Opt-in Carry proxy contracts; no provider secrets in public configuration."""
from decimal import Decimal, InvalidOperation

DEFAULTS = {
    'CARRY_PROXY_MODE': 'disabled',
    'CARRY_PROXY_HISTORY_POLICY': 'strict',
    'CARRY_PROXY_CLASSIFIER_MODEL': 'gpt-6-luna',
    'CARRY_PROXY_CLASSIFIER_EFFORT': 'low',
    # This trusted lane has a fixed OpenAI upstream even behind the local gateway.
    'CARRY_PROXY_CLASSIFIER_CACHE_POLICY': 'openai-explicit',
    'CARRY_PROXY_PAYOFF_REQUESTS': '1',
    'CARRY_PROXY_MIN_PAYBACK_PERCENT': '25',
}


def validate_config(values):
    config = {key: values.get(key, default) for key, default in DEFAULTS.items()}
    if config['CARRY_PROXY_MODE'] not in {'disabled', 'off', 'audit', 'compact'}:
        raise ValueError('CARRY_PROXY_MODE must be disabled, off, audit, or compact')
    if config['CARRY_PROXY_HISTORY_POLICY'] not in {'strict', 'reset-on-divergence'}:
        raise ValueError('CARRY_PROXY_HISTORY_POLICY must be strict or reset-on-divergence')
    if config['CARRY_PROXY_MODE'] != 'disabled':
        if values.get('BENCHMARK_HARNESS', '') not in {'codex', 'pi'}:
            raise ValueError('proxy lanes require exactly one Codex or Pi harness')
        # Cross-task retained-context reuse needs a persistent trusted sidecar;
        # ordinary slots use fresh state and cannot silently claim that contract.
        if values.get('BENCHMARK_MODE', '') in {'session-smoke-5', 'session-20'}:
            raise ValueError('proxy retained-session benchmark modes are not yet supported')
    if config['CARRY_PROXY_CLASSIFIER_EFFORT'] not in {'minimal', 'low', 'medium', 'high'}:
        raise ValueError('invalid CARRY_PROXY_CLASSIFIER_EFFORT')
    model = config['CARRY_PROXY_CLASSIFIER_MODEL']
    policy = config['CARRY_PROXY_CLASSIFIER_CACHE_POLICY']
    if policy not in {'auto', 'disabled', 'openai-explicit'}:
        raise ValueError('CARRY_PROXY_CLASSIFIER_CACHE_POLICY must be auto, disabled, or openai-explicit')
    if policy == 'openai-explicit' and model not in {'gpt-6-luna', 'gpt-6-sol', 'gpt-6.1-sol'}:
        raise ValueError('openai-explicit requires a supported classifier model')
    if not model or not model.isascii() or any(c.isspace() or c in '\x00\r\n' for c in model):
        raise ValueError('invalid CARRY_PROXY_CLASSIFIER_MODEL')
    horizon = config['CARRY_PROXY_PAYOFF_REQUESTS']
    if not horizon.isascii() or not horizon.isdecimal() or not 1 <= int(horizon) <= 100:
        raise ValueError('CARRY_PROXY_PAYOFF_REQUESTS must be a positive ASCII integer')
    value = config['CARRY_PROXY_MIN_PAYBACK_PERCENT']
    if not value.isascii() or not value.isdecimal():
        raise ValueError('CARRY_PROXY_MIN_PAYBACK_PERCENT must be an ASCII integer')
    try:
        margin = Decimal(value)
    except InvalidOperation as error:
        raise ValueError('invalid CARRY_PROXY_MIN_PAYBACK_PERCENT') from error
    if not margin.is_finite() or not 0 <= margin <= 100:
        raise ValueError('CARRY_PROXY_MIN_PAYBACK_PERCENT must be between 0 and 100')
    return config


def sidecar_command(*, image, name, network, state_dir, config):
    import os
    return ['docker', 'run', '--detach', '--name', name, '--network', network,
        '--user', f'{os.getuid()}:{os.getgid()}',
        '--network-alias', 'carry-context-proxy', '--read-only', '--cap-drop=ALL',
        '--security-opt', 'no-new-privileges', '--tmpfs', '/tmp:rw,nosuid,nodev,size=16m',
        '--mount', f'type=bind,src={state_dir.resolve()},dst=/proxy-state',
        '--env', 'CARRY_PROXY_UPSTREAM_KEY', '--env', 'CARRY_PROXY_CLASSIFIER_KEY',
        '--env', 'CARRY_PROXY_AUTH_TOKEN', '--entrypoint', '/opt/swebench-harness/bin/carry',
        image, 'proxy', '--listen', '0.0.0.0:8787',
        '--upstream-url', 'https://api.openai.com/v1/responses',
        '--classifier-url', 'http://openai-proxy:8080/v1/responses',
        '--state-dir', '/proxy-state', '--mode', config['CARRY_PROXY_MODE'],
        '--classifier-model', config['CARRY_PROXY_CLASSIFIER_MODEL'],
        '--classifier-cache-policy', config['CARRY_PROXY_CLASSIFIER_CACHE_POLICY'],
        '--classifier-reasoning-effort', config['CARRY_PROXY_CLASSIFIER_EFFORT'],
        '--payoff-requests', config['CARRY_PROXY_PAYOFF_REQUESTS'],
        '--min-payback-percent', config['CARRY_PROXY_MIN_PAYBACK_PERCENT']]


def native_cost(model, usage, *, service_tier='default'):
    import json
    from pathlib import Path
    table = json.loads((Path(__file__).resolve().parents[1] / 'benchmarks/proxy-standard-rates.json').read_text())
    if not isinstance(model, str) or service_tier not in ('default', 'standard', None):
        return None
    rates = table['models'].get(model)
    if rates is None:
        return None
    if not isinstance(usage, dict):
        return None
    details = usage.get('input_tokens_details', {})
    if (not isinstance(details, dict) or 'cached_tokens' not in details
            or set(details) - {'cached_tokens', 'cache_write_tokens'}):
        return None
    output_details = usage.get('output_tokens_details', {})
    if (not isinstance(output_details, dict)
            or any(value != 0 for key, value in output_details.items()
                   if key != 'reasoning_tokens')):
        # Only text/reasoning output is covered by this reviewed price table.
        # Do not assign the text rate to audio or other unreviewed categories.
        return None
    names = ('input_tokens', 'output_tokens')
    if any(type(usage.get(key)) is not int or usage[key] < 0 for key in names):
        return None
    cached = details.get('cached_tokens', 0)
    writes = details.get('cache_write_tokens', 0)
    if any(type(value) is not int or value < 0 for value in (cached, writes)):
        return None
    ordinary = usage['input_tokens'] - cached - writes
    if ordinary < 0:
        return None
    multiplier = Decimal(2) if usage['input_tokens'] > 272000 else Decimal(1)
    output_multiplier = Decimal('1.5') if multiplier == 2 else Decimal(1)
    return float((multiplier * (ordinary * Decimal(rates['ordinary_input']) +
        cached * Decimal(rates['cached_input']) + writes * Decimal(rates['cache_write_input'])) +
        output_multiplier * usage['output_tokens'] * Decimal(rates['output'])) / Decimal(1000000))


def summarize_events(events, *, metrics=None):
    import json
    from collections import Counter, defaultdict
    valid = []
    malformed = 0
    for event in events:
        if (not isinstance(event, dict) or event.get('actor') not in ('primary', 'shadow')
                or not isinstance(event.get('request_id'), str) or not event['request_id']
                or event.get('event') not in ('started', 'completed', 'censored')):
            malformed += 1
        else:
            # Keep identifiable receipts even when ancillary counters are bad:
            # independently known usage still contributes to the lower bound.
            bad_counter = any(key in event and event[key] is not None
                and (type(event[key]) is not int or event[key] < 0)
                for key in ('latency_ms', 'tool_calls', 'http_status'))
            bad_metadata = (any(event.get(key) is not None and not isinstance(event[key], str)
                for key in ('model', 'service_tier'))
                or ('native_compaction' in event and type(event['native_compaction']) is not bool))
            usage = event.get('usage')
            bad_usage = event['event'] == 'completed' and not isinstance(usage, dict)
            if event['event'] == 'completed' and isinstance(usage, dict):
                for key, value in usage.items():
                    if key in ('input_tokens_details', 'output_tokens_details'):
                        bad_usage |= not isinstance(value, dict) or any(
                            type(part) is not int or part < 0 for part in value.values())
                    else:
                        bad_usage |= type(value) is not int or value < 0
            malformed += int(bad_counter or bad_metadata or bad_usage)
            valid.append(event)
    events = valid
    owners = defaultdict(set)
    for event in events:
        owners[event['request_id']].add(event['actor'])
    actors = {}
    for actor in ('primary', 'shadow'):
        lifecycles = defaultdict(list)
        for event in events:
            if event['actor'] == actor:
                lifecycles[event['request_id']].append(event['event'])
        lifecycle_errors = sum(sequence not in (['started', 'completed'], ['started', 'censored'])
                               for sequence in lifecycles.values())
        counts = Counter((e['request_id'], e.get('event')) for e in events if e.get('actor') == actor)
        starts = {e['request_id'] for e in events if e.get('actor') == actor and e.get('event') == 'started'}
        receipts = defaultdict(list)
        for event in events:
            if event.get('actor') == actor and event.get('event') == 'completed':
                receipts[event['request_id']].append(event)
        conflicts = {request for request in lifecycles if len(owners[request]) != 1}
        conflicts.update(request for request, records in receipts.items()
                         if any(record != records[0] for record in records[1:]))
        # Conflicting ancillary counters do not erase an agreed billing receipt.
        # Conflicting model/tier/usage or actor identity must never be chosen by order.
        # JSON signatures distinguish billing integers from malformed floats/bools.
        def billing(record):
            return json.dumps((record.get('model'), record.get('service_tier', 'default'),
                               record.get('usage')), sort_keys=True)
        pricing_conflicts = {request for request, records in receipts.items()
            if len(owners[request]) != 1 or any(billing(record) != billing(records[0]) for record in records[1:])}
        conflicts |= pricing_conflicts
        completed = {request: records[0] for request, records in receipts.items() if request not in pricing_conflicts}
        summary = dict(requests=len(starts), completed_requests=len(receipts),
            conflicting_requests=len(conflicts), lifecycle_errors=lifecycle_errors,
            completion_records=sum(count for (_, event), count in counts.items() if event == 'completed'),
            duplicate_records=sum(count - 1 for count in counts.values()),
            orphan_completed_requests=len(receipts.keys() - starts),
            censored_requests=len(starts - receipts.keys()), ordinary_input_tokens=0,
            cached_input_tokens=0, cache_write_input_tokens=0, output_tokens=0,
            latency_ms=0, tool_calls=0, native_compaction_requests=0, observed_cost_usd=0.0)
        unavailable = len(pricing_conflicts)
        for request in completed:
            event = completed[request]
            usage = event.get('usage', {})
            cost = native_cost(event.get('model'), usage, service_tier=event.get('service_tier', 'default'))
            for key in ('latency_ms', 'tool_calls'):
                value = event.get(key)
                if (type(value) is int and value >= 0
                        and all(record.get(key) == value for record in receipts[request])):
                    summary[key] += value
            summary['native_compaction_requests'] += int(all(
                record.get('native_compaction') is True for record in receipts[request]))
            if cost is None:
                unavailable += 1
            else:
                summary['observed_cost_usd'] += cost
            details = usage.get('input_tokens_details', {}) if isinstance(usage, dict) else {}
            if not isinstance(usage, dict) or not isinstance(details, dict):
                continue
            cached, writes = details.get('cached_tokens', 0), details.get('cache_write_tokens', 0)
            parts = (usage.get('input_tokens'), usage.get('output_tokens'), cached, writes)
            if any(type(v) is not int or v < 0 for v in parts) or parts[0] < cached + writes:
                continue
            summary['ordinary_input_tokens'] += usage['input_tokens'] - cached - writes
            summary['cached_input_tokens'] += cached
            summary['cache_write_input_tokens'] += writes
            summary['output_tokens'] += usage['output_tokens']
        summary['unpriced_requests'] = unavailable
        summary['estimated_cost_usd'] = None if (malformed or lifecycle_errors or unavailable or summary['censored_requests']
            or summary['orphan_completed_requests'] or summary['duplicate_records']
            or summary['conflicting_requests']) else summary['observed_cost_usd']
        actors[actor] = summary
    costs = [s['estimated_cost_usd'] for s in actors.values()]
    ledger_complete = not malformed and bool(actors['primary']['requests']) and not any(
        s['lifecycle_errors'] or s['conflicting_requests'] for s in actors.values())
    return {**actors, 'malformed_records': malformed, 'ledger_complete': ledger_complete,
        'estimated_total_cost_usd': None if not ledger_complete or None in costs else sum(costs),
        'observed_cost_lower_bound_usd': sum(s['observed_cost_usd'] for s in actors.values()),
        'proxy_rewrites': metrics.get('rewrites') if metrics else None,
        'native_compactions': metrics.get('native_compactions') if metrics else None,
        'effective_mode': metrics.get('mode') if metrics else None}


def read_metrics(network, *, execute=None):
    import json
    import subprocess
    execute = execute or subprocess.run
    script = """fetch('http://carry-context-proxy:8787/carry/metrics', {
      headers: {authorization:'Bearer '+process.env.CARRY_PROXY_AUTH_TOKEN,
        'x-carry-session':process.env.BENCHMARK_SESSION_ID,
        'x-carry-tenant':'benchmark','x-carry-branch':'main'},
      signal:AbortSignal.timeout(3000)})
      .then(async r => {if (!r.ok) process.exit(1); console.log(JSON.stringify(await r.json()));})
      .catch(() => process.exit(1));"""
    result = execute(['docker', 'exec', network['proxy'], 'node', '-e', script],
                     check=False, capture_output=True, text=True, timeout=10)
    if result.returncode:
        return None
    try:
        value = json.loads(result.stdout)
        # Persist only agreed public counters, never arbitrary metric payloads.
        return {'mode': value.get('mode'), 'rewrites': value.get('compactions', value.get('rewrites')),
                'native_compactions': value.get('native_compactions')}
    except (ValueError, AttributeError):
        return None


def capture_evidence(network, *, execute=None):
    import json
    from pathlib import Path
    import subprocess
    execute = execute or subprocess.run
    events = []
    complete = True
    try:
        result = execute(['docker', 'logs', network['proxy']], check=False, capture_output=True,
                         text=True, timeout=30)
        complete = result.returncode == 0
        for line in result.stdout.splitlines():
            if line.startswith('BENCHMARK_CONTEXT_EVENT '):
                try:
                    events.append(json.loads(line.split(' ', 1)[1]))
                except (ValueError, AttributeError):
                    complete = False
        metrics = read_metrics(network, execute=execute)
    except (OSError, subprocess.SubprocessError):
        complete, metrics = False, None
    summary = summarize_events(events, metrics=metrics)
    summary['ledger_complete'] = complete and summary['ledger_complete']
    if not summary['ledger_complete']:
        summary['estimated_total_cost_usd'] = None
        for actor in ('primary', 'shadow'):
            summary[actor]['estimated_cost_usd'] = None
    state_dir = Path(network['state_dir'])
    (state_dir / 'benchmark-events.jsonl').write_text(''.join(json.dumps(e, sort_keys=True) + '\n' for e in events))
    (state_dir / 'benchmark-summary.json').write_text(json.dumps(summary, indent=2, sort_keys=True) + '\n')
    return summary


def provenance(values):
    config = validate_config(values)
    return {key.removeprefix('CARRY_PROXY_').lower(): value for key, value in config.items()}


def validate_report(report, records, values):
    expected = provenance(values)
    actual = report.get('provenance', {}).get('proxy')
    if actual is None and expected['mode'] == 'disabled':
        return  # Explicit legacy direct semantics only.
    if actual != expected:
        raise ValueError('reported proxy settings do not match dispatched settings')
    if expected['mode'] != 'disabled':
        if not records or any(r.get('proxy_summary', {}).get('effective_mode') != expected['mode'] for r in records):
            raise ValueError('slot effective proxy mode missing or mismatched')


if __name__ == '__main__':
    import json
    import os
    import sys
    import shlex
    config = validate_config(os.environ)
    if '--shell' in sys.argv:
        for key, value in config.items():
            print(f'export {key}={shlex.quote(value)}')
    else:
        print(json.dumps(provenance(os.environ), sort_keys=True))
