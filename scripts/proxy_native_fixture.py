#!/usr/bin/env python3
"""Actual pinned clients + integrated binary against a scripted HTTP provider.

This is credential-free protocol evidence, never live model/quality/usage evidence.
Only CI installs the clients; this script never installs anything.
"""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import socket
import subprocess
import threading
import base64
import io
import http.client
import select
import socketserver
import time


class ClientObserver:
    """Synthetic-fixture ONLY: byte-transparent loopback TCP relay + HTTP capture.

    Complete grading evidence is separate from bounded production diagnostics.
    The relay never rebuilds requests/responses or buffers an SSE response.
    Captures contain synthetic auth/payloads; never use this on a live trial.
    """
    def __init__(self, target_port, output):
        self.records, self.errors = [], []
        self.output = Path(output)
        self.lock = threading.Lock()
        self.stopping = threading.Event()
        outer = self
        class Handler(socketserver.BaseRequestHandler):
            def handle(self):
                pending = b''
                try:
                    with socket.create_connection(('127.0.0.1', target_port), timeout=10) as upstream:
                        peers = [self.request, upstream]
                        for peer in peers:
                            peer.settimeout(.1)
                        while peers and not outer.stopping.is_set():
                            ready, _, _ = select.select(peers, [], [], .1)
                            for source in ready:
                                destination = upstream if source is self.request else self.request
                                data = source.recv(65536)
                                if not data:
                                    if source is self.request and pending:
                                        raise ValueError('incomplete observed HTTP request')
                                    peers.remove(source)
                                    destination.shutdown(socket.SHUT_WR)
                                    continue
                                if source is self.request:
                                    pending += data
                                    pending = outer.capture(pending)
                                # Poll shutdown even when an accepted client stops reading.
                                # Preserve the existing ten-second per-write upper bound.
                                remaining = memoryview(data)
                                deadline = time.monotonic() + 10
                                while remaining:
                                    if outer.stopping.is_set(): return
                                    if time.monotonic() >= deadline:
                                        raise TimeoutError('observed socket send timed out')
                                    try:
                                        sent = destination.send(remaining)
                                    except socket.timeout:
                                        continue
                                    if not sent:
                                        raise ConnectionError('observed socket closed during send')
                                    remaining = remaining[sent:]
                except (OSError, ValueError) as error:
                    with outer.lock:
                        outer.errors.append(str(error))
        self.server = socketserver.ThreadingTCPServer(('127.0.0.1', 0), Handler)
        self.port = self.server.server_address[1]
        self.url = f'http://127.0.0.1:{self.port}'

    def capture(self, pending):
        while b'\r\n\r\n' in pending:
            head, rest = pending.split(b'\r\n\r\n', 1)
            line, headers = head.split(b'\r\n', 1)
            method, path, _ = line.decode('ascii').split(' ')
            parsed = http.client.parse_headers(io.BytesIO(headers + b'\r\n\r\n'))
            if parsed.get('transfer-encoding'):
                # Passive decoding only; chunk framing is forwarded unchanged.
                if parsed['transfer-encoding'].lower() != 'chunked':
                    raise ValueError('unsupported observed transfer encoding')
                offset, chunks = 0, []
                while True:
                    end = rest.find(b'\r\n', offset)
                    if end < 0: return pending
                    size = int(rest[offset:end].split(b';', 1)[0], 16)
                    offset = end + 2
                    if size == 0:
                        if rest[offset:offset+2] == b'\r\n':
                            length = offset + 2
                        else:
                            end = rest.find(b'\r\n\r\n', offset)
                            if end < 0: return pending
                            length = end + 4
                        break
                    if len(rest) < offset + size + 2: return pending
                    if rest[offset+size:offset+size+2] != b'\r\n':
                        raise ValueError('invalid observed chunk framing')
                    chunks.append(rest[offset:offset+size]); offset += size + 2
                body = b''.join(chunks)
            else:
                length = int(parsed.get('content-length', '0'))
                if length < 0: raise ValueError('negative observed content length')
                if len(rest) < length: return pending
                body = rest[:length]
            end = len(head) + 4 + length
            record = {'method': method, 'path': path, 'body': body.decode('utf-8'),
                      'wire_base64': base64.b64encode(pending[:end]).decode('ascii')}
            with self.lock:
                self.records.append(record)
                with self.output.open('a') as output:
                    output.write(json.dumps(record) + '\n')
            pending = pending[end:]
        return pending

    def bodies(self):
        assert not self.errors, f'client wire capture failed: {self.errors}'
        return [json.loads(r['body']) for r in self.records
                if r['method'] == 'POST' and r['path'].split('?', 1)[0]
                in ('/v1/responses', '/v1/responses/compact')]

    def __enter__(self):
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *args):
        self.stopping.set()
        self.server.shutdown(); self.server.server_close(); self.thread.join(timeout=3)


