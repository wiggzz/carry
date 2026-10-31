//! Server-owned exact-prefix index. Immutable nodes share canonical wire values;
//! Completed advice is prefix-bound; owners hold transport and paid accounting only.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

static IO: std::sync::Mutex<()> = std::sync::Mutex::new(());
static PINS: std::sync::Mutex<BTreeMap<(PathBuf, String), Owner>> =
    std::sync::Mutex::new(BTreeMap::new());

struct Owner {
    count: usize,
    // Only a fresh resolution grants coalescing authority. An arbitrary pin on
    // a historical owner must not turn a lookup miss into restored authority.
    reservation: Option<String>,
}

/// Selected authority stays live across lock waits, primary transport and paid
/// reviewer settlement. Restart censors pending work, rather than reviving workers.
pub(super) struct Pin((PathBuf, String));
impl Drop for Pin {
    fn drop(&mut self) {
        let mut pins = PINS.lock().unwrap();
        let owner = pins.get_mut(&self.0).unwrap();
        owner.count -= 1;
        if owner.count == 0 {
            pins.remove(&self.0);
        }
    }
}
pub(super) fn pin(service: &Service, id: &str) -> Result<Pin, Failure> {
    pin_owner(service, id, &mut PINS.lock().unwrap())
}
fn pin_owner(
    service: &Service,
    id: &str,
    pins: &mut BTreeMap<(PathBuf, String), Owner>,
) -> Result<Pin, Failure> {
    let key = (service.config.state_dir.clone(), id.to_owned());
    if !pins.contains_key(&key)
        && pins.keys().filter(|(dir, _)| dir == &key.0).count()
            >= service.config.history_max_references as usize
    {
        return Err(failure(
            StatusCode::INSUFFICIENT_STORAGE,
            "history owners busy",
        ));
    }
    pins.entry(key.clone())
        .or_insert(Owner {
            count: 0,
            reservation: None,
        })
        .count += 1;
    Ok(Pin(key))
}

// Called under IO: reserve a genuinely cold owner without promoting an expired
// snapshot or mirror. Keep the pin lock through lookup/acquire so the last live
// fresh request cannot disappear between coalescing and pinning its successor.
fn reserve(
    service: &Service,
    key: String,
    occupied: &HashSet<String>,
) -> Result<(String, Pin), Failure> {
    let mut pins = PINS.lock().unwrap();
    let existing = pins.iter().find_map(|((dir, id), owner)| {
        (dir == &service.config.state_dir
            && !occupied.contains(id)
            && owner.reservation.as_ref() == Some(&key))
        .then(|| id.clone())
    });
    if let Some(id) = existing {
        let pin = pin_owner(service, &id, &mut pins)?;
        return Ok((id, pin));
    }
    let mut id = key.clone();
    let mut generation = 0_u64;
    while occupied.contains(&id)
        || pins.contains_key(&(service.config.state_dir.clone(), id.clone()))
        || service.config.state_dir.join(format!("{id}.json")).exists()
    {
        generation += 1;
        id = hash(&(&key, generation));
    }
    let pin = pin_owner(service, &id, &mut pins)?;
    pins.get_mut(&pin.0).unwrap().reservation = Some(key);
    Ok((id, pin))
}
fn pinned(service: &Service) -> HashSet<String> {
    PINS.lock()
        .unwrap()
        .keys()
        .filter(|(dir, _)| dir == &service.config.state_dir)
        .map(|(_, id)| id.clone())
        .collect()
}

#[derive(Default, Serialize, Deserialize)]
#[serde(tag = "format", rename = "carry-prefix-history-v3")]
struct Index {
    nodes: BTreeMap<String, Node>,
    upstream_cache: Vec<CacheEvidence>,
    tips: BTreeMap<String, Reference>,
    reviews: BTreeMap<String, semantic::Record>,
    #[serde(default)]
    transcripts: BTreeMap<String, Transcript>,
    #[serde(default)]
    snapshots: BTreeMap<String, Session>,
}
#[derive(Serialize, Deserialize)]
struct Node {
    parent: String,
    value: Value,
}
#[derive(Serialize, Deserialize)]
struct Reference {
    session: String,
    namespace: String,
    at: u64,
}
#[derive(Serialize, Deserialize)]
struct Transcript {
    namespace: String,
    external_id: String,
    frontier: String,
    session: String,
    at: u64,
}
fn hash(value: &impl Serialize) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}
fn path(namespace: &str, input: &[Value]) -> Vec<String> {
    let mut parent = namespace.to_owned();
    input
        .iter()
        .map(|value| {
            parent = hash(&(parent.as_str(), value));
            parent.clone()
        })
        .collect()
}
fn minimum_bytes() -> std::io::Result<usize> {
    Ok(serde_json::to_vec(&Index::default())?.len())
}

