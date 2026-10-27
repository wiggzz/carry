//! Best-effort evidence only. Checkpoints and lifetime accounting never enter retention.
use super::*;
use serde::Serialize;
use std::io::{BufRead, BufReader, Read};

const RAW_RECORD: usize = 64 * 1024;
const RAW_LANE: u64 = 256 * 1024;
const RAW_TOTAL: u64 = 1024 * 1024;
const JOURNAL_RECORD: usize = 16 * 1024;
const JOURNAL_LANE: u64 = 64 * 1024;
const JOURNAL_TOTAL: u64 = 256 * 1024;

// One directory is OS-locked by Service; this also serializes different lanes
// and test Services. No unbounded per-lane maps or checkpoint coupling.
static LOG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static RAW_DROPPED: AtomicU64 = AtomicU64::new(0);
static JOURNAL_DROPPED: AtomicU64 = AtomicU64::new(0);

#[path = "proxy_fingerprint.rs"]
mod fingerprints;
pub(super) use fingerprints::{ReviewWire, review_wire};

#[derive(Serialize)]
struct Record<'a> {
    event: &'a str,
    at: u64,
    session_id: &'a str,
    request_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    review_attempt_id: Option<&'a str>,
    mode: &'a str,
    policy_id: String,
    dropped_before: u64,
    data: &'a Value,
}

struct Limited(Vec<u8>, usize);
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.1.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("log record limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn mode(config: &ProxyCli) -> &'static str {
    match config.mode {
        Mode::Off => "off",
        Mode::Audit => "audit",
        Mode::Compact => "compact",
    }
}

fn directory(service: &Service, raw: bool) -> std::io::Result<PathBuf> {
    let path = if raw {
        service.config.state_dir.clone()
    } else {
        service.config.state_dir.join("diagnostics")
    };
    if !raw {
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if !meta.file_type().is_dir() => {
                return Err(std::io::Error::other("invalid journal directory"));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = std::fs::DirBuilder::new();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    builder.mode(0o700);
                }
                builder.create(&path)?;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(path)
}

fn files(directory: &Path) -> std::io::Result<Vec<(PathBuf, u64, SystemTime)>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        // identity() has always named session logs with a lowercase SHA-256.
        // Raw logs share the root with independent benchmark/export artifacts;
        // neither eviction nor restart validation owns arbitrary JSONL files.
        let owned = path.extension().is_some_and(|ext| ext == "jsonl")
            && path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|id| {
                    id.len() == 64
                        && id
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                });
        if owned && entry.file_type()?.is_file() {
            let meta = entry.metadata()?;
            files.push((path, meta.len(), meta.modified().unwrap_or(UNIX_EPOCH)));
        }
    }
    files.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
    Ok(files)
}

