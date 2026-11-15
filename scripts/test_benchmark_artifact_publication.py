"""Behavioral publisher controls using owned harmless files; no Docker/model calls."""
import pathlib,shutil,subprocess,tempfile,unittest
from unittest import mock
from scripts import swebench_smoke as swe

class ArtifactPublication(unittest.TestCase):
    def test_readonly_rejected_agent_tree_cannot_be_followed_by_publisher(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=pathlib.Path(tmp);out=root/'published';out.mkdir()
            sentinel=root/'host-only';sentinel.write_text('HARMLESS_HOST_ONLY_SENTINEL')
            actor_paths=[]
            def actor(argv,**kwargs):
                mounts=[str(x) for x in argv if isinstance(x,str) and x.endswith(',dst=/benchmark/output')]
                self.assertEqual(len(mounts),1)
                raw=pathlib.Path(mounts[0].split('src=',1)[1].rsplit(',dst=',1)[0]);actor_paths.append(raw)
                nested=raw/'nested';nested.mkdir(parents=True);(nested/'trace.jsonl').symlink_to(sentinel);nested.chmod(0o555)
                return subprocess.CompletedProcess(argv,0)
            try:
                with mock.patch.dict('os.environ',{'OPENAI_API_KEY':'owned-scoped-fixture'}),mock.patch.object(swe.subprocess,'run',side_effect=actor):
                    with self.assertRaises(swe.AgentArtifactError):
                        swe.run_agent(instance_id='owned-task',harness='codex',image='owned:fixture',repo=root,harness_bundle=root,task_input=root,output=out,model='owned-model',reasoning='low',network='owned-network',proxy_ip='127.0.0.1')
                # Exercise an actual following publisher, not a source-string assertion.
                exported=root/'uploaded';shutil.copytree(out,exported,symlinks=False)
                contents=[p.read_bytes() for p in exported.rglob('*') if p.is_file()]
                self.assertFalse(any(sentinel.read_bytes() in value for value in contents),'Rejected agent symlink leaked owned host sentinel through publisher')
                self.assertTrue(actor_paths)
            finally:
                for raw in actor_paths:
                    nested=raw/'nested'
                    if nested.exists():nested.chmod(0o755)

    def test_regular_agent_files_are_published_as_owned_copies(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=pathlib.Path(tmp);out=root/'published';out.mkdir();seen=[]
            def actor(argv,**kwargs):
                mount=next(str(x) for x in argv if isinstance(x,str) and x.endswith(',dst=/benchmark/output'))
                raw=pathlib.Path(mount.split('src=',1)[1].rsplit(',dst=',1)[0]);seen.append(raw)
                self.assertEqual(raw.stat().st_mode & 0o777, 0o777)
                self.assertEqual(raw.parent.stat().st_mode & 0o777, 0o700)
                (raw/'final.patch').write_text('owned patch bytes')
                (raw/'nested').mkdir();(raw/'nested'/'trace.jsonl').write_text('owned trace bytes')
                return subprocess.CompletedProcess(argv,0)
            with mock.patch.dict('os.environ',{'OPENAI_API_KEY':'owned-scoped-fixture'}),mock.patch.object(swe.subprocess,'run',side_effect=actor):
                result=swe.run_agent(instance_id='owned-task',harness='codex',image='owned:fixture',repo=root,harness_bundle=root,task_input=root,output=out,model='owned-model',reasoning='low',network='owned-network',proxy_ip='127.0.0.1')
            self.assertEqual(result['patch'],'owned patch bytes');self.assertEqual((out/'nested'/'trace.jsonl').read_text(),'owned trace bytes')
            self.assertNotEqual(seen[0],out);self.assertFalse(seen[0].is_relative_to(out));self.assertFalse(any(p.is_symlink() for p in out.rglob('*')))

    def test_publish_reopens_files_no_follow_after_validation(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=pathlib.Path(tmp);raw=root/'raw';raw.mkdir();out=root/'published';sentinel=root/'host-only';sentinel.write_text('HARMLESS_HOST_ONLY_SENTINEL');trace=raw/'trace';trace.write_text('ordinary agent bytes')
            original=swe.agent_artifact_bytes
            def swap(source,relative,**kwargs):
                trace.unlink();trace.symlink_to(sentinel)
                return original(source,relative,**kwargs)
            with mock.patch.object(swe,'agent_artifact_bytes',side_effect=swap):
                with self.assertRaises(swe.AgentArtifactError):swe.publish_agent_artifacts(raw,out)
            self.assertFalse(any(sentinel.read_bytes() in p.read_bytes() for p in out.rglob('*') if p.is_file()))

    def test_selected_publication_refuses_links_and_byte_overflow(self):
        for kind in ('link', 'byte_overflow'):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as tmp:
                root=pathlib.Path(tmp);raw=root/'raw';raw.mkdir();out=root/'published'
                sentinel=root/'host-only';sentinel.write_text('HARMLESS_HOST_ONLY_SENTINEL')
                if kind=='link':(raw/'selected').symlink_to(sentinel)
                else:(raw/'selected').write_bytes(b'owned bytes exceed a tiny declared bound')
                with mock.patch.object(swe,'AGENT_ARTIFACT_TOTAL_BYTES',4):
                    with self.assertRaises(swe.AgentArtifactError):
                        swe.publish_agent_artifacts(raw,out,paths=[pathlib.Path('selected')])
                self.assertEqual(list(out.rglob('*')),[])
                self.assertEqual(sentinel.read_text(),'HARMLESS_HOST_ONLY_SENTINEL')

    def test_early_budget_rejection_after_failed_worker_keeps_raw_tree_private(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=pathlib.Path(tmp);out=root/'published';out.mkdir();sentinel=root/'host-only';sentinel.write_text('HARMLESS_HOST_ONLY_SENTINEL')
            def actor(argv,**kwargs):
                mount=next(str(x) for x in argv if isinstance(x,str) and x.endswith(',dst=/benchmark/output'))
                raw=pathlib.Path(mount.split('src=',1)[1].rsplit(',dst=',1)[0]);(raw/'a').mkdir();(raw/'b').mkdir();first,last=list(raw.iterdir());(first/'oversized').write_bytes(b'x'*17);(last/'trace').symlink_to(sentinel)
                raise subprocess.CalledProcessError(1,argv)
            with mock.patch.dict('os.environ',{'OPENAI_API_KEY':'owned-scoped-fixture'}),mock.patch.object(swe,'AGENT_ARTIFACT_MAX_BYTES',16),mock.patch.object(swe.subprocess,'run',side_effect=actor):
                with self.assertRaises(swe.AgentArtifactError):swe.run_agent(instance_id='owned-task',harness='codex',image='owned:fixture',repo=root,harness_bundle=root,task_input=root,output=out,model='owned-model',reasoning='low',network='owned-network',proxy_ip='127.0.0.1')
            self.assertEqual(list(out.iterdir()),[])

    def test_native_trace_collection_uses_owned_view_and_restores_original(self):
        from scripts import slopcodebench_runtime as runtime
        with tempfile.TemporaryDirectory() as tmp:
            root=pathlib.Path(tmp);raw=root/'trace';raw.mkdir();(raw/'ok.jsonl').write_text('owned native trace');dest=root/'copy';dest.mkdir()
            class Agent:
                _trace_dir=raw
                def collect(self,target):
                    self.seen=self._trace_dir
                    # Actual following copy behavior of pinned native helper.
                    for path in self._trace_dir.rglob('*.jsonl'):shutil.copy(path,target/path.name)
                    return 'native result unchanged'
            agent=Agent();wrapped=runtime.owned_codex_trace_method(Agent.collect)
            self.assertEqual(wrapped(agent,dest),'native result unchanged');self.assertEqual((dest/'ok.jsonl').read_text(),'owned native trace');self.assertEqual(agent._trace_dir,raw);self.assertNotEqual(agent.seen,raw)

    def test_native_trace_symlink_never_reaches_following_copy(self):
        from scripts import slopcodebench_runtime as runtime

        with tempfile.TemporaryDirectory() as tmp:
            root=pathlib.Path(tmp);raw=root/'trace';raw.mkdir();sentinel=root/'host-only';sentinel.write_text('HARMLESS_HOST_ONLY_SENTINEL');(raw/'bad.jsonl').symlink_to(sentinel);dest=root/'copy';dest.mkdir();calls=[]
            class Agent:
                _trace_dir=raw
                def collect(self,target):
                    calls.append(True)
                    for path in self._trace_dir.rglob('*.jsonl'):shutil.copy(path,target/path.name)
            agent=Agent()
            with self.assertRaises(swe.AgentArtifactError):runtime.owned_codex_trace_method(Agent.collect)(agent,dest)
            self.assertEqual(calls,[]);self.assertEqual(list(dest.iterdir()),[]);self.assertEqual(agent._trace_dir,raw)

if __name__=='__main__':unittest.main(verbosity=2)
