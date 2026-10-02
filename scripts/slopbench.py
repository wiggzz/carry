#!/usr/bin/env python3
"""Frozen SlopCodeBench continuous-workspace/native-conversation pilot.

The upstream pytest files/fixtures are authoritative; this is NOT the published
conversation-reset benchmark. Trusted assets never enter the agent image/mounts.
"""
from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import urllib.request

try:
    from scripts import swebench_smoke as swe
except ModuleNotFoundError:
    import swebench_smoke as swe

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / 'benchmarks/slop-continuous-2.json'
STATE_FILE = 'context-state.json'
BASE_IMAGE = 'node@sha256:afff6d8c97964a438d2e6a9c96509367e45d8bf93f790ad561a1eaea926303d9'


def preparation_identity(source: Path = ROOT) -> dict:
    recipe = {name: sha256(source/'containers/slopbench'/name)
              for name in ('Dockerfile','requirements.txt')}
    recipe['base_image']=BASE_IMAGE
    return {'protocol_task_sha256':sha256(source/'benchmarks/slop-continuous-2.json'),
            'prepared_recipe_sha256':hashlib.sha256(json.dumps(recipe,sort_keys=True).encode()).hexdigest()}


def validate_catalog(catalog: dict, source: Path = ROOT) -> None:
    if catalog.get('schema') != 'carry.slop-task-catalog.v1':
        raise ValueError('not a Slop task catalog')
    if any(catalog.get(k)!=v for k,v in preparation_identity(source).items()):
        raise ValueError('protocol/task or prepared dependency recipe changed; prepare once for this identity')
    if not swe.DIGEST_IMAGE.fullmatch(catalog.get('image','')):
        raise ValueError('Slop image must be immutable')
    manifest=json.loads((source/'benchmarks/slop-continuous-2.json').read_text())
    expected={(p['name'],n) for p in manifest['projects'] for n in p['checkpoints']}
    checks=catalog.get('reference_checks',[])
    if len(checks)!=8 or {(r.get('project'),r.get('checkpoint')) for r in checks}!=expected:
        raise ValueError('reference gate must cover all eight canonical checkpoints')
    if any(not r.get('resolved') or r.get('status')!='graded' for r in checks):
        raise ValueError('reference solutions failed')
    negative=catalog.get('negative_checks',[])
    if len(negative)!=2 or {r.get('project') for r in negative}!={p['name'] for p in manifest['projects']}:
        raise ValueError('negative controls missing')
    if any(r.get('resolved') or r.get('status')!='graded' or not r.get('counts',{}).get('failed') for r in negative):
        raise ValueError('negative control did not fail executed tests')


def extract_pinned_archive(archive: Path, destination: Path, digest: str, prefix: str) -> Path:
    if sha256(archive)!=digest:
        raise ValueError('upstream archive SHA-256 mismatch')
    with tarfile.open(archive) as stream:
        members=stream.getmembers()
        for member in members:
            path=Path(member.name)
            if path.is_absolute() or '..' in path.parts or not path.parts or path.parts[0]!=prefix:
                raise ValueError('unsafe archive path')
            if not (member.isfile() or member.isdir()):
                raise ValueError('archive links or special files forbidden')
        destination.mkdir(parents=True,exist_ok=True)
        stream.extractall(destination,members=members)
    return destination/prefix


def fetch_assets(work: Path, manifest: dict) -> Path:
    downloads=work/'assets';downloads.mkdir(parents=True,exist_ok=True)
    problems=None
    for kind in ('problems','runner'):
        pin=manifest[kind]; archive=downloads/(kind+'.tar.gz')
        if not archive.exists():
            url=f"https://codeload.github.com/{pin['repository']}/tar.gz/{pin['commit']}"
            with urllib.request.urlopen(url,timeout=90) as response, archive.open('wb') as target:
                shutil.copyfileobj(response,target)
        prefix=pin['repository'].split('/')[-1]+'-'+pin['commit']
        target=downloads/prefix
        if target.exists():
            # Always check bytes, even when a previous trusted invocation extracted.
            if sha256(archive)!=pin['archive_sha256']:raise ValueError('archive changed')
        else:
            extract_pinned_archive(archive,downloads,pin['archive_sha256'],prefix)
        if kind=='problems':problems=target
    return problems


def reference_check(assets: Path, output: Path, *, image: str | None = None,
                    source: Path = ROOT) -> dict:
    manifest=json.loads((source/'benchmarks/slop-continuous-2.json').read_text())
    references=[];negatives=[]
    for project in manifest['projects']:
        problem=assets/project['name']
        for n in project['checkpoints']:
            result=grade_snapshot(problem,problem/f'solutions/checkpoint_{n}',
                output/project['name']/f'stage-{n}',project['entry_file'],n,
                project['test_timeout_seconds'][n-1],project['static_assets'],image=image)
            result.update(project=project['name'],checkpoint=n);references.append(result)
            dump(output/'reference-tests.json',{'reference_checks':references,'negative_checks':negatives})
            print('SLOP_REFERENCE '+json.dumps({'project':project['name'],'checkpoint':n,
                'status':result['status'],'counts':result['counts']}),flush=True)
        negative=output/project['name']/'negative-workspace';negative.mkdir(parents=True)
        (negative/(project['entry_file']+'.py')).write_text('raise SystemExit(17)\n')
        result=grade_snapshot(problem,negative,output/project['name']/'negative-grade',
            project['entry_file'],4,project['test_timeout_seconds'][3],project['static_assets'],image=image)
        result.update(project=project['name']);negatives.append(result)
        dump(output/'reference-tests.json',{'reference_checks':references,'negative_checks':negatives})
    return {'reference_checks':references,'negative_checks':negatives}


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def dump(path: Path, value) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + '.tmp')
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')
    temporary.replace(path)


