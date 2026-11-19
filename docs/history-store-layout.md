# Carry history storage layout

New proxy stores put record keys and payloads in separate SQLite B-trees. The age and owner indexes explicitly include the record key so accounting and expiry can stream in the same order. Both layouts use the same v1 logical schema. Carry continues to read and write existing stores in place without converting them on startup or during requests. Updating the binary alone does not speed up an existing `WITHOUT ROWID` store.

## Rehearse conversion on a copy

This is an offline staging tool, not a live upgrade command. Use an independent, private copy of the complete stopped proxy state. The copied `history.index` must be fully checkpointed, with no nonempty WAL or rollback journal. Never delete a WAL to meet that precondition: it can contain committed history. Leave the original state and its backup untouched.

With Python 3 and SQLite, choose a **new** staging directory name outside the frozen copy, under a trusted parent with enough free space for the rebuilt index and reserve. The tool creates the directory with owner-only permissions. Pin the copied index's SHA-256, then run:

```sh
python3 -B scripts/stage_history_rowid.py /path/to/copy/history.index /path/to/staging \
  --sha256 <expected-sha256>
```

The script writes only `history.index` in that staging directory. It refuses an existing destination or unexpected schema, compares every logical row, and keeps the source unchanged. Keep the output private: it still contains retained prompts and receipt evidence. To rehearse, combine the staged index with copies of the other state files in a separate test directory. Do not bring old SQLite WAL or SHM files along with the replacement index. Verify dashboard/accounting views and restart before considering any separately managed cutover. No request timeout, history retention or accounting policy changes with the layout.
