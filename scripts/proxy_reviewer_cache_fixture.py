#!/usr/bin/env python3
"""Integrated Carry binary + credential-free loopback cache simulator.

Synthetic counters below test protocol/accounting, NOT live provider caching,
inference, token counts, prices or savings. Never installs or builds anything.
Run old binary with --policy-arg none for a behavioral RED (no new CLI flag).
"""
import argparse
import copy
import hashlib
import http.server
import json
import os
from pathlib import Path
import socket
import subprocess
import threading
import time
import urllib.request


try:
    from .proxy_native_fixture import review_records
except ImportError:
    from proxy_native_fixture import review_records


def text(item):
    content = item.get('content', '')
    if isinstance(content, str):
        return content
    assert len(content) == 1 and content[0]['type'] == 'input_text'
    return content[0]['text']


def canonical(items):
    result = copy.deepcopy(items)
    for item in result:
        content = item.get('content')
        if isinstance(content, list):
            for block in content:
                block.pop('prompt_cache_breakpoint', None)
            if len(content) == 1 and content[0].get('type') in ('input_text', 'output_text'):
                item['content'] = content[0]['text']
    return result


def without_markers(items):
    # Preserve native wrappers, IDs, reasoning and phase; remove ONLY our marks.
    items = copy.deepcopy(items)
    for item in items:
        for block in item.get('content', []) if isinstance(item.get('content'), list) else []:
            marker = block.pop('prompt_cache_breakpoint', None)
            assert marker is None or marker == {'mode': 'explicit'}
    return items


def prefix_identity(body, end):
    items = without_markers(body['input'][:end])
    wire = json.dumps(items, separators=(',', ':'), ensure_ascii=False).encode()
    return hashlib.sha256(wire).hexdigest()


def grade_cache_records(records, history):
    """Check the shared purpose-neutral evidence against actual outbound bodies."""
    assert len(records) <= 128
    assert len(json.dumps(records, separators=(',', ':')).encode()) <= 65536
    digest = lambda v: hashlib.sha256(json.dumps(v, sort_keys=True, separators=(',', ':'),
                                               ensure_ascii=False).encode()).hexdigest()
    required = {'route', 'credential_sha256', 'base_sha256', 'input_sha256',
                'input_len', 'estimated_tokens', 'at', 'ttl_seconds', 'native_cached_fraction'}
    optional = {'native_input_tokens', 'native_write_tokens', 'write_window'}
    def unsigned(value):
        return type(value) is int and 0 <= value < 2 ** 64
    def nonnegative(value):
        import math
        return type(value) in (int, float) and math.isfinite(value) and value >= 0
    for record in records:
        assert required <= set(record) <= required | optional
        assert all(unsigned(record[key]) for key in ('input_len', 'at', 'ttl_seconds'))
        assert nonnegative(record['estimated_tokens'])
        assert nonnegative(record['native_cached_fraction']) and record['native_cached_fraction'] <= 1
        assert record['at'] > 0 and record['ttl_seconds'] == 1800
        for key in ('native_input_tokens', 'native_write_tokens'):
            if key in record:
                assert unsigned(record[key])
        if 'native_input_tokens' in record:
            assert record['native_input_tokens'] > 0
        if 'native_write_tokens' in record:
            assert 'native_input_tokens' in record
            assert record['native_write_tokens'] <= record['native_input_tokens']
        if 'write_window' in record:
            window = record['write_window']
            assert isinstance(window, dict) and set(window) == {'not_before', 'not_after', 'lifetime'}
            assert all(unsigned(value) for value in window.values())
            assert window['not_before'] <= window['not_after'] == record['at']
            assert window['lifetime'] == record['ttl_seconds']
            assert record.get('native_input_tokens', 0) > 0
            assert record.get('native_write_tokens') == record['native_input_tokens']
            assert record['native_cached_fraction'] == 0
        # A raw read or an untimed/partial write remains valid observation data;
        # this schema grader must not promote it to reusable prefix evidence.
        assert any(0 < record['input_len'] <= len(body['input'])
                   and record['input_sha256'] == digest(without_markers(body['input'][:record['input_len']]))
                   and record['base_sha256'] == digest({k: v for k, v in body.items() if k not in ('input', 'stream')})
                   for body in history), 'cache identity must bind an actual wire prefix and settings'


