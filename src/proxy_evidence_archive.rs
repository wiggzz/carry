//! RequestEvidence's numeric source journal. No bodies, item vectors, or
//! predicted costs. Disk grows with tracked events; state-directory lifecycle
//! owns deletion (no semantic-rollover GC). Replay streams one bounded record.
use super::*;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

fn journal_file(path: &Path, write: bool, create: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(write)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other(
            "numeric archive is not a regular file",
        ));
    }
    Ok(file)
}

const MAX_RECORD: u64 = 512 * 1024;
const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Watermark {
    owner: String,
    namespace: String,
    bytes: u64,
    events: u64,
    sha256: String,
    #[serde(default)]
    last_record_bytes: u64,
    prior_untracked: u64,
    #[serde(
        default,
        deserialize_with = "generation",
        skip_serializing_if = "Option::is_none"
    )]
    generation: Option<String>,
}
// Only a missing field denotes the original journal. Malformed optional
// metadata censors comparison rather than preventing the core Session load.
fn generation<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
    struct Generation;
    impl<'de> Visitor<'de> for Generation {
        type Value = Option<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("optional journal generation")
        }
        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
            Ok(Some(value.into()))
        }
        fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
            Ok(Some(value))
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(Some(String::new()))
        }
        fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok(Some(String::new()))
        }
        fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
            Ok(Some(String::new()))
        }
        fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
            Ok(Some(String::new()))
        }
        fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok(Some(String::new()))
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut values: A) -> Result<Self::Value, A::Error> {
            while values.next_element::<IgnoredAny>()?.is_some() {}
            Ok(Some(String::new()))
        }
        fn visit_map<A: MapAccess<'de>>(self, mut values: A) -> Result<Self::Value, A::Error> {
            while values.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(Some(String::new()))
        }
    }
    deserializer.deserialize_any(Generation)
}

#[derive(Clone, Debug)]
pub(super) struct Access {
    directory: PathBuf,
    owner: String,
    namespace: String,
    prior_untracked: u64,
    started: bool,
    pending_bytes: usize,
}
#[derive(Deserialize, Serialize)]
struct Record {
    version: u32,
    owner: String,
    namespace: String,
    sequence: u64,
    previous: String,
    event: Observation,
}

fn unavailable() -> std::io::Error {
    std::io::Error::other("numeric evidence archive unavailable")
}

impl Access {
    fn path(&self, mark: &Watermark) -> std::io::Result<PathBuf> {
        if mark.owner != self.owner || mark.namespace != self.namespace {
            return Err(unavailable());
        }
        let suffix = match &mark.generation {
            None => String::new(),
            Some(g)
                if g.len() == 64
                    && g.bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) =>
            {
                format!("-g{g}")
            }
            Some(_) => return Err(unavailable()),
        };
        Ok(self.directory.join(format!(
            ".request-evidence-{}-{}{suffix}.jsonl",
            self.owner,
            hash(&self.namespace)
        )))
    }

    // The same bounded integrity traversal serves replay and copy-on-conflict.
    // Copy original validated lines, never a reserialized/uncommitted tail.
    fn scan(
        &self,
        mark: &Watermark,
        mut observe: impl FnMut(&[u8], &Observation) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let path = self.path(mark)?;
        if mark.bytes == 0 {
            return if mark.events == 0 && mark.sha256.is_empty() {
                Ok(())
            } else {
                Err(unavailable())
            };
        }
        let file = journal_file(&path, false, false)?;
        if file.metadata()?.len() < mark.bytes {
            return Err(unavailable());
        }
        let mut reader = BufReader::new(file.take(mark.bytes));
        let (mut bytes, mut count, mut previous, mut last) = (0u64, 0u64, String::new(), 0u64);
        loop {
            let mut line = Vec::new();
            let read = (&mut reader)
                .take(MAX_RECORD + 1)
                .read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }
            if read as u64 > MAX_RECORD || line.last() != Some(&b'\n') {
                return Err(unavailable());
            }
            let record: Record = serde_json::from_slice(&line).map_err(|_| unavailable())?;
            if record.version != 1
                || record.owner != mark.owner
                || record.namespace != mark.namespace
                || record.sequence != count
                || record.previous != previous
            {
                return Err(unavailable());
            }
            if let Observation::Primary { facts, .. } = &record.event
                && (facts.input.is_some()
                    || facts.semantic_input.is_some()
                    || facts.semantic_output.is_some())
            {
                return Err(unavailable());
            }
            previous = format!("{:x}", Sha256::digest(&line));
            bytes += read as u64;
            count += 1;
            last = read as u64;
            observe(&line, &record.event)?;
            #[cfg(test)]
            if self.directory.join(".numeric-read-fault").exists() {
                std::fs::remove_file(self.directory.join(".numeric-read-fault"))?;
                return Err(unavailable());
            }
        }
        if bytes != mark.bytes
            || count != mark.events
            || previous != mark.sha256
            || (mark.last_record_bytes != 0 && mark.last_record_bytes != last)
        {
            return Err(unavailable());
        }
        Ok(())
    }
}

