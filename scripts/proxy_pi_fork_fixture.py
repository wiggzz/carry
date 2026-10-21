"""Pinned Pi RPC fork tracer. Synthetic provider, real client and Carry only."""
import contextlib
import hashlib
import json
import os
from pathlib import Path
import queue
import select
import subprocess
import threading
import time
import uuid
import urllib.request

try:
    from proxy_trial import client_configuration, stop_process_group
    from proxy_native_settle import wait_settled
except ImportError:
    from scripts.proxy_trial import client_configuration, stop_process_group
    from scripts.proxy_native_settle import wait_settled


class Rpc:
    def __init__(self, command, workspace, env, log):
        self.process = subprocess.Popen(command, cwd=workspace, env=env, stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, start_new_session=True)
        self.events = queue.Queue()
        self.log = log
        self.serial = 0
        def read():
            for line in self.process.stdout:
                log.write(line); log.flush()
                try: self.events.put(json.loads(line))
                except ValueError: pass
            self.events.put(None)
        self.reader = threading.Thread(target=read, daemon=True); self.reader.start()

    def wait(self, predicate):
        deadline = time.monotonic()+30
        while time.monotonic() < deadline:
            event = self.events.get(timeout=max(.001, deadline-time.monotonic()))
            assert event is not None, 'actual Pi RPC early EOF'
            if predicate(event): return event
        raise TimeoutError('actual Pi RPC deadline')

    def call(self, kind, **fields):
        self.serial += 1
        identifier = str(self.serial)
        value = dict(id=identifier, type=kind, **fields)
        self.log.write(json.dumps({'fixture_command': value})+'\n'); self.log.flush()
        self.process.stdin.write(json.dumps(value)+'\n'); self.process.stdin.flush()
        event = self.wait(lambda e: e.get('type')=='response' and e.get('id')==identifier)
        assert event.get('success'), event
        return event.get('data', {})

    def prompt(self, text):
        self.call('prompt', message=text)
        return self.wait(lambda e: e.get('type')=='agent_settled')

    def close(self):
        stop_process_group(self.process); self.reader.join(3)
        self.process.stdin.close(); self.process.stdout.close()


def path_ids(tree):
    entries = {e['id']: e for e in tree['entries']}
    leaf = tree['leafId']; path = []
    while leaf:
        entry = entries[leaf]; path.append(leaf); leaf = entry['parentId']
    return list(reversed(path))


def state_path(directory, tenant, session, branch):
    # Exact checkpoint FILE locator only, never fork authority/cut canonicalization.
    identity = json.dumps([tenant, session, branch], separators=(',', ':')).encode()
    return Path(directory)/(hashlib.sha256(identity).hexdigest()+'.json')


def settled(directory, session, branch, completed):
    path = state_path(directory, 'local-trial', session, branch)
    deadline = time.monotonic()+30
    while time.monotonic() < deadline:
        if path.exists():
            state = json.loads(path.read_text())
            if state.get('completed_requests', 0)>=completed and state.get('background_review_pending') is False:
                return state
        time.sleep(.01)
    raise TimeoutError('exact native lineage checkpoint did not settle: '+branch)


