//! Server-owned, disk-backed exact-prefix authority. Reference bounds limit live
//! work, not retention. Publication is one SQLite transaction; no capacity GC.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[path = "proxy_store.rs"]
mod storage;
pub(crate) use storage::Policy as FilePolicy;
use storage::Store;

/// One serialized physical-write owner, shared by numeric archives, checkpoint
/// staging and SQL publication. Holding the connected view preserves sidecars
/// in admission measurements; it does not hold a SQLite read transaction.
pub(crate) struct Publication {
    store: Store,
    _guard: std::sync::MutexGuard<'static, ()>,
}
impl FilePolicy {
    pub(crate) fn acquire(&self) -> std::io::Result<Publication> {
        let guard = IO.lock().unwrap();
        let pins = PINS
            .lock()
            .unwrap()
            .keys()
            .filter(|(directory, _)| directory.as_path() == self.directory())
            .map(|(_, id)| id.clone())
            .collect();
        Ok(Publication {
            _guard: guard,
            store: Store::open_policy(self, pins, false)?,
        })
    }
}
impl Publication {
    pub(super) fn admit_checkpoint(
        &self,
        directory: &Path,
        bytes: u64,
        reusable: u64,
    ) -> std::io::Result<()> {
        self.store.admit_checkpoint(directory, bytes, reusable)
    }
    pub(crate) fn admit_file(&self, directory: &Path, additional: u64) -> std::io::Result<()> {
        self.store.admit_file(directory, additional)
    }
    pub(super) fn settlement(&self, id: &str) -> std::io::Result<()> {
        self.store.begin_settlement(id)
    }
    pub(super) fn committed(
        &self,
        service: &Service,
        id: &str,
        session: &Session,
        observation: Option<(&[Value], &Value)>,
    ) -> std::io::Result<()> {
        committed_locked(service, id, session, observation)
    }
    pub(super) fn settled(
        &self,
        service: &Service,
        id: &str,
        session: &Session,
    ) -> std::io::Result<()> {
        settled_locked(service, id, session)
    }
}
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
    // Serialize admission with durable writes, always IO before PINS. Otherwise
    // a new promise could race a transaction's already-frozen capacity budget.
    let _guard = IO.lock().unwrap();
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
    if !pins.contains_key(&key) {
        let owners = pins.keys().filter(|(dir, _)| dir == &key.0).count() + 1;
        // A normal pin is read-only: do not expire the authority the caller is
        // about to protect. Under pressure, maintenance spends its independent
        // promise before retrying admission, not the new owner's headroom.
        if storage::admit_owner(service, owners).is_err() {
            storage::maintain(
                service,
                pins.keys()
                    .filter(|(dir, _)| dir == &key.0)
                    .map(|(_, id)| id.clone())
                    .collect(),
            )
            .map_err(read_failure)?;
            storage::admit_owner(service, owners).map_err(|_| {
                failure(
                    StatusCode::INSUFFICIENT_STORAGE,
                    "durable history disk admission refused",
                )
            })?;
        }
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
    while store.contains("owners", &id).map_err(read_failure)?
        || store.contains("snapshots", &id).map_err(read_failure)?
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
    // An empty request has no content cut. Bind its dispatch to the observed
    // response, never the namespace root shared by unrelated histories.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    empty_dispatch: Option<Continuation>,
}
/// Branch-qualified request evidence, never semantic removal permission or paid
/// ownership. Advice and actual dispatch have independent completion clocks;
/// copying advice to every dispatch cut would hide a later ancestor revocation.
#[derive(Clone, Serialize, Deserialize)]
struct Continuation {
    session: String,
    at: u64,
    submitted: Vec<u64>,
    input: crate::proxy_quantity::InputMeasurements,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    comparison_context: Option<crate::proxy_savings::ComparisonContext>,
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
pub(super) fn initialize(service: &Service) -> std::io::Result<()> {
    let _guard = IO.lock().unwrap();
    storage::initialize(service, pinned(service))
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
    initialize(service).map_err(read_failure)?;
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
        .map(|id| pin_owner(service, &id, &mut PINS.lock().unwrap()).map(|pin| (id, pin)))
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
        let pin = pin_owner(service, &id, &mut PINS.lock().unwrap())?;
        return Ok((id, pin));
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
    pub(super) fn expired_accounting_totals(
        &self,
    ) -> std::io::Result<(crate::proxy_savings::DashboardTotals, DashboardUsage)> {
        self.store.lock().unwrap().expired_accounting_totals()
    }
    pub(super) fn expired_accounting_has_owner(&self, id: &str) -> std::io::Result<bool> {
        self.store.lock().unwrap().expired_accounting_has_owner(id)
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
    pub(super) fn current_accounting(&self, id: &str) -> std::io::Result<Option<Session>> {
        let store = self.store.lock().unwrap();
        // Resolve expired shared authority from small owner/age metadata before
        // scanning its raw checkpoint, even with source fields ignored. Frozen
        // numeric accounting already owns this expired population; decoding all
        // raw snapshots here makes a bounded dashboard scale with raw bytes.
        if !store.live("snapshots", id)?
            && store
                .get::<Value>("owners", id)?
                .is_some_and(|owner| owner["shared"] == true)
        {
            return Err(std::io::Error::other("selected history unavailable"));
        }
        let Some(view) = store.get::<crate::proxy_state::AccountingView>("snapshots", id)? else {
            if store
                .get::<Value>("owners", id)?
                .is_some_and(|o| o["shared"] == true)
            {
                return Err(std::io::Error::other("selected history unavailable"));
            }
            return Ok(None);
        };
        let session = view.into_session();
        if !session.history_shared {
            return Ok(None);
        }
        if !matches!(session.version, 0 | 3) || !store.live("snapshots", id)? {
            return Err(std::io::Error::other("selected history unavailable"));
        }
        // Numeric coverage is checkpoint authority, not an ancestry query. A
        // smaller raw decode bound must not hide already settled native money.
        Ok(Some(session))
    }
    pub(super) fn shared(&self, id: &str) -> std::io::Result<Option<Session>> {
        {
            let store = self.store.lock().unwrap();
            let Some(session) = store.get::<Session>("snapshots", id)? else {
                if store
                    .get::<Value>("owners", id)?
                    .is_some_and(|o| o["shared"] == true)
                {
                    return Err(std::io::Error::other("selected history unavailable"));
                }
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
        prefix_record::<semantic::Record>(&store, "reviews", namespace, input)
            .ok()
            .flatten()
            .filter(|r| r.policy.as_deref() == Some(self.policy.as_str()))
    }
    fn continuation(
        &self,
        namespace: &str,
        input: &[Value],
        original: &Value,
    ) -> std::io::Result<Option<Continuation>> {
        let store = self.store.lock().unwrap();
        if let Some(cut) = prefix_record(&store, "dispatches", namespace, input)? {
            return Ok(Some(cut));
        }
        let external = original["previous_response_id"]
            .as_str()
            .map(|id| ("response", id))
            .or_else(|| conversation(original).map(|id| ("conversation", id)));
        let Some((kind, external)) = external else {
            return Ok(None);
        };
        let key = hash(&(namespace, kind, external));
        if !store.live("transcripts", &key)? {
            return Ok(None);
        }
        let Some(reference) = store.get::<Transcript>("transcripts", &key)? else {
            return Ok(None);
        };
        let ancestor = store.ancestry(namespace, &reference.frontier)?;
        if !input.starts_with(&ancestor) {
            return Err(std::io::Error::other("invalid observed dispatch ancestry"));
        }
        Ok(reference.empty_dispatch)
    }
}
fn prefix_record<T: serde::de::DeserializeOwned>(
    store: &Store,
    kind: &str,
    namespace: &str,
    input: &[Value],
) -> std::io::Result<Option<T>> {
    // Presence of the newest applicable cut is a revocation barrier. Only
    // absence/expiry permits looking at an older cut; unreadable authority
    // (including a lowered decode bound) never revives older permission.
    for key in path(namespace, input).iter().rev() {
        if store.live(kind, key)? {
            return store.get(kind, key);
        }
    }
    Ok(None)
}
/// Hydrate both independent facts from one frozen branch view. The planner must
/// reauthorize the prior dispatch against current advice/cohort/protection, and
/// InputMeasurements still checks its own model/route/settings/opaque contract.
pub(super) fn hydrate(
    service: &Service,
    session: &mut Session,
    body: &Value,
    original: &Value,
) -> Result<(), Failure> {
    session.opinions.clear();
    session.completed_review_len = 0;
    session.drop_members.clear();
    session.memories.clear();
    session.memory_anchors.clear();
    let Some(namespace) = session.history_namespace.as_deref() else {
        return Ok(());
    };
    // Only review-mode array histories have a namespace. Off-mode forwarding
    // still accepts non-array provider input without consulting shared state.
    let input = body["input"].as_array().unwrap();
    let view = ReadView::new(service).map_err(read_failure)?;
    let continuation = view
        .continuation(namespace, input, original)
        .map_err(read_failure)?;
    let advice = view.advice(namespace, input);
    if let Some(continuation) = continuation {
        if let Some(evidence) = &mut session.request_evidence {
            evidence
                .inherit_comparison_context(&continuation.session, continuation.comparison_context);
        }
        session.last_submitted_ids = continuation.submitted;
        session.primary_input = continuation.input;
    }
    if let Some(advice) = advice {
        advice.apply(session);
    }
    Ok(())
}
/// Publish captured paid facts against existing authority without replaying its
/// ancestry, transcript bindings or advice. The caller constructs this Session
/// by merging only numeric settlement into the durable predecessor.
#[cfg(test)]
pub(super) fn settled(service: &Service, id: &str, session: &Session) -> std::io::Result<()> {
    let _guard = IO.lock().unwrap();
    settled_locked(service, id, session)
}
fn settled_locked(service: &Service, id: &str, session: &Session) -> std::io::Result<()> {
    let store = Store::open(service, pinned(service), true)?;
    store.begin_settlement(id)?;
    if session.history_shared && !store.contains("snapshots", id)? {
        return Err(std::io::Error::other("paid settlement owner unavailable"));
    }
    store.account(id, session)?;
    if session.history_shared {
        store.put("snapshots", id, &compact(session), now(), id)?;
    }
    store.commit()
}

#[cfg(test)]
pub(super) fn committed(
    service: &Service,
    id: &str,
    session: &Session,
    observation: Option<(&[Value], &Value)>,
) -> std::io::Result<()> {
    let _guard = IO.lock().unwrap();
    committed_locked(service, id, session, observation)
}
fn committed_locked(
    service: &Service,
    id: &str,
    session: &Session,
    observation: Option<(&[Value], &Value)>,
) -> std::io::Result<()> {
    // Every admitted pending checkpoint, including a fresh explicit Off owner,
    // needs durable accounting authority before transport can spend money. Do
    // not defer schema creation until settlement, when ordinary headroom may
    // already be exhausted while the paid promise itself remains protected.
    let store = Store::open(service, pinned(service), true)?;
    store.begin()?;
    store.put(
        "owners",
        id,
        &json!({"shared":session.history_shared}),
        now(),
        id,
    )?;
    store.account(id, session)?;
    let Some(namespace) = &session.history_namespace else {
        return store.commit();
    };
    let input = session
        .history
        .iter()
        .map(|i| i.value.clone())
        .collect::<Vec<_>>();
    let keys = store.insert(namespace, &input)?;
    let canonical_key = keys.last().unwrap_or(namespace).clone();
    let mut continuation = observation.map(|_| Continuation {
        session: id.into(),
        at: now(),
        submitted: session.last_submitted_ids.clone(),
        input: session.primary_input.clone(),
        comparison_context: session
            .request_evidence
            .as_ref()
            .map(|e| e.comparison_context()),
    });
    let empty_dispatch = if keys.is_empty() {
        continuation.take()
    } else {
        None
    };
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
                            empty_dispatch: empty_dispatch.clone(),
                        },
                        now(),
                        id,
                    )?;
                }
            }
        }
    }
    store.replace_tip(id);
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
    if let Some(continuation) = continuation {
        store.put("dispatches", &canonical_key, &continuation, now(), id)?;
    }
    store.commit()
}
#[cfg(test)]
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
pub(super) fn accounting(
    service: &Service,
    after_at: u64,
    after_key: &str,
    limit: usize,
) -> std::io::Result<Value> {
    let _guard = IO.lock().unwrap();
    read(service)?.accounting_page(after_at, after_key, limit)
}
pub(super) fn accounting_window(service: &Service, start: u64, end: u64) -> std::io::Result<Value> {
    let _guard = IO.lock().unwrap();
    read(service)?.accounting_window(start, end)
}
pub(super) fn cache(service: &Service) -> std::io::Result<Vec<CacheEvidence>> {
    let _guard = IO.lock().unwrap();
    let store = read(service)?;
    let mut cache = store.get("upstream_cache", "current")?.unwrap_or_default();
    crate::proxy_cache::trim(&mut cache, now());
    Ok(cache)
}
