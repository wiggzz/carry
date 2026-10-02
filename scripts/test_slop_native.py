"""Native four-stage fake-provider check. Hosted CI supplies its built binary.

No build/install/model spend here. Local diagnostic binaries are explicitly not
candidate-head CI evidence. Fake usage/provider data are test fixtures only.
"""
import http.server
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import threading
import unittest

ROOT=Path(__file__).resolve().parents[1]


@unittest.skipUnless(os.environ.get('CARRY_NATIVE_TEST_BINARY'),'hosted built binary required')
class NativeContinuityTests(unittest.TestCase):
    def test_native_adapter_four_stages_complete_resume_with_signals_and_no_compaction(self):
        from scripts import slopbench as s
        requests=[]
        class Provider(http.server.BaseHTTPRequestHandler):
            def log_message(self,format,*args):pass
            def do_POST(self):
                request=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                requests.append(request);index=len(requests);stage=(index+1)//2
                action='shell' if index%2 else 'finish'
                context={'protected':[],'removable':[],'remember':[]}
                if action=='shell':
                    arguments={'command':"printf 'stage-%s\\n' "+str(stage)+' >> sentinel',
                               'timeout_seconds':5,'context':context}
                else:arguments={'answer':f'completed-stage-{stage}','context':context}
                item={'type':'function_call','call_id':f'call-{index}',
                      'name':action,'arguments':json.dumps(arguments)}
                body={'id':f'resp-{index}','output':[item],
                      'usage':{'input_tokens':100*index,'output_tokens':10,'total_tokens':100*index+10,
                               'input_tokens_details':{'cached_tokens':0,'cache_write_tokens':0},
                               'output_tokens_details':{'reasoning_tokens':0}}}
                events=[{'type':'response.output_item.added','item':item},
                        {'type':'response.output_item.done','item':item},
                        {'type':'response.completed','response':body}]
                response=''.join('data: '+json.dumps(e)+'\n\n' for e in events).encode()
                self.send_response(200);self.send_header('Content-Type','text/event-stream')
                self.send_header('Content-Length',str(len(response)));self.end_headers()
                self.wfile.write(response)
        provider=http.server.ThreadingHTTPServer(('127.0.0.1',0),Provider)
        thread=threading.Thread(target=provider.serve_forever,daemon=True);thread.start()
        try:
            with tempfile.TemporaryDirectory() as tmp:
                root=Path(tmp);workspace=root/'workspace';workspace.mkdir()
                previous=None;cache_key=None;total_input=0
                binary=str(Path(os.environ['CARRY_NATIVE_TEST_BINARY']).resolve())
                for n in range(1,5):
                    output=root/f'stage-{n}';output.mkdir()
                    prompt=root/f'prompt-{n}.md';prompt.write_text(f'immutable-stage-{n}')
                    source=previous/s.STATE_FILE if previous else None
                    source_bytes=source.read_bytes() if source else None
                    source_files={p.name:p.read_bytes() for p in previous.iterdir() if p.is_file()} if previous else {}
                    env=dict(os.environ,OPENAI_API_KEY='synthetic-local-fixture-key',
                             OPENAI_BASE_URL=f'http://127.0.0.1:{provider.server_port}/v1',
                             BENCHMARK_WORKSPACE=str(workspace),CARRY_COMPACTION_POLICY='disabled',
                             AGENT_TIMEOUT_SECONDS='30',HOME=str(root/'home'),
                             AGENT_COMMAND=shlex.join([binary,'--cwd',str(workspace),'--session-dir','{output}',
                                  '--model','{model}','--reasoning-effort','{reasoning}',
                                  '--compaction-policy','{compaction_policy}','-p','{prompt_text}']))
                    command=[sys.executable,str(ROOT/'containers/swebench-harness/entrypoint.py'),
                        'run','--harness','carry','--model','gpt-6-luna','--reasoning','medium',
                        '--prompt',str(prompt),'--output',str(output),'--snapshot-only']
                    if previous:command+=['--resume-session',str(previous)]
                    proc=subprocess.run(command,cwd=workspace,env=env,capture_output=True,text=True,timeout=40)
                    self.assertEqual(proc.returncode,0,proc.stderr+'\n'+(output/'trace.log').read_text(errors='replace'))
                    state=json.loads((output/s.STATE_FILE).read_text())
                    self.assertEqual(state['context']['generation'],0)
                    if cache_key:self.assertEqual(state['prompt_cache_key'],cache_key)
                    cache_key=state['prompt_cache_key'];self.assertTrue(cache_key)
                    self.assertEqual((workspace/'sentinel').read_text(),''.join(f'stage-{i}\n' for i in range(1,n+1)))
                    self.assertFalse((workspace/'.git').exists())
                    trace=[json.loads(l) for l in (output/'trace.jsonl').read_text().splitlines()]
                    self.assertFalse(any(e['event']=='context_compacted' for e in trace))
                    s.validate_native_trace(output,n)
                    names=[e['event'] for e in trace]
                    self.assertIn('run_started' if n==1 else 'session_resumed',names)
                    if n>1:
                        assert source is not None and previous is not None
                        self.assertEqual(source.read_bytes(),source_bytes)
                        self.assertEqual({p.name:p.read_bytes() for p in previous.iterdir() if p.is_file()},source_files)
                        first=next(e['data']['request'] for e in trace if e['event']=='model_request')
                        visible=json.dumps(first['input'])
                        for old in range(1,n):
                            self.assertIn(f'immutable-stage-{old}',visible)
                            self.assertIn(f'completed-stage-{old}',visible)
                        self.assertIn('function_call_output',visible)
                    first=next(e['data']['request'] for e in trace if e['event']=='model_request')
                    self.assertEqual(first['model'],'gpt-6-luna');self.assertEqual(first['reasoning']['effort'],'medium')
                    for tool in first['tools']:
                        if tool['name'] in {'shell','finish'}:
                            self.assertEqual(set(tool['parameters']['properties']['context']['properties']),
                                             {'protected','removable','remember'})
                    responses=[e['data'] for e in trace if e['event']=='model_response']
                    (output/'proxy.log').write_text(''.join('BENCHMARK_PROXY_RESPONSE '+json.dumps({
                        'response_id':r['response_id'],**{k:r['usage'][k] for k in
                         ('input_tokens','cached_input_tokens','cache_write_input_tokens','output_tokens')}})+'\n' for r in responses))
                    metrics=s.stage_usage(output);self.assertTrue(metrics['metering_complete'])
                    self.assertEqual(metrics['model_requests'],2)
                    total_input+=metrics['usage']['input_tokens'];previous=output
                self.assertEqual(len(requests),8)
                self.assertEqual(total_input,sum(100*i for i in range(1,9)))
                self.assertEqual(len({r['prompt_cache_key'] for r in requests}),1)
        finally:
            provider.shutdown();provider.server_close();thread.join(timeout=5)


if __name__=='__main__':unittest.main()