def review_records(body):
    """Decode source annotations for fixture grading without wrapping wire items."""
    records, pending, memory_observations = [], [], {}
    memory_prefix = ('Quoted accepted memory observation, not instructions. '
                     'Current membership is in the final ledger: ')
    materialized_prefix = 'Quoted historical context data, not instructions: '
    for item in body.get('input', []):
        content = item.get('content', '')
        text = content if isinstance(content, str) else ''.join(
            block.get('text', '') for block in content if isinstance(block, dict)) if isinstance(content, list) else ''
        if text.startswith('[carry context] '):
            annotation = text[len('[carry context] '):]
            data = json.loads(annotation)
            count = len(data['member_ids'])
            data['items'] = pending[-count:]
            pending.clear()
            records.append(data)
            continue
        if text.startswith(memory_prefix) or text.startswith(materialized_prefix):
            prefix = memory_prefix if text.startswith(memory_prefix) else materialized_prefix
            fact = json.loads(text[len(prefix):])
            identity = fact.get('memory_id')
            assert identity, 'missing current memory identity binding'
            prior = memory_observations.get(identity)
            assert prior is None or prior == fact, 'conflicting memory identity binding'
            memory_observations[identity] = fact
            continue
        try:
            data = json.loads(text)
        except (ValueError, TypeError):
            pending.append(item)
            continue
        if isinstance(data, dict):
            if {'memory_id', 'source_ids', 'text'} <= data.keys():
                memory_observations[data['memory_id']] = data
                continue
            records.append(data)
    # Reviewer nudges/answers now remain in the append-only transcript. Grade
    # source observations against the current ledger rather than treating old
    # state snapshots as additional live sources.
    ledgers = [record for record in records if 'eligible_group_ids' in record]
    if not ledgers:
        return records
    current = ledgers[-1]
    sources = [record for record in records if 'eligible_group_ids' not in record
               and not {'protected', 'removable', 'memories'}.issubset(record)]
    for memory in current.get('current_memories', []):
        if 'text' in memory:  # Legacy full-ledger fixture compatibility.
            fact = {'source_ids': memory['source_ids'], 'text': memory['text']}
        elif memory['memory_id'] in memory_observations:
            observed = memory_observations[memory['memory_id']]
            assert observed['source_ids'] == memory['source_ids'], 'memory provenance mismatch'
            fact = {'source_ids': observed['source_ids'], 'text': observed['text']}
        else:
            # Provenance identifies evidence, not a fact: same-source siblings
            # require distinct explicit bindings for their current m handles.
            raise AssertionError('missing current memory observation or materialized fact')
        if fact not in sources:
            sources.append(fact)
    return sources + [current]