def copy_workspace(source: Path, destination: Path) -> Path:
    """Copy plain files only; refuse links/special files, never follow escapes."""
    destination.mkdir(parents=True, exist_ok=False)
    for item in sorted(source.rglob('*')):
        rel = item.relative_to(source)
        if item.is_symlink():
            raise ValueError(f'workspace symlink not allowed: {rel}')
        target = destination / rel
        if item.is_dir():
            target.mkdir(exist_ok=True)
        elif item.is_file():
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(item, target)
        else:
            raise ValueError(f'workspace special file not allowed: {rel}')
    return destination


def prepare_grade(problem: Path, workspace: Path, destination: Path,
                  checkpoint: int, static_assets: list[str]) -> Path:
    if checkpoint not in range(1, 5):
        raise ValueError('checkpoint must be 1-4')
    copy_workspace(workspace, destination)
    tests = destination / '.evaluation_tests'
    if tests.exists():
        shutil.rmtree(tests)
    tests.mkdir()
    for item in (problem/'tests').iterdir():
        if item.name in {'__pycache__', '.pytest_cache'}:
            continue
        match = re.fullmatch(r'test_checkpoint_(\d+)\.py', item.name)
        if match and int(match[1]) > checkpoint:
            continue
        if item.is_dir():
            shutil.copytree(item, tests/item.name)
        else:
            shutil.copy2(item, tests/item.name)
    for asset in static_assets:
        shutil.copytree(problem/asset, tests/'assets'/asset, dirs_exist_ok=True)
    (destination/'pytest.ini').write_text('[pytest]\nmarkers =\n'
        '    error: error-handling / edge-case tests\n'
        '    functionality: non-core / nice-to-have tests\n'
        '    regression: regression tests from prior checkpoints\n')
    (destination/'.scbench').mkdir(exist_ok=True)
    return destination


def pytest_command(workspace: Path, entry_file: str, checkpoint: int,
                   timeout: int, python: str = sys.executable) -> list[str]:
    """Mirror pinned PytestRunner flags, using preinstalled canonical plugins."""
    return [python, '-m', 'pytest', str(workspace/'.evaluation_tests'), f'--timeout={timeout}',
            '--entrypoint=' + shlex.join([python, str(workspace/(entry_file+'.py'))]),
            f'--checkpoint=checkpoint_{checkpoint}', '--confcutdir=.evaluation_tests',
            '--ctrf=.scbench/ctrf-report.json', '--json-report',
            '--json-report-file=.scbench/pytest-report.json',
            '--json-report-omit=traceback', '--json-report-omit=streams',
            '--json-report-omit=log', '--json-report-omit=collectors',
            '--json-report-omit=warnings', '-vv']


def grade_snapshot(problem: Path, workspace: Path, destination: Path,
                   entry_file: str, checkpoint: int, timeout: int,
                   static_assets: list[str], *, python: str = sys.executable,
                   image: str | None = None, execute=subprocess.run) -> dict:
    prepare_grade(problem, workspace, destination, checkpoint, static_assets)
    grade_root = Path('/grade') if image else destination.resolve()
    command = pytest_command(grade_root, entry_file, checkpoint, timeout,
                             '/opt/slop-venv/bin/python' if image else python)
    container = 'carry-slop-grade-' + hashlib.sha256(str(destination.resolve()).encode()).hexdigest()[:16]
    if image:
        command = ['docker', 'run', '--rm', '--name', container, '--network=none',
                   '--read-only', '--user', f'{os.getuid()}:{os.getgid()}',
                   '--cap-drop=ALL', '--security-opt=no-new-privileges',
                   '--tmpfs', '/tmp:rw,nosuid,nodev,size=512m',
                   '--mount', f'type=bind,src={destination.resolve()},dst=/grade',
                   '--workdir', '/grade', '--entrypoint', '/opt/slop-venv/bin/python',
                   image, *command[1:]]
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(('OPENAI_', 'AWS_', 'ANTHROPIC_'))}
    env['PYTEST_DISABLE_PLUGIN_AUTOLOAD'] = '1'
    # Explicit plugins prevent agent-authored pytest configuration/entry points
    # or incidental host plugins from altering the grader contract.
    idx = command.index('-m') + 2
    command[idx:idx] = ['-p', 'pytest_jsonreport.plugin', '-p', 'ctrf.main',
                      '-p', 'pytest_timeout']
    if image:
        # env must cross the Docker boundary explicitly, not merely the client.
        command[2:2] = ['--env', 'PYTEST_DISABLE_PLUGIN_AUTOLOAD=1']
    with (destination/'pytest.log').open('w') as log:
        try:
            result = execute(command, cwd=destination, env=env, stdout=log,
                             stderr=subprocess.STDOUT, timeout=600, check=False)
            code = result.returncode
        except subprocess.TimeoutExpired:
            code = 124
        finally:
            if image:
                swe.force_remove_container(container, exact_name=True)
    path = destination/'.scbench/pytest-report.json'
    raw = json.loads(path.read_text()) if path.exists() else {}
    tests = raw.get('tests', [])
    counts = {key: 0 for key in ('passed', 'failed', 'skipped', 'error')}
    for test in tests:
        outcome = test.get('outcome', 'error')
        counts[outcome if outcome in counts else 'error'] += 1
    execution_bearing = any(test.get('call', {}).get('outcome') in {'passed', 'failed'} for test in tests)
    valid = (code in {0, 1} and execution_bearing and raw.get('summary', {}).get('total') == len(tests)
             and not counts['error'] and raw.get('summary', {}).get('collected') == len(tests))
    grade = {'status': 'graded' if valid else 'evaluator-incomplete', 'returncode': code,
             'counts': counts, 'collected': raw.get('summary', {}).get('collected', 0),
             'resolved': bool(valid and code == 0 and counts['passed'] and not counts['skipped']),
             'command': command}
    dump(destination/'grade.json', grade)
    return grade