def boundaries(body):
    return [index + 1 for index, item in enumerate(body['input'])
            if isinstance(item.get('content'), list)
            and any('prompt_cache_breakpoint' in block for block in item['content'])]


def implicit_eligible_endings(items):
    """Documented GPT-5.6+ message boundaries; not a live cache oracle."""
    def tool(item):
        return item.get('role') == 'tool' or item.get('type') in (
            'function_call_output', 'custom_tool_call_output')

    initial_developers = 0
    while initial_developers < len(items) and items[initial_developers].get('role') == 'developer':
        initial_developers += 1
    return [n + 1 for n, item in enumerate(items)
            if item.get('role') == 'user'
            or (initial_developers and n + 1 == initial_developers)
            or (tool(item) and (n + 1 == len(items) or not tool(items[n + 1])))]


class Provider:
    def __init__(self, case='rotation'):
        self.case = case
        self.reviews = []
        self.review_outputs = []
        self.primary = []
        self.primary_raw_hashes = []
        self.receipts = []
        self.cache = set()
        self.errors = []
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, format, *args):
                pass

            def do_POST(self):
                try:
                    wire = self.rfile.read(int(self.headers['content-length']))
                    body = json.loads(wire)
                    if self.path == '/classifier':
                        owner.reviews.append(body)
                        points = boundaries(body)
                        assert len(points) <= 4, 'more than four explicit WRITE slots'
                        for item in body['input']:
                            for block in item.get('content', []) if isinstance(item.get('content'), list) else []:
                                if 'prompt_cache_breakpoint' in block:
                                    assert block['prompt_cache_breakpoint'] == {'mode': 'explicit'}
                        data = canonical(body['input'])
                        ledger = json.loads(data[-1]['content'])
                        assert 'eligible_group_ids' in ledger
                        base = {k: v for k, v in body.items() if k != 'input'}
                        key = lambda n: json.dumps([base, n, prefix_identity(body, n)], sort_keys=True)
                        # GPT-5.6+ implicit mode WRITES the latest eligible
                        # ending, not every earlier stable prefix. LOOKUP may
                        # inspect it and up to 20 earlier eligible endings.
                        # This simulator is protocol evidence, not provider proof.
                        explicit = bool(points)
                        eligible = implicit_eligible_endings(data)
                        candidates = points if explicit else eligible[-21:]
                        matched = [n for n in candidates if key(n) in owner.cache]
                        cached = 2048 if matched else 0
                        if owner.case == 'missing-usage' and len(owner.reviews) <= 2:
                            cached = 0
                        for point in points if explicit else eligible[-1:]:
                            owner.cache.add(key(point))
                        owner.receipts.append({'synthetic_cached_tokens': cached,
                                               'matched_boundaries': matched,
                                               'transmitted_boundaries': points,
                                               'synthetic_cache_mode': 'explicit' if explicit else 'implicit'})
                        ids = ledger['eligible_group_ids']
                        targets = [record['group_id'] for record in review_records(body)
                                   if record.get('group_id') in ids
                                   and 'DISCARD_NATIVE_BYTES' in json.dumps(record.get('items', []))]
                        # Omission preserves Rust's existing opinion. Audit
                        # cases explicitly replace the same eligible ID so the
                        # final two no-append reviews really change the ledger.
                        advice = {'protected': ids[:1] if owner.case == 'coupled' or len(owner.reviews) % 2 else [],
                                  'removable': sorted(set(targets)) if owner.case == 'coupled' else
                                               ([] if len(owner.reviews) % 2 else ids[:1]), 'memories': []}
                        sequence = len(owner.reviews)
                        output = [
                            {'type': 'reasoning', 'id': f'review-reasoning-{sequence}',
                             'summary': [{'type': 'summary_text', 'text': f'Synthetic reasoning {sequence}'}],
                             'encrypted_content': f'FIXTURE_ONLY_OPAQUE_REVIEW_REASONING_{sequence}'},
                            {'type': 'message', 'id': f'review-commentary-{sequence}',
                             'role': 'assistant', 'phase': 'commentary',
                             'content': [{'type': 'output_text', 'text': f'Synthetic progress {sequence}'}]},
                            {'type': 'message', 'id': f'review-final-{sequence}',
                             'role': 'assistant', 'phase': 'final_answer',
                             'content': [{'type': 'output_text', 'text': json.dumps(advice)}]},
                        ]
                        owner.review_outputs.append(copy.deepcopy(output))
                    else:
                        assert self.path == '/v1/responses'
                        owner.primary.append(body)
                        owner.primary_raw_hashes.append(hashlib.sha256(wire).hexdigest())
                        output = []
                        cached = 0
                    value = {'id': 'fixture', 'model': body['model'], 'status': 'completed',
                             'output': output, 'usage': {'input_tokens': 10000,
                             'output_tokens': 10, 'input_tokens_details': {'cached_tokens': cached}}}
                    if owner.case == 'missing-usage' and self.path == '/classifier':
                        if len(owner.reviews) == 1:
                            del value['usage']
                        elif len(owner.reviews) == 2:
                            value['usage']['input_tokens_details']['cache_write_tokens'] = 10000
                    raw = json.dumps(value).encode()
                    self.send_response(200)
                    self.send_header('content-type', 'application/json')
                    self.send_header('content-length', str(len(raw)))
                    self.end_headers()
                    self.wfile.write(raw)
                except Exception as exc:
                    owner.errors.append(str(exc))
                    self.send_response(500)
                    self.end_headers()

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.url = 'http://127.0.0.1:' + str(self.server.server_port)

    def __enter__(self):
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *args):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=3)


