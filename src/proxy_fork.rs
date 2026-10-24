//! Same-owner committed review versions for transparent history reuse. This is
//! not native client ancestry discovery. Evidence holds hashes/facts, not raw replay,
//! usage, cache receipts, transport frontiers or in-flight reviewer state.
use super::*;
use crate::proxy_state::{Item, Memory};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Source {
    id: u64,
    cohort: u64,
    exposed: bool,
    #[serde(default)]
    removed: bool,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Point {
    pub id: String,
    pub(super) at: u64,
    scope: String,
    pub(super) sources: Vec<Source>,
    groups: Value,
    opinions: BTreeMap<u64, String>,
    drop_members: BTreeMap<u64, Vec<u64>>,
    memories: Vec<Memory>,
    memory_anchors: BTreeMap<u64, u64>,
}

fn digest(value: &impl Serialize) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}

pub(super) fn scope(config: &ProxyCli, context: &Value) -> String {
    let mut context = context.clone();
    if let Some(object) = context.as_object_mut() {
        // Affinity is branch-local, and is never evidence of review authority.
        object.remove("prompt_cache_key");
    }
    digest(
        &json!({"policy":"carry-transparent-review-v2", "context":context,
        "upstream":config.upstream_url, "subscription":config.codex_login,
        "reviewer":config.classifier_model, "reviewer_url":config.classifier_url,
        "reasoning":config.classifier_reasoning_effort,
        "reviewer_output":config.classifier_max_output_tokens}),
    )
}

fn groups(session: &Session) -> Value {
    json!(
        session
            .groups()
            .iter()
            .map(|g| json!({"id":g.id,"members":g.members,
        "pinned":g.pinned,"replayable":g.replayable}))
            .collect::<Vec<_>>()
    )
}

pub(super) fn expired(config: &ProxyCli, point: &Point, at: u64) -> bool {
    point.at > at || at.saturating_sub(point.at) >= config.fork_max_age_secs
}

/// Bound the actual serialized evidence vector, not approximate text sizes.
/// Diagnostics retain only 32 expired handles; older ones become unknown, not
/// fresh history. This is per lineage, not a global state-directory quota.
pub(super) fn prune(config: &ProxyCli, session: &mut Session) {
    let at = now();
    let mut retained = Vec::new();
    for point in std::mem::take(&mut session.fork_points) {
        if expired(config, &point, at) {
            session.fork_expired.push(point.id);
        } else {
            retained.push(point);
        }
    }
    while retained.len() > config.fork_max_nodes as usize
        || serde_json::to_vec(&retained).unwrap().len() > config.fork_max_bytes as usize
    {
        session.fork_expired.push(retained.remove(0).id);
    }
    session.fork_points = retained;
    let discard = session.fork_expired.len().saturating_sub(32);
    session.fork_expired.drain(..discard);
}

/// Publish committed review versions at their entire canonical observation
/// frontier, including prior removed members and dependent accepted facts.
/// Claimed source IDs never shorten this dependency closure.
pub(super) fn publish(config: &ProxyCli, current: &mut Session, before: &Session, valid: bool) {
    if !valid
        || current.review_feedback["rejected_count"] != 0
        || current.history.is_empty()
        || current.history.len() != before.history.len()
        || current
            .history
            .iter()
            .zip(&before.history)
            .any(|(a, b)| a.value != b.value || a.removed != b.removed)
        || current.review_context != before.review_context
    {
        return;
    }
    let point = selection(config, current);
    if current.fork_points.iter().any(|p| p.id == point.id)
        || current.fork_expired.contains(&point.id)
    {
        return;
    }
    current.fork_points.push(point);
    prune(config, current);
}

/// The committed branch selection, not a reviewer receipt. Capture only
/// transferable state, at its entire canonical dependency frontier. Both live
/// continuation and historical hydration resolve this same representation.
pub(super) fn selection(config: &ProxyCli, current: &Session) -> Point {
    let mut opinions = current.opinions.clone();
    // Preserve historical pins even if adding C makes the former latest user
    // no longer automatically pinned. Current child pins still apply as well.
    for group in current.groups().into_iter().filter(|g| g.pinned) {
        opinions.insert(group.id, "keep".into());
    }
    let mut point = Point {
        id: String::new(),
        at: now(),
        scope: scope(config, &current.review_context),
        sources: current
            .history
            .iter()
            .map(|i| Source {
                id: i.id,
                cohort: i.cohort,
                exposed: i.exposed,
                removed: i.removed,
                sha256: digest(&i.value),
            })
            .collect(),
        groups: groups(current),
        opinions,
        drop_members: current.drop_members.clone(),
        memories: current.memories.clone(),
        memory_anchors: current.memory_anchors.clone(),
    };
    // Time is retention metadata, not selection identity. Physical removals,
    // exposure, cohort membership and accepted facts all participate in identity.
    point.id = format!(
        "selection-v1:{}:{}",
        current.review_epoch,
        digest(&Point {
            at: 0,
            ..point.clone()
        })
    );
    point
}