def workspace_manifest(workspace: Path) -> dict:
    """Actual complete plain tree, including agent-created caches/directories."""
    entries=[]
    for p in sorted(workspace.rglob('*')):
        record={'path':p.relative_to(workspace).as_posix()}
        if p.is_symlink():record.update(type='symlink',target=os.readlink(p))
        elif p.is_file():record.update(type='file',sha256=sha256(p),size=p.stat().st_size)
        elif p.is_dir():record['type']='directory'
        else:record['type']='special'
        entries.append(record)
    return {'schema':'carry.slop-workspace-tree.v1','entries':entries}


def manifest_bytes(manifest: dict) -> bytes:
    return (json.dumps(manifest,indent=2,sort_keys=True)+'\n').encode()


def workspace_hash(workspace: Path) -> str:
    return hashlib.sha256(manifest_bytes(workspace_manifest(workspace))).hexdigest()


def capture_workspace(workspace: Path, path: Path) -> str:
    dump(path,workspace_manifest(workspace))
    return sha256(path)


def run_trajectories(manifest: dict, assets: Path, work: Path, output: Path, attempt: int,
                     *, agent, grader=grade_snapshot) -> list[dict]:
    records = [{'project': project['name'], 'checkpoint': n, 'attempt': attempt,
                'harness':'carry', 'status':'not-run', 'resolved':False,
                'estimated_cost_usd':None, 'metering_complete':False, 'usage':swe.empty_usage()}
               for project in manifest['projects'] for n in project['checkpoints']]
    output.mkdir(parents=True, exist_ok=True)
    dump(output/'records.json', records)
    for project in manifest['projects']:
        name = project['name']; problem = assets/name
        workspace = work/name/'workspace'; workspace.mkdir(parents=True, exist_ok=False)
        for asset in project['static_assets']:
            shutil.copytree(problem/asset, workspace/asset)
        workspace_key=hashlib.sha256(json.dumps({'run_id':os.environ.get('RUN_ID'),
            'attempt':attempt,'project':name,'workspace':str(workspace.resolve())},sort_keys=True).encode()).hexdigest()
        previous = None; feedback = 'not available (first checkpoint)'; cache_key = None; blocked = False
        for record in [r for r in records if r['project']==name]:
            n = record['checkpoint']
            if blocked:
                record['status']='blocked'; record['error']='preceding stage incomplete; no reset or retry'
                dump(output/'records.json', records)
                continue
            stage = output/name/f'stage-{n}'; stage.mkdir(parents=True)
            session = stage/'session'; session.mkdir()
            task_input = work/name/f'input-{n}'; task_input.mkdir()
            spec = (problem/f'checkpoint_{n}.md').read_text()
            spec = spec.replace('%%%ENTRYPOINT:entry_file%%%', project['entry_file']+'.py').replace(
                '%%%ENTRYPOINT:entry_command%%%', 'python3 '+project['entry_file']+'.py')
            prompt = manifest['boundary_template'].format(project=name, checkpoint=n,
                entry_file=project['entry_file'], feedback=feedback, spec=spec)
            (task_input/'task.md').write_text(prompt)
            (stage/'user-message.md').write_text(prompt)
            record['user_message_sha256']=sha256(task_input/'task.md')
            record['workspace_key_sha256']=workspace_key
            record['workspace_before_sha256']=capture_workspace(workspace,stage/'workspace-before.json')
            record['source_workspace_sha256']=record['workspace_before_sha256']
            record['trace_creation_code']=int(previous is not None)
            source_hash = sha256(previous/STATE_FILE) if previous else None
            record['source_session_state_sha256']=source_hash
            try:
                metrics = agent(project=project, checkpoint=n, workspace=workspace,
                    output=session, task_input=task_input, resume_session=previous)
                record.update(metrics)
                if previous and sha256(previous/STATE_FILE) != source_hash:
                    raise RuntimeError('read-only resume source changed')
                state = json.loads((session/STATE_FILE).read_text())
                if state.get('version') != 1 or state.get('model') != manifest['model']:
                    raise ValueError('invalid native checkpoint identity')
                key = state.get('prompt_cache_key')
                if not isinstance(key,str) or not key or (cache_key is not None and key != cache_key):
                    raise ValueError('missing or changed native prompt-cache identity')
                cache_key = key
                record['session_state_sha256']=sha256(session/STATE_FILE)
                record['prompt_cache_key']=key
                record['context_generation']=state.get('context',{}).get('generation')
                if record['context_generation']!=0:
                    raise ValueError('context generation changed in disabled protocol')
                if record.get('compactions', 0) != 0:
                    raise ValueError('physical compaction in disabled protocol')
                blocked = metrics['agent_status'] != 'complete'
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                record['error']=type(error).__name__ + ': ' + str(error)
                record['agent_status']='incomplete'; blocked=True
            record['workspace_after_sha256']=capture_workspace(workspace,stage/'workspace-after.json')
            try:
                copy_workspace(workspace, stage/'workspace')
                record['workspace_sha256']=workspace_hash(stage/'workspace')
                grade = grader(problem, stage/'workspace', stage/'grade', project['entry_file'],
                    n, project['test_timeout_seconds'][n-1], project['static_assets'])
                record['grade']=grade
                record['resolved']=grade['resolved']
                blocked = blocked or grade['status'] != 'graded'
                feedback=json.dumps(grade['counts'],sort_keys=True,separators=(',',':'))
                if workspace_hash(workspace) != record['workspace_sha256']:
                    raise RuntimeError('grading mutated the ongoing workspace')
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                record['grade']={'status':'evaluator-incomplete'}
                record['error']=type(error).__name__ + ': ' + str(error);blocked=True
            record['status']='incomplete' if blocked else 'graded'
            if record.get('agent_status')!='complete':
                # A timed-out/missing native finish/state is a censored horizon,
                # even if its observed response prefix happens to pair perfectly.
                record['metering_complete']=False
            if not record.get('metering_complete'):
                record['estimated_cost_usd']=None
            record['trace_sha256']=sha256(session/'trace.jsonl') if (session/'trace.jsonl').is_file() else None
            record['session_state_sha256']=sha256(session/STATE_FILE) if (session/STATE_FILE).is_file() else None
            provenance={k:record.get(k) for k in ('source_session_state_sha256','session_state_sha256',
                'workspace_key_sha256','workspace_before_sha256','workspace_after_sha256','trace_creation_code','trace_sha256')}
            provenance.update(source_commit=os.environ.get('SOURCE_COMMIT'),model=manifest['model'],
                reasoning=manifest['reasoning'],compaction_policy=manifest['compaction_policy'],
                attempt=attempt,run_id=os.environ.get('RUN_ID'),project=name,checkpoint=n)
            dump(stage/'stage-provenance.json',provenance)
            record['stage_provenance_sha256']=sha256(stage/'stage-provenance.json')
            previous=session
            dump(output/'records.json', records)
            print('BENCHMARK_PROGRESS '+json.dumps({'instance_id':f'{name}/stage-{n}',
                'harness':'carry','state':'completed','status':record['status']}),flush=True)
    return records