fn encode(mark: &mut Watermark, event: &Observation) -> std::io::Result<Vec<u8>> {
    let record = Record {
        version: 1,
        owner: mark.owner.clone(),
        namespace: mark.namespace.clone(),
        sequence: mark.events,
        previous: mark.sha256.clone(),
        event: event.clone(),
    };
    let mut line = serde_json::to_vec(&record)?;
    line.push(b'\n');
    if line.len() as u64 > MAX_RECORD {
        return Err(std::io::Error::other("numeric record exceeds bound"));
    }
    mark.bytes += line.len() as u64;
    mark.last_record_bytes = line.len() as u64;
    mark.events += 1;
    mark.sha256 = format!("{:x}", Sha256::digest(&line));
    Ok(line)
}

fn write_numeric(access: &Access, file: &mut File, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(test)]
    if !bytes.is_empty() && access.directory.join(".numeric-write-fault").exists() {
        std::fs::remove_file(access.directory.join(".numeric-write-fault"))?;
        file.write_all(&bytes[..bytes.len() / 2])?;
        file.sync_all()?;
        return Err(std::io::Error::other("injected partial numeric write"));
    }
    #[cfg(not(test))]
    let _ = access;
    file.write_all(bytes)
}

impl RequestEvidence {
    /// Bind only after the canonical Session/index owner has loaded. Archive
    /// failures never reject core state or ordinary forwarding.
    pub(crate) fn bind_archive(&mut self, directory: &Path, owner: &str, namespace: Option<&str>) {
        if self.archive_access.is_some() {
            return;
        }
        // This is the accounting window's birth namespace, not a mutable H
        // recognition/configuration namespace. Explicit owners keep the same
        // paid window through legitimate settings changes; record contracts
        // still fence cache/semantic assumptions at each actual request.
        let namespace = self.archive.as_ref().map_or_else(
            || namespace.unwrap_or("").to_owned(),
            |mark| mark.namespace.clone(),
        );
        self.archive_access = Some(Access {
            directory: directory.to_owned(),
            owner: owner.into(),
            namespace,
            prior_untracked: self.prior_untracked,
            started: false,
            pending_bytes: self.pending_bytes(),
        });
    }

    pub(crate) fn archive_tracking(&self) -> bool {
        // Reading or semantically saving an untouched legacy owner must not
        // rewrite its frozen accounting representation. Start migration only
        // when a real new numeric event has been admitted.
        self.archive.is_some()
            || self
                .archive_access
                .as_ref()
                .is_some_and(|access| access.started)
    }

    pub(super) fn start_numeric(&mut self) {
        let Some(access) = &mut self.archive_access else {
            return;
        };
        if access.started {
            return;
        }
        access.started = true;
        if self.archive.is_none() && self.numeric_pending.is_empty() {
            // Upgrade only retained source evidence, never money aggregates or
            // already-discarded inputs. Expand one bounded semantic suffix.
            let (mut wire, mut semantic) = (Vec::new(), Vec::new());
            let mut prior_items = 0;
            for index in 0..self.observations.len() {
                let event = match &self.observations[index] {
                    Observation::Primary {
                        facts,
                        restoration,
                        edited,
                    } => {
                        let mut full = facts.as_ref().clone();
                        if Self::expand(facts, &mut wire, &mut semantic) {
                            full.input = Some(wire.clone());
                            full.semantic_input = Some(semantic.clone());
                        } else {
                            full.input = None;
                            full.semantic_input = None;
                        }
                        let numeric = full.numeric(prior_items);
                        prior_items = full.input_len().unwrap_or(0);
                        Observation::Primary {
                            facts: Box::new(numeric),
                            restoration: *restoration,
                            edited: *edited,
                        }
                    }
                    other => other.clone(),
                };
                self.queue_numeric(event);
            }
        }
    }

