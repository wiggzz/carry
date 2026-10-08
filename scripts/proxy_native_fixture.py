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


class Fixture:
    def __init__(self, client, case="off"):
        self.client = client
        self.case = case
        self.reviews = []
        self.compact_calls = []
        self.summary_calls = []
        self.calls = []
        self.errors = []
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
                        for record in body.get('input', []):
                            try: data=json.loads(record.get('content', ''))
                            except (ValueError, TypeError): continue
                            if data.get('eligible') and 'DROP_COHORT_PAYLOAD' in json.dumps(data.get('items', [])):
                                targets.add(data['group_id'])
                        value = {'id':'shadow-fixture','status':'completed','model':'gpt-6-luna',
                            'output':[{'type':'message','role':'assistant','content':[{'type':'output_text',
                            'text':json.dumps({'protected':[], 'removable':sorted(targets), 'memories':[]})}]}],
                            'usage':{'input_tokens':100,'output_tokens':10,
                            'input_tokens_details':{'cached_tokens':0,'cache_write_tokens':0}}}
                        raw=json.dumps(value).encode(); content_type='application/json'
                    else:
                        legacy=self.path == '/v1/responses/compact'
                        native_compact=legacy or any(i.get('type')=='compaction_trigger' for i in body.get('input',[]))
                        if self.path not in ('/v1/responses','/v1/responses/compact'):
                            raise ValueError('unsupported fixture route')
                        cache_key = body.get('prompt_cache_key')
                        summary=outer.case=='pi-checkpoint' and cache_key is None
                        coding=[c for c in outer.calls if c.get('prompt_cache_key')]
                        if not summary and (not isinstance(cache_key, str) or not cache_key or (coding and
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
                            if outer.case in ('compact','pi-checkpoint'):
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
                            if outer.case == 'compact' and count < 4:
                                command='printf SMALL_COHORT_'+str(count)
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


def compact_evidence(trial, fixture):
    """Grade the executed native caller, wire projection and durable paired state."""
    files=list((trial / 'state').glob('*.json'))
    if len(files) != 1: raise AssertionError('exactly one owned session required')
    state=json.loads(files[0].read_text())
    events=[json.loads(line) for line in files[0].with_suffix('.jsonl').read_text().splitlines()]
    received=[e['data'] for e in events if e['event']=='primary_received']
    submitted=[e['data'] for e in events if e['event']=='primary_submitted']
    cohort={'call_fixture_1_a','call_fixture_1_b'}
    def members(body):
        return [i for i in body['input'] if i.get('call_id') in cohort]
    assert len(received)==len(submitted)==4, 'four actual native coding turns required'
    assert len(members(received[2]))==4, 'caller must echo both calls and both outputs'
    assert len(members(submitted[1]))==4, 'cohort must receive completed exposure before removal'
    assert not members(submitted[2]) and not members(submitted[3]), 'atomic cohort must stay removed'
    assert len(fixture.reviews)>=2, 'later shadow request required, not a source-only check'
    cache_keys=[r.get('prompt_cache_key') for r in fixture.reviews]
    assert all(isinstance(k,str) and 0 < len(k) <= 64 for k in cache_keys), 'classifier cache key exceeds provider limit'
    assert len(set(cache_keys))==1, 'classifier cache affinity must stay stable within a session'
    assert 'DROP_COHORT_PAYLOAD' in json.dumps(fixture.reviews[0])
    assert 'DROP_COHORT_PAYLOAD' not in json.dumps(fixture.reviews[-1]), 'later shadow must mechanically prune'
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
            'proxy_compactions':state['compactions']}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--carry', required=True)
    parser.add_argument('--codex', required=True)
    parser.add_argument('--pi', required=True)
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    if args.output.exists():
        parser.error('fixture output must be fresh')
    args.output.mkdir(parents=True)
    results=[]
    for client,case in [('codex','off'),('codex','compact'),('pi','off'),('pi','compact'),('codex','v1'),('codex','v2'),('pi','pi-checkpoint-strict'),('pi','pi-checkpoint'),('pi','pi-checkpoint-off')]:
        label=client+'-'+case
        workspace=args.output / (label+'-workspace'); workspace.mkdir()
        subprocess.run(['git','init','-q',str(workspace)],check=True)
        env={k:v for k,v in os.environ.items() if not k.startswith(('OPENAI_','CARRY_PROXY_'))}
        env.update(CARRY_PROXY_UPSTREAM_KEY='synthetic-fixture-key', CARRY_PROXY_CLASSIFIER_KEY='synthetic-fixture-key')
        with Fixture(client,'pi-checkpoint' if case.startswith('pi-checkpoint') else case) as fixture, socket.socket() as sock:
            sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]; sock.close()
            trial=args.output / (label+'-trial')
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
            run=subprocess.run(command,env=env,text=True,capture_output=True,timeout=85)
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
                    events=[json.loads(line) for line in states[0].with_suffix('.jsonl').read_text().splitlines()]
                    received=[e['data'] for e in events if e['event']=='primary_received']
                    submitted=[e['data'] for e in events if e['event']=='primary_submitted']
                    result['pi_checkpoint_verified']=bool(len(fixture.calls)-len(fixture.summary_calls)==3 and fixture.calls[-1].get('prompt_cache_key') and fixture.summary_calls and compact and compact[0].get('success') is True and
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
                try: result.update(compact_evidence(trial,fixture))
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
    print(json.dumps(results,indent=2))


if __name__ == '__main__':
    main()
