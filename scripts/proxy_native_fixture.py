#!/usr/bin/env python3
"""Actual pinned clients + integrated binary against a scripted HTTP provider.

This is credential-free protocol evidence, never live model/quality/usage evidence.
Only CI installs the clients; this script never installs anything.
"""
import argparse
import http.server
import json
import os
from pathlib import Path
import socket
import subprocess
import threading


class Fixture:
    def __init__(self, client):
        self.client = client
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
                        value = {'id':'shadow-fixture','status':'completed','model':'gpt-6-luna',
                            'output':[{'type':'message','role':'assistant','content':[{'type':'output_text',
                            'text':json.dumps({'protected':[], 'removable':[], 'memories':[]})}]}],
                            'usage':{'input_tokens':100,'output_tokens':10,
                            'input_tokens_details':{'cached_tokens':0,'cache_write_tokens':0}}}
                        raw=json.dumps(value).encode(); content_type='application/json'
                    else:
                        if self.path != '/v1/responses':
                            raise ValueError('unsupported fixture route')
                        cache_key = body.get('prompt_cache_key')
                        if not isinstance(cache_key, str) or not cache_key or (outer.calls and
                                cache_key != outer.calls[0].get('prompt_cache_key')):
                            raise ValueError('native cache namespace missing or changed')
                        outer.calls.append(body)
                        count = len(outer.calls)
                        if count > 4:
                            raise ValueError('unexpected client request loop')
                        name = 'exec_command' if outer.client == 'codex' else 'bash'
                        if count == 1:
                            if not any(tool.get('name') == name for tool in body.get('tools', [])):
                                raise ValueError('native client tool schema missing')
                            arguments = ({'cmd':'printf FIXTURE_TOOL_OK > proxy-fixture.txt; cat proxy-fixture.txt',
                                          'yield_time_ms':1000,'max_output_tokens':128} if outer.client == 'codex'
                                         else {'command':'printf FIXTURE_TOOL_OK > proxy-fixture.txt; cat proxy-fixture.txt'})
                            output = [{'type':'function_call','id':'fc_fixture','status':'completed',
                                'name':name,'call_id':'call_fixture','arguments':json.dumps(arguments)}]
                        else:
                            if not any(item.get('type') == 'function_call_output'
                                       for item in body.get('input', []) if isinstance(item,dict)):
                                raise ValueError('native full-history tool-result echo missing')
                            output = [{'type':'message','id':'msg_fixture','status':'completed','role':'assistant',
                                'content':[{'type':'output_text','text':'FIXTURE_COMPLETE','annotations':[]}]}]
                        value = {'id':f'resp_fixture_{count}','object':'response','status':'completed',
                            'model':'gpt-6-luna','output':output,
                            'usage':{'input_tokens':1000,'output_tokens':10,'total_tokens':1010,
                            'input_tokens_details':{'cached_tokens':0,'cache_write_tokens':1000},
                            'output_tokens_details':{'reasoning_tokens':0}}}
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
                            else:
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
                            events.append({'type':'response.output_item.done','output_index':index,'item':item})
                        events.append({'type':'response.completed','response':value})
                        raw = b''.join(('event: '+event['type']+'\ndata: '+json.dumps(event)+'\n\n').encode() for event in events)
                        content_type='text/event-stream'
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
    for client in ('codex','pi'):
        workspace=args.output / (client+'-workspace'); workspace.mkdir()
        subprocess.run(['git','init','-q',str(workspace)],check=True)
        env={k:v for k,v in os.environ.items() if not k.startswith(('OPENAI_','CARRY_PROXY_'))}
        env.update(CARRY_PROXY_UPSTREAM_KEY='synthetic-fixture-key', CARRY_PROXY_CLASSIFIER_KEY='synthetic-fixture-key')
        with Fixture(client) as fixture, socket.socket() as sock:
            sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]; sock.close()
            trial=args.output / (client+'-trial')
            command=['python3',str(Path(__file__).with_name('proxy_trial.py')),
                '--client',client,'--carry-binary',args.carry,'--client-binary',getattr(args,client),
                '--workspace',str(workspace),'--trial-dir',str(trial),'--mode','off',
                '--listen',f'127.0.0.1:{port}','--upstream-url',fixture.url+'/v1/responses',
                '--classifier-url',fixture.url+'/classifier','--timeout','60',
                '--prompt','Run the supplied tool once and finish. This is a scripted offline fixture.']
            run=subprocess.run(command,env=env,text=True,capture_output=True,timeout=85)
            trace=(trial / 'client-events.jsonl').read_text() if (trial / 'client-events.jsonl').exists() else ''
            marker=workspace / 'proxy-fixture.txt'
            import hashlib
            cache_key=fixture.calls[0].get('prompt_cache_key') if fixture.calls else None
            result={'client':client,'fixture_only':True,'exit_code':run.returncode,
                'native_cache_namespace_sha256':hashlib.sha256(cache_key.encode()).hexdigest() if cache_key else None,
                'native_requests':len(fixture.calls),'native_tool_result_echo':len(fixture.calls)>1,
                'tool_effect_verified':marker.is_file() and marker.read_text()=='FIXTURE_TOOL_OK',
                'native_final_verified':'FIXTURE_COMPLETE' in trace,'provider_errors':fixture.errors}
            results.append(result)
            (args.output / 'results.json').write_text(json.dumps(results,indent=2)+'\n')
            if run.returncode or fixture.errors or not result['tool_effect_verified'] or not result['native_final_verified'] or not result['native_tool_result_echo']:
                raise RuntimeError(f'{client} integrated fixture failed; inspect isolated fixture artifacts')
    if len({r['native_cache_namespace_sha256'] for r in results}) != len(results):
        raise RuntimeError('fresh native clients reused a cache namespace')
    print(json.dumps(results,indent=2))


if __name__ == '__main__':
    main()
