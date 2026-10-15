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
    pub removed: bool,
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
pub(super) struct ShadowRecord {
    pub source_id: u64,
    pub value: Value,
}

/// The reviewer's actual replayable conversation. Source identity is bounded:
/// keep a digest rather than another copy of the already stored source items.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct ReviewHistory {
    pub source_len: usize,
    pub source_sha256: String,
    pub input: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Memory {
    pub id: u64,
    pub source_ids: Vec<u64>,
    pub text: String,
}

pub(super) use crate::proxy_cache::CacheEvidence;

/// Reviewer-only boundary requested on the actual wire. A requested write is
/// NOT proof of a provider write; economic credit requires a confirmed read.
/// Identity of the exact marked stable prefix, with ONLY renderer-owned marker
/// metadata removed. Never retain another source payload per boundary.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct ReviewerCacheBoundary {
    pub format_version: u32,
    pub base: Value,
    pub input_len: usize,
    pub input_sha256: String,
    pub at: u64,
    pub read_estimated_tokens: f64,
    pub read_confirmed_at: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct Session {
    pub version: u32,
    #[serde(default)]
    pub title: Option<String>,
    pub next_id: u64,
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
    pub opinions: BTreeMap<u64, String>,
    /// A drop authorizes exactly the native membership actually reviewed.
    /// Legacy opinions without this provenance remain protective, never drops.
    #[serde(default)]
    pub drop_members: BTreeMap<u64, Vec<u64>>,
    pub memories: Vec<Memory>,
    #[serde(default)]
    pub next_memory_id: u64,
    #[serde(default)]
    pub memory_anchors: BTreeMap<u64, u64>,
    /// Active reviewer view only. Audit source may persist in history, but is
    /// NEVER used to render the reviewer after paired selection removes it.
    pub active_shadow: Vec<ShadowRecord>,
    #[serde(default)]
    pub review_history: Option<ReviewHistory>,
    #[serde(default)]
    pub reviewer_compactions: u64,
    /// One immutable observation per active-window main item ID; value is its coupled group ID.
    /// Retired windows are reconstructed from canonical source when reconsidered.
    /// Strings retain checkpoint compatibility with the former payload map.
    pub observed: BTreeMap<u64, String>,
    pub primary_cache: Vec<CacheEvidence>,
    /// Legacy whole-review receipts decode unchanged, but cannot establish a
    /// shorter stable reviewer boundary and are no longer used by its planner.
    pub shadow_cache: Vec<CacheEvidence>,
    #[serde(default)]
    pub reviewer_cache: Vec<ReviewerCacheBoundary>,
    #[serde(default)]
    pub cost_trajectory: Option<crate::proxy_savings::Trajectory>,
    pub primary: UsageLedger,
    pub shadow: UsageLedger,
    pub completed_requests: u64,
    pub last_review_request: u64,
    /// A replay import still needs pre-primary review until that epoch's first
    /// primary completes. Persist this across failed calls and process restarts.
    #[serde(default)]
    pub bootstrap_review_pending: bool,
    #[serde(default)]
    pub bootstrap_end_id: u64,
    #[serde(default)]
    pub bootstrap_reviewed_ids: BTreeSet<u64>,
    #[serde(default)]
    pub bootstrap_review_complete: bool,
    #[serde(default)]
    pub bootstrap_estimated_cost_usd: f64,
    pub invalid_reviews: u64,
    #[serde(default)]
    pub partial_reviews: u64,
    /// Only bounded item indexes/reasons, never rejected source text or raw advice.
    #[serde(default)]
    pub review_feedback: Value,
    #[serde(default)]
    pub review_epoch: u64,
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
    pub review_cache_key: String,
    pub review_context: Value,
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
        if self.version != 0 && self.version != 1 {
            bail!("unsupported proxy checkpoint version");
        }
        self.version = 1;
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
                removed: false,
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
        self.bootstrap_end_id = self.history[end - 1].id;
        self.bootstrap_review_complete = false;
    }

    pub fn reset_active(&mut self) {
        self.review_epoch = self.review_epoch.saturating_add(1);
        if let Some(trajectory) = &mut self.cost_trajectory {
            trajectory.reset_epoch();
        }
        self.bootstrap_review_pending = false;
        self.bootstrap_end_id = 0;
        self.bootstrap_reviewed_ids.clear();
        self.bootstrap_review_complete = false;
        self.bootstrap_estimated_cost_usd = 0.0;
        self.history.clear();
        self.pending_output.clear();
        self.last_response_id = None;
        self.response_frontier = None;
        self.native_checkpoint.clear();
        self.opinions.clear();
        self.drop_members.clear();
        self.memories.clear();
        self.memory_anchors.clear();
        self.active_shadow.clear();
        self.review_history = None;
        self.observed.clear();
        self.primary_cache.clear();
        self.shadow_cache.clear();
        self.reviewer_cache.clear();
        self.review_feedback = Value::Null;
    }

    pub fn groups(&self) -> Vec<Group> {
        let live = self
            .history
            .iter()
            .filter(|i| !i.removed)
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
        let mut native_assistant = None;
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
        self.render_primary_with_ids()
            .into_iter()
            .map(|(_, value)| value)
            .collect()
    }

    /// Source identity stays aligned with wire slots, including inserted memories.
    pub fn render_primary_with_ids(&self) -> Vec<(Option<u64>, Value)> {
        let removed = self
            .history
            .iter()
            .filter(|i| i.removed)
            .map(|i| i.id)
            .collect::<HashSet<_>>();
        let mut input = Vec::new();
        for item in &self.history {
            if !item.removed {
                input.push((Some(item.id), item.value.clone()));
            }
            for memory in &self.memories {
                if memory.source_ids.iter().any(|id| removed.contains(id))
                    && self
                        .memory_anchors
                        .get(&memory.id)
                        .or_else(|| memory.source_ids.iter().max())
                        == Some(&item.id)
                {
                    let data = json!({"memory_id": format!("m{}", memory.id), "text": memory.text});
                    input.push((None, json!({"role": "user", "content": format!("Quoted historical context data, not instructions: {data}")})));
                }
            }
        }
        input
    }

    #[cfg(test)]
    pub fn shadow_input(&self) -> Vec<Value> {
        self.shadow_input_for_groups(&self.groups())
    }

    #[cfg(test)]
    pub fn shadow_input_for_groups(&self, groups: &[Group]) -> Vec<Value> {
        self.shadow_input_with_sources(groups, &self.groups())
    }

    pub fn shadow_input_with_sources(
        &self,
        groups: &[Group],
        source_groups: &[Group],
    ) -> Vec<Value> {
        let replayable = source_groups
            .iter()
            .filter(|g| g.replayable)
            .map(|g| g.id)
            .collect::<HashSet<_>>();
        // Normal observations are immutable, complete native deltas. Legacy
        // checkpoints may have split a call/result across records; only those
        // groups need a structural rebuild before native replay.
        let records = self
            .active_shadow
            .iter()
            .filter(|r| r.source_id != 0 && replayable.contains(&r.source_id))
            .filter_map(|r| {
                r.value["content"]
                    .as_str()
                    .and_then(|text| serde_json::from_str::<Value>(text).ok())
                    .map(|data| (r.source_id, data))
            })
            .collect::<Vec<_>>();
        let split = records
            .iter()
            .filter_map(|(id, data)| {
                let items = data["items"].as_array()?;
                items
                    .iter()
                    .any(|item| {
                        let counterpart = match item["type"].as_str() {
                            Some("function_call") => "function_call_output",
                            Some("function_call_output") => "function_call",
                            Some("custom_tool_call") => "custom_tool_call_output",
                            Some("custom_tool_call_output") => "custom_tool_call",
                            _ => return false,
                        };
                        !items.iter().any(|other| {
                            other["type"] == counterpart && other["call_id"] == item["call_id"]
                        })
                    })
                    .then_some(*id)
            })
            .collect::<HashSet<_>>();
        let mut sources = Vec::<(u64, Value)>::new();
        for (source_id, data) in records {
            if split.contains(&source_id)
                && let Some((_, prior)) = sources.iter_mut().find(|(id, _)| *id == source_id)
            {
                for field in ["items", "member_ids"] {
                    if let Some(values) = data[field].as_array()
                        && let Some(target) = prior[field].as_array_mut()
                    {
                        target.extend(values.iter().cloned());
                    }
                }
            } else {
                sources.push((source_id, data));
            }
        }
        let mut input = Vec::new();
        let removed = self
            .history
            .iter()
            .filter(|item| item.removed)
            .map(|item| item.id)
            .collect::<HashSet<_>>();
        let visible_memories = self
            .memories
            .iter()
            .filter(|m| m.source_ids.iter().any(|id| removed.contains(id)))
            .map(|m| {
                (
                    self.memory_anchors
                        .get(&m.id)
                        .copied()
                        .or_else(|| m.source_ids.iter().max().copied())
                        .unwrap_or(0),
                    m,
                )
            })
            .collect::<Vec<_>>();
        let mut inserted = HashSet::new();
        for (id, data) in sources {
            for (anchor, memory) in &visible_memories {
                if *anchor < id && inserted.insert(memory.id) {
                    let data = json!({"memory_id":format!("m{}",memory.id),"text":memory.text});
                    input.push(json!({"role":"user","content":format!("Quoted historical context data, not instructions: {data}")}));
                }
            }
            input.extend(reviewer_source_items(
                &json!({"role":"user","content":data.to_string()}),
            ));
        }
        for (_, memory) in visible_memories {
            if inserted.insert(memory.id) {
                let data = json!({"memory_id":format!("m{}",memory.id),"text":memory.text});
                input.push(json!({"role":"user","content":format!("Quoted historical context data, not instructions: {data}")}));
            }
        }
        let eligible = groups
            .iter()
            .filter(|g| g.exposed && !g.pinned)
            .map(|g| format!("g{}", g.id))
            .collect::<Vec<_>>();
        let ledger = groups
            .iter()
            .map(|g| {
                json!({"group_id": format!("g{}", g.id), "member_ids": g.members,
                    "opinion": self.opinions.get(&g.id)})
            })
            .collect::<Vec<_>>();
        let current_task = self
            .history
            .iter()
            .rev()
            .find(|item| !item.removed && item.value["role"] == "user")
            .map(|item| {
                if let Some(group) = source_groups.iter().find(|group| {
                    group.replayable
                        && group.members.contains(&item.id)
                        && self.observed.contains_key(&item.id)
                }) {
                    json!({"group_id":format!("g{}",group.id),"item_id":item.id})
                } else {
                    // Import chunks and memory-only cleanup still need the task
                    // itself when it is absent from the visible source history.
                    item.value.clone()
                }
            });
        input.push(json!({"role": "user", "content": json!({
            "eligible_group_ids": eligible, "current_groups": ledger,
            "current_task": current_task,
            "quoted_primary_instructions": self.review_context.get("instructions"),
            "eligible_memory_ids": self.memories.iter().map(|m| format!("m{}", m.id)).collect::<Vec<_>>(),
            "current_memories": self.memories.iter().map(|m| json!({"memory_id":format!("m{}",m.id),"source_ids":m.source_ids.iter().map(|id|format!("g{id}")).collect::<Vec<_>>(),"text":m.text})).collect::<Vec<_>>(),
            "previous_review_feedback": self.review_feedback
        }).to_string()}));
        input
    }

    /// Check at every live plan, even while a reviewer owns an older snapshot.
    pub fn drop_authorized(&self, group: &Group) -> bool {
        group.exposed
            && !group.pinned
            && self.opinions.get(&group.id).is_some_and(|s| s == "drop")
            && self.drop_members.get(&group.id) == Some(&group.members)
            && !group
                .members
                .iter()
                .any(|id| self.opinions.get(id).is_some_and(|s| s == "keep"))
    }

    pub fn reconcile_opinions(&mut self, groups: &[Group]) {
        for group in groups {
            let protected = group
                .members
                .iter()
                .any(|id| self.opinions.get(id).is_some_and(|s| s == "keep"));
            let authorized = self.drop_authorized(group);
            for id in &group.members {
                self.opinions.remove(id);
                if *id != group.id || !authorized {
                    self.drop_members.remove(id);
                }
            }
            if protected {
                self.opinions.insert(group.id, "keep".into());
            } else if authorized {
                self.opinions.insert(group.id, "drop".into());
            }
        }
    }

    pub fn observe_groups(&mut self, groups: &[Group]) {
        self.reconcile_opinions(groups);
        // v1 checkpoints formerly stored mutable group snapshots. Rebuild only
        // this derived projection once, without changing canonical main history.
        if self.observed.values().any(|value| value.starts_with('{')) {
            self.observed.clear();
            self.active_shadow.retain(|record| record.source_id == 0);
        }
        for record in &mut self.active_shadow {
            if record.source_id != 0 {
                continue;
            }
            let Some(data) = record.value["content"]
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
            else {
                continue;
            };
            let Some(memory) = self.memories.iter().find(|memory| {
                data["memory_id"].as_str() == Some(format!("m{}", memory.id).as_str())
            }) else {
                continue;
            };
            let sources = memory
                .source_ids
                .iter()
                .map(|id| format!("g{id}"))
                .collect::<Vec<_>>();
            record.value["content"] =
                json!(json!({"source_ids": sources, "text": memory.text}).to_string());
        }
        for group in groups {
            // Observe a native reasoning/call/result delta only once complete.
            if !group.replayable {
                continue;
            }
            // A legacy split cohort can retain a later member's observation
            // while its earlier reasoning window has retired. Rebuild that
            // group in canonical order, including its current annotation ID.
            let rebuild_at = self
                .active_shadow
                .iter()
                .any(|record| {
                    record.source_id != group.id && group.members.contains(&record.source_id)
                })
                .then(|| {
                    self.active_shadow
                        .iter()
                        .position(|record| group.members.contains(&record.source_id))
                        .unwrap()
                });
            if rebuild_at.is_some() {
                self.active_shadow
                    .retain(|record| !group.members.contains(&record.source_id));
                self.observed.retain(|id, _| !group.members.contains(id));
            }
            // Ordinary growing cohorts keep immutable observations; current
            // membership lives in the ledger and new members append below.
            for id in &group.members {
                if let Some(owner) = self.observed.get_mut(id) {
                    *owner = group.id.to_string();
                }
            }
            let new_items = self
                .history
                .iter()
                .filter(|i| group.members.contains(&i.id) && !self.observed.contains_key(&i.id))
                .collect::<Vec<_>>();
            if new_items.is_empty() {
                continue;
            }
            let member_ids = new_items.iter().map(|i| i.id).collect::<Vec<_>>();
            let values = new_items
                .iter()
                .map(|i| i.value.clone())
                .collect::<Vec<_>>();
            let data = json!({"group_id": format!("g{}", group.id), "member_ids": member_ids,
                "items": values, "human": group.human});
            let record = ShadowRecord {
                source_id: group.id,
                value: json!({"role": "user", "content": data.to_string()}),
            };
            if let Some(index) = rebuild_at {
                self.active_shadow.insert(index, record);
            } else {
                self.active_shadow.push(record);
            }
            for id in member_ids {
                self.observed.insert(id, group.id.to_string());
            }
        }
    }

    #[cfg(test)]
    pub fn apply_advice(&mut self, groups: &[Group], value: &Value) -> Result<()> {
        self.apply_advice_with_memory_admission(groups, value, |_| true)
    }

    pub fn apply_advice_with_memory_admission(
        &mut self,
        groups: &[Group],
        value: &Value,
        admit: impl Fn(&Session) -> bool,
    ) -> Result<()> {
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
        self.reconcile_opinions(groups);
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
        let known_memories = self.memories.iter().map(|m| m.id).collect::<HashSet<_>>();
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
                if let Some(memory_id) = raw.as_str().and_then(|s| s.strip_prefix('m')) {
                    match memory_id.parse::<u64>() {
                        Ok(id) if known_memories.contains(&id) => {
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
                        }
                        _ => reject(
                            field,
                            index,
                            "memory is unknown; use current eligible_memory_ids",
                        ),
                    }
                    continue;
                }
                let Ok(id) = parse_group(raw) else {
                    reject(field, index, "expected a group ID such as g1");
                    continue;
                };
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
                        .map(parse_group)
                        .collect::<Result<Vec<_>>>()?;
                    if sources.iter().any(|id| !known.contains(id)) {
                        bail!("all memory sources must be in current eligible_group_ids");
                    }
                    Ok((sources, text.to_owned()))
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
                    Err(error) => reject("memories", index, &error.to_string()),
                }
            }
        } else {
            reject("memories", 0, "expected an array of at most 64 memories");
        }
        let primary_before = (self.cost_trajectory.is_some()
            && (!memories.is_empty() || memory_updates.values().any(|opinion| *opinion == "drop")))
        .then(|| self.render_primary());
        let accepted_opinions = updates.len() + memory_updates.len();
        let mut accepted_memories = 0;
        for (id, opinion) in updates {
            self.opinions.insert(id, opinion.into());
            self.drop_members.remove(&id);
            if opinion == "drop"
                && let Some(group) = groups.iter().find(|group| group.id == id)
            {
                self.drop_members.insert(id, group.members.clone());
            }
        }
        let retired = memory_updates
            .into_iter()
            .filter_map(|(id, opinion)| (opinion == "drop").then_some(id))
            .collect::<HashSet<_>>();
        let retired_facts = self
            .memories
            .iter()
            .filter(|m| retired.contains(&m.id))
            .cloned()
            .collect::<Vec<_>>();
        self.active_shadow.retain(|record| {
            if record.source_id != 0 {
                return true;
            }
            let Some(data) = record.value["content"]
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
            else {
                return true;
            };
            !retired_facts.iter().any(|m| {
                data["memory_id"] == format!("m{}", m.id)
                    || (data["text"] == m.text
                        && data["source_ids"]
                            == json!(
                                m.source_ids
                                    .iter()
                                    .map(|id| format!("g{id}"))
                                    .collect::<Vec<_>>()
                            ))
            })
        });
        self.memories.retain(|m| !retired.contains(&m.id));
        self.memory_anchors.retain(|id, _| !retired.contains(id));
        if !retired_facts.is_empty() {
            // Answers and old ledgers can contain an unmaterialized fact too.
            self.compact_reviewer();
        }
        for ((sources, text), index) in memories.into_iter().zip(memory_indexes) {
            if self.memories.len() >= 128 {
                reject("memories", index, "active memory limit of 128 reached");
                continue;
            }
            let mut candidate = self.clone();
            let id = candidate.next_memory_id;
            candidate.next_memory_id = candidate
                .next_memory_id
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("memory ID space exhausted"))?;
            let provenance = sources
                .iter()
                .map(|id| format!("g{id}"))
                .collect::<Vec<_>>();
            // Source IDs identify atomic groups, not necessarily their last
            // member (e.g. a tool call/result cohort). Anchor after the whole
            // latest source group, even after that group has been removed.
            if let Some(anchor) = groups
                .iter()
                .filter(|g| sources.contains(&g.id))
                .flat_map(|g| &g.members)
                .max()
            {
                candidate.memory_anchors.insert(id, *anchor);
            }
            candidate.memories.push(Memory {
                id,
                source_ids: sources,
                text: text.clone(),
            });
            candidate.active_shadow.push(ShadowRecord {
                source_id: 0,
                value: json!({"role": "user", "content": json!({"source_ids": provenance, "text": text}).to_string()}),
            });
            if admit(&candidate) {
                *self = candidate;
                accepted_memories += 1;
            } else {
                reject(
                    "memories",
                    index,
                    "aggregate rendered memory byte or token budget exceeded; cleanup headroom required",
                );
            }
        }
        self.review_feedback = json!({"accepted_opinions":accepted_opinions, "accepted_memories":accepted_memories,
            "rejected_count":rejected_count, "rejected":rejected,
            "guidance":"Accepted items were applied. Correct rejected items in a future review using the current eligible ledger; do not repeat accepted memories."});
        if let Some(before) = primary_before {
            let after = self.render_primary();
            // Visible memory edits belong to Carry, not the traditional
            // counterfactual. Even an equal-size edit breaks prefix reuse.
            if before != after
                && let Some(trajectory) = &mut self.cost_trajectory
            {
                let before = crate::proxy_savings::token_units(&json!(before));
                let after = crate::proxy_savings::token_units(&json!(after));
                trajectory.compacted(before, after);
            }
        }
        Ok(())
    }

    /// Rebuild only shared fact records after a background memory delta. Native
    /// observations are independent; source retirement cannot retire a fact.
    pub fn refresh_memory_shadow(&mut self) {
        self.active_shadow.retain(|record| record.source_id != 0);
        for memory in &self.memories {
            let sources = memory
                .source_ids
                .iter()
                .map(|id| format!("g{id}"))
                .collect::<Vec<_>>();
            self.active_shadow.push(ShadowRecord {
                source_id: 0,
                value: json!({"role":"user","content":json!({"source_ids":sources,"text":memory.text}).to_string()}),
            });
        }
    }

    pub fn remove(&mut self, ids: &[u64], groups: &[Group]) {
        let members = groups
            .iter()
            .filter(|g| ids.contains(&g.id))
            .flat_map(|g| g.members.iter().copied())
            .collect::<HashSet<_>>();
        for item in &mut self.history {
            item.removed |= members.contains(&item.id);
        }
        self.active_shadow
            .retain(|record| !ids.contains(&record.source_id));
        self.observed.retain(|id, _| !members.contains(id));
        self.opinions.retain(|id, _| !members.contains(id));
        self.drop_members.retain(|id, _| !members.contains(id));
        self.compact_reviewer();
    }

    /// Nudge prompts and answers have no retention authority. Accepted advice
    /// and memories live separately and survive rebuilding the shared view.
    pub fn compact_reviewer(&mut self) {
        if self.review_history.take().is_some() {
            self.reviewer_compactions += 1;
        }
        // Keep historical receipt evidence: exact base/prefix/TTL checks admit
        // only boundaries still present in the rebuilt live request.
        self.review_epoch = self.review_epoch.saturating_add(1);
    }
}

