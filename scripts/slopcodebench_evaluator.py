"""Explicit source-bound correction of the pinned inventory count owner.

Only trusted preparation writes these bytes. Runtime validation is read-only;
no other tracked change is accepted, and the upstream Git pin stays unchanged.
"""
import hashlib
import json
from pathlib import Path
import subprocess

SOURCE = Path(__file__).resolve().parents[1]
LOCK = SOURCE/'benchmarks/slopcodebench/upstream.json'


def lock():
    return json.loads(LOCK.read_text())


def identity():
    value = lock()
    return {'runner_commit': value['runner_commit'], 'report_sha256': value['report_sha256'],
            **value['evaluator_correction']}


def corrected_bytes(raw):
    spec = identity()
    if hashlib.sha256(raw).hexdigest() != spec['before_sha256']:
        raise ValueError('evaluator correction requires exact pinned before bytes')
    edit_raw = (SOURCE/spec['edit']).read_bytes()
    if hashlib.sha256(edit_raw).hexdigest() != spec['edit_sha256']:
        raise ValueError('evaluator correction edit digest mismatch')
    edit = json.loads(edit_raw)
    before, after = edit['before'].encode(), edit['after'].encode()
    if raw.count(before) != 1:
        raise ValueError('evaluator correction must match exactly once')
    result = raw.replace(before, after)
    if hashlib.sha256(result).hexdigest() != spec['after_sha256']:
        raise ValueError('evaluator correction after digest mismatch')
    return result


def verify(root, *, corrected=True):
    root = Path(root)
    spec = identity()
    def git(*args):
        return subprocess.check_output(['git', '-C', str(root), *args])
    if git('rev-parse', 'HEAD').decode().strip() != spec['runner_commit']:
        raise ValueError('evaluator checkout revision mismatch')
    status = git('status', '--porcelain', '--untracked-files=no').decode()
    expected_status = ' M '+spec['module']+'\n' if corrected else ''
    if status != expected_status:
        raise ValueError('evaluator checkout has unexpected tracked changes')
    # Verify the base object as well as the working copy, not just a dirty path.
    original = git('show', 'HEAD:'+spec['module'])
    expected = corrected_bytes(original) if corrected else original
    if hashlib.sha256(original).hexdigest() != spec['before_sha256']:
        raise ValueError('evaluator base object mismatch')
    if (root/spec['module']).is_symlink() or (root/spec['module']).read_bytes() != expected:
        raise ValueError('evaluator collection source bytes mismatch')
    report = root/lock()['report_module']
    if report.is_symlink() or hashlib.sha256(report.read_bytes()).hexdigest() != spec['report_sha256']:
        raise ValueError('evaluator report source bytes mismatch')
    return spec


def install(root):
    """Run once on a fresh trusted hosted checkout, before any upstream import."""
    spec = verify(root, corrected=False)
    path = Path(root)/spec['module']
    path.write_bytes(corrected_bytes(path.read_bytes()))
    verify(root)
    return spec


def validate_binding(contract, prepared):
    expected = identity()
    if (contract.get('runner_commit') != expected['runner_commit']
            or contract.get('evaluator_identity') != expected
            or prepared.get('evaluator_identity') != expected):
        raise ValueError('missing or mismatched effective evaluator provenance')