def run_stage(*, project: dict, checkpoint: int, workspace: Path, output: Path,
              task_input: Path, resume_session: Path | None, image: str,
              bundle: Path, source: Path = ROOT, execute=subprocess.run) -> dict:
    identity=str(output.resolve())
    name='carry-slop-agent-'+hashlib.sha256(identity.encode()).hexdigest()[:16]
    network=swe.start_agent_network(identity=identity,proxy_image=BASE_IMAGE,
        proxy_script=source/'scripts/openai_proxy.js',execute=execute)
    code=125
    try:
        command=swe.agent_docker_command(image=image,harness='carry',repo=workspace,
            harness_bundle=bundle,task_input=task_input,output=output,model='gpt-6-luna',
            reasoning='medium',container_name=name,agent_timeout_seconds=600,
            network=network['internal'],proxy_ip=network['proxy_ip'],api_base=network['api_base'],
            resume_session=resume_session)
        command.append('--snapshot-only')
        # This exact command, not a second native invocation, owns the session.
        try:
            code=execute(command,check=False,timeout=645).returncode
        except subprocess.TimeoutExpired:
            code=124
        finally:
            swe.force_remove_container(name,exact_name=True)
        logs=execute(['docker','logs',network['proxy']],capture_output=True,text=True,
                     check=True,timeout=30)
        (output/'proxy.log').write_text(logs.stdout)
        result=stage_usage(output)
        terminal=output/'result.json'
        completed=terminal.exists() and json.loads(terminal.read_text()).get('completed') is True
        result.update(agent_returncode=code,timed_out=code==124,
                      agent_status='complete' if code==0 and completed and result['metering_complete'] else 'incomplete')
        return result
    finally:
        swe.cleanup_agent_network(network,execute=execute)