def grade_projection(reviews, outputs):
    assert len(reviews) == len(outputs), 'each actual reviewer result must be captured'
    output_ids = {item['id'] for output in outputs for item in output}
    for body in reviews:
        items = without_markers(body['input'])
        assert not any(item.get('id') in output_ids for item in items), 'disposable reviewer output must not replay'
        records = review_records(body)
        groups = [r for r in records if 'member_ids' in r]
        members = [m for r in groups for m in r['member_ids']]
        assert len(members) == len(set(members)), 'native source members must occur once per projection'


def grade(reviews, receipts, enabled=True, missing_usage=False, outputs=None):
    assert len(reviews) >= 8, 'at least eight actual reviews required'
    assert len(reviews) == len(receipts), 'every actual review has one simulator receipt'
    for body in reviews:
        assert json.loads(text(body['input'][-1])).get('current_groups'), 'missing current ledger groups'
        points = boundaries(body)
        if enabled:
            assert points == [len(body['input']) - 1], 'one stable observation boundary, not ledger'
            assert body['prompt_cache_options'] == {'mode': 'explicit'}
        else:
            assert not points and 'prompt_cache_options' not in body, 'disabled means no explicit markers'
    if outputs is not None:
        grade_projection(reviews, outputs)
    assert canonical(reviews[-1]['input'][:-1]) == canonical(reviews[-2]['input'][:-1]), 'ledger-only review required'
    assert text(reviews[-1]['input'][-1]) != text(reviews[-2]['input'][-1]), 'ledger really must change'
    # No rotating historical markers or answer replay is required. Only an
    # actually matching stable prefix may produce the final synthetic hit.
    if not missing_usage:
        assert receipts[-1]['synthetic_cached_tokens'] > 0, 'unchanged stable observations must be reusable'


def grade_coupled(provider, sent, state, enabled):
    assert len(provider.primary) == len(sent) == 11 and len(provider.reviews) >= 8
    def native(body):
        return [i for i in body['input'] if i.get('call_id') in ('native-a', 'native-b') or i.get('type') == 'reasoning']
    assert len(native(sent[2])) == 5, 'actual native parallel cohort must be supplied'
    assert native(provider.primary[2]) == native(sent[2]), 'new native bytes must first receive exposure intact'
    assert native(provider.primary[3]) == native(sent[3]), 'new review advice affects the following request'
    assert not native(provider.primary[4]), 'complete exposed cohort must be removed together'
    before_reset = [b for b in provider.reviews if 'NEW_RESET_DIRECTION' not in json.dumps(b)]
    after_reset = [b for b in provider.reviews if 'NEW_RESET_DIRECTION' in json.dumps(b)]
    assert before_reset and after_reset
    assert any('DISCARD_NATIVE_BYTES' in json.dumps(b) for b in before_reset)
    assert any('DISCARD_NATIVE_BYTES' not in json.dumps(b) for b in before_reset[3:]), 'selected reviewer projection must omit the whole cohort'
    assert all('DISCARD_NATIVE_BYTES' not in json.dumps(b) for b in after_reset)
    assert state['compactions'] >= 1 and state['history_rebases'] == 1
    assert len(state['history']) == 2
    grade_projection(provider.reviews, provider.review_outputs)
    for body in provider.reviews:
        assert boundaries(body) == ([len(body['input']) - 1] if enabled else [])
    reset_index = provider.reviews.index(after_reset[0])
    assert provider.receipts[reset_index]['synthetic_cached_tokens'] == 0, 'divergent reset must start cold'