    fn pending_bytes(&self) -> usize {
        self.archive_access.as_ref().map_or_else(
            || {
                self.numeric_pending.iter().fold(0usize, |total, event| {
                    total.saturating_add(
                        serde_json::to_vec(event).map_or(usize::MAX, |bytes| bytes.len()),
                    )
                })
            },
            |access| access.pending_bytes,
        )
    }

    pub(super) fn queue_numeric(&mut self, event: Observation) {
        if let Observation::Primary { facts, .. } = &event {
            self.numeric_primary_items = facts.input_len();
        }
        // Normal saves flush each event. A prolonged unavailable disk cannot
        // grow the resident/checkpoint suffix without bound: report the gap.
        if self.numeric_lost {
            return;
        }
        let size = serde_json::to_vec(&event).map_or(usize::MAX, |bytes| bytes.len());
        if size > MAX_RECORD as usize - 4096 {
            self.numeric_lost = true;
            return;
        }
        if self.numeric_pending.len() >= Self::MAX_OBSERVATIONS
            || self.pending_bytes().saturating_add(size) > MAX_PENDING_BYTES
        {
            // Seal a saturated legacy upgrade before admitting the next real
            // event. Its watermark is still committed only with the core save.
            let _ = self.flush_numeric();
        }
        if self.numeric_pending.len() < Self::MAX_OBSERVATIONS
            && self.pending_bytes().saturating_add(size) <= MAX_PENDING_BYTES
        {
            self.numeric_pending.push(event);
            if let Some(access) = &mut self.archive_access {
                access.pending_bytes = access.pending_bytes.saturating_add(size);
            }
        } else {
            self.numeric_lost = true;
        }
    }

    pub(super) fn numeric_prior_untracked(&self) -> u64 {
        self.archive
            .as_ref()
            .map_or(self.prior_untracked, |m| m.prior_untracked)
    }

    pub(super) fn scan_numeric(
        &self,
        mut observe: impl FnMut(&Observation),
    ) -> std::io::Result<()> {
        if self.numeric_lost {
            return Err(unavailable());
        }
        if let Some(mark) = &self.archive {
            self.archive_access
                .as_ref()
                .ok_or_else(unavailable)?
                .scan(mark, |_, event| {
                    observe(event);
                    Ok(())
                })?;
        }
        for event in &self.numeric_pending {
            observe(event);
        }
        Ok(())
    }

    /// Stage journal bytes BEFORE publishing the same owner's atomic core
    /// checkpoint/index watermark. Uncommitted trailing bytes are ignored on
    /// reads; retry reuses identical bytes and preserves conflicting tails.
    /// Repeated saves are deterministic/idempotent.
    /// On I/O failure retain the bounded numeric suffix in the core checkpoint;
    /// missing/corrupt history remains explicitly unknown, never forwarding 500.
    pub(crate) fn prepare_archive(
        &mut self,
        directory: &Path,
        owner: &str,
        namespace: Option<&str>,
    ) {
        self.bind_archive(directory, owner, namespace);
        self.start_numeric();
        let _ = self.flush_numeric();
    }