def stage_usage(session: Path) -> dict:
    trace = session/'trace.jsonl'
    events=[];trace_incomplete=not trace.exists()
    for line in trace.read_text(errors='replace').splitlines() if trace.exists() else []:
        try:events.append(json.loads(line))
        except json.JSONDecodeError:
            trace_incomplete=True
            break  # Retain real completed prefix, never fabricate a trailing event.
    requests = [e['data'] for e in events if e.get('event')=='model_request']
    responses = [e['data'] for e in events if e.get('event')=='model_response']
    request_steps = [r.get('step') for r in requests]
    response_steps = [r.get('step') for r in responses]
    unanswered = sorted(set(request_steps)-set(response_steps))
    usage = swe.empty_usage(); lower_bound = 0.0; valid = bool(requests and responses) and not trace_incomplete
    retries=sum(r.get('response_retries',0) for r in responses)
    valid=valid and retries==0
    proxy_path = session/'proxy.log'
    provider = [json.loads(line.split(' ',1)[1]) for line in proxy_path.read_text().splitlines()
                if line.startswith('BENCHMARK_PROXY_RESPONSE ')] if proxy_path.exists() else []
    identities = [r.get('response_id') for r in responses]
    valid = valid and len(set(request_steps))==len(request_steps) and request_steps==response_steps
    valid = valid and all(isinstance(r,str) and r for r in identities) and len(set(identities))==len(identities)
    valid = valid and len(provider)==len(responses) and {r.get('response_id') for r in provider}==set(identities)
    for response in responses:
        raw=response.get('usage',{})
        if any(type(raw.get(k)) is not int or raw[k]<0 for k in swe.USAGE_KEYS):
            valid=False; continue
        for k in usage: usage[k]+=raw[k]
        matches=[p for p in provider if p.get('response_id')==response.get('response_id')]
        valid=valid and len(matches)==1 and all(matches[0].get(k)==raw[k] for k in
            ('input_tokens','output_tokens','cached_input_tokens','cache_write_input_tokens'))
        cost=swe.estimate_cost_usd(raw,swe.pricing_for_model('gpt-6-luna'),
            max_round_input_tokens=raw['input_tokens'],observed_round_input_tokens=raw['input_tokens'])
        if cost is None: valid=False
        else: lower_bound+=cost
    compactions=sum(e.get('event')=='context_compacted' for e in events)
    return {'usage':usage, 'usage_scope':'stage-local-model-response-delta',
            'model_requests':len(requests), 'model_responses':len(responses),
            'provider_response_ids':identities, 'unanswered_steps':unanswered,
            'metering_complete':bool(valid), 'estimated_cost_usd':lower_bound if valid else None,
            'observed_response_cost_lower_bound_usd':lower_bound,
            'compactions':compactions,'trace_incomplete':trace_incomplete,'response_retries':retries}


def exact_slots(records: list[dict], source: Path, attempt: int) -> None:
    manifest=json.loads((source/'benchmarks/slop-continuous-2.json').read_text())
    expected={(p['name'],n,attempt) for p in manifest['projects'] for n in p['checkpoints']}
    slots=[(r.get('project'),r.get('checkpoint'),r.get('attempt')) for r in records]
    if len(records)!=8 or len(set(slots))!=8 or set(slots)!=expected:
        raise ValueError('expected exactly eight stage slots / two trajectories in this worker attempt')


def finalize(records: list[dict], output: Path, *, source: Path, attempt: int,
             attempts: int, catalog: dict) -> dict:
    exact_slots(records,source,attempt)
    complete=all(r['status']=='graded' and r['metering_complete'] for r in records)
    report={'schema':'carry.slop-continuous-report.v1','phase':'complete' if complete else 'incomplete',
            'denominator':8,'trajectories':2,'attempt':attempt,'attempts':attempts,
            'source_commit':os.environ.get('SOURCE_COMMIT'), 'run_id':os.environ.get('RUN_ID'),
            **preparation_identity(source),'catalog_reference':os.environ.get('TASK_IMAGE_CATALOG'),
            'prepared_image':catalog.get('image'), 'model':'gpt-6-luna','reasoning':'medium',
            'compaction_policy':'disabled','native_retention_signals':True,
            'resolved':sum(bool(r['resolved']) for r in records),
            'status_counts':dict(Counter(r['status'] for r in records)),
            'usage':{k:sum(r['usage'][k] for r in records) for k in swe.USAGE_KEYS},
            'metered_stage_slots':sum(bool(r['metering_complete']) for r in records),
            'estimated_cost_usd':sum(r['estimated_cost_usd'] for r in records) if complete else None,
            'observed_response_cost_lower_bound_usd':sum(r.get('observed_response_cost_lower_bound_usd',0) for r in records)}
    dump(output/'records.json',records);dump(output/'report.json',report)
    (output/'report.md').write_text('# Slop continuous-conversation pilot\n\n'
        f"- Quality: {report['resolved']}/8 stage slots; two project trajectories\n"
        f"- Phase: {report['phase']}; metered: {report['metered_stage_slots']}/8\n"
        f"- Complete modeled API-token cost: {report['estimated_cost_usd']} USD (None means unknown)\n"
        '- Not the upstream conversation-reset score. No compaction; native retention signals enabled.\n'
        '- Main-based source; differences from historical planner runs are descriptive, not causal.\n')
    return report