/// Stored observations retain the original native bytes. Expand them only for
/// reviewer transport; encrypted reasoning must never become quoted JSON text.
fn reviewer_source_items(record: &Value) -> Vec<Value> {
    let Some(mut data) = record["content"]
        .as_str()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
    else {
        return vec![record.clone()];
    };
    let Some(items) = data
        .as_object_mut()
        .and_then(|o| o.remove("items"))
        .and_then(|v| v.as_array().cloned())
    else {
        return vec![record.clone()];
    };
    let mut native = Vec::new();
    let roles = items
        .iter()
        .map(|item| item["role"].clone())
        .collect::<Vec<_>>();
    for mut item in items {
        let ty = item["type"].as_str().unwrap_or("message");
        if matches!(
            ty,
            "message"
                | "reasoning"
                | "function_call"
                | "function_call_output"
                | "custom_tool_call"
                | "custom_tool_call_output"
                | "compaction"
        ) {
            // Primary system/developer text is evidence, not reviewer authority.
            if matches!(item["role"].as_str(), Some("system" | "developer")) {
                item["role"] = json!("user");
            }
            native.push(item);
        } else {
            native.push(
                json!({"role":"user","content":json!({"opaque_native_item":item}).to_string()}),
            );
        }
    }
    data["original_roles"] = json!(roles);
    let annotation = format!("\n\n[carry context] {data}");
    if let Some(last) = native.last_mut()
        && matches!(last["role"].as_str(), Some("user" | "assistant"))
    {
        if let Some(text) = last["content"].as_str() {
            last["content"] = json!(format!("{text}{annotation}"));
            return native;
        }
        let role = last["role"].as_str().unwrap().to_owned();
        if let Some(blocks) = last["content"].as_array_mut() {
            blocks.push(json!({"type":if role == "assistant" { "output_text" } else { "input_text" },"text":annotation}));
            return native;
        }
    }
    // Tool protocol and ciphertext stay untouched. Their small source-ID note
    // follows the native cohort rather than wrapping its contents.
    native.push(json!({"role":"user","content":annotation}));
    native
}

