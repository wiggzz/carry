"""Owned-process adapter regressions; no Docker, no inherited home mutation."""
import ctypes
import json
import os
import pathlib
import signal
import subprocess
import sys
import tempfile
import time
import unittest

ENTRYPOINT = pathlib.Path(__file__).parents[1]/'containers/swebench-harness/entrypoint.py'


class DeadlineProcessTests(unittest.TestCase):
    def exercise(self, cooperative):
        # Catch/reap baseline orphans too: never leave fixture processes behind.
        self.assertEqual(ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0), 0)
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory); repo=root/'repo'; repo.mkdir(); (root/'home').mkdir()
            for command in (['init','-q'], ['config','user.name','Test'], ['config','user.email','test@example.com']):
                subprocess.run(['git','-C',str(repo),*command],check=True)
            (repo/'file').write_text('before\n')
            subprocess.run(['git','-C',str(repo),'add','.'],check=True)
            subprocess.run(['git','-C',str(repo),'commit','-qm','base'],check=True)
            (root/'prompt').write_text('synthetic')
            writer = root/'writer.py'
            writer.write_text("import pathlib,time,os\n"
                "pathlib.Path('writer.pid').write_text(str(os.getpid()))\n"
                "while True:\n time.sleep(.05)\n pathlib.Path('file').write_text(str(time.monotonic()))\n")
            agent=root/'agent.py'
            agent.write_text("import subprocess,sys,signal,time,pathlib,json\n"
                f"child=subprocess.Popen([sys.executable,{str(writer)!r}],start_new_session=True)\n"
                "def stop(*_):\n time.sleep(.3)\n pathlib.Path('drained').write_text('yes')\n sys.exit(0)\n"
                + ("signal.signal(signal.SIGUSR1,stop)\n" if cooperative else "signal.signal(signal.SIGUSR1,signal.SIG_IGN)\n")
                + "while True: time.sleep(.05)\n")
            env = dict(os.environ, HOME=str(root/'home'), OPENAI_API_KEY='synthetic-only',
                OPENAI_BASE_URL='http://127.0.0.1:1/v1', BENCHMARK_WORKSPACE=str(repo),
                AGENT_COMMAND=f'{sys.executable} {agent}', AGENT_TIMEOUT_SECONDS='1', AGENT_SHUTDOWN_GRACE_SECONDS='1')
            pid=None
            try:
                run=subprocess.run([sys.executable,str(ENTRYPOINT),'run','--harness','carry','--model','gpt-6-sol',
                    '--reasoning','medium','--prompt',str(root/'prompt'),'--output',str(root/'output')],
                    env=env,capture_output=True,text=True,timeout=8)
                self.assertEqual(run.returncode,124,run.stderr)
                pid=int((repo/'writer.pid').read_text())
                self.assertFalse(pathlib.Path(f'/proc/{pid}').exists(), 'owned separate-session writer survived patch capture')
                before=(repo/'file').read_bytes(); time.sleep(.15)
                self.assertEqual((repo/'file').read_bytes(),before,'patch raced a live writer')
                self.assertEqual((repo/'drained').exists(),cooperative)
                self.assertTrue((root/'output/final.patch').exists())
            finally:
                if pid is None and (repo/'writer.pid').exists(): pid=int((repo/'writer.pid').read_text())
                if pid:
                    try: os.kill(pid,signal.SIGKILL)
                    except ProcessLookupError: pass
                    try: os.waitpid(pid,0)
                    except ChildProcessError: pass
                    self.assertFalse(pathlib.Path(f'/proc/{pid}').exists(),'fixture cleanup failed')

    def test_cooperative_deadline_reaps_separate_group_before_patch(self): self.exercise(True)
    def test_forced_deadline_reaps_separate_group_before_patch(self): self.exercise(False)


if __name__=='__main__': unittest.main()
