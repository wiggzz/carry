import hashlib
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest

from subprocess import run
import sys

from history_store import assert_streaming_index_plans

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"
sys.path.insert(0, str(SCRIPTS))
from stage_history_rowid import stage

TABLE = ('CREATE TABLE records(kind TEXT NOT NULL, key TEXT NOT NULL, '
         'data BLOB NOT NULL, at INTEGER NOT NULL, owner TEXT NOT NULL, PRIMARY KEY(kind,key))')
SQL = 'SELECT kind,key,data,at,owner FROM records ORDER BY kind,key'


class RowidStageTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.src = self.root / 'input'
        self.src.mkdir(mode=0o700)
        self.dest = self.root / 'staged'
        self.db = self.src / 'history.index'
        c = sqlite3.connect(self.db)
        try:
            c.executescript('PRAGMA page_size=4096; PRAGMA auto_vacuum=INCREMENTAL;'
                            'PRAGMA journal_mode=WAL; PRAGMA user_version=1;'
                            + TABLE + ' WITHOUT ROWID;'
                            'CREATE INDEX records_age ON records(kind,at);'
                            'CREATE INDEX records_owner ON records(kind,owner);')
            self.rows = sorted((k, f'{i:064x}', bytes([i])*sz, i*9, f'owner-{i%3}')
                               for k, sz in [('nodes', 9500), ('snapshots', 6000), ('accounting', 213), ('metadata', 8)]
                               for i in range(16))
            c.executemany('INSERT INTO records VALUES(?,?,?,?,?)', self.rows)
            c.commit()
        finally:
            c.close()
        self.db.chmod(0o600)
        self.original = self.db.read_bytes()

    def digest(self):
        return hashlib.sha256(self.db.read_bytes()).hexdigest()

    def convert(self):
        return stage(self.db, self.dest, self.digest())

    def test_converts_real_sqlite_rows_keeps_source_and_reopens_for_writes(self):
        proc = run([sys.executable, "-B", str(SCRIPTS / "stage_history_rowid.py"),
                    str(self.db), str(self.dest), "--sha256", self.digest()],
                   capture_output=True, text=True, timeout=15)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(json.loads(proc.stdout)["rows"], len(self.rows))
        self.assertEqual(self.db.read_bytes(), self.original)
        output = self.dest/'history.index'
        self.assertFalse(output.samefile(self.db))
        self.assertEqual(output.stat().st_mode & 0o777, 0o600)
        for write in (True, False):
            c = sqlite3.connect(output)
            try:
                self.assertEqual(c.execute('PRAGMA journal_mode').fetchone(), ('wal',))
                self.assertEqual(c.execute('PRAGMA user_version').fetchone(), (1,))
                self.assertEqual(c.execute('PRAGMA auto_vacuum').fetchone(), (2,))
                self.assertEqual(c.execute('PRAGMA quick_check').fetchone(), ('ok',))
                self.assertEqual(c.execute(SQL).fetchall(), sorted(self.rows))
                self.assertEqual(c.execute('SELECT count(rowid) FROM records').fetchone(), (len(self.rows),))
                assert_streaming_index_plans(self, c)
                if write:
                    row = ('nodes', 'z'*64, b'after restart', 123, 'owner-after')
                    c.execute('INSERT INTO records VALUES(?,?,?,?,?)', row)
                    c.commit()
                    self.rows.append(row)
            finally:
                c.close()

    def test_wrong_digest_and_nonempty_wal_refused_without_publication(self):
        with self.assertRaises(ValueError):
            stage(self.db,self.dest,'0'*64)
        c = sqlite3.connect(self.db)
        try:
            c.execute('INSERT INTO records VALUES(?,?,?,?,?)',('nodes','live',b'new',0,''))
            c.commit()
            self.assertGreater(self.db.with_name('history.index-wal').stat().st_size,0)
            with self.assertRaises(ValueError):
                self.convert()
        finally:
            c.close()
        self.assertFalse(self.dest.exists())

    def test_source_directory_destination_and_existing_destination_refused(self):
        with self.assertRaises(FileExistsError):
            stage(self.db,self.src,self.digest())
        with self.assertRaises(ValueError):
            stage(self.db,self.src/'nested',self.digest())
        self.dest.mkdir()
        output=self.dest/'history.index'
        output.write_bytes(b'existing authority')
        with self.assertRaises(FileExistsError):
            self.convert()
        self.assertEqual(output.read_bytes(),b'existing authority')
        self.assertEqual(self.db.read_bytes(),self.original)

    def test_unexpected_authority_and_symlink_refused(self):
        with sqlite3.connect(self.db) as c:
            c.execute('CREATE TABLE other(authority BLOB)')
        c.close()
        with self.assertRaises(ValueError):
            self.convert()
        self.assertFalse(self.dest.exists())
        alias=self.src/'alias'
        alias.mkdir()
        (alias/'history.index').symlink_to(self.db)
        with self.assertRaises(ValueError):
            stage(alias/'history.index',self.dest,self.digest())


if __name__=='__main__':
    unittest.main()
