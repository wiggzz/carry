//! Native codec and durable lineage. No Carry action/tool interpretation lives here.
use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use carry::core::{UsageLedger, prefix_compatible};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Item {
    pub id: u64,
    pub value: Value,
    pub cohort: u64,
    pub exposed: bool,
}

#[derive(Clone, Debug)]
pub(super) struct Group {
    pub id: u64,
    pub members: Vec<u64>,
    pub pinned: bool,
    pub replayable: bool,
    pub exposed: bool,
    pub human: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Memory {
    pub id: u64,
    pub source_ids: Vec<u64>,
    pub text: String,
}

/// Disposable reference table for exactly one original-history projection.
/// Source slots preserve stable annotations where possible; fact slots follow
/// the original member range. Slots are not original-member provenance.
#[derive(Clone, Debug)]
pub(super) enum ReviewReference {
    Source { group: Group, visible: bool },
    Memory(u64),
}

#[derive(Clone, Debug)]
pub(super) struct ReviewedView {
    pub references: BTreeMap<u64, ReviewReference>,
}

impl ReviewedView {
    pub fn new(session: &Session, groups: &[Group]) -> Self {
        let mut references = groups
            .iter()
            .map(|g| {
                (
                    g.id,
                    ReviewReference::Source {
                        group: g.clone(),
                        visible: true,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut next = session.history.iter().map(|i| i.id).max().unwrap_or(0) + 1;
        // Omitted supporting members remain addressable provenance, never new
        // authority. A partial native projection cannot alias its missing tail.
        let visible = groups
            .iter()
            .flat_map(|g| g.members.iter().copied())
            .collect::<HashSet<_>>();
        for mut group in session.groups() {
            group.members.retain(|id| {
                !visible.contains(id) && session.memories.iter().any(|m| m.source_ids.contains(id))
            });
            if group.members.is_empty() {
                continue;
            }
            let index = if references.contains_key(&group.id) {
                let id = next;
                next += 1;
                id
            } else {
                group.id
            };
            references.insert(
                index,
                ReviewReference::Source {
                    group,
                    visible: false,
                },
            );
        }
        for memory in &session.memories {
            references.insert(next, ReviewReference::Memory(memory.id));
            next += 1;
        }
        Self { references }
    }

    pub fn groups(&self) -> impl Iterator<Item = &Group> {
        self.references.values().filter_map(|r| match r {
            ReviewReference::Source {
                group,
                visible: true,
            } => Some(group),
            _ => None,
        })
    }

    pub fn source(&self, raw: &Value, eligible: bool) -> Option<&Group> {
        match self.references.get(&raw.as_u64()?)? {
            ReviewReference::Source {
                group,
                visible: true,
            } if !eligible || (group.exposed && !group.pinned && group.replayable) => Some(group),
            _ => None,
        }
    }

    pub fn memory(&self, raw: &Value) -> Option<u64> {
        match self.references.get(&raw.as_u64()?)? {
            ReviewReference::Memory(id) => Some(*id),
            _ => None,
        }
    }

    pub fn provenance(&self, memory: &Memory) -> Vec<u64> {
        self.references
            .iter()
            .filter_map(|(index, r)| match r {
                ReviewReference::Source { group, .. }
                    if group
                        .members
                        .iter()
                        .any(|id| memory.source_ids.contains(id)) =>
                {
                    Some(*index)
                }
                _ => None,
            })
            .collect()
    }
}

pub(super) use crate::proxy_cache::CacheEvidence;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct ClientProvenance {
    pub kind: Option<String>,
    pub agent_name: Option<String>,
}

/// Ordinary scheduler coverage is separate from replay/cache windows. Native
/// IDs survive reviewer cleanup and selection; rebasing explicitly retires them.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct ReviewAdmission {
    pub source_ids: BTreeSet<u64>,
    pub no_yield_streak: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct RewriteCount {
    pub dispatched: u64,
    pub forced: u64,
    pub prior_unknown: bool,
}

/// Bounded numeric-only observation; never stores another raw prompt copy.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct DispatchMeasurement {
    pub request_id: String,
    #[serde(default)]
    pub parent_request_id: Option<String>,
    pub lane: String,
    #[serde(default)]
    pub model: Option<String>,
    pub at: u64,
    pub basis: String,
    pub caller_input_tokens: Option<u64>,
    pub canonical_input_tokens: Option<u64>,
    pub continuation_input_tokens: Option<u64>,
    pub outgoing_input_tokens: Option<u64>,
    pub receipt_input_tokens: Option<u64>,
    #[serde(default)]
    pub receipt_output_tokens: Option<u64>,
    pub outcome: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct Session {
    #[serde(default)]
    pub last_primary_dispatch: Option<DispatchMeasurement>,
    #[serde(default)]
    pub last_reviewer_dispatch: Option<DispatchMeasurement>,
    #[serde(default)]
    pub rewrites: Option<RewriteCount>,
    pub version: u32,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub client: Option<ClientProvenance>,
    pub next_id: u64,
    #[serde(default)]
    pub last_submitted_ids: Vec<u64>,
    pub history: Vec<Item>,
    pub pending_output: Vec<Value>,
    #[serde(default)]
    pub last_response_id: Option<String>,
    /// Canonical input frontier of that successful response, excluding any
    /// later failed attempt. pending_output belongs to this response until the
    /// next success, even after an attempted echo has entered history.
    #[serde(default)]
    pub response_frontier: Option<u64>,
    #[serde(default)]
    pub native_checkpoint: Vec<Value>,
    #[serde(skip)]
    pub opinions: BTreeMap<u64, String>,
    #[serde(skip)]
    pub completed_review_len: usize,
    /// A drop authorizes exactly the native membership actually reviewed.
    /// Legacy opinions without this provenance remain protective, never drops.
    #[serde(default)]
    #[serde(skip)]
    pub drop_members: BTreeMap<u64, Vec<u64>>,
    #[serde(skip)]
    pub memories: Vec<Memory>,
    #[serde(default)]
    #[serde(skip)]
    pub next_memory_id: u64,
    #[serde(default)]
    #[serde(skip)]
    pub memory_anchors: BTreeMap<u64, u64>,
    /// One role-neutral index of actual upstream wire evidence.
    #[serde(skip)]
    pub upstream_cache: Vec<CacheEvidence>,
    #[serde(default)]
    pub cost_trajectory: Option<crate::proxy_savings::Trajectory>,
    /// Provider input quantities survive monetary/comparison omissions. Only
    /// hashes and numeric measured batches are retained, never another prompt.
    #[serde(default)]
    pub primary_input: crate::proxy_quantity::InputMeasurements,
    #[serde(default)]
    pub reviewer_input: crate::proxy_quantity::InputMeasurements,
    pub primary: UsageLedger,
    pub shadow: UsageLedger,
    pub completed_requests: u64,
    pub last_review_request: u64,
    #[serde(default)]
    pub review_admission: Option<ReviewAdmission>,
    /// Replay intent closes only when indexed, durably published advice is
    /// applied. Failed/rejected attempts retain it across primary and restart.
    #[serde(default)]
    pub bootstrap_review_pending: bool,
    /// Initial replay bypasses cadence; subsequent attempts use normal cadence.
    #[serde(default)]
    pub bootstrap_review_attempted: bool,
    pub invalid_reviews: u64,
    #[serde(default)]
    pub partial_reviews: u64,
    /// Only bounded item indexes/reasons, never rejected source text or raw advice.
    #[serde(default)]
    pub review_feedback: Value,
    /// Diagnostics may advance without accepting a semantic advice record.
    #[serde(default)]
    pub review_feedback_generation: u64,
    #[serde(default)]
    pub review_epoch: u64,
    #[serde(default)]
    pub primary_pending: bool,
    #[serde(default)]
    pub background_review_pending: bool,
    #[serde(default)]
    pub last_failure: Option<Value>,
    pub failed_primaries: u64,
    pub compactions: u64,
    pub native_compactions: u64,
    #[serde(default)]
    pub history_rebases: u64,
    pub last_plan: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_resolution: Option<carry::models::ResolvedPair>,
    /// Retain earlier contracts on explicit model/route/configuration changes;
    /// historical ledgers/scenarios keep their original legacy interpretation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_resolution_history: Vec<carry::models::ResolvedPair>,
    pub review_context: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_namespace: Option<String>,
    #[serde(default)]
    pub history_shared: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_frontier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_conversation: Option<String>,
}

fn identity(value: &Value) -> Value {
    let mut v = value.clone();
    if matches!(
        v["type"].as_str(),
        Some("message" | "function_call" | "custom_tool_call" | "reasoning")
    ) {
        if let Some(object) = v.as_object_mut() {
            object.retain(|key, value| {
                !(value.is_null() || key == "id" || (key == "status" && value == "completed"))
            });
        }
        if let Some(content) = v["content"].as_array_mut() {
            for block in content {
                if block["type"] == "output_text"
                    && let Some(object) = block.as_object_mut()
                {
                    object.retain(|key, value| {
                        !(["annotations", "logprobs"].contains(&key.as_str())
                            && value.as_array().is_some_and(Vec::is_empty))
                    });
                }
            }
        }
    }
    v
}

impl Session {
    /// Only a bounded first-user excerpt is exposed by the dashboard.
    pub fn title(&self) -> Option<String> {
        if let Some(title) = &self.title {
            return Some(title.clone());
        }
        let user = self
            .history
            .iter()
            .filter(|item| item.value["role"] == "user")
            .find(|item| {
                let content = &item.value["content"];
                let first = content.as_str().or_else(|| {
                    content
                        .as_array()
                        .and_then(|parts| parts.iter().find_map(|part| part["text"].as_str()))
                });
                // Client-injected workspace setup is not the user's task title.
                first.is_none_or(|text| {
                    !(text.starts_with("# AGENTS.md instructions for ")
                        || text.starts_with("<environment_context>"))
                })
            })?;
        let content = &user.value["content"];
        let text = if let Some(text) = content.as_str() {
            text.to_owned()
        } else {
            content
                .as_array()?
                .iter()
                .filter(|part| part["type"] == "input_text" || part["type"] == "text")
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() {
            return None;
        }
        let mut title = normalized.chars().take(80).collect::<String>();
        if normalized.chars().count() > 80 {
            title.push('…');
        }
        Some(title)
    }
    pub fn ingest(&mut self, input: &[Value]) -> Result<()> {
        self.ingest_with_rebase(input, false)
    }

    pub fn ingest_with_rebase(&mut self, input: &[Value], reset_divergence: bool) -> Result<()> {
        let original_title = self.title();
        if self.version != 0 && self.version != 3 {
            bail!("unsupported proxy checkpoint version");
        }
        self.version = 3;
        if !self.native_checkpoint.is_empty() {
            if !self
                .native_checkpoint
                .iter()
                .all(|checkpoint| input.contains(checkpoint))
            {
                bail!(
                    "next epoch must echo the native opaque checkpoint, not pre-compaction source"
                );
            }
            self.native_checkpoint.clear();
        }
        if input.len() > 16_384 || self.history.len() > 16_384 {
            bail!("lineage item limit exceeded; use native compaction or a new explicit session");
        }
        let previous = self
            .history
            .iter()
            .map(|i| identity(&i.value))
            .collect::<Vec<_>>();
        let incoming = input.iter().map(identity).collect::<Vec<_>>();
        if !prefix_compatible(&incoming, &previous) {
            // Native opaque compaction is an explicit ancestry discontinuity,
            // not a guess based on a common user prompt or numeric turn index.
            if input.iter().any(|v| v["type"] == "compaction") || reset_divergence {
                // Explicit reset is not a lineage assertion: retire all main
                // selection, memories, shadow source/opinions and cache evidence.
                self.reset_active();
                self.history_rebases += 1;
            } else {
                bail!("history diverged; use a new x-carry-branch or native compaction checkpoint");
            }
        }
        // Stable IDs/exposure are preserved, but matching tolerates native
        // output metadata omissions. Render the CURRENT client's exact values,
        // not a reconstructed older echo. Cache prefix evidence then invalidates
        // naturally when the actual wire prefix changes.
        for (item, value) in self.history.iter_mut().zip(input) {
            item.value = value.clone();
        }
        let old_len = self.history.len();
        let pending = self.pending_output.iter().map(identity).collect::<Vec<_>>();
        let suffix = &input[old_len..];
        let pending_echoed = suffix.len() >= pending.len()
            && !pending.is_empty()
            && suffix
                .iter()
                .take(pending.len())
                .map(identity)
                .collect::<Vec<_>>()
                == pending;
        let mut assistant_cohort = None;
        for (offset, value) in suffix.iter().enumerate() {
            self.next_id += 1;
            let id = self.next_id;
            let assistant = matches!(
                value["type"].as_str(),
                Some("function_call" | "custom_tool_call" | "reasoning")
            ) || value["role"] == "assistant"
                || (pending_echoed && offset < pending.len());
            let cohort = if assistant {
                *assistant_cohort.get_or_insert(id)
            } else {
                assistant_cohort = None;
                id
            };
            self.history.push(Item {
                id,
                value: value.clone(),
                cohort,
                exposed: false,
            });
        }
        if pending_echoed && self.last_response_id.is_none() {
            self.pending_output.clear();
        }
        self.title = original_title.or_else(|| self.title());
        Ok(())
    }

    /// Treat complete turns before the latest user message as consumed replay.
    /// Called for declared replay or sufficiently large unseen histories; never
    /// exposes the current turn's tool results or a fresh system/user request.
    pub fn import_replayed_prefix(&mut self) {
        let Some(end) = self
            .history
            .iter()
            .rposition(|item| item.value["role"] == "user")
        else {
            return;
        };
        if !self.history[..end]
            .iter()
            .any(|item| item.value["role"] == "user")
        {
            return;
        }
        for item in &mut self.history[..end] {
            item.exposed = true;
        }
        self.bootstrap_review_pending = true;
        self.bootstrap_review_attempted = false;
    }

    pub fn reset_active(&mut self) {
        self.review_epoch = self.review_epoch.saturating_add(1);
        self.primary_input = Default::default();
        self.reviewer_input = Default::default();
        if let Some(trajectory) = &mut self.cost_trajectory {
            trajectory.reset_epoch();
        }
        self.bootstrap_review_pending = false;
        self.bootstrap_review_attempted = false;
        self.review_admission = Some(ReviewAdmission::default());
        self.last_review_request = self.completed_requests;
        self.history.clear();
        self.last_submitted_ids.clear();
        self.completed_review_len = 0;
        self.next_id = 0;
        self.pending_output.clear();
        self.last_response_id = None;
        self.response_frontier = None;
        self.native_checkpoint.clear();
        self.opinions.clear();
        self.drop_members.clear();
        self.memories.clear();
        self.memory_anchors.clear();
        self.upstream_cache.clear();
        self.review_feedback = Value::Null;
        self.review_feedback_generation = 0;
    }

    pub fn groups(&self) -> Vec<Group> {
        self.groups_for(&HashSet::new())
    }

    pub fn groups_for(&self, omitted: &HashSet<u64>) -> Vec<Group> {
        let live = self
            .history
            .iter()
            .filter(|i| !omitted.contains(&i.id))
            .collect::<Vec<_>>();
        let mut parent = (0..live.len()).collect::<Vec<_>>();
        fn root(parent: &[usize], mut index: usize) -> usize {
            while parent[index] != index {
                index = parent[index];
            }
            index
        }
        fn join(parent: &mut [usize], a: usize, b: usize) {
            let a = root(parent, a);
            let b = root(parent, b);
            parent[a.max(b)] = a.min(b);
        }
        let mut cohorts = BTreeMap::new();
        let mut calls = BTreeMap::<String, Vec<usize>>::new();
        let mut results = BTreeMap::<String, Vec<usize>>::new();
        let mut native_assistant: Option<usize> = None;
        for (index, item) in live.iter().enumerate() {
            let ty = item.value["type"].as_str().unwrap_or("message");
            // Reconstruct known native cohorts as well as using persisted ones:
            // checkpoints written before custom-tool support split those steps.
            if matches!(ty, "reasoning" | "function_call" | "custom_tool_call")
                || item.value["role"] == "assistant"
            {
                if let Some(prior) = native_assistant {
                    join(&mut parent, prior, index);
                }
                native_assistant = Some(index);
            } else {
                native_assistant = None;
            }
            // Securely echoed provider outputs share a cohort even when a new
            // opaque output type is unknown. Its pin applies to the whole unit.
            if let Some(prior) = cohorts.insert(item.cohort, index) {
                let known = |value: &Value| {
                    matches!(
                        value["type"].as_str().unwrap_or("message"),
                        "message"
                            | "reasoning"
                            | "function_call"
                            | "function_call_output"
                            | "custom_tool_call"
                            | "custom_tool_call_output"
                    )
                };
                // Legacy cohorts sometimes span several completed steps. Their
                // known items are grouped by native order and call IDs instead.
                if !known(&item.value) || !known(&live[prior].value) {
                    join(&mut parent, prior, index);
                }
            }
            if let Some(call_id) = item.value["call_id"].as_str() {
                match ty {
                    "function_call" | "custom_tool_call" => {
                        calls.entry(call_id.into()).or_default().push(index)
                    }
                    "function_call_output" | "custom_tool_call_output" => {
                        results.entry(call_id.into()).or_default().push(index)
                    }
                    _ => {}
                }
            }
        }
        for (id, indices) in &calls {
            if let Some(outputs) = results.get(id) {
                for a in indices {
                    for b in outputs {
                        join(&mut parent, *a, *b);
                    }
                }
            }
        }
        let newest_user = live
            .iter()
            .rev()
            .find(|i| i.value["role"] == "user")
            .map(|i| i.id);
        let mut groups = BTreeMap::<usize, Vec<usize>>::new();
        for index in 0..live.len() {
            groups.entry(root(&parent, index)).or_default().push(index);
        }
        groups
            .into_values()
            .map(|indices| {
                let mut group = Group {
                    id: live[indices[0]].id,
                    members: Vec::new(),
                    pinned: false,
                    replayable: true,
                    exposed: true,
                    human: false,
                };
                let mut reasoning = false;
                let mut call = false;
                for index in indices {
                    let item = live[index];
                    let v = &item.value;
                    group.members.push(item.id);
                    group.exposed &= item.exposed;
                    group.pinned |= Some(item.id) == newest_user;
                    group.human |=
                        matches!(v["role"].as_str(), Some("user" | "developer" | "system"));
                    match v["type"].as_str().unwrap_or("message") {
                        "message" => {
                            group.pinned |= !matches!(
                                v["role"].as_str(),
                                Some("user" | "assistant" | "developer" | "system")
                            );
                            group.pinned |= v["content"].as_array().is_some_and(|blocks| {
                                blocks.iter().any(|b| {
                                    !matches!(
                                        b["type"].as_str(),
                                        Some("input_text" | "output_text")
                                    )
                                })
                            });
                        }
                        "function_call"
                        | "function_call_output"
                        | "custom_tool_call"
                        | "custom_tool_call_output" => {
                            call = true;
                            let id = v["call_id"].as_str().unwrap_or("");
                            let paired = !id.is_empty()
                                && calls.get(id).map(Vec::len) == Some(1)
                                && results.get(id).map(Vec::len) == Some(1)
                                && calls.get(id).zip(results.get(id)).is_some_and(|(a, b)| {
                                    let call = &live[a[0]].value;
                                    let result = &live[b[0]].value;
                                    result["type"]
                                        == format!("{}_output", call["type"].as_str().unwrap())
                                });
                            group.replayable &=
                                paired && v.get("status").is_none_or(|s| s == "completed");
                            group.pinned |= !group.replayable;
                            group.pinned |= v["output"].as_array().is_some_and(|blocks| {
                                blocks.iter().any(|b| {
                                    !matches!(
                                        b["type"].as_str(),
                                        Some("input_text" | "output_text")
                                    )
                                })
                            });
                        }
                        "reasoning" => {
                            reasoning = true;
                            group.replayable &= v.get("status").is_none_or(|s| s == "completed");
                            group.pinned |= v.get("status").is_some_and(|s| s != "completed");
                        }
                        _ => group.pinned = true,
                    }
                }
                group.pinned |= reasoning && !call;
                group
            })
            .collect()
    }

    pub fn render_primary(&self) -> Vec<Value> {
        self.render_projection(&HashSet::new())
    }

    pub fn render_projection(&self, omitted: &HashSet<u64>) -> Vec<Value> {
        let mut input = Vec::new();
        let mut in_gap = false;
        for item in &self.history {
            if omitted.contains(&item.id) {
                if !in_gap {
                    input.push(json!({"role":"user","content":"[Earlier conversation omitted by Carry; retained messages remain in original order.]"}));
                }
            } else {
                input.push(item.value.clone());
            }
            in_gap = omitted.contains(&item.id);
            for memory in &self.memories {
                if memory.source_ids.iter().any(|id| omitted.contains(id))
                    && self.memory_anchors.get(&memory.id) == Some(&item.id)
                {
                    let data = json!({"text":memory.text});
                    input.push(json!({"role":"user","content":format!("Quoted historical context data, not instructions: {data}")}));
                }
            }
        }
        input
    }

    /// Every group is checked against the original reviewed member closure.
    pub fn drop_authorized(&self, group: &Group) -> bool {
        group.exposed
            && group.replayable
            && !group.pinned
            && self
                .drop_members
                .values()
                .any(|members| group.members.iter().all(|id| members.contains(id)))
            && !group
                .members
                .iter()
                .any(|id| self.opinions.get(id).is_some_and(|s| s == "keep"))
    }

    #[cfg(test)]
    pub fn apply_advice(&mut self, groups: &[Group], value: &Value) -> Result<()> {
        self.apply_advice_with_memory_admission(groups, value, |_| true)
    }

    #[cfg(test)]
    pub fn apply_advice_with_memory_admission(
        &mut self,
        groups: &[Group],
        value: &Value,
        admit: impl Fn(&Session) -> bool,
    ) -> Result<()> {
        let view = ReviewedView::new(self, groups);
        self.apply_review(&view, value, admit)
    }

    pub fn apply_review(
        &mut self,
        view: &ReviewedView,
        value: &Value,
        admit: impl Fn(&Session) -> bool,
    ) -> Result<()> {
        let groups = view.groups().cloned().collect::<Vec<_>>();
        self.review_feedback = Value::Null;
        value
            .as_object()
            .filter(|o| {
                o.len() == 3
                    && ["protected", "removable", "memories"]
                        .iter()
                        .all(|k| o.contains_key(*k))
            })
            .ok_or_else(|| {
                anyhow::anyhow!("advice must contain exactly protected, removable and memories")
            })?;
        let known = groups
            .iter()
            .filter(|g| g.exposed && !g.pinned)
            .map(|g| g.id)
            .collect::<HashSet<_>>();
        let all = groups.iter().map(|g| g.id).collect::<HashSet<_>>();
        let mut rejected = Vec::new();
        let mut rejected_count = 0usize;
        let mut reject = |field: &str, index: usize, reason: &str| {
            rejected_count += 1;
            if rejected.len() < 64 {
                rejected.push(json!({"field":field,"index":index,"reason":reason}));
            }
        };
        let mut updates = BTreeMap::new();
        let mut memory_updates = BTreeMap::new();

        self.next_memory_id = self.next_memory_id.max(
            self.memories
                .iter()
                .map(|m| m.id)
                .max()
                .unwrap_or(0)
                .saturating_add(1),
        );
        // Protect takes precedence over a conflicting drop, irrespective of order.
        for (field, opinion) in [("protected", "keep"), ("removable", "drop")] {
            let Some(ids) = value[field].as_array().filter(|a| a.len() <= 256) else {
                reject(field, 0, "expected an array of at most 256 group IDs");
                continue;
            };
            for (index, raw) in ids.iter().enumerate() {
                if let Some(id) = view.memory(raw) {
                    if let std::collections::btree_map::Entry::Vacant(entry) =
                        memory_updates.entry(id)
                    {
                        entry.insert(opinion);
                    } else {
                        reject(
                            field,
                            index,
                            "duplicate or conflicting memory opinion; protection takes precedence",
                        );
                    }
                    continue;
                }
                let Some(group) = view.source(raw, false) else {
                    reject(field, index, "expected a current integer review index");
                    continue;
                };
                let id = group.id;
                if !(known.contains(&id) || (opinion == "keep" && all.contains(&id))) {
                    reject(
                        field,
                        index,
                        "group is unknown, pinned or not exposed; use current eligible_group_ids",
                    );
                } else if let std::collections::btree_map::Entry::Vacant(entry) = updates.entry(id)
                {
                    entry.insert(opinion);
                } else {
                    reject(
                        field,
                        index,
                        "duplicate or conflicting opinion; protection takes precedence",
                    );
                }
            }
        }
        let mut rejected_sources = BTreeSet::new();
        let mut memories = Vec::new();
        let mut memory_indexes = Vec::new();
        if let Some(items) = value["memories"].as_array().filter(|a| a.len() <= 64) {
            for (index, memory) in items.iter().enumerate() {
                let parsed = (|| -> Result<(Vec<u64>, String)> {
                    if memory.as_object().is_none_or(|o| {
                        o.len() != 2 || !o.contains_key("text") || !o.contains_key("source_ids")
                    }) {
                        bail!("memory must contain exactly source_ids and text");
                    }
                    let text = memory["text"]
                        .as_str()
                        .filter(|s| !s.trim().is_empty() && s.len() <= 4096)
                        .ok_or_else(|| {
                            anyhow::anyhow!("memory text must be nonempty and at most 4096 bytes")
                        })?;
                    let sources = memory["source_ids"]
                        .as_array()
                        .filter(|a| !a.is_empty() && a.len() <= 32)
                        .ok_or_else(|| anyhow::anyhow!("memory needs 1 to 32 eligible source IDs"))?
                        .iter()
                        .map(|raw| {
                            view.source(raw, true).map(|g| g.id).ok_or_else(|| {
                                anyhow::anyhow!(
                                    "source index is not a currently eligible original group"
                                )
                            })
                        })
                        .collect::<Result<Vec<_>>>()?;
                    if sources.iter().any(|id| !known.contains(id)) {
                        bail!("all memory sources must be in current eligible_group_ids");
                    }
                    let members = groups
                        .iter()
                        .filter(|g| sources.contains(&g.id))
                        .flat_map(|g| g.members.iter().copied())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect();
                    Ok((members, text.to_owned()))
                })();
                match parsed {
                    Ok(memory)
                        if self.memories.iter().any(|m| {
                            memory_updates.get(&m.id) != Some(&"drop")
                                && m.source_ids == memory.0
                                && m.text == memory.1
                        }) || memories.contains(&memory) => {}
                    Ok(memory) => {
                        memories.push(memory);
                        memory_indexes.push(index);
                    }
                    Err(error) => {
                        if let Some(sources) = memory["source_ids"].as_array() {
                            rejected_sources.extend(
                                sources
                                    .iter()
                                    .filter_map(|raw| view.source(raw, true))
                                    .flat_map(|g| g.members.iter().copied()),
                            );
                        }
                        reject(
                            "memories",
                            index,
                            &format!("{error}; supporting sources kept"),
                        );
                    }
                }
            }
        } else {
            reject("memories", 0, "expected an array of at most 64 memories");
            if let Some(items) = value["memories"].as_array() {
                rejected_sources.extend(items.iter().flat_map(|memory| {
                    memory["source_ids"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|raw| view.source(raw, true))
                        .flat_map(|g| g.members.iter().copied())
                }));
            }
        }
        let accepted_opinions = updates.len() + memory_updates.len();
        let mut accepted_memories = 0;
        for (id, opinion) in updates {
            if let Some(group) = groups.iter().find(|group| group.id == id) {
                // Normalize projected group IDs to immutable original members.
                for member in &group.members {
                    self.opinions.insert(*member, opinion.into());
                }
                self.drop_members
                    .retain(|_, members| !members.iter().any(|m| group.members.contains(m)));
                if opinion == "drop" {
                    self.drop_members.insert(id, group.members.clone());
                }
            }
        }
        let retired = memory_updates
            .into_iter()
            .filter_map(|(id, opinion)| (opinion == "drop").then_some(id))
            .collect::<HashSet<_>>();
        self.memories.retain(|m| !retired.contains(&m.id));
        self.memory_anchors.retain(|id, _| !retired.contains(id));
        let first_new_memory_id = self.next_memory_id;
        for ((sources, text), index) in memories.into_iter().zip(memory_indexes) {
            if self.memories.len() >= 128 {
                rejected_sources.extend(&sources);
                reject(
                    "memories",
                    index,
                    "active memory limit of 128 reached; supporting sources kept",
                );
                continue;
            }
            let mut candidate = self.clone();
            let id = candidate.next_memory_id;
            candidate.next_memory_id = candidate
                .next_memory_id
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("memory ID space exhausted"))?;
            // Source IDs identify atomic groups, not necessarily their last
            // member (e.g. a tool call/result cohort). Anchor after the whole
            // latest source group, even after that group has been removed.
            if let Some(anchor) = sources.iter().max() {
                candidate.memory_anchors.insert(id, *anchor);
            }
            candidate.memories.push(Memory {
                id,
                source_ids: sources,
                text: text.clone(),
            });
            if admit(&candidate) {
                *self = candidate;
                accepted_memories += 1;
            } else {
                rejected_sources.extend(&candidate.memories.last().unwrap().source_ids);
                reject(
                    "memories",
                    index,
                    "aggregate rendered memory byte or token budget exceeded; supporting sources kept; cleanup headroom required",
                );
            }
        }
        self.preserve_memory_sources(&rejected_sources.into_iter().collect::<Vec<_>>());
        self.review_feedback = json!({"accepted_opinions":accepted_opinions, "accepted_memories":accepted_memories,
            "rejected_count":rejected_count, "rejected":rejected,
            "guidance":"Accepted items were applied except supporting sources of rejected replacements remain protected. Correct rejected items in a future review using the current eligible ledger; do not repeat accepted memories."});
        // Admission includes the actual final feedback, not a smaller transient
        // ledger. Detailed diagnostics cannot consume the review's whole budget.
        while !admit(self) {
            if self.review_feedback["rejected"]
                .as_array()
                .is_some_and(|r| !r.is_empty())
            {
                self.review_feedback["rejected"]
                    .as_array_mut()
                    .unwrap()
                    .pop();
            } else if self
                .memories
                .last()
                .is_some_and(|m| m.id >= first_new_memory_id)
            {
                let memory = self.memories.pop().unwrap();
                self.memory_anchors.remove(&memory.id);
                self.preserve_memory_sources(&memory.source_ids);
                accepted_memories -= 1;
                rejected_count += 1;
                self.review_feedback["accepted_memories"] = json!(accepted_memories);
                self.review_feedback["rejected_count"] = json!(rejected_count);
                self.review_feedback["rejected"] = json!([{"field":"memories","reason":"final rendered review budget exceeded; supporting sources kept"}]);
            } else {
                break;
            }
        }
        Ok(())
    }

    /// A proposed replacement that was rejected cannot authorize losing its
    /// supporting source. This is not a memory requirement for unrelated drops.
    pub fn preserve_memory_sources(&mut self, sources: &[u64]) {
        for group in self
            .groups()
            .iter()
            .filter(|g| g.members.iter().any(|id| sources.contains(id)))
        {
            for member in &group.members {
                self.opinions.insert(*member, "keep".into());
            }
            self.drop_members
                .retain(|_, members| !members.iter().any(|m| group.members.contains(m)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consecutive_completed_tool_steps_keep_independent_context_ids() {
        let mut state = Session::default();
        let mut input = vec![json!({"role":"user","content":"goal"})];
        for id in ["a", "b"] {
            input.extend([
                json!({"type":"reasoning","encrypted_content":id,"summary":[]}),
                json!({"type":"custom_tool_call","call_id":id,"name":"inspect","input":"source"}),
                json!({"type":"custom_tool_call_output","call_id":id,"output":"result"}),
            ]);
        }
        input.push(json!({"role":"user","content":"current"}));
        state.ingest(&input).unwrap();
        for item in &mut state.history {
            item.exposed = true;
        }
        let groups = state.groups();
        assert_eq!(
            groups.iter().map(|g| g.members.clone()).collect::<Vec<_>>(),
            vec![vec![1], vec![2, 3, 4], vec![5, 6, 7], vec![8]]
        );
        state
            .apply_advice(
                &groups,
                &json!({"protected":[2],"removable":[5],"memories":[]}),
            )
            .unwrap();
        let projected = state.render_projection(&HashSet::from([5, 6, 7]));
        assert_eq!(&projected[..4], &input[..4]);
        assert_eq!(&projected[5..], &input[7..]);
        assert_eq!(projected.len(), input.len() - 2);
        assert_eq!(
            state.render_primary(),
            input,
            "planning never edits original source"
        );
    }

    #[test]
    fn memory_retirement_frees_capacity_for_a_replacement_in_the_same_review() {
        let mut state = Session::default();
        state
            .ingest(&[
                json!({"role":"user","content":"goal"}),
                json!({"role":"user","content":"current"}),
            ])
            .unwrap();
        for item in &mut state.history {
            item.exposed = true;
        }
        state.memories = (1..=128)
            .map(|id| Memory {
                id,
                source_ids: vec![1],
                text: format!("fact {id}"),
            })
            .collect();
        state.apply_advice(&state.groups(), &json!({"protected":[],"removable":[3],"memories":[{"source_ids":[1],"text":"replacement fact"}]})).unwrap();
        assert_eq!(state.memories.len(), 128);
        assert_eq!(state.memories.last().unwrap().id, 129);
        assert_eq!(state.review_feedback["accepted_memories"], 1);
        assert_eq!(state.review_feedback["rejected_count"], 0);
    }

    #[test]
    fn memory_protection_wins_and_memory_ids_cannot_be_cited_as_sources() {
        let mut state = Session::default();
        state
            .ingest(&[
                json!({"role":"user","content":"goal"}),
                json!({"role":"assistant","content":"evidence"}),
                json!({"role":"user","content":"current"}),
            ])
            .unwrap();
        for item in &mut state.history {
            item.exposed = true;
        }
        let groups = state.groups();
        state.apply_advice(&groups, &json!({"protected":[],"removable":[],"memories":[{"source_ids":[2],"text":"useful fact"}]})).unwrap();
        state.apply_advice(&groups, &json!({"protected":[4],"removable":[4],"memories":[{"source_ids":[4],"text":"invalid provenance"}]})).unwrap();
        assert_eq!(state.memories.len(), 1);
        assert_eq!(state.review_feedback["accepted_opinions"], 1);
        assert_eq!(state.review_feedback["rejected_count"], 2);
    }

    #[test]
    fn mixed_advice_applies_valid_items_without_invalid_sources() {
        let mut state = Session::default();
        state
            .ingest(&[
                json!({"role":"user","content":"goal"}),
                json!({"role":"assistant","content":"evidence"}),
                json!({"role":"user","content":"latest goal"}),
            ])
            .unwrap();
        for item in &mut state.history {
            item.exposed = true;
        }
        let groups = state.groups();
        let result = state.apply_advice(
            &groups,
            &json!({
                "protected":[1], "removable":[999,2],
                "memories":[{"source_ids":[999],"text":"unsafe"},
                            {"source_ids":[2],"text":"valid fact"}]
            }),
        );
        assert!(result.is_ok(), "one invalid item must not reject the batch");
        assert_eq!(state.opinions.get(&2).map(String::as_str), Some("drop"));
        assert_eq!(state.memories.len(), 1);
        assert_eq!(state.memories[0].text, "valid fact");
        assert_eq!(state.review_feedback["rejected_count"], 2);
        let restored: Session =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        assert_eq!(restored.review_feedback, state.review_feedback);
        assert!(!restored.review_feedback.to_string().contains("unsafe"));
        state.reset_active();
        assert!(state.review_feedback.is_null());
    }

    #[test]
    fn conflicting_and_malformed_advice_items_are_isolated_and_feedback_is_bounded() {
        let mut state = Session::default();
        state
            .ingest(&[
                json!({"role":"user","content":"goal"}),
                json!({"role":"assistant","content":"evidence"}),
                json!({"role":"user","content":"current"}),
            ])
            .unwrap();
        for item in &mut state.history {
            item.exposed = true;
        }
        let groups = state.groups();
        state
            .apply_advice(
                &groups,
                &json!({"protected":[2,2],"removable":[2,3,17],
            "memories":[{"source_ids":[2],"text":"fact"},{"source_ids":[3],"text":"pinned"},
                {"source_ids":[2],"text":""},{"source_ids":[2],"text":"fact"}]}),
            )
            .unwrap();
        assert_eq!(state.opinions.get(&2).map(String::as_str), Some("keep"));
        assert_eq!(state.memories.len(), 1);
        assert_eq!(state.review_feedback["rejected_count"], 6);
        state
            .apply_advice(
                &groups,
                &json!({"protected":null,"removable":[2],"memories":[]}),
            )
            .unwrap();
        assert_eq!(state.opinions.get(&2).map(String::as_str), Some("drop"));
        assert_eq!(state.review_feedback["rejected_count"], 1);
        state
            .apply_advice(
                &groups,
                &json!({"protected":vec![999;256],"removable":[],"memories":[]}),
            )
            .unwrap();
        assert_eq!(state.review_feedback["rejected_count"], 256);
        assert_eq!(
            state.review_feedback["rejected"].as_array().unwrap().len(),
            64
        );
    }

    #[test]
    fn tolerated_native_metadata_echo_uses_current_exact_wire_values() {
        let mut state = Session::default();
        let first = vec![
            json!({"role": "user", "content": "goal"}),
            json!({"type": "message", "id": "msg", "status": "completed", "role": "assistant", "content": [{"type": "output_text", "text": "done", "annotations": []}]}),
        ];
        state.ingest(&first).unwrap();
        let mut echoed = first;
        echoed[1].as_object_mut().unwrap().remove("id");
        echoed[1].as_object_mut().unwrap().remove("status");
        echoed[1]["content"][0]
            .as_object_mut()
            .unwrap()
            .remove("annotations");
        state.ingest(&echoed).unwrap();
        assert_eq!(
            state.render_primary(),
            echoed,
            "tolerant echo matching must not rewrite native metadata back to prior values"
        );
    }

    #[test]
    fn unknown_item_in_secure_completed_output_cohort_pins_parallel_tools() {
        let mut state = Session::default();
        let goal = json!({"role": "user", "content": "goal"});
        state.ingest(std::slice::from_ref(&goal)).unwrap();
        let call =
            json!({"type": "function_call", "call_id": "a", "name": "native", "arguments": "{}"});
        let opaque = json!({"type": "future_native_output", "payload": "opaque"});
        state.pending_output = vec![call.clone(), opaque.clone()];
        state
            .ingest(&[
                goal,
                call,
                opaque,
                json!({"type": "function_call_output", "call_id": "a", "output": "result"}),
            ])
            .unwrap();
        let group = state.groups().into_iter().find(|g| g.id == 2).unwrap();
        assert!(
            group.pinned,
            "unknown native output pins its securely known completion cohort"
        );
        assert_eq!(group.members, vec![2, 3, 4]);
    }

    #[test]
    fn incomplete_and_unknown_parallel_native_cohorts_are_pinned() {
        for unknown in [false, true] {
            let mut state = Session::default();
            let mut input = vec![
                json!({"role": "user", "content": "goal"}),
                json!({"type": "reasoning", "encrypted_content": "opaque", "summary": []}),
                json!({"type": "function_call", "call_id": "a", "name": "native", "arguments": "{}"}),
                json!({"type": "function_call", "call_id": "b", "name": "native", "arguments": "{}"}),
                json!({"type": "function_call_output", "call_id": "a", "output": "one result"}),
            ];
            if unknown {
                input.push(json!({"type": "unknown_native", "encrypted_content": "keep exact"}));
            }
            state.ingest(&input).unwrap();
            for item in &mut state.history {
                item.exposed = true;
            }
            let group = state.groups().into_iter().find(|g| g.id == 2).unwrap();
            assert!(group.pinned);
            assert_eq!(group.members, vec![2, 3, 4, 5]);
            if unknown {
                assert!(state.groups().last().unwrap().pinned);
            }
        }
    }

    #[test]
    fn duplicate_tool_results_pin_the_entire_completion() {
        let mut state = Session::default();
        state.ingest(&[
            json!({"type": "function_call", "call_id": "a", "name": "native", "arguments": "{}"}),
            json!({"type": "function_call_output", "call_id": "a", "output": "one"}),
            json!({"type": "function_call_output", "call_id": "a", "output": "duplicate"}),
        ]).unwrap();
        let groups = state.groups();
        assert_eq!(groups.len(), 1);
        assert!(groups[0].pinned);
    }

    #[test]
    fn new_outputs_and_results_do_not_inherit_prior_input_exposure() {
        let mut state = Session::default();
        let initial = json!({"role": "user", "content": "goal"});
        state.ingest(std::slice::from_ref(&initial)).unwrap();
        state.history[0].exposed = true;
        state.pending_output = vec![
            json!({"type": "function_call", "id": "fc", "status": "completed", "call_id": "a", "name": "native", "arguments": "{}"}),
        ];
        state.ingest(&[
            initial,
            json!({"type": "function_call", "call_id": "a", "name": "native", "arguments": "{}"}),
            json!({"type": "function_call_output", "call_id": "a", "output": "fresh"}),
        ]).unwrap();
        assert!(
            !state
                .groups()
                .into_iter()
                .find(|g| g.id == 2)
                .unwrap()
                .exposed
        );
    }
}
