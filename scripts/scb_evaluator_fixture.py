"""Exact retained producer replay, with import-only native boundaries disabled.

Requires SCB_PRODUCER_EVIDENCE pointing at the immutable retained experiment.
This is not Docker/native gold qualification.
"""
from pathlib import Path
from types import ModuleType, SimpleNamespace
from unittest.mock import patch
import hashlib
import json
import os
import sys
import zipfile


def replay(collection_path):
    root = Path(os.environ['SCB_PRODUCER_EVIDENCE'])
    producer = root/'producer-20261115T225735Z'
    report = root/'eligibility-20261115T2250/pinned-runner/src/slop_code/evaluation/report.py'
    runner = producer/'pytest_runner.py'
    for path, expected in [(report, '5243b72604cee806491934e742f5b6312f778720dd7fd89abd35443d3d7de19e'),
                           (runner, 'a7bbc8cb447019569ed214e85d987f2daeae27216638709ecacd4693033b9b80')]:
        assert hashlib.sha256(path.read_bytes()).hexdigest() == expected
    def forbidden(*args, **kwargs):
        raise AssertionError('native boundary unavailable in exact-component replay')
    class Logger:
        def __getattr__(self, name): return lambda *a, **k: None
    modules = {}
    def shim(name, **attrs):
        module = ModuleType(name); module.__dict__.update(attrs); modules[name] = module
        return module
    for name in ['slop_code', 'slop_code.evaluation', 'slop_code.execution']:
        shim(name, __path__=[])
    shim('slop_code.logging', get_logger=lambda *a: Logger())
    shim('slop_code.common', WORKSPACE_TEST_DIR='tests')
    shim('slop_code.evaluation.config', CheckpointConfig=object, ProblemConfig=object,
         ConfigError=Exception, GroupConfig=object, get_available_problems=forbidden)
    shim('slop_code.execution.assets', resolve_static_assets=forbidden)
    shim('slop_code.execution.models', EnvironmentSpec=object)
    shim('slop_code.execution.session', Session=SimpleNamespace(from_environment_spec=forbidden))
    with patch.dict(sys.modules, modules):
        def load(name, path):
            module = ModuleType(name); sys.modules[name] = module
            exec(compile(path.read_bytes(), str(path), 'exec'), module.__dict__)
            return module
        r = load('slop_code.evaluation.report', report)
        p = load('slop_code.evaluation.pytest_runner', runner)
        c = load('slop_code.evaluation.collection', collection_path)
        with zipfile.ZipFile(root/'gold-preparation-failed.zip') as archive:
            base = 'prepared/readiness/gold-reports/database_migration/checkpoint_3/'
            raw = archive.read(base+'evaluation.json')
            assert hashlib.sha256(raw).hexdigest() == '09c32ddd63ecb70ed04adb0952d08bde71c579f9f53701ab4641517c8694f0a9'
            saved = json.loads(raw)
            pytest_raw = archive.read(base+'evaluation/report.json')
            assert hashlib.sha256(pytest_raw).hexdigest() == 'ea618dacc78e3073c18816f9c1ffb14de9e77ce6f19db01ce68175173f8e4ad3'
            pytest_report = json.loads(pytest_raw)
        runner = p.PytestRunner(problem=SimpleNamespace(markers={}),
            checkpoint=SimpleNamespace(name='checkpoint_3'), environment=None,
            submission_path=Path('/unavailable-native-workspace'))
        parsed = runner._parse_pytest_report_tests(pytest_report)
        tests = [runner._convert_pytest_report_test_to_result(t) for t in parsed]
        metadata = {k:v for k,v in saved.items() if k not in {'tests', 'pass_counts', 'total_counts', 'test_collection_hash'}}
        result = r.CorrectnessResults(**metadata)
        for g in r.GroupType:
            result.total_counts.setdefault(g, 0); result.pass_counts.setdefault(g, 0)
        by_nodeid, grouped = {}, {}
        for entry, test in zip(parsed, tests, strict=True):
            result.add_test_result(test)
            nodeid = entry['nodeid']
            by_nodeid[nodeid] = c.CollectedTestCase(nodeid=nodeid, test_id=test.id,
                file_path=test.file_path, checkpoint=test.checkpoint, group_type=test.group_type)
            grouped.setdefault(f'{test.checkpoint}-{test.group_type.value}', []).append(test.id)
        inventory_hash = c.compute_tc_hash(grouped)
        assert inventory_hash == saved['test_collection_hash']
        inventory = c.CheckpointTestCollection(tests=[by_nodeid[k] for k in sorted(by_nodeid)],
            by_nodeid=by_nodeid, grouped_test_ids=grouped, total_collected=len(by_nodeid),
            test_collection_hash=inventory_hash)
        init = producer/'__init__.py'
        assert hashlib.sha256(init.read_bytes()).hexdigest() == '9f464b6d53df3112e61f0fd3e91898efce6c348018c2b940dc09371f86c5a005'
        public = load('slop_code.evaluation', init)
        # Execute the real public alias -> pytest wrapper -> collection owner.
        # Only the native collector and subprocess runner are synthetic boundaries.
        with patch.object(c, 'collect_checkpoint_tc', return_value=inventory), \
                patch.object(p.PytestRunner, 'run', return_value=result):
            returned = public.run_checkpoint(submission_path=Path('/unavailable-native-workspace'),
                problem=runner.problem, checkpoint=runner.checkpoint, env_spec=None)
        assert returned is result
        return r, c, result, inventory, saved