def validate_native_trace(session: Path, checkpoint: int) -> None:
    events=[json.loads(line) for line in (session/'trace.jsonl').read_text().splitlines()]
    event_names=[e.get('event') for e in events]
    expected='run_started' if checkpoint==1 else 'session_resumed'
    starts=[e for e in events if e.get('event') in {'run_started','session_resumed'}]
    if len(starts)!=1 or starts[0]['event']!=expected or event_names.count('run_finished')!=1:
        raise ValueError('native stage is not a single finished continuation')
    config=starts[0]['data']
    if (config.get('model'),config.get('reasoning_effort'),config.get('compaction_policy'))!=('gpt-6-luna','medium','disabled'):
        raise ValueError('native effective settings differ')
    if checkpoint==1 and config.get('max_steps') is not None:
        raise ValueError('hidden native turn cap')
    if 'context_compacted' in event_names or 'compaction_review' in event_names:
        raise ValueError('compaction/review in disabled protocol')
    recoveries=[i for i,e in enumerate(events) if e.get('event')=='trace_recovery']
    if recoveries and (checkpoint==1 or recoveries!=[0]):
        raise ValueError('unexpected trace recovery')
    for event in events:
        if event.get('event')!='model_request':continue
        request=event['data']['request']
        if request.get('model')!='gpt-6-luna' or request.get('reasoning',{}).get('effort')!='medium':
            raise ValueError('provider request treatment differs')
        tools={t.get('name'):t for t in request.get('tools',[])}
        for name in ('shell','finish'):
            properties=tools.get(name,{}).get('parameters',{}).get('properties',{}).get('context',{}).get('properties',{})
            if set(properties)!={'protected','removable','remember'}:
                raise ValueError('native retention signal schema missing')


def validate_report(output: Path, *, source: Path = ROOT, attempt: int = 1, attempts: int = 1) -> None:
    records=json.loads((output/'records.json').read_text());exact_slots(records,source,attempt)
    report=json.loads((output/'report.json').read_text())
    if (report.get('schema')!='carry.slop-continuous-report.v1' or report.get('denominator')!=8
        or report.get('trajectories')!=2 or report.get('phase')!='complete'
        or report.get('attempt')!=attempt or report.get('attempts')!=attempts):
        raise ValueError('incomplete or wrong Slop report/denominator/attempt')
    if not 1<=attempt<=attempts<=3:
        raise ValueError('Slop attempt identity must be 1-3')
    if any(report.get(k)!=v for k,v in preparation_identity(source).items()):
        raise ValueError('Slop preparation identity differs')
    if (report.get('model'),report.get('reasoning'),report.get('compaction_policy'),report.get('native_retention_signals'))!=('gpt-6-luna','medium','disabled',True):
        raise ValueError('wrong effective Slop settings')
    if os.environ.get('SOURCE_COMMIT') and report.get('source_commit')!=os.environ['SOURCE_COMMIT']:
        raise ValueError('source commit differs')
    if os.environ.get('TASK_IMAGE_CATALOG') and report.get('catalog_reference')!=os.environ['TASK_IMAGE_CATALOG']:
        raise ValueError('prepared catalog differs')
    total=0.0
    for name in {r['project'] for r in records}:
        previous=None;cache=None;previous_workspace=None;workspace_key=None
        for record in sorted((r for r in records if r['project']==name),key=lambda r:r['checkpoint']):
            stage=output/name/f"stage-{record['checkpoint']}";session=stage/'session'
            validate_native_trace(session,record['checkpoint'])
            if not record.get('workspace_key_sha256') or (workspace_key and record['workspace_key_sha256']!=workspace_key):
                raise ValueError('project workspace identity changed')
            workspace_key=record['workspace_key_sha256']
            if record['status']!='graded' or record.get('agent_status')!='complete' or record.get('grade',{}).get('status')!='graded':
                raise ValueError('unexecuted/incomplete stage in Slop denominator')
            if record.get('source_session_state_sha256')!=previous:
                raise ValueError('broken native session hash chain')
            if previous_workspace is not None and record.get('source_workspace_sha256')!=previous_workspace:
                raise ValueError('broken continuous workspace hash chain')
            state=json.loads((session/STATE_FILE).read_text())
            previous=sha256(session/STATE_FILE)
            if previous!=record.get('session_state_sha256') or state.get('version')!=1 or state.get('model')!='gpt-6-luna':
                raise ValueError('native checkpoint changed or invalid')
            if not state.get('prompt_cache_key') or (cache and state['prompt_cache_key']!=cache):
                raise ValueError('prompt-cache identity changed')
            cache=state['prompt_cache_key']
            if state.get('context',{}).get('generation')!=0:
                raise ValueError('context generation changed in no-compaction study')
            previous_workspace=workspace_hash(stage/'workspace')
            if record.get('workspace_sha256')!=previous_workspace:
                raise ValueError('workspace snapshot changed')
            for kind in ('before','after'):
                path=stage/f'workspace-{kind}.json'
                if sha256(path)!=record.get(f'workspace_{kind}_sha256'):
                    raise ValueError('workspace tree manifest changed')
            if record['workspace_after_sha256']!=previous_workspace:
                raise ValueError('after manifest does not match retained workspace snapshot')
            if record['workspace_before_sha256']!=record['source_workspace_sha256']:
                raise ValueError('before manifest does not match source workspace')
            provenance=json.loads((stage/'stage-provenance.json').read_text())
            if sha256(stage/'stage-provenance.json')!=record.get('stage_provenance_sha256'):
                raise ValueError('stage provenance changed')
            expected_fields={k:record.get(k) for k in ('source_session_state_sha256','session_state_sha256',
                'workspace_key_sha256','workspace_before_sha256','workspace_after_sha256','trace_creation_code','trace_sha256')}
            if any(provenance.get(k)!=v for k,v in expected_fields.items()):
                raise ValueError('stage provenance differs from captured records')
            if (provenance.get('source_commit'),provenance.get('model'),provenance.get('reasoning'),provenance.get('compaction_policy'))!=(report['source_commit'],'gpt-6-luna','medium','disabled'):
                raise ValueError('provenance source/treatment differs')
            if record['trace_creation_code']!=int(record['checkpoint']>1):
                raise ValueError('fresh checkpoint-only trace creation not declared')
            if sha256(session/'trace.jsonl')!=record.get('trace_sha256'):
                raise ValueError('raw stage delta trace changed')
            if sha256(stage/'user-message.md')!=record.get('user_message_sha256'):
                raise ValueError('immutable user message changed')
            measured=stage_usage(session)
            if not measured['metering_complete'] or measured['compactions'] or measured['usage']!=record['usage']:
                raise ValueError('native/provider metering incomplete or physical compaction observed')
            if measured['estimated_cost_usd']!=record['estimated_cost_usd']:
                raise ValueError('cost differs from raw native/provider accounting')
            total+=measured['estimated_cost_usd']
    if abs(total-report['estimated_cost_usd'])>1e-12:
        raise ValueError('report cost differs from stage-local deltas')
    if (report['usage']!={k:sum(r['usage'][k] for r in records) for k in swe.USAGE_KEYS}
        or report['resolved']!=sum(bool(r['resolved']) for r in records)
        or report['status_counts']!=dict(Counter(r['status'] for r in records))
        or report['metered_stage_slots']!=8):
        raise ValueError('aggregate usage/quality/status differs')


