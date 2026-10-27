"""Exercise the real local trial launcher with fake executables."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class ProxyTrialTests(unittest.TestCase):
    def test_native_output_policy_cannot_exceed_known_model_bounds(self):
        from scripts import model_catalog
        from unittest.mock import patch
        model = 'gpt-6-luna'
        original = model_catalog.model_spec(model)
        for changes in ({'client_output_tokens': 128001}, {'context_tokens': 1000}):
            with self.subTest(changes=changes), patch.dict(model_catalog._CATALOG, {model: {**original, **changes}}):
                with self.assertRaises(ValueError):
                    model_catalog.pi_model(model, name='fixture')

    def test_known_legacy_pi_models_use_explicit_client_output_policy_not_a_fabricated_maximum(self):
        from scripts import proxy_trial
        catalog = json.loads((ROOT / 'src/model_catalog.json').read_text())
        for model, spec in catalog.items():
            if spec['max_output_tokens'] is not None:
                continue
            with self.subTest(model=model), tempfile.TemporaryDirectory() as directory:
                try:
                    proxy_trial.client_configuration(Path(directory), client='pi',
                        base_url='http://127.0.0.1:1/v1', model=model, session_id=None)
                except ValueError as error:
                    self.fail(f'previously supported catalog model rejected: {error}')
                row = json.loads((Path(directory) / 'pi/models.json').read_text())['providers']['carry-trial']['models'][0]
                self.assertEqual(row['contextWindow'], spec['context_tokens'])
                self.assertEqual(row['maxTokens'], 128000)
                self.assertIsNone(spec['max_output_tokens'])

    def test_native_trial_and_benchmark_share_all_known_catalog_entries(self):
        from scripts import proxy_trial, swebench_smoke
        for model in ('gpt-6-luna', 'gpt-6-sol', 'gpt-6.1-sol', 'gpt-6-astra'):
            with self.subTest(model=model), tempfile.TemporaryDirectory() as directory:
                proxy_trial.client_configuration(Path(directory), client='pi',
                    base_url='http://127.0.0.1:1/v1', model=model, session_id=None)
                row = json.loads((Path(directory)/'pi/models.json').read_text())['providers']['carry-trial']['models'][0]
                self.assertEqual(row['contextWindow'], 1050000)
                self.assertEqual(row['maxTokens'], 128000)
                self.assertIsNotNone(swebench_smoke.pricing_for_model(model))
                self.assertEqual(swebench_smoke.pricing_for_model(model)['long_context_threshold'], 272000)
        self.assertIsNone(swebench_smoke.pricing_for_model('unknown-model'))
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(ValueError):
                proxy_trial.client_configuration(Path(directory), client='pi',
                    base_url='http://127.0.0.1:1/v1', model='unknown-model', session_id=None)

    def test_fixture_observer_rejects_remote_tls_and_credential_urls_before_side_effects(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); workspace=root/'workspace'; workspace.mkdir()
            for url in ('http://example.com/v1','https://127.0.0.1:123/v1',
                        'http://user:secret@127.0.0.1:123/v1','http://127.0.0.1:123/wrong'):
                run=subprocess.run(['python3',str(ROOT/'scripts/proxy_trial.py'),
                    '--client','codex','--workspace',str(workspace),'--trial-dir',str(root/'trial'),
                    '--prompt','fixture','--fixture-client-base-url',url],
                    capture_output=True,text=True,timeout=5)
                self.assertNotEqual(run.returncode,0)
                self.assertIn('fixture client observer must be',run.stderr)
                self.assertFalse((root/'trial').exists())

    def test_launcher_explicit_api_key_auth_even_with_saved_subscription_and_no_key(self):
        # Execute main through its subprocess/HTTP boundaries, never real auth or HTTP.
        import io
        from unittest.mock import patch
        from scripts import proxy_trial
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ordinary = root / 'ordinary'; ordinary.mkdir()
            carry_home = ordinary / '.carry'; carry_home.mkdir()
            saved = carry_home / 'auth.json'
            saved.write_text('{"fixture_only":"saved subscription must not select auth"}')
            workspace = root / 'workspace'; workspace.mkdir()
            trial = root / 'trial'
            launches = []
            class Process:
                def poll(self): return None
                def wait(self, timeout=None): return 0
            def launch(argv, **kwargs):
                launches.append((argv, kwargs['env'].copy()))
                return Process()
            class Response(io.BytesIO):
                status = 200
            def get(request, **kwargs):
                return Response(b'{"mode":"off"}')
            env = {'HOME': str(ordinary), 'CARRY_HOME': str(carry_home),
                   'XDG_CONFIG_HOME': str(ordinary / '.config')}
            argv = ['trial', '--client', 'codex', '--workspace', str(workspace),
                    '--trial-dir', str(trial), '--prompt', 'synthetic task',
                    '--fixture-client-base-url', 'http://127.0.0.1:12345/v1']
            with patch.dict(os.environ, env, clear=True), patch('sys.argv', argv), \
                 patch.object(proxy_trial.subprocess, 'Popen', side_effect=launch), \
                 patch.object(proxy_trial.urllib.request, 'urlopen', side_effect=get), \
                 patch.object(proxy_trial, 'stop_process_group'):
                self.assertEqual(proxy_trial.main(), 0)
            self.assertEqual(len(launches), 2)
            import tomllib
            config = tomllib.loads((trial/'codex/config.toml').read_text())
            self.assertEqual(config['model_providers']['carry-trial']['base_url'],
                             'http://127.0.0.1:12345/v1')
            proxy_argv, proxy_env = launches[0]
            self.assertIn('--verbose', proxy_argv)
            self.assertIn('--login', proxy_argv)
            self.assertEqual(proxy_argv[proxy_argv.index('--login') + 1], 'api-key')
            self.assertEqual(proxy_argv[proxy_argv.index('--mode') + 1], 'off')
            self.assertNotIn('--background-reviews=false', proxy_argv)
            self.assertEqual(proxy_env['HOME'], str(ordinary))
            self.assertNotIn('OPENAI_API_KEY', proxy_env)
            self.assertNotIn('CARRY_PROXY_UPSTREAM_KEY', proxy_env)
            self.assertEqual(saved.read_text(), '{"fixture_only":"saved subscription must not select auth"}')
            _, client_env = launches[1]
            self.assertEqual(client_env['HOME'], str(trial / 'home'))
            self.assertNotIn('CARRY_PROXY_AUTH_TOKEN', client_env)
            self.assertEqual(client_env['CARRY_TRIAL_GATEWAY_TOKEN'], proxy_env['CARRY_PROXY_AUTH_TOKEN'])
            self.assertNotIn(proxy_env['CARRY_PROXY_AUTH_TOKEN'], proxy_argv)

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
