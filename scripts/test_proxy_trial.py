"""Exercise the real local trial launcher with fake executables."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class ProxyTrialTests(unittest.TestCase):
    def test_both_clients_use_fresh_homes_scoped_auth_and_stable_session(self):
        for client, cache_policy in (('codex', 'auto'), ('pi', 'disabled'), ('codex', 'openai-explicit')):
            with self.subTest(client=client), tempfile.TemporaryDirectory() as directory:
                root = Path(directory); workspace = root / 'workspace'; workspace.mkdir()
                carry = root / 'carry'
                carry.write_text('''#!/usr/bin/env python3
import http.server,json,os,sys
assert os.environ.get('CARRY_PROXY_UPSTREAM_KEY') == 'provider-private'
assert '--mode' in sys.argv and sys.argv[sys.argv.index('--mode')+1] == 'compact'
assert sys.argv[sys.argv.index('--classifier-cache-policy')+1] == os.environ['EXPECT_REVIEWER_CACHE_POLICY']
class Handler(http.server.BaseHTTPRequestHandler):
 def do_GET(self):
  self.send_response(200); self.end_headers(); self.wfile.write(b'{"mode":"compact"}')
 def log_message(self,*a): pass
listen=sys.argv[sys.argv.index('--listen')+1]
host,port=listen.rsplit(':',1)
http.server.HTTPServer((host,int(port)),Handler).serve_forever()
''')
                carry.chmod(0o755)
                binary = root / client
                binary.write_text('''#!/usr/bin/env python3
import json,os,pathlib,sys
assert 'CARRY_PROXY_UPSTREAM_KEY' not in os.environ
assert 'CARRY_PROXY_CLASSIFIER_KEY' not in os.environ
assert 'OPENAI_API_KEY' not in os.environ
assert os.environ.get('CARRY_TRIAL_GATEWAY_TOKEN') != 'provider-private'
pathlib.Path(os.environ['HOME'],'client-evidence.json').write_text(json.dumps({'argv':sys.argv,'home':os.environ['HOME']}))
''')
                binary.chmod(0o755)
                # Ask the kernel for a loopback port; the launcher still owns only
                # the exact process it creates, never a global listener/service.
                import socket
                with socket.socket() as sock:
                    sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
                trial = root / 'trial'
                env = dict(os.environ, CARRY_PROXY_UPSTREAM_KEY='provider-private',
                           CARRY_PROXY_CLASSIFIER_KEY='provider-private',
                           EXPECT_REVIEWER_CACHE_POLICY=cache_policy)
                result = subprocess.run(['python3', str(ROOT / 'scripts/proxy_trial.py'),
                    '--client', client, '--carry-binary', str(carry), '--client-binary', str(binary),
                    '--workspace', str(workspace), '--trial-dir', str(trial), '--mode', 'compact',
                    '--listen', '127.0.0.1:' + str(port), '--prompt', 'fixture',
                    '--history-policy','reset-on-divergence',
                    *(['--classifier-cache-policy',cache_policy] if cache_policy != 'auto' else []),
                    *(['--codex-sandbox','danger-full-access','--codex-native-compaction','v1'] if client=='codex' else [])],
                    env=env, text=True, capture_output=True, timeout=20)
                self.assertEqual(result.returncode, 0, result.stderr)
                evidence = json.loads((trial / 'home/client-evidence.json').read_text())
                self.assertEqual(json.loads((trial/'trial.json').read_text())['classifier_cache_policy'],cache_policy)
                self.assertNotEqual(evidence['home'], os.environ['HOME'])
                self.assertNotIn('provider-private', ''.join(p.read_text() for p in trial.rglob('*') if p.is_file()))
                if client == 'codex':
                    import tomllib
                    config = tomllib.loads((trial / 'codex/config.toml').read_text())
                    provider = config['model_providers']['carry-trial']
                    self.assertEqual(provider['wire_api'], 'responses')
                    self.assertEqual(provider['name'],'OpenAI')
                    self.assertEqual(config['model_auto_compact_token_limit'],20000)
                    self.assertFalse(config['features']['remote_compaction_v2'])
                    self.assertEqual(provider['http_headers']['x-carry-history-policy'],'reset-on-divergence')
                    self.assertIn('danger-full-access',evidence['argv'])
                    self.assertTrue(provider['http_headers']['x-carry-session'])
                    self.assertFalse(provider.get('supports_websockets', True))
                else:
                    config = json.loads((trial / 'pi/models.json').read_text())
                    self.assertEqual(config['providers']['carry-trial']['api'], 'openai-responses')
                    self.assertTrue(config['providers']['carry-trial']['headers']['x-carry-session'])
                    self.assertEqual(config['providers']['carry-trial']['headers']['x-carry-history-policy'],'reset-on-divergence')
                with socket.socket() as sock:
                    self.assertNotEqual(sock.connect_ex(('127.0.0.1',port)), 0, 'owned proxy not cleaned')


    def test_rpc_checkpoint_waits_for_settled_then_installs_and_continues(self):
        from scripts.proxy_trial import run_rpc_checkpoint
        import sys
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); binary=root/'fake-pi'
            binary.write_text('''import json,sys
for line in sys.stdin:
 command=json.loads(line)
 if command['type']=='prompt':
  print(json.dumps({'type':'response','id':command['id'],'success':True}),flush=True)
  print(json.dumps({'type':'agent_end'}),flush=True)
  print(json.dumps({'type':'agent_settled'}),flush=True)
 else:
  print(json.dumps({'type':'response','id':command['id'],'success':True,'data':{'summary':'checkpoint'}}),flush=True)
''')
            import warnings
            with warnings.catch_warnings(record=True) as caught:
                warnings.simplefilter('always',ResourceWarning)
                with (root/'events').open('w') as output:
                    code=run_rpc_checkpoint([sys.executable,str(binary)],cwd=root,env=os.environ.copy(),
                        output=output,prompt='fixture',timeout=3)
            self.assertFalse(caught,'RPC supervisor must close owned subprocess pipes')
            self.assertEqual(code,0)
            events=[json.loads(line) for line in (root/'events').read_text().splitlines()]
            self.assertEqual(sum(e['type']=='agent_settled' for e in events),2)
            self.assertEqual([e['id'] for e in events if e['type']=='response'],['p1','c1','p2'])

    def test_effective_mode_is_checked_locally_before_client_start(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); workspace=root/'workspace'; workspace.mkdir()
            carry=root/'carry'
            carry.write_text('''#!/usr/bin/env python3
import http.server,sys
class Handler(http.server.BaseHTTPRequestHandler):
 def do_GET(self):
  self.send_response(200); self.end_headers(); self.wfile.write(b'{"mode":"off"}')
 def log_message(self,*a): pass
listen=sys.argv[sys.argv.index('--listen')+1]; host,port=listen.rsplit(':',1)
http.server.HTTPServer((host,int(port)),Handler).serve_forever()
'''); carry.chmod(0o755)
            binary=root/'client'
            binary.write_text('#!/usr/bin/env python3\nimport pathlib\npathlib.Path("client-started").touch()\n'); binary.chmod(0o755)
            import socket
            with socket.socket() as sock:
                sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
            run=subprocess.run(['python3',str(ROOT/'scripts/proxy_trial.py'),'--client','pi',
                '--carry-binary',str(carry),'--client-binary',str(binary),'--workspace',str(workspace),
                '--trial-dir',str(root/'trial'),'--mode','compact','--listen',f'127.0.0.1:{port}',
                '--prompt','fixture'],text=True,capture_output=True,timeout=20)
            self.assertNotEqual(run.returncode,0)
            self.assertFalse((workspace/'client-started').exists())


    def test_trial_timeout_stops_native_client_tool_descendants(self):
        import socket
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); workspace=root/'workspace'; workspace.mkdir()
            carry=root/'carry'; binary=root/'pi'
            carry.write_text('''#!/usr/bin/env python3
import http.server,sys
class Handler(http.server.BaseHTTPRequestHandler):
 def do_GET(self):
  self.send_response(200); self.end_headers(); self.wfile.write(b'{"mode":"off"}')
 def log_message(self,*a): pass
host,port=sys.argv[sys.argv.index('--listen')+1].rsplit(':',1)
http.server.HTTPServer((host,int(port)),Handler).serve_forever()
'''); carry.chmod(0o755)
            binary.write_text('''#!/usr/bin/env python3
import subprocess,sys,time,pathlib,os
child=subprocess.Popen([sys.executable,'-c',"import socket,pathlib,os,time; s=socket.socket(); s.bind(('127.0.0.1',0)); s.listen(); pathlib.Path(os.environ['HOME'],'tool-port').write_text(str(s.getsockname()[1])); time.sleep(60)"])
pathlib.Path(os.environ['HOME'],'tool-pid').write_text(str(child.pid))
time.sleep(60)
'''); binary.chmod(0o755)
            with socket.socket() as sock:
                sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
            run=subprocess.run(['python3',str(ROOT/'scripts/proxy_trial.py'),'--client','pi',
                '--carry-binary',str(carry),'--client-binary',str(binary),'--workspace',str(workspace),
                '--trial-dir',str(root/'trial'),'--timeout','1','--listen',f'127.0.0.1:{port}',
                '--prompt','fixture'],text=True,capture_output=True,timeout=20)
            self.assertNotEqual(run.returncode,0)
            tool_port=int((root/'trial/home/tool-port').read_text())
            try:
                with socket.socket() as sock:
                    self.assertNotEqual(sock.connect_ex(('127.0.0.1',tool_port)),0,'tool descendant remains listening')
            finally:
                import signal
                try: os.kill(int((root/'trial/home/tool-pid').read_text()),signal.SIGKILL)
                except ProcessLookupError: pass


if __name__ == '__main__':
    unittest.main()