pub(super) fn validate_capacity(config: &ProxyCli) -> std::io::Result<()> {
    let minimum = minimum_bytes()?;
    if (config.history_max_bytes as usize) < minimum {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("history-max-bytes must fit the serialized empty index ({minimum} bytes)"),
        ));
    }
    Ok(())
}

fn read(service: &Service) -> std::io::Result<Index> {
    match std::fs::read(service.config.state_dir.join("history.index")) {
        Ok(bytes) if bytes.len() <= service.config.history_max_bytes as usize => {
            serde_json::from_slice(&bytes).map_err(std::io::Error::other)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Index::default()),
        _ => Err(std::io::Error::other("history index unavailable")),
    }
}
fn insert(index: &mut Index, namespace: &str, input: &[Value]) -> Vec<String> {
    let keys = path(namespace, input);
    let mut parent = namespace.to_owned();
    for (key, value) in keys.iter().zip(input) {
        index.nodes.entry(key.clone()).or_insert_with(|| Node {
            parent: parent.clone(),
            value: value.clone(),
        });
        parent = key.clone();
    }
    keys
}
fn gc(index: &mut Index, pins: &HashSet<String>) {
    let live = index
        .tips
        .values()
        .map(|r| r.session.as_str())
        .collect::<HashSet<_>>();
    index
        .snapshots
        .retain(|id, _| live.contains(id.as_str()) || pins.contains(id));
    let mut reachable = HashSet::new();
    for root in index
        .tips
        .keys()
        .chain(index.reviews.keys())
        .chain(index.transcripts.values().map(|r| &r.frontier))
        .chain(
            index
                .snapshots
                .values()
                .filter_map(|s| s.history_frontier.as_ref()),
        )
    {
        let mut key = root.as_str();
        while reachable.insert(key.to_owned()) {
            let Some(node) = index.nodes.get(key) else {
                break;
            };
            key = &node.parent;
        }
    }
    index.nodes.retain(|key, _| reachable.contains(key));
}
fn prune(service: &Service, index: &mut Index) -> bool {
    let at = now();
    crate::proxy_cache::trim(&mut index.upstream_cache, at);
    let pins = pinned(service);
    index.tips.retain(|_, r| {
        pins.contains(&r.session) || (r.at <= at && at - r.at < service.config.history_max_age_secs)
    });
    index
        .reviews
        .retain(|_, r| r.at <= at && at - r.at < service.config.history_max_age_secs);
    index
        .transcripts
        .retain(|_, r| r.at <= at && at - r.at < service.config.history_max_age_secs);
    loop {
        gc(index, &pins);
        if index.nodes.len() <= service.config.history_max_nodes as usize
            && index.tips.len() + index.reviews.len() + index.transcripts.len()
                <= service.config.history_max_references as usize
            && serde_json::to_vec(index).unwrap().len() <= service.config.history_max_bytes as usize
        {
            return true;
        }
        let review = index
            .reviews
            .iter()
            .map(|(key, r)| (r.at, key.clone()))
            .min();
        let tip = index
            .tips
            .iter()
            .filter(|(_, r)| !pins.contains(&r.session))
            .map(|(k, r)| (r.at, k.clone()))
            .min();
        let transcript = index
            .transcripts
            .iter()
            .map(|(k, r)| (r.at, k.clone()))
            .min();
        let oldest = review
            .as_ref()
            .map(|r| r.0)
            .into_iter()
            .chain(tip.as_ref().map(|r| r.0))
            .min();
        if let Some((at, key)) = transcript
            && oldest.is_none_or(|t| at <= t)
        {
            index.transcripts.remove(&key);
            continue;
        }
        match (review, tip) {
            (Some((at, key)), Some((t, _))) if at <= t => {
                index.reviews.remove(&key);
            }
            (_, Some((_, key))) => {
                index.tips.remove(&key);
            }
            (Some((_, key)), None) => {
                index.reviews.remove(&key);
            }
            _ => return false,
        }
    }
}

