use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

use crate::protocol::ContextManagement;

const ESTIMATED_BYTES_PER_TOKEN: usize = 4;
const CACHE_READ_RATE: f64 = 0.10;
const CACHE_WRITE_RATE: f64 = 1.25;
const NEUTRAL_RECENCY_SCORE_SCALE: u64 = 1_000_000;
const NEUTRAL_TARGET_NUMERATOR: usize = 3;
const NEUTRAL_TARGET_DENOMINATOR: usize = 4;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Retention {
    Stable,
    Volatile,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RetentionSignal {
    Neutral,
    Keep,
    Drop,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ContextItemKind {
    User,
    Memory,
    Tool,
}

impl Retention {
    fn label(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Volatile => "volatile",
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct ContextItem {
    pub id: u64,
    pub kind: ContextItemKind,
    pub retention: Retention,
    pub signal: RetentionSignal,
    pub bytes: usize,
    pub input_items: Vec<Value>,
    memory: Option<MemoryData>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
struct MemoryData {
    content: String,
    source_id: u64,
    materialized: bool,
}

impl ContextItem {
    fn user(id: u64, content: String) -> Self {
        Self::new(
            id,
            ContextItemKind::User,
            Retention::Stable,
            vec![json!({
                "role": "user",
                "content": [{ "type": "input_text", "text": content }]
            })],
        )
    }

    fn memory(id: u64, source_id: u64, content: String) -> Self {
        let mut item = Self::new(id, ContextItemKind::Memory, Retention::Stable, Vec::new());
        item.bytes = content.len();
        item.memory = Some(MemoryData {
            content,
            source_id,
            materialized: false,
        });
        item
    }

    pub fn tool(id: u64, output_items: Vec<Value>, function_call_output: Value) -> Result<Self> {
        if function_call_output["type"].as_str() != Some("function_call_output") {
            bail!("tool result item is not a function_call_output");
        }
        let call_id = function_call_output["call_id"]
            .as_str()
            .context("function call output has no call_id")?;
        if !output_items.iter().any(|item| {
            item["type"].as_str() == Some("function_call")
                && item["call_id"].as_str() == Some(call_id)
        }) {
            bail!("response output has no matching function call");
        }
        let mut input_items = output_items;
        input_items.push(function_call_output);
        Ok(Self::new(
            id,
            ContextItemKind::Tool,
            Retention::Volatile,
            input_items,
        ))
    }

    fn new(id: u64, kind: ContextItemKind, retention: Retention, input_items: Vec<Value>) -> Self {
        Self {
            id,
            kind,
            retention,
            signal: RetentionSignal::Neutral,
            bytes: serialized_bytes(&input_items),
            input_items,
            memory: None,
        }
    }

    fn marker(&self, checkpoint: bool) -> Value {
        let mut block = json!({
            "type": "input_text",
            "text": format!("[context {} {}]", self.id, self.retention.label())
        });
        if checkpoint {
            block["prompt_cache_breakpoint"] = json!({ "mode": "explicit" });
        }
        json!({ "role": "developer", "content": [block] })
    }

    fn compact_marker(&self) -> String {
        format!("[context {} {}]", self.id, self.retention.label())
    }
}

fn serialized_bytes(items: &[Value]) -> usize {
    serde_json::to_vec(items).map_or(usize::MAX, |bytes| bytes.len())
}

#[derive(Clone, Debug)]
pub(crate) struct ContextState {
    items: Vec<ContextItem>,
    next_id: u64,
    generation: u64,
    max_read_breakpoints: usize,
    breakpoints: Vec<StoredBreakpoint>,
}

#[derive(Clone, Debug)]
struct StoredBreakpoint {
    generation: u64,
    item_ids: Vec<u64>,
    marker_frontiers: Vec<u64>,
    rendered_prefix: Vec<Value>,
}

impl ContextState {
    #[cfg(test)]
    pub fn new(initial_prompt: String) -> Self {
        Self::new_with_max_read_breakpoints(initial_prompt, usize::MAX)
    }

    pub fn new_with_max_read_breakpoints(
        initial_prompt: String,
        max_read_breakpoints: usize,
    ) -> Self {
        let items = vec![ContextItem::user(1, initial_prompt)];
        let rendered_prefix = Self::render_items(&items, &[1]);
        Self {
            items,
            next_id: 1,
            generation: 0,
            max_read_breakpoints,
            breakpoints: vec![StoredBreakpoint {
                generation: 0,
                item_ids: vec![1],
                marker_frontiers: vec![1],
                rendered_prefix,
            }],
        }
    }

    pub fn add_user(&mut self, content: String) -> u64 {
        let id = self.allocate_id();
        self.items.push(ContextItem::user(id, content));
        id
    }

    pub fn add_tool(
        &mut self,
        output_items: Vec<Value>,
        function_call_output: Value,
    ) -> Result<u64> {
        let id = self.allocate_id();
        self.items
            .push(ContextItem::tool(id, output_items, function_call_output)?);
        Ok(id)
    }

    fn allocate_id(&mut self) -> u64 {
        self.next_id = self.next_id.saturating_add(1);
        self.next_id
    }

    pub fn input_items(&self) -> Vec<Value> {
        self.render_with_compatible_breakpoints(&self.items)
    }

    fn render_items(items: &[ContextItem], breakpoint_frontiers: &[u64]) -> Vec<Value> {
        let present = items.iter().map(|item| item.id).collect::<HashSet<_>>();
        let mut input = Vec::new();
        for item in items {
            let checkpoint = breakpoint_frontiers.contains(&item.id);
            match item.kind {
                ContextItemKind::User => {
                    input.extend(item.input_items.iter().cloned());
                    input.push(item.marker(checkpoint));
                }
                ContextItemKind::Tool => {
                    let mut native = item.input_items.clone();
                    let output = native
                        .last_mut()
                        .expect("tool context always has a function output");
                    let mut annotated = output["output"].as_str().unwrap_or_default().to_owned();
                    annotated.push_str(&format!("\n{}", item.compact_marker()));
                    for memory in items.iter().filter(|candidate| {
                        candidate.memory.as_ref().is_some_and(|memory| {
                            memory.source_id == item.id && !memory.materialized
                        })
                    }) {
                        annotated
                            .push_str(&format!("\n\n[memory stored]\n{}", memory.compact_marker()));
                    }
                    output["output"] = Value::String(annotated);
                    input.extend(native);
                    if checkpoint {
                        input.push(cache_frontier_marker());
                    }
                }
                ContextItemKind::Memory => {
                    let memory = item.memory.as_ref().expect("memory metadata is present");
                    if memory.materialized || !present.contains(&memory.source_id) {
                        input.push(json!({
                            "role": "assistant",
                            "content": [{
                                "type": "output_text",
                                "text": format!("[memory]\n{}", memory.content)
                            }]
                        }));
                        input.push(item.marker(checkpoint));
                    } else if checkpoint {
                        input.push(cache_frontier_marker());
                    }
                }
            }
        }
        input
    }

    fn compatible_breakpoint_frontiers(&self, items: &[ContextItem]) -> Vec<u64> {
        let mut frontiers = self
            .breakpoints
            .iter()
            .filter(|breakpoint| {
                Self::render_items(items, &breakpoint.marker_frontiers)
                    .starts_with(&breakpoint.rendered_prefix)
            })
            .filter_map(|breakpoint| breakpoint.item_ids.last().copied())
            .collect::<Vec<_>>();
        if frontiers.len() > self.max_read_breakpoints {
            frontiers.drain(..frontiers.len() - self.max_read_breakpoints);
        }
        frontiers
    }

    fn render_with_compatible_breakpoints(&self, items: &[ContextItem]) -> Vec<Value> {
        Self::render_items(items, &self.compatible_breakpoint_frontiers(items))
    }

    pub fn snapshot(&self) -> Vec<&ContextItem> {
        self.items.iter().collect()
    }

    pub fn stable_frontier_len(&self) -> usize {
        self.items
            .iter()
            .take_while(|item| item.retention == Retention::Stable)
            .count()
    }

    pub fn stable_frontier_id(&self) -> Option<u64> {
        self.stable_frontier_len()
            .checked_sub(1)
            .and_then(|index| self.items.get(index))
            .map(|item| item.id)
    }

    pub fn retained_bytes(&self) -> usize {
        self.items.iter().map(|item| item.bytes).sum()
    }

    pub fn estimated_tokens(&self) -> usize {
        estimated_tokens(&self.input_items())
    }

    #[cfg(test)]
    fn item_estimated_tokens(&self, id: u64) -> usize {
        self.items
            .iter()
            .find(|item| item.id == id)
            .map_or(0, |item| item.bytes.div_ceil(ESTIMATED_BYTES_PER_TOKEN))
    }

    pub fn rendered_breakpoints(&self) -> Vec<RenderedBreakpoint> {
        let rendered = self.input_items();
        self.breakpoints
            .iter()
            .filter(|breakpoint| rendered.starts_with(&breakpoint.rendered_prefix))
            .map(|breakpoint| RenderedBreakpoint {
                generation: breakpoint.generation,
                prefix_tokens: estimated_tokens(&breakpoint.rendered_prefix),
            })
            .collect()
    }

    pub fn record_signals(&mut self, update: &ContextManagement, source_id: u64) -> SignalChange {
        let mut keep = Vec::new();
        let mut drop = Vec::new();
        let mut ignored = Vec::new();

        // Apply drops first so a contradictory same-turn keep wins conservatively.
        for id in unique_ids(&update.drop) {
            match self.items.iter_mut().find(|item| item.id == id) {
                Some(item) => {
                    item.signal = RetentionSignal::Drop;
                    drop.push(id);
                }
                None => ignored.push(id),
            }
        }
        for id in unique_ids(&update.keep) {
            match self.items.iter_mut().find(|item| item.id == id) {
                Some(item) => {
                    item.signal = RetentionSignal::Keep;
                    keep.push(id);
                }
                None => ignored.push(id),
            }
        }

        let mut added = Vec::new();
        for content in update
            .remember
            .iter()
            .map(|content| content.trim())
            .filter(|content| !content.is_empty())
        {
            let id = self.allocate_id();
            self.items
                .push(ContextItem::memory(id, source_id, content.to_owned()));
            added.push(id);
        }

        SignalChange {
            keep,
            drop,
            ignored: unique_ids(&ignored),
            added,
        }
    }

    #[cfg(test)]
    pub fn plan_compaction(
        &self,
        protected: &[u64],
        policy: CompactionPolicy,
    ) -> Option<CompactionPlan> {
        self.plan_compaction_with_neutral_budget(protected, policy, 0)
    }

    pub fn plan_compaction_with_neutral_budget(
        &self,
        protected: &[u64],
        policy: CompactionPolicy,
        neutral_budget_tokens: usize,
    ) -> Option<CompactionPlan> {
        let protected = protected.iter().copied().collect::<HashSet<_>>();
        let newest_id = self.items.last().map_or(0, |item| item.id);
        // Explicit keep/drop signals remain authoritative. Neutral volatile items compete for
        // a separate automatic budget using a monotone recency score; the shape can later gain
        // other evidence without changing the packing or telemetry contract.
        let mut neutral = self
            .items
            .iter()
            .filter(|item| {
                item.retention == Retention::Volatile && item.signal == RetentionSignal::Neutral
            })
            .map(|item| NeutralRetentionDecision {
                id: item.id,
                tokens: item.bytes.div_ceil(ESTIMATED_BYTES_PER_TOKEN),
                score: NEUTRAL_RECENCY_SCORE_SCALE
                    / newest_id.saturating_sub(item.id).saturating_add(1),
            })
            .collect::<Vec<_>>();
        neutral.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| right.id.cmp(&left.id))
        });

        let neutral_total_tokens = neutral
            .iter()
            .map(|decision| decision.tokens)
            .fold(0usize, usize::saturating_add);
        // Only cross the neutral high-water mark before collecting neutral items, then compact
        // toward a lower target so one new result does not trigger another compaction next turn.
        let neutral_target_tokens = neutral_budget_tokens.saturating_mul(NEUTRAL_TARGET_NUMERATOR)
            / NEUTRAL_TARGET_DENOMINATOR;
        let neutral_over_budget = neutral_total_tokens > neutral_budget_tokens;
        let mut neutral_retained = Vec::new();
        let mut neutral_retained_tokens = neutral
            .iter()
            .filter(|decision| protected.contains(&decision.id))
            .map(|decision| decision.tokens)
            .fold(0usize, usize::saturating_add);
        let mut neutral_removable = HashSet::new();
        for decision in neutral {
            if protected.contains(&decision.id) {
                neutral_retained.push(decision);
            } else if !neutral_over_budget
                || neutral_retained_tokens.saturating_add(decision.tokens) <= neutral_target_tokens
            {
                neutral_retained_tokens = neutral_retained_tokens.saturating_add(decision.tokens);
                neutral_retained.push(decision);
            } else {
                neutral_removable.insert(decision.id);
            }
        }

        let removable = self
            .items
            .iter()
            .filter(|item| {
                !protected.contains(&item.id)
                    && (item.signal == RetentionSignal::Drop
                        || neutral_removable.contains(&item.id))
            })
            .map(|item| item.id)
            .collect::<Vec<_>>();

        let mut candidates = Vec::new();
        if !removable.is_empty() {
            candidates.push(self.compaction_candidate(
                removable.clone(),
                neutral_retained.clone(),
                neutral_budget_tokens,
                neutral_target_tokens,
                None,
                &policy,
            ));
        }
        for priced in &policy.breakpoints {
            let Some(stored) = self
                .breakpoints
                .iter()
                .find(|breakpoint| breakpoint.generation == priced.generation)
            else {
                continue;
            };
            if priced.cached_tokens == 0 {
                continue;
            }
            let dropped = removable
                .iter()
                .copied()
                .filter(|id| !stored.item_ids.contains(id))
                .collect::<Vec<_>>();
            if dropped.is_empty() {
                continue;
            }
            let candidate = self.compaction_candidate(
                dropped,
                neutral_retained.clone(),
                neutral_budget_tokens,
                neutral_target_tokens,
                Some(priced),
                &policy,
            );
            if candidate.reused_generation == Some(priced.generation) {
                candidates.push(candidate);
            }
        }

        candidates
            .into_iter()
            .filter(|plan| plan.estimated_savings_input_units > 0.0)
            .max_by(|left, right| {
                left.estimated_savings_input_units
                    .total_cmp(&right.estimated_savings_input_units)
            })
    }

    fn compaction_candidate(
        &self,
        dropped: Vec<u64>,
        neutral_retained: Vec<NeutralRetentionDecision>,
        neutral_budget_tokens: usize,
        neutral_target_tokens: usize,
        reused: Option<&PricedBreakpoint>,
        policy: &CompactionPolicy,
    ) -> CompactionPlan {
        let dropped_set = dropped.iter().copied().collect::<HashSet<_>>();
        let neutral_retained_set = neutral_retained
            .iter()
            .map(|decision| decision.id)
            .collect::<HashSet<_>>();
        let current_tokens = self.estimated_tokens();
        let mut retained = self
            .items
            .iter()
            .filter(|item| !dropped_set.contains(&item.id))
            .cloned()
            .collect::<Vec<_>>();
        for item in &mut retained {
            if !neutral_retained_set.contains(&item.id) {
                item.retention = Retention::Stable;
            }
        }
        let retained_rendered = self.render_with_compatible_breakpoints(&retained);
        let retained_tokens = estimated_tokens(&retained_rendered);
        let dropped_tokens = current_tokens.saturating_sub(retained_tokens);
        let reused_generation = reused.and_then(|priced| {
            let stored = self
                .breakpoints
                .iter()
                .find(|breakpoint| breakpoint.generation == priced.generation)?;
            retained_rendered
                .starts_with(&stored.rendered_prefix)
                .then_some(priced.generation)
        });
        let reused_tokens = reused
            .filter(|priced| reused_generation == Some(priced.generation))
            .and_then(|priced| {
                let prefix_tokens = self
                    .breakpoints
                    .iter()
                    .find(|stored| stored.generation == priced.generation)
                    .map(|stored| estimated_tokens(&stored.rendered_prefix))?;
                Some(priced.cached_tokens.min(prefix_tokens).min(retained_tokens))
            })
            .unwrap_or_default();
        let rewrite_tokens = retained_tokens.saturating_sub(reused_tokens);
        let implicit_cached_tokens = policy.implicit_cached_tokens.min(current_tokens);
        let baseline = implicit_cached_tokens as f64 * CACHE_READ_RATE
            + current_tokens.saturating_sub(implicit_cached_tokens) as f64 * CACHE_WRITE_RATE;
        let candidate_first =
            reused_tokens as f64 * CACHE_READ_RATE + rewrite_tokens as f64 * CACHE_WRITE_RATE;
        let invalidated_generations = policy
            .breakpoints
            .iter()
            .filter(|priced| {
                self.breakpoints
                    .iter()
                    .find(|stored| stored.generation == priced.generation)
                    .is_none_or(|stored| !retained_rendered.starts_with(&stored.rendered_prefix))
            })
            .map(|priced| priced.generation)
            .collect::<Vec<_>>();
        let invalidated_cache_tokens = policy
            .breakpoints
            .iter()
            .filter(|priced| invalidated_generations.contains(&priced.generation))
            .map(|priced| priced.cached_tokens)
            .sum();

        CompactionPlan {
            dropped,
            neutral_retained,
            neutral_budget_tokens,
            neutral_target_tokens,
            dropped_tokens,
            retained_tokens,
            rewrite_tokens,
            reused_generation,
            considered_generations: policy
                .breakpoints
                .iter()
                .map(|breakpoint| breakpoint.generation)
                .collect(),
            invalidated_generations,
            invalidated_cache_tokens,
            estimated_savings_input_units: baseline - candidate_first,
        }
    }

    pub fn compact(&mut self, plan: CompactionPlan) -> ContextChange {
        let dropped = plan.dropped.iter().copied().collect::<HashSet<_>>();
        let neutral_retained = plan
            .neutral_retained
            .iter()
            .map(|decision| decision.id)
            .collect::<HashSet<_>>();
        self.items.retain(|item| !dropped.contains(&item.id));
        for item in &mut self.items {
            if let Some(memory) = item.memory.as_mut()
                && dropped.contains(&memory.source_id)
            {
                memory.materialized = true;
            }
            if neutral_retained.contains(&item.id) {
                item.retention = Retention::Volatile;
            } else {
                item.retention = Retention::Stable;
            }
            if item.signal == RetentionSignal::Keep && item.retention == Retention::Stable {
                item.signal = RetentionSignal::Neutral;
            }
        }
        self.generation = self.generation.saturating_add(1);
        let item_ids = self.items.iter().map(|item| item.id).collect::<Vec<_>>();
        let mut frontiers = self.compatible_breakpoint_frontiers(&self.items);
        if let Some(frontier) = item_ids.last().copied() {
            frontiers.push(frontier);
        }
        let rendered_prefix = Self::render_items(&self.items, &frontiers);
        self.breakpoints.push(StoredBreakpoint {
            generation: self.generation,
            item_ids,
            marker_frontiers: frontiers,
            rendered_prefix,
        });

        ContextChange {
            dropped: plan.dropped,
            neutral_retained: plan.neutral_retained,
            neutral_budget_tokens: plan.neutral_budget_tokens,
            neutral_target_tokens: plan.neutral_target_tokens,
            dropped_tokens: plan.dropped_tokens,
            retained_tokens: plan.retained_tokens,
            rewrite_tokens: plan.rewrite_tokens,
            reused_generation: plan.reused_generation,
            considered_generations: plan.considered_generations,
            invalidated_generations: plan.invalidated_generations,
            invalidated_cache_tokens: plan.invalidated_cache_tokens,
            estimated_savings_input_units: plan.estimated_savings_input_units,
            generation: self.generation,
            stable_frontier: self.stable_frontier_id(),
            retained_bytes: self.retained_bytes(),
        }
    }

    #[cfg(test)]
    fn signal_for(&self, id: u64) -> Option<RetentionSignal> {
        self.items
            .iter()
            .find(|item| item.id == id)
            .map(|item| item.signal)
    }

    #[cfg(test)]
    fn force_stable_for_test(&mut self) {
        for item in &mut self.items {
            item.retention = Retention::Stable;
        }
    }
}

fn unique_ids(ids: &[u64]) -> Vec<u64> {
    let mut seen = HashSet::new();
    ids.iter().copied().filter(|id| seen.insert(*id)).collect()
}

fn cache_frontier_marker() -> Value {
    json!({
        "role": "developer",
        "content": [{
            "type": "input_text",
            "text": "[cache frontier]",
            "prompt_cache_breakpoint": { "mode": "explicit" }
        }]
    })
}

fn estimated_tokens(items: &[Value]) -> usize {
    serialized_bytes(items).div_ceil(ESTIMATED_BYTES_PER_TOKEN)
}

#[derive(Clone, Debug)]
pub(crate) struct CompactionPolicy {
    pub implicit_cached_tokens: usize,
    pub breakpoints: Vec<PricedBreakpoint>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PricedBreakpoint {
    pub generation: u64,
    pub cached_tokens: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RenderedBreakpoint {
    pub generation: u64,
    pub prefix_tokens: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CompactionPlan {
    pub dropped: Vec<u64>,
    pub neutral_retained: Vec<NeutralRetentionDecision>,
    pub neutral_budget_tokens: usize,
    pub neutral_target_tokens: usize,
    pub dropped_tokens: usize,
    pub retained_tokens: usize,
    pub rewrite_tokens: usize,
    pub reused_generation: Option<u64>,
    pub considered_generations: Vec<u64>,
    pub invalidated_generations: Vec<u64>,
    pub invalidated_cache_tokens: usize,
    pub estimated_savings_input_units: f64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct NeutralRetentionDecision {
    pub id: u64,
    pub tokens: usize,
    pub score: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct SignalChange {
    pub keep: Vec<u64>,
    pub drop: Vec<u64>,
    pub ignored: Vec<u64>,
    pub added: Vec<u64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ContextChange {
    pub dropped: Vec<u64>,
    pub neutral_retained: Vec<NeutralRetentionDecision>,
    pub neutral_budget_tokens: usize,
    pub neutral_target_tokens: usize,
    pub dropped_tokens: usize,
    pub retained_tokens: usize,
    pub rewrite_tokens: usize,
    pub reused_generation: Option<u64>,
    pub considered_generations: Vec<u64>,
    pub invalidated_generations: Vec<u64>,
    pub invalidated_cache_tokens: usize,
    pub estimated_savings_input_units: f64,
    pub generation: u64,
    pub stable_frontier: Option<u64>,
    pub retained_bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_cache_capabilities_render_no_explicit_breakpoints() {
        let state = ContextState::new_with_max_read_breakpoints("initial".into(), 0);
        assert!(state.rendered_breakpoints().is_empty());
        assert!(state.input_items().iter().all(|item| {
            item.get("content")
                .and_then(Value::as_array)
                .is_none_or(|content| {
                    content
                        .iter()
                        .all(|part| part.get("prompt_cache_breakpoint").is_none())
                })
        }));
    }

    fn update(keep: &[u64], drop: &[u64], remember: &[&str]) -> ContextManagement {
        ContextManagement {
            keep: keep.to_vec(),
            drop: drop.to_vec(),
            remember: remember.iter().map(|value| (*value).into()).collect(),
        }
    }

    fn add_tool(state: &mut ContextState) -> u64 {
        add_tool_with_output(state, "exact output")
    }

    fn add_tool_with_output(state: &mut ContextState, output: &str) -> u64 {
        let next = state.next_id + 1;
        state
            .add_tool(
                vec![json!({
                    "type": "function_call",
                    "call_id": format!("call-{next}"),
                    "name": "shell",
                    "arguments": "{}"
                })],
                json!({
                    "type": "function_call_output",
                    "call_id": format!("call-{next}"),
                    "output": output
                }),
            )
            .unwrap()
    }

    #[test]
    fn neutral_budget_retains_the_highest_recency_scores() {
        let mut state = ContextState::new("initial".into());
        let oldest = add_tool_with_output(&mut state, &"old ".repeat(100));
        let middle = add_tool_with_output(&mut state, &"middle ".repeat(100));
        let newest = add_tool_with_output(&mut state, &"new ".repeat(100));
        let retained_target =
            state.item_estimated_tokens(middle) + state.item_estimated_tokens(newest);
        let budget = retained_target.saturating_mul(4).div_ceil(3);

        let plan = state
            .plan_compaction_with_neutral_budget(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                },
                budget,
            )
            .unwrap();

        assert_eq!(plan.dropped, vec![oldest]);
        assert_eq!(
            plan.neutral_retained
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![newest, middle]
        );
        assert!(plan.neutral_retained[0].score > plan.neutral_retained[1].score);
    }

    #[test]
    fn budget_retained_neutral_items_remain_volatile_for_future_scoring() {
        let mut state = ContextState::new("initial".into());
        let oldest = add_tool_with_output(&mut state, &"old ".repeat(100));
        let middle = add_tool_with_output(&mut state, &"middle ".repeat(100));
        let newest = add_tool_with_output(&mut state, &"new ".repeat(100));
        let retained_target =
            state.item_estimated_tokens(middle) + state.item_estimated_tokens(newest);
        let budget = retained_target.saturating_mul(4).div_ceil(3);

        let plan = state
            .plan_compaction_with_neutral_budget(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                },
                budget,
            )
            .unwrap();
        assert_eq!(plan.dropped, vec![oldest]);
        state.compact(plan);

        for id in [middle, newest] {
            assert_eq!(
                state
                    .snapshot()
                    .iter()
                    .find(|item| item.id == id)
                    .unwrap()
                    .retention,
                Retention::Volatile
            );
        }

        let latest = add_tool_with_output(&mut state, &"latest ".repeat(100));
        let next = state
            .plan_compaction_with_neutral_budget(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                },
                budget,
            )
            .unwrap();
        assert_eq!(next.dropped, vec![middle]);
        assert_eq!(
            next.neutral_retained
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![latest, newest]
        );
    }

    #[test]
    fn crossing_the_neutral_budget_compacts_to_a_lower_target() {
        let mut state = ContextState::new("initial".into());
        let oldest = add_tool_with_output(&mut state, &"same ".repeat(100));
        let middle = add_tool_with_output(&mut state, &"same ".repeat(100));
        let newest = add_tool_with_output(&mut state, &"same ".repeat(100));
        let each = state.item_estimated_tokens(newest);
        let budget = each * 2 + each / 2;

        let plan = state
            .plan_compaction_with_neutral_budget(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                },
                budget,
            )
            .unwrap();

        assert_eq!(plan.neutral_target_tokens, budget * 3 / 4);
        assert_eq!(plan.dropped, vec![oldest, middle]);
        assert_eq!(
            plan.neutral_retained
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![newest]
        );
    }

    #[test]
    fn budget_candidate_prices_the_same_volatile_markers_it_will_commit() {
        for padding in 0..16 {
            let mut state = ContextState::new("initial".into());
            let oldest = add_tool_with_output(&mut state, &"old ".repeat(100));
            let newest = add_tool_with_output(&mut state, &"x".repeat(padding));
            let newest_tokens = state.item_estimated_tokens(newest);
            let budget = newest_tokens.saturating_mul(4).div_ceil(3);

            let plan = state
                .plan_compaction_with_neutral_budget(
                    &[],
                    CompactionPolicy {
                        implicit_cached_tokens: 0,
                        breakpoints: Vec::new(),
                    },
                    budget,
                )
                .unwrap();
            assert_eq!(plan.dropped, vec![oldest]);

            let retained = state
                .items
                .iter()
                .filter(|item| !plan.dropped.contains(&item.id))
                .cloned()
                .collect::<Vec<_>>();
            let expected = estimated_tokens(&state.render_with_compatible_breakpoints(&retained));
            assert_eq!(
                plan.retained_tokens, expected,
                "padding {padding} must price the volatile marker committed by compact"
            );
        }
    }

    #[test]
    fn explicit_drop_is_collectable_inside_the_neutral_budget() {
        let mut state = ContextState::new("initial".into());
        let dropped = add_tool(&mut state);
        state.record_signals(&update(&[], &[dropped], &[]), dropped);

        let plan = state
            .plan_compaction_with_neutral_budget(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                },
                usize::MAX,
            )
            .unwrap();

        assert_eq!(plan.dropped, vec![dropped]);
        assert!(plan.neutral_retained.is_empty());
    }

    #[test]
    fn stable_human_message_below_volatile_item_does_not_move_the_frontier() {
        let mut state = ContextState::new("initial".into());
        let tool = add_tool(&mut state);
        let steering = state.add_user("steer here".into());

        assert_eq!(state.stable_frontier_len(), 1);
        let rendered = state.input_items();
        let texts = rendered
            .iter()
            .filter_map(|item| item["content"][0]["text"].as_str())
            .collect::<Vec<_>>();
        assert!(texts.contains(&"[context 1 stable]"));
        assert!(rendered.iter().any(|item| {
            item["type"] == "function_call_output"
                && item["output"]
                    .as_str()
                    .is_some_and(|output| output.ends_with(&format!("[context {tool} volatile]")))
        }));
        assert!(texts.contains(&format!("[context {steering} stable]").as_str()));
        assert_eq!(
            rendered
                .iter()
                .filter(|item| item["content"][0].get("prompt_cache_breakpoint").is_some())
                .count(),
            1
        );
    }

    #[test]
    fn rendered_context_markers_expose_neutral_retention_defaults() {
        let mut state = ContextState::new("initial".into());
        let tool = add_tool(&mut state);
        let steering = state.add_user("steer here".into());

        let rendered = serde_json::to_string(&state.input_items()).unwrap();

        assert!(rendered.contains("[context 1 stable]"));
        assert!(rendered.contains(&format!("[context {tool} volatile]")));
        assert!(rendered.contains(&format!("[context {steering} stable]")));
    }

    #[test]
    fn memory_is_an_inline_stable_handle_without_duplicating_its_content() {
        let mut state = ContextState::new("initial".into());
        let tool = add_tool(&mut state);
        let change = state.record_signals(&update(&[], &[], &["durable outcome"]), tool);
        let memory = change.added[0];

        assert_eq!(
            state.items.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![1, tool, memory]
        );
        assert_eq!(state.items[2].retention, Retention::Stable);
        assert_eq!(state.stable_frontier_len(), 1);

        let rendered = state.input_items();
        let tool_output = rendered
            .iter()
            .find(|item| item["type"] == "function_call_output")
            .unwrap()["output"]
            .as_str()
            .unwrap();
        assert!(tool_output.contains(&format!("[context {tool} volatile]")));
        assert!(tool_output.contains("[memory stored]"));
        assert!(tool_output.contains(&format!("[context {memory} stable]")));
        assert!(!tool_output.contains("durable outcome"));
        assert!(!rendered.iter().any(|item| {
            item["role"] == "user"
                && item["content"][0]["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("[memory]"))
        }));
    }

    #[test]
    fn dropping_a_tool_materializes_its_stored_memory_with_the_same_memory_id() {
        let mut state = ContextState::new("initial".into());
        let tool = add_tool(&mut state);
        let memory = state
            .record_signals(&update(&[], &[tool], &["durable outcome"]), tool)
            .added[0];

        let plan = state
            .plan_compaction(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                },
            )
            .unwrap();
        state.compact(plan);

        let rendered = state.input_items();
        assert!(
            !rendered
                .iter()
                .any(|item| item["type"] == "function_call_output")
        );
        assert!(rendered.iter().any(|item| {
            item["role"] == "assistant"
                && item["content"][0]["type"] == "output_text"
                && item["content"][0]["text"] == "[memory]\ndurable outcome"
        }));
        assert!(
            rendered
                .iter()
                .any(|item| { item["content"][0]["text"] == format!("[context {memory} stable]") })
        );
    }

    #[test]
    fn planner_prices_memory_materialization_before_dropping_its_source() {
        let mut state = ContextState::new("initial".into());
        let tool = add_tool(&mut state);
        state.record_signals(&update(&[], &[tool], &[&"important ".repeat(2_000)]), tool);

        assert!(
            state
                .plan_compaction(
                    &[],
                    CompactionPolicy {
                        implicit_cached_tokens: 0,
                        breakpoints: Vec::new(),
                    },
                )
                .is_none()
        );
    }

    #[test]
    fn dropping_a_memory_with_its_source_does_not_materialize_it() {
        let mut state = ContextState::new("initial".into());
        let tool = add_tool(&mut state);
        let memory = state
            .record_signals(&update(&[], &[tool], &["temporary outcome"]), tool)
            .added[0];
        state.record_signals(&update(&[], &[memory], &[]), tool);

        let plan = state
            .plan_compaction(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                },
            )
            .unwrap();
        state.compact(plan);

        assert!(!state.input_items().iter().any(|item| {
            item["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("temporary outcome"))
        }));
    }

    #[test]
    fn tool_item_preserves_all_native_response_output_items() {
        let mut state = ContextState::new("initial".into());
        let call =
            json!({"type":"function_call","call_id":"call-2","name":"shell","arguments":"{}"});
        let reasoning = json!({"type":"reasoning","encrypted_content":"opaque"});
        let id = state
            .add_tool(
                vec![reasoning.clone(), call],
                json!({"type":"function_call_output","call_id":"call-2","output":"ok"}),
            )
            .unwrap();
        let item = state.items.iter().find(|item| item.id == id).unwrap();
        assert_eq!(item.input_items[0], reasoning);
        assert_eq!(item.input_items.len(), 3);
    }

    #[test]
    fn keep_and_drop_are_sticky_advice_until_compaction() {
        let mut state = ContextState::new("initial".into());
        let tool = add_tool(&mut state);

        state.record_signals(&update(&[], &[tool], &[]), tool);
        assert_eq!(state.signal_for(tool), Some(RetentionSignal::Drop));
        assert!(state.snapshot().iter().any(|item| item.id == tool));

        state.record_signals(&update(&[tool], &[], &[]), tool);
        assert_eq!(state.signal_for(tool), Some(RetentionSignal::Keep));
        assert!(state.snapshot().iter().any(|item| item.id == tool));
    }

    #[test]
    fn contradictory_and_unknown_signals_are_idempotent() {
        let mut state = ContextState::new("initial".into());
        let tool = add_tool(&mut state);

        let change = state.record_signals(&update(&[tool, 999], &[tool, 998], &[]), tool);

        assert_eq!(state.signal_for(tool), Some(RetentionSignal::Keep));
        assert_eq!(change.ignored, vec![998, 999]);
    }

    #[test]
    fn warm_compaction_requires_savings_on_the_next_request() {
        let mut state = ContextState::new("initial".into());
        let dropped = add_tool(&mut state);
        let retained = add_tool(&mut state);
        state.record_signals(&update(&[], &[dropped], &[]), retained);
        state.record_signals(&update(&[retained], &[], &[]), retained);

        let short = state.plan_compaction(
            &[],
            CompactionPolicy {
                implicit_cached_tokens: usize::MAX,
                breakpoints: Vec::new(),
            },
        );
        assert!(short.is_none());

        assert!(
            state
                .plan_compaction(
                    &[],
                    CompactionPolicy {
                        implicit_cached_tokens: usize::MAX,
                        breakpoints: Vec::new(),
                    },
                )
                .is_none()
        );
    }

    #[test]
    fn planner_selects_a_written_compatible_breakpoint_generation() {
        let mut state = ContextState::new("initial ".repeat(800));
        let dropped = add_tool(&mut state);
        let retained = add_tool(&mut state);
        state.record_signals(&update(&[], &[dropped], &[]), retained);
        state.record_signals(&update(&[retained], &[], &[]), retained);

        let plan = state
            .plan_compaction(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: vec![PricedBreakpoint {
                        generation: 0,
                        cached_tokens: 1_200,
                    }],
                },
            )
            .unwrap();

        assert_eq!(plan.reused_generation, Some(0));
        assert_eq!(plan.dropped, vec![dropped]);
        assert!(plan.rewrite_tokens < plan.retained_tokens);
    }

    #[test]
    fn neutral_volatile_items_are_removable_unless_kept() {
        let mut state = ContextState::new("initial".into());
        let disposable = add_tool(&mut state);

        let plan = state
            .plan_compaction(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                },
            )
            .unwrap();
        assert_eq!(plan.dropped, vec![disposable]);

        state.record_signals(&update(&[disposable], &[], &[]), disposable);
        assert!(
            state
                .plan_compaction(
                    &[],
                    CompactionPolicy {
                        implicit_cached_tokens: 0,
                        breakpoints: Vec::new(),
                    },
                )
                .is_none()
        );
    }

    #[test]
    fn expired_cache_applies_drop_signals_and_builds_a_new_generation() {
        let mut state = ContextState::new("initial".into());
        let dropped = add_tool(&mut state);
        let retained = add_tool(&mut state);
        state.record_signals(&update(&[retained], &[dropped], &[]), retained);

        let plan = state
            .plan_compaction(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                },
            )
            .unwrap();
        let change = state.compact(plan);

        assert_eq!(change.dropped, vec![dropped]);
        assert!(
            state
                .snapshot()
                .iter()
                .all(|item| item.retention == Retention::Stable)
        );
        assert_eq!(state.signal_for(retained), Some(RetentionSignal::Neutral));
    }

    #[test]
    fn compaction_can_remove_stable_drop_candidates() {
        let mut state = ContextState::new("initial".into());
        let old_tool = add_tool(&mut state);
        state.force_stable_for_test();
        let new_tool = add_tool(&mut state);
        state.record_signals(&update(&[], &[old_tool, new_tool], &[]), new_tool);

        let plan = state
            .plan_compaction(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                },
            )
            .unwrap();

        assert_eq!(plan.dropped, vec![old_tool, new_tool]);
    }

    #[test]
    fn later_generations_preserve_exact_compatible_older_breakpoints() {
        let mut state = ContextState::new("initial ".repeat(800));
        let first = state.input_items();
        let disposable = add_tool(&mut state);
        let retained = add_tool(&mut state);
        state.record_signals(&update(&[], &[disposable], &[]), retained);
        state.record_signals(&update(&[retained], &[], &[]), retained);
        let plan = state
            .plan_compaction(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                },
            )
            .unwrap();
        state.compact(plan);

        let later = state.input_items();
        assert_eq!(&later[..first.len()], first.as_slice());
        assert_eq!(
            later
                .iter()
                .filter(|item| item["content"][0].get("prompt_cache_breakpoint").is_some())
                .count(),
            2
        );
    }

    #[test]
    fn new_generation_does_not_embed_an_incompatible_older_breakpoint() {
        let mut state = ContextState::new("initial ".repeat(800));
        let disposable = add_tool(&mut state);
        let first_retained = add_tool(&mut state);
        state.record_signals(
            &update(&[first_retained], &[disposable], &[]),
            first_retained,
        );
        let first_plan = state
            .plan_compaction(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                },
            )
            .unwrap();
        state.compact(first_plan);

        let latest = add_tool(&mut state);
        state.record_signals(&update(&[latest], &[1], &[]), latest);
        let second_plan = state
            .plan_compaction(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                },
            )
            .unwrap();
        state.compact(second_plan);

        assert_eq!(
            state
                .input_items()
                .iter()
                .filter(|item| item["content"][0].get("prompt_cache_breakpoint").is_some())
                .count(),
            1
        );
        assert_eq!(
            state
                .rendered_breakpoints()
                .iter()
                .map(|breakpoint| breakpoint.generation)
                .collect::<Vec<_>>(),
            vec![2]
        );
    }
}