    fn flush_numeric(&mut self) -> std::io::Result<()> {
        if self.numeric_lost {
            return Err(unavailable());
        }
        let access = self.archive_access.as_ref().ok_or_else(unavailable)?;
        let base = self.archive.clone().unwrap_or_else(|| Watermark {
            owner: access.owner.clone(),
            namespace: access.namespace.clone(),
            bytes: 0,
            events: 0,
            sha256: String::new(),
            last_record_bytes: 0,
            prior_untracked: access.prior_untracked,
            generation: None,
        });
        let path = access.path(&base)?;
        if self.numeric_pending.is_empty() {
            self.archive = Some(base);
            return Ok(());
        }
        let mut file = journal_file(&path, true, base.bytes == 0)?;
        // Normal append retains the cheap last-record check. Replay, and any
        // conflict fork, validate the FULL committed chain before using it.
        if base.bytes > 0 {
            if base.last_record_bytes == 0 {
                access.scan(&base, |_, _| Ok(()))?;
            } else {
                if base.last_record_bytes > MAX_RECORD || base.last_record_bytes > base.bytes {
                    return Err(unavailable());
                }
                file.seek(SeekFrom::Start(base.bytes - base.last_record_bytes))?;
                let mut tail = vec![0; base.last_record_bytes as usize];
                file.read_exact(&mut tail)?;
                if format!("{:x}", Sha256::digest(&tail)) != base.sha256 {
                    return Err(unavailable());
                }
            }
        } else if base.events != 0 || !base.sha256.is_empty() {
            return Err(unavailable());
        }
        let existing_bytes = file.metadata()?.len();
        file.seek(SeekFrom::Start(base.bytes))?;
        let mut mark = base.clone();
        for event in &self.numeric_pending {
            let offset = mark.bytes;
            let line = encode(&mut mark, event)?;
            let reused = existing_bytes.saturating_sub(offset).min(line.len() as u64) as usize;
            if reused > 0 {
                let mut existing = vec![0; reused];
                file.read_exact(&mut existing)?;
                if existing != line[..reused] {
                    // Possibly committed elsewhere: NEVER mutate these bytes.
                    return self.fork_numeric(&base);
                }
            }
            write_numeric(access, &mut file, &line[reused..])?;
        }
        file.sync_all()?;
        File::open(&access.directory)?.sync_all()?;
        self.finish_numeric(mark);
        Ok(())
    }

    fn finish_numeric(&mut self, mark: Watermark) {
        self.archive = Some(mark);
        self.numeric_pending.clear();
        self.archive_access.as_mut().unwrap().pending_bytes = 0;
    }

