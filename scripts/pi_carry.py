#!/usr/bin/env python3
"""Run an installed Pi client through the local Carry proxy."""
import json
import os
from pathlib import Path
import secrets
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

root = Path.home() / '.local/share/carry-proxy'
carry_binary = os.environ.get('CARRY_PI_BINARY', str(root / 'carry'))
pi_binary = os.environ.get('CARRY_PI_CLIENT', 'pi')
mode = os.environ.get('CARRY_PI_MODE', 'compact')
model = os.environ.get('CARRY_PI_MODEL', 'gpt-6.1-sol')
reviewer_model = os.environ.get('CARRY_PI_CLASSIFIER_MODEL', 'gpt-6-luna')
if mode not in ('off', 'audit', 'compact'):
    sys.exit('CARRY_PI_MODE must be off, audit, or compact')
check = sys.argv[1:] == ['--check']
auth = os.environ.get('CARRY_PI_AUTH', 'codex')
if auth not in ('codex', 'api-key'):
    sys.exit('CARRY_PI_AUTH must be codex or api-key')
env = os.environ.copy()
if not check and auth == 'api-key':
    if not (env.get('CARRY_PROXY_UPSTREAM_KEY') or env.get('OPENAI_API_KEY')):
        sys.exit('Set CARRY_PROXY_UPSTREAM_KEY or OPENAI_API_KEY securely before launching pi-carry.')
    if mode != 'off' and not (env.get('CARRY_PROXY_CLASSIFIER_KEY') or env.get('OPENAI_API_KEY')):
        sys.exit('Set CARRY_PROXY_CLASSIFIER_KEY or OPENAI_API_KEY for paid shadow review.')
token = secrets.token_urlsafe(32)
session = uuid.uuid4().hex
run_dir = root / 'runs' / session
run_dir.mkdir(parents=True, mode=0o700)
env.update(CARRY_PROXY_AUTH_TOKEN=token, CARRY_PI_TOKEN=token, CARRY_PI_SESSION=session)
command = [carry_binary, 'proxy', '--listen', '127.0.0.1:8787',
           '--state-dir', str(run_dir / 'state'), '--mode', mode,
           '--classifier-model', reviewer_model, '--classifier-reasoning-effort', 'low',
           '--classifier-cache-policy', 'auto', '--review-replayed-history', '--payoff-requests', '1',
           '--min-payback-percent', '25']
if auth == 'codex':
    command.append('--codex-login')
proxy = None
try:
    with (run_dir / 'proxy.log').open('w') as log:
        os.chmod(run_dir / 'proxy.log', 0o600)
        proxy = subprocess.Popen(command, env=env, stdout=log, stderr=log)
        for _ in range(100):
            if proxy.poll() is not None:
                sys.exit(f'Proxy failed to start. See {run_dir / "proxy.log"} (is port 8787 in use?)')
            try:
                request = urllib.request.Request('http://127.0.0.1:8787/carry/metrics',
                    headers={'Authorization': 'Bearer ' + token})
                with urllib.request.urlopen(request, timeout=1) as response:
                    metrics = json.load(response)
                if metrics['mode'] != mode:
                    sys.exit('Proxy mode mismatch')
                break
            except (OSError, urllib.error.URLError):
                time.sleep(0.1)
        else:
            sys.exit(f'Proxy readiness timed out. See {run_dir / "proxy.log"}')
        if check:
            print(f'Carry proxy ready: authenticated loopback, mode={mode}, auth={auth}; no model calls made.')
        else:
            dashboard_url = 'http://127.0.0.1:8787/carry/dashboard'
            link_file = root / 'dashboard-url'
            temporary = run_dir / 'dashboard-url'
            fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, 'w') as saved:
                saved.write(dashboard_url + '\n')
            os.replace(temporary, link_file)
            print(f'Carry proxy: {mode}, auth={auth}; logs/state: {run_dir}', file=sys.stderr)
            print(f'Dashboard: {dashboard_url} (saved in {link_file})', file=sys.stderr)
        # Only the scoped loopback credential goes into Pi's environment.
        client_env = {k: v for k, v in env.items()
                      if not k.startswith('CARRY_PROXY_') and k != 'OPENAI_API_KEY'}
        args = ['--list-models', 'carry'] if check else ['--model', f'carry/{model}', *sys.argv[1:]]
        result = subprocess.run([pi_binary, *args], env=client_env)
        sys.exit(result.returncode)
except KeyboardInterrupt:
    sys.exit(130)
finally:
    if proxy is not None and proxy.poll() is None:
        proxy.terminate()
        try:
            proxy.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proxy.kill()
            proxy.wait()