/// Owner/settings identity is a boundary, not new authentication or native UI
/// ancestry. Exact content identifies compatible histories within that boundary.
pub(super) fn namespace(
    service: &Service,
    headers: &HeaderMap,
    body: &Value,
) -> Result<String, Failure> {
    let mut owner = headers.clone();
    if identity(headers)?.is_none() {
        owner.insert("x-carry-session", "carry-active-prefix".parse().unwrap());
    }
    Ok(hash(
        &json!({"policy":"carry-prefix-history-v3", "owner":identity(&owner)?,
        "settings":semantic::scope(&service.config, body),
        "client_scope":stable_client_scope(body)}),
    ))
}

pub(super) fn conversation(body: &Value) -> Option<&str> {
    body["conversation"]
        .as_str()
        .or_else(|| body["conversation"]["id"].as_str())
}

/// Expand only immutable, successfully observed frontiers; never ask the provider
/// to reconstruct an unknown ancestor or silently send a delta as fresh history.
pub(super) fn expand(service: &Service, namespace: &str, body: &mut Value) -> Result<(), Failure> {
    let previous = body.get("previous_response_id").filter(|v| !v.is_null());
    let conv = body.get("conversation").filter(|v| !v.is_null());
    if previous.is_some() && conv.is_some() {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "conversation and previous_response_id are mutually exclusive",
        ));
    }
    let reference = if let Some(v) = previous {
        Some((
            "response",
            v.as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "invalid previous_response_id"))?,
        ))
    } else if conv.is_some() {
        Some((
            "conversation",
            conversation(body)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "invalid conversation"))?,
        ))
    } else {
        None
    };
    let Some((kind, external)) = reference else {
        return Ok(());
    };
    let _guard = IO.lock().unwrap();
    let mut index = read(service).map_err(|_| {
        failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "history index unavailable",
        )
    })?;
    prune(service, &mut index);
    let reference = index
        .transcripts
        .get(&hash(&(namespace, kind, external)))
        .ok_or_else(|| {
            failure(
                StatusCode::CONFLICT,
                "unknown, expired or unobserved transcript identifier",
            )
        })?;
    let mut key = reference.frontier.as_str();
    let mut input = Vec::new();
    let mut visited = HashSet::new();
    while key != namespace {
        if !visited.insert(key) {
            return Err(failure(StatusCode::CONFLICT, "invalid transcript ancestry"));
        }
        let node = index
            .nodes
            .get(key)
            .ok_or_else(|| failure(StatusCode::CONFLICT, "transcript ancestry unavailable"))?;
        input.push(node.value.clone());
        key = &node.parent;
    }
    input.reverse();
    input.extend(
        body["input"]
            .as_array()
            .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "array input required"))?
            .iter()
            .cloned(),
    );
    body["input"] = json!(input);
    body.as_object_mut().unwrap().remove("previous_response_id");
    body.as_object_mut().unwrap().remove("conversation");
    Ok(())
}

pub(super) fn resolve(
    service: &Service,
    namespace: &str,
    body: &Value,
) -> Result<(String, Pin), Failure> {
    let _guard = IO.lock().unwrap();
    let mut index = read(service).map_err(|_| {
        failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "history index unavailable",
        )
    })?;
    // Pruning is a read-time projection. Those historical IDs remain occupied
    // in the on-disk generation until publication; pinning one would otherwise
    // make restore protect and resurrect exactly the snapshot just retired.
    let occupied = index.snapshots.keys().cloned().collect::<HashSet<_>>();
    prune(service, &mut index);
    let input = body["input"]
        .as_array()
        .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "array input required"))?;
    let keys = path(namespace, input);
    // Routing has no semantic authority. Reuse only a compatible live owner;
    // every request independently looks up completed advice below.
    for key in keys
        .iter()
        .rev()
        .chain(std::iter::once(&namespace.to_owned()))
    {
        if let Some(reference) = index.tips.get(key)
            && let Some(snapshot) = index.snapshots.get(&reference.session)
            && snapshot.history.len() <= input.len()
        {
            return Ok((reference.session.clone(), pin(service, &reference.session)?));
        }
    }
    reserve(service, hash(&(namespace, keys.last())), &occupied)
}

