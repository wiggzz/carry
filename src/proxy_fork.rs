//! Explicit, same-owner historical full-history forks. This is not native client
//! ancestry discovery. Evidence contains hashes/accepted facts, never raw replay,
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
    sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Point {
    pub id: String,
    at: u64,
    scope: String,
    sources: Vec<Source>,
    groups: Value,
    opinions: BTreeMap<u64, String>,
    drop_members: BTreeMap<u64, Vec<u64>>,
    memories: Vec<Memory>,
    memory_anchors: BTreeMap<u64, u64>,
}

fn digest(value: &impl Serialize) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}

fn scope(config: &ProxyCli, context: &Value) -> String {
    let mut context = context.clone();
    if let Some(object) = context.as_object_mut() {
        // Affinity is branch-local, and is never evidence of review authority.
        object.remove("prompt_cache_key");
    }
    digest(
        &json!({"policy":"carry-explicit-fork-v1", "context":context,
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

/// Publish only a first, fully committed review of a raw ancestor. Its entire
/// observation/dependency frontier is the cut; claimed memory source_ids alone
/// never shorten that frontier. Legacy opinions and later-context memory chains
/// are deliberately not promoted to portable evidence.
pub(super) fn publish(config: &ProxyCli, current: &mut Session, before: &Session, valid: bool) {
    if !valid
        || current.review_feedback["rejected_count"] != 0
        || !before.opinions.is_empty()
        || !before.memories.is_empty()
        || current.history.is_empty()
        || current.history.len() != before.history.len()
        || current
            .history
            .iter()
            .zip(&before.history)
            .any(|(a, b)| a.value != b.value || a.removed || b.removed)
        || current.review_context != before.review_context
    {
        return;
    }
    let values = current.history.iter().map(|i| &i.value).collect::<Vec<_>>();
    let id = format!("{}:{}", current.review_epoch, digest(&values));
    if current.fork_points.iter().any(|p| p.id == id) {
        return; // immutable: a later review cannot replace the historical cut
    }
    let mut opinions = current.opinions.clone();
    // Preserve historical pins even if adding C makes the former latest user
    // no longer automatically pinned. Current child pins still apply as well.
    for group in current.groups().into_iter().filter(|g| g.pinned) {
        opinions.insert(group.id, "keep".into());
    }
    let point = Point {
        id,
        at: now(),
        scope: scope(config, &current.review_context),
        sources: current
            .history
            .iter()
            .map(|i| Source {
                id: i.id,
                cohort: i.cohort,
                exposed: i.exposed,
                sha256: digest(&i.value),
            })
            .collect(),
        groups: groups(current),
        opinions,
        drop_members: current.drop_members.clone(),
        memories: current.memories.clone(),
        memory_anchors: current.memory_anchors.clone(),
    };
    current.fork_points.push(point);
}

fn conflict(code: &str) -> Failure {
    (
        StatusCode::CONFLICT,
        axum::Json(json!({"error":{"code":code,
        "message":"explicit historical fork unavailable; no fresh-history fallback"}})),
    )
}

/// Resolve the immutable parent under its lock, release it, then acquire the
/// child lock at the call site. Never nest parent/child/worker locks or rewrite
/// parent state. The parent checkpoint's atomic publication owns the evidence.
pub(super) async fn resolve(
    service: &Service,
    headers: &HeaderMap,
    body: &Value,
    child_id: &str,
    implicit: bool,
    native: bool,
) -> Result<Option<(String, Point)>, Failure> {
    let parent = headers.get("x-carry-fork-parent");
    let point = headers.get("x-carry-fork-point");
    if parent.is_none() && point.is_none() {
        return Ok(None);
    }
    if implicit
        || native
        || service.config.mode == Mode::Off
        || body
            .get("previous_response_id")
            .is_some_and(|v| !v.is_null())
        || !body["input"].is_array()
    {
        return Err(conflict("fork_unsupported"));
    }
    let (Some(parent), Some(point)) = (parent, point) else {
        return Err(conflict("fork_invalid_intent"));
    };
    let handle = point
        .to_str()
        .map_err(|_| conflict("fork_invalid_intent"))?;
    let valid = handle.split_once(':').is_some_and(|(epoch, hash)| {
        epoch.parse::<u64>().is_ok_and(|v| v.to_string() == epoch)
            && hash.len() == 64
            && hash
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    });
    if !valid {
        return Err(conflict("fork_invalid_intent"));
    }
    let mut parent_headers = headers.clone();
    parent_headers.insert("x-carry-branch", parent.clone());
    let parent_id = identity(&parent_headers)?.ok_or_else(|| conflict("fork_invalid_intent"))?;
    if parent_id == child_id {
        return Err(conflict("fork_requires_distinct_branch"));
    }
    let _lock = session_lock(service, &parent_id).await;
    let parent = load(service, &parent_id)?;
    let point = parent
        .fork_points
        .iter()
        .find(|p| p.id == handle)
        .ok_or_else(|| conflict("fork_unknown"))?
        .clone();
    Ok(Some((format!("{parent_id}/{handle}"), point)))
}

pub(super) fn hydrate(
    config: &ProxyCli,
    session: &mut Session,
    origin: &str,
    point: &Point,
    body: &Value,
) -> Result<(), Failure> {
    if let Some(existing) = &session.fork_origin {
        return if existing == origin {
            Ok(())
        } else {
            Err(conflict("fork_child_conflict"))
        };
    }
    if session.version != 0 || !session.history.is_empty() {
        return Err(conflict("fork_child_conflict"));
    }
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
                removed: false,
                value: v.clone(),
            })
            .collect(),
        next_id: point.sources.iter().map(|s| s.id).max().unwrap_or(0),
        ..Session::default()
    };
    if groups(&child) != point.groups {
        return Err(conflict("fork_incompatible"));
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
    child
        .ingest(input)
        .map_err(|_| conflict("fork_incompatible"))?;
    // A suffix must not grow a supposedly complete authorized cohort.
    if point.drop_members.iter().any(|(id, members)| {
        !child
            .groups()
            .iter()
            .any(|g| g.id == *id && g.members == *members)
    }) {
        return Err(conflict("fork_incompatible"));
    }
    child.review_context = cache_base(body);
    child.refresh_memory_shadow();
    if !memory_admission_fits(config, &child) {
        return Err(conflict("fork_incompatible"));
    }
    child.fork_origin = Some(origin.into());
    *session = child;
    Ok(())
}