class Fixture:
    def __init__(self, client, case="off"):
        self.client = client
        self.case = case
        self.reviews = []
        self.compact_calls = []
        self.summary_calls = []
        self.calls = []
        self.responses = []
        self.errors = []
        self.state_directory = None
        outer = self
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass
            def do_GET(self):
                data = json.dumps({'object':'list', 'data':[{'id':'gpt-6-luna','object':'model'}]}).encode()
                self.send_response(200); self.send_header('content-type','application/json')
                self.send_header('content-length', str(len(data))); self.end_headers(); self.wfile.write(data)
            def do_POST(self):
                try:
                    body = json.loads(self.rfile.read(int(self.headers['content-length'])))
                    if self.path == '/classifier':
                        outer.reviews.append(body)
                        cache_key = body.get('prompt_cache_key')
                        if 'prompt_cache_key' in body and (not isinstance(cache_key, str) or len(cache_key) > 64):
                            code = 'string_above_max_length' if isinstance(cache_key, str) else 'invalid_type'
                            outer.errors.append(code)
                            raw = json.dumps({'error': {'type': 'invalid_request_error',
                                'code': code, 'param': 'prompt_cache_key',
                                'message': 'prompt_cache_key must be a string with at most 64 characters'}}).encode()
                            self.send_response(400); self.send_header('content-type', 'application/json')
                            self.send_header('content-length', str(len(raw))); self.end_headers()
                            self.wfile.write(raw)
                            return
                        # JSON mode checks message text, not instructions, metadata or format keys.
                        message_text = []
                        for record in body.get('input', []):
                            if record.get('type', 'message') != 'message':
                                continue
                            content = record.get('content', '')
                            if isinstance(content, str):
                                message_text.append(content)
                            elif isinstance(content, list):
                                message_text.extend(part['text'] for part in content
                                    if isinstance(part, dict) and isinstance(part.get('text'), str))
                        if (body.get('text', {}).get('format', {}).get('type') == 'json_object'
                                and not any('json' in text.lower() for text in message_text)):
                            outer.errors.append('json_instruction_missing')
                            raw = json.dumps({'error': {'type': 'invalid_request_error', 'param': 'input',
                                'message': "Response input messages must contain the word 'json' in some form to use 'text.format' of type 'json_object'."}}).encode()
                            self.send_response(400); self.send_header('content-type', 'application/json')
                            self.send_header('content-length', str(len(raw))); self.end_headers()
                            self.wfile.write(raw)
                            return
                        targets=set()
                        records=review_records(body)
                        eligible=set(next((record['eligible_group_ids'] for record in reversed(records) if 'eligible_group_ids' in record), []))
                        for data in records:
                            if data.get('group_id') in eligible and 'DROP_COHORT_PAYLOAD' in json.dumps(data.get('items', [])):
                                targets.add(data['group_id'])
                        value = {'id':'shadow-fixture','status':'completed','model':'gpt-6-luna',
                            'output':[{'type':'message','role':'assistant','content':[{'type':'output_text',
                            'text':json.dumps({'protected':[], 'removable':sorted(targets), 'memories':[]})}]}],
                            'usage':{'input_tokens':100,'output_tokens':10,
                            'input_tokens_details':{'cached_tokens':0,'cache_write_tokens':0}}}
                        if outer.case == 'fork':
                            advice = outer.fork_advice(body, targets, records, eligible)
                            value['output'][0]['content'][0]['text'] = json.dumps(advice)
                        raw=json.dumps(value).encode(); content_type='application/json'
                    else:
                        legacy=self.path == '/v1/responses/compact'
                        native_compact=legacy or any(i.get('type')=='compaction_trigger' for i in body.get('input',[]))
                        if self.path not in ('/v1/responses','/v1/responses/compact'):
                            raise ValueError('unsupported fixture route')
                        cache_key = body.get('prompt_cache_key')
                        summary=outer.case=='pi-checkpoint' and cache_key is None
                        coding=[c for c in outer.calls if c.get('prompt_cache_key')]
                        if not summary and (not isinstance(cache_key, str) or not cache_key or (coding and outer.case != 'fork' and
                                cache_key != coding[0].get('prompt_cache_key'))):
                            raise ValueError('native cache namespace missing or changed')
                        outer.calls.append(body)
                        if native_compact: outer.compact_calls.append(body)
                        if summary: outer.summary_calls.append(body)
                        count = len(outer.calls)-len(outer.compact_calls)-len(outer.summary_calls)
                        if len(outer.calls) > 8:
                            raise ValueError('unexpected client request loop')
                        name = 'exec_command' if outer.client == 'codex' else 'bash'
                        if summary:
                            output=[{'type':'message','id':'msg_summary_'+str(len(outer.summary_calls)),
                                'status':'completed','role':'assistant','content':[{'type':'output_text',
                                'text':'FIXTURE_PI_CHECKPOINT','annotations':[]}]}]
                        elif native_compact:
                            if outer.case not in ('v1','v2'): raise ValueError('unexpected native compaction')
                            output=[{'type':'compaction','id':'cmp_fixture',
                                'encrypted_content':'FIXTURE_ONLY_NOT_PROVIDER_STATE'}]
                        elif count == 1:
                            if not any(tool.get('name') == name for tool in body.get('tools', [])):
                                raise ValueError('native client tool schema missing')
                            arguments = ({'cmd':'printf FIXTURE_TOOL_OK > proxy-fixture.txt; cat proxy-fixture.txt',
                                          'yield_time_ms':1000,'max_output_tokens':128} if outer.client == 'codex'
                                         else {'command':'printf FIXTURE_TOOL_OK > proxy-fixture.txt; cat proxy-fixture.txt'})
                            if outer.case in ('compact','pi-checkpoint','fork'):
                                commands=['printf FIXTURE_TOOL_OK > proxy-fixture.txt; python3 -c '+
                                    '\"print(\'DROP_COHORT_PAYLOAD_A \' * 1400)\"',
                                    'printf FIXTURE_TOOL_OK_B > proxy-fixture-b.txt; python3 -c '+
                                    '\"print(\'DROP_COHORT_PAYLOAD_B \' * 1400)\"']
                                output=[]
                                for suffix, command in zip(('a','b'),commands):
                                    args=({'cmd':command,'yield_time_ms':1000,'max_output_tokens':15000}
                                          if outer.client == 'codex' else {'command':command})
                                    output.append({'type':'function_call','id':'fc_fixture_1_'+suffix,
                                        'status':'completed','name':name,'call_id':'call_fixture_1_'+suffix,
                                        'arguments':json.dumps(args)})
                            else:
                                output = [{'type':'function_call','id':'fc_fixture','status':'completed',
                                    'name':name,'call_id':'call_fixture','arguments':json.dumps(arguments)}]
                        else:
                            if not any(item.get('type') in ('function_call_output','compaction')
                                       for item in body.get('input', []) if isinstance(item,dict)) and not (
                                           outer.case=='pi-checkpoint' and 'FIXTURE_PI_CHECKPOINT' in json.dumps(body['input'])):
                                raise ValueError('native full-history tool-result echo missing')
                            if (outer.case == 'compact' and count < 5) or (outer.case == 'fork' and count == 2):
                                command='printf SMALL_COHORT_'+str(count)
                                if outer.state_directory is not None:
                                    # The primary has already completed. Sequence this
                                    # fixture's next native turn after durable reviewer
                                    # merge, never by waiting inside Carry forwarding.
                                    import shlex
                                    command += '; ' + shlex.join(['python3',
                                        str(Path(__file__).with_name('proxy_native_settle.py').resolve()),
                                        str(outer.state_directory), str(count)])
                                args=({'cmd':command,'yield_time_ms':1000,'max_output_tokens':128}
                                      if outer.client == 'codex' else {'command':command})
                                output=[{'type':'function_call','id':f'fc_fixture_{count}',
                                    'status':'completed','name':name,'call_id':f'call_fixture_{count}',
                                    'arguments':json.dumps(args)}]
                            else:
                                output = [{'type':'message','id':'msg_fixture','status':'completed','role':'assistant',
                                    'content':[{'type':'output_text','text':'FIXTURE_COMPLETE','annotations':[]}]}]
                        value = {'id':f'resp_fixture_{count}','object':'response','status':'completed',
                            'model':'gpt-6-luna','output':output,
                            'usage':{'input_tokens':1000,'output_tokens':10,'total_tokens':1010,
                            'input_tokens_details':{'cached_tokens':0,'cache_write_tokens':1000},
                            'output_tokens_details':{'reasoning_tokens':0}}}
                        if outer.case in ('v1','v2') and not native_compact and count==1:
                            value['usage']['input_tokens']=50000
                            value['usage']['total_tokens']=50010
                            value['usage']['input_tokens_details']['cache_write_tokens']=50000
                        outer.responses.append(value)
                        events = [{'type':'response.created','response':{**value,'output':[],'status':'in_progress'}}]
                        for index, item in enumerate(output):
                            initial = {**item,'status':'in_progress'}
                            if item['type'] == 'function_call':
                                initial['arguments']=''
                                events.extend([
                                    {'type':'response.output_item.added','output_index':index,'item':initial},
                                    {'type':'response.function_call_arguments.delta','output_index':index,
                                     'item_id':item['id'],'delta':item['arguments']},
                                    {'type':'response.function_call_arguments.done','output_index':index,
                                     'item_id':item['id'],'arguments':item['arguments']},
                                ])
                            elif item['type'] == 'message':
                                initial['content']=[]
                                part=item['content'][0]
                                events.extend([
                                    {'type':'response.output_item.added','output_index':index,'item':initial},
                                    {'type':'response.content_part.added','output_index':index,'content_index':0,
                                     'item_id':item['id'],'part':{**part,'text':''}},
                                    {'type':'response.output_text.delta','output_index':index,'content_index':0,
                                     'item_id':item['id'],'delta':part['text']},
                                    {'type':'response.output_text.done','output_index':index,'content_index':0,
                                     'item_id':item['id'],'text':part['text']},
                                    {'type':'response.content_part.done','output_index':index,'content_index':0,
                                     'item_id':item['id'],'part':part},
                                ])
                            else:
                                events.append({'type':'response.output_item.added','output_index':index,'item':initial})
                            events.append({'type':'response.output_item.done','output_index':index,'item':item})
                        events.append({'type':'response.completed','response':value})
                        raw = b''.join(('event: '+event['type']+'\ndata: '+json.dumps(event)+'\n\n').encode() for event in events)
                        content_type='text/event-stream'
                        if legacy:
                            raw=json.dumps({'output':output,'usage':value['usage']}).encode()
                            content_type='application/json'
                    self.send_response(200); self.send_header('content-type',content_type)
                    self.send_header('content-length',str(len(raw))); self.end_headers()
                    # Deliberately fragment UTF-8/JSON/SSE independently of lines.
                    for offset in range(0, len(raw), 31):
                        self.wfile.write(raw[offset:offset+31]); self.wfile.flush()
                except (ValueError, KeyError) as error:
                    outer.errors.append(type(error).__name__)
                    self.send_response(400); self.end_headers()
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1',0), Handler)
        self.url='http://127.0.0.1:'+str(self.server.server_port)
    def __enter__(self):
        self.thread=threading.Thread(target=self.server.serve_forever,daemon=True); self.thread.start()
        return self
    def __exit__(self,*args):
        self.server.shutdown(); self.server.server_close(); self.thread.join(timeout=3)


