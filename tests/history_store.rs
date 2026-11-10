// Test-only database inspection/staging. Existing behavioral graders inspect
// the actual authority rows rather than a diagnostic checkpoint or mock store.
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::path::Path;
pub fn read_index(path: &Path) -> Vec<u8> {
    let raw = std::fs::read(path).unwrap();
    if !raw.starts_with(b"SQLite format 3") {
        return raw;
    }
    let db = Connection::open(path).unwrap();
    let mut index = json!({"format":"carry-prefix-history-v4","nodes":{},"tips":{},"reviews":{},"transcripts":{},"snapshots":{},"upstream_cache":[]});
    let mut statement = db.prepare("SELECT kind,key,data FROM records").unwrap();
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })
        .unwrap();
    for row in rows {
        let (kind, key, bytes) = row.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        if kind == "upstream_cache" {
            index[&kind] = value;
        } else if index.get(&kind).is_some() {
            index[&kind][&key] = value;
        }
    }
    serde_json::to_vec(&index).unwrap()
}
pub fn write_index(path: &Path, bytes: &[u8]) {
    let index: Value = serde_json::from_slice(bytes).unwrap();
    if index["format"] != "carry-prefix-history-v4" {
        std::fs::write(path, bytes).unwrap();
        return;
    }
    if !std::fs::read(path)
        .unwrap_or_default()
        .starts_with(b"SQLite format 3")
    {
        let _ = std::fs::remove_file(path);
    }
    let mut db = Connection::open(path).unwrap();
    db.execute_batch("CREATE TABLE IF NOT EXISTS records(kind TEXT NOT NULL,key TEXT NOT NULL,data BLOB NOT NULL,at INTEGER NOT NULL,owner TEXT NOT NULL,PRIMARY KEY(kind,key)) WITHOUT ROWID; PRAGMA user_version=1;").unwrap();
    let tx = db.transaction().unwrap();
    tx.execute("DELETE FROM records", []).unwrap();
    let at = ["tips", "transcripts", "reviews"]
        .iter()
        .flat_map(|kind| index[kind].as_object().into_iter().flat_map(|v| v.values()))
        .filter_map(|v| v["at"].as_u64())
        .max()
        .unwrap_or(0);
    for kind in ["nodes", "tips", "transcripts", "reviews", "snapshots"] {
        for (key, value) in index[kind].as_object().into_iter().flatten() {
            let owner =
                value["session"]
                    .as_str()
                    .unwrap_or(if kind == "snapshots" { key } else { "" });
            let stamp = value["at"]
                .as_u64()
                .or_else(|| {
                    index["tips"]
                        .as_object()?
                        .values()
                        .find(|r| r["session"] == owner)?["at"]
                        .as_u64()
                })
                .unwrap_or(at);
            tx.execute(
                "INSERT INTO records VALUES(?1,?2,?3,?4,?5)",
                params![kind, key, serde_json::to_vec(value).unwrap(), stamp, owner],
            )
            .unwrap();
        }
    }
    tx.execute(
        "INSERT INTO records VALUES('upstream_cache','current',?1,?2,'')",
        params![serde_json::to_vec(&index["upstream_cache"]).unwrap(), at],
    )
    .unwrap();
    tx.commit().unwrap();
}
pub fn checkpoint_fault(path: &Path, enabled: bool) {
    let db = Connection::open(path).unwrap();
    db.execute_batch(if enabled {"CREATE TRIGGER checkpoint_fault BEFORE INSERT ON records WHEN new.kind='snapshots' BEGIN SELECT RAISE(ABORT,'fixture checkpoint failure'); END"}else{"DROP TRIGGER checkpoint_fault"}).unwrap();
}
pub fn publication_fault(path: &Path, enabled: bool) {
    let db = Connection::open(path).unwrap();
    db.execute_batch(if enabled {"CREATE TRIGGER publication_fault BEFORE INSERT ON records WHEN new.kind='reviews' BEGIN SELECT RAISE(ABORT,'fixture publication failure'); END"}else{"DROP TRIGGER publication_fault"}).unwrap();
}
