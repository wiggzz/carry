"""Fixture-only launcher for the actual production gateway handler."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]


class Gateway:
    def __init__(self, carry_port, provider_port, policy=None):
        env = {k:v for k,v in os.environ.items() if not k.startswith(('OPENAI_', 'CARRY_PROXY_', 'BENCHMARK_'))}
        env.update(BENCHMARK_CONTEXT_PROXY='1', BENCHMARK_CLIENT_TOKEN='fixture-client',
            CARRY_PROXY_AUTH_TOKEN='fixture-trusted', BENCHMARK_SESSION_ID='fixture-session',
            BENCHMARK_SHADOW_TOKEN='fixture-shadow', BENCHMARK_CLASSIFIER_KEY='fixture-shadow-provider',
            FIXTURE_CARRY_PORT=str(carry_port), FIXTURE_PROVIDER_PORT=str(provider_port))
        if policy is not None:
            env['BENCHMARK_CONTEXT_HISTORY_POLICY'] = policy
        script = "const s=require(process.argv[1]).serve({port:0,host:'127.0.0.1'}); s.on('listening',()=>console.log('PORT '+s.address().port)); process.on('SIGTERM',()=>s.close(()=>process.exit(0)));"
        self.log = tempfile.TemporaryFile()
        self.error_log = tempfile.TemporaryFile()
        self.process = subprocess.Popen(['node', '--require', str(ROOT/'scripts/proxy_gateway_fixture_transport.cjs'),
            '-e', script, str(ROOT/'scripts/openai_proxy.js')], env=env,
            stdout=self.log, stderr=self.error_log)
        deadline = time.monotonic()+20
        while True:
            lines = os.pread(self.log.fileno(), 4096, 0).decode().splitlines()
            if lines and lines[0].startswith('PORT '):
                self.url = 'http://127.0.0.1:'+lines[0].split()[1]
                break
            if self.process.poll() is not None:
                self.close()
                raise RuntimeError('gateway exited before readiness: '+self.stderr)
            if time.monotonic() >= deadline:
                self.close()
                raise RuntimeError('gateway readiness timeout')
            time.sleep(.02)
    def close(self):
        self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill(); self.process.wait(timeout=5)
        self.log.seek(0); self.stdout = self.log.read().decode(); self.log.close()
        self.error_log.seek(0); self.stderr = self.error_log.read().decode(); self.error_log.close()
        self.events = [json.loads(line.split(' ',1)[1]) for line in self.stdout.splitlines()
                       if line.startswith('BENCHMARK_CONTEXT_EVENT ')]
    def request(self, body, headers=None, token='fixture-client'):
        req = urllib.request.Request(self.url+'/v1/responses', data=body,
            headers={'Authorization':'Bearer '+token, **(headers or {})})
        try:
            with urllib.request.urlopen(req, timeout=3) as response:
                return response.status, response.read()
        except urllib.error.HTTPError as error:
            with error:
                return error.code, error.read()