def prepare_images(source: Path, work: Path, output: Path, repository: str) -> dict:
    if os.environ.get('OPENAI_API_KEY'):raise ValueError('preparation must have no model credential')
    manifest=json.loads((source/'benchmarks/slop-continuous-2.json').read_text())
    assets=fetch_assets(work,manifest)
    identity=preparation_identity(source)
    tag=repository+':slop-'+identity['prepared_recipe_sha256']
    subprocess.run(['docker','build','--pull','--file',str(source/'containers/slopbench/Dockerfile'),
        '--build-arg','BASE_IMAGE='+BASE_IMAGE,'--tag',tag,str(source/'containers/slopbench')],check=True)
    checks=reference_check(assets,output/'reference',image=tag,source=source)
    # Validate readiness before publishing a reusable image or granting model authority.
    provisional={'schema':'carry.slop-task-catalog.v1',**identity,**checks,
                 'image':repository+'@sha256:'+'0'*64}
    validate_catalog(provisional,source)
    inventory=subprocess.check_output(['docker','run','--rm','--network=none','--entrypoint',
        '/opt/slop-venv/bin/pip',tag,'freeze'],text=True)
    (output/'dependency-manifest.txt').write_text(inventory)
    subprocess.run(['docker','push',tag],check=True)
    image=swe._inspect_catalog_image(tag,execute=subprocess.run)['resolved_digest']
    catalog={**provisional,'image':image,'dependency_manifest_sha256':sha256(output/'dependency-manifest.txt'),
             'problems_pin':manifest['problems'],'runner_pin':manifest['runner']}
    reference=swe.publish_task_catalog_image(catalog=catalog,repository=repository,output=output)
    report={**catalog,'catalog_reference':reference,'phase':'complete','denominator':8,'trajectories':2}
    dump(output/'preparation-report.json',report)
    print(json.dumps(report,sort_keys=True))
    return report