def fixture_environment():
    return {k: v for k, v in os.environ.items() if k not in
           ('OPENAI_API_KEY', 'CARRY_PROXY_UPSTREAM_KEY', 'CARRY_PROXY_CLASSIFIER_KEY', 'CARRY_PROXY_AUTH_TOKEN',
            'HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy')}


def reject_unknown(binary, directory):
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    command = [str(Path(binary).resolve()), 'proxy', '--listen', '127.0.0.1:0',
               '--upstream-url', 'http://127.0.0.1:1/v1/responses',
               '--classifier-cache-policy', 'openai-explicit', '--classifier-model', 'unknown-fixture-model',
               '--state-dir', str(directory / 'state')]
    process = subprocess.run(command, capture_output=True, text=True, timeout=10, env=fixture_environment())
    diagnostic = process.stdout + process.stderr
    (directory / 'process.log').write_text(diagnostic)
    assert process.returncode != 0 and 'openai-explicit classifier cache requires exact supported model' in diagnostic
    assert not (directory / 'state').exists(), 'unsupported explicit config must fail before state side effects'
    return {'case': 'explicit-unknown-rejected', 'basis': 'actual binary startup validation; no HTTP provider calls'}


def run(binary, directory, policy_arg='openai-explicit', enabled=True, model='gpt-6-luna', case='rotation'):
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    env = fixture_environment()
    # Cache protocol fixture, not a production-admission benchmark.
    env.update(CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS='0', CARRY_PROXY_REVIEW_MIN_NEW_TOKENS='0',
               CARRY_PROXY_REVIEW_BACKOFF_FACTOR='1', CARRY_PROXY_REVIEW_EVERY_REQUESTS='1')
    home = directory / 'home'; home.mkdir(exist_ok=True)
    env.update(HOME=str(home), CARRY_HOME=str(home / '.carry'), XDG_CONFIG_HOME=str(home / '.config'))
    inputs: list[dict] = []
    sent = []
    sent_hashes = []
    credit_snapshots = []
    with Provider(case) as provider:
        # Two actual OS processes share durable state; provider cache survives restart.
        # After the missing receipt and real cold write, repeat the COMPLETE
        # unchanged payload once to actually earn the early cached-read probe.
        # Keep every original append/turn; no reduced payload or clock change.
        first_epoch = [0, 1, 2, None, 3, 4] if case == 'missing-usage' else range(5)
        for epoch, turns in enumerate((first_epoch, range(5, 11))):
            with socket.socket() as sock:
                sock.bind(('127.0.0.1', 0))
                port = sock.getsockname()[1]
            cmd = [str(Path(binary).resolve()), 'proxy', '--login', 'api-key', '--listen', f'127.0.0.1:{port}',
                   '--upstream-url', provider.url + '/v1/responses', '--classifier-url', provider.url + '/classifier',
                   '--classifier-model', model, '--state-dir', str(directory / 'state'),
                   # Synthetic cache/replay budget, including the deliberately
                   # unlisted-model control; never depend on a model-role default.
                   '--classifier-max-input-tokens', '300000',
                   '--mode', 'compact' if case == 'coupled' else 'audit']
            if case == 'coupled':
                cmd += ['--payoff-requests', '1', '--min-payback-percent', '0']
            if policy_arg != 'none':
                cmd += ['--classifier-cache-policy', policy_arg]
            with (directory / f'process-{epoch}.log').open('wb') as log:
                process = subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT, env=env)
                try:
                    endpoint = f'http://127.0.0.1:{port}'
                    deadline = time.monotonic() + 10
                    while True:
                        if process.poll() is not None:
                            raise RuntimeError('binary exited before fixture: ' + (directory / f'process-{epoch}.log').read_text())
                        try:
                            urllib.request.urlopen(endpoint + '/health', timeout=1).close()
                            break
                        except OSError:
                            if time.monotonic() > deadline:
                                raise RuntimeError('binary readiness timeout')
                            time.sleep(0.05)
                    for turn in turns:
                        if case == 'coupled':
                            if turn == 0:
                                inputs.append({'role': 'user', 'content': 'UNIQUE_DIRECTION ' + 'keep ' * 600})
                            elif turn == 1:
                                inputs.append({'role': 'user', 'content': 'Continue with current direction.'})
                            elif turn == 2:
                                inputs.extend([
                                    {'type': 'reasoning', 'id': 'reasoning-native', 'summary': [],
                                     'encrypted_content': 'FIXTURE_ONLY_OPAQUE_REASONING_NOT_PROVIDER_STATE'},
                                    {'type': 'function_call', 'call_id': 'native-a', 'name': 'native',
                                     'arguments': '{ "input" : "é 🦀" }'},
                                    {'type': 'function_call', 'call_id': 'native-b', 'name': 'native',
                                     'arguments': '{ "input" : "b" }'},
                                    {'type': 'function_call_output', 'call_id': 'native-a',
                                     'output': '  DISCARD_NATIVE_BYTES_A\né 🦀\t' * 600},
                                    {'type': 'function_call_output', 'call_id': 'native-b',
                                     'output': '\tDISCARD_NATIVE_BYTES_B\r\n' * 600},
                                ])
                            elif turn == 8:
                                inputs = [{'role': 'user', 'content': 'NEW_RESET_DIRECTION ' + 'new ' * 600}]
                            elif turn == 9:
                                inputs.append({'role': 'user', 'content': 'Continue on the new branch.'})
                        elif turn is not None and turn < 9:
                            inputs.append({'role': 'user', 'content': f'FIXTURE_OBSERVATION_{turn} ' + 'stable data ' * 400})
                        body = {'model': 'gpt-6-luna', 'input': copy.deepcopy(inputs), 'store': False, 'stream': False,
                                'prompt_cache_key': 'primary-fixture-affinity'}
                        sent.append(body)
                        wire = json.dumps(body).encode()
                        sent_hashes.append(hashlib.sha256(wire).hexdigest())
                        headers = {'content-type': 'application/json', 'x-carry-session': 'reviewer-cache-fixture'}
                        if case == 'coupled' and turn >= 8:
                            headers['x-carry-history-policy'] = 'reset-on-divergence'
                        req = urllib.request.Request(endpoint + '/v1/responses', data=wire, headers=headers)
                        with urllib.request.urlopen(req, timeout=10) as response:
                            assert json.load(response)['status'] == 'completed'
                        # This cache/restart fixture sequences actual completed
                        # reviews. A consumed primary is not a review receipt:
                        # settle only in the driver, never in production admission.
                        deadline = time.monotonic() + 30
                        while True:
                            checkpoints = list((directory / 'state').glob('*.json'))
                            assert len(checkpoints) == 1
                            snapshot = json.loads(checkpoints[0].read_text())
                            if not snapshot['background_review_pending']:
                                break
                            if time.monotonic() > deadline:
                                raise AssertionError('reviewer cache receipt did not settle')
                            time.sleep(.01)
                        if turn == 0 and policy_arg in ('openai-explicit', 'disabled') and model == 'gpt-6-luna':
                            # Advertise only the synthetic simulator capability;
                            # cache receipts still come exclusively from HTTP.
                            reviewer = snapshot['model_resolution']['reviewer']
                            reviewer['cache'] = {'minimum_prefix_tokens': 1024, 'max_read_breakpoints': 50,
                                'max_write_breakpoints': 4, 'implicit_breakpoint_uses_write_slot': True}
                            reviewer['explicit_cache_transport'] = enabled
                            checkpoints[0].write_text(json.dumps(snapshot))
                        if case == 'missing-usage':
                            index = json.loads((directory / 'state' / 'history.index').read_text())
                            credit_snapshots.append({'turn': turn,
                                'read_credit': sum(e['estimated_tokens'] * e['native_cached_fraction']
                                                   for e in index['upstream_cache'])})
                finally:
                    process.terminate()
                    process.wait(timeout=5)
        evidence = {'basis': 'credential-free loopback provider cache simulator; synthetic usage, NOT live inference',
                    'binary_sha256': hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
                    'process_count': 2, 'reviews': provider.reviews, 'review_outputs': provider.review_outputs,
                    'receipts': provider.receipts,
                    'primary_requests': provider.primary, 'primary_raw_sha256': provider.primary_raw_hashes,
                    'credit_snapshots': credit_snapshots, 'errors': provider.errors}
        (directory / 'evidence.json').write_text(json.dumps(evidence, indent=2) + '\n')
        assert not provider.errors, provider.errors
        state_files = list((directory / 'state').glob('*.json'))
        assert len(state_files) == 1
        state = json.loads(state_files[0].read_text())
        if case == 'coupled':
            grade_coupled(provider, sent, state, enabled)
            assert provider.primary_raw_hashes[:3] == sent_hashes[:3], 'pre-trim native requests must remain byte-identical'
        else:
            assert provider.primary == sent, 'reviewer repair must leave primary bodies unchanged'
            assert provider.primary_raw_hashes == sent_hashes, 'audit primary raw JSON bytes must remain identical'
            grade(provider.reviews, provider.receipts, enabled, case == 'missing-usage',
                  outputs=provider.review_outputs)
        if case == 'missing-usage':
            assert sent[2] == sent[3] and sent_hashes[2] == sent_hashes[3], 'read probe must repeat the full unchanged native payload'
            assert provider.receipts[2]['synthetic_cached_tokens'] > 0, 'read-credit assertion requires an actual transmitted matching prefix and cached-read receipt'
            assert credit_snapshots[1]['read_credit'] == credit_snapshots[2]['read_credit'] == 0
            assert credit_snapshots[3]['read_credit'] > 0, 'only an explicit cached-read receipt may grant credit'
            assert state['shadow']['unavailable_cost_calls'] >= 1, 'missing usage must remain unknown in metering'
        records = json.loads((directory / 'state' / 'history.index').read_text())['upstream_cache']
        if policy_arg == 'auto':
            assert not records, 'unknown generic capability cannot confer cache evidence'
        else:
            assert records and any(e['native_cached_fraction'] > 0 for e in records)
        grade_cache_records(records, provider.reviews + provider.primary)
        return {'reviews': len(provider.reviews), 'processes': 2, 'case': case, 'basis': evidence['basis'],
                'classifier_cache_policy': policy_arg, 'classifier_model': model, 'explicit_enabled': enabled}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--carry', '--binary', dest='binary', required=True)
    parser.add_argument('--output', '--directory', dest='directory', required=True)
    parser.add_argument('--policy-arg', choices=('none', 'auto', 'disabled', 'openai-explicit'), default='openai-explicit')
    parser.add_argument('--expect-disabled', action='store_true')
    parser.add_argument('--model', default='gpt-6-luna')
    parser.add_argument('--case', choices=('all', 'rotation', 'coupled', 'missing-usage'), default='all')
    parser.add_argument('--legacy-red', action='store_true', help='omit new CLI flag but still require outbound stable markers')
    args = parser.parse_args()
    directory = Path(args.directory)
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    try:
        policy = 'none' if args.legacy_red else args.policy_arg
        enabled = not args.expect_disabled or args.legacy_red
        if args.case == 'all' and enabled and not args.legacy_red:
            cases = [run(args.binary, directory / name, policy, True, args.model, name)
                     for name in ('rotation', 'coupled', 'missing-usage')]
            for name, policy, model in [('disabled', 'disabled', args.model),
                                       ('auto-generic', 'auto', args.model),
                                       ('auto-unknown', 'auto', 'unknown-fixture-model')]:
                result = run(args.binary, directory / name, policy, False, model)
                result['case'] = name
                cases.append(result)
            for case in ('coupled', 'missing-usage'):
                name = 'implicit-' + case
                result = run(args.binary, directory / name, 'disabled', False, args.model, case)
                result['case'] = name
                cases.append(result)
            cases.append(reject_unknown(args.binary, directory / 'explicit-unknown-rejected'))
            result = {'cases': cases, 'case_count': len(cases), 'basis': 'loopback simulator only; not live provider evidence'}
        else:
            result = run(args.binary, args.directory, policy, enabled, args.model,
                         'rotation' if args.case == 'all' else args.case)
        result['status'] = 'PASS'
    except Exception as error:
        (directory / 'results.json').write_text(json.dumps({'status': 'FAIL', 'error': str(error),
            'basis': 'loopback simulator only; not live provider evidence'}, indent=2) + '\n')
        raise
    (directory / 'results.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result))


if __name__ == '__main__':
    main()
