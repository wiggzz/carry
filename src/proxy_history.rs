//! Server-owned exact-prefix index. Immutable nodes share canonical wire values;
//! mutable sessions retain selection, workers, cache receipts and accounting.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

static IO: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Default, Serialize, Deserialize)]
struct Index {
    nodes: BTreeMap<String, Node>,
    tips: BTreeMap<String, Reference>,
    reviews: BTreeMap<String, Vec<forking::Point>>,
    #[serde(default)]
    transcripts: BTreeMap<String, Transcript>,
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
fn read(service: &Service) -> std::io::Result<Index> {
    match std::fs::read(service.config.state_dir.join("history.index")) {
        Ok(bytes) if bytes.len() <= service.config.fork_max_bytes as usize => {
            Ok(serde_json::from_slice(&bytes)?)
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
fn gc(index: &mut Index) {
    let mut reachable = HashSet::new();
    for root in index
        .tips
        .keys()
        .chain(index.reviews.keys())
        .chain(index.transcripts.values().map(|r| &r.frontier))
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
fn prune(service: &Service, index: &mut Index) {
    let at = now();
    index
        .tips
        .retain(|_, r| r.at <= at && at - r.at < service.config.fork_max_age_secs);
    index.reviews.retain(|_, points| {
        points.retain(|p| !forking::expired(&service.config, p, at));
        !points.is_empty()
    });
    index
        .transcripts
        .retain(|_, r| r.at <= at && at - r.at < service.config.fork_max_age_secs);
    loop {
        gc(index);
        if index.nodes.len() <= service.config.history_max_nodes as usize
            && index.tips.len() + index.reviews.len() + index.transcripts.len()
                <= service.config.history_max_references as usize
            && index.reviews.values().map(Vec::len).sum::<usize>()
                <= service.config.fork_max_nodes as usize
            && serde_json::to_vec(index).unwrap().len() <= service.config.fork_max_bytes as usize
        {
            break;
        }
        let review = index
            .reviews
            .iter()
            .flat_map(|(k, ps)| ps.iter().map(move |p| (p.at, k.clone(), p.id.clone())))
            .min();
        let tip = index.tips.iter().map(|(k, r)| (r.at, k.clone())).min();
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
            (Some((at, k, id)), Some((t, _))) if at <= t => {
                index.reviews.get_mut(&k).unwrap().retain(|p| p.id != id);
                index.reviews.retain(|_, ps| !ps.is_empty());
            }
            (_, Some((_, k))) => {
                index.tips.remove(&k);
            }
            (Some((_, k, id)), None) => {
                index.reviews.get_mut(&k).unwrap().retain(|p| p.id != id);
                index.reviews.retain(|_, ps| !ps.is_empty());
            }
            _ => break,
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
        &json!({"policy":"carry-transparent-history-v1", "owner":identity(&owner)?,
        "settings":forking::scope(&service.config, &cache_base(body)),
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
) -> Result<(String, Option<forking::Point>), Failure> {
    let _guard = IO.lock().unwrap();
    let mut index = read(service).map_err(|_| {
        failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "history index unavailable",
        )
    })?;
    prune(service, &mut index);
    let input = body["input"]
        .as_array()
        .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "array input required"))?;
    let keys = path(namespace, input);
    for key in keys.iter().rev() {
        if let Some(r) = index.tips.get(key) {
            return Ok((r.session.clone(), None));
        }
        if let Some(points) = index.reviews.get(key) {
            for point in points.iter().rev() {
                if forking::compatible_child(&service.config, point, body).is_ok() {
                    return Ok((
                        hash(&(namespace, "reviewed-child", &point.id, keys.last())),
                        Some(point.clone()),
                    ));
                }
            }
        }
    }
    Ok((hash(&(namespace, keys.last())), None))
}

/// Called only AFTER atomic checkpoint publication. A crash can lose an index
/// optimization, never create authority from uncommitted or temporary advice.
pub(super) fn committed(service: &Service, id: &str, session: &Session) -> std::io::Result<()> {
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
    let keys = insert(&mut index, namespace, &input);
    if let Some(response) = &session.last_response_id
        && let Some(frontier) = session.response_frontier
    {
        let transcript = session
            .history
            .iter()
            .take_while(|i| i.id <= frontier)
            .map(|i| i.value.clone())
            .chain(session.pending_output.iter().cloned())
            .collect::<Vec<_>>();
        let full_keys = insert(&mut index, namespace, &transcript);
        if let Some(key) = full_keys.last() {
            for (kind, external) in [
                ("response", Some(response)),
                ("conversation", session.history_conversation.as_ref()),
            ] {
                if let Some(external) = external {
                    let reference_key = hash(&(namespace, kind, external));
                    let reference = Transcript {
                        namespace: namespace.clone(),
                        external_id: external.clone(),
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
    }
    index.tips.retain(|_, r| r.session != id);
    if let Some(key) = keys.last() {
        index.tips.insert(
            key.clone(),
            Reference {
                session: id.into(),
                namespace: namespace.clone(),
                at: now(),
            },
        );
    }
    for point in &session.fork_points {
        if !forking::matches_sources(point, &input) {
            continue;
        }
        if let Some(key) = keys.get(point.sources.len().saturating_sub(1)) {
            let points = index.reviews.entry(key.clone()).or_default();
            if !points.iter().any(|p| p.id == point.id) {
                points.push(point.clone());
            }
        }
    }
    prune(service, &mut index);
    let temporary = service.config.state_dir.join(".history-index.tmp");
    let mut file = private_file(&temporary, false)?;
    serde_json::to_writer(&mut file, &index)?;
    file.sync_all()?;
    std::fs::rename(temporary, service.config.state_dir.join("history.index"))?;
    File::open(&service.config.state_dir)?.sync_all()
}
