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


def text(item):
    content = item.get('content', '')
    if isinstance(content, str):
        return content
    assert len(content) == 1 and content[0]['type'] == 'input_text'
    return content[0]['text']


def canonical(items):
    return [{'role': item['role'], 'content': text(item)} for item in items]


def prefix_identity(body, end):
    # Exact renderer JSON identity: preserve wrappers, roles, text and other
    # fields; remove ONLY the owned content-block marker, not quoted source.
    items = copy.deepcopy(body['input'][:end])
    for item in items:
        for block in item['content']:
            marker = block.pop('prompt_cache_breakpoint', None)
            assert marker is None or marker == {'mode': 'explicit'}
    wire = json.dumps(items, separators=(',', ':'), ensure_ascii=False).encode()
    return hashlib.sha256(wire).hexdigest()


def grade_cache_records(records, body):
    assert records and len(records) <= 8
    assert len(json.dumps(records, separators=(',', ':'), ensure_ascii=False).encode()) <= 64 * 1024
    stable_end = len(body['input']) - 1
    settings = {key: value for key, value in body.items() if key != 'input'}
    for record in records:
        assert set(record) == {'format_version', 'base', 'input_len', 'input_sha256', 'at',
                               'read_estimated_tokens', 'read_confirmed_at'}
        assert record['format_version'] == 1
        assert 0 < record['input_len'] <= stable_end, 'cache evidence includes ledger or out-of-bounds input'
        assert record['input_sha256'] == prefix_identity(body, record['input_len']), 'cache prefix identity mismatch'
        assert record['base']['settings'] == settings, 'cache non-input settings mismatch'


def boundaries(body):
    return [index + 1 for index, item in enumerate(body['input'])
            if isinstance(item.get('content'), list)
            and any('prompt_cache_breakpoint' in block for block in item['content'])]


class Provider:
    def __init__(self, case='rotation'):
        self.case = case
        self.reviews = []
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
                        key = lambda n: json.dumps([base, data[:n]], sort_keys=True)
                        matched = [n for n in points if key(n) in owner.cache]
                        # Explicit simulator: bounded synthetic native read, never tokenization.
                        cached = 2048 if matched else 0
                        if owner.case == 'missing-usage' and len(owner.reviews) <= 2:
                            cached = 0
                        for point in points:
                            owner.cache.add(key(point))
                        owner.receipts.append({'synthetic_cached_tokens': cached,
                                               'matched_boundaries': matched,
                                               'transmitted_boundaries': points})
                        ids = ledger['eligible_group_ids']
                        targets = [json.loads(item['content'])['group_id'] for item in data[:-1]
                                   if 'DISCARD_NATIVE_BYTES' in item['content']
                                   and json.loads(item['content']).get('group_id') in ids]
                        # Omission preserves Rust's existing opinion. Audit
                        # cases explicitly replace the same eligible ID so the
                        # final two no-append reviews really change the ledger.
                        advice = {'protected': ids[:1] if owner.case == 'coupled' or len(owner.reviews) % 2 else [],
                                  'removable': sorted(set(targets)) if owner.case == 'coupled' else
                                               ([] if len(owner.reviews) % 2 else ids[:1]), 'memories': []}
                        output = [{'type': 'message', 'role': 'assistant',
                                   'content': [{'type': 'output_text', 'text': json.dumps(advice)}]}]
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


def grade(reviews, receipts, enabled=True, missing_usage=False):
    assert len(reviews) >= 8, 'at least eight actual reviews required'
    if not enabled:
        assert all(not boundaries(b) and 'prompt_cache_options' not in b for b in reviews)
        return
    for body in reviews:
        points = boundaries(body)
        assert points, 'missing outbound stable reviewer cache breakpoint'
        assert body['prompt_cache_options'] == {'mode': 'explicit'}
        assert max(points) == len(body['input']) - 1, 'must WRITE stable observations, not ledger'
        assert len(points) <= 4
        assert json.loads(text(body['input'][-1]))['current_groups']
    for index, (previous, current, receipt) in enumerate(zip(reviews, reviews[1:], receipts[1:])):
        previous_end = max(boundaries(previous))
        assert canonical(current['input'][:previous_end]) == canonical(previous['input'][:previous_end])
        assert previous_end in boundaries(current), 'latest prior WRITE must remain a lookup candidate'
        if not missing_usage or index > 0:
            assert receipt['synthetic_cached_tokens'] > 0, 'simulator failed to reuse preserved stable boundary'
    assert canonical(reviews[-1]['input'][:-1]) == canonical(reviews[-2]['input'][:-1]), 'ledger-only review required'
    assert text(reviews[-1]['input'][-1]) != text(reviews[-2]['input'][-1]), 'ledger really must change'


