#!/usr/bin/env python3
"""Stage a rowid history.index from an independently frozen Carry v1 copy.

Never use a serving state directory as input or output. This tool does not
install, stop, start, or mutate a Carry service. Run under a finite disk/memory
workload guard. A successful stage is not authorization to install it.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import stat
import tempfile
from contextlib import closing

RESERVE = 256 << 20
SLACK = 32 << 20
TABLE = ('CREATE TABLE records(kind TEXT NOT NULL, key TEXT NOT NULL, '
         'data BLOB NOT NULL, at INTEGER NOT NULL, owner TEXT NOT NULL, PRIMARY KEY(kind,key))')
RECORDS = 'SELECT kind,key,data,at,owner FROM records ORDER BY kind,key'


def metadata(path):
    s = path.lstat()
    if not stat.S_ISREG(s.st_mode) or s.st_uid != os.getuid() or s.st_mode & 0o077:
        raise ValueError('expected a private regular file owned by the current user')
    for ext in ('-wal', '-journal', '-shm'):
        p = path.with_name(path.name + ext)
        try:
            a = p.lstat()
        except FileNotFoundError:
            continue
        if not stat.S_ISREG(a.st_mode) or (ext != '-shm' and a.st_size != 0):
            raise ValueError('source must be a checkpointed independent copy with no active journal')
    return s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns, s.st_ctime_ns, s.st_mode


def sha(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for b in iter(lambda: f.read(1 << 20), b''):
            h.update(b)
    return h.hexdigest()


def frozen_uri(path):
    return path.as_uri() + '?mode=ro&immutable=1'


def schema(*, rowid):
    suffix = ',key' if rowid else ''
    return {
        'records': TABLE + ('' if rowid else ' WITHOUT ROWID'),
        'records_age': f'CREATE INDEX records_age ON records(kind,at{suffix})',
        'records_owner': f'CREATE INDEX records_owner ON records(kind,owner{suffix})',
    }


def identity(path, *, rowid):
    c = sqlite3.connect(frozen_uri(path), uri=True)
    try:
        c.executescript('PRAGMA query_only=ON; PRAGMA cache_size=-2048; PRAGMA mmap_size=0;')
        expected = schema(rowid=rowid)
        objects = dict(c.execute('SELECT name, sql FROM sqlite_schema WHERE sql IS NOT NULL'))
        if (objects != expected or any(c.execute('PRAGMA '+k).fetchone() != (v,) for k, v in
                                      [('user_version',1),('page_size',4096),('auto_vacuum',2)])):
            raise ValueError('unsupported history authority schema or layout')
        if rowid and c.execute('PRAGMA quick_check').fetchall() != [('ok',)]:
            raise ValueError('staged database did not pass SQLite quick_check')
        h, rows = hashlib.sha256(), 0
        for kind, key, data, at, owner in c.execute(RECORDS):
            if type(at) is not int or not isinstance(data, bytes) or not all(isinstance(v, str) for v in (kind,key,owner)):
                raise ValueError('unexpected record field storage class')
            for v in (kind.encode(), key.encode(), data, str(at).encode(), owner.encode()):
                h.update(len(v).to_bytes(8, 'big'))
                h.update(v)
            rows += 1
        return rows, h.hexdigest()
    finally:
        c.close()


def stage(source, out_dir, expected_sha):
    source = Path(source).absolute()
    out_dir = Path(out_dir).absolute()
    if source.name != 'history.index':
        raise ValueError('supply a frozen history.index')
    if os.path.lexists(out_dir):
        raise FileExistsError('staging requires a new directory')
    if source.parent.resolve() in out_dir.resolve().parents:
        raise ValueError('staging must be outside the frozen input directory')
    original = metadata(source)
    if sha(source) != expected_sha:
        raise ValueError('frozen source SHA-256 mismatch')
    extent_limit = original[2] + (48 << 20)
    if shutil.disk_usage(out_dir.parent).free <= RESERVE + SLACK + extent_limit:
        raise OSError('insufficient disk space for bounded staging and unchanged reserve')
    source_rows = identity(source, rowid=False)
    # Claim a new private directory exclusively. No caller-owned destination
    # can be overwritten, and publication does not depend on hard-link support.
    out_dir.mkdir(mode=0o700)
    try:
        return stage_in_new_dir(source, out_dir, expected_sha, original, extent_limit, source_rows)
    except BaseException:
        if not any(out_dir.iterdir()):
            out_dir.rmdir()
        raise


def stage_in_new_dir(source, out_dir, expected_sha, original, extent_limit, source_rows):
    output = out_dir/'history.index'
    with tempfile.TemporaryDirectory(prefix='.rowid-stage-',dir=out_dir) as temp:
        db = Path(temp)/'index.sqlite'
        with closing(sqlite3.connect(frozen_uri(source), uri=True)) as src, closing(sqlite3.connect(db)) as c:
            src.executescript('PRAGMA query_only=ON; PRAGMA cache_size=-2048; PRAGMA mmap_size=0; BEGIN;')
            # Journal OFF is limited to the unpublished scratch file. Source is
            # immutable/read-only; staged output returns to the original WAL mode.
            ddl = '; '.join(schema(rowid=True).values())
            c.executescript(f'''PRAGMA main.page_size=4096; PRAGMA main.max_page_count={extent_limit//4096};
                PRAGMA main.auto_vacuum=INCREMENTAL; PRAGMA main.journal_mode=OFF;
                PRAGMA main.cache_size=-2048;
                PRAGMA temp_store=FILE; BEGIN;
                {ddl};
                PRAGMA main.user_version=1;''')
            c.executemany('INSERT INTO records VALUES(?,?,?,?,?)', src.execute(RECORDS))
            c.commit()
            if c.execute('PRAGMA main.journal_mode=WAL').fetchone() != ('wal',):
                raise ValueError('could not enable WAL on staged output')
        os.chmod(db, stat.S_IMODE(original[-1]))
        if metadata(db)[2] > extent_limit or identity(db, rowid=True) != source_rows:
            raise ValueError('staged authority differs from the frozen source rows')
        if metadata(source) != original or sha(source) != expected_sha or metadata(source) != original:
            raise ValueError('frozen input changed during staging')
        if shutil.disk_usage(out_dir).free <= RESERVE:
            raise OSError('insufficient disk reserve after staging')
        with db.open('rb') as f:
            output_sha = sha(db)
            os.fsync(f.fileno())
        os.rename(db, output)  # Atomic publication within our exclusively created directory.
        d = os.open(out_dir, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(d)
        finally:
            os.close(d)
    return {'status':'STAGED_ONLY', 'input_sha256':expected_sha, 'output_sha256':output_sha,
            'rows':source_rows[0], 'logical_rows_sha256':source_rows[1], 'output_bytes':output.stat().st_size}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('frozen', type=Path)
    p.add_argument('out_dir', type=Path)
    p.add_argument('--sha256', required=True)
    a = p.parse_args()
    print(json.dumps(stage(a.frozen,a.out_dir,a.sha256)))


if __name__ == '__main__':
    main()