def preflight(source: Path, work: Path, output: Path, repository: str, reference: str) -> dict:
    if os.environ.get('OPENAI_API_KEY'):raise ValueError('preflight must run before fetching model credential')
    manifest=json.loads((source/'benchmarks/slop-continuous-2.json').read_text())
    catalog=swe.load_task_catalog_image(reference=reference,repository=repository,output=output/'catalog')
    validate_catalog(catalog,source)
    subprocess.run(['docker','pull',catalog['image']],check=True)
    subprocess.run(['docker','pull',BASE_IMAGE],check=True)
    assets=fetch_assets(work,manifest)
    config=dict(os.environ,BASE_IMAGE=BASE_IMAGE,CODEX_VERSION='0.147.0',PI_VERSION='0.84.2',
        MODEL='gpt-6-luna',REASONING='medium',CARRY_COMPACTION_POLICY='disabled',
        CARRY_BASE_IMAGE='rust@sha256:948f9b08a66e7fe01b03a98ef1c7568292e07ec2e4fe90d88c07bb14563c84ff')
    images=swe.build_images(source=source,run_id=os.environ.get('RUN_ID','slop'),config=config,harnesses=('carry',))
    bundle=swe.export_harness_bundles(images,work/'harness-bundles')['carry']
    ready={'schema':'carry.slop-ready.v1','assets':str(assets),'bundle':str(bundle),'catalog':catalog,
           'catalog_reference':reference,'source_commit':os.environ.get('SOURCE_COMMIT'),
           'adapter_sha256':sha256(bundle/'bin/adapter'),
           'carry_binary_sha256':sha256(bundle/'bin/carry'),'harness_images':images}
    dump(work/'slop-ready.json',ready)
    dump(output/'preflight.json',ready)
    return ready


def main() -> int:
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command',choices=['reference-check','prepare-images','preflight','run','validate-report','validate-preparation','identity'])
    parser.add_argument('--source',type=Path,default=ROOT)
    parser.add_argument('--work',type=Path)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--assets',type=Path)
    parser.add_argument('--image',help='Model-free reference grader image; omitted runs installed local plugins')
    parser.add_argument('--attempt',type=int,default=int(os.environ.get('BENCHMARK_ATTEMPT','1')))
    parser.add_argument('--attempts',type=int,default=int(os.environ.get('BENCHMARK_ATTEMPTS','1')))
    args=parser.parse_args()
    source=args.source.resolve();output=args.output.resolve()
    if not 1<=args.attempt<=args.attempts<=3:parser.error('Slop attempt must be 1-3')
    if args.command=='identity':
        print(json.dumps(preparation_identity(source)));return 0
    if args.command=='validate-report':
        validate_report(output,source=source,attempt=args.attempt,attempts=args.attempts);return 0
    if args.command=='validate-preparation':
        report=json.loads((output/'preparation-report.json').read_text());validate_catalog(report,source)
        if report.get('phase')!='complete' or not swe.DIGEST_IMAGE.fullmatch(report.get('catalog_reference','')):
            raise ValueError('preparation not complete or immutable')
        return 0
    output.mkdir(parents=True,exist_ok=True)
    if args.work is None:parser.error('--work required for execution')
    work=args.work.resolve();work.mkdir(parents=True,exist_ok=True)
    if args.command=='reference-check':
        if os.environ.get('OPENAI_API_KEY'):raise ValueError('reference checks must have no model key')
        manifest=json.loads((source/'benchmarks/slop-continuous-2.json').read_text())
        assets=args.assets or fetch_assets(work,manifest)
        checks=reference_check(assets,output,source=source,image=args.image)
        validate_catalog({'schema':'carry.slop-task-catalog.v1',**preparation_identity(source),**checks,
                          'image':'reference@sha256:'+'0'*64},source);return 0
    repository=os.environ.get('TASK_IMAGE_REPOSITORY','')
    reference=os.environ.get('TASK_IMAGE_CATALOG','')
    if args.command=='prepare-images':
        prepare_images(source,work,output,repository);return 0
    if args.command=='preflight':
        preflight(source,work,output,repository,reference);return 0
    if ((os.environ.get('MODEL','gpt-6-luna'),os.environ.get('REASONING','medium'),
         os.environ.get('CARRY_COMPACTION_POLICY','disabled'),os.environ.get('BENCHMARK_HARNESS','carry'))
        !=('gpt-6-luna','medium','disabled','carry')):
        raise ValueError('Slop pilot requires Carry / gpt-6-luna / medium / disabled compaction')
    if not os.environ.get('OPENAI_API_KEY'):raise ValueError('model credential absent')
    ready=json.loads((work/'slop-ready.json').read_text())
    validate_catalog(ready['catalog'],source)
    if ready['source_commit']!=os.environ.get('SOURCE_COMMIT') or ready['catalog_reference']!=reference:
        raise ValueError('ready source/catalog changed')
    bundle=Path(ready['bundle'])
    if (sha256(bundle/'bin/adapter')!=ready['adapter_sha256']
        or sha256(bundle/'bin/carry')!=ready['carry_binary_sha256']):
        raise ValueError('native adapter/binary changed')
    os.environ['CARRY_COMPACTION_POLICY']='disabled'
    manifest=json.loads((source/'benchmarks/slop-continuous-2.json').read_text())
    image=ready['catalog']['image']
    def agent(**kw):return run_stage(**kw,image=image,bundle=bundle,source=source)
    def grader(*a,**kw):return grade_snapshot(*a,**kw,image=image)
    records=run_trajectories(manifest,Path(ready['assets']),work/'trajectories',output,args.attempt,
                             agent=agent,grader=grader)
    report=finalize(records,output,source=source,attempt=args.attempt,attempts=args.attempts,catalog=ready['catalog'])
    return 0 if report['phase']=='complete' else 1


if __name__=='__main__':
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print('SLOP_INCOMPLETE '+type(error).__name__,file=sys.stderr)
        raise SystemExit(1)