pub(super) fn conflict(code: &str) -> Failure {
    (
        StatusCode::CONFLICT,
        axum::Json(json!({"error":{"code":code,
        "message":"historical review version unavailable"}})),
    )
}

/// Validate caller-owned full input using exactly the hydration prefix,
/// settings and complete-cohort rules. This transient projection contains no
/// inherited advice, memories, receipts or worker state and is never saved.
pub(super) fn compatible_child(
    config: &ProxyCli,
    point: &Point,
    body: &Value,
) -> Result<Session, Failure> {
    let input = body["input"]
        .as_array()
        .ok_or_else(|| conflict("fork_unsupported"))?;
    if scope(config, &cache_base(body)) != point.scope
        || input.len() < point.sources.len()
        || input
            .iter()
            .zip(&point.sources)
            .any(|(v, s)| digest(v) != s.sha256)
    {
        return Err(conflict("fork_incompatible"));
    }
    let mut child = Session {
        version: 1,
        history: input
            .iter()
            .zip(&point.sources)
            .map(|(v, s)| Item {
                id: s.id,
                cohort: s.cohort,
                exposed: s.exposed,
                removed: s.removed,
                value: v.clone(),
            })
            .collect(),
        next_id: point.sources.iter().map(|s| s.id).max().unwrap_or(0),
        ..Session::default()
    };
    if groups(&child) != point.groups {
        return Err(conflict("fork_incompatible"));
    }
    child
        .ingest(input)
        .map_err(|_| conflict("fork_incompatible"))?;
    // Validate each authorization in the selected topology. Restore only that
    // cohort's members if already removed: reviving unrelated tombstones (such
    // as a removed user separator) would undo legitimate post-compaction merges.
    // Ingested suffix members remain live, so extending an authorized tool
    // cohort still changes its dependency closure and rejects the selection.
    for (id, members) in &point.drop_members {
        let mut topology = child.clone();
        for item in &mut topology.history {
            if members.contains(&item.id) {
                item.removed = false;
            }
        }
        if !topology
            .groups()
            .iter()
            .any(|g| g.id == *id && g.members == *members)
        {
            return Err(conflict("fork_incompatible"));
        }
    }
    Ok(child)
}

pub(super) fn hydrate(
    config: &ProxyCli,
    session: &mut Session,
    origin: &str,
    point: &Point,
    body: &Value,
) -> Result<bool, Failure> {
    if expired(config, point, now()) {
        return Err(conflict("fork_expired"));
    }
    if session
        .fork_origin
        .as_ref()
        .is_some_and(|existing| existing != origin)
        || (session.fork_origin.is_none() && (session.version != 0 || !session.history.is_empty()))
    {
        return Err(conflict("fork_child_conflict"));
    }
    let input = body["input"]
        .as_array()
        .ok_or_else(|| conflict("fork_unsupported"))?;
    let mut child = compatible_child(config, point, body)?;
    if session.fork_origin.is_some() {
        // Revalidate both the declared cut and the live lineage before ordinary
        // ingest can reset it. A retry never restores parent advice/memories or
        // tombstones over legitimate branch-local changes.
        let mut retry = session.clone();
        retry
            .ingest(input)
            .map_err(|_| conflict("fork_incompatible"))?;
        if retry.history_rebases != session.history_rebases {
            return Err(conflict("fork_incompatible"));
        }
        return Ok(false);
    }
    child.opinions = point.opinions.clone();
    child.drop_members = point.drop_members.clone();
    child.memories = point.memories.clone();
    child.memory_anchors = point.memory_anchors.clone();
    child.next_memory_id = child
        .memories
        .iter()
        .map(|m| m.id.saturating_add(1))
        .max()
        .unwrap_or(0);
    child.review_context = cache_base(body);
    child.refresh_memory_shadow();
    // Accepted historical facts are not a new reviewer admission. The normal
    // planner owns genuine primary capacity; reviewer pressure cannot gate a
    // fitting inherited primary or trigger paid fork recovery.
    child.fork_origin = Some(origin.into());
    *session = child;
    Ok(true)
}
