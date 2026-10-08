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
        for client in ('codex', 'pi'):
            with self.subTest(client=client), tempfile.TemporaryDirectory() as directory:
                root = Path(directory); workspace = root / 'workspace'; workspace.mkdir()
                carry = root / 'carry'
                carry.write_text('''#!/usr/bin/env python3
import http.server,json,os,sys
assert os.environ.get('CARRY_PROXY_UPSTREAM_KEY') == 'provider-private'
assert '--mode' in sys.argv and sys.argv[sys.argv.index('--mode')+1] == 'compact'
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
                           CARRY_PROXY_CLASSIFIER_KEY='provider-private')
                result = subprocess.run(['python3', str(ROOT / 'scripts/proxy_trial.py'),
                    '--client', client, '--carry-binary', str(carry), '--client-binary', str(binary),
                    '--workspace', str(workspace), '--trial-dir', str(trial), '--mode', 'compact',
                    '--listen', '127.0.0.1:' + str(port), '--prompt', 'fixture'],
                    env=env, text=True, capture_output=True, timeout=20)
                self.assertEqual(result.returncode, 0, result.stderr)
                evidence = json.loads((trial / 'home/client-evidence.json').read_text())
                self.assertNotEqual(evidence['home'], os.environ['HOME'])
                self.assertNotIn('provider-private', ''.join(p.read_text() for p in trial.rglob('*') if p.is_file()))
                if client == 'codex':
                    import tomllib
                    config = tomllib.loads((trial / 'codex/config.toml').read_text())
                    provider = config['model_providers']['carry-trial']
                    self.assertEqual(provider['wire_api'], 'responses')
                    self.assertTrue(provider['http_headers']['x-carry-session'])
                    self.assertFalse(provider.get('supports_websockets', True))
                else:
                    config = json.loads((trial / 'pi/models.json').read_text())
                    self.assertEqual(config['providers']['carry-trial']['api'], 'openai-responses')
                    self.assertTrue(config['providers']['carry-trial']['headers']['x-carry-session'])
                with socket.socket() as sock:
                    self.assertNotEqual(sock.connect_ex(('127.0.0.1',port)), 0, 'owned proxy not cleaned')


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
