#!/usr/bin/env python3
"""One opt-in local native-client trial, isolated from normal client config."""
import argparse
import ipaddress
import json
import os
from pathlib import Path
import secrets
import signal
import queue
import threading
import subprocess
import time
import urllib.error
import urllib.request
import uuid


def client_configuration(root, *, client, base_url, model, session_id,
                         history_policy="strict", native_compaction="disabled"):
    """Write credential-free native HTTP Responses provider configuration."""
    headers = {'x-carry-session': session_id, 'x-carry-tenant':'local-trial', 'x-carry-branch':'main'}
    if history_policy != 'strict': headers['x-carry-history-policy']=history_policy
    if client == 'codex':
        home = root / 'codex'; home.mkdir()
        controls=[]
        if native_compaction != 'disabled':
            controls=['model_context_window = 500000', 'model_auto_compact_token_limit = 20000',
                'model_auto_compact_token_limit_scope = \"total\"', '[features]',
                'token_budget = false', 'remote_compaction_v2 = '+str(native_compaction=='v2').lower()]
        text = '\n'.join(['model_provider = "carry-trial"'] + controls + [
            '[model_providers.carry-trial]',
            'name = '+json.dumps('OpenAI' if native_compaction!='disabled' else 'Carry local trial'), f'base_url = {json.dumps(base_url)}',
            'wire_api = "responses"', 'env_key = "CARRY_TRIAL_GATEWAY_TOKEN"',
            'requires_openai_auth = false', 'request_max_retries = 0', 'stream_max_retries = 0',
            'supports_websockets = false',
            'http_headers = {' + ', '.join(f'{json.dumps(k)} = {json.dumps(v)}' for k,v in headers.items()) + '}', '',
        ])
        (home / 'config.toml').write_text(text)
        return {'CODEX_HOME':str(home)}
    home = root / 'pi'; home.mkdir()
    provider = {'baseUrl':base_url, 'api':'openai-responses', 'apiKey':'$CARRY_TRIAL_GATEWAY_TOKEN',
        'headers':headers, 'models':[{'id':model, 'name':'Local trial', 'reasoning':True,
        'input':['text','image'], 'contextWindow':1050000 if model == 'gpt-6-luna' else 400000,
        'maxTokens':128000, 'cost':{'input':0,'output':0,'cacheRead':0,'cacheWrite':0}}]}
    (home / 'models.json').write_text(json.dumps({'providers':{'carry-trial':provider}}, indent=2) + '\n')
    return {'PI_CODING_AGENT_DIR':str(home), 'PI_OFFLINE':'1', 'PI_TELEMETRY':'0'}


