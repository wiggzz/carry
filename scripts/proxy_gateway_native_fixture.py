#!/usr/bin/env python3
"""Pinned Pi -> production Node gateway -> integrated Carry; scripted provider ONLY.

The --require hook redirects only the gateway's fixed destinations to owned loopback
ports. No provider credential, package install, TLS interception or live inference.
RPC drives two native summaries and continuations; automatic compaction is NOT certified.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import socket
import subprocess
import threading
import time
import urllib.error
import urllib.request

try:
    from proxy_native_fixture import Fixture
    from proxy_trial import client_configuration, stop_process_group
    from proxy_gateway_fixture_support import Gateway
except ModuleNotFoundError:
    from scripts.proxy_native_fixture import Fixture
    from scripts.proxy_trial import client_configuration, stop_process_group
    from scripts.proxy_gateway_fixture_support import Gateway


def grade_case(case, *, exit_code, rpc, fixture, state, events):
    assert not fixture.errors, 'scripted provider contract failed'
    compact=[e for e in rpc if e.get('type')=='response' and e.get('id') in ('c1','c2')]
    if case=='compact-strict':
        assert exit_code and len(fixture.calls)==2 and not fixture.summary_calls
        assert compact and compact[0].get('success') is False and '409' in json.dumps(compact[0])
        return {'strict_409_verified':True}
    assert not exit_code, 'Pi fixture process failed'
    assert len(fixture.summary_calls)>=2 and len(fixture.calls)-len(fixture.summary_calls)==4
    assert len(compact)==2 and all(e.get('success') is True for e in compact)
    assert sum(e.get('type')=='agent_settled' for e in rpc)>=3
    assert state['history_rebases']>=2 and not state['active_shadow']
    received=[e['data'] for e in events if e['event']=='primary_received']
    submitted=[e['data'] for e in events if e['event']=='primary_submitted']
    assert received and submitted and received[-1]==submitted[-1]
    assert 'FIXTURE_PI_CHECKPOINT' in json.dumps(submitted[-1]['input'])
    return {'two_summary_continuation_verified':True,'checkpoint_wire_preserved':True,
        'summary_calls':len(fixture.summary_calls),'history_rebases':state['history_rebases']}


def run_rpc_two_summaries(command, *, cwd, env, output, timeout=60):
    process=subprocess.Popen(command,cwd=cwd,env=env,stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True,start_new_session=True)
    values=queue.Queue()
    def read():
        for line in process.stdout:
            output.write(line); output.flush()
            try: values.put(json.loads(line))
            except ValueError: pass
        values.put(None)
    reader=threading.Thread(target=read,daemon=True); reader.start()
    deadline=time.monotonic()+timeout
    def send(value):
        process.stdin.write(json.dumps(value)+'\n'); process.stdin.flush()
    def wait(predicate):
        while True:
            remaining=deadline-time.monotonic()
            if remaining<=0: raise TimeoutError('two-summary RPC fixture deadline')
            value=values.get(timeout=remaining)
            if value is None: raise RuntimeError('two-summary RPC fixture early EOF')
            if predicate(value): return value
    try:
        send({'id':'p1','type':'prompt','message':'Run the supplied fixture tools and finish.'})
        wait(lambda e:e.get('type')=='agent_settled')
        for number in (1,2):
            send({'id':f'c{number}','type':'compact'})
            compact=wait(lambda e:e.get('type')=='response' and e.get('id')==f'c{number}')
            if compact.get('success') is not True: return 1
            # Explicit fixture-only new native input gives the second manual summary real work.
            # This is not a production context limit, truncation or agent step cap.
            message='Continue from the installed checkpoint; finish.'
            if number==1: message += '\n' + 'SECOND_CHECKPOINT_FIXTURE_CONTEXT ' * 200
            send({'id':f'p{number+1}','type':'prompt','message':message})
            wait(lambda e:e.get('type')=='agent_settled')
        return 0
    finally:
        stop_process_group(process); reader.join(timeout=3)
        process.stdin.close(); process.stdout.close()


def run_case(args,case):
    root=args.output/case; root.mkdir()
    workspace=root/'workspace'; workspace.mkdir()
    home=root/'home'; home.mkdir()
    state_dir=root/'state'
    with socket.socket() as sock:
        sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
    env={k:v for k,v in os.environ.items() if not k.startswith(('OPENAI_','CARRY_PROXY_','BENCHMARK_','PI_','CODEX_'))}
    env.update(CARRY_PROXY_UPSTREAM_KEY='fixture-primary-provider',
        CARRY_PROXY_CLASSIFIER_KEY='fixture-shadow',CARRY_PROXY_AUTH_TOKEN='fixture-trusted')
    mode='off' if case=='off-strict' else 'compact'
    policy='reset-on-divergence' if case=='compact-reset' else 'strict'
    with Fixture('pi','pi-checkpoint') as fixture, (root/'carry.log').open('w') as log:
        # Start gateway first so the integrated classifier traverses the production shadow handler.
        gateway=Gateway(port,fixture.server.server_port,policy)
        proxy=None
        try:
            command=[args.carry,'proxy','--listen',f'127.0.0.1:{port}',
                '--upstream-url',fixture.url+'/v1/responses','--classifier-url',gateway.url+'/v1/responses',
                '--state-dir',str(state_dir),'--mode',mode,'--payoff-requests','5','--min-payback-percent','0']
            proxy=subprocess.Popen(command,env=env,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
            deadline=time.monotonic()+20
            while True:
                if proxy.poll() is not None: raise RuntimeError('integrated proxy exited before readiness')
                try:
                    with urllib.request.urlopen(gateway.url+'/healthz',timeout=1) as response:
                        if response.status==200: break
                except (OSError,urllib.error.URLError): pass
                if time.monotonic()>=deadline: raise RuntimeError('integrated gateway health timeout')
                time.sleep(.05)
            client_env={k:v for k,v in env.items() if not k.startswith(('CARRY_PROXY_','BENCHMARK_'))}
            client_env.update(HOME=str(home),XDG_CONFIG_HOME=str(home/'.config'),
                CARRY_TRIAL_GATEWAY_TOKEN='fixture-client')
            # Caller intentionally asks for reset even in strict case: only operator policy is authoritative.
            client_env.update(client_configuration(root,client='pi',base_url=gateway.url+'/v1',
                model='gpt-6-luna',session_id='spoofed-client-session',history_policy='reset-on-divergence'))
            (root/'pi/settings.json').write_text(json.dumps({'compaction':{
                'enabled':False,'reserveTokens':1024,'keepRecentTokens':1024}}))
            command=[args.pi,'--mode','rpc','--provider','carry-trial','--model','gpt-6-luna',
                '--thinking','medium','--no-approve','--no-extensions','--no-skills',
                '--no-prompt-templates','--no-context-files','--session-dir',str(root/'pi-sessions')]
            with (root/'client-events.jsonl').open('w') as output:
                exit_code=run_rpc_two_summaries(command,cwd=workspace,env=client_env,output=output)
        finally:
            if proxy is not None: stop_process_group(proxy)
            gateway.close()
            (root/'gateway.log').write_text(gateway.stdout+gateway.stderr)
    rpc=[json.loads(line) for line in (root/'client-events.jsonl').read_text().splitlines() if line.startswith('{')]
    files=list(state_dir.glob('*.json'))
    assert len(files)==1, 'one operator-owned session required'
    state=json.loads(files[0].read_text())
    events=[json.loads(line) for line in files[0].with_suffix('.jsonl').read_text().splitlines()]
    # If caller identity had leaked, its tenant/session would have created a different state file.
    expected=hashlib.sha256(json.dumps(['benchmark','fixture-session','main'],separators=(',',':')).encode()).hexdigest()
    assert files[0].stem==expected, 'caller identity leaked across trusted gateway'
    marker=workspace/'proxy-fixture.txt'
    assert marker.read_text()=='FIXTURE_TOOL_OK', 'actual native tool effect required'
    cache_key=fixture.calls[0]['prompt_cache_key']
    result={'case':case,'fixture_only':True,'client':'pi','history_policy':policy,'mode':mode,
        'native_cache_namespace_sha256':hashlib.sha256(cache_key.encode()).hexdigest(),
        'native_requests':len(fixture.calls),'operator_identity_verified':True,
        'tool_effect_verified':True,'production_gateway_verified':True,
        'automatic_compaction_certified':False, 'summary_driver':'two native RPC compact commands'}
    result.update(grade_case(case,exit_code=exit_code,rpc=rpc,fixture=fixture,state=state,events=events))
    if case!='compact-strict':
        assert 'FIXTURE_COMPLETE' in (root/'client-events.jsonl').read_text()
    (root/'result.json').write_text(json.dumps(result,indent=2)+'\n')
    return result


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--carry',required=True)
    parser.add_argument('--pi',required=True)
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    if args.output.exists(): parser.error('fixture output must be fresh')
    version=subprocess.run([args.pi,'--version'],text=True,capture_output=True,check=True,timeout=20).stdout.strip()
    if version!='0.84.2': parser.error('requires the CI-pinned Pi 0.84.2')
    args.output=args.output.resolve(); args.output.mkdir(parents=True)
    results=[]
    for case in ('compact-strict','off-strict','compact-reset'):
        results.append(run_case(args,case))
        (args.output/'results.json').write_text(json.dumps(results,indent=2)+'\n')
    assert len(results)==3 and len({r['native_cache_namespace_sha256'] for r in results})==3
    print(json.dumps({'pi_version':version,'cases':results},indent=2))


if __name__=='__main__': main()