    fn fork_numeric(&mut self, base: &Watermark) -> std::io::Result<()> {
        let access = self.archive_access.as_ref().ok_or_else(unavailable)?;
        let mut desired = base.clone();
        for event in &self.numeric_pending {
            encode(&mut desired, event)?;
        }
        // A branch identity, NOT publication authority. Identical failed saves
        // reuse one installed branch; only checkpoint/index selects its facts.
        desired.generation = Some(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                "carry-numeric-fork-v1",
                base,
                &desired
            ))?)
        ));
        let target = access.path(&desired)?;
        let mut temporary = tempfile::NamedTempFile::new_in(&access.directory)?;
        access.scan(base, |line, _| {
            write_numeric(access, temporary.as_file_mut(), line)
        })?;
        let mut mark = base.clone();
        for event in &self.numeric_pending {
            write_numeric(access, temporary.as_file_mut(), &encode(&mut mark, event)?)?;
        }
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(&target) {
            Ok(_) => (),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Never overwrite/truncate a corrupt, foreign or symlink target.
                access.scan(&desired, |_, _| Ok(()))?;
            }
            Err(error) => return Err(error.error),
        }
        File::open(&access.directory)?.sync_all()?;
        self.finish_numeric(desired);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn primary(evidence: &mut RequestEvidence, n: u64) {
        let body = json!({"model":"gpt-6.1-sol","instructions":"private-instruction-sentinel",
            "max_output_tokens":1,"input":[{"role":"user","content":"private-input-sentinel"}]});
        let response = json!({"status":"completed","output":[],"usage":{"input_tokens":1000+n,"output_tokens":0,"input_tokens_details":{"cached_tokens":0}},"private":"private-response-sentinel"});
        evidence.primary_settled(PrimaryObservation {
            body: &body,
            response: &response,
            route: "route",
            at: 100 + n,
            standard: true,
            resolved: None,
            cost: Some((1000 + n) as f64 * 2e-6),
            measurements: None,
        });
    }

    fn restored(bytes: &[u8], directory: &Path) -> RequestEvidence {
        let mut result: RequestEvidence = serde_json::from_slice(bytes).unwrap();
        result.bind_archive(directory, "fixture-owner", Some("fixture-namespace"));
        result
    }

    #[test]
    fn watermark_retry_ignores_uncommitted_tail_and_repeated_saves_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mut evidence = RequestEvidence::default();
        evidence.bind_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        primary(&mut evidence, 0);
        evidence.prepare_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        let committed = serde_json::to_vec(&evidence).unwrap();
        let path = evidence
            .archive_access
            .as_ref()
            .unwrap()
            .path(evidence.archive.as_ref().unwrap())
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        evidence.prepare_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        // Journal sync succeeded, but the checkpoint/index publish did not.
        // Restore precisely the last committed watermark as a crashed process
        // would; orphan bytes are not another successful accounting event.
        primary(&mut evidence, 1);
        evidence.prepare_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        let tail = std::fs::read(&path).unwrap();
        assert!(tail.len() > bytes.len());
        let mut retry = restored(&committed, dir.path());
        assert_eq!(retry.snapshot().chronological.unwrap().primary_responses, 1);
        // Retry the same uncommitted receipt by reusing its exact bytes.
        primary(&mut retry, 1);
        retry.prepare_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        let mut count = 0;
        retry.scan_numeric(|_| count += 1).unwrap();
        assert_eq!(count, 2);
        assert_eq!(retry.archive.as_ref().unwrap().events, 2);
        assert!(std::fs::read(&path).unwrap().starts_with(&bytes));
        assert_eq!(std::fs::read(&path).unwrap(), tail);
        let restarted = restored(&serde_json::to_vec(&retry).unwrap(), dir.path());
        assert_eq!(
            serde_json::to_value(restarted.snapshot().chronological).unwrap(),
            serde_json::to_value(retry.snapshot().chronological).unwrap()
        );
    }

    #[test]
    fn conflict_fork_faults_reuse_and_generation_integrity() {
        let dir = tempfile::tempdir().unwrap();
        let mut evidence = RequestEvidence::default();
        evidence.bind_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        primary(&mut evidence, 0);
        evidence.flush_numeric().unwrap();
        primary(&mut evidence, 1);
        evidence.flush_numeric().unwrap();
        let committed = serde_json::to_vec(&evidence).unwrap();
        let path = evidence
            .archive_access
            .as_ref()
            .unwrap()
            .path(evidence.archive.as_ref().unwrap())
            .unwrap();
        let prefix = std::fs::read(&path).unwrap();
        let mut orphan = prefix.clone();
        orphan.extend_from_slice(b"conflicting orphan must never become an event\n");
        std::fs::write(&path, &orphan).unwrap();
        primary(&mut evidence, 2);
        let pending = serde_json::to_vec(&evidence).unwrap();
        std::fs::write(dir.path().join(".numeric-write-fault"), b"fault").unwrap();
        assert!(evidence.flush_numeric().is_err()); // partial prefix copy
        assert_eq!(serde_json::to_vec(&evidence).unwrap(), pending);
        assert_eq!(std::fs::read(&path).unwrap(), orphan);
        evidence.flush_numeric().unwrap();
        let desired = evidence.archive.as_ref().unwrap().clone();
        let target = evidence
            .archive_access
            .as_ref()
            .unwrap()
            .path(&desired)
            .unwrap();
        assert_ne!(target, path);
        let target_bytes = std::fs::read(&target).unwrap();
        assert!(target_bytes.starts_with(&prefix));
        assert_eq!(desired.events, 3);
        assert_eq!(std::fs::read(&path).unwrap(), orphan);
        let installed = std::fs::read_dir(dir.path()).unwrap().count();
        let mut retry = restored(&pending, dir.path()); // core publish failed
        retry.flush_numeric().unwrap();
        assert_eq!(
            serde_json::to_vec(&retry).unwrap(),
            serde_json::to_vec(&evidence).unwrap()
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), installed);
        assert_eq!(std::fs::read(&target).unwrap(), target_bytes);
        // Each mark resolves its OWN path, not a cached path from another clone.
        let old = restored(&committed, dir.path());
        assert_eq!(old.snapshot().chronological.unwrap().primary_responses, 2);
        assert_eq!(retry.snapshot().chronological.unwrap().primary_responses, 3);
        for case in ["missing", "corrupt", "foreign", "truncated", "symlink"] {
            std::fs::remove_file(&target).unwrap();
            match case {
                "missing" => (),
                "corrupt" => std::fs::write(&target, b"bad\n").unwrap(),
                "foreign" => {
                    let mut foreign = target_bytes.clone();
                    let at = foreign
                        .windows(b"fixture-owner".len())
                        .position(|w| w == b"fixture-owner")
                        .unwrap();
                    foreign[at] = b'X';
                    std::fs::write(&target, foreign).unwrap();
                }
                "truncated" => {
                    std::fs::write(&target, &target_bytes[..target_bytes.len() - 1]).unwrap()
                }
                _ => {
                    let foreign = dir.path().join("foreign-target");
                    std::fs::write(&foreign, b"foreign-target-must-survive").unwrap();
                    std::os::unix::fs::symlink(&foreign, &target).unwrap();
                }
            }
            let loaded = restored(&serde_json::to_vec(&evidence).unwrap(), dir.path());
            assert!(loaded.snapshot().archive_unavailable, "{case}");
            if case != "missing" {
                // Reusing an existing invalid deterministic fork must fail closed.
                let mut retry = restored(&pending, dir.path());
                assert!(retry.flush_numeric().is_err(), "{case}");
                assert_eq!(serde_json::to_vec(&retry).unwrap(), pending);
                std::fs::remove_file(&target).unwrap();
            }
            std::fs::write(&target, &target_bytes).unwrap();
        }
        for generation in ["../outside", "", "AA", &"F".repeat(64), &"0".repeat(64)] {
            let mut loaded = restored(&serde_json::to_vec(&evidence).unwrap(), dir.path());
            loaded.archive.as_mut().unwrap().generation = Some(generation.into());
            assert!(loaded.snapshot().archive_unavailable);
            primary(&mut loaded, 3);
            assert!(loaded.flush_numeric().is_err());
            assert_eq!(loaded.numeric_pending.len(), 1);
        }
        for generation in [
            serde_json::Value::Null,
            serde_json::json!(false),
            serde_json::json!(42),
            serde_json::json!(["bad"]),
            serde_json::json!({"bad": true}),
        ] {
            let mut value = serde_json::to_value(&evidence).unwrap();
            value["archive"]["generation"] = generation;
            let loaded = restored(&serde_json::to_vec(&value).unwrap(), dir.path());
            assert!(loaded.snapshot().archive_unavailable);
        }
        // Earlier corruption (last record remains valid) must not be laundered
        // into a healthy fork by the ordinary hot writer's last-record check.
        let mut broken = orphan.clone();
        broken[0] = b'[';
        std::fs::write(&path, &broken).unwrap();
        let mut retry = restored(&pending, dir.path());
        assert!(retry.flush_numeric().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), broken);
        assert_eq!(std::fs::read(&target).unwrap(), target_bytes);
    }

    #[test]
    fn censored_gap_and_same_owner_namespace_changes_survive_numeric_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut evidence = RequestEvidence::default();
        evidence.bind_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        primary(&mut evidence, 0);
        evidence.prepare_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        let bytes = serde_json::to_vec(&evidence).unwrap();
        let mut next: RequestEvidence = serde_json::from_slice(&bytes).unwrap();
        next.bind_archive(dir.path(), "fixture-owner", Some("changed-H-settings"));
        assert_eq!(
            serde_json::to_value(next.snapshot()).unwrap(),
            serde_json::to_value(evidence.snapshot()).unwrap()
        );
        next.prior_untracked(1); // an actual censored attempt, not semantic rollover
        primary(&mut next, 1);
        next.prepare_archive(dir.path(), "fixture-owner", Some("changed-H-settings"));
        let restarted = restored(&serde_json::to_vec(&next).unwrap(), dir.path());
        let estimate = restarted.snapshot();
        assert_eq!(estimate.coverage.prior_untracked_calls, 1);
        assert_eq!(estimate.coverage.primary_responses, 2);
        assert!(estimate.dashboard(false).whole_window.is_none());
    }

    #[test]
    fn byte_saturated_numeric_chunks_seal_without_losing_real_appends() {
        let dir = tempfile::tempdir().unwrap();
        let mut evidence = RequestEvidence::default();
        evidence.bind_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        let body = json!({"model":"model-alias-".repeat(9000),"max_output_tokens":1,
            "input":[{"role":"user","content":"source"}]});
        let response = json!({"status":"completed","output":[],"usage":{"input_tokens":1000,"output_tokens":0,"input_tokens_details":{"cached_tokens":0}}});
        for n in 0..30 {
            evidence.primary_settled(PrimaryObservation {
                body: &body,
                response: &response,
                route: "route",
                at: 100 + n,
                standard: true,
                resolved: None,
                cost: Some(0.002),
                measurements: None,
            });
            assert!(evidence.pending_bytes() <= MAX_PENDING_BYTES);
            assert_eq!(
                evidence.pending_bytes(),
                evidence
                    .numeric_pending
                    .iter()
                    .map(|event| serde_json::to_vec(event).unwrap().len())
                    .sum::<usize>()
            );
            assert!(evidence.numeric_pending.len() <= RequestEvidence::MAX_OBSERVATIONS);
            assert!(!evidence.numeric_lost);
        }
        assert!(
            evidence
                .archive
                .as_ref()
                .is_some_and(|mark| mark.events > 0),
            "real appends sealed a bounded numeric chunk"
        );
        evidence.prepare_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        let restarted = restored(&serde_json::to_vec(&evidence).unwrap(), dir.path());
        let mut count = 0;
        restarted.scan_numeric(|_| count += 1).unwrap();
        assert_eq!(count, 30);
        assert_eq!(
            restarted
                .snapshot()
                .chronological
                .unwrap()
                .primary_responses,
            30
        );
    }

    #[test]
    fn oversized_numeric_record_is_explicitly_unknown_not_an_unbounded_checkpoint_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let mut evidence = RequestEvidence::default();
        evidence.bind_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        let body = json!({"model":"x".repeat(MAX_RECORD as usize),"max_output_tokens":1,
            "input":[{"role":"user","content":"source"}]});
        let response = json!({"status":"completed","output":[],"usage":{"input_tokens":1000,"output_tokens":0,"input_tokens_details":{"cached_tokens":0}}});
        evidence.primary_settled(PrimaryObservation {
            body: &body,
            response: &response,
            route: "route",
            at: 100,
            standard: true,
            resolved: None,
            cost: Some(0.002),
            measurements: None,
        });
        assert!(evidence.numeric_lost && evidence.numeric_pending.is_empty());
        assert_eq!(evidence.snapshot().coverage.primary_responses, 1);
        assert!(evidence.snapshot().dashboard(false).whole_window.is_none());
    }

    #[test]
    fn chronological_archive_does_not_collect_lifetime_policy_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let mut evidence = RequestEvidence::default();
        evidence.bind_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        let mut resolved = carry::models::resolve_model(
            "gpt-6.1-sol",
            carry::models::Route::new("https://api.openai.com/v1/responses", false),
            None,
            None,
            0,
            0,
        )
        .unwrap();
        let body = json!({"model":"gpt-6.1-sol","max_output_tokens":1,"input":[{"role":"user","content":"source"}]});
        let response = json!({"status":"completed","output":[],"usage":{"input_tokens":1000,"output_tokens":0,"input_tokens_details":{"cached_tokens":0}}});
        for n in 0..128 {
            resolved.scenario_cache_policy.as_mut().unwrap().version = 1 + n % 2;
            evidence.primary_settled(PrimaryObservation {
                body: &body,
                response: &response,
                route: "route",
                at: 100 + n as u64,
                standard: true,
                resolved: Some(&resolved),
                cost: Some(0.002),
                measurements: None,
            });
            evidence.prepare_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        }
        let mut versions = Vec::new();
        evidence
            .scan_numeric(|event| {
                if let Observation::Primary { facts, .. } = event {
                    versions.push(
                        facts
                            .resolved
                            .as_ref()
                            .unwrap()
                            .scenario_cache_policy
                            .as_ref()
                            .unwrap()
                            .version,
                    );
                }
            })
            .unwrap();
        assert_eq!(versions.len(), 128);
        assert!(versions.windows(2).all(|p| p[0] != p[1]));
        let chronological = evidence.replay_snapshot(true);
        assert_eq!(chronological.coverage.primary_responses, 128);
        assert!(
            chronological.cache_policy_boundaries.is_empty(),
            "RAM-bound: chronological replay retains an unused lifetime policy list"
        );
        // Bounded strict suffix diagnostics still disclose their version changes.
        assert_eq!(
            evidence
                .replay_snapshot(false)
                .cache_policy_boundaries
                .len(),
            127
        );
    }

    #[test]
    fn failed_replay_cannot_be_certified_by_a_later_healthy_scan() {
        let dir = tempfile::tempdir().unwrap();
        let mut evidence = RequestEvidence::default();
        evidence.bind_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        primary(&mut evidence, 0);
        primary(&mut evidence, 1);
        evidence.prepare_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        let before = evidence.snapshot();
        std::fs::write(
            dir.path().join(".numeric-read-fault"),
            b"fail first traversal",
        )
        .unwrap();
        let failed = evidence.snapshot();
        assert_eq!(
            failed.coverage.primary_responses, before.coverage.primary_responses,
            "integrity: partial first replay must not replace complete retained money/counts"
        );
        assert_eq!(failed.observed_primary_cost, before.observed_primary_cost);
        assert!(failed.archive_unavailable);
        assert!(failed.chronological.is_none());
        let recovered = evidence.snapshot();
        assert!(!recovered.archive_unavailable);
        assert_eq!(recovered.chronological.unwrap().primary_responses, 2);
    }

    #[test]
    fn numeric_storage_has_no_bodies_vectors_or_saved_predictions() {
        let dir = tempfile::tempdir().unwrap();
        let mut evidence = RequestEvidence::default();
        evidence.bind_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        primary(&mut evidence, 0);
        evidence.prepare_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        let bytes = std::fs::read(
            evidence
                .archive_access
                .as_ref()
                .unwrap()
                .path(evidence.archive.as_ref().unwrap())
                .unwrap(),
        )
        .unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        for sentinel in [
            "private-input-sentinel",
            "private-response-sentinel",
            "private-instruction-sentinel",
            "traditional_cost",
            "summary_cost",
            "canonical_offset",
            "retired",
        ] {
            assert!(!text.contains(sentinel), "archive contains {sentinel}");
        }
        let record: Record = serde_json::from_slice(&bytes).unwrap();
        let Observation::Primary { facts, .. } = record.event else {
            panic!("primary");
        };
        assert!(
            facts.input.is_none()
                && facts.semantic_input.is_none()
                && facts.semantic_output.is_none()
        );
        assert!(facts.prompt.is_some() && facts.fixed.is_some());
        assert!(evidence.numeric_pending.is_empty());
    }

    #[test]
    fn corrupt_missing_foreign_and_truncated_archives_are_unknown_not_core_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut evidence = RequestEvidence::default();
        evidence.bind_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        primary(&mut evidence, 0);
        evidence.prepare_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
        let checkpoint = serde_json::to_vec(&evidence).unwrap();
        let path = evidence
            .archive_access
            .as_ref()
            .unwrap()
            .path(evidence.archive.as_ref().unwrap())
            .unwrap();
        let original = std::fs::read(&path).unwrap();
        for case in 0..5 {
            std::fs::write(&path, &original).unwrap();
            match case {
                0 => std::fs::remove_file(&path).unwrap(),
                1 => std::fs::write(&path, b"{corrupt archive}\n").unwrap(),
                2 => std::fs::write(&path, &original[..original.len() - 1]).unwrap(),
                3 => {
                    let mut record: Record = serde_json::from_slice(&original).unwrap();
                    record.owner = "foreign-owner".into();
                    let mut bytes = serde_json::to_vec(&record).unwrap();
                    bytes.push(b'\n');
                    std::fs::write(&path, bytes).unwrap();
                }
                _ => {
                    let mut bytes = original.clone();
                    bytes[0] = b'[';
                    std::fs::write(&path, bytes).unwrap();
                }
            }
            let mut current = restored(&checkpoint, dir.path());
            assert!(
                current.snapshot().dashboard(false).whole_window.is_none(),
                "case {case}"
            );
            primary(&mut current, 1);
            current.prepare_archive(dir.path(), "fixture-owner", Some("fixture-namespace"));
            assert_eq!(
                current.numeric_pending.len(),
                1,
                "failed archive keeps the numeric receipt in checkpoint"
            );
            let restarted = restored(&serde_json::to_vec(&current).unwrap(), dir.path());
            assert!(restarted.snapshot().dashboard(false).whole_window.is_none());
        }
    }
}