def run_rpc_checkpoint(command, *, cwd, env, output, prompt, timeout):
    """Scripted fixture only: settled prompt -> manual compact -> settled continuation."""
    process=subprocess.Popen(command,cwd=cwd,env=env,stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True,start_new_session=True)
    events=queue.Queue()
    def read():
        for line in process.stdout:
            output.write(line); output.flush()
            try: events.put(json.loads(line))
            except ValueError: pass
        events.put(None)
    reader=threading.Thread(target=read,daemon=True); reader.start()
    deadline=time.monotonic()+timeout
    def send(value):
        process.stdin.write(json.dumps(value)+'\n'); process.stdin.flush()
    def wait(predicate):
        while True:
            remaining=deadline-time.monotonic()
            if remaining<=0: raise TimeoutError('RPC fixture deadline')
            value=events.get(timeout=remaining)
            if value is None: raise RuntimeError('RPC fixture early EOF')
            if predicate(value): return value
    try:
        send({'id':'p1','type':'prompt','message':prompt})
        wait(lambda e:e.get('type')=='agent_settled')
        send({'id':'c1','type':'compact'})
        compact=wait(lambda e:e.get('type')=='response' and e.get('id')=='c1')
        if compact.get('success') is not True: return 1
        send({'id':'p2','type':'prompt','message':'Continue from the installed checkpoint; finish.'})
        wait(lambda e:e.get('type')=='agent_settled')
        return 0
    finally:
        stop_process_group(process)
        reader.join(timeout=3)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--client', choices=['codex','pi'], required=True)
    parser.add_argument('--carry-binary', default='carry')
    parser.add_argument('--client-binary')
    parser.add_argument('--workspace', type=Path, required=True)
    parser.add_argument('--trial-dir', type=Path, required=True)
    parser.add_argument('--mode', choices=['off','audit','compact'], default='off')
    parser.add_argument('--pi-checkpoint-fixture',action='store_true',
        help='Credential-free scripted RPC manual-compaction/continuation fixture')
    parser.add_argument('--history-policy',choices=['strict','reset-on-divergence'],default='strict',
        help='Explicitly retire both active projections on caller replacement; never infer ancestry')
    parser.add_argument('--codex-sandbox',choices=['workspace-write','danger-full-access'],default='workspace-write',
        help='Unsandboxed mode is only for disposable contained fixtures; normal trials default workspace-write')
    parser.add_argument('--codex-native-compaction',choices=['disabled','v1','v2'],default='disabled',
        help='Fixture-only forced 20000-token native compaction threshold')
    parser.add_argument('--listen', default='127.0.0.1:8787')
    parser.add_argument('--upstream-url', default='https://api.openai.com/v1/responses')
    parser.add_argument('--classifier-url', default='https://api.openai.com/v1/responses')
    parser.add_argument('--model', default='gpt-6-luna')
    parser.add_argument('--reasoning', default='medium')
    parser.add_argument('--classifier-model', default='gpt-6-luna')
    parser.add_argument('--classifier-effort', default='low')
    parser.add_argument('--payoff-requests', default='1')
    parser.add_argument('--min-payback-percent', default='25')
    parser.add_argument('--timeout', type=int, default=300)
    parser.add_argument('--prompt', required=True)
    args = parser.parse_args()
    host, port = args.listen.rsplit(':',1)
    if not ipaddress.ip_address(host).is_loopback or not 1 <= int(port) <= 65535:
        parser.error('local trials require an explicit loopback listen address')
    workspace = args.workspace.resolve(strict=True)
    root = args.trial_dir.resolve()
    if root == workspace or root.is_relative_to(workspace):
        parser.error('trial state must be outside the model workspace')
    if root.exists():
        parser.error('trial directory must be fresh (choose a new directory for each arm)')
    if args.timeout < 1:
        parser.error('--timeout must be positive')
    root.mkdir(parents=True, mode=0o700)
    home = root / 'home'; home.mkdir()
    token, session = secrets.token_urlsafe(32), uuid.uuid4().hex
    env = os.environ.copy()
    env['CARRY_PROXY_AUTH_TOKEN'] = token
    command = [args.carry_binary,'proxy','--listen',args.listen,'--upstream-url',args.upstream_url,
        '--classifier-url',args.classifier_url,'--state-dir',str(root / 'state'), '--mode',args.mode,
        '--classifier-model',args.classifier_model,'--classifier-reasoning-effort',args.classifier_effort,
        '--payoff-requests',args.payoff_requests,'--min-payback-percent',args.min_payback_percent]
    proxy = None
    try:
        with (root / 'proxy.log').open('w') as log:
            proxy = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            deadline = time.monotonic() + 10
            while True:
                if proxy.poll() is not None:
                    raise RuntimeError('Carry proxy exited before readiness; inspect proxy.log')
                try:
                    with urllib.request.urlopen(f'http://{args.listen}/health', timeout=1) as response:
                        if response.status == 200:
                            break
                except (OSError, urllib.error.URLError):
                    pass
                if time.monotonic() >= deadline:
                    raise RuntimeError('Carry proxy did not become ready')
                time.sleep(0.05)
            metrics_request = urllib.request.Request(f'http://{args.listen}/carry/metrics', headers={
                'Authorization':'Bearer '+token, 'x-carry-session':session,
                'x-carry-tenant':'local-trial', 'x-carry-branch':'main'})
            with urllib.request.urlopen(metrics_request, timeout=3) as response:
                metrics = json.load(response)
            if metrics.get('mode') != args.mode:
                raise RuntimeError('Carry proxy effective mode does not match requested mode')
            client_env = {k:v for k,v in os.environ.items()
                          if not k.startswith(('OPENAI_', 'CARRY_PROXY_', 'CODEX_', 'PI_'))}
            client_env.update(HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
                              CARRY_TRIAL_GATEWAY_TOKEN=token)
            client_env.update(client_configuration(root, client=args.client,
                base_url=f'http://{args.listen}/v1', model=args.model, session_id=session,
                history_policy=args.history_policy,native_compaction=args.codex_native_compaction))
            binary = args.client_binary or args.client
            if args.client == 'codex':
                client_command = [binary,'exec','--cd',str(workspace),'--sandbox',args.codex_sandbox,
                    '--model',args.model,'--config',f'model_reasoning_effort={args.reasoning}',
                    '--config','approval_policy="never"','--json',args.prompt]
            else:
                client_command = [binary,'--mode','json','--provider','carry-trial','--model',args.model,
                    '--thinking',args.reasoning,'--no-approve','--no-extensions','--no-skills',
                    '--no-prompt-templates','--no-context-files',
                    '--session-dir',str(root / 'pi-sessions'),args.prompt]
            if args.pi_checkpoint_fixture:
                if args.client != 'pi': raise ValueError('RPC fixture requires Pi')
                client_command[client_command.index('json')]='rpc'
                client_command.pop()
                (root/'pi/settings.json').write_text(json.dumps({'compaction':{
                    'enabled':False,'reserveTokens':1024,'keepRecentTokens':1024}}))
            (root / 'trial.json').write_text(json.dumps({'client':args.client,'mode':args.mode,
                'session_id':session,'model':args.model,'reasoning':args.reasoning,
                'history_policy':args.history_policy,'codex_sandbox':args.codex_sandbox,
                'codex_native_compaction':args.codex_native_compaction,
                'classifier_model':args.classifier_model,'classifier_effort':args.classifier_effort,
                'payoff_requests':args.payoff_requests,'min_payback_percent':args.min_payback_percent}, indent=2)+'\n')
            with (root / 'client-events.jsonl').open('w') as output:
                if args.pi_checkpoint_fixture:
                    return run_rpc_checkpoint(client_command,cwd=workspace,env=client_env,output=output,
                        prompt=args.prompt,timeout=args.timeout)
                client = subprocess.Popen(client_command, cwd=workspace, env=client_env,
                    stdout=output, stderr=subprocess.STDOUT, start_new_session=True)
                try:
                    return client.wait(timeout=args.timeout)
                finally:
                    stop_process_group(client)
    finally:
        if proxy is not None:
            stop_process_group(proxy)


def stop_process_group(process):
    """Stop only our process group, including tool descendants after exit."""
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=5)


if __name__ == '__main__':
    raise SystemExit(main())