/// Selected checkpoints reference canonical nodes; the atomic index is their
/// authority. The per-session JSON is a diagnostic mirror, never new authority.
pub(super) fn compact(session: &Session) -> Session {
    let mut snapshot = session.clone();
    if snapshot.history_shared {
        let namespace = snapshot.history_namespace.as_deref().unwrap();
        let input = snapshot
            .history
            .iter()
            .map(|i| i.value.clone())
            .collect::<Vec<_>>();
        snapshot.history_frontier = path(namespace, &input).last().cloned();
        for item in &mut snapshot.history {
            item.value = Value::Null;
        }
    }
    snapshot
}

pub(super) fn restore(service: &Service, id: &str) -> std::io::Result<Session> {
    let _guard = IO.lock().unwrap();
    let mut index = read(service)?;
    prune(service, &mut index);
    let session = index
        .snapshots
        .remove(id)
        .ok_or_else(|| std::io::Error::other("selected history unavailable"))?;
    canonical(&index, session)
}

fn canonical(index: &Index, mut session: Session) -> std::io::Result<Session> {
    let mut key = session.history_frontier.as_deref();
    for item in session.history.iter_mut().rev() {
        let node = key
            .and_then(|k| index.nodes.get(k))
            .ok_or_else(|| std::io::Error::other("canonical history unavailable"))?;
        item.value = node.value.clone();
        key = Some(&node.parent);
    }
    if key != session.history_namespace.as_deref() && !session.history.is_empty() {
        return Err(std::io::Error::other("invalid canonical ancestry"));
    }
    session.history_frontier = None;
    Ok(session)
}

/// Publish nodes, reviewed versions and selected metadata in ONE atomic index.
/// A mirror written before this transaction cannot publish uncommitted advice.
pub(super) fn committed(
    service: &Service,
    id: &str,
    session: &Session,
    observation: Option<(&[Value], &Value)>,
) -> std::io::Result<()> {
    let Some(namespace) = &session.history_namespace else {
        return Ok(());
    };
    let _guard = IO.lock().unwrap();
    let mut index = read(service)?;
    let input = session
        .history
        .iter()
        .map(|i| i.value.clone())
        .collect::<Vec<_>>();
    let prior = index.snapshots.keys().cloned().collect::<HashSet<_>>();
    let keys = insert(&mut index, namespace, &input);
    let observed_keys = observation
        .map(|(input, _)| insert(&mut index, namespace, input))
        .unwrap_or_else(|| keys.clone());
    // Only an actual successful observation creates external authority. Pending,
    // reviewer and failed-primary saves preserve existing immutable bindings;
    // mutable session frontiers must never be re-scoped under current settings.
    let completed = observation.map(|(input, response)| {
        (
            response["id"].as_str(),
            conversation(response).or(session.history_conversation.as_deref()),
            input
                .iter()
                .cloned()
                .chain(response["output"].as_array().into_iter().flatten().cloned())
                .collect::<Vec<_>>(),
        )
    });
    if let Some((Some(response), conversation, transcript)) = completed {
        let full_keys = insert(&mut index, namespace, &transcript);
        let key = full_keys.last().unwrap_or(namespace);
        for (kind, external) in [("response", Some(response)), ("conversation", conversation)] {
            if let Some(external) = external {
                let reference_key = hash(&(namespace, kind, external));
                let reference = Transcript {
                    namespace: namespace.clone(),
                    external_id: external.to_owned(),
                    frontier: key.clone(),
                    session: id.into(),
                    at: now(),
                };
                if kind == "response" {
                    index.transcripts.entry(reference_key).or_insert(reference);
                } else {
                    index.transcripts.insert(reference_key, reference);
                }
            }
        }
    }
    index.tips.retain(|_, r| r.session != id);
    let key = observed_keys.last().unwrap_or(namespace);
    index.tips.insert(
        key.clone(),
        Reference {
            session: id.into(),
            namespace: namespace.clone(),
            at: now(),
        },
    );
    if session.history_shared {
        index.snapshots.insert(id.into(), compact(session));
    }
    persist(service, index, prior, session.history_shared.then_some(id))
}

