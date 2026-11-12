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

/// The same capacity policy governs SQL and every external numeric/checkpoint
/// write. It contains no provider, authentication, or client configuration.
#[derive(Clone, Debug)]
pub(crate) struct Policy {
    directory: PathBuf,
    max_record: usize,
    max_nodes: usize,
    age: u64,
    accounting_age: u64,
    disk_limit: u64,
}
impl Policy {
    pub(crate) fn new(config: &ProxyCli) -> Self {
        Self {
            directory: config.state_dir.clone(),
            max_record: config.history_max_bytes as usize,
            max_nodes: config.history_max_nodes as usize,
            age: config.history_max_age_secs,
            accounting_age: config.accounting_max_age_secs,
            disk_limit: config.history_disk_bytes,
        }
    }
    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }
    #[cfg(test)]
    pub(crate) fn fixture(directory: &Path) -> Self {
        let mut config = <ProxyCli as clap::Parser>::parse_from(["carry"]);
        config.state_dir = directory.to_owned();
        Self::new(&config)
    }
}

pub(super) struct Store {
    pub(super) connection: Connection,
    policy: Policy,
    physical_record_bound: u64,
    publish_record_bound: bool,
    at: u64,
    pins: HashSet<String>,
    retired: std::cell::RefCell<Vec<String>>,
    projected_bytes: std::cell::Cell<u64>,
    reserved_bytes: std::cell::Cell<u64>,
    pending: std::cell::RefCell<BTreeMap<(String, String), Pending>>,
    pending_bytes: std::cell::Cell<usize>,
    path_roots: std::cell::RefCell<HashSet<String>>,
    replaced_tips: std::cell::RefCell<HashSet<String>>,
    settlement: std::cell::Cell<bool>,
}
struct Pending {
    bytes: Vec<u8>,
    at: u64,
    owner: String,
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
fn maintenance_reserve(physical_record_bound: u64, disk_limit: u64) -> u64 {
    // Keep the transaction's startup charge separate from the complete charge
    // of one deletion. Otherwise the largest admitted rows never fit, even in
    // an empty batch, and the oldest such row permanently blocks collection.
    write_charge(physical_record_bound)
        .saturating_add(65536 + write_charge(0))
        .min(disk_limit / 2)
}
fn protected_bytes(max_record: u64, physical_bound: u64, owners: usize, disk_limit: u64) -> u64 {
    settlement_reserve(max_record)
        .saturating_mul(owners as u64)
        .saturating_add(maintenance_reserve(physical_bound, disk_limit))
}
fn admit_extent(
    directory: &Path,
    disk_limit: u64,
    reserved: u64,
    additional: u64,
) -> std::io::Result<()> {
    if disk_bytes(directory, 0)?
        .saturating_add(additional)
        .saturating_add(reserved)
        > disk_limit
    {
        return Err(full());
    }
    #[cfg(unix)]
    if free_bytes(directory)?
        < additional
            .saturating_add(reserved)
            .saturating_add(16 * 1024 * 1024)
    {
        return Err(full());
    }
    Ok(())
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
    Ok(filesystem_available_bytes(stat.f_bavail, stat.f_frsize))
}
#[cfg(unix)]
fn filesystem_available_bytes(blocks: impl Into<u64>, block_size: impl Into<u64>) -> u64 {
    // statvfs uses different integer widths on Linux and Darwin. Normalize both
    // operands before multiplication so neither target narrows capacity.
    blocks.into().saturating_mul(block_size.into())
}
#[cfg(all(test, unix))]
#[test]
fn filesystem_capacity_preserves_platform_widths_and_saturates() {
    assert_eq!(filesystem_available_bytes(3u32, 1u64 << 33), 3u64 << 33);
    assert_eq!(filesystem_available_bytes(3u64, 4096u64), 12288);
    assert_eq!(filesystem_available_bytes(u32::MAX, u64::MAX), u64::MAX);
    assert_eq!(filesystem_available_bytes(0u32, u64::MAX), 0);
}
pub(super) fn admit_owner(service: &Service, owners: usize) -> std::io::Result<()> {
    // Measure the connected store, not its smaller last-connection-closed
    // footprint. SQLite recreates WAL sidecars even for a read-only view;
    // admitting against the closed footprint can spend maintenance's promise
    // before the next publication opens its connection. Keep this view alive
    // through the physical measurement, without modifying retained authority.
    let view = Store::open(service, HashSet::new(), false)?;
    let reserve = protected_bytes(
        service.config.history_max_bytes as u64,
        view.physical_record_bound,
        owners,
        service.config.history_disk_bytes,
    );
    admit_extent(
        &service.config.state_dir,
        service.config.history_disk_bytes,
        reserve,
        0,
    )
}
pub(super) fn maintain(service: &Service, pins: HashSet<String>) -> std::io::Result<()> {
    if !service.config.state_dir.join("history.index").exists() {
        return Ok(());
    }
    Store::open(service, pins, true)?.maintain().map(|_| ())
}
pub(super) fn initialize(service: &Service, pins: HashSet<String>) -> std::io::Result<()> {
    let path = service.config.state_dir.join("history.index");
    if !path.exists() {
        return Ok(());
    }
    Store::open(service, pins, false).map(|_| ())
}

impl Store {
    pub(super) fn admit_checkpoint(
        &self,
        directory: &Path,
        bytes: u64,
        reusable: u64,
    ) -> std::io::Result<()> {
        // Explicit files share the SQL record bound used to promise paid
        // settlement capacity, including the full coexisting staged file.
        if bytes > self.policy.max_record as u64 {
            return Err(full());
        }
        self.admit_file(directory, bytes.saturating_sub(reusable))
    }
    pub(super) fn admit_file(&self, directory: &Path, additional: u64) -> std::io::Result<()> {
        if directory != self.policy.directory.as_path() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "physical write owner directory mismatch",
            ));
        }
        // External files consume exact new bytes, not SQL page/WAL charges.
        // Keep the connected footprint and the very same paid/maintenance
        // promises used by transaction admission until the writer is finished.
        admit_extent(
            &self.policy.directory,
            self.policy.disk_limit,
            self.reserved_bytes.get(),
            additional,
        )
    }
    fn charge(&self, bytes: u64) -> std::io::Result<()> {
        // Reserve index/overflow pages and WAL copies before SQLite can spill.
        // Refusal rolls back the transaction; it never deletes a younger row.
        let charge = write_charge(bytes);
        let next = self.projected_bytes.get().saturating_add(charge);
        let reserved = self.reserved_bytes.get();
        if next.saturating_add(reserved) > self.policy.disk_limit {
            #[cfg(test)]
            eprintln!(
                "retention-capacity: bytes={bytes} projected={} next={next} reserved={reserved} limit={} physical={:?}",
                self.projected_bytes.get(),
                self.policy.disk_limit,
                disk_bytes(&self.policy.directory, 0)
            );
            return Err(full());
        }
        #[cfg(unix)]
        if free_bytes(&self.policy.directory)?
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
        Self::open_policy(&Policy::new(&service.config), pins, write)
    }
    pub(super) fn open_policy(
        policy: &Policy,
        pins: HashSet<String>,
        write: bool,
    ) -> std::io::Result<Self> {
        let path = &policy.directory.join("history.index");
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
            // The first connection also writes authority: reserve its complete
            // bounded main/WAL/SHM extent before even creating a target.
            admit_extent(
                &policy.directory,
                policy.disk_limit,
                protected_bytes(
                    policy.max_record as u64,
                    policy.max_record as u64,
                    pins.len(),
                    policy.disk_limit,
                ),
                write_charge(16 * 4096),
            )?;
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
            // Bound schema creation itself, not just its preflight estimate.
            // One transaction limits WAL frames to the new sixteen-page store;
            // normal publication receives the configured page limit below.
            connection.execute_batch("PRAGMA page_size=4096; PRAGMA max_page_count=16; PRAGMA auto_vacuum=INCREMENTAL; PRAGMA journal_mode=WAL; BEGIN IMMEDIATE; CREATE TABLE records(kind TEXT NOT NULL, key TEXT NOT NULL, data BLOB NOT NULL, at INTEGER NOT NULL, owner TEXT NOT NULL, PRIMARY KEY(kind,key)) WITHOUT ROWID;
                CREATE INDEX records_age ON records(kind,at);
                CREATE INDEX records_owner ON records(kind,owner);
                PRAGMA user_version=1; COMMIT;").map_err(error)?;
        }
        if write {
            let pages = (policy.disk_limit / 4096).max(16);
            connection
                .pragma_update(None, "max_page_count", pages)
                .map_err(error)?;
        }
        // Physical maintenance promises survive a lower decode bound. Persist
        // this scalar with successful publications; existing stores bootstrap
        // from lengths once, without decoding their payloads. Normal opens use
        // the primary-key lookup rather than rescanning the retained corpus.
        let stored_bound: Option<Option<Vec<u8>>> = connection.query_row(
            "SELECT CASE WHEN length(data)<=20 THEN data ELSE NULL END FROM records WHERE kind='metadata' AND key='physical_record_bound'",
            [], |row| row.get(0),
        ).optional().map_err(error)?;
        let recorded = match stored_bound {
            Some(bytes) => Some(serde_json::from_slice::<u64>(&bytes.ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invalid physical record bound",
                )
            })?)?),
            None => None,
        };
        let retained_bound = match recorded {
            Some(bound) => bound,
            None => connection
                .query_row(
                    "SELECT coalesce(max(length(data)),0) FROM records",
                    [],
                    |row| row.get::<_, u64>(0),
                )
                .map_err(error)?,
        };
        let physical_record_bound = retained_bound.max(policy.max_record as u64);
        Ok(Self {
            connection,
            policy: policy.clone(),
            physical_record_bound,
            publish_record_bound: recorded != Some(physical_record_bound),
            at: now(),
            reserved_bytes: std::cell::Cell::new(protected_bytes(
                policy.max_record as u64,
                physical_record_bound,
                pins.len(),
                policy.disk_limit,
            )),
            pins,
            retired: Default::default(),
            projected_bytes: Default::default(),
            pending: Default::default(),
            pending_bytes: Default::default(),
            path_roots: Default::default(),
            replaced_tips: Default::default(),
            settlement: Default::default(),
        })
    }
    pub(super) fn get<T: DeserializeOwned>(
        &self,
        kind: &str,
        key: &str,
    ) -> std::io::Result<Option<T>> {
        if let Some(record) = self.pending.borrow().get(&(kind.into(), key.into())) {
            return serde_json::from_slice(&record.bytes)
                .map(Some)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e));
        }
        let bytes: Option<(usize, Option<Vec<u8>>)> = self
            .connection
            .query_row(
                "SELECT length(data), CASE WHEN length(data)<=?3 THEN data ELSE NULL END FROM records WHERE kind=?1 AND key=?2",
                params![kind, key, self.policy.max_record as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(error)?;
        let Some((length, data)) = bytes else {
            return Ok(None);
        };
        if length > self.policy.max_record {
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
            self.pins.contains(&owner) || at > self.at || self.at - at < self.policy.age
        }))
    }
    pub(super) fn contains(&self, kind: &str, key: &str) -> std::io::Result<bool> {
        if self
            .pending
            .borrow()
            .contains_key(&(kind.into(), key.into()))
        {
            return Ok(true);
        }
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
        if bytes.len() > self.policy.max_record {
            return Err(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                "history record exceeds working-set bound",
            ));
        }
        // The publication working set is bounded independently of the retained
        // corpus: two paths, checkpoint, accounting and their small metadata.
        // Stage bytes once so admission and SQLite consume the same payloads.
        let key = (kind.to_owned(), key.to_owned());
        let mut pending = self.pending.borrow_mut();
        let old = pending.get(&key).map_or(0, |record| {
            record.bytes.len() + record.owner.len() + key.0.len() + key.1.len()
        });
        let next = self
            .pending_bytes
            .get()
            .saturating_sub(old)
            .saturating_add(bytes.len() + owner.len() + key.0.len() + key.1.len());
        if next > self.policy.max_record.saturating_mul(8) {
            return Err(full());
        }
        self.pending_bytes.set(next);
        pending.insert(
            key,
            Pending {
                bytes,
                at,
                owner: owner.into(),
            },
        );
        Ok(())
    }
    pub(super) fn replace_tip(&self, owner: &str) {
        self.replaced_tips.borrow_mut().insert(owner.into());
    }
    pub(super) fn insert(&self, namespace: &str, input: &[Value]) -> std::io::Result<Vec<String>> {
        if input.len() > self.policy.max_nodes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                "history path exceeds working-set bound",
            ));
        }
        let keys = super::path(namespace, input);
        // Include existing intermediate nodes too: a newly staged frontier is
        // not in SQL yet, so maintenance cannot follow its parent from SQL.
        self.path_roots.borrow_mut().extend(keys.iter().cloned());
        let mut parent = namespace.to_owned();
        for (key, value) in keys.iter().zip(input) {
            // Nodes are content-addressed and immutable. Republishing a path
            // must neither reserve another write nor refresh its original age;
            // live references, not node rewrites, keep ancestry reachable.
            if !self.contains("nodes", key)? {
                self.put(
                    "nodes",
                    key,
                    &json!({"parent":parent,"value":value}),
                    self.at,
                    "",
                )?;
            }
            parent = key.clone();
        }
        Ok(keys)
    }
    pub(super) fn ancestry(&self, namespace: &str, frontier: &str) -> std::io::Result<Vec<Value>> {
        let mut key = frontier.to_owned();
        let mut values = Vec::new();
        let mut bytes = 0usize;
        while key != namespace {
            if values.len() >= self.policy.max_nodes {
                return Err(std::io::Error::other(
                    "history ancestry exceeds working-set bound",
                ));
            }
            let node: Value = self
                .get("nodes", &key)?
                .ok_or_else(|| std::io::Error::other("history ancestry unavailable"))?;
            bytes = bytes.saturating_add(serde_json::to_vec(&node)?.len());
            if bytes > self.policy.max_record {
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
        let invalid = || {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid accounting sequence authority",
            )
        };
        if let Some(marker) = self.get::<Value>("accounted", &identity)? {
            let sequence = marker["sequence"].as_u64().ok_or_else(invalid)?;
            // Set membership is not observation identity: A -> B -> A must
            // retain the final transition, including a cumulative decrease or
            // recovered coverage. Only suppress this owner's immediate repeat.
            let latest: Option<String> = self.connection.query_row(
                "SELECT key FROM records INDEXED BY records_owner WHERE kind='accounting' AND owner=?1 ORDER BY key DESC LIMIT 1",
                [id], |row| row.get(0),
            ).optional().map_err(error)?;
            if latest == Some(format!("{sequence:064x}")) {
                return Ok(());
            }
        }
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
    pub(super) fn expired_accounting_has_owner(&self, id: &str) -> std::io::Result<bool> {
        if self.live("owners", id)? {
            return Ok(false);
        }
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM records INDEXED BY records_owner WHERE kind='accounting' AND owner=?1 AND at>=?2)",
            params![id, self.at.saturating_sub(self.policy.accounting_age)],
            |row| row.get(0),
        ).map_err(error)
    }
    pub(super) fn expired_accounting_totals(
        &self,
    ) -> std::io::Result<(crate::proxy_savings::DashboardTotals, DashboardUsage)> {
        // Stream one frozen cumulative observation per expired raw owner.
        // Live owners are projected through their current authority instead;
        // prior comparison coverage cannot override a current unknown window.
        // The SQL owner index selects on disk, not in a process-sized map.
        // Sequence keys break ties when several paid states share a second.
        // Bound each anti-join to its owner using the existing index: SQLite
        // otherwise chooses a kind-only primary-key scan of all observations.
        let mut statement = self
            .connection
            .prepare(
                "SELECT length(r.data),CASE WHEN length(r.data)<=?1 THEN r.data ELSE NULL END,r.owner
             FROM records r WHERE r.kind='accounting' AND r.at>=?2
             AND NOT EXISTS(SELECT 1 FROM records n INDEXED BY records_owner WHERE n.kind='accounting' AND n.owner=r.owner
                AND (n.at>r.at OR (n.at=r.at AND n.key>r.key)))",
            )
            .map_err(error)?;
        let mut rows = statement
            .query(params![
                self.policy.max_record as u64,
                self.at.saturating_sub(self.policy.accounting_age)
            ])
            .map_err(error)?;
        let mut totals = crate::proxy_savings::DashboardTotals::default();
        let mut usage = DashboardUsage::default();
        while let Some(row) = rows.next().map_err(error)? {
            let owner: String = row.get(2).map_err(error)?;
            if self.live("owners", &owner)? {
                continue;
            }
            if row.get::<_, usize>(0).map_err(error)? > self.policy.max_record {
                return Err(full());
            }
            let data: Vec<u8> = row.get(1).map_err(error)?;
            #[derive(serde::Deserialize)]
            struct Observation {
                primary: carry::core::UsageLedger,
                reviewer: carry::core::UsageLedger,
                estimate: Option<crate::proxy_savings::DashboardEstimate>,
            }
            let observation: Observation = serde_json::from_slice(&data)?;
            usage.observe(&observation.primary, &observation.reviewer)?;
            if let Some(estimate) = observation.estimate {
                totals.observe(&estimate);
            } else {
                totals.excluded_sessions += 1;
                totals.unavailable_whole_windows(1);
            }
        }
        Ok((totals, usage))
    }
    pub(super) fn accounting_window(&self, start: u64, end: u64) -> std::io::Result<Value> {
        let cutoff = self.at.saturating_sub(self.policy.accounting_age);
        // records_owner provides bounded disk-backed owner ordering. Sequence
        // keys order immutable observations, including same-second revisions.
        let mut statement = self.connection.prepare(
            "SELECT owner,key,at,length(data),CASE WHEN length(data)<=?1 THEN data ELSE NULL END
             FROM records INDEXED BY records_owner WHERE kind='accounting' AND at>=?2 ORDER BY owner,key"
        ).map_err(error)?;
        let mut rows = statement
            .query(params![self.policy.max_record as u64, cutoff])
            .map_err(error)?;
        let mut window = accounting::Window::new(start, end, cutoff);
        while let Some(row) = rows.next().map_err(error)? {
            if row.get::<_, usize>(3).map_err(error)? > self.policy.max_record {
                return Err(full());
            }
            let owner: String = row.get(0).map_err(error)?;
            let key: String = row.get(1).map_err(error)?;
            let at = row.get(2).map_err(error)?;
            let data: Vec<u8> = row.get(4).map_err(error)?;
            window.observe(&owner, &key, at, &data)?;
        }
        window.finish()
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
                self.policy.max_record as u64,
                self.at.saturating_sub(self.policy.accounting_age),
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
            if length > self.policy.max_record {
                return Err(std::io::Error::other(
                    "accounting record exceeds working-set bound",
                ));
            }
            if records.len() == limit
                || (!records.is_empty() && bytes.saturating_add(length) > self.policy.max_record)
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
            json!({"records":records,"has_more":more,"next":if more{next}else{None},"basis":"immutable cumulative observations; select one observation per owner and time, never sum snapshots", "accounting_retention_secs":self.policy.accounting_age,"raw_history_retention_secs":self.policy.age}),
        )
    }
    fn expire(&self, kind: &str, cutoff: u64, budget: &mut u64) -> std::io::Result<Vec<String>> {
        // Protect ancestry by reachability, not by rewriting its original age.
        // Live references can point to nodes older than their own publication
        // (notably a paid-only settlement of an old pinned checkpoint).
        let mut statement=self.connection.prepare("WITH RECURSIVE ancestry(key) AS (
            SELECT json_extract(CAST(data AS TEXT),'$.history_frontier') FROM records WHERE kind='snapshots' AND (at>?2 OR owner IN (SELECT id FROM pins))
            UNION SELECT key FROM records WHERE kind IN ('tips','reviews','dispatches') AND (at>?2 OR owner IN (SELECT id FROM pins))
            UNION SELECT json_extract(CAST(data AS TEXT),'$.frontier') FROM records WHERE kind='transcripts' AND (at>?2 OR owner IN (SELECT id FROM pins))
            UNION SELECT key FROM publication_roots
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
    fn collect(&self, budget: &mut u64) -> std::io::Result<bool> {
        self.connection
            .execute_batch("CREATE TEMP TABLE IF NOT EXISTS pins(id TEXT PRIMARY KEY) WITHOUT ROWID; DELETE FROM pins;
                CREATE TEMP TABLE IF NOT EXISTS publication_roots(key TEXT PRIMARY KEY) WITHOUT ROWID; DELETE FROM publication_roots;")
            .map_err(error)?;
        for id in &self.pins {
            self.connection
                .execute("INSERT INTO pins VALUES(?1)", [id])
                .map_err(error)?;
        }
        for key in self.path_roots.borrow().iter() {
            self.connection
                .execute("INSERT INTO publication_roots VALUES(?1)", [key])
                .map_err(error)?;
        }
        let mut changed = false;
        if let Some(cutoff) = self.at.checked_sub(self.policy.age) {
            *self.retired.borrow_mut() = self.expire("snapshots", cutoff, budget)?;
            changed |= !self.retired.borrow().is_empty();
            for kind in ["tips", "transcripts", "reviews", "dispatches", "nodes"] {
                changed |= !self.expire(kind, cutoff, budget)?.is_empty();
            }
        }
        if let Some(cutoff) = self.at.checked_sub(self.policy.accounting_age) {
            changed |= !self.expire("accounting", cutoff, budget)?.is_empty();
            changed |= !self.expire("accounted", cutoff, budget)?.is_empty();
        }
        Ok(changed)
    }
    fn maintain(&self) -> std::io::Result<bool> {
        let allowance = maintenance_reserve(self.physical_record_bound, self.policy.disk_limit);
        let reserved = self.reserved_bytes.get();
        // Spend maintenance's promise only; every paid owner keeps its full
        // settlement reserve. Restore the normal reservation even on failure.
        self.reserved_bytes.set(reserved.saturating_sub(allowance));
        let result = (|| {
            self.start()?;
            let mut budget = allowance.saturating_sub(65536 + write_charge(0));
            let changed = self.collect(&mut budget)?;
            self.finish()?;
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
                .map_err(error)?;
            Ok(changed)
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
                    .saturating_sub(settlement_reserve(self.policy.max_record as u64)),
            );
        }
        self.settlement.set(true);
        Ok(())
    }
    fn start(&self) -> std::io::Result<()> {
        self.projected_bytes
            .set(disk_bytes(&self.policy.directory, 0)?.saturating_add(65536));
        self.charge(0)?;
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(error)
    }
    pub(super) fn begin(&self) -> std::io::Result<()> {
        self.maintain().map(|_| ())
    }
    pub(super) fn commit(&self) -> std::io::Result<()> {
        if self.publish_record_bound {
            self.put(
                "metadata",
                "physical_record_bound",
                &self.physical_record_bound,
                0,
                "",
            )?;
        }
        let demand = self
            .pending
            .borrow()
            .values()
            .fold(65536u64.saturating_add(write_charge(0)), |sum, record| {
                sum.saturating_add(write_charge(record.bytes.len() as u64))
            })
            .saturating_add(
                write_charge(0).saturating_mul(self.replaced_tips.borrow().len() as u64),
            );
        // Admission is about this complete publication, not just a pin or an
        // empty transaction. Maintenance never spends another owner's promise.
        // Bound each call to eight batches (64 vacuum pages, eight row batches
        // per kind); continued pressure returns backpressure, not unbounded GC.
        for pass in 0..=8 {
            let physical = disk_bytes(&self.policy.directory, 0)?;
            if physical
                .saturating_add(demand)
                .saturating_add(self.reserved_bytes.get())
                <= self.policy.disk_limit
            {
                break;
            }
            if self.settlement.get() || pass == 8 {
                return Err(full());
            }
            let changed = self.maintain()?;
            if !changed && disk_bytes(&self.policy.directory, 0)? >= physical {
                return Err(full());
            }
        }
        self.start()?;
        for owner in self.replaced_tips.borrow().iter() {
            self.charge(0)?;
            self.connection
                .execute(
                    "DELETE FROM records WHERE kind='tips' AND owner=?1",
                    [owner],
                )
                .map_err(error)?;
        }
        for ((kind, key), record) in self.pending.borrow().iter() {
            self.charge(record.bytes.len() as u64)?;
            self.connection.execute("INSERT INTO records VALUES(?1,?2,?3,?4,?5) ON CONFLICT(kind,key) DO UPDATE SET data=excluded.data,at=excluded.at,owner=excluded.owner",
                params![kind,key,record.bytes,record.at,record.owner]).map_err(error)?;
        }
        self.finish()
    }
    fn finish(&self) -> std::io::Result<()> {
        self.connection.execute_batch("COMMIT").map_err(error)?;
        for id in self.retired.take() {
            if id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
                let _ = std::fs::remove_file(self.policy.directory.join(format!("{id}.json")));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod initialization_admission_tests {
    use super::*;

    #[test]
    fn fresh_schema_refuses_before_crossing_physical_capacity() {
        let directory = tempfile::tempdir().unwrap();
        let mut policy = Policy::fixture(directory.path());
        policy.max_record = 8192;
        policy.disk_limit = 2 * 1024 * 1024;
        let path = directory.path().join("history.index");
        let occupied = disk_bytes(directory.path(), 0).unwrap();
        File::create(directory.path().join("unrelated-retained-evidence"))
            .unwrap()
            .set_len(policy.disk_limit - occupied - 4096)
            .unwrap();
        let before = disk_bytes(directory.path(), 0).unwrap();
        assert_eq!(before, policy.disk_limit - 4096);
        // Observe the connected WAL/SHM footprint, not only the closed store.
        let opened = Store::open_policy(&policy, HashSet::new(), true);
        let connected_bytes = disk_bytes(directory.path(), 0).unwrap();
        assert!(
            opened.is_err() && connected_bytes <= policy.disk_limit,
            "schema creation must refuse before physical capacity: before={before} connected={connected_bytes} cap={} opened={}",
            policy.disk_limit,
            opened.is_ok()
        );
        assert!(!path.exists(), "refused schema must not publish authority");
    }

    #[test]
    fn admitted_schema_reopens_without_rewriting_authority() {
        let directory = tempfile::tempdir().unwrap();
        let mut policy = Policy::fixture(directory.path());
        policy.max_record = 8192;
        policy.disk_limit = 2 * 1024 * 1024;
        let path = directory.path().join("history.index");
        let store = Store::open_policy(&policy, HashSet::new(), true).unwrap();
        let pages: u64 = store
            .connection
            .pragma_query_value(None, "page_count", |row| row.get(0))
            .unwrap();
        assert!(pages <= 16);
        let configured_pages: u64 = store
            .connection
            .pragma_query_value(None, "max_page_count", |row| row.get(0))
            .unwrap();
        assert_eq!(configured_pages, policy.disk_limit / 4096);
        assert!(disk_bytes(directory.path(), 0).unwrap() <= write_charge(16 * 4096));
        assert!(store.get::<Value>("snapshots", "absent").unwrap().is_none());
        drop(store);
        let frozen = std::fs::read(&path).unwrap();
        let reopened = Store::open_policy(&policy, HashSet::new(), false).unwrap();
        assert!(
            reopened
                .get::<Value>("snapshots", "absent")
                .unwrap()
                .is_none()
        );
        drop(reopened);
        assert_eq!(std::fs::read(&path).unwrap(), frozen);
    }
}
