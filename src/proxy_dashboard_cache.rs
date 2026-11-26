//! A single derived dashboard page. Disk authority, retention and live pins
//! determine validity; this cache never participates in forwarding or billing.
use super::*;

pub(super) struct Validity {
    pub(super) built_at: u64,
    pub(super) expires_at: Option<u64>,
}

pub(super) struct Snapshot {
    pub(super) revision: [u8; 32],
    pub(super) after: Option<SessionOrder>,
    pub(super) limit: usize,
    pub(super) validity: Validity,
    pub(super) data: Value,
}

impl Snapshot {
    pub(super) fn matches(
        &self,
        revision: Option<[u8; 32]>,
        after: &Option<SessionOrder>,
        limit: usize,
        at: u64,
    ) -> bool {
        revision == Some(self.revision)
            && after == &self.after
            && limit == self.limit
            && at >= self.validity.built_at
            && self.validity.expires_at.is_none_or(|expiry| at < expiry)
    }
}

pub(super) fn revision(service: &Service) -> std::io::Result<[u8; 32]> {
    // Hash metadata rather than reading checkpoint, archive or SQLite payloads.
    // Include every direct file: numeric archives and success seeds can change
    // independently of checkpoints. Diagnostics live in a separate directory.
    let mut revision: [u8; 32] = Sha256::digest(format!("{:?}", service.config)).into();
    for entry in std::fs::read_dir(&service.config.state_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        // SQLite readers can update shared-memory bookkeeping without changing
        // data. The main database and WAL are both included below.
        if name == "history.index-shm" {
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path())?;
        // Opening a read-only WAL database can create an empty sidecar. It has
        // no committed frames and is equivalent to an absent WAL.
        if name == "history.index-wal" && metadata.is_file() && metadata.len() == 0 {
            continue;
        }
        if metadata.is_dir() {
            continue;
        }
        if !metadata.is_file() {
            return Err(std::io::Error::other(
                "dashboard source is not a regular file",
            ));
        }
        let mut digest = Sha256::new();
        digest.update(name.as_encoded_bytes());
        digest.update(metadata.len().to_le_bytes());
        digest.update(format!(
            "{:?}{:?}",
            metadata.modified()?,
            metadata.permissions()
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            digest.update(metadata.ino().to_le_bytes());
            digest.update(metadata.ctime().to_le_bytes());
            digest.update(metadata.ctime_nsec().to_le_bytes());
        }
        // Directory iteration order is unspecified. Unique filenames make this
        // an order-independent revision with constant memory as sessions grow.
        for (byte, part) in revision.iter_mut().zip(digest.finalize()) {
            *byte ^= part;
        }
    }
    let pins = Sha256::digest(serde_json::to_vec(&history::dashboard_pins(service))?);
    for (byte, part) in revision.iter_mut().zip(pins) {
        *byte ^= part;
    }
    Ok(revision)
}
