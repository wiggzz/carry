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
    disk_limit: u64,
    projected_bytes: std::cell::Cell<u64>,
    reserved_bytes: std::cell::Cell<u64>,
}
// A paid-only transaction writes at most a bounded checkpoint and accounting
// observation, two small sequence rows, and a staged checkpoint file. Ordinary
// publications cannot spend this headroom, including their own owner's promise.
fn write_charge(bytes: u64) -> u64 {
    bytes.saturating_mul(4).saturating_add(16384)
}
fn settlement_reserve(max_record: u64) -> u64 {
    write_charge(max_record)
        .saturating_mul(2)
        .saturating_add(write_charge(128).saturating_mul(2))
        .saturating_add(max_record.saturating_mul(2))
        .saturating_add(65536)
}
// Maintenance has its own bounded promise. Ordinary writes and paid settlement
// both leave it intact, so expiry never depends on admitting another owner.
fn maintenance_reserve(max_record: u64, disk_limit: u64) -> u64 {
    write_charge(max_record)
        .saturating_add(65536)
        .min(disk_limit / 2)
}
fn full() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::StorageFull,
        "durable history disk admission refused; retained evidence was not evicted",
    )
}
fn disk_bytes(directory: &Path, depth: usize) -> std::io::Result<u64> {
    if depth > 16 {
        return Err(full());
    }
    let mut bytes = 0u64;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_file() {
            bytes = bytes.saturating_add(entry.metadata()?.len());
        } else if kind.is_dir() {
            bytes = bytes
                .saturating_add(entry.metadata()?.len())
                .saturating_add(disk_bytes(&entry.path(), depth + 1)?);
        }
    }
    Ok(bytes)
}
#[cfg(unix)]
fn free_bytes(directory: &Path) -> std::io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path =
        std::ffi::CString::new(directory.as_os_str().as_bytes()).map_err(std::io::Error::other)?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: NUL-terminated trusted directory and an output-sized writable slot.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}
