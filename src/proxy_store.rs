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
    accounting_age: u64,
    directory: PathBuf,
    retired: std::cell::RefCell<Vec<String>>,
}
impl Store {
    pub(super) fn open(
        service: &Service,
        pins: HashSet<String>,
        write: bool,
    ) -> std::io::Result<Self> {
        let path = service.config.state_dir.join("history.index");
        let exists = match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => true,
            Ok(_) => {
                return Err(std::io::Error::other(
                    "history authority is not a regular file",
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e),
        };
        if !exists && write {
            drop(private_file(&path, false)?);
        }
        let connection = if exists || write {
            Connection::open(&path)
        } else {
            Connection::open_in_memory()
        }
        .map_err(error)?;
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
        if version == 0 {
            connection.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE records(kind TEXT NOT NULL, key TEXT NOT NULL, data BLOB NOT NULL, at INTEGER NOT NULL, owner TEXT NOT NULL, PRIMARY KEY(kind,key)) WITHOUT ROWID;
                CREATE INDEX records_age ON records(kind,at);
                CREATE INDEX records_owner ON records(kind,owner);
                PRAGMA user_version=1;").map_err(error)?;
        }
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
            accounting_age: service.config.accounting_max_age_secs,
            directory: service.config.state_dir.clone(),
            retired: Default::default(),
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
        let keys = super::path(namespace, input);
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
    pub(super) fn account(&self, id: &str, session: &Session) -> std::io::Result<()> {
        if session.primary.calls == 0 && session.shadow.calls == 0 {
            return Ok(());
        }
        // Cumulative observations are immutable and deduplicated by native paid
        // state, not by checkpoint time. They are NOT additive receipt amounts.
        let key = super::hash(&(id, &session.primary, &session.shadow));
        if self.contains("accounting", &key)? {
            return Ok(());
        }
        let mut provenance = session
            .request_evidence
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?;
        if let Some(Value::Object(fields)) = &mut provenance {
            // Keep the independently streamed numeric journal's authority and
            // latest frozen facts, never copy an entire suffix at every sample.
            let last = fields
                .remove("observations")
                .and_then(|v| v.as_array().and_then(|a| a.last()).cloned());
            fields.insert("latest_observation".into(), last.unwrap_or(Value::Null));
            fields.remove("numeric_pending");
        }
        let estimate = session
            .request_evidence
            .as_ref()
            .map(|e| e.snapshot().dashboard(false));
        self.put("accounting", &key, &json!({
            "version":1, "owner":id, "at":self.at,
            "basis":"immutable_cumulative_observation_not_additive_receipts",
            "producer_version":env!("CARGO_PKG_VERSION"),
            "estimator_policy":crate::proxy_savings::accounting_policy(),
            "primary":session.primary, "reviewer":session.shadow,
            "completed_requests":session.completed_requests, "failed_primaries":session.failed_primaries,
            "estimate":estimate, "numeric_provenance":provenance,
            "last_primary_dispatch":session.last_primary_dispatch,
            "last_reviewer_dispatch":session.last_reviewer_dispatch
        }), self.at, id)
    }
    fn collect(&self) -> std::io::Result<()> {
        self.connection
            .execute_batch("CREATE TEMP TABLE pins(id TEXT PRIMARY KEY) WITHOUT ROWID;")
            .map_err(error)?;
        for id in &self.pins {
            self.connection
                .execute("INSERT INTO pins VALUES(?1)", [id])
                .map_err(error)?;
        }
        if let Some(cutoff) = self.at.checked_sub(self.age) {
            // Old in-flight owners retain their ancestry until paid settlement.
            // This walk is only for expired pinned checkpoints, not the corpus.
            self.connection.execute("WITH RECURSIVE ancestry(key) AS (
                SELECT json_extract(CAST(data AS TEXT),'$.history_frontier') FROM records
                WHERE kind='snapshots' AND at<=?1 AND owner IN (SELECT id FROM pins)
                UNION SELECT json_extract(CAST(r.data AS TEXT),'$.parent') FROM records r JOIN ancestry a ON r.key=a.key WHERE r.kind='nodes')
                UPDATE records SET at=max(at,?2) WHERE kind='nodes' AND key IN (SELECT key FROM ancestry)",params![cutoff,self.at]).map_err(error)?;
            let mut statement=self.connection.prepare("SELECT key FROM records WHERE kind='snapshots' AND at<=?1 AND owner NOT IN (SELECT id FROM pins) LIMIT 128").map_err(error)?;
            let retired = statement
                .query_map([cutoff], |r| r.get::<_, String>(0))
                .map_err(error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(error)?;
            for id in &retired {
                self.connection
                    .execute(
                        "DELETE FROM records WHERE kind='snapshots' AND key=?1",
                        [id],
                    )
                    .map_err(error)?;
            }
            *self.retired.borrow_mut() = retired;
            for kind in ["nodes", "tips", "transcripts", "reviews"] {
                self.connection.execute("DELETE FROM records WHERE kind=?1 AND key IN (SELECT key FROM records WHERE kind=?1 AND at<=?2 AND owner NOT IN (SELECT id FROM pins) LIMIT 512)",params![kind,cutoff]).map_err(error)?;
            }
        }
        if let Some(cutoff) = self.at.checked_sub(self.accounting_age) {
            self.connection.execute("DELETE FROM records WHERE kind='accounting' AND key IN (SELECT key FROM records WHERE kind='accounting' AND at<=?1 LIMIT 512)",[cutoff]).map_err(error)?;
        }
        Ok(())
    }
    pub(super) fn begin(&self) -> std::io::Result<()> {
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(error)?;
        self.collect()
    }
    pub(super) fn commit(&self) -> std::io::Result<()> {
        self.connection.execute_batch("COMMIT").map_err(error)?;
        for id in self.retired.borrow().iter() {
            if id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
                let _ = std::fs::remove_file(self.directory.join(format!("{id}.json")));
            }
        }
        Ok(())
    }
}