def compact_evidence(trial, fixture, *, received):
    """Grade complete fixture-owned inbound/outbound wire and durable paired state."""
    files=list((trial / 'state').glob('*.json'))
    if len(files) != 1: raise AssertionError('exactly one owned session required')
    state=json.loads(files[0].read_text())
    submitted=fixture.calls
    cohort={'call_fixture_1_a','call_fixture_1_b'}
    def members(body):
        return [i for i in body['input'] if i.get('call_id') in cohort]
    assert len(received)==len(submitted)==5, 'five actual native coding turns required'
    assert len(members(received[2]))==4, 'caller must echo both calls and both outputs'
    assert len(members(submitted[1]))==4, 'cohort must receive completed exposure before removal'
    assert len(members(submitted[2]))==4, 'advice emitted this turn applies to the next request'
    assert not members(submitted[3]) and not members(submitted[4]), 'atomic cohort must stay removed'
    assert len(fixture.reviews)>=2, 'later shadow request required, not a source-only check'
    cache_keys=[r.get('prompt_cache_key') for r in fixture.reviews]
    assert all(isinstance(k,str) and 0 < len(k) <= 64 for k in cache_keys), 'classifier cache key exceeds provider limit'
    assert len(set(cache_keys))==1, 'classifier cache affinity must stay stable within a session'
    assert 'DROP_COHORT_PAYLOAD' in json.dumps(fixture.reviews[0])
    assert 'DROP_COHORT_PAYLOAD' not in json.dumps(fixture.reviews[-1]), 'later shadow must mechanically prune'
    observation_counts=[]
    tail_eligible=[]
    cohort_group=None
    for review in fixture.reviews:
        records=review_records(review)
        blocks=[r for r in records if 'items' in r]
        ids=[member for block in blocks for member in block['member_ids']]
        assert len(ids)==len(set(ids)), 'every immutable main member must occur only once per shadow request'
        assert all('eligible' not in b for b in blocks), 'eligibility must not be frozen inside immutable observations'
        observation_counts.append(len(ids))
        tail_eligible.append(records[-1]['eligible_group_ids'])
        for block in blocks:
            if any(i.get('call_id') in cohort for i in block['items']):
                cohort_group=block['group_id']
    assert cohort_group is not None
    assert cohort_group not in tail_eligible[0] and cohort_group in tail_eligible[1], 'tail must reflect newly completed cohort exposure'
    bulk_mentions=[json.dumps(r).count('DROP_COHORT_PAYLOAD') for r in fixture.reviews]
    assert bulk_mentions[0]==bulk_mentions[1] and bulk_mentions[-1]==0, 'eligibility transition must not double source payload'
    removed=[i for i in state['history'] if i['removed'] and i['value'].get('call_id') in cohort]
    assert len(removed)==4 and len({i['cohort'] for i in removed if i['value']['type']=='function_call'})==1
    removed_ids={i['id'] for i in removed}
    assert not any(r['source_id'] in removed_ids for r in state['active_shadow'])
    assert state['invalid_reviews']==0 and state['compactions']>=1
    assert (trial.parent/(trial.name.removesuffix('-trial')+'-workspace')/'proxy-fixture-b.txt').read_text()=='FIXTURE_TOOL_OK_B'
    # Forwarded surviving tool outputs must be the caller's unmodified bytes.
    for before,after in zip(received,submitted):
        originals={i['call_id']:i for i in before['input'] if i.get('type')=='function_call_output'}
        for item in after['input']:
            if item.get('type')=='function_call_output': assert item==originals[item['call_id']]
    return {'compact_verified':True,'removed_atomic_members':len(removed),
            'classifier_cache_key_characters':len(cache_keys[0]),
            'classifier_cache_namespace_sha256':hashlib.sha256(cache_keys[0].encode()).hexdigest(),
            'classifier_cache_affinity_verified':True,
            'classifier_calls':len(fixture.reviews),'invalid_reviews':state['invalid_reviews'],
            'coupled_later_shadow_pruning_verified':True,'tool_output_bytes_preserved':True,
            'immutable_member_observation_verified':True,'current_tail_eligibility_verified':True,
            'bulk_mentions_per_review':bulk_mentions,'observed_members_per_review':observation_counts,
            'shadow_input_bytes_per_review':[len(json.dumps(r['input']).encode()) for r in fixture.reviews],
            'proxy_compactions':state['compactions']}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--carry', required=True)
    parser.add_argument('--codex')
    parser.add_argument('--pi')
    parser.add_argument('--client', choices=('codex','pi'), help='Run only one installed pinned client')
    parser.add_argument('--case', choices=('fork',), help='Run only the isolated Pi fork tracer')
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    for client in ([args.client] if args.client else ['codex','pi']):
        if not getattr(args,client): parser.error('--'+client+' is required for the selected cases')
    if args.output.exists():
        parser.error('fixture output must be fresh')
    args.output.mkdir(parents=True)
    results=[]
    if args.case and args.client != 'pi': parser.error('--case fork requires --client pi')
    for client,case in [('codex','off'),('codex','compact'),('pi','off'),('pi','compact'),('codex','v1'),('codex','v2'),('pi','pi-checkpoint-strict'),('pi','pi-checkpoint'),('pi','pi-checkpoint-off')]:
        if args.case: continue
        if args.client and client != args.client: continue
        label=client+'-'+case
        workspace=args.output / (label+'-workspace'); workspace.mkdir()
        subprocess.run(['git','init','-q',str(workspace)],check=True)
        env={k:v for k,v in os.environ.items() if not k.startswith(('OPENAI_','CARRY_PROXY_'))}
        env.update(CARRY_PROXY_UPSTREAM_KEY='synthetic-fixture-key', CARRY_PROXY_CLASSIFIER_KEY='synthetic-fixture-key')
        # Force the legacy cadence for the tiny native-client protocol fixture.
        env.update(CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS='0', CARRY_PROXY_REVIEW_MIN_NEW_TOKENS='0',
               CARRY_PROXY_REVIEW_BACKOFF_FACTOR='1', CARRY_PROXY_REVIEW_EVERY_REQUESTS='1')
        home=args.output/(label+'-home'); home.mkdir()
        env.update(HOME=str(home), CARRY_HOME=str(home/'.carry'), XDG_CONFIG_HOME=str(home/'.config'))
        with Fixture(client,'pi-checkpoint' if case.startswith('pi-checkpoint') else case) as fixture, socket.socket() as sock:
            sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]; sock.close()
            trial=args.output / (label+'-trial')
            fixture.state_directory = (trial / 'state').resolve()
            command=['python3',str(Path(__file__).with_name('proxy_trial.py')),
                '--client',client,'--carry-binary',args.carry,'--client-binary',getattr(args,client),
                '--workspace',str(workspace),'--trial-dir',str(trial),'--mode','off' if case in ('off','pi-checkpoint-off') else 'compact',
                '--codex-sandbox','danger-full-access',
                '--codex-native-compaction',case if case in ('v1','v2') else 'disabled',
                '--payoff-requests','5','--min-payback-percent','0',
                '--listen',f'127.0.0.1:{port}','--upstream-url',fixture.url+'/v1/responses',
                '--classifier-url',fixture.url+'/classifier','--timeout','60',
                '--prompt','Run the supplied tool once and finish. This is a scripted offline fixture.']
            if case.startswith('pi-checkpoint'):
                command+=['--pi-checkpoint-fixture','--history-policy',
                    'reset-on-divergence' if case=='pi-checkpoint' else 'strict']
            with ClientObserver(port, args.output/(label+'-client-wire.jsonl')) as observer:
                command += ['--fixture-client-base-url', observer.url+'/v1']
                run=subprocess.run(command,env=env,text=True,capture_output=True,timeout=85)
            received=observer.bodies()
            (args.output/(label+'-provider-wire.json')).write_text(json.dumps({
                'fixture_only':True, 'calls':fixture.calls, 'reviews':fixture.reviews})+'\n')
            if not run.returncode:
                assert len(received)==len(fixture.calls), 'each executed native request must have complete wire evidence'
            trace=(trial / 'client-events.jsonl').read_text() if (trial / 'client-events.jsonl').exists() else ''
            marker=workspace / 'proxy-fixture.txt'
            cache_key=fixture.calls[0].get('prompt_cache_key') if fixture.calls else None
            result={'client':client,'case':case,'fixture_only':True,'exit_code':run.returncode,
                'native_cache_namespace_sha256':hashlib.sha256(cache_key.encode()).hexdigest() if cache_key else None,
                'native_requests':len(fixture.calls),'native_tool_result_echo':len(fixture.calls)>1,
                'tool_effect_verified':marker.is_file() and marker.read_text()=='FIXTURE_TOOL_OK',
                'native_final_verified':'FIXTURE_COMPLETE' in trace,'provider_errors':fixture.errors}
            if case.startswith('pi-checkpoint'):
                rpc=[json.loads(line) for line in trace.splitlines() if line.startswith('{')]
                compact=[e for e in rpc if e.get('type')=='response' and e.get('id')=='c1']
                if case=='pi-checkpoint-strict':
                    result['explicit_strict_rejection_verified']=bool(result['tool_effect_verified'] and not fixture.errors and len(fixture.calls)==2 and run.returncode and compact and compact[0].get('success') is False and '409' in json.dumps(compact))
                    if result['explicit_strict_rejection_verified']:
                        results.append(result)
                        (args.output/'results.json').write_text(json.dumps(results,indent=2)+'\n')
                        continue
                elif not run.returncode:
                    states=list((trial/'state').glob('*.json'))
                    state=json.loads(states[0].read_text())
                    submitted=fixture.calls
                    result['pi_checkpoint_verified']=bool(len(received)==len(submitted) and received and len(fixture.calls)-len(fixture.summary_calls)==3 and fixture.calls[-1].get('prompt_cache_key') and fixture.summary_calls and compact and compact[0].get('success') is True and
                        'FIXTURE_PI_CHECKPOINT' in json.dumps(fixture.calls[-1]['input']) and
                        received[-1]==submitted[-1] and state['history_rebases']>=1 and
                        not state['active_shadow'] and sum(e.get('type')=='agent_settled' for e in rpc)>=2)
                    result['pi_summary_calls']=len(fixture.summary_calls)
                    result['history_rebases']=state['history_rebases']
                    if not result['pi_checkpoint_verified']: result['acceptance_failure']='Pi checkpoint reset/continuation failed'
            if case in ('v1','v2') and not run.returncode:
                states=[json.loads(p.read_text()) for p in (trial/'state').glob('*.json')]
                result['native_checkpoint_verified']=(len(fixture.compact_calls)==1 and
                    states[0]['native_compactions']==1 and
                    any(i.get('type')=='compaction' for i in fixture.calls[-1]['input']) and
                    not any(i.get('type')=='function_call' for i in fixture.calls[-1]['input']))
                if not result['native_checkpoint_verified']: result['acceptance_failure']='native checkpoint continuation failed'
            if case == 'compact' and not run.returncode:
                try: result.update(compact_evidence(trial,fixture,received=received))
                except AssertionError as error: result['acceptance_failure']=str(error)
            results.append(result)
            (args.output / 'results.json').write_text(json.dumps(results,indent=2)+'\n')
            if result.get('acceptance_failure') or run.returncode or fixture.errors or not result['tool_effect_verified'] or not result['native_final_verified'] or not result['native_tool_result_echo']:
                raise RuntimeError(f'{client} integrated fixture failed; inspect isolated fixture artifacts')
    if len({r['native_cache_namespace_sha256'] for r in results}) != len(results):
        raise RuntimeError('fresh native clients reused a cache namespace')
    compact=[r for r in results if r['case']=='compact']
    if len({r['classifier_cache_namespace_sha256'] for r in compact}) != len(compact):
        raise RuntimeError('fresh native clients reused a classifier cache namespace')
    if args.client in (None, 'pi'):
        from proxy_pi_fork_fixture import run_case
        results.append(run_case(carry=args.carry, pi=args.pi, output=args.output/'pi-fork',
            fixture_class=Fixture, observer_class=ClientObserver))
        (args.output/'results.json').write_text(json.dumps(results,indent=2)+'\n')
    print(json.dumps(results,indent=2))


if __name__ == '__main__':
    main()
