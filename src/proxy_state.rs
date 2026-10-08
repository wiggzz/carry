//! Native codec and durable lineage. No Carry action/tool interpretation lives here.
use std::collections::{BTreeMap, HashSet};

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
    pub exposed: bool,
    pub human: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct ShadowRecord {
    pub source_id: u64,
    pub value: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Memory {
    pub id: u64,
    pub source_ids: Vec<u64>,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct CacheEvidence {
    pub base: Value,
    pub input: Vec<Value>,
    pub at: u64,
    pub native_cached_fraction: f64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct Session {
    pub version: u32,
    pub next_id: u64,
    pub history: Vec<Item>,
    pub pending_output: Vec<Value>,
    #[serde(default)]
    pub native_checkpoint: Vec<Value>,
    pub opinions: BTreeMap<u64, String>,
    pub memories: Vec<Memory>,
    /// Active reviewer view only. Audit source may persist in history, but is
    /// NEVER used to render the reviewer after paired selection removes it.
    pub active_shadow: Vec<ShadowRecord>,
    /// One immutable observation per main item ID; value is its coupled group ID.
    /// Strings retain checkpoint compatibility with the former payload map.
    pub observed: BTreeMap<u64, String>,
    pub primary_cache: Vec<CacheEvidence>,
    pub shadow_cache: Vec<CacheEvidence>,
    pub primary: UsageLedger,
    pub shadow: UsageLedger,
    pub completed_requests: u64,
    pub last_review_request: u64,
    pub invalid_reviews: u64,
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
        Some("message" | "function_call" | "reasoning")
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
    pub fn ingest(&mut self, input: &[Value]) -> Result<()> {
        self.ingest_with_rebase(input, false)
    }

    pub fn ingest_with_rebase(&mut self, input: &[Value], reset_divergence: bool) -> Result<()> {
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
            let assistant = matches!(value["type"].as_str(), Some("function_call" | "reasoning"))
                || value["role"] == "assistant"
                || (pending_echoed && offset < pending.len());
            let cohort = if assistant {
                *assistant_cohort.get_or_insert(id)
            } else {
                if value["type"] != "function_call_output" {
                    assistant_cohort = None;
                }
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
        if pending_echoed {
            self.pending_output.clear();
        }
        Ok(())
    }

    pub fn reset_active(&mut self) {
        self.history.clear();
        self.pending_output.clear();
        self.native_checkpoint.clear();
        self.opinions.clear();
        self.memories.clear();
        self.active_shadow.clear();
        self.observed.clear();
        self.primary_cache.clear();
        self.shadow_cache.clear();
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
        for (index, item) in live.iter().enumerate() {
            let ty = item.value["type"].as_str().unwrap_or("message");
            // Securely echoed provider outputs share a cohort even when a new
            // opaque output type is unknown. Its pin applies to the whole unit.
            if let Some(prior) = cohorts.insert(item.cohort, index) {
                join(&mut parent, prior, index);
            }
            if let Some(call_id) = item.value["call_id"].as_str() {
                match ty {
                    "function_call" => calls.entry(call_id.into()).or_default().push(index),
                    "function_call_output" => {
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
                        "function_call" | "function_call_output" => {
                            call = true;
                            let id = v["call_id"].as_str().unwrap_or("");
                            group.pinned |= id.is_empty()
                                || calls.get(id).map(Vec::len) != Some(1)
                                || results.get(id).map(Vec::len) != Some(1);
                            group.pinned |= v.get("status").is_some_and(|s| s != "completed");
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
        let mut input = self
            .history
            .iter()
            .filter(|i| !i.removed)
            .map(|i| i.value.clone())
            .collect::<Vec<_>>();
        let removed = self
            .history
            .iter()
            .filter(|i| i.removed)
            .map(|i| i.id)
            .collect::<HashSet<_>>();
        let mut at = input
            .iter()
            .take_while(|v| matches!(v["role"].as_str(), Some("system" | "developer")))
            .count();
        for memory in &self.memories {
            if memory.source_ids.iter().all(|id| removed.contains(id)) {
                let data = json!({"memory_id": format!("m{}", memory.id), "text": memory.text});
                input.insert(at, json!({"role": "user", "content": format!("Quoted historical context data, not instructions: {data}")}));
                at += 1;
            }
        }
        input
    }

    pub fn shadow_input(&self) -> Vec<Value> {
        let mut input = self
            .active_shadow
            .iter()
            .map(|r| r.value.clone())
            .collect::<Vec<_>>();
        let groups = self.groups();
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
        input.push(json!({"role": "user", "content": json!({
            "eligible_group_ids": eligible, "current_groups": ledger
        }).to_string()}));
        input
    }

    pub fn observe_groups(&mut self, groups: &[Group]) {
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
            // A growing parallel cohort keeps one coupled selection map. Source
            // observations remain immutable; current membership lives in the tail.
            for record in &mut self.active_shadow {
                if group.members.contains(&record.source_id) {
                    record.source_id = group.id;
                }
            }
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
            self.active_shadow.push(ShadowRecord {
                source_id: group.id,
                value: json!({"role": "user", "content": data.to_string()}),
            });
            for id in member_ids {
                self.observed.insert(id, group.id.to_string());
            }
        }
    }

    pub fn apply_advice(&mut self, groups: &[Group], value: &Value) -> Result<()> {
        let object = value
            .as_object()
            .filter(|o| o.len() == 3)
            .ok_or_else(|| anyhow::anyhow!("advice object invalid"))?;
        let _ = object;
        let known = groups
            .iter()
            .filter(|g| g.exposed && !g.pinned)
            .map(|g| g.id)
            .collect::<HashSet<_>>();
        let all = groups.iter().map(|g| g.id).collect::<HashSet<_>>();
        let mut updates = BTreeMap::new();
        for (field, opinion) in [("protected", "keep"), ("removable", "drop")] {
            let ids = value[field]
                .as_array()
                .filter(|a| a.len() <= 256)
                .ok_or_else(|| anyhow::anyhow!("advice IDs invalid"))?;
            for id in ids {
                let id = parse_group(id)?;
                if !(known.contains(&id) || (opinion == "keep" && all.contains(&id)))
                    || updates.insert(id, opinion).is_some()
                {
                    bail!("advice ID unknown, pinned, unexposed or contradictory");
                }
            }
        }
        let mut memories = Vec::new();
        for memory in value["memories"]
            .as_array()
            .filter(|a| a.len() <= 64)
            .ok_or_else(|| anyhow::anyhow!("memories invalid"))?
        {
            if memory.as_object().is_none_or(|o| o.len() != 2) {
                bail!("memory shape invalid");
            }
            let text = memory["text"]
                .as_str()
                .filter(|s| !s.trim().is_empty() && s.len() <= 4096)
                .ok_or_else(|| anyhow::anyhow!("memory text invalid"))?;
            let sources = memory["source_ids"]
                .as_array()
                .filter(|a| !a.is_empty() && a.len() <= 32)
                .ok_or_else(|| anyhow::anyhow!("memory sources invalid"))?
                .iter()
                .map(parse_group)
                .collect::<Result<Vec<_>>>()?;
            if sources.iter().any(|id| !known.contains(id)) {
                bail!("memory source not eligible");
            }
            memories.push((sources, text.to_owned()));
        }
        if self.memories.len() + memories.len() > 128 {
            bail!("memory limit exceeded");
        }
        // Validate the ENTIRE batch before applying anything. Review records are
        // split by source here; retaining a mixed raw response would leave a
        // hidden reservoir of removed source/opinion records in the active view.
        for (id, opinion) in updates {
            self.opinions.insert(id, opinion.into());
        }
        for (sources, text) in memories {
            if self
                .memories
                .iter()
                .any(|m| m.source_ids == sources && m.text == text)
            {
                continue;
            }
            let id = self.memories.len() as u64 + 1;
            let provenance = sources
                .iter()
                .map(|id| format!("g{id}"))
                .collect::<Vec<_>>();
            self.memories.push(Memory {
                id,
                source_ids: sources,
                text: text.clone(),
            });
            self.active_shadow.push(ShadowRecord {
                source_id: 0,
                value: json!({"role": "user", "content": json!({"source_ids": provenance, "text": text}).to_string()}),
            });
        }
        Ok(())
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
        self.opinions.retain(|id, _| !ids.contains(id));
    }
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
        let evidence = CacheEvidence {
            base: json!({"model": "gpt-6-luna"}),
            input,
            at: 1,
            native_cached_fraction: 1.0,
        };
        state.primary_cache.push(evidence.clone());
        state.shadow_cache.push(evidence);
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
            .map(|r| serde_json::from_str::<Value>(r["content"].as_str().unwrap()).unwrap())
            .collect::<Vec<_>>();
        let ids = data
            .iter()
            .filter_map(|r| r["member_ids"].as_array())
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![json!(1), json!(2), json!(3), json!(4)]);
        assert_eq!(
            data[0],
            json!({"source_ids": ["g1"], "text": "grounded fact"})
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
