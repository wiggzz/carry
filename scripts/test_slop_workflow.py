"""Execute the actual workflow shell; no source-presence assertions."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import yaml

ROOT=Path(__file__).resolve().parents[1]


def workflow_step(name=None, job='bootstrap-worker', step_id=None):
    workflow=yaml.safe_load((ROOT/'.github/workflows/run-swebench.yml').read_text())
    return next(s for s in workflow['jobs'][job]['steps'] if
                (s.get('name')==name if name else s.get('id')==step_id))['run']


class WorkflowContracts(unittest.TestCase):
    def test_slop_attempts_bound_to_three_other_modes_preserved(self):
        script=workflow_step(job='plan-attempts',step_id='plan')
        with tempfile.TemporaryDirectory() as tmp:
            output=Path(tmp)/'output'
            for mode,count,valid in [('slop-2','3',True),('slop-2','4',False),('slop-2','0',False),
                                     ('prepare-slop-2','2',False),('official-50','10',True),('smoke-5','2',False)]:
                output.write_text('')
                result=subprocess.run(['bash','-c',script],cwd=ROOT,env=dict(os.environ,MODE=mode,ATTEMPTS=count,
                                      GITHUB_OUTPUT=str(output)),capture_output=True,text=True)
                self.assertEqual(result.returncode==0,valid,(mode,count,result.stderr))
                if valid:self.assertEqual(json.loads(output.read_text().splitlines()[0].split('=',1)[1])['attempt'],list(range(1,int(count)+1)))

    def test_slop_report_uses_its_eight_slot_gate_not_official_swe_task_ids(self):
        from scripts import slopbench as s
        import tarfile
        script=workflow_step(name='Validate fixed-denominator results')
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);(root/'scripts').symlink_to(ROOT/'scripts',target_is_directory=True)
            (root/'benchmarks').symlink_to(ROOT/'benchmarks',target_is_directory=True)
            assets=root/'assets';manifest=json.loads(s.MANIFEST.read_text())
            for project in manifest['projects']:
                problem=assets/project['name'];problem.mkdir(parents=True)
                for n in range(1,5):(problem/f'checkpoint_{n}.md').write_text(f'published-{n}')
                for asset in project['static_assets']:(problem/asset).mkdir()
            def agent(**kw):
                session=kw['output'];usage=dict(s.swe.empty_usage(),input_tokens=1,output_tokens=1,total_tokens=2)
                tools=[{'name':name,'parameters':{'properties':{'context':{'properties':
                    {k:{} for k in ('protected','removable','remember')}}}}} for name in ('shell','finish')]
                config={'model':'gpt-6-luna','reasoning_effort':'medium','compaction_policy':'disabled','max_steps':None}
                request={'model':'gpt-6-luna','reasoning':{'effort':'medium'},'tools':tools}
                events=[{'event':'run_started' if kw['checkpoint']==1 else 'session_resumed','data':config},
                        {'event':'model_request','data':{'step':0,'request':request}},
                        {'event':'model_response','data':{'step':0,'response_id':'resp_1','usage':usage}},
                        {'event':'run_finished','data':{}}]
                (session/'trace.jsonl').write_text('\n'.join(map(json.dumps,events))+'\n')
                (session/'proxy.log').write_text('BENCHMARK_PROXY_RESPONSE '+json.dumps({'response_id':'resp_1','input_tokens':1,'output_tokens':1,'cached_input_tokens':0,'cache_write_input_tokens':0})+'\n')
                s.dump(session/s.STATE_FILE,{'version':1,'model':'gpt-6-luna','context':{'generation':0},'prompt_cache_key':kw['project']['name']})
                return dict(s.stage_usage(session),agent_status='complete')
            def grader(*args,**kwargs):return {'status':'graded','resolved':False,'counts':{'passed':1,'failed':1,'skipped':0,'error':0}}
            output=root/'payload'
            records=s.run_trajectories(manifest,assets,root/'work',output,1,agent=agent,grader=grader)
            s.finalize(records,output,source=ROOT,attempt=1,attempts=1,catalog={'image':'image'})
            (output/'worker-exit-status').write_text('0\n')
            archive=root/'benchmark-artifact';archive.mkdir()
            with tarfile.open(archive/'results.tar.gz','w:gz') as stream:
                for p in output.iterdir():stream.add(p,arcname=p.name)
            env=dict(os.environ,BENCHMARK_MODE='slop-2',BENCHMARK_HARNESS='carry',BENCHMARK_ATTEMPT='1',BENCHMARK_ATTEMPTS='1')
            result=subprocess.run(['bash','-c',script],cwd=root,env=env,capture_output=True,text=True)
            self.assertEqual(result.returncode,0,result.stderr)
            (output/'file_backup/stage-2/workspace-before.json').write_text('{}\n')
            with self.assertRaises(ValueError):s.validate_report(output,source=ROOT)
            (output/'file_backup/stage-2/workspace-before.json').write_bytes((output/'file_backup/stage-1/workspace-after.json').read_bytes())
            (output/'file_backup/stage-2/session/trace.jsonl').write_text((output/'file_backup/stage-2/session/trace.jsonl').read_text().replace('"medium"','"high"'))
            with self.assertRaises(ValueError):s.validate_report(output,source=ROOT)
            report=json.loads((output/'report.json').read_text());report['compaction_policy']='economic';s.dump(output/'report.json',report)
            with tarfile.open(archive/'results.tar.gz','w:gz') as stream:
                for p in output.iterdir():stream.add(p,arcname=p.name)
            result=subprocess.run(['bash','-c',script],cwd=root,env=env,capture_output=True,text=True)
            self.assertNotEqual(result.returncode,0)


class WorkerContracts(unittest.TestCase):
    def test_paid_slop_worker_preflight_precedes_key_and_forwards_attempt(self):
        import base64
        import hashlib
        import tarfile
        import sys
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);payload=root/'payload';(payload/'scripts').mkdir(parents=True)
            (payload/'scripts/slopbench.py').write_text('not executed: fake CLI boundary\n')
            archive=root/'source.tar.gz'
            with tarfile.open(archive,'w:gz') as stream:stream.add(payload/'scripts',arcname='scripts')
            fake=root/'bin';fake.mkdir();log=root/'calls.jsonl'
            for name in ('dnf','systemctl'):
                (fake/name).write_text('#!/bin/sh\nexit 0\n');(fake/name).chmod(0o755)
            curl=fake/'curl';curl.write_text('#!'+sys.executable+'\nimport os,sys,pathlib,shutil\n'
                "a=sys.argv;output=pathlib.Path(a[a.index('-o')+1])\n"
                "if output.name=='source.tar.gz':shutil.copy2(os.environ['ARCHIVE'],output)\n"
                "else:output.write_text('fake-secret')\n")
            curl.chmod(0o755)
            python=fake/'python3';python.write_text('#!'+sys.executable+'\nimport os,sys,json,subprocess\n'
                "a=sys.argv[1:]\n"
                "if a==['-']:raise SystemExit(subprocess.run([os.environ['REAL_PYTHON'],'-'],stdin=sys.stdin).returncode)\n"
                "if a[0].endswith('docker_registry_login.py'):raise SystemExit(0)\n"
                "if a[0].endswith('slopbench.py'):\n"
                " with open(os.environ['CALL_LOG'],'a') as f:f.write(json.dumps({'argv':a,'key_present':bool(os.environ.get('OPENAI_API_KEY')),'attempt':os.environ.get('BENCHMARK_ATTEMPT'),'catalog':os.environ.get('TASK_IMAGE_CATALOG')})+'\\n')\n"
                " raise SystemExit(0)\n"
                "raise SystemExit(90)\n")
            python.chmod(0o755)
            capability=base64.b64encode(b'https://example.invalid/object').decode()
            env=dict(os.environ,PATH=str(fake)+':'+os.environ['PATH'],ARCHIVE=str(archive),CALL_LOG=str(log),
                REAL_PYTHON=sys.executable,CARRY_ROOT=str(root/'worker'),SECRET_FILE=str(root/'secret'),
                DOCKER_AUTH_FILE=str(root/'docker-auth'),REGISTRY_AUTH_FILE=str(root/'registry'),DOCKER_CONFIG=str(root/'docker-config'),
                SOURCE_URL_B64=capability,SOURCE_SHA256=hashlib.sha256(archive.read_bytes()).hexdigest(),SOURCE_COMMIT='a'*40,
                KEY_URL_B64=capability,DOCKER_AUTH_URL_B64=capability,CONTROL_URL_B64='',RESULT_URL_B64='',
                BENCHMARK_MODE='slop-2',BENCHMARK_HARNESS='carry',BENCHMARK_ATTEMPT='2',BENCHMARK_ATTEMPTS='3',
                CARRY_COMPACTION_POLICY='disabled',MODEL='gpt-6-luna',REASONING='medium',
                TASK_IMAGE_REPOSITORY='registry/task',TASK_IMAGE_CATALOG='registry/task@sha256:'+'a'*64,
                RUN_ID='gh-test-1-attempt-2',BOOTSTRAP_WAIT_SECONDS='1',PYTHON_BIN=str(python),SKIP_SHUTDOWN='1')
            env.pop('OPENAI_API_KEY',None)
            run=subprocess.run(['bash',str(ROOT/'scripts/swebench_ec2_worker.sh')],env=env,capture_output=True,text=True,timeout=30)
            self.assertEqual(run.returncode,0,run.stderr+'\n'+run.stdout)
            calls=[json.loads(line) for line in log.read_text().splitlines()]
            self.assertEqual([c['argv'][1] for c in calls],['preflight','run'])
            self.assertEqual([c['key_present'] for c in calls],[False,True])
            self.assertTrue(all(c['attempt']=='2' and c['catalog']==env['TASK_IMAGE_CATALOG'] for c in calls))
            self.assertFalse((root/'secret').exists())


if __name__=='__main__':unittest.main()
