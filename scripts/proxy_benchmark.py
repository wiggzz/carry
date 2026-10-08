"""Opt-in Carry proxy contracts; no provider secrets in public configuration."""
from decimal import Decimal, InvalidOperation

DEFAULTS = {
    'CARRY_PROXY_MODE': 'disabled',
    'CARRY_PROXY_HISTORY_POLICY': 'strict',
    'CARRY_PROXY_CLASSIFIER_MODEL': 'gpt-6-luna',
    'CARRY_PROXY_CLASSIFIER_EFFORT': 'low',
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
        '--classifier-reasoning-effort', config['CARRY_PROXY_CLASSIFIER_EFFORT'],
        '--payoff-requests', config['CARRY_PROXY_PAYOFF_REQUESTS'],
        '--min-payback-percent', config['CARRY_PROXY_MIN_PAYBACK_PERCENT']]


def native_cost(model, usage, *, service_tier='default'):
    import json
    from pathlib import Path
    table = json.loads((Path(__file__).resolve().parents[1] / 'benchmarks/proxy-standard-rates.json').read_text())
    rates = table['models'].get(model)
    if rates is None or service_tier not in {'default', 'standard', None}:
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
    actors = {}
    for actor in ('primary', 'shadow'):
        starts = {e['request_id'] for e in events if e.get('actor') == actor and e.get('event') == 'started'}
        completed = {e['request_id']: e for e in events
                     if e.get('actor') == actor and e.get('event') == 'completed'}
        summary = dict(requests=len(starts), completed_requests=len(starts & completed.keys()),
            censored_requests=len(starts - completed.keys()), ordinary_input_tokens=0,
            cached_input_tokens=0, cache_write_input_tokens=0, output_tokens=0,
            latency_ms=0, tool_calls=0, native_compaction_requests=0, observed_cost_usd=0.0)
        unavailable = 0
        for request in starts & completed.keys():
            event = completed[request]
            usage = event.get('usage', {})
            cost = native_cost(event.get('model'), usage, service_tier=event.get('service_tier', 'default'))
            summary['latency_ms'] += event.get('latency_ms', 0)
            summary['tool_calls'] += event.get('tool_calls') or 0
            summary['native_compaction_requests'] += int(event.get('native_compaction') is True)
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
        summary['estimated_cost_usd'] = None if unavailable or summary['censored_requests'] else summary['observed_cost_usd']
        actors[actor] = summary
    costs = [s['estimated_cost_usd'] for s in actors.values()]
    return {**actors, 'estimated_total_cost_usd': None if None in costs else sum(costs),
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
                    event = json.loads(line.split(' ', 1)[1])
                    if event.get('actor') in {'primary', 'shadow'} and isinstance(event.get('request_id'), str):
                        events.append(event)
                except (ValueError, AttributeError):
                    complete = False
        metrics = read_metrics(network, execute=execute)
    except (OSError, subprocess.SubprocessError):
        complete, metrics = False, None
    summary = summarize_events(events, metrics=metrics)
    summary['ledger_complete'] = complete and bool(summary['primary']['requests'])
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