pub(super) fn advice(
    service: &Service,
    namespace: &str,
    input: &[Value],
) -> std::io::Result<Option<semantic::Record>> {
    let _guard = IO.lock().unwrap();
    let mut index = read(service)?;
    prune(service, &mut index);
    // Select the newest record at the longest cut FIRST. Invalid individual
    // actions fail closed; never fall back to an older record/fact.
    Ok(path(namespace, input)
        .iter()
        .rev()
        .find_map(|key| index.reviews.get(key).cloned()))
}

pub(super) fn publish(
    service: &Service,
    subject: &Session,
    reviewed: &Session,
) -> std::io::Result<()> {
    let namespace = subject
        .history_namespace
        .as_deref()
        .ok_or_else(|| std::io::Error::other("review namespace missing"))?;
    let _guard = IO.lock().unwrap();
    let mut index = read(service)?;
    let input = subject
        .history
        .iter()
        .map(|i| i.value.clone())
        .collect::<Vec<_>>();
    let keys = insert(&mut index, namespace, &input);
    let key = keys
        .last()
        .unwrap_or(&subject.history_namespace.clone().unwrap())
        .clone();
    let record = semantic::Record::capture(reviewed);
    if index
        .reviews
        .get(&key)
        .is_none_or(|old| old.generation < record.generation)
    {
        index.reviews.insert(key, record);
    }
    let prior = index.snapshots.keys().cloned().collect();
    persist(service, index, prior, None)
}

fn persist(
    service: &Service,
    mut index: Index,
    prior: HashSet<String>,
    owner: Option<&str>,
) -> std::io::Result<()> {
    if !prune(service, &mut index) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            "history capacity held by active owners",
        ));
    }
    // Validate the exact bytes that will become authority before opening the
    // temporary file. No failed publication may replace a readable generation.
    let bytes = serde_json::to_vec(&index)?;
    if bytes.len() > service.config.history_max_bytes as usize {
        return Err(std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            "history index exceeds configured byte bound",
        ));
    }
    let temporary = service.config.state_dir.join(".history-index.tmp");
    let mut file = private_file(&temporary, false)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    std::fs::rename(temporary, service.config.state_dir.join("history.index"))?;
    File::open(&service.config.state_dir)?.sync_all()?;
    for stale in prior.into_iter().chain(owner.map(str::to_owned)) {
        if !index.snapshots.contains_key(&stale) {
            let _ = std::fs::remove_file(service.config.state_dir.join(format!("{stale}.json")));
        }
    }
    Ok(())
}

/// Only actual terminal observations mutate cache evidence. Session/reviewer
/// snapshots are disposable read views, never sources to merge back. The IO
/// transaction orders receipts even when TTL timestamps have the same second.
pub(super) fn observe_cache(
    service: &Service,
    body: &Value,
    usage: &Value,
    resolved: Option<&ResolvedModel>,
    credential: &str,
    time: crate::proxy_cache::ObservationTime,
) -> std::io::Result<Vec<CacheEvidence>> {
    let _guard = IO.lock().unwrap();
    let mut index = read(service)?;
    if crate::proxy_cache::observe(
        &mut index.upstream_cache,
        body,
        usage,
        resolved,
        credential,
        time,
    ) {
        let cache = index.upstream_cache.clone();
        let prior = index.snapshots.keys().cloned().collect();
        persist(service, index, prior, None)?;
        Ok(cache)
    } else {
        Ok(index.upstream_cache)
    }
}

pub(super) fn cache(service: &Service) -> std::io::Result<Vec<CacheEvidence>> {
    let _guard = IO.lock().unwrap();
    let mut index = read(service)?;
    crate::proxy_cache::trim(&mut index.upstream_cache, now());
    Ok(index.upstream_cache)
}
