//! Disk authority for exact history. The page cache and each decoded record are
//! bounded; unrelated sessions are never deserialized on a request. SQLite's
//! rollback journal commits nodes, bindings and checkpoints as one generation.
use super::*;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};

pub(super) fn error(e: rusqlite::Error) -> std::io::Error {
    let kind = match &e {
        rusqlite::Error::SqliteFailure(code, _) if code.code == rusqlite::ErrorCode::DiskFull => {
            std::io::ErrorKind::StorageFull
        }
        _ => std::io::ErrorKind::InvalidData,
    };
    std::io::Error::new(kind, e)
}

pub(super) struct Store {
    pub(super) connection: Connection,
    max_record: usize,
    max_nodes: usize,
    at: u64,
    age: u64,
    pins: HashSet<String>,
}
impl Store {
    pub(super) fn open(service: &Service, pins: HashSet<String>) -> std::io::Result<Self> {
        let path = service.config.state_dir.join("history.index");
        // Do not follow a planted file, or mistake an unsupported old format for
        // an empty store. A separate explicit import owns legacy generations.
        if let Ok(metadata) = std::fs::symlink_metadata(&path)
            && !metadata.is_file()
        {
            return Err(std::io::Error::other(
                "history authority is not a regular file",
            ));
        }
        if !path.exists() {
            drop(private_file(&path, false)?);
        }
        let connection = Connection::open(&path).map_err(error)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(error)?;
        connection.execute_batch("PRAGMA cache_size=-2048; PRAGMA mmap_size=0; PRAGMA temp_store=FILE; PRAGMA synchronous=FULL;").map_err(error)?;
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(error)?;
        if version != 0 && version != 1 {
            return Err(std::io::Error::other("unsupported history store version"));
        }
        connection.execute_batch("CREATE TABLE IF NOT EXISTS records(kind TEXT NOT NULL, key TEXT NOT NULL, data BLOB NOT NULL, at INTEGER NOT NULL, owner TEXT NOT NULL, PRIMARY KEY(kind,key)) WITHOUT ROWID;
            CREATE INDEX IF NOT EXISTS records_age ON records(kind,at);
            CREATE INDEX IF NOT EXISTS records_owner ON records(kind,owner);
            PRAGMA user_version=1;").map_err(error)?;
        // This is a disk-admission ceiling, never an eviction target. A failed
        // transaction leaves every previously committed record intact.
        let pages = (service.config.history_disk_bytes / 4096).max(16);
        connection
            .pragma_update(None, "max_page_count", pages)
            .map_err(error)?;
        Ok(Self {
            connection,
            max_record: service.config.history_max_bytes as usize,
            max_nodes: service.config.history_max_nodes as usize,
            at: now(),
            age: service.config.history_max_age_secs,
            pins,
        })
    }
    pub(super) fn get<T: DeserializeOwned>(
        &self,
        kind: &str,
        key: &str,
    ) -> std::io::Result<Option<T>> {
        let bytes: Option<Vec<u8>> = self
            .connection
            .query_row(
                "SELECT data FROM records WHERE kind=?1 AND key=?2 AND length(data)<=?3",
                params![kind, key, self.max_record as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        bytes
            .map(|b| serde_json::from_slice(&b).map_err(std::io::Error::other))
            .transpose()
    }
    pub(super) fn live(&self, kind: &str, key: &str) -> std::io::Result<bool> {
        let row: Option<(u64, String)> = self
            .connection
            .query_row(
                "SELECT at,owner FROM records WHERE kind=?1 AND key=?2",
                params![kind, key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(error)?;
        Ok(row.is_some_and(|(at, owner)| {
            self.pins.contains(&owner) || at > self.at || self.at - at < self.age
        }))
    }
    pub(super) fn contains(&self, kind: &str, key: &str) -> std::io::Result<bool> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM records WHERE kind=?1 AND key=?2)",
                params![kind, key],
                |r| r.get(0),
            )
            .map_err(error)
    }
    pub(super) fn put(
        &self,
        kind: &str,
        key: &str,
        value: &impl Serialize,
        at: u64,
        owner: &str,
    ) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(value)?;
        if bytes.len() > self.max_record {
            return Err(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                "history record exceeds working-set bound",
            ));
        }
        self.connection.execute("INSERT INTO records VALUES(?1,?2,?3,?4,?5) ON CONFLICT(kind,key) DO UPDATE SET data=excluded.data,at=excluded.at,owner=excluded.owner",
            params![kind,key,bytes,at,owner]).map_err(error)?;
        Ok(())
    }
    pub(super) fn insert(&self, namespace: &str, input: &[Value]) -> std::io::Result<Vec<String>> {
        if input.len() > self.max_nodes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                "history path exceeds working-set bound",
            ));
        }
        let keys = history::path(namespace, input);
        let mut parent = namespace.to_owned();
        for (key, value) in keys.iter().zip(input) {
            self.put(
                "nodes",
                key,
                &json!({"parent":parent,"value":value}),
                self.at,
                "",
            )?;
            parent = key.clone();
        }
        Ok(keys)
    }
    pub(super) fn ancestry(&self, namespace: &str, frontier: &str) -> std::io::Result<Vec<Value>> {
        let mut key = frontier.to_owned();
        let mut values = Vec::new();
        let mut bytes = 0usize;
        while key != namespace {
            if values.len() >= self.max_nodes {
                return Err(std::io::Error::other(
                    "history ancestry exceeds working-set bound",
                ));
            }
            let node: Value = self
                .get("nodes", &key)?
                .ok_or_else(|| std::io::Error::other("history ancestry unavailable"))?;
            bytes = bytes.saturating_add(serde_json::to_vec(&node)?.len());
            if bytes > self.max_record {
                return Err(std::io::Error::other("history ancestry exceeds byte bound"));
            }
            values.push(node["value"].clone());
            key = node["parent"]
                .as_str()
                .ok_or_else(|| std::io::Error::other("invalid ancestry"))?
                .to_owned();
        }
        values.reverse();
        Ok(values)
    }
    pub(super) fn begin(&self) -> std::io::Result<()> {
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(error)
    }
    pub(super) fn commit(&self) -> std::io::Result<()> {
        self.connection.execute_batch("COMMIT").map_err(error)
    }
}