def grade_coupled(provider, sent, state, enabled):
    assert len(provider.primary) == len(sent) == 11 and len(provider.reviews) >= 8
    def native(body):
        return [i for i in body['input'] if i.get('call_id') in ('native-a', 'native-b') or i.get('type') == 'reasoning']
    assert len(native(sent[2])) == 5, 'actual native parallel cohort must be supplied'
    assert native(provider.primary[2]) == native(sent[2]), 'new native bytes must first receive exposure intact'
    assert not native(provider.primary[3]), 'complete exposed cohort must be removed together'
    assert not any(native(b) for b in provider.primary[3:]), 'removed native source must never resurrect'
    before_reset = [b for b in provider.reviews if 'NEW_RESET_DIRECTION' not in json.dumps(b)]
    after_reset = [b for b in provider.reviews if 'NEW_RESET_DIRECTION' in json.dumps(b)]
    assert before_reset and after_reset
    assert any('DISCARD_NATIVE_BYTES' in json.dumps(b) for b in before_reset[:2])
    assert all('DISCARD_NATIVE_BYTES' not in json.dumps(b) for b in before_reset[3:]), 'paired shadow source must be pruned'
    assert all('DISCARD_NATIVE_BYTES' not in json.dumps(b) for b in after_reset)
    assert state['compactions'] >= 1 and state['history_rebases'] == 1
    assert len(state['active_shadow']) == 2 and len(state['history']) == 2
    if enabled:
        assert all(0 < len(boundaries(b)) <= 4 and max(boundaries(b)) == len(b['input']) - 1 for b in provider.reviews)
        # A compatible smaller pre-cohort generation survives the shared trim.
        assert provider.receipts[3]['synthetic_cached_tokens'] > 0, 'post-prune stable prefix must remain a lookup candidate'
        reset_index = provider.reviews.index(after_reset[0])
        assert provider.receipts[reset_index]['synthetic_cached_tokens'] == 0, 'divergent reset must start cold'
        # Hash/length matching the actual post-reset render proves no stale
        # pre-reset prefix evidence survives, without storing source copies.
        grade_cache_records(state['reviewer_cache'], after_reset[-1])


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
    inputs: list[dict] = []
    sent = []
    sent_hashes = []
    credit_snapshots = []
    with Provider(case) as provider:
        # Two actual OS processes share durable state; provider cache survives restart.
        for epoch, turns in enumerate((range(5), range(5, 11))):
            with socket.socket() as sock:
                sock.bind(('127.0.0.1', 0))
                port = sock.getsockname()[1]
            cmd = [str(Path(binary).resolve()), 'proxy', '--listen', f'127.0.0.1:{port}',
                   '--upstream-url', provider.url + '/v1/responses', '--classifier-url', provider.url + '/classifier',
                   '--classifier-model', model, '--state-dir', str(directory / 'state'),
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
                        elif turn < 9:
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
                        if case == 'missing-usage':
                            checkpoints = list((directory / 'state').glob('*.json'))
                            assert len(checkpoints) == 1
                            snapshot = json.loads(checkpoints[0].read_text())
                            credit_snapshots.append({'turn': turn,
                                'read_credit': sum(e['read_estimated_tokens'] for e in snapshot.get('reviewer_cache', []))})
                finally:
                    process.terminate()
                    process.wait(timeout=5)
        evidence = {'basis': 'credential-free loopback provider cache simulator; synthetic usage, NOT live inference',
                    'binary_sha256': hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
                    'process_count': 2, 'reviews': provider.reviews, 'receipts': provider.receipts,
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
            grade(provider.reviews, provider.receipts, enabled, case == 'missing-usage')
        if case == 'missing-usage':
            assert credit_snapshots[1]['read_credit'] == credit_snapshots[2]['read_credit'] == 0
            assert credit_snapshots[3]['read_credit'] > 0, 'only an explicit cached-read receipt may grant credit'
            assert state['shadow']['unavailable_cost_calls'] >= 1, 'missing usage must remain unknown in metering'
        if enabled:
            records = state['reviewer_cache']
            assert records and any(e['read_estimated_tokens'] > 0 for e in records)
            assert sum(e['read_confirmed_at'] is not None for e in records) == 1, 'aggregate read must credit only one boundary'
            latest = provider.reviews[-1]
            total_estimate = len(json.dumps(latest, separators=(',', ':'), ensure_ascii=False).encode()) / 4
            assert sum(e['read_estimated_tokens'] for e in records) <= total_estimate * 2048 / 10000 + 1e-9
            grade_cache_records(records, latest)
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