// Evict complete oldest lane files, never partial JSON. Emptying the active lane
// at its ceiling deliberately trades history for a small, predictable writer.
fn retain(
    directory: &Path,
    target: Option<&Path>,
    incoming: u64,
    lane: u64,
    total: u64,
    dropped: &AtomicU64,
) -> std::io::Result<()> {
    let mut used = 0u64;
    let mut retained = Vec::new();
    for (path, bytes, modified) in files(directory)? {
        if bytes > lane || (target == Some(path.as_path()) && bytes.saturating_add(incoming) > lane)
        {
            std::fs::remove_file(path)?;
            dropped.fetch_add(1, Ordering::Relaxed);
        } else {
            used = used.saturating_add(bytes);
            retained.push((path, bytes, modified));
        }
    }
    for (path, bytes, _) in retained {
        if used.saturating_add(incoming) <= total {
            break;
        }
        std::fs::remove_file(path)?;
        used = used.saturating_sub(bytes);
        dropped.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}

pub(super) fn maintain(service: &Service) {
    let Ok(_guard) = LOG_LOCK.lock() else {
        return;
    };
    for raw in [false, true] {
        // A default restart leaves old opt-in raw evidence untouched.
        if raw && !service.config.verbose {
            continue;
        }
        let (record_max, lane, total, dropped) = if raw {
            (RAW_RECORD, RAW_LANE, RAW_TOTAL, &RAW_DROPPED)
        } else {
            (
                JOURNAL_RECORD,
                JOURNAL_LANE,
                JOURNAL_TOTAL,
                &JOURNAL_DROPPED,
            )
        };
        let result = (|| -> std::io::Result<()> {
            let directory = directory(service, raw)?;
            retain(&directory, None, 0, lane, total, dropped)?;
            // Recover legacy/partial files without reading an unbounded line.
            for (path, _, _) in files(&directory)? {
                let mut options = OpenOptions::new();
                options.read(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.custom_flags(libc::O_NOFOLLOW);
                }
                let mut reader = BufReader::new(options.open(&path)?);
                let mut line = Vec::new();
                let mut invalid = false;
                loop {
                    line.clear();
                    let n = reader
                        .by_ref()
                        .take((record_max + 1) as u64)
                        .read_until(b'\n', &mut line)?;
                    if n == 0 {
                        break;
                    }
                    if n > record_max
                        || line.last() != Some(&b'\n')
                        || serde_json::from_slice::<Value>(&line).is_err()
                    {
                        invalid = true;
                        break;
                    }
                }
                if invalid {
                    std::fs::remove_file(path)?;
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
            Ok(())
        })();
        if result.is_err() {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn append(
    service: &Service,
    id: &str,
    request_id: &str,
    review_attempt_id: Option<&str>,
    event: &str,
    data: &Value,
    raw: bool,
) {
    let (record_max, lane, total, dropped) = if raw {
        (RAW_RECORD, RAW_LANE, RAW_TOTAL, &RAW_DROPPED)
    } else {
        (
            JOURNAL_RECORD,
            JOURNAL_LANE,
            JOURNAL_TOTAL,
            &JOURNAL_DROPPED,
        )
    };
    let result = (|| -> std::io::Result<()> {
        let _guard = LOG_LOCK
            .lock()
            .map_err(|_| std::io::Error::other("log lock poisoned"))?;
        let config = &service.config;
        // Hash effective settings, never persist labels, endpoints, or keys.
        let mut policy = Sha256::new();
        policy.update(config.classifier_model.as_bytes());
        policy.update(format!(
            "{:?}",
            (
                config.mode,
                config.classifier_cache_policy,
                &config.classifier_reasoning_effort,
                config.review_replayed_history,
                config.bootstrap_min_input_bytes
            )
        ));
        policy.update(format!(
            "{:?}",
            (
                config.primary_context_tokens,
                config.classifier_max_input_tokens,
                config.classifier_max_input_bytes,
                config.review_every_requests,
                config.payoff_requests,
                config.min_payback_percent,
                config.context_margin_percent,
                config.primary_output_reserve_tokens,
                config.classifier_max_output_tokens
            )
        ));
        policy.update(format!(
            "{:?}",
            (
                config.review_min_primary_tokens,
                config.review_min_new_tokens,
                config.review_backoff_factor,
                config.review_backoff_max
            )
        ));
        let record = Record {
            event,
            at: now(),
            session_id: id,
            request_id,
            review_attempt_id,
            mode: mode(config),
            policy_id: format!("{:x}", policy.finalize()),
            dropped_before: dropped.load(Ordering::Relaxed),
            data,
        };
        let mut bytes = Limited(Vec::new(), record_max - 1);
        serde_json::to_writer(&mut bytes, &record)?;
        bytes.0.push(b'\n');
        let directory = directory(service, raw)?;
        let path = directory.join(format!("{id}.jsonl"));
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if !meta.file_type().is_file() => {
                return Err(std::io::Error::other("invalid log file"));
            }
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        retain(
            &directory,
            Some(&path),
            bytes.0.len() as u64,
            lane,
            total,
            dropped,
        )?;
        let mut file = private_file(&path, true)?;
        let before = file.metadata()?.len();
        if let Err(error) = file.write_all(&bytes.0) {
            // Do not leave a partial JSON line after a short/disk-full write.
            let _ = file.set_len(before);
            return Err(error);
        }
        Ok(())
    })();
    if result.is_err() {
        dropped.fetch_add(1, Ordering::Relaxed);
    }
}

pub(super) fn raw(service: &Service, id: &str, event: &str, data: &Value) {
    if service.config.verbose {
        append(service, id, "", None, event, data, true);
    }
}

pub(super) fn raw_chunk(service: &Service, id: &str, bytes: &[u8]) {
    if !service.config.verbose {
        return;
    }
    // A JSON byte array expands to at most four bytes per element. Check before
    // json! allocates Values; an oversized chunk is evidence loss, not an error.
    if bytes.len() > RAW_RECORD / 5 {
        RAW_DROPPED.fetch_add(1, Ordering::Relaxed);
    } else {
        raw(
            service,
            id,
            "primary_response_chunk",
            &json!({"bytes":bytes}),
        );
    }
}

// Callers construct ONLY fixed enums, numeric facts, safe_error identifiers and
// planner summaries here. Never send a provider body, feedback prose or advice.
pub(super) fn journal(service: &Service, id: &str, request_id: &str, event: &str, data: &Value) {
    append(service, id, request_id, None, event, data, false);
}

pub(super) fn journal_attempt(
    service: &Service,
    id: &str,
    request_id: &str,
    attempt_id: &str,
    event: &str,
    data: &Value,
) {
    append(
        service,
        id,
        request_id,
        Some(attempt_id),
        event,
        data,
        false,
    );
}

pub(super) fn request_id() -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT_ATTEMPT.fetch_add(1, Ordering::Relaxed)
    )
}

pub(super) fn usage(
    service: &Service,
    id: &str,
    request_id: &str,
    actor: &str,
    response: Option<&Value>,
    model: &str,
    standard: bool,
) {
    journal(
        service,
        id,
        request_id,
        "usage",
        &usage_data(
            actor,
            response,
            model,
            standard,
            service.config.model_resolution.as_ref().map(|p| {
                if actor == "shadow" {
                    &p.reviewer
                } else {
                    &p.primary
                }
            }),
        ),
    );
}

pub(super) fn usage_data(
    actor: &str,
    response: Option<&Value>,
    model: &str,
    standard: bool,
    resolved: Option<&ResolvedModel>,
) -> Value {
    let absent = Value::Null;
    let value = response.unwrap_or(&absent);
    let usage = numeric_review_usage(&value["usage"]);
    let mut priced = carry::core::UsageLedger::default();
    priced.observe_with_rates(
        model,
        &usage,
        standard
            && value["status"] == "completed"
            && value.get("service_tier").is_none_or(|v| v == "default"),
        receipt_rates(model, &usage, resolved),
    );
    json!({"actor":actor,"native_usage":usage,
        "usage_available":usage["input_tokens"].as_u64().is_some() && usage["output_tokens"].as_u64().is_some(),
        "cost_available":priced.unavailable_cost_calls == 0,"cost_censored":priced.unavailable_cost_calls != 0})
}

pub(super) fn planned(service: &Service, id: &str, request_id: &str, report: &Value) {
    // The planner is local, but bound candidate detail independently of its size.
    let mut safe = json!({});
    for field in [
        "selected",
        "forced",
        "reason",
        "primary_fits",
        "reviewer_fits",
        "primary_estimated_tokens",
        "reviewer_estimated_tokens",
        "primary_required_tokens",
        "reviewer_required_tokens",
        "primary_context_tokens",
        "reviewer_context_tokens",
        "context_margin_percent",
        "payoff_requests",
        "min_payback_percent",
        "future_reviewer_calls",
        "current_review_cost_is_sunk",
        "estimate_basis",
        "review_completion_basis",
        "cache_basis",
    ] {
        safe[field] = report[field].clone();
    }
    let candidates = report["candidates"].as_array();
    safe["candidate_count"] = json!(candidates.map_or(0, Vec::len));
    safe["candidates"] = json!(candidates.into_iter().flatten().take(8).map(|candidate| {
        let mut summary = json!({"removed_group_count":candidate["removed_groups"].as_array().map_or(0,Vec::len)});
        for field in ["feasible", "joint", "future_reviewer_keep_calls", "future_reviewer_compact_calls",
            "primary_retained_estimated_tokens", "reviewer_retained_estimated_tokens", "reviewer_cached_estimated_tokens"] {
            summary[field] = candidate[field].clone();
        }
        summary
    }).collect::<Vec<_>>());
    journal(service, id, request_id, "paired_plan", &safe);
}

pub(super) fn changes(
    service: &Service,
    id: &str,
    request_id: &str,
    event: &str,
    before: &Session,
    after: &Session,
) {
    journal(
        service,
        id,
        request_id,
        event,
        &json!({
        "main_changed":after.compactions > before.compactions || after.native_compactions > before.native_compactions,
        "main_compactions":after.compactions.saturating_sub(before.compactions),
        "native_compactions":after.native_compactions.saturating_sub(before.native_compactions)}),
    );
}