pub(super) fn admit_owner(service: &Service, owners: usize) -> std::io::Result<()> {
    let reserve = settlement_reserve(service.config.history_max_bytes as u64)
        .saturating_mul(owners as u64)
        .saturating_add(maintenance_reserve(
            service.config.history_max_bytes as u64,
            service.config.history_disk_bytes,
        ));
    if disk_bytes(&service.config.state_dir, 0)?.saturating_add(reserve)
        > service.config.history_disk_bytes
    {
        return Err(full());
    }
    #[cfg(unix)]
    if free_bytes(&service.config.state_dir)? < reserve.saturating_add(16 * 1024 * 1024) {
        return Err(full());
    }
    Ok(())
}
pub(super) fn maintain(service: &Service, pins: HashSet<String>) -> std::io::Result<()> {
    if !service.config.state_dir.join("history.index").exists() {
        return Ok(());
    }
    Store::open(service, pins, true)?.maintain()
}
pub(super) fn initialize(service: &Service, pins: HashSet<String>) -> std::io::Result<()> {
    let path = service.config.state_dir.join("history.index");
    if !path.exists() {
        return Ok(());
    }
    match Store::open(service,pins.clone(),false) {
        Ok(_)=>Ok(()),
        Err(e) if e.get_ref().and_then(|e|e.downcast_ref::<rusqlite::Error>()).is_some_and(|e|matches!(e,rusqlite::Error::SqliteFailure(code,_) if code.code==rusqlite::ErrorCode::NotADatabase))=>import_v4(service,pins),
        Err(e)=>Err(e),
    }
}
fn import_v4(service: &Service, pins: HashSet<String>) -> std::io::Result<()> {
    use std::io::Read;
    let path = service.config.state_dir.join("history.index");
    let mut bytes = Vec::new();
    File::open(&path)?
        .take(service.config.history_max_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > service.config.history_max_bytes as usize {
        return Err(full());
    }
    let invalid = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "unsupported or invalid predecessor history",
        )
    };
    let index: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if index["format"] != "carry-prefix-history-v4" {
        return Err(invalid());
    }
    validate_predecessor(&index).map_err(|_| invalid())?;
    let temporary = service.config.state_dir.join(".history-import.tmp");
    if std::fs::symlink_metadata(&temporary).is_ok_and(|m| !m.is_file()) {
        return Err(invalid());
    }
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
            }
        }
    }
    let _cleanup = Cleanup(temporary.clone());
    if temporary.exists() {
        std::fs::remove_file(&temporary)?;
    }
    let store = Store::open_path(service, pins, true, &temporary)?;
    store.begin()?;
    let at = index["tips"]
        .as_object()
        .unwrap()
        .values()
        .chain(index["reviews"].as_object().unwrap().values())
        .chain(index["transcripts"].as_object().unwrap().values())
        .filter_map(|v| v["at"].as_u64())
        .max()
        .unwrap_or(0);
    for kind in ["nodes", "tips", "reviews", "transcripts", "snapshots"] {
        for (key, value) in index[kind].as_object().unwrap() {
            let owner = if kind == "snapshots" {
                key.as_str()
            } else {
                value["session"].as_str().unwrap_or("")
            };
            let age = if kind == "snapshots" {
                index["tips"]
                    .as_object()
                    .unwrap()
                    .values()
                    .filter(|v| v["session"] == *key)
                    .filter_map(|v| v["at"].as_u64())
                    .max()
                    .ok_or_else(invalid)?
            } else {
                value["at"].as_u64().unwrap_or(at)
            };
            store.put(kind, key, value, age, owner)?;
            if kind == "snapshots" {
                let session: Session =
                    serde_json::from_value(value.clone()).map_err(|_| invalid())?;
                // Decode drops the runtime validation flag. Rebind the same
                // successful-window authority as normal load before observing
                // any historical money or coverage in the replacement store.
                let session =
                    recover_success_prefix(service, key, session).map_err(|_| invalid())?;
                store.put(
                    "owners",
                    key,
                    &json!({"shared":session.history_shared}),
                    age,
                    key,
                )?;
                store.account(key, &session)?;
            }
        }
    }
    store.put(
        "upstream_cache",
        "current",
        &index["upstream_cache"],
        at,
        "",
    )?;
    store.commit()?;
    drop(store);
    File::open(&temporary)?.sync_all()?;
    std::fs::rename(&temporary, &path)?;
    File::open(&service.config.state_dir)?.sync_all()
}
// The bounded predecessor generation is validated as a graph before any staging
// database is created. A successful rename must never replace readable JSON by
// rows that the new reader cannot decode or whose ancestry is incomplete.
fn validate_predecessor(value: &Value) -> Result<(), ()> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Node {
        parent: String,
        value: Value,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Predecessor {
        #[serde(rename = "format")]
        _format: String,
        nodes: BTreeMap<String, Node>,
        tips: BTreeMap<String, Reference>,
        reviews: BTreeMap<String, semantic::Record>,
        #[serde(default)]
        transcripts: BTreeMap<String, Transcript>,
        #[serde(default)]
        snapshots: BTreeMap<String, Session>,
        #[serde(rename = "upstream_cache")]
        _cache: Vec<CacheEvidence>,
    }
    let index: Predecessor = serde_json::from_value(value.clone()).map_err(|_| ())?;
    let mut roots: HashMap<String, (String, usize)> = HashMap::new();
    for (key, node) in &index.nodes {
        if *key != super::hash(&(&node.parent, &node.value)) {
            return Err(());
        }
    }
    // Memoize each ancestry once. Migration is bounded by predecessor bytes,
    // rather than repeatedly walking every shared prefix of every record.
    for key in index.nodes.keys() {
        let mut cursor = key.as_str();
        let mut trail = Vec::new();
        let mut seen = HashSet::new();
        while !roots.contains_key(cursor) {
            let Some(node) = index.nodes.get(cursor) else {
                break;
            };
            if !seen.insert(cursor) {
                return Err(());
            }
            trail.push(cursor);
            cursor = &node.parent;
        }
        let (root, mut depth) = roots.get(cursor).cloned().unwrap_or((cursor.to_owned(), 0));
        if root.len() == 64 && root.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(());
        }
        for key in trail.into_iter().rev() {
            depth += 1;
            roots.insert(key.to_owned(), (root.clone(), depth));
        }
    }
    let extent = |key: &str, namespace: &str| -> Result<usize, ()> {
        if key == namespace {
            return Ok(0);
        }
        let (root, depth) = roots.get(key).ok_or(())?;
        if root != namespace {
            return Err(());
        }
        Ok(*depth)
    };
    for (key, reference) in &index.tips {
        if reference.session.is_empty() || reference.namespace.is_empty() {
            return Err(());
        }
        extent(key, &reference.namespace)?;
    }
    for (key, reference) in &index.transcripts {
        if reference.session.is_empty() || reference.external_id.is_empty() {
            return Err(());
        }
        extent(&reference.frontier, &reference.namespace)?;
        if !["response", "conversation"]
            .into_iter()
            .any(|kind| *key == super::hash(&(&reference.namespace, kind, &reference.external_id)))
        {
            return Err(());
        }
    }
    for (key, record) in &index.reviews {
        let depth = roots.get(key).map(|(_, depth)| *depth).unwrap_or(0);
        if depth != record.len
            || (depth == 0 && key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(());
        }
    }
    for (id, snapshot) in &index.snapshots {
        let namespace = snapshot.history_namespace.as_deref().ok_or(())?;
        if !snapshot.history_shared
            || !matches!(snapshot.version, 0 | 3)
            || !index
                .tips
                .values()
                .any(|r| r.session == *id && r.namespace == namespace)
        {
            return Err(());
        }
        let depth = extent(
            snapshot.history_frontier.as_deref().unwrap_or(namespace),
            namespace,
        )?;
        if depth != snapshot.history.len() {
            return Err(());
        }
    }
    Ok(())
}

