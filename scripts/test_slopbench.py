"""Executable contracts for the continuous-workspace Slop pilot."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class SnapshotAdapterTests(unittest.TestCase):
    def test_native_adapter_runs_without_git_objects_and_preserves_workspace(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            repo = root / 'repo'; repo.mkdir()
            binary = root / 'bin' / 'carry'; binary.parent.mkdir()
            binary.write_text("#!/usr/bin/env python3\nimport pathlib,sys,json\n"
                              "assert '--max-steps' not in sys.argv\n"
                              "assert sys.argv[sys.argv.index('--compaction-policy')+1]=='disabled'\n"
                              "assert '--no-model-context-management' not in sys.argv\n"
                              "p=pathlib.Path('sentinel');p.write_text(p.read_text()+'x' if p.exists() else 'x')\n")
            binary.chmod(0o755)
            prompt = root / 'task.md'; prompt.write_text('immutable')
            env = dict(os.environ, OPENAI_API_KEY='fake', OPENAI_BASE_URL='http://fake/v1',
                       PREPARED_HARNESS_ROOT=str(root), BENCHMARK_WORKSPACE=str(repo),
                       CARRY_COMPACTION_POLICY='disabled', HOME=str(root))
            for n in range(2):
                proc = subprocess.run([sys.executable, str(ROOT/'containers/swebench-harness/entrypoint.py'),
                    'run', '--harness', 'carry', '--model', 'gpt-6-luna', '--reasoning', 'medium',
                    '--prompt', str(prompt), '--output', str(root/f'out{n}'), '--snapshot-only'],
                    cwd=repo, env=env, capture_output=True, text=True)
                self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertEqual((repo/'sentinel').read_text(), 'xx')
            self.assertFalse((repo/'.git').exists())


class GradeTests(unittest.TestCase):
    def test_real_pytest_reports_failure_and_does_not_accept_collection_only(self):
        from scripts import slopbench as s
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); problem = root/'problem'; (problem/'tests').mkdir(parents=True)
            repo = root/'repo'; repo.mkdir()
            (problem/'tests/conftest.py').write_text("def pytest_addoption(parser):\n"
                " parser.addoption('--entrypoint');parser.addoption('--checkpoint')\n")
            (problem/'tests/test_checkpoint_1.py').write_text('def test_real_failure(): assert False\n')
            result = s.grade_snapshot(problem, repo, root/'grade', 'main', 1, 2, [],
                                      python=sys.executable)
            self.assertEqual(result['status'], 'graded')
            self.assertEqual(result['counts']['failed'], 1)
            self.assertFalse(result['resolved'])
            (problem/'tests/test_checkpoint_1.py').write_text('import nonexistent_slop_dependency\n')
            result = s.grade_snapshot(problem, repo, root/'grade-error', 'main', 1, 2, [],
                                      python=sys.executable)
            self.assertEqual(result['status'], 'evaluator-incomplete')

    def test_grade_copies_cumulative_canonical_fixtures_without_mutating_workspace(self):
        from scripts import slopbench as s
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); problem = root/'problem'; (problem/'tests').mkdir(parents=True)
            repo = root/'repo'; repo.mkdir(); (repo/'sentinel').write_text('untouched')
            (problem/'tests/conftest.py').write_text('')
            for n in range(1, 5):
                (problem/f'tests/test_checkpoint_{n}.py').write_text(f'def test_{n}(): pass\n')
            prepared = s.prepare_grade(problem, repo, root/'grade', 2, [])
            self.assertEqual(sorted(p.name for p in (prepared/'.evaluation_tests').glob('test*.py')),
                             ['test_checkpoint_1.py', 'test_checkpoint_2.py'])
            self.assertFalse((repo/'.evaluation_tests').exists())
            self.assertEqual((repo/'sentinel').read_text(), 'untouched')
            command = s.pytest_command(prepared, 'backup_scheduler', 2, 20)
            self.assertIn('--checkpoint=checkpoint_2', command)
            self.assertIn('--timeout=20', command)


class ContinuousProtocolTests(unittest.TestCase):
    def test_two_fresh_projects_four_native_resumes_ordinary_failure_continues(self):
        from scripts import slopbench as s
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); assets = root/'assets'; calls = []
            manifest = json.loads(s.MANIFEST.read_text())
            for project in manifest['projects']:
                problem = assets/project['name']; problem.mkdir(parents=True)
                for n in range(1, 5): (problem/f'checkpoint_{n}.md').write_text(f'SPEC-{n}')
                for asset in project['static_assets']:
                    (problem/asset).mkdir(); (problem/asset/'static.txt').write_text('static')
            def agent(**kw):
                repo, output, source = kw['workspace'], kw['output'], kw['resume_session']
                n = kw['checkpoint']; calls.append((repo, source, output))
                self.assertEqual((repo/'counter').read_text() if (repo/'counter').exists() else '', 'x'*(n-1))
                if source:
                    self.assertEqual(json.loads((source/s.STATE_FILE).read_text())['context']['stage'], n-1)
                self.assertFalse(any(repo.rglob('checkpoint_*.md')))
                self.assertFalse((repo/'.git').exists())
                (repo/'counter').write_text('x'*n)
                s.dump(output/s.STATE_FILE, {'version':1, 'model':'gpt-6-luna', 'prompt_cache_key':str(repo),
                                             'context':{'stage':n,'generation':0}})
                return {'agent_status':'complete', 'usage':s.swe.empty_usage(), 'metering_complete':True,
                        'estimated_cost_usd':0.0, 'compactions':0}
            def grader(*args, **kw):
                n = args[4]
                return {'status':'graded','resolved':n != 2,'counts':{'passed':1,'failed':int(n==2),'error':0,'skipped':0}}
            records = s.run_trajectories(manifest, assets, root/'work', root/'out', 1,
                                         agent=agent, grader=grader)
            self.assertEqual(len(records), 8)
            self.assertEqual(len({c[0] for c in calls}), 2)
            self.assertEqual(len({c[2] for c in calls}), 8)
            self.assertEqual(sum(c[1] is None for c in calls), 2)
            self.assertEqual(sum(r['resolved'] for r in records), 6)
            for project in manifest['projects']:
                selected = [r for r in records if r['project']==project['name']]
                self.assertEqual(selected[2]['source_session_state_sha256'], selected[1]['session_state_sha256'])
            self.assertEqual(len({r['prompt_cache_key'] for r in records}), 2)
            for project in manifest['projects']:
                selected=[r for r in records if r['project']==project['name']]
                self.assertEqual(len({r['workspace_key_sha256'] for r in selected}),1)
                for index,record in enumerate(selected):
                    stage=root/'out'/project['name']/f"stage-{record['checkpoint']}"
                    before=json.loads((stage/'workspace-before.json').read_text())
                    after=json.loads((stage/'workspace-after.json').read_text())
                    self.assertEqual(s.sha256(stage/'workspace-before.json'),record['workspace_before_sha256'])
                    self.assertEqual(s.sha256(stage/'workspace-after.json'),record['workspace_after_sha256'])
                    files={v['path']:v for v in after['entries'] if v['type']=='file'}
                    self.assertEqual(files['counter']['sha256'],s.sha256(stage/'workspace/counter'))
                    self.assertEqual(record['trace_creation_code'],int(index>0))
                    provenance=json.loads((stage/'stage-provenance.json').read_text())
                    self.assertEqual(provenance['workspace_after_sha256'],record['workspace_after_sha256'])
                    if index:self.assertEqual(record['workspace_before_sha256'],selected[index-1]['workspace_after_sha256'])

    def test_incomplete_native_stage_preserves_every_future_denominator_slot(self):
        from scripts import slopbench as s
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); assets=root/'assets'; manifest=json.loads(s.MANIFEST.read_text())
            for project in manifest['projects']:
                problem=assets/project['name'];problem.mkdir(parents=True)
                for n in range(1,5): (problem/f'checkpoint_{n}.md').write_text('spec')
                for asset in project['static_assets']: (problem/asset).mkdir()
            def agent(**kw):
                return {'agent_status':'incomplete','usage':s.swe.empty_usage(),'metering_complete':True,
                        'estimated_cost_usd':0.1,'observed_response_cost_lower_bound_usd':0.1,'compactions':0}
            def grader(*args, **kw):
                return {'status':'graded','resolved':False,'counts':{'passed':0,'failed':1,'error':0,'skipped':0}}
            records=s.run_trajectories(manifest,assets,root/'work',root/'out',1,agent=agent,grader=grader)
            self.assertEqual(len(records),8)
            self.assertEqual(sum(r['status']=='blocked' for r in records),6)
            self.assertTrue(all(r['estimated_cost_usd'] is None for r in records))


class TelemetryTests(unittest.TestCase):
    def test_stage_usage_is_per_response_not_cumulative_and_censoring_is_explicit(self):
        from scripts import slopbench as s
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp); usage=dict(s.swe.empty_usage(),input_tokens=100,output_tokens=20,total_tokens=120)
            request={'event':'model_request','data':{'step':1,'request':{'prompt_cache_key':'k','model':'gpt-6-luna','tools':[{'name':'retain'}]}}}
            response={'event':'model_response','data':{'step':1,'response_id':'resp_1','usage':usage}}
            (root/'trace.jsonl').write_text('\n'.join(json.dumps(e) for e in [request,response])+'\n')
            s.dump(root/'result.json',{'usage':dict(usage,input_tokens=900)})
            proxy='BENCHMARK_PROXY_RESPONSE '+json.dumps({'response_id':'resp_1','input_tokens':100,'output_tokens':20,'cached_input_tokens':0,'cache_write_input_tokens':0})+'\n'
            (root/'proxy.log').write_text(proxy)
            measured=s.stage_usage(root)
            self.assertEqual(measured['usage']['input_tokens'],100)
            self.assertTrue(measured['metering_complete'])
            self.assertIsNotNone(measured['estimated_cost_usd'])
            with (root/'trace.jsonl').open('a') as f:f.write(json.dumps({'event':'model_request','data':{'step':2}})+'\n')
            censored=s.stage_usage(root)
            self.assertFalse(censored['metering_complete'])
            self.assertIsNone(censored['estimated_cost_usd'])
            self.assertEqual(censored['unanswered_steps'],[2])
            self.assertGreater(censored['observed_response_cost_lower_bound_usd'],0)
            with (root/'trace.jsonl').open('a') as f:f.write('{"unfinished":')
            truncated=s.stage_usage(root)
            self.assertFalse(truncated['metering_complete'])
            self.assertTrue(truncated['trace_incomplete'])
            self.assertEqual(truncated['usage']['input_tokens'],100)
            (root/'trace.jsonl').write_text('\n'.join(json.dumps(e) for e in [request,{**response,'data':{**response['data'],'response_retries':1}}])+'\n')
            retried=s.stage_usage(root)
            self.assertFalse(retried['metering_complete'])
            self.assertEqual(retried['response_retries'],1)

    def test_proxy_retains_response_identity_without_prompt_payload(self):
        command="const p=require(process.argv[1]);console.log(JSON.stringify(p.usageRecords(process.argv[2],true)))"
        body=json.dumps({'id':'resp_a','output':[{'text':'not telemetry'}],'usage':{'input_tokens':11,'output_tokens':3}})
        run=subprocess.run(['node','-e',command,str(ROOT/'scripts/openai_proxy.js'),body],capture_output=True,text=True,check=True)
        data=json.loads(run.stdout)
        self.assertEqual(data[0]['response_id'],'resp_a')
        self.assertNotIn('output',data[0])


class PreparationTests(unittest.TestCase):
    def test_model_free_preflight_records_and_pins_the_actual_native_bundle(self):
        from scripts import slopbench as s
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);bundle=root/'bundle';(bundle/'bin').mkdir(parents=True)
            (bundle/'bin/adapter').write_text('actual-fixture-adapter')
            (bundle/'bin/carry').write_text('actual-fixture-binary')
            assets=root/'assets';assets.mkdir()
            manifest=json.loads(s.MANIFEST.read_text())
            catalog={'schema':'carry.slop-task-catalog.v1',**s.preparation_identity(),
                'image':'registry/task@sha256:'+'a'*64,
                'reference_checks':[{'project':p['name'],'checkpoint':n,'resolved':True,'status':'graded'}
                    for p in manifest['projects'] for n in range(1,5)],
                'negative_checks':[{'project':p['name'],'resolved':False,'status':'graded','counts':{'failed':1}}
                    for p in manifest['projects']]}
            images={'carry':{'image_id':'sha256:'+'b'*64,'package_version':'fixture-source'}}
            work=root/'work';work.mkdir();output=root/'output';output.mkdir()
            env=dict(os.environ);env.pop('OPENAI_API_KEY',None)
            with patch.dict(os.environ,env,clear=True),patch.object(s.swe,'load_task_catalog_image',return_value=catalog),\
                 patch.object(s.swe,'build_images',return_value=images),\
                 patch.object(s.swe,'export_harness_bundles',return_value={'carry':bundle}),\
                 patch.object(s,'fetch_assets',return_value=assets),patch.object(s.subprocess,'run'):
                ready=s.preflight(ROOT,work,output,'registry/task','registry/task@sha256:'+'c'*64)
            self.assertEqual(ready['carry_binary_sha256'],s.sha256(bundle/'bin/carry'))
            self.assertEqual(json.loads((output/'preflight.json').read_text())['harness_images'],images)
            self.assertEqual(ready['adapter_sha256'],s.sha256(bundle/'bin/adapter'))

    def test_catalog_recipe_compatibility_is_independent_of_harness_source(self):
        from scripts import slopbench as s
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp); (root/'containers/slopbench').mkdir(parents=True)
            for name in ('Dockerfile','requirements.txt'):
                (root/'containers/slopbench'/name).write_text((ROOT/'containers/slopbench'/name).read_text())
            (root/'benchmarks').mkdir(); (root/'benchmarks/slop-continuous-2.json').write_bytes(s.MANIFEST.read_bytes())
            identity=s.preparation_identity(root)
            catalog={'schema':'carry.slop-task-catalog.v1',**identity,'image':'registry/task@sha256:'+'a'*64,
                     'reference_checks':[{'project':p['name'],'checkpoint':n,'resolved':True,'status':'graded'}
                       for p in json.loads(s.MANIFEST.read_text())['projects'] for n in range(1,5)],
                     'negative_checks':[{'project':p['name'],'resolved':False,'status':'graded','counts':{'failed':1}}
                       for p in json.loads(s.MANIFEST.read_text())['projects']]}
            s.validate_catalog(catalog,root)
            (root/'unrelated-harness.py').write_text('new source')
            self.assertEqual(s.preparation_identity(root),identity)
            (root/'containers/slopbench/requirements.txt').write_text('different dependencies')
            with self.assertRaises(ValueError):s.validate_catalog(catalog,root)

    def test_exact_tar_digest_checked_before_any_extraction(self):
        from scripts import slopbench as s
        import tarfile
        import io
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp); archive=root/'input.tar.gz'
            with tarfile.open(archive,'w:gz') as stream:
                info=tarfile.TarInfo('../escape');info.size=1;stream.addfile(info,io.BytesIO(b'x'))
            with self.assertRaises(ValueError):s.extract_pinned_archive(archive,root/'out','b'*64,'repo-commit')
            self.assertFalse((root/'out').exists())
            with self.assertRaises(ValueError):s.extract_pinned_archive(archive,root/'out',s.sha256(archive),'repo-commit')
            self.assertFalse((root.parent/'escape').exists())


class DockerBoundaryTests(unittest.TestCase):
    def test_stage_launch_uses_existing_isolation_and_explicit_600_second_budget(self):
        from scripts import slopbench as s
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp); output=root/'session';output.mkdir();repo=root/'repo';repo.mkdir()
            task=root/'input';task.mkdir();(task/'task.md').write_text('prompt')
            calls=[]
            def execute(command, **kwargs):
                calls.append((command,kwargs))
                if command[:2]==['docker','run']:
                    usage=dict(s.swe.empty_usage(),input_tokens=1,output_tokens=1,total_tokens=2)
                    events=[{'event':'model_request','data':{'step':0}},
                            {'event':'model_response','data':{'step':0,'response_id':'r','usage':usage}}]
                    (output/'trace.jsonl').write_text('\n'.join(map(json.dumps,events))+'\n')
                    s.dump(output/'result.json',{'completed':True})
                log='BENCHMARK_PROXY_RESPONSE '+json.dumps({'response_id':'r','input_tokens':1,'output_tokens':1,'cached_input_tokens':0,'cache_write_input_tokens':0})+'\n'
                return subprocess.CompletedProcess(command,0,stdout=log,stderr='')
            network={'internal':'isolated','proxy_ip':'172.20.0.2','proxy':'proxy','api_base':'http://openai-proxy:8080/v1'}
            with patch.object(s.swe,'start_agent_network',return_value=network), patch.object(s.swe,'cleanup_agent_network'), patch.object(s.swe,'force_remove_container') as cleanup:
                result=s.run_stage(project={'name':'file_backup'},checkpoint=1,workspace=repo,output=output,
                    task_input=task,resume_session=None,image='image',bundle=root,source=ROOT,execute=execute)
            command,kw=next(c for c in calls if c[0][:2]==['docker','run'])
            self.assertIn('AGENT_TIMEOUT_SECONDS=600',command)
            self.assertEqual(kw['timeout'],645)
            self.assertIn('--snapshot-only',command)
            self.assertIn('--dns',command)
            self.assertIn('127.0.0.1',command)
            self.assertNotIn('--max-steps',command)
            self.assertEqual(result['agent_status'],'complete')
            cleanup.assert_called_once()


class ReportGateTests(unittest.TestCase):
    def test_report_keeps_censored_slots_and_never_prices_missing_cost(self):
        from scripts import slopbench as s
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            records=[{'project':p['name'],'checkpoint':n,'attempt':1,'status':'graded','resolved':True,
                      'usage':s.swe.empty_usage(),'metering_complete':True,'estimated_cost_usd':0.1}
                     for p in json.loads(s.MANIFEST.read_text())['projects'] for n in range(1,5)]
            records[0].update(status='incomplete',metering_complete=False,estimated_cost_usd=None)
            report=s.finalize(records,root,source=ROOT,attempt=1,attempts=1,catalog={'image':'image'})
            self.assertEqual(report['denominator'],8)
            self.assertEqual(report['trajectories'],2)
            self.assertIsNone(report['estimated_cost_usd'])
            self.assertEqual(report['phase'],'incomplete')
            with self.assertRaises(ValueError):s.validate_report(root,source=ROOT,attempt=1,attempts=1)
            with self.assertRaises(ValueError):s.finalize(records[:-1],root,source=ROOT,attempt=1,attempts=1,catalog={})


if __name__ == '__main__':
    unittest.main()
