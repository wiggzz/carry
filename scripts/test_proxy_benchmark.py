"""Behavioral contracts for opt-in trusted proxy benchmark lanes."""
import importlib.util
import pathlib
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('worker', ROOT / 'scripts/swebench_smoke.py')
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class ProxyBenchmarkTests(unittest.TestCase):
    def test_classifier_cache_policy_rejects_unsupported_inputs_before_launch(self):
        proxy = worker.proxy_benchmark
        for policy in ('', 'caller-decides', 'OPENAI-EXPLICIT'):
            with self.subTest(policy=policy), self.assertRaisesRegex(ValueError, 'CACHE_POLICY'):
                proxy.validate_config({'CARRY_PROXY_CLASSIFIER_CACHE_POLICY': policy})
        with self.assertRaisesRegex(ValueError, 'supported classifier model'):
            proxy.validate_config({'CARRY_PROXY_CLASSIFIER_CACHE_POLICY': 'openai-explicit',
                                   'CARRY_PROXY_CLASSIFIER_MODEL': 'generic-compatible-model'})
        for policy in ('disabled', 'auto'):
            config = proxy.validate_config({'CARRY_PROXY_CLASSIFIER_CACHE_POLICY': policy,
                                            'CARRY_PROXY_CLASSIFIER_MODEL': 'generic-compatible-model'})
            self.assertEqual(config['CARRY_PROXY_CLASSIFIER_CACHE_POLICY'], policy)

    def test_trusted_sidecar_records_and_forwards_explicit_classifier_cache_policy(self):
        proxy = worker.proxy_benchmark
        values = {'BENCHMARK_HARNESS': 'pi', 'CARRY_PROXY_MODE': 'compact',
                  'CARRY_PROXY_CLASSIFIER_CACHE_POLICY': 'openai-explicit'}
        config = proxy.validate_config(values)
        self.assertEqual(config.get('CARRY_PROXY_CLASSIFIER_CACHE_POLICY'), 'openai-explicit')
        self.assertEqual(proxy.provenance(values).get('classifier_cache_policy'), 'openai-explicit')
        command = proxy.sidecar_command(image='fixed-image', name='fixture', network='fixed',
            state_dir=pathlib.Path('/nonsecret-fixture'), config=config)
        self.assertIn('--classifier-cache-policy', command)
        self.assertEqual(command[command.index('--classifier-cache-policy') + 1], 'openai-explicit')

    def test_opt_in_config_survives_runner_validation(self):
        config = worker.validate_config(dict(BASE_IMAGE='rust@sha256:' + 'a' * 64,
            CODEX_VERSION='0.147.0', PI_VERSION='0.84.2', MODEL='gpt-6-luna', REASONING='medium',
            CARRY_PROXY_MODE='compact', CARRY_PROXY_HISTORY_POLICY='reset-on-divergence', CARRY_PROXY_CLASSIFIER_MODEL='gpt-6-luna',
            CARRY_PROXY_CLASSIFIER_EFFORT='low', CARRY_PROXY_PAYOFF_REQUESTS='5',
            CARRY_PROXY_MIN_PAYBACK_PERCENT='3', BENCHMARK_HARNESS='codex'))
        self.assertEqual(config.get('CARRY_PROXY_MODE'), 'compact')
        self.assertEqual(config.get('CARRY_PROXY_HISTORY_POLICY'), 'reset-on-divergence')
        self.assertEqual(config.get('CARRY_PROXY_MIN_PAYBACK_PERCENT'), '3')


    def test_trusted_sidecar_gets_provider_key_and_client_gets_only_scoped_token(self):
        import os
        from unittest.mock import patch
        calls = []
        def docker(command, **kwargs):
            calls.append((command, kwargs))
            if command[1] == 'exec':
                return subprocess.CompletedProcess(command, 0, '{"mode":"compact"}', '')
            if command[1:4] == ['inspect', '--type', 'container'] or command[1:3] == ['network', 'inspect']:
                return subprocess.CompletedProcess(command, 1, '', 'No such container')
            return subprocess.CompletedProcess(command, 0, '172.20.0.2\n' if '--format' in command else '', '')
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {
                'CARRY_PROXY_MODE': 'compact', 'CARRY_PROXY_HISTORY_POLICY':'reset-on-divergence', 'BENCHMARK_HARNESS': 'pi',
                'OPENAI_API_KEY': 'provider-private'}):
            network = worker.start_agent_network(identity='one-slot', proxy_image='node-image',
                proxy_script=ROOT / 'scripts/openai_proxy.js', execute=docker,
                carry_image='sha256:' + 'a' * 64, state_dir=pathlib.Path(directory) / 'trusted')
            sidecars = [(cmd, kw) for cmd, kw in calls if 'proxy' in cmd and '--mode' in cmd]
            self.assertEqual(len(sidecars), 1)
            command, kwargs = sidecars[0]
            self.assertEqual(kwargs['env'].get('CARRY_PROXY_UPSTREAM_KEY'), 'provider-private')
            self.assertNotIn('provider-private', command)
            self.assertIn('--classifier-reasoning-effort', command)
            self.assertEqual(command[command.index('--user')+1], f'{os.getuid()}:{os.getgid()}')
            gateway = next((kw for cmd, kw in calls if '/proxy/openai_proxy.js' in cmd), {})
            self.assertNotIn('OPENAI_API_KEY', gateway.get('env', {}))
            self.assertEqual(gateway.get('env', {}).get('BENCHMARK_CONTEXT_HISTORY_POLICY'), 'reset-on-divergence')
            gateway_cmd = next(cmd for cmd,kw in calls if '/proxy/openai_proxy.js' in cmd)
            self.assertIn('BENCHMARK_CONTEXT_HISTORY_POLICY',gateway_cmd)
            self.assertNotEqual(network['client_token'], 'provider-private')
            self.assertTrue(network['session_id'])
            worker.cleanup_agent_network(network, execute=docker)
            self.assertTrue(any(cmd[1:3] == ['rm', '--force'] and cmd[-1] == network['carry_proxy'] for cmd, _ in calls))


    def test_gateway_routes_only_to_fixed_sidecar_and_overwrites_identity(self):
        import json
        import os
        import urllib.request
        script = r'''
const http = require('node:http');
const gateway = require(process.argv[1]);
const upstream = http.createServer((req, res) => {
  let body=''; req.on('data', c => body += c);
  req.on('end', () => { res.setHeader('content-type','application/json');
    res.end(JSON.stringify({headers:req.headers, body, path:req.url})); });
});
upstream.listen(0, '127.0.0.1', () => {
  const transport = (options, callback) => http.request({...options, hostname:'127.0.0.1',
    port:upstream.address().port}, callback);
  const server = gateway.serve({port:0, host:'127.0.0.1', request:transport});
  server.on('listening', () => console.log('PORT ' + server.address().port));
});
'''
        env = dict(os.environ, BENCHMARK_CONTEXT_PROXY='1', BENCHMARK_CLIENT_TOKEN='slot-token',
                   CARRY_PROXY_AUTH_TOKEN='trusted-token', BENCHMARK_SESSION_ID='stable-session',
                   BENCHMARK_SHADOW_TOKEN='shadow-token', BENCHMARK_CLASSIFIER_KEY='classifier-private')
        process = subprocess.Popen(['node', '-e', script, str(ROOT / 'scripts/openai_proxy.js')],
            env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            # A bounded socket/read deadline prevents a broken fixture hanging CI.
            import select
            self.assertTrue(select.select([process.stdout], [], [], 5)[0], 'gateway never listened')
            line = process.stdout.readline()
            self.assertTrue(line.startswith('PORT '), line + process.stderr.read() if process.poll() is not None else line)
            base = 'http://127.0.0.1:' + line.split()[1]
            request = urllib.request.Request(base + '/v1/responses', data=b'{"input":[]}',
                headers={'Authorization':'Bearer slot-token', 'x-carry-session':'spoofed'})
            with urllib.request.urlopen(request, timeout=3) as response:
                value = json.load(response)
            self.assertEqual(value['headers']['authorization'], 'Bearer trusted-token')
            self.assertEqual(value['headers']['x-carry-session'], 'stable-session')
            self.assertEqual(value['body'], '{"input":[]}')
            shadow = urllib.request.Request(base + '/v1/responses', data=b'{"model":"gpt-6-luna"}',
                headers={'Authorization':'Bearer shadow-token'})
            with urllib.request.urlopen(shadow, timeout=3) as response:
                shadow_value = json.load(response)
            self.assertEqual(shadow_value['headers']['authorization'], 'Bearer classifier-private')
            self.assertNotIn('x-carry-session', shadow_value['headers'])
            for path, token, status in [('/v1/responses', 'wrong', 401), ('/v1/files', 'slot-token', 403),
                                       ('/carry/metrics', 'slot-token', 403)]:
                with self.assertRaises(urllib.error.HTTPError) as error:
                    urllib.request.urlopen(urllib.request.Request(base + path, data=b'{}',
                        headers={'Authorization':'Bearer ' + token}), timeout=3)
                self.assertEqual(error.exception.code, status)
                error.exception.close()
        finally:
            process.terminate()
            stdout, stderr = process.communicate(timeout=5)
        events = [json.loads(line.split(' ', 1)[1]) for line in stdout.splitlines()
                  if line.startswith('BENCHMARK_CONTEXT_EVENT ')]
        self.assertEqual({e['actor'] for e in events}, {'primary', 'shadow'})
        self.assertEqual(sum(e['event'] == 'started' for e in events), 2)
        self.assertEqual(sum(e['event'] == 'censored' for e in events), 2)
        self.assertNotIn('classifier-private', stdout)


    def test_production_agent_receives_scoped_key_not_provider_key(self):
        import os
        from unittest.mock import patch
        received = []
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / 'final.patch').write_text('')
            (root / 'trace.log').write_text('')
            def docker(command, **kwargs):
                received.append((command, kwargs))
                return subprocess.CompletedProcess(command, 0, '', '')
            with patch.dict(os.environ, {'OPENAI_API_KEY':'real-provider-key'}), \
                    patch.object(worker.subprocess, 'run', side_effect=docker):
                worker.run_agent(instance_id='test', harness='pi', image='image', repo=root,
                    harness_bundle=root, task_input=root, output=root, model='gpt-6-luna', reasoning='medium',
                    network='internal', proxy_ip='172.20.0.2', api_base='http://openai-proxy:8080/v1',
                    client_token='scoped-slot-token')
            launch = next(kw for cmd, kw in received if cmd[:2] == ['docker', 'run'])
            self.assertEqual(launch.get('env', {}).get('OPENAI_API_KEY'), 'scoped-slot-token')
            self.assertNotIn('real-provider-key', repr(launch))


    def test_native_cost_rejects_missing_cache_partition(self):
        usage = {'input_tokens': 100, 'output_tokens': 10,
                 'input_tokens_details': {}}
        self.assertIsNone(worker.proxy_benchmark.native_cost('gpt-6-luna', usage))

    def test_native_cost_rejects_unpriced_output_audio(self):
        usage = {'input_tokens': 100, 'output_tokens': 10,
                 'input_tokens_details': {'cached_tokens': 0},
                 'output_tokens_details': {'reasoning_tokens': 2, 'audio_tokens': 4}}
        self.assertIsNone(worker.proxy_benchmark.native_cost('gpt-6-luna', usage))

    def test_summary_separates_native_costs_and_censors_unanswered_primary(self):
        events = [
            {'actor':'primary', 'event':'started', 'request_id':'p1'},
            {'actor':'shadow', 'event':'started', 'request_id':'s1'},
            {'actor':'shadow', 'event':'completed', 'request_id':'s1', 'model':'gpt-6-luna',
             'latency_ms':12, 'usage':{'input_tokens':100, 'output_tokens':10,
              'input_tokens_details':{'cached_tokens':40, 'cache_write_tokens':20}}},
        ]
        summary = worker.proxy_benchmark.summarize_events(events, metrics={'mode':'compact', 'rewrites':1})
        self.assertEqual(summary['primary']['censored_requests'], 1)
        self.assertIsNone(summary['estimated_total_cost_usd'])
        self.assertEqual(summary['shadow']['ordinary_input_tokens'], 40)
        self.assertEqual(summary['shadow']['cached_input_tokens'], 40)
        self.assertEqual(summary['shadow']['cache_write_input_tokens'], 20)
        self.assertAlmostEqual(summary['shadow']['observed_cost_usd'], 0.0000119)
        self.assertEqual(summary['shadow']['latency_ms'], 12)
        self.assertEqual(summary['proxy_rewrites'], 1)
        self.assertIsNone(summary['native_compactions'])


    def test_isolated_lane_retains_proxy_evidence_before_exact_cleanup(self):
        import os
        from unittest.mock import patch
        order = []
        def launch(**kwargs):
            self.assertEqual(kwargs.get('carry_image'), 'sha256:' + 'a' * 64)
            state = kwargs['state_dir']; state.mkdir()
            return dict(internal='i', proxy='gateway', carry_proxy='sidecar', egress='e',
                        proxy_ip='172.20.0.2', api_base='http://openai-proxy:8080/v1',
                        client_token='slot', session_id='fresh', state_dir=str(state))
        def capture(network, **kwargs):
            order.append('capture')
            return {'effective_mode':'compact', 'estimated_total_cost_usd':None,
                    'primary':{'estimated_cost_usd':0.05}}
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {
                'CARRY_PROXY_MODE':'compact', 'BENCHMARK_HARNESS':'pi'}):
            root = pathlib.Path(directory); output = root / 'agent'; output.mkdir()
            with patch.object(worker, 'start_agent_network', side_effect=launch), \
                    patch.object(worker, 'run_agent', return_value={'usage':{}, 'estimated_cost_usd':1}), \
                    patch.object(worker.proxy_benchmark, 'capture_evidence', side_effect=capture, create=True), \
                    patch.object(worker, 'cleanup_agent_network', side_effect=lambda n:order.append('cleanup')):
                record = worker.run_isolated_agent(instance_id='task', harness='pi', image='task-image',
                    harness_bundle=root, proxy_image='node-image', proxy_script=ROOT / 'scripts/openai_proxy.js',
                    repo=root, task_input=root, output=output, model='gpt-6-luna', reasoning='medium',
                    carry_image='sha256:' + 'a' * 64)
            self.assertEqual(order, ['capture', 'cleanup'])
            self.assertIsNone(record['estimated_cost_usd'])
            self.assertEqual(record['primary_estimated_cost_usd'],0.05)
            self.assertEqual(record['client_estimated_cost_usd'],1)
            self.assertEqual(record['proxy_summary']['effective_mode'], 'compact')
            self.assertFalse((output / 'proxy-state').exists())


    def test_capture_reads_actual_gateway_ledger_and_saves_numeric_artifact(self):
        import json
        from unittest.mock import patch
        events = [dict(actor='primary', event='started', request_id='p'),
                  dict(actor='shadow', event='started', request_id='s')]
        def docker(command, **kwargs):
            if command[1] == 'logs':
                return subprocess.CompletedProcess(command, 0,
                    ''.join('BENCHMARK_CONTEXT_EVENT ' + json.dumps(e) + '\n' for e in events), '')
            return subprocess.CompletedProcess(command, 0, '{"mode":"off","compactions":7}', '')
        with tempfile.TemporaryDirectory() as directory:
            network = dict(proxy='gateway', state_dir=directory)
            summary = worker.proxy_benchmark.capture_evidence(network, execute=docker)
            self.assertEqual(summary['primary']['censored_requests'], 1)
            self.assertEqual(summary['shadow']['censored_requests'], 1)
            self.assertEqual(summary['effective_mode'], 'off')
            self.assertEqual(summary['proxy_rewrites'], 7)
            self.assertEqual(json.loads((pathlib.Path(directory) / 'benchmark-summary.json').read_text()), summary)
            self.assertEqual(len((pathlib.Path(directory) / 'benchmark-events.jsonl').read_text().splitlines()), 2)


    def test_legacy_direct_and_proxy_off_have_distinct_merge_identities(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location('merger', ROOT / 'scripts/merge_swebench_attempts.py')
        merger = importlib.util.module_from_spec(spec); spec.loader.exec_module(merger)
        direct = merger.normalize_legacy_provenance({})
        off = merger.normalize_legacy_provenance({'proxy':{'mode':'off'}})
        self.assertEqual(direct.get('proxy', {}).get('mode'), 'disabled')
        self.assertNotEqual(direct['proxy'], off['proxy'])
        self.assertIn('proxy', merger.IMMUTABLE_PROVENANCE_FIELDS)


    def test_readiness_rejects_effective_mode_drift_before_agent_launch(self):
        import os
        from unittest.mock import patch
        calls=[]
        def docker(command, **kwargs):
            calls.append(command)
            if command[1] == 'exec':
                return subprocess.CompletedProcess(command, 0, '{"mode":"off"}', '')
            if command[1:4] == ['inspect','--type','container'] or command[1:3] == ['network','inspect']:
                return subprocess.CompletedProcess(command, 1, '', 'No such container')
            return subprocess.CompletedProcess(command,0,'172.20.0.2\n' if '--format' in command else '', '')
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {
                'CARRY_PROXY_MODE':'compact','BENCHMARK_HARNESS':'pi','OPENAI_API_KEY':'private'}):
            with self.assertRaisesRegex(RuntimeError, 'effective mode'):
                worker.start_agent_network(identity='drift', proxy_image='node',
                    proxy_script=ROOT / 'scripts/openai_proxy.js', carry_image='sha256:'+'a'*64,
                    state_dir=pathlib.Path(directory)/'trusted', execute=docker)
        self.assertTrue(any(command[1:3] == ['rm','--force'] for command in calls))


    def test_finalizer_does_not_present_partial_proxy_cost_as_total(self):
        import json
        with tempfile.TemporaryDirectory() as directory:
            output=pathlib.Path(directory)
            tasks=[{'instance_id':f'task-{i}'} for i in range(5)]
            records=[{'instance_id':task['instance_id'],'harness':'pi','status':'evaluated',
                      'estimated_cost_usd':None if i == 4 else 0.1}
                     for i,task in enumerate(tasks)]
            worker.finalize(tasks=tasks,records=records,output=output,
                provenance={'proxy':{'mode':'compact'}},harnesses=('pi',))
            report=json.loads((output/'report.json').read_text())
            self.assertIsNone(report['harnesses']['pi']['estimated_cost_usd'])
            self.assertEqual(report['denominator'],5)
            self.assertAlmostEqual(report['harnesses']['pi']['observed_cost_lower_bound_usd'],0.4)


    def test_report_gate_rejects_dropped_treatment_and_effective_mode_drift(self):
        config={'BENCHMARK_HARNESS':'pi','CARRY_PROXY_MODE':'compact'}
        expected=worker.proxy_benchmark.provenance(config)
        for report,records in [({'provenance':{}},[]),
                ({'provenance':{'proxy':expected}},[{'proxy_summary':{'effective_mode':'off'}}])]:
            with self.subTest(report=report), self.assertRaises(ValueError):
                worker.proxy_benchmark.validate_report(report,records,config)
        worker.proxy_benchmark.validate_report({'provenance':{'proxy':expected}},
            [{'proxy_summary':{'effective_mode':'compact'}}],config)


    def test_configuration_rejects_values_unsupported_by_actual_proxy_cli(self):
        for key,value in [('CARRY_PROXY_PAYOFF_REQUESTS','101'),
                          ('CARRY_PROXY_MIN_PAYBACK_PERCENT','2.5')]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                worker.proxy_benchmark.validate_config({key:value})


    def test_cost_refuses_unknown_billed_categories_and_exact_model_aliases(self):
        usage={'input_tokens':10,'output_tokens':1,'input_tokens_details':{'cached_tokens':1,'mystery_billed_tokens':2}}
        self.assertIsNone(worker.proxy_benchmark.native_cost('gpt-6-luna',usage))
        self.assertIsNone(worker.proxy_benchmark.native_cost('luna',{'input_tokens':10,'output_tokens':1}))
        self.assertIsNone(worker.proxy_benchmark.native_cost('gpt-6-luna',{'input_tokens':10,'output_tokens':1},service_tier='priority'))


    def test_unpriced_completed_usage_retains_native_tokens_and_latency(self):
        events=[{'actor':'shadow','request_id':'one','event':'started'},
                {'actor':'shadow','request_id':'one','event':'completed','model':'unknown',
                 'latency_ms':17,'usage':{'input_tokens':100,'output_tokens':2,
                    'input_tokens_details':{'cached_tokens':10,'cache_write_tokens':20}}}]
        result=worker.proxy_benchmark.summarize_events(events)
        self.assertIsNone(result['estimated_total_cost_usd'])
        self.assertEqual(result['shadow']['ordinary_input_tokens'],70)
        self.assertEqual(result['shadow']['latency_ms'],17)


    def test_gateway_records_effective_native_model_tier_and_fragmented_usage(self):
        import json, os, select, urllib.request
        script=r'''
const http=require('node:http'); const gateway=require(process.argv[1]);
const upstream=http.createServer((req,res)=>{ req.resume(); req.on('end',()=>{
 const value={status:'completed',model:'gpt-6.1-sol',service_tier:'priority',
 output:[{type:'function_call',name:'bash',call_id:'a',arguments:'{}'}],
 usage:{input_tokens:100,output_tokens:5,input_tokens_details:{cached_tokens:20,cache_write_tokens:10}}};
 const body=req.headers.authorization === 'Bearer classifier-private'
 ? JSON.stringify(value,null,2) : 'event: response.completed\ndata: '+JSON.stringify({type:'response.completed',response:value})+'\n\n';
 res.writeHead(200,{'content-type':'text/event-stream'});
 for(let i=0;i<body.length;i+=7) res.write(body.slice(i,i+7)); res.end();
 });});
upstream.listen(0,'127.0.0.1',()=>{ const transport=(opts,cb)=>http.request({...opts,hostname:'127.0.0.1',port:upstream.address().port},cb);
 const server=gateway.serve({port:0,host:'127.0.0.1',request:transport});
 server.on('listening',()=>console.log('PORT '+server.address().port)); });
'''
        env=dict(os.environ,BENCHMARK_CONTEXT_PROXY='1',BENCHMARK_CLIENT_TOKEN='slot-token',
            BENCHMARK_SHADOW_TOKEN='shadow-token',BENCHMARK_CLASSIFIER_KEY='classifier-private',
            CARRY_PROXY_AUTH_TOKEN='trusted-token',BENCHMARK_SESSION_ID='fixed')
        process=subprocess.Popen(['node','-e',script,str(ROOT/'scripts/openai_proxy.js')],
            env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        try:
            self.assertTrue(select.select([process.stdout],[],[],5)[0])
            port=process.stdout.readline().split()[1]
            for token in ('slot-token','shadow-token'):
                req=urllib.request.Request('http://127.0.0.1:'+port+'/v1/responses',
                    data=b'{"model":"gpt-6-luna","service_tier":"default"}',
                    headers={'Authorization':'Bearer '+token})
                with urllib.request.urlopen(req,timeout=3) as response: response.read()
        finally:
            process.terminate(); stdout,stderr=process.communicate(timeout=5)
        events=[json.loads(s.split(' ',1)[1]) for s in stdout.splitlines() if s.startswith('BENCHMARK_CONTEXT_EVENT ')]
        completed=[e for e in events if e['event']=='completed']
        self.assertEqual(len(completed),2)
        for event in completed:
            self.assertEqual(event['model'],'gpt-6.1-sol')
            self.assertEqual(event['service_tier'],'priority')
            self.assertEqual(event['usage']['input_tokens'],100)
            self.assertEqual(event.get('tool_calls'),1)


    def test_active_proxy_cleanup_fails_closed_when_daemon_cannot_prove_absence(self):
        from unittest.mock import patch
        network={'proxy':'gateway-one','carry_proxy':'carry-one','internal':'inside-one','egress':'outside-one'}
        calls=[]
        def docker(command,**kwargs):
            calls.append(command)
            return subprocess.CompletedProcess(command,1,'','Cannot connect to the Docker daemon')
        with patch.object(worker.time,'sleep'), self.assertRaises(worker.ContainerCleanupError):
            worker.cleanup_agent_network(network,execute=docker)
        self.assertEqual([c[-1] for c in calls if c[1:3]==['rm','--force']],
                         ['gateway-one','carry-one']*3)


    def test_missing_gateway_ledger_is_unknown_not_zero_cost(self):
        def docker(command,**kwargs):
            return subprocess.CompletedProcess(command,0,'' if command[1]=='logs' else '{"mode":"off"}','')
        with tempfile.TemporaryDirectory() as directory:
            summary=worker.proxy_benchmark.capture_evidence({'proxy':'gateway','state_dir':directory},execute=docker)
        self.assertIsNone(summary['estimated_total_cost_usd'])
        self.assertIsNone(summary['primary']['estimated_cost_usd'])
        self.assertIsNone(summary['shadow']['estimated_cost_usd'])


    def test_malformed_native_usage_remains_an_unpriced_completed_request(self):
        events=[{'actor':'primary','request_id':'one','event':'started'},
                {'actor':'primary','request_id':'one','event':'completed','model':'gpt-6-luna','usage':[]}]
        summary=worker.proxy_benchmark.summarize_events(events)
        self.assertIsNone(summary['estimated_total_cost_usd'])
        self.assertEqual(summary['primary']['unpriced_requests'],1)


class HistoryPolicyContractTests(unittest.TestCase):
    def test_only_absent_legacy_classifier_cache_policy_normalizes_to_disabled(self):
        from scripts.merge_swebench_attempts import normalize_legacy_provenance
        old = worker.proxy_benchmark.provenance({'CARRY_PROXY_MODE': 'off', 'BENCHMARK_HARNESS': 'pi'})
        old.pop('classifier_cache_policy')
        normalized = normalize_legacy_provenance({'proxy': old})['proxy']
        self.assertEqual(normalized.get('classifier_cache_policy'), 'disabled')
        self.assertEqual({k: v for k, v in normalized.items() if k != 'classifier_cache_policy'}, old)
        for policy in ('auto', 'disabled', 'openai-explicit'):
            explicit = {**old, 'classifier_cache_policy': policy}
            self.assertEqual(normalize_legacy_provenance({'proxy': explicit})['proxy'], explicit)
        self.assertEqual(normalize_legacy_provenance({})['proxy'].get('classifier_cache_policy'), 'disabled')

    def test_validation_provenance_and_report_gate_preserve_explicit_policy(self):
        proxy=worker.proxy_benchmark
        values={'BENCHMARK_HARNESS':'pi','CARRY_PROXY_MODE':'compact',
                'CARRY_PROXY_HISTORY_POLICY':'reset-on-divergence'}
        self.assertEqual(proxy.provenance(values)['history_policy'],'reset-on-divergence')
        self.assertEqual(proxy.provenance({})['history_policy'],'strict')
        for invalid in ('','caller-decides','STRICT'):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                proxy.validate_config({**values,'CARRY_PROXY_HISTORY_POLICY':invalid})
        strict=proxy.provenance({**values,'CARRY_PROXY_HISTORY_POLICY':'strict'})
        with self.assertRaisesRegex(ValueError,'dispatched'):
            proxy.validate_report({'provenance':{'proxy':strict}},[],values)


if __name__ == '__main__':
    unittest.main()
