#!/usr/bin/env python3
"""Owned fake native executable for hosted synthetic SCB integration ONLY."""
import http.client
import json
import os
from pathlib import Path
import socket
import sys
import uuid

if '--version' in sys.argv:
    print('codex-cli 0.0.0-synthetic')
    raise SystemExit(0)
assert sys.argv[1] == 'exec'
resume = sys.argv[2:4] == ['resume', '--last']
prompt = sys.argv[4] if resume else sys.argv[2]
home = Path(os.environ['HOME'])
assert not os.environ['OPENAI_API_KEY'].startswith('synthetic-provider-')
assert not any(name in os.environ for name in ('CARRY_PROXY_UPSTREAM_KEY', 'BENCHMARK_PROVIDER_KEY', 'AWS_SECRET_ACCESS_KEY'))
assert not Path('/var/run/docker.sock').exists()
assert not Path('/proxy-state').exists()
assert not Path('tests').exists(), 'private tests must not reach the solving workspace'
assert not Path('solutions').exists(), 'gold must not reach the solving workspace'
try:
    socket.create_connection(('1.1.1.1', 443), timeout=0.2).close()
except OSError:
    pass
else:
    raise AssertionError('agent escaped its internal network')
state = home/'.codex'/'synthetic-conversation.json'
if resume:
    prior = json.loads(state.read_text())
    checkpoint, conversation, history = prior['checkpoint'], prior['conversation'], prior['history']
else:
    checkpoint = 2 if 'checkpoint_2' in prompt else 1
    if checkpoint == 2:
        assert Path('main.py').exists(), 'workspace did not survive the spec update'
        assert (home/'persistent-marker').exists(), 'HOME did not survive the spec update'
    conversation, history = uuid.uuid4().hex, []
(home/'persistent-marker').write_text('synthetic')
history.append({'role': 'user', 'content': [{'type': 'input_text', 'text': prompt}]})
body = {'model': 'gpt-6-luna', 'input': history, 'stream': False, 'store': False}
connection = http.client.HTTPConnection('openai-proxy', 8080, timeout=15)
connection.request('POST', '/v1/responses', json.dumps(body),
                   {'Authorization': 'Bearer '+os.environ['OPENAI_API_KEY'], 'Content-Type': 'application/json'})
response = connection.getresponse()
raw = response.read()
assert response.status == 200, (response.status, raw)
payload = json.loads(raw)
history.extend(payload['output'])
state.write_text(json.dumps({'checkpoint': checkpoint, 'conversation': conversation, 'history': history}))
with Path('synthetic-lifecycle.jsonl').open('a') as log:
    log.write(json.dumps({'checkpoint': checkpoint, 'resume': resume, 'conversation': conversation,
                          'pid': os.getpid(), 'home': str(home), 'provider_receipt': payload['id']})+'\n')
# A charged initial failure MUST remain in trusted usage; only the retry succeeds.
if checkpoint == 1 and not resume:
    print('synthetic native failure after a provider receipt', file=sys.stderr)
    raise SystemExit(1)
Path('main.py').write_text(f'print({checkpoint})\n')
print(json.dumps({'type': 'item.completed', 'item': {'type': 'agent_message', 'text': 'synthetic fixture'}}))
print(json.dumps({'type': 'turn.completed', 'usage': {'input_tokens': 30, 'cached_input_tokens': 0, 'output_tokens': 3}}))
