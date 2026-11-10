"""Test-only logical view/staging of Carry's actual disk authority.

This decodes database records, not diagnostic mirrors. It lets existing wire and
money graders remain unchanged while their storage staging uses transactions.
"""
import json
import sqlite3
from contextlib import closing

KINDS = ('nodes', 'tips', 'reviews', 'transcripts', 'snapshots')


def read_index(path):
    if not path.read_bytes()[:16].startswith(b'SQLite format 3'):
        return json.loads(path.read_text())
    with closing(sqlite3.connect(path)) as db, db:
        result = {'format': 'carry-prefix-history-v4', 'upstream_cache': []}
        result.update({kind: {} for kind in KINDS})
        for kind, key, data in db.execute('SELECT kind,key,data FROM records'):
            if kind == 'upstream_cache':
                result[kind] = json.loads(data)
            elif kind in KINDS:
                result[kind][key] = json.loads(data)
        return result


def write_index(path, index):
    if index.get('format') != 'carry-prefix-history-v4':
        path.write_text(json.dumps(index))
        return
    if path.exists() and not path.read_bytes().startswith(b'SQLite format 3'):
        path.unlink()
    with closing(sqlite3.connect(path)) as db, db:
        db.execute('CREATE TABLE IF NOT EXISTS records(kind TEXT NOT NULL,key TEXT NOT NULL,data BLOB NOT NULL,at INTEGER NOT NULL,owner TEXT NOT NULL,PRIMARY KEY(kind,key)) WITHOUT ROWID')
        db.execute('PRAGMA user_version=1')
        db.execute("DELETE FROM records WHERE kind IN ('nodes','tips','reviews','transcripts','snapshots','upstream_cache')")
        at = max((r.get('at', 0) for k in ('tips', 'transcripts', 'reviews')
                  for r in index.get(k, {}).values()), default=0)
        owner_ages = {r['session']: r['at'] for r in index.get('tips', {}).values()}
        for kind in KINDS:
            for key, value in index.get(kind, {}).items():
                owner = value.get('session', key if kind == 'snapshots' else '')
                timestamp = value.get('at', owner_ages.get(owner, at))
                db.execute('INSERT INTO records VALUES(?,?,?,?,?)',
                           (kind, key, json.dumps(value).encode(), timestamp, owner))
        db.execute('INSERT INTO records VALUES(?,?,?,?,?)',
                   ('upstream_cache', 'current', json.dumps(index.get('upstream_cache', [])).encode(), at, ''))


def publication_fault(path, enabled):
    with closing(sqlite3.connect(path)) as db, db:
        if enabled:
            db.execute("CREATE TRIGGER publication_fault BEFORE INSERT ON records WHEN new.kind='reviews' BEGIN SELECT RAISE(ABORT,'fixture publication failure'); END")
        else:
            db.execute('DROP TRIGGER publication_fault')
