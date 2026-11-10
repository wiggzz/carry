//! Server-owned, disk-backed exact-prefix authority. Reference bounds limit live
//! work, not retention. Publication is one SQLite transaction; no capacity GC.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[path = "proxy_store.rs"]
mod storage;
use storage::Store;
#[cfg(test)]
#[allow(dead_code)]
#[path = "../tests/history_store.rs"]
pub(super) mod fixture;
static IO: std::sync::Mutex<()> = std::sync::Mutex::new(());
static PINS: std::sync::Mutex<BTreeMap<(PathBuf, String), Owner>> =
    std::sync::Mutex::new(BTreeMap::new());
struct Owner {
    count: usize,
    reservation: Option<String>,
}
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
fn pinned(service: &Service) -> HashSet<String> {
    PINS.lock()
        .unwrap()
        .keys()
        .filter(|(dir, _)| dir == &service.config.state_dir)
        .map(|(_, id)| id.clone())
        .collect()
}
fn reserve(service: &Service, key: String, store: &Store) -> Result<(String, Pin), Failure> {
    let mut pins = PINS.lock().unwrap();
    for ((dir, id), owner) in pins.iter() {
        if dir == &service.config.state_dir
            && owner.reservation.as_ref() == Some(&key)
            && !store.contains("snapshots", id).map_err(read_failure)?
        {
            let id = id.clone();
            return pin_owner(service, &id, &mut pins).map(|pin| (id, pin));
        }
    }
    let mut id = key.clone();
    let mut generation = 0u64;
    while store.contains("snapshots", &id).map_err(read_failure)?
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
pub(super) fn validate_capacity(config: &ProxyCli) -> std::io::Result<()> {
    if config.history_max_bytes < 256 || config.history_disk_bytes < 65536 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "history capacity below minimum working set",
        ));
    }
    Ok(())
}
fn read(service: &Service) -> std::io::Result<Store> {
    Store::open(service, pinned(service), false)
}
fn read_failure(error: std::io::Error) -> Failure {
    let status = match error.kind() {
        std::io::ErrorKind::StorageFull => StatusCode::INSUFFICIENT_STORAGE,
        std::io::ErrorKind::InvalidData => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    failure(
        status,
        if error.kind() == std::io::ErrorKind::InvalidData {
            "unsupported or invalid history index format"
        } else {
            "history authority unavailable"
        },
    )
}
pub(super) fn namespace(
    service: &Service,
    headers: &HeaderMap,
    body: &Value,
) -> Result<String, Failure> {
    let mut owner = headers.clone();
    if identity(headers)?.is_none() {
        owner.insert("x-carry-session", "carry-active-prefix".parse().unwrap());
    }
    let owner = identity(&owner)?;
    let namespace = observed_namespace(&service.config, &owner, body);
    let _guard = IO.lock().unwrap();
    read(service).map_err(read_failure)?;
    Ok(namespace)
}
fn observed_namespace(config: &ProxyCli, owner: &Option<String>, body: &Value) -> String {
    format!(
        "observed-v1:{}",
        hash(
            &json!({"owner":owner,"model":body.get("model"),"prompt":crate::savings::prompt_overhead(body),"upstream":config.upstream_url,"subscription":config.codex_login,"client_scope":stable_client_scope(body)})
        )
    )
}
pub(super) fn bind_namespace(session: &mut Session, namespace: Option<String>) {
    if session.history_namespace != namespace {
        session.history_conversation = None;
        session.review_feedback = Value::Null;
        session.review_feedback_generation = 0;
        session.history_namespace = namespace;
    }
}
pub(super) fn conversation(body: &Value) -> Option<&str> {
    body["conversation"]
        .as_str()
        .or_else(|| body["conversation"]["id"].as_str())
}
pub(super) fn expand(service: &Service, namespace: &str, body: &mut Value) -> Result<(), Failure> {
    let previous = body.get("previous_response_id").filter(|v| !v.is_null());
    let conv = body.get("conversation").filter(|v| !v.is_null());
    if previous.is_some() && conv.is_some() {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "conversation and previous_response_id are mutually exclusive",
        ));
    }
    let external = if let Some(v) = previous {
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
    let Some((kind, external)) = external else {
        return Ok(());
    };
    let _guard = IO.lock().unwrap();
    let store = read(service).map_err(read_failure)?;
    let key = hash(&(namespace, kind, external));
    if !store.live("transcripts", &key).map_err(read_failure)? {
        return Err(failure(
            StatusCode::CONFLICT,
            "unknown, expired or unobserved transcript identifier",
        ));
    }
    let reference: Transcript = store
        .get("transcripts", &key)
        .map_err(read_failure)?
        .ok_or_else(|| failure(StatusCode::CONFLICT, "transcript unavailable"))?;
    let mut input = store
        .ancestry(namespace, &reference.frontier)
        .map_err(read_failure)?;
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
pub(super) fn candidate(
    service: &Service,
    namespace: &str,
    body: &Value,
) -> Result<Option<(String, Pin)>, Failure> {
    let _guard = IO.lock().unwrap();
    let Ok(store) = read(service) else {
        return Ok(None);
    };
    let external = body["previous_response_id"]
        .as_str()
        .map(|id| ("response", id))
        .or_else(|| conversation(body).map(|id| ("conversation", id)));
    let mut owner = None;
    if let Some((kind, id)) = external {
        let key = hash(&(namespace, kind, id));
        if store.live("transcripts", &key).map_err(read_failure)?
            && let Some(reference) = store
                .get::<Transcript>("transcripts", &key)
                .map_err(read_failure)?
            && store
                .live("snapshots", &reference.session)
                .map_err(read_failure)?
        {
            owner = Some(reference.session);
        }
    }
    if owner.is_none()
        && let Some(input) = body["input"].as_array()
    {
        owner = prefix_owner(&store, namespace, input).map_err(read_failure)?;
    }
    owner
        .map(|id| pin(service, &id).map(|pin| (id, pin)))
        .transpose()
}
fn prefix_owner(
    store: &Store,
    namespace: &str,
    input: &[Value],
) -> std::io::Result<Option<String>> {
    for key in path(namespace, input)
        .iter()
        .rev()
        .map(String::as_str)
        .chain(std::iter::once(namespace))
    {
        if store.live("tips", key)?
            && let Some(reference) = store.get::<Reference>("tips", key)?
            && store.live("snapshots", &reference.session)?
            && let Some(snapshot) = store.get::<Session>("snapshots", &reference.session)?
            && snapshot.history.len() <= input.len()
        {
            return Ok(Some(reference.session));
        }
    }
    Ok(None)
}
pub(super) fn resolve(
    service: &Service,
    namespace: &str,
    body: &Value,
) -> Result<(String, Pin), Failure> {
    let _guard = IO.lock().unwrap();
    let store = read(service).map_err(read_failure)?;
    let input = body["input"]
        .as_array()
        .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "array input required"))?;
    if let Some(id) = prefix_owner(&store, namespace, input).map_err(read_failure)? {
        return Ok((id.clone(), pin(service, &id)?));
    }
    reserve(
        service,
        hash(&(namespace, path(namespace, input).last())),
        &store,
    )
}
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
    ReadView::new(service)?.restore(id)
}
/// A read transaction, not an in-memory copy of the entire retained population.
/// Mutex makes the connection safe to hold across dashboard await boundaries.
pub(super) struct ReadView {
    store: std::sync::Mutex<Store>,
    policy: String,
}
impl ReadView {
    pub(super) fn new(service: &Service) -> std::io::Result<Self> {
        let _guard = IO.lock().unwrap();
        let store = read(service)?;
        store
            .connection
            .execute_batch("BEGIN; SELECT count(*) FROM sqlite_master;")
            .map_err(storage::error)?;
        Ok(Self {
            store: std::sync::Mutex::new(store),
            policy: semantic::scope(&service.config),
        })
    }
    pub(super) fn restore(&self, id: &str) -> std::io::Result<Session> {
        let store = self.store.lock().unwrap();
        if !store.live("snapshots", id)? {
            return Err(std::io::Error::other("selected history unavailable"));
        }
        let mut session: Session = store
            .get("snapshots", id)?
            .ok_or_else(|| std::io::Error::other("selected history unavailable"))?;
        if session.history_shared && !session.history.is_empty() {
            let values = store.ancestry(
                session.history_namespace.as_deref().unwrap_or_default(),
                session.history_frontier.as_deref().unwrap_or_default(),
            )?;
            if values.len() != session.history.len() {
                return Err(std::io::Error::other("invalid canonical ancestry"));
            }
            for (item, value) in session.history.iter_mut().zip(values) {
                item.value = value;
            }
        }
        session.history_frontier = None;
        Ok(session)
    }
    pub(super) fn shared(&self, id: &str) -> std::io::Result<Option<Session>> {
        {
            let store = self.store.lock().unwrap();
            let Some(session) = store.get::<Session>("snapshots", id)? else {
                return Ok(None);
            };
            if !session.history_shared {
                return Ok(None);
            }
            if !matches!(session.version, 0 | 3) {
                return Err(std::io::Error::other(
                    "unsupported shared checkpoint format",
                ));
            }
        }
        self.restore(id).map(Some)
    }
    pub(super) fn advice(&self, namespace: &str, input: &[Value]) -> Option<semantic::Record> {
        let store = self.store.lock().unwrap();
        path(namespace, input)
            .iter()
            .rev()
            .find_map(|key| {
                store
                    .live("reviews", key)
                    .ok()?
                    .then(|| store.get::<semantic::Record>("reviews", key).ok().flatten())
                    .flatten()
            })
            .filter(|r| r.policy.as_deref() == Some(self.policy.as_str()))
    }
}
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
    let store = Store::open(service, pinned(service), true)?;
    store.begin()?;
    let input = session
        .history
        .iter()
        .map(|i| i.value.clone())
        .collect::<Vec<_>>();
    let keys = store.insert(namespace, &input)?;
    let observed_keys = if let Some((input, _)) = observation {
        store.insert(namespace, input)?
    } else {
        keys
    };
    if let Some((input, response)) = observation
        && let Some(response_id) = response["id"].as_str()
    {
        let transcript = input
            .iter()
            .cloned()
            .chain(response["output"].as_array().into_iter().flatten().cloned())
            .collect::<Vec<_>>();
        let full_keys = store.insert(namespace, &transcript)?;
        let frontier = full_keys.last().unwrap_or(namespace);
        for (kind, external) in [
            ("response", Some(response_id)),
            (
                "conversation",
                conversation(response).or(session.history_conversation.as_deref()),
            ),
        ] {
            if let Some(external) = external {
                let key = hash(&(namespace, kind, external));
                if kind != "response" || !store.contains("transcripts", &key)? {
                    store.put(
                        "transcripts",
                        &key,
                        &Transcript {
                            namespace: namespace.clone(),
                            external_id: external.into(),
                            frontier: frontier.clone(),
                            session: id.into(),
                            at: now(),
                        },
                        now(),
                        id,
                    )?;
                }
            }
        }
    }
    store
        .connection
        .execute("DELETE FROM records WHERE kind='tips' AND owner=?1", [id])
        .map_err(storage::error)?;
    let key = observed_keys.last().unwrap_or(namespace);
    store.put(
        "tips",
        key,
        &Reference {
            session: id.into(),
            namespace: namespace.clone(),
            at: now(),
        },
        now(),
        id,
    )?;
    if session.history_shared {
        store.put("snapshots", id, &compact(session), now(), id)?;
    }
    store.commit()
}
pub(super) fn advice(
    service: &Service,
    namespace: &str,
    input: &[Value],
) -> std::io::Result<Option<semantic::Record>> {
    Ok(ReadView::new(service)?.advice(namespace, input))
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
    let store = Store::open(service, pinned(service), true)?;
    store.begin()?;
    let input = subject
        .history
        .iter()
        .map(|i| i.value.clone())
        .collect::<Vec<_>>();
    let keys = store.insert(namespace, &input)?;
    let key = keys.last().map(String::as_str).unwrap_or(namespace);
    let mut record = semantic::Record::capture(reviewed);
    record.policy = Some(semantic::scope(&service.config));
    if store
        .get::<semantic::Record>("reviews", key)?
        .is_none_or(|old| old.policy != record.policy || old.generation < record.generation)
    {
        store.put("reviews", key, &record, now(), "")?;
    }
    store.commit()
}
pub(super) fn observe_cache(
    service: &Service,
    body: &Value,
    usage: &Value,
    resolved: Option<&ResolvedModel>,
    credential: &str,
    time: crate::proxy_cache::ObservationTime,
) -> std::io::Result<Vec<CacheEvidence>> {
    let _guard = IO.lock().unwrap();
    let store = Store::open(service, pinned(service), true)?;
    store.begin()?;
    let mut cache = store.get("upstream_cache", "current")?.unwrap_or_default();
    if crate::proxy_cache::observe(&mut cache, body, usage, resolved, credential, time) {
        store.put("upstream_cache", "current", &cache, now(), "")?;
    }
    store.commit()?;
    Ok(cache)
}
pub(super) fn cache(service: &Service) -> std::io::Result<Vec<CacheEvidence>> {
    let _guard = IO.lock().unwrap();
    let store = read(service)?;
    let mut cache = store.get("upstream_cache", "current")?.unwrap_or_default();
    crate::proxy_cache::trim(&mut cache, now());
    Ok(cache)
}