impl Store {
    fn charge(&self, bytes: u64) -> std::io::Result<()> {
        // Reserve index/overflow pages and WAL copies before SQLite can spill.
        // Refusal rolls back the transaction; it never deletes a younger row.
        let charge = write_charge(bytes);
        let next = self.projected_bytes.get().saturating_add(charge);
        let reserved = self.reserved_bytes.get();
        if next.saturating_add(reserved) > self.disk_limit {
            return Err(full());
        }
        #[cfg(unix)]
        if free_bytes(&self.directory)?
            < charge
                .saturating_add(reserved)
                .saturating_add(16 * 1024 * 1024)
        {
            return Err(full());
        }
        self.projected_bytes.set(next);
        Ok(())
    }
    pub(super) fn open(
        service: &Service,
        pins: HashSet<String>,
        write: bool,
    ) -> std::io::Result<Self> {
        Self::open_path(
            service,
            pins,
            write,
            &service.config.state_dir.join("history.index"),
        )
    }
    fn open_path(
        service: &Service,
        pins: HashSet<String>,
        write: bool,
        path: &Path,
    ) -> std::io::Result<Self> {
        let exists = match std::fs::symlink_metadata(path) {
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
            drop(private_file(path, false)?);
        }
        let connection = if exists && !write {
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        } else if exists || write {
            Connection::open(path)
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
        if version != 1 && (exists || version != 0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unsupported history store version",
            ));
        }
        if version == 0 {
            connection.execute_batch("PRAGMA auto_vacuum=INCREMENTAL; PRAGMA journal_mode=WAL; CREATE TABLE records(kind TEXT NOT NULL, key TEXT NOT NULL, data BLOB NOT NULL, at INTEGER NOT NULL, owner TEXT NOT NULL, PRIMARY KEY(kind,key)) WITHOUT ROWID;
                CREATE INDEX records_age ON records(kind,at);
                CREATE INDEX records_owner ON records(kind,owner);
                PRAGMA user_version=1;").map_err(error)?;
        }
        if write {
            let pages = (service.config.history_disk_bytes / 4096).max(16);
            connection
                .pragma_update(None, "max_page_count", pages)
                .map_err(error)?;
        }
        Ok(Self {
            connection,
            max_record: service.config.history_max_bytes as usize,
            max_nodes: service.config.history_max_nodes as usize,
            at: now(),
            age: service.config.history_max_age_secs,
            reserved_bytes: std::cell::Cell::new(
                settlement_reserve(service.config.history_max_bytes as u64)
                    .saturating_mul(pins.len() as u64)
                    .saturating_add(maintenance_reserve(
                        service.config.history_max_bytes as u64,
                        service.config.history_disk_bytes,
                    )),
            ),
            pins,
            accounting_age: service.config.accounting_max_age_secs,
            directory: service.config.state_dir.clone(),
            retired: Default::default(),
            disk_limit: service.config.history_disk_bytes,
            projected_bytes: Default::default(),
        })
    }
    pub(super) fn get<T: DeserializeOwned>(
        &self,
        kind: &str,
        key: &str,
    ) -> std::io::Result<Option<T>> {
        let bytes: Option<(usize, Option<Vec<u8>>)> = self
            .connection
            .query_row(
                "SELECT length(data), CASE WHEN length(data)<=?3 THEN data ELSE NULL END FROM records WHERE kind=?1 AND key=?2",
                params![kind, key, self.max_record as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(error)?;
        let Some((length, data)) = bytes else {
            return Ok(None);
        };
        if length > self.max_record {
            return Err(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                "retained history record exceeds working-set bound",
            ));
        }
        let data = data.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "missing history record data",
            )
        })?;
        serde_json::from_slice(&data)
            .map(Some)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
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
        self.charge(bytes.len() as u64)?;
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
        let mut provenance = session
            .request_evidence
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?;
        if let Some(Value::Object(fields)) = &mut provenance {
            // Retain the journal binding and latest fact, not the entire suffix.
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
        // Identify frozen numeric coverage, not the mutable dispatch/journal
        // cursor. Corrected coverage is a new observation even when native
        // money is unchanged; dispatch alone is not a settled observation.
        let identity = super::hash(&(
            id,
            &session.primary,
            &session.shadow,
            session.completed_requests,
            session.failed_primaries,
            &estimate,
            crate::proxy_savings::accounting_policy(),
        ));
        if self.contains("accounted", &identity)? {
            return Ok(());
        }
        let invalid = || {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid accounting sequence authority",
            )
        };
        let clock = self.get::<Value>("metadata", "accounting_clock")?;
        let (previous, previous_at) = if let Some(clock) = clock {
            (
                clock["sequence"].as_u64().ok_or_else(invalid)?,
                clock["at"].as_u64().ok_or_else(invalid)?,
            )
        } else {
            let existing: bool = self
                .connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM records WHERE kind='accounting')",
                    [],
                    |r| r.get(0),
                )
                .map_err(error)?;
            if existing {
                return Err(invalid());
            }
            (0, 0)
        };
        let sequence = previous.checked_add(1).ok_or_else(full)?;
        let order_at = self.at.max(previous_at);
        let key = format!("{sequence:064x}");
        self.put(
            "accounted",
            &identity,
            &json!({"sequence":sequence}),
            order_at,
            id,
        )?;
        self.put(
            "metadata",
            "accounting_clock",
            &json!({"sequence":sequence,"at":order_at}),
            order_at,
            "",
        )?;
        self.put("accounting", &key, &json!({
            "version":1, "owner":id, "at":self.at, "sequence":sequence, "order_at":order_at,
            "basis":"immutable_cumulative_observation_not_additive_receipts",
            "producer_version":env!("CARGO_PKG_VERSION"),
            "estimator_policy":crate::proxy_savings::accounting_policy(),
            "primary":session.primary, "reviewer":session.shadow,
            "completed_requests":session.completed_requests, "failed_primaries":session.failed_primaries,
            "estimate":estimate, "numeric_provenance":provenance,
            "last_primary_dispatch":session.last_primary_dispatch,
            "last_reviewer_dispatch":session.last_reviewer_dispatch
        }), order_at, id)
    }
    pub(super) fn accounting_page(
        &self,
        after_at: u64,
        after_key: &str,
        limit: usize,
    ) -> std::io::Result<Value> {
        let mut statement=self.connection.prepare("SELECT key,at,length(data),CASE WHEN length(data)<=?1 THEN data ELSE NULL END FROM records WHERE kind='accounting' AND at>=?2 AND (at>?3 OR (at=?3 AND key>?4)) ORDER BY at,key LIMIT ?5").map_err(error)?;
        let mut rows = statement
            .query(params![
                self.max_record as u64,
                self.at.saturating_sub(self.accounting_age),
                after_at,
                after_key,
                (limit + 1) as u64
            ])
            .map_err(error)?;
        let mut records = Vec::new();
        let mut bytes = 0usize;
        let mut next = None;
        let mut more = false;
        while let Some(row) = rows.next().map_err(error)? {
            let length: usize = row.get(2).map_err(error)?;
            if length > self.max_record {
                return Err(std::io::Error::other(
                    "accounting record exceeds working-set bound",
                ));
            }
            if records.len() == limit
                || (!records.is_empty() && bytes.saturating_add(length) > self.max_record)
            {
                more = true;
                break;
            }
            let data: Vec<u8> = row.get(3).map_err(error)?;
            records.push(serde_json::from_slice::<Value>(&data)?);
            bytes += data.len();
            next = Some(
                json!({"after_at":row.get::<_,u64>(1).map_err(error)?,"after_key":row.get::<_,String>(0).map_err(error)?}),
            );
        }
        Ok(
            json!({"records":records,"has_more":more,"next":if more{next}else{None},"basis":"immutable cumulative observations; select one observation per owner and time, never sum snapshots", "accounting_retention_secs":self.accounting_age,"raw_history_retention_secs":self.age}),
        )
    }
    fn expire(&self, kind: &str, cutoff: u64, budget: &mut u64) -> std::io::Result<Vec<String>> {
        // Protect ancestry by reachability, not by rewriting its original age.
        // Live references can point to nodes older than their own publication
        // (notably a paid-only settlement of an old pinned checkpoint).
        let mut statement=self.connection.prepare("WITH RECURSIVE ancestry(key) AS (
            SELECT json_extract(CAST(data AS TEXT),'$.history_frontier') FROM records WHERE kind='snapshots' AND (at>?2 OR owner IN (SELECT id FROM pins))
            UNION SELECT key FROM records WHERE kind IN ('tips','reviews') AND (at>?2 OR owner IN (SELECT id FROM pins))
            UNION SELECT json_extract(CAST(data AS TEXT),'$.frontier') FROM records WHERE kind='transcripts' AND (at>?2 OR owner IN (SELECT id FROM pins))
            UNION SELECT json_extract(CAST(r.data AS TEXT),'$.parent') FROM records r JOIN ancestry a ON r.key=a.key WHERE r.kind='nodes')
            SELECT key,length(data) FROM records WHERE kind=?1 AND at<=?2 AND (?1 IN ('accounting','accounted') OR owner NOT IN (SELECT id FROM pins)) AND (?1!='nodes' OR key NOT IN (SELECT key FROM ancestry WHERE key IS NOT NULL)) ORDER BY at,key LIMIT 8").map_err(error)?;
        let entries = statement
            .query_map(params![kind, cutoff], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?))
            })
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        let mut keys = Vec::new();
        for (key, bytes) in entries {
            let charge = write_charge(bytes);
            if charge > *budget {
                break;
            }
            self.charge(bytes)?;
            self.connection
                .execute(
                    "DELETE FROM records WHERE kind=?1 AND key=?2",
                    params![kind, key],
                )
                .map_err(error)?;
            *budget -= charge;
            keys.push(key);
        }
        Ok(keys)
    }
    fn collect(&self, budget: &mut u64) -> std::io::Result<()> {
        self.connection
            .execute_batch("CREATE TEMP TABLE IF NOT EXISTS pins(id TEXT PRIMARY KEY) WITHOUT ROWID; DELETE FROM pins;")
            .map_err(error)?;
        for id in &self.pins {
            self.connection
                .execute("INSERT INTO pins VALUES(?1)", [id])
                .map_err(error)?;
        }
        if let Some(cutoff) = self.at.checked_sub(self.age) {
            *self.retired.borrow_mut() = self.expire("snapshots", cutoff, budget)?;
            for kind in ["tips", "transcripts", "reviews", "nodes"] {
                self.expire(kind, cutoff, budget)?;
            }
        }
        if let Some(cutoff) = self.at.checked_sub(self.accounting_age) {
            self.expire("accounting", cutoff, budget)?;
            self.expire("accounted", cutoff, budget)?;
        }
        Ok(())
    }
    fn maintain(&self) -> std::io::Result<()> {
        let allowance = maintenance_reserve(self.max_record as u64, self.disk_limit);
        let reserved = self.reserved_bytes.get();
        // Spend maintenance's promise only; every paid owner keeps its full
        // settlement reserve. Restore the normal reservation even on failure.
        self.reserved_bytes.set(reserved.saturating_sub(allowance));
        let result = (|| {
            self.start()?;
            let mut budget = allowance.saturating_sub(65536 + write_charge(0));
            self.collect(&mut budget)?;
            self.commit()?;
            // Without TRUNCATE a checkpoint merely recycles the WAL. Vacuum
            // reclaims a bounded number of freelist pages; active read views
            // may defer reclamation, never force eviction or exceed admission.
            self.connection
                .busy_timeout(Duration::ZERO)
                .map_err(error)?;
            // This pragma yields one row per reclaimed page. execute_batch
            // steps once, so it would reclaim only one page despite the bound.
            let mut vacuum = self
                .connection
                .prepare("PRAGMA incremental_vacuum(8)")
                .map_err(error)?;
            let mut rows = vacuum.query([]).map_err(error)?;
            while rows.next().map_err(error)?.is_some() {}
            drop(rows);
            drop(vacuum);
            self.connection
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .map_err(error)
        })();
        self.reserved_bytes.set(reserved);
        self.connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(error)?;
        result
    }
    pub(super) fn begin_settlement(&self, owner: &str) -> std::io::Result<()> {
        // Only a bounded paid-only publication may use its owner's promise.
        // Do not run ancestry maintenance inside this emergency transaction.
        if self.pins.contains(owner) {
            self.reserved_bytes.set(
                self.reserved_bytes
                    .get()
                    .saturating_sub(settlement_reserve(self.max_record as u64)),
            );
        }
        self.start()
    }
    fn start(&self) -> std::io::Result<()> {
        self.projected_bytes
            .set(disk_bytes(&self.directory, 0)?.saturating_add(65536));
        self.charge(0)?;
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(error)
    }
    pub(super) fn begin(&self) -> std::io::Result<()> {
        self.maintain()?;
        self.start()
    }
    pub(super) fn commit(&self) -> std::io::Result<()> {
        self.connection.execute_batch("COMMIT").map_err(error)?;
        for id in self.retired.take() {
            if id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
                let _ = std::fs::remove_file(self.directory.join(format!("{id}.json")));
            }
        }
        Ok(())
    }
}