def run_case(*, carry, pi, output, fixture_class, observer_class):
    root = Path(output); root.mkdir()
    workspace = root/'workspace'; workspace.mkdir()
    home = root/'home'; home.mkdir()
    env = {k:v for k,v in os.environ.items() if not k.startswith(('OPENAI_', 'CARRY_', 'PI_', 'CODEX_'))}
    env.update(HOME=str(home), XDG_CONFIG_HOME=str(home/'.config'), CARRY_HOME=str(home/'.carry'),
        CARRY_PROXY_AUTH_TOKEN='synthetic-fork-gateway', CARRY_PROXY_UPSTREAM_KEY='synthetic-key',
        CARRY_PROXY_CLASSIFIER_KEY='synthetic-key', CARRY_TRIAL_GATEWAY_TOKEN='synthetic-fork-gateway',
        CARRY_PROXY_REVIEW_MIN_PRIMARY_TOKENS='0', CARRY_PROXY_REVIEW_MIN_NEW_TOKENS='0',
        CARRY_PROXY_REVIEW_BACKOFF_FACTOR='1', CARRY_PROXY_REVIEW_EVERY_REQUESTS='2')
    session = uuid.uuid4().hex
    state_dir = root/'state'
    rpc = proxy = None
    bridge = None
    result = {'case':'fork', 'client':'pi', 'fixture_only':True}
    with fixture_class('pi', 'fork') as fixture, contextlib.ExitStack() as stack:
        def capture():
            (root/'provider-wire.json').write_text(json.dumps({'calls':fixture.calls,
                'reviews':fixture.reviews, 'errors':fixture.errors})+'\n')
        stack.callback(capture)
        version = subprocess.run([str(pi), '--version'], env=env, capture_output=True, text=True, check=True)
        assert version.stdout.strip()=='0.84.2', version.stdout
        (root/'provenance.json').write_text(json.dumps({'pi_version':version.stdout.strip(),
            'pi_source':'914cf1472e715297caa30db4b9535d534a9eb718',
            'source_sha':os.environ.get('GITHUB_SHA'),
            'carry_sha256':hashlib.sha256(Path(carry).read_bytes()).hexdigest()})+'\n')
        def advice(body, targets, records, eligible):
            blocks = [r for r in records if r.get('group_id') in eligible]
            old = [r['group_id'] for r in blocks if 'DROP_COHORT_PAYLOAD_A' in json.dumps(r)]
            b = [r['group_id'] for r in blocks if 'B_ONLY' in json.dumps(r)]
            return {'protected':[r['group_id'] for r in blocks if 'PROTECTED_A' in json.dumps(r)],
                'removable':old, 'memories':([{'source_ids':[old[0]], 'text':'ACCEPTED_A_FACT'}] if old else [])+
                ([{'source_ids':[b[0]], 'text':'B_ONLY_FACT'}] if b else [])}
        fixture.fork_advice = advice
        command = [str(carry), 'proxy', '--verbose', '--login','api-key', '--listen','127.0.0.1:0',
            '--upstream-url',fixture.url+'/v1/responses', '--classifier-url',fixture.url+'/classifier',
            '--state-dir',str(state_dir), '--mode','compact', '--classifier-model','gpt-6-luna',
            '--classifier-cache-policy','disabled', '--payoff-requests','5', '--min-payback-percent','0']
        (root/'command.json').write_text(json.dumps(command)+'\n')
        proxy_log = stack.enter_context((root/'proxy.log').open('w'))
        proxy = subprocess.Popen(command, env=env, stdout=subprocess.PIPE, stderr=proxy_log,
            text=True, start_new_session=True)
        stack.callback(stop_process_group, proxy)
        stack.callback(proxy.stdout.close)
        assert select.select([proxy.stdout], [], [], 30)[0], 'Carry readiness'
        banner = proxy.stdout.readline().strip()
        assert banner.startswith('CARRY_PROXY_LISTEN '), banner
        url = 'http://'+banner.split(' ',1)[1]
        observer = stack.enter_context(observer_class(int(url.rsplit(':',1)[1]), root/'carry-wire.jsonl'))
        endpoint = observer.url
        # Tests-only RED uses the existing isolated launcher unchanged. The future
        # explicit bridge is the only optional seam; absence still runs real RPC.
        try:
            from pi_native_bridge import PiNativeBridge
        except ImportError:
            PiNativeBridge = None
        if PiNativeBridge:
            bridge = stack.enter_context(PiNativeBridge(endpoint, 'synthetic-fork-gateway',
                tenant='local-trial', session=session, mapping=root/'mapping.json'))
            endpoint = bridge.url
        env.update(client_configuration(root, client='pi', base_url=endpoint+'/v1',
            model='gpt-6-luna', session_id=session))
        (root/'pi/settings.json').write_text(json.dumps({'compaction':{'enabled':False}}))
        client_command = [str(pi),'--mode','rpc','--provider','carry-trial','--model','gpt-6-luna',
            '--thinking','off','--no-approve','--no-extensions','--no-skills','--no-prompt-templates',
            '--no-context-files','--session-dir',str(root/'sessions')]
        log = stack.enter_context((root/'rpc.jsonl').open('w'))
        rpc = Rpc(client_command, workspace, env, log); stack.callback(rpc.close)
        native = rpc.call('get_state')
        if bridge: bridge.resume(native['sessionId'])
        rpc.prompt('PROTECTED_A: run the scripted tools and finish A.')
        a = settled(state_dir, session, bridge.branch if bridge else 'main', 3)
        assert a.get('fork_points'), 'real first accepted unchanged raw cut must commit'
        assert any(m['text']=='ACCEPTED_A_FACT' for m in a['memories']), 'actual A advice required'
        a_tree = rpc.call('get_entries')
        if bridge: bridge.observe_boundary(native['sessionId'], a_tree)
        (root/'a-checkpoint.json').write_text(json.dumps(a)+'\n')
        rpc.prompt('B_ONLY: advance the parent and clean up A.')
        b_tree = rpc.call('get_entries'); parent_native = rpc.call('get_state')
        parent_branch = bridge.branch if bridge else 'main'
        b = settled(state_dir, session, parent_branch, 4)
        rpc.prompt('B_ONLY: verify later disposable reviewer cleanup.')
        b = settled(state_dir, session, parent_branch, 5)
        assert b['reviewer_compactions']>0, 'actual parent disposable cleanup'
        assert 'DROP_COHORT_PAYLOAD_A' not in json.dumps(fixture.reviews[-1]), 'later real review must prune A'
        parent_tree = rpc.call('get_entries')
        anchors = rpc.call('get_fork_messages')['messages']
        selected = next(x['entryId'] for x in anchors if x['text'].startswith('B_ONLY: advance'))
        selected_entry = next(e for e in parent_tree['entries'] if e['id']==selected)
        assert selected_entry['parentId']==a_tree['leafId'], 'selected B user immediately follows A'
        # Inclusive clone preserves current native leaf and all B entries.
        clone_result = rpc.call('clone'); clone_native = rpc.call('get_state'); clone_tree = rpc.call('get_entries')
        assert clone_result['cancelled'] is False
        assert clone_native['sessionId']!=parent_native['sessionId']
        assert path_ids(clone_tree)==path_ids(parent_tree), 'clone must be inclusive, not fork-before-user'
        rpc.call('switch_session', sessionPath=parent_native['sessionFile'])
        assert rpc.call('get_state')['sessionId']==parent_native['sessionId']
        fork_result = rpc.call('fork', entryId=selected)
        child_native = rpc.call('get_state'); child_tree = rpc.call('get_entries')
        assert fork_result['cancelled'] is False and fork_result['text'].startswith('B_ONLY: advance')
        assert child_native['sessionId'] not in (parent_native['sessionId'],clone_native['sessionId'])
        assert path_ids(child_tree)==path_ids(a_tree), 'fork-before-B must retain exactly the A path'
        assert selected not in path_ids(child_tree), 'off-by-one: selected B user excluded'
        # Paths are native evidence only, never credentials sent to Carry.
        header = json.loads(Path(child_native['sessionFile']).read_text().splitlines()[0])
        assert header['id']==child_native['sessionId'] and header['parentSession']==parent_native['sessionFile']
        (root/'native-operation.json').write_text(json.dumps({'parent':parent_native,
            'child':child_native,'clone':clone_native,'selected_b_user':selected,
            'a_tree':a_tree,'parent_tree':parent_tree,'clone_tree':clone_tree,
            'child_tree':child_tree,'child_header':header}, indent=2)+'\n')
        if bridge: bridge.fork(parent_native['sessionId'], child_native['sessionId'], child_tree)
        before_reviews = len(fixture.reviews)
        before_calls = len(fixture.calls)
        rpc.prompt('C_ONLY: continue from A, never B.')
        sent = fixture.calls[-1] if len(fixture.calls)>before_calls else {}
        text = json.dumps(sent.get('input', []))
        assert text.count('ACCEPTED_A_FACT')==1, 'PI_FORK_RED: actual fork A+C must inherit A fact exactly once'
        assert 'DROP_COHORT_PAYLOAD_A' not in text and 'B_ONLY' not in text
        assert 'PROTECTED_A' in text and 'C_ONLY' in text
        assert len(fixture.reviews)==before_reviews, 'no recovery review'
        assert not fixture.summary_calls, 'no native summary rescue'
        child_branch = bridge.branch if bridge else 'main'
        child = settled(state_dir, session, child_branch, 1)
        assert child['primary']['calls']==1 and child['shadow']['calls']==0
        assert child['reviewer_cache']==[] and not child['background_review_pending']
        assert json.loads(state_path(state_dir,'local-trial',session,parent_branch).read_text())==b, 'parent B unchanged'
        incoming = observer.bodies()
        assert len(incoming)==len(fixture.calls), 'both wire sides complete cardinality'
        assert (workspace/'proxy-fixture.txt').read_text()=='FIXTURE_TOOL_OK'
        result.update(native_fork_verified=True, clone_inclusive_verified=True, parent_branch=parent_branch,
            child_branch=child_branch, parent_native=parent_native, child_native=child_native,
            clone_native=clone_native, selected_b_user=selected, a_path=path_ids(a_tree),
            clone_path=path_ids(clone_tree), child_path=path_ids(child_tree),
            native_requests=len(fixture.calls), reviews=len(fixture.reviews))
        (root/'result.json').write_text(json.dumps(result, indent=2)+'\n')
    return result