fn parse_group(value: &Value) -> Result<u64> {
    let s = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("group ID is not a string"))?;
    let digits = s
        .strip_prefix('g')
        .filter(|s| !s.is_empty() && !s.starts_with('0') && s.chars().all(|c| c.is_ascii_digit()))
        .ok_or_else(|| anyhow::anyhow!("invalid group ID"))?;
    Ok(digits.parse()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_task_is_not_duplicated_in_the_reviewer_nudge() {
        let mut state = Session::default();
        state
            .ingest(&[
                json!({"role":"user","content":"original task"}),
                json!({"role":"assistant","content":"prior result"}),
                json!({"role":"user","content":"CURRENT_LARGE_TASK_SENTINEL"}),
            ])
            .unwrap();
        state.observe_groups(&state.groups());
        let input = state.shadow_input();
        assert_eq!(
            json!(input)
                .to_string()
                .matches("CURRENT_LARGE_TASK_SENTINEL")
                .count(),
            1
        );
    }

    #[test]
    fn legacy_drop_needs_fresh_membership_but_protection_can_be_released_by_review() {
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
        state.opinions.insert(2, "drop".into());
        let mut state: Session =
            serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
        let groups = state.groups();
        let source = groups.iter().find(|group| group.id == 2).unwrap();
        assert!(!state.drop_authorized(source));
        state.observe_groups(&groups);
        assert!(!state.opinions.contains_key(&2));
        state.opinions.insert(2, "keep".into());
        state.observe_groups(&groups);
        assert_eq!(state.opinions.get(&2).map(String::as_str), Some("keep"));
        state
            .apply_advice(
                &groups,
                &json!({"protected":[],"removable":["g2"],"memories":[]}),
            )
            .unwrap();
        assert!(
            state.drop_authorized(source),
            "fresh review can release old protection, not a permanent keep"
        );
        let restored: Session =
            serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
        assert!(restored.drop_authorized(source));
    }

    #[test]
    fn primary_source_ids_keep_memory_slots_aligned_with_native_history() {
        let mut state = Session::default();
        state
            .ingest(&[
                json!({"role":"developer","content":"instructions"}),
                json!({"role":"user","content":"old evidence"}),
                json!({"role":"assistant","content":"result"}),
                json!({"role":"user","content":"current task"}),
            ])
            .unwrap();
        for item in &mut state.history {
            item.exposed = true;
        }
        let groups = state.groups();
        state
            .apply_advice(
                &groups,
                &json!({"protected":[],"removable":[],"memories":[
                    {"source_ids":["g2"],"text":"retained learning"}
                ]}),
            )
            .unwrap();
        state.remove(&[2], &groups);
        let indexed = state.render_primary_with_ids();
        assert_eq!(
            indexed.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![Some(1), None, Some(3), Some(4)]
        );
        assert_eq!(
            indexed
                .into_iter()
                .map(|(_, value)| value)
                .collect::<Vec<_>>(),
            state.render_primary()
        );
    }

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
                &json!({"protected":["g2"],"removable":["g5"],"memories":[]}),
            )
            .unwrap();
        state.remove(&[5], &groups);
        assert_eq!(
            state.render_primary(),
            [input[..4].to_vec(), input[7..].to_vec()].concat()
        );
    }

    #[test]
    fn legacy_custom_tool_cohorts_are_repaired_before_review() {
        let mut state = Session::default();
        state
            .ingest(&[
                json!({"role":"user","content":"goal"}),
                json!({"type":"reasoning","encrypted_content":"opaque","summary":[]}),
                json!({"type":"custom_tool_call","call_id":"a","name":"inspect","input":"source"}),
                json!({"type":"custom_tool_call_output","call_id":"a","output":"result"}),
                json!({"role":"user","content":"current"}),
            ])
            .unwrap();
        // Older checkpoints classified custom calls as unknown items.
        for item in &mut state.history {
            item.cohort = item.id;
            item.exposed = true;
        }
        state.opinions.insert(2, "drop".into());
        state.opinions.insert(3, "keep".into());
        let mut state: Session =
            serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
        let groups = state.groups();
        let step = groups.iter().find(|g| g.id == 2).unwrap();
        assert_eq!(step.members, vec![2, 3, 4]);
        assert!(!step.pinned);
        state.observe_groups(&groups);
        assert_eq!(
            state.opinions.get(&2).map(String::as_str),
            Some("keep"),
            "cohort repair must preserve prior protection on any member"
        );
    }

    #[test]
    fn retiring_visible_memory_restores_the_traditional_scenario_input() {
        let mut state = Session {
            cost_trajectory: Some(crate::proxy_savings::Trajectory::default()),
            ..Session::default()
        };
        state
            .ingest(&[
                json!({"role":"user","content":"old source"}),
                json!({"role":"user","content":"current task"}),
            ])
            .unwrap();
        for item in &mut state.history {
            item.exposed = true;
        }
        let groups = state.groups();
        state.apply_advice(&groups, &json!({"protected":[],"removable":[],"memories":[{"source_ids":["g1"],"text":"historical learning"}]})).unwrap();
        let trajectory = serde_json::to_value(state.cost_trajectory.as_ref().unwrap()).unwrap();
        assert_eq!(
            trajectory["carry_edited"], false,
            "an invisible memory does not alter the primary"
        );
        state.remove(&[1], &groups);
        let before = crate::proxy_savings::token_units(&json!(state.render_primary()));
        state
            .apply_advice(
                &state.groups(),
                &json!({"protected":[],"removable":["m1"],"memories":[]}),
            )
            .unwrap();
        let after = crate::proxy_savings::token_units(&json!(state.render_primary()));
        let trajectory = serde_json::to_value(state.cost_trajectory.as_ref().unwrap()).unwrap();
        assert_eq!(
            trajectory["restored"],
            json!(before as i128 - after as i128)
        );
        assert_eq!(trajectory["carry_edited"], true);
        assert_eq!(state.compactions, 0);
    }

    #[test]
    fn historical_reviewer_window_receives_latest_task_in_mutable_ledger() {
        let mut state = Session::default();
        let task = json!({"type":"message","role":"user","content":[{"type":"input_text","text":"new task"}]});
        state
            .ingest(&[
                json!({"role":"user","content":"original task"}),
                json!({"role":"assistant","content":"historical finding"}),
                task.clone(),
            ])
            .unwrap();
        let historical = state
            .groups()
            .into_iter()
            .filter(|group| group.id < 3)
            .collect::<Vec<_>>();
        state.observe_groups(&historical);
        let before = state.shadow_input_for_groups(&historical);
        let ledger: Value =
            serde_json::from_str(before.last().unwrap()["content"].as_str().unwrap()).unwrap();
        assert_eq!(ledger["current_task"], task);

        let replacement = json!({"role":"user","content":"another task"});
        state.history.last_mut().unwrap().value = replacement.clone();
        let after = state.shadow_input_for_groups(&historical);
        assert_eq!(&before[..before.len() - 1], &after[..after.len() - 1]);
        let ledger: Value =
            serde_json::from_str(after.last().unwrap()["content"].as_str().unwrap()).unwrap();
        assert_eq!(ledger["current_task"], replacement);
    }

    #[test]
    fn legacy_retained_shadow_repairs_source_order_and_annotation() {
        let mut state = Session::default();
        let reasoning = json!({"type":"reasoning","encrypted_content":"opaque","summary":[]});
        let call =
            json!({"type":"custom_tool_call","call_id":"a","name":"inspect","input":"source"});
        let output = json!({"type":"custom_tool_call_output","call_id":"a","output":"result"});
        state
            .ingest(&[
                json!({"role":"user","content":"goal"}),
                reasoning.clone(),
                call.clone(),
                output.clone(),
                json!({"role":"user","content":"current"}),
            ])
            .unwrap();
        for item in &mut state.history {
            item.cohort = item.id;
            item.exposed = true;
        }
        // A former reviewer window retained the call and result, but retired
        // the earlier reasoning window before native cohort repair existed.
        for (id, item) in [(3, call.clone()), (4, output.clone())] {
            state.active_shadow.push(ShadowRecord {
                source_id: id,
                value: json!({"role":"user","content":json!({"group_id":format!("g{id}"),"member_ids":[id],"items":[item],"human":false}).to_string()}),
            });
            state.observed.insert(id, id.to_string());
        }
        state.observe_groups(&state.groups());
        let wire = state.shadow_input();
        let at = wire.iter().position(|item| item == &reasoning).unwrap();
        assert_eq!(&wire[at..at + 3], &[reasoning, call, output]);
        let metadata = wire[at + 3]["content"]
            .as_str()
            .unwrap()
            .split("[carry context] ")
            .nth(1)
            .unwrap();
        let metadata: Value = serde_json::from_str(metadata).unwrap();
        assert_eq!(metadata["group_id"], "g2");
        assert_eq!(metadata["member_ids"], json!([2, 3, 4]));
    }

    #[test]
    fn custom_tool_step_is_atomic_and_an_incomplete_step_is_not_replayed() {
        let mut state = Session::default();
        let reasoning = json!({"type":"reasoning","id":"rs_custom","summary":[],"encrypted_content":"CUSTOM_REASONING"});
        let call = json!({"type":"custom_tool_call","id":"ctc_custom","call_id":"custom-a","name":"inspect","input":"inspect source"});
        let output =
            json!({"type":"custom_tool_call_output","call_id":"custom-a","output":"custom result"});
        let incomplete = vec![
            json!({"role":"user","content":"goal"}),
            reasoning.clone(),
            call.clone(),
        ];
        state.ingest(&incomplete).unwrap();
        for item in &mut state.history {
            item.exposed = true;
        }
        state.observe_groups(&state.groups());
        assert!(
            !state.shadow_input().contains(&call),
            "unmatched calls must never reach native replay"
        );
        assert!(
            !state.shadow_input().contains(&reasoning),
            "withhold the whole unfinished reasoning/tool cohort"
        );
        let mut complete = incomplete;
        complete.extend([output.clone(), json!({"role":"user","content":"current"})]);
        state.ingest(&complete).unwrap();
        for item in &mut state.history {
            item.exposed = true;
        }
        let groups = state.groups();
        let step = groups.iter().find(|g| g.id == 2).unwrap();
        assert_eq!(step.members, vec![2, 3, 4]);
        assert!(!step.pinned);
        state.observe_groups(&groups);
        let wire = state.shadow_input();
        let at = wire.iter().position(|item| item == &reasoning).unwrap();
        assert_eq!(&wire[at..at + 3], &[reasoning, call, output]);
        state.remove(&[2], &groups);
        assert_eq!(
            state.render_primary(),
            vec![complete[0].clone(), complete[4].clone()]
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
        state.apply_advice(&state.groups(), &json!({"protected":[],"removable":["m1"],"memories":[{"source_ids":["g1"],"text":"replacement fact"}]})).unwrap();
        assert_eq!(state.memories.len(), 128);
        assert_eq!(state.memories.last().unwrap().id, 129);
        assert_eq!(state.review_feedback["accepted_memories"], 1);
        assert_eq!(state.review_feedback["rejected_count"], 0);
    }

    #[test]
    fn retired_memories_leave_both_views_and_ids_are_not_reused_after_restart() {
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
        state.observe_groups(&groups);
        state.apply_advice(&groups, &json!({"protected":[],"removable":[],"memories":[{"source_ids":["g2"],"text":"obsolete fact"}]})).unwrap();
        state.remove(&[2], &groups);
        assert!(
            json!(state.render_primary())
                .to_string()
                .contains("obsolete fact")
        );
        let ledger: Value = serde_json::from_str(
            state.shadow_input().last().unwrap()["content"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(ledger["eligible_memory_ids"], json!(["m1"]));
        state
            .apply_advice(
                &state.groups(),
                &json!({"protected":[],"removable":["m1","m999"],"memories":[]}),
            )
            .unwrap();
        assert!(state.memories.is_empty());
        assert!(
            !json!(state.render_primary())
                .to_string()
                .contains("obsolete fact")
        );
        assert!(
            !json!(state.shadow_input())
                .to_string()
                .contains("obsolete fact")
        );
        assert!(state.memory_anchors.is_empty());
        assert_eq!(state.review_feedback["accepted_opinions"], 1);
        assert_eq!(state.review_feedback["rejected_count"], 1);
        let mut resumed: Session =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        resumed.apply_advice(&resumed.groups(), &json!({"protected":[],"removable":[],"memories":[{"source_ids":["g1"],"text":"new fact"}]})).unwrap();
        assert_eq!(resumed.memories[0].id, 2);
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
        state.apply_advice(&groups, &json!({"protected":[],"removable":[],"memories":[{"source_ids":["g2"],"text":"useful fact"}]})).unwrap();
        state.apply_advice(&groups, &json!({"protected":["m1"],"removable":["m1"],"memories":[{"source_ids":["m1"],"text":"invalid provenance"}]})).unwrap();
        assert_eq!(state.memories.len(), 1);
        assert_eq!(state.review_feedback["accepted_opinions"], 1);
        assert_eq!(state.review_feedback["rejected_count"], 2);
    }

    #[test]
    fn reviewer_receives_native_encrypted_reasoning_and_complete_tool_cohorts() {
        let mut state = Session::default();
        let reasoning = json!({"id":"rs_native","type":"reasoning","encrypted_content":"OPAQUE_ENCRYPTED_REASONING","summary":[]});
        let call = json!({"id":"fc_native","type":"function_call","call_id":"call_native","name":"inspect","arguments":"{}"});
        let output =
            json!({"type":"function_call_output","call_id":"call_native","output":"native result"});
        state
            .ingest(&[
                json!({"role":"user","content":"task"}),
                reasoning.clone(),
                call.clone(),
                output.clone(),
                json!({"role":"user","content":"next"}),
            ])
            .unwrap();
        for item in &mut state.history {
            item.exposed = true;
        }
        state.observe_groups(&state.groups());
        let input = state.shadow_input();
        assert!(
            input.contains(&reasoning),
            "ciphertext must remain a native reasoning item"
        );
        assert!(
            input.contains(&call),
            "native call IDs and arguments must survive"
        );
        assert!(
            input.contains(&output),
            "tool outputs must preserve the matched native call"
        );
        let reasoning_pos = input.iter().position(|i| i == &reasoning).unwrap();
        let call_pos = input.iter().position(|i| i == &call).unwrap();
        let output_pos = input.iter().position(|i| i == &output).unwrap();
        assert!(reasoning_pos < call_pos && call_pos < output_pos);
        assert_eq!(state.render_primary()[1], reasoning);
    }

    #[test]
    fn carried_memories_do_not_prepend_to_reconstructed_source_prefix() {
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
        state.observe_groups(&groups);
        let before = state.shadow_input_for_groups(&groups);
        state.apply_advice(&groups, &json!({"protected":[],"removable":[],"memories":[{"source_ids":["g2"],"text":"fact"}]})).unwrap();
        state.active_shadow.retain(|record| record.source_id == 0);
        state.observed.clear();
        state.observe_groups(&groups);
        let after = state.shadow_input_for_groups(&groups);
        assert!(after.starts_with(&before[..before.len() - 1]));
        let fact: Value =
            serde_json::from_str(after.last().unwrap()["content"].as_str().unwrap()).unwrap();
        assert_eq!(fact["current_memories"][0]["source_ids"], json!(["g2"]));
        assert_eq!(fact["current_memories"][0]["text"], "fact");
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
                "protected":["g1"], "removable":["g999","g2"],
                "memories":[{"source_ids":["g999"],"text":"unsafe"},
                            {"source_ids":["g2"],"text":"valid fact"}]
            }),
        );
        assert!(result.is_ok(), "one invalid item must not reject the batch");
        assert_eq!(state.opinions.get(&2).map(String::as_str), Some("drop"));
        assert_eq!(state.memories.len(), 1);
        assert_eq!(state.memories[0].text, "valid fact");
        assert_eq!(state.review_feedback["rejected_count"], 2);
        let restored: Session =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        let shadow = restored.shadow_input();
        let ledger: Value =
            serde_json::from_str(shadow.last().unwrap()["content"].as_str().unwrap()).unwrap();
        assert_eq!(ledger["previous_review_feedback"], state.review_feedback);
        assert!(
            !ledger["previous_review_feedback"]
                .to_string()
                .contains("unsafe")
        );
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
                &json!({"protected":["g2","g2"],"removable":["g2","g3",17],
            "memories":[{"source_ids":["g2"],"text":"fact"},{"source_ids":["g3"],"text":"pinned"},
                {"source_ids":["g2"],"text":""},{"source_ids":["g2"],"text":"fact"}]}),
            )
            .unwrap();
        assert_eq!(state.opinions.get(&2).map(String::as_str), Some("keep"));
        assert_eq!(state.memories.len(), 1);
        assert_eq!(state.review_feedback["rejected_count"], 6);
        state
            .apply_advice(
                &groups,
                &json!({"protected":null,"removable":["g2"],"memories":[]}),
            )
            .unwrap();
        assert_eq!(state.opinions.get(&2).map(String::as_str), Some("drop"));
        assert_eq!(state.review_feedback["rejected_count"], 1);
        state
            .apply_advice(
                &groups,
                &json!({"protected":vec!["g999";256],"removable":[],"memories":[]}),
            )
            .unwrap();
        assert_eq!(state.review_feedback["rejected_count"], 256);
        assert_eq!(
            state.review_feedback["rejected"].as_array().unwrap().len(),
            64
        );
    }

    #[test]
    fn memories_activate_on_any_removed_source_at_latest_source_position() {
        let mut state = Session::default();
        let input = vec![
            json!({"role": "user", "content": "goal"}),
            json!({"role": "assistant", "content": "first evidence"}),
            json!({"role": "assistant", "content": "last evidence"}),
            json!({"role": "user", "content": "next task"}),
        ];
        state.ingest(&input).unwrap();
        state.memories.push(Memory {
            id: 1,
            source_ids: vec![2, 3],
            text: "small fact".into(),
        });
        assert_eq!(state.render_primary(), input);
        state.history[1].removed = true;
        let rendered = state.render_primary();
        assert_eq!(rendered.len(), 4);
        assert_eq!(rendered[1], input[2]);
        assert!(
            rendered[2]["content"]
                .as_str()
                .unwrap()
                .contains("small fact")
        );
        assert_eq!(rendered[3], input[3]);
        state.history[2].removed = true;
        let rendered = state.render_primary();
        assert_eq!(rendered.len(), 3);
        assert!(
            rendered[1]["content"]
                .as_str()
                .unwrap()
                .contains("small fact")
        );
        assert_eq!(rendered[2], input[3]);
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

    #[test]
    fn explicit_rebase_retires_populated_selection_memories_shadow_and_cache_but_keeps_usage() {
        let mut state = Session::default();
        let input = vec![
            json!({"role": "user", "content": "old requirement"}),
            json!({"type": "function_call", "call_id": "a", "name": "native", "arguments": "{}"}),
            json!({"type": "function_call_output", "call_id": "a", "output": "old evidence"}),
            json!({"role": "user", "content": "old latest goal"}),
        ];
        state.ingest(&input).unwrap();
        for item in &mut state.history {
            item.exposed = true;
        }
        let groups = state.groups();
        state.observe_groups(&groups);
        state.apply_advice(&groups, &json!({"protected": ["g1"], "removable": ["g2"], "memories": [{"source_ids": ["g2"], "text": "old derived memory"}]})).unwrap();
        state.remove(&[2], &groups);
        let evidence = CacheEvidence::legacy(json!({"model": "gpt-6-luna"}), input, 1, 1.0);
        state.primary_cache.push(evidence.clone());
        state.shadow_cache.push(evidence);
        state.review_history = Some(ReviewHistory {
            source_len: 0,
            source_sha256: String::new(),
            input: vec![json!({"role":"user","content":"old reviewer turn"})],
        });
        state.primary.calls = 3;
        state.shadow.calls = 2;
        let prior_id = state.next_id;
        let replacement = vec![json!({"role": "user", "content": "caller checkpoint summary"})];
        assert!(
            state.ingest(&replacement).is_err(),
            "strict mode must not guess summary ancestry"
        );
        assert!(!state.active_shadow.is_empty());
        state.ingest_with_rebase(&replacement, true).unwrap();
        assert_eq!(state.render_primary(), replacement);
        assert!(
            state.active_shadow.is_empty()
                && state.opinions.is_empty()
                && state.observed.is_empty()
        );
        assert!(
            state.memories.is_empty()
                && state.primary_cache.is_empty()
                && state.shadow_cache.is_empty()
        );
        assert!(state.next_id > prior_id && !state.history[0].exposed);
        assert_eq!(state.history_rebases, 1);
        assert!(state.review_history.is_none());
        assert_eq!((state.primary.calls, state.shadow.calls), (3, 2));
    }

    #[test]
    fn legacy_checkpoint_projection_migrates_once_without_changing_native_history() {
        let input = vec![
            json!({"role": "user", "content": "old requirement"}),
            json!({"role": "user", "content": "latest task"}),
            json!({"type": "function_call", "call_id": "a", "name": "native", "arguments": "{}"}),
            json!({"type": "function_call_output", "call_id": "a", "output": "legacy evidence"}),
        ];
        let mut state = Session::default();
        state.ingest(&input).unwrap();
        for item in &mut state.history {
            item.exposed = true;
        }
        let groups = state.groups();
        state.memories.push(Memory {
            id: 1,
            source_ids: vec![1],
            text: "grounded fact".into(),
        });
        for group in &groups {
            let values = state
                .history
                .iter()
                .filter(|i| group.members.contains(&i.id))
                .map(|i| i.value.clone())
                .collect::<Vec<_>>();
            for eligible in [false, true] {
                let content = json!({"group_id": format!("g{}", group.id), "eligible": eligible, "human": group.human, "items": values}).to_string();
                state.observed.insert(group.id, content.clone());
                state.active_shadow.push(ShadowRecord {
                    source_id: group.id,
                    value: json!({"role": "user", "content": content}),
                });
            }
        }
        state.active_shadow.push(ShadowRecord { source_id: 0, value: json!({"role": "user", "content": json!({"memory_id": "m1", "text": "grounded fact"}).to_string()}) });
        let checkpoint = serde_json::to_vec(&state).unwrap();
        let mut resumed: Session = serde_json::from_slice(&checkpoint).unwrap();
        resumed.observe_groups(&resumed.groups());
        let first = resumed.shadow_input();
        resumed.observe_groups(&resumed.groups());
        assert_eq!(resumed.shadow_input(), first);
        assert_eq!(resumed.render_primary(), input);
        let data = first
            .iter()
            .filter_map(|r| {
                r["content"].as_str().and_then(|text| {
                    serde_json::from_str::<Value>(text.rsplit("[carry context] ").next().unwrap())
                        .ok()
                })
            })
            .collect::<Vec<_>>();
        let ids = data
            .iter()
            .filter_map(|r| r["member_ids"].as_array())
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![json!(1), json!(2), json!(3), json!(4)]);
        assert_eq!(
            data.last().unwrap()["current_memories"][0],
            json!({"memory_id":"m1", "source_ids": ["g1"], "text": "grounded fact"})
        );
        assert_eq!(
            data.last().unwrap()["eligible_group_ids"],
            json!(["g1", "g3"])
        );
        assert!(
            data.iter()
                .all(|r| r.get("eligible").is_none() && r.get("memory_id").is_none())
        );
    }

    #[test]
    fn mixed_batch_projection_removes_only_main_source_and_keeps_shared_memory() {
        let mut state = Session::default();
        let input = vec![
            json!({"role": "user", "content": "retained requirement"}),
            json!({"type": "function_call", "call_id": "a", "name": "native", "arguments": "{}"}),
            json!({"type": "function_call_output", "call_id": "a", "output": "REMOVED_EXACT_SOURCE"}),
            json!({"role": "user", "content": "latest goal"}),
        ];
        state.ingest(&input).unwrap();
        for item in &mut state.history {
            item.exposed = true;
        }
        let groups = state.groups();
        state.observe_groups(&groups);
        state.apply_advice(&groups, &json!({"protected": ["g1"], "removable": ["g2"], "memories": [{"source_ids": ["g2"], "text": "durable learning"}]})).unwrap();
        state.remove(&[2], &groups);
        let shadow = serde_json::to_string(&state.shadow_input()).unwrap();
        assert!(!shadow.contains("REMOVED_EXACT_SOURCE"));
        assert!(state.active_shadow.iter().all(|record| {
            let data: Value =
                serde_json::from_str(record.value["content"].as_str().unwrap()).unwrap();
            data["group_id"] != "g2"
        }));
        let facts = state
            .active_shadow
            .iter()
            .filter_map(|record| {
                let data: Value = serde_json::from_str(record.value["content"].as_str()?).ok()?;
                (data["text"] == "durable learning").then_some(data)
            })
            .collect::<Vec<_>>();
        assert_eq!(facts[0]["source_ids"], json!(["g2"]));
        assert!(shadow.contains("retained requirement"));
        assert!(shadow.contains("durable learning"));
        let main = serde_json::to_string(&state.render_primary()).unwrap();
        assert!(main.contains("durable learning"));
        state.ingest(&input).unwrap();
        state.observe_groups(&state.groups());
        assert!(
            !serde_json::to_string(&state.shadow_input())
                .unwrap()
                .contains("REMOVED_EXACT_SOURCE")
        );
        assert!(
            !serde_json::to_string(&state.render_primary())
                .unwrap()
                .contains("REMOVED_EXACT_SOURCE")
        );
    }
}
