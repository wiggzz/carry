#!/usr/bin/env python3
"""One opt-in local native-client trial, isolated from normal client config."""
import argparse
import ipaddress
import json
import os
from pathlib import Path
import secrets
import signal
import subprocess
import time
import urllib.error
import urllib.request
import uuid


def client_configuration(root, *, client, base_url, model, session_id):
    """Write credential-free native HTTP Responses provider configuration."""
    headers = {'x-carry-session': session_id, 'x-carry-tenant':'local-trial', 'x-carry-branch':'main'}
    if client == 'codex':
        home = root / 'codex'; home.mkdir()
        text = '\n'.join([
            'model_provider = "carry-trial"', '[model_providers.carry-trial]',
            'name = "Carry local trial"', f'base_url = {json.dumps(base_url)}',
            'wire_api = "responses"', 'env_key = "CARRY_TRIAL_GATEWAY_TOKEN"',
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--client', choices=['codex','pi'], required=True)
    parser.add_argument('--carry-binary', default='carry')
    parser.add_argument('--client-binary')
    parser.add_argument('--workspace', type=Path, required=True)
    parser.add_argument('--trial-dir', type=Path, required=True)
    parser.add_argument('--mode', choices=['off','audit','compact'], default='off')
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
                base_url=f'http://{args.listen}/v1', model=args.model, session_id=session))
            binary = args.client_binary or args.client
            if args.client == 'codex':
                client_command = [binary,'exec','--cd',str(workspace),'--sandbox','workspace-write',
                    '--model',args.model,'--config',f'model_reasoning_effort={args.reasoning}',
                    '--config','approval_policy="never"','--json',args.prompt]
            else:
                client_command = [binary,'--mode','json','--provider','carry-trial','--model',args.model,
                    '--thinking',args.reasoning,'--no-approve','--no-extensions','--no-skills',
                    '--no-prompt-templates','--no-context-files',
                    '--session-dir',str(root / 'pi-sessions'),args.prompt]
            (root / 'trial.json').write_text(json.dumps({'client':args.client,'mode':args.mode,
                'session_id':session,'model':args.model,'reasoning':args.reasoning,
                'classifier_model':args.classifier_model,'classifier_effort':args.classifier_effort,
                'payoff_requests':args.payoff_requests,'min_payback_percent':args.min_payback_percent}, indent=2)+'\n')
            with (root / 'client-events.jsonl').open('w') as output:
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
