use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::protocol::ContextManagement;

const ESTIMATED_BYTES_PER_TOKEN: usize = 4;
const CACHE_READ_RATE: f64 = 0.10;
const CACHE_WRITE_RATE: f64 = 1.25;
const COMPACTION_MIN_PAYBACK_RATIO: f64 = 0.10;
const NEUTRAL_RECENCY_SCORE_SCALE: u64 = 1_000_000;
const NEUTRAL_TARGET_NUMERATOR: usize = 3;
const NEUTRAL_TARGET_DENOMINATOR: usize = 4;
const HISTORY_COMPACTED_STATUS: &str =
    "[history status: earlier context has been removed by compaction]";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) enum Retention {
    #[serde(rename = "protected", alias = "stable")]
    Protected,
    #[serde(rename = "eligible", alias = "volatile")]
    Eligible,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RetentionSignal {
    Neutral,
    Keep,
    Drop,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ContextItemKind {
    User,
    Status,
    Memory,
    Tool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct ContextItem {
    pub id: u64,
    pub kind: ContextItemKind,
    pub retention: Retention,
    pub signal: RetentionSignal,
    pub bytes: usize,
    pub input_items: Vec<Value>,
    #[serde(default)]
    keep_lease_expires_at_turn: Option<u64>,
    #[serde(default)]
    keep_lease_expired: bool,
    /// Immutable sweep advisory rendered inside this completed tool result.
    #[serde(default)]
    keep_lease_review: Option<String>,
    memory: Option<MemoryData>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
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
            Retention::Protected,
            vec![json!({
                "role": "user",
                "content": [{ "type": "input_text", "text": content }]
            })],
        )
    }

    fn memory(id: u64, source_id: u64, content: String) -> Self {
        let mut item = Self::new(id, ContextItemKind::Memory, Retention::Eligible, Vec::new());
        item.bytes = content.len();
        item.memory = Some(MemoryData {
            content,
            source_id,
            materialized: false,
        });
        item
    }

    fn history_status(id: u64) -> Self {
        Self::new(
            id,
            ContextItemKind::Status,
            Retention::Eligible,
            vec![json!({
                "role": "developer",
                "content": [{ "type": "input_text", "text": HISTORY_COMPACTED_STATUS }]
            })],
        )
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
            Retention::Eligible,
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
            keep_lease_expires_at_turn: None,
            keep_lease_expired: false,
            keep_lease_review: None,
            memory: None,
        }
    }

    fn marker(&self, checkpoint: bool) -> Value {
        let mut block = json!({
            "type": "input_text",
            "text": format!("[context {}]", self.id)
        });
        if checkpoint {
            block["prompt_cache_breakpoint"] = json!({ "mode": "explicit" });
        }
        json!({ "role": "developer", "content": [block] })
    }

    fn compact_marker(&self) -> String {
        format!("[context {}]", self.id)
    }
}

fn serialized_bytes(items: &[Value]) -> usize {
    serde_json::to_vec(items).map_or(usize::MAX, |bytes| bytes.len())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ContextState {
    items: Vec<ContextItem>,
    next_id: u64,
    generation: u64,
    max_read_breakpoints: usize,
    breakpoints: Vec<StoredBreakpoint>,
    #[serde(default)]
    retention_turn: u64,
    #[serde(default)]
    pending_keep_lease_review: Vec<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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
            retention_turn: 0,
            pending_keep_lease_review: Vec::new(),
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let state: Self = serde_json::from_slice(bytes)?;
        let max_id = state.items.iter().map(|item| item.id).max().unwrap_or(0);
        if state.next_id < max_id {
            bail!("persisted context next_id is behind its item IDs");
        }
        Ok(state)
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

    pub fn advance_retention_turn(&mut self) {
        self.retention_turn = self.retention_turn.saturating_add(1);
    }

    pub fn retention_turn(&self) -> u64 {
        self.retention_turn
    }

    pub fn arm_keep_leases(&mut self, ids: &[u64], turns: u64) {
        if turns == 0 {
            return;
        }
        let expires_at_turn = self.retention_turn.saturating_add(turns);
        for id in unique_ids(ids) {
            if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
                item.keep_lease_expires_at_turn = Some(expires_at_turn);
                item.keep_lease_expired = false;
            }
        }
    }

    /// At a sweep, attach one immutable review to the newest completed tool block.
    /// The review is resolved only after the next model response has seen it.
    pub fn attach_due_keep_lease_review(&mut self) -> KeepLeaseReview {
        if !self.pending_keep_lease_review.is_empty() {
            return KeepLeaseReview {
                item_ids: Vec::new(),
            };
        }
        let item_ids = self
            .items
            .iter()
            .filter(|item| {
                item.signal != RetentionSignal::Drop
                    && item
                        .keep_lease_expires_at_turn
                        .is_some_and(|expires_at| expires_at <= self.retention_turn)
            })
            .map(|item| item.id)
            .collect::<Vec<_>>();
        if !item_ids.is_empty() {
            let ids = item_ids
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            let text = format!(
                "Previously protected items {ids} may be removed soon. If their learnings need to be preserved exactly, protect them. If there are learnings that can be summarized, remember those."
            );
            if let Some(item) = self
                .items
                .iter_mut()
                .rev()
                .find(|item| item.kind == ContextItemKind::Tool)
            {
                item.keep_lease_review = Some(text);
                self.pending_keep_lease_review = item_ids.clone();
            }
        }
        KeepLeaseReview { item_ids }
    }

    pub fn resolve_keep_lease_review(&mut self, renewed: &[u64]) -> Vec<u64> {
        let renewed = unique_ids(renewed).into_iter().collect::<HashSet<_>>();
        let mut expired = Vec::new();
        for id in std::mem::take(&mut self.pending_keep_lease_review) {
            let Some(item) = self.items.iter_mut().find(|item| item.id == id) else {
                continue;
            };
            if renewed.contains(&id) || item.signal == RetentionSignal::Drop {
                continue;
            }
            item.signal = RetentionSignal::Neutral;
            item.retention = Retention::Eligible;
            item.keep_lease_expires_at_turn = None;
            item.keep_lease_expired = true;
            expired.push(id);
        }
        expired
    }

    fn allocate_id(&mut self) -> u64 {
        self.next_id = self.next_id.saturating_add(1);
        self.next_id
    }

    pub fn input_items(&self) -> Vec<Value> {
        self.render_with_compatible_breakpoints(&self.items)
    }

    fn render_items(items: &[ContextItem], breakpoint_frontiers: &[u64]) -> Vec<Value> {
        Self::render_items_through(items, breakpoint_frontiers, None)
    }

    fn render_items_through(
        items: &[ContextItem],
        breakpoint_frontiers: &[u64],
        through: Option<u64>,
    ) -> Vec<Value> {
        let present = items.iter().map(|item| item.id).collect::<HashSet<_>>();
        let mut input = Vec::new();
        for item in items {
            let checkpoint = breakpoint_frontiers.contains(&item.id);
            match item.kind {
                ContextItemKind::User | ContextItemKind::Status => {
                    input.extend(item.input_items.iter().cloned());
                    input.push(item.marker(checkpoint));
                }

                ContextItemKind::Tool => {
                    let mut native = item.input_items.clone();
                    let output = native
                        .last_mut()
                        .expect("tool context always has a function output");
                    let mut annotated = output["output"].as_str().unwrap_or_default().to_owned();
                    if let Some(review) = &item.keep_lease_review {
                        annotated.push_str("\n\n");
                        annotated.push_str(review);
                    }
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
            if through == Some(item.id) {
                break;
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

    pub fn protected_frontier_len(&self) -> usize {
        self.items
            .iter()
            .take_while(|item| item.retention == Retention::Protected)
            .count()
    }

    pub fn protected_frontier_id(&self) -> Option<u64> {
        self.protected_frontier_len()
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

        // Human-authored items begin protected; every other kind begins eligible.
        // Explicit removable/protected signals override those defaults.
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
        // Explicit keep/drop signals remain authoritative. Neutral eligible items compete for
        // a separate automatic budget using a monotone recency score; the shape can later gain
        // other evidence without changing the packing or telemetry contract.
        let mut neutral = self
            .items
            .iter()
            .filter(|item| {
                item.retention == Retention::Eligible && item.signal == RetentionSignal::Neutral
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
            .filter(|plan| {
                meets_payback_threshold(
                    plan.estimated_savings_input_units,
                    plan.minimum_payback_input_units,
                )
            })
            .max_by(|left, right| {
                left.estimated_savings_input_units
                    .total_cmp(&right.estimated_savings_input_units)
            })
    }

    pub(crate) fn flat_rollout_estimate(
        &self,
        plan: &CompactionPlan,
        _protected: &[u64],
        policy: CompactionPolicy,
        config: FlatRolloutConfig,
    ) -> FlatRolloutEstimate {
        debug_assert!(config.samples > 0);
        debug_assert!(config.horizon > 0);
        let mut compacted = self.clone();
        compacted.compact(plan.clone());
        let retained_ids = compacted
            .items
            .iter()
            .filter(|item| item.kind != ContextItemKind::User)
            .map(|item| item.id)
            .collect::<Vec<_>>();
        let mean_virtual_item_tokens = compacted
            .items
            .iter()
            .filter(|item| item.kind != ContextItemKind::User)
            .map(|item| item.bytes.div_ceil(ESTIMATED_BYTES_PER_TOKEN))
            .sum::<usize>()
            .checked_div(retained_ids.len())
            .unwrap_or(1)
            .max(1);
        let initial_compact_cost = compact_request_cost(plan);
        let direct_next_request_savings_input_units = direct_next_request_savings(
            policy.implicit_cached_tokens,
            self.estimated_tokens(),
            initial_compact_cost,
        );
        let initial_keep_cost = keep_request_cost(self.estimated_tokens(), &policy);
        let mut total_compact_cost = 0.0;
        let mut total_keep_cost = 0.0;
        let mut max_sampled_drops = 0usize;

        for sample in 0..config.samples {
            let mut compact_branch = compacted.clone();
            let mut keep_branch = self.clone();
            let mut compact_cost = initial_compact_cost;
            let mut keep_cost = initial_keep_cost;
            let mut rng = FlatRolloutRng::new(config.seed ^ u64::from(sample));
            for _ in 1..config.horizon {
                let count = rng.range_inclusive(retained_ids.len().min(4));
                max_sampled_drops = max_sampled_drops.max(count);
                let dropped_ids = sample_ids_without_replacement(&retained_ids, count, &mut rng);
                compact_cost += simulate_rollout_turn(
                    &mut compact_branch,
                    &dropped_ids,
                    mean_virtual_item_tokens,
                    plan.neutral_budget_tokens,
                    &policy,
                );
                keep_cost += simulate_rollout_turn(
                    &mut keep_branch,
                    &dropped_ids,
                    mean_virtual_item_tokens,
                    plan.neutral_budget_tokens,
                    &policy,
                );
            }
            total_compact_cost += compact_cost;
            total_keep_cost += keep_cost;
        }

        let samples = f64::from(config.samples);
        let expected_compact_cost = total_compact_cost / samples;
        let expected_keep_cost = total_keep_cost / samples;
        FlatRolloutEstimate {
            samples: config.samples,
            horizon: config.horizon,
            seed: config.seed,
            mean_virtual_item_tokens,
            max_sampled_drops,
            expected_compact_cost,
            expected_keep_cost,
            direct_next_request_savings_input_units,
            expected_savings_input_units: expected_keep_cost - expected_compact_cost,
        }
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
                item.retention = Retention::Protected;
            }
        }
        if !retained
            .iter()
            .any(|item| item.kind == ContextItemKind::Status)
        {
            retained.push(ContextItem::history_status(self.next_id.saturating_add(1)));
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

        let compact_first_cost =
            reused_tokens as f64 * CACHE_READ_RATE + rewrite_tokens as f64 * CACHE_WRITE_RATE;
        let payoff_savings = payoff_savings_input_units(
            implicit_cached_tokens,
            current_tokens,
            retained_tokens,
            compact_first_cost,
            policy.payoff_requests,
        );
        let keep_payoff_cost = implicit_cached_tokens as f64 * CACHE_READ_RATE
            + current_tokens.saturating_sub(implicit_cached_tokens) as f64 * CACHE_WRITE_RATE
            + policy.payoff_requests.saturating_sub(1) as f64
                * current_tokens as f64
                * CACHE_READ_RATE;

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
            estimated_savings_input_units: payoff_savings,
            minimum_payback_input_units: keep_payoff_cost * COMPACTION_MIN_PAYBACK_RATIO,
        }
    }

    pub fn compact(&mut self, plan: CompactionPlan) -> ContextChange {
        let dropped = plan.dropped.iter().copied().collect::<HashSet<_>>();
        let neutral_retained = plan
            .neutral_retained
            .iter()
            .map(|decision| decision.id)
            .collect::<HashSet<_>>();
        let retention_audit = self
            .items
            .iter()
            .map(|item| {
                let action = if dropped.contains(&item.id) {
                    RetentionAuditAction::Removed
                } else {
                    RetentionAuditAction::Kept
                };
                let reason = if item.signal == RetentionSignal::Drop {
                    RetentionAuditReason::ExplicitRemovable
                } else if item.keep_lease_expired {
                    RetentionAuditReason::ExpiredKeepLease
                } else if item
                    .keep_lease_expires_at_turn
                    .is_some_and(|expires_at| expires_at > self.retention_turn)
                {
                    RetentionAuditReason::ActiveKeepLease
                } else if item.signal == RetentionSignal::Keep {
                    RetentionAuditReason::ExplicitKeep
                } else if neutral_retained.contains(&item.id)
                    || item.retention == Retention::Eligible
                {
                    RetentionAuditReason::AutomaticEligible
                } else {
                    RetentionAuditReason::ProtectedBaseline
                };
                RetentionAuditEntry {
                    id: item.id,
                    estimated_tokens: item.bytes.div_ceil(ESTIMATED_BYTES_PER_TOKEN),
                    action,
                    reason,
                }
            })
            .collect::<Vec<_>>();
        self.items.retain(|item| !dropped.contains(&item.id));
        for item in &mut self.items {
            if let Some(memory) = item.memory.as_mut()
                && dropped.contains(&memory.source_id)
            {
                memory.materialized = true;
            }
            if neutral_retained.contains(&item.id) {
                item.retention = Retention::Eligible;
            } else {
                item.retention = Retention::Protected;
            }
            if item.signal == RetentionSignal::Keep && item.retention == Retention::Protected {
                item.signal = RetentionSignal::Neutral;
            }
        }
        if !self
            .items
            .iter()
            .any(|item| item.kind == ContextItemKind::Status)
        {
            let id = self.allocate_id();
            self.items.push(ContextItem::history_status(id));
        }
        self.generation = self.generation.saturating_add(1);
        let protected_frontier = self.protected_frontier_id();
        if let Some(frontier) = protected_frontier {
            let prefix_len = self
                .items
                .iter()
                .position(|item| item.id == frontier)
                .expect("protected frontier belongs to retained context")
                + 1;
            let item_ids = self
                .items
                .iter()
                .take(prefix_len)
                .map(|item| item.id)
                .collect::<Vec<_>>();
            let prefix_ids = item_ids.iter().copied().collect::<HashSet<_>>();
            let mut frontiers = self
                .compatible_breakpoint_frontiers(&self.items)
                .into_iter()
                .filter(|candidate| prefix_ids.contains(candidate))
                .collect::<Vec<_>>();
            if !frontiers.contains(&frontier) {
                frontiers.push(frontier);
            }
            let rendered_prefix =
                Self::render_items_through(&self.items, &frontiers, Some(frontier));
            self.breakpoints.push(StoredBreakpoint {
                generation: self.generation,
                item_ids,
                marker_frontiers: frontiers,
                rendered_prefix,
            });
        }

        ContextChange {
            dropped: plan.dropped,
            retention_audit,
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
            protected_frontier: self.protected_frontier_id(),
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
    fn force_protected_for_test(&mut self) {
        for item in &mut self.items {
            item.retention = Retention::Protected;
        }
    }
}

fn meets_payback_threshold(savings: f64, minimum_payback: f64) -> bool {
    savings > minimum_payback
}

fn direct_next_request_savings(
    implicit_cached_tokens: usize,
    current_tokens: usize,
    compact_first_cost: f64,
) -> f64 {
    keep_request_cost_with_implicit(implicit_cached_tokens, current_tokens) - compact_first_cost
}

fn payoff_savings_input_units(
    implicit_cached_tokens: usize,
    current_tokens: usize,
    retained_tokens: usize,
    compact_first_cost: f64,
    payoff_requests: u64,
) -> f64 {
    let keep_first = keep_request_cost_with_implicit(implicit_cached_tokens, current_tokens);
    let compact_first = compact_first_cost;
    let later_requests = payoff_requests.saturating_sub(1) as f64;
    keep_first + later_requests * current_tokens as f64 * CACHE_READ_RATE
        - compact_first
        - later_requests * retained_tokens as f64 * CACHE_READ_RATE
}

fn keep_request_cost(current_tokens: usize, policy: &CompactionPolicy) -> f64 {
    keep_request_cost_with_implicit(
        policy.implicit_cached_tokens.min(current_tokens),
        current_tokens,
    )
}

fn keep_request_cost_with_implicit(implicit_cached_tokens: usize, current_tokens: usize) -> f64 {
    implicit_cached_tokens as f64 * CACHE_READ_RATE
        + current_tokens.saturating_sub(implicit_cached_tokens) as f64 * CACHE_WRITE_RATE
}

fn compact_request_cost(plan: &CompactionPlan) -> f64 {
    plan.retained_tokens.saturating_sub(plan.rewrite_tokens) as f64 * CACHE_READ_RATE
        + plan.rewrite_tokens as f64 * CACHE_WRITE_RATE
}

fn simulate_rollout_turn(
    state: &mut ContextState,
    dropped_ids: &[u64],
    virtual_item_tokens: usize,
    neutral_budget_tokens: usize,
    base_policy: &CompactionPolicy,
) -> f64 {
    let cached_before_virtual_item = state.estimated_tokens();
    let virtual_id = state.add_simulated_tool_item(virtual_item_tokens);
    for id in dropped_ids {
        if let Some(item) = state.items.iter_mut().find(|item| item.id == *id) {
            item.signal = RetentionSignal::Drop;
        }
    }
    let policy = CompactionPolicy {
        implicit_cached_tokens: cached_before_virtual_item,
        breakpoints: state
            .rendered_breakpoints()
            .into_iter()
            .map(|breakpoint| PricedBreakpoint {
                generation: breakpoint.generation,
                cached_tokens: breakpoint.prefix_tokens,
            })
            .collect(),
        payoff_requests: base_policy.payoff_requests,
    };
    let Some(plan) = state.plan_compaction_with_neutral_budget(
        &[virtual_id],
        policy.clone(),
        neutral_budget_tokens,
    ) else {
        return keep_request_cost(state.estimated_tokens(), &policy);
    };
    let cost = compact_request_cost(&plan);
    state.compact(plan);
    cost
}

impl ContextState {
    fn add_simulated_tool_item(&mut self, estimated_tokens: usize) -> u64 {
        let id = self.allocate_id();
        let bytes = estimated_tokens.saturating_mul(ESTIMATED_BYTES_PER_TOKEN);
        self.items.push(ContextItem::new(
            id,
            ContextItemKind::Tool,
            Retention::Eligible,
            vec![json!({
                "role": "developer",
                "content": [{"type": "input_text", "text": "x".repeat(bytes)}]
            })],
        ));
        id
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FlatRolloutConfig {
    pub samples: u32,
    pub horizon: u64,
    pub seed: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct FlatRolloutEstimate {
    pub samples: u32,
    pub horizon: u64,
    pub seed: u64,
    pub mean_virtual_item_tokens: usize,
    pub max_sampled_drops: usize,
    pub expected_compact_cost: f64,
    pub expected_keep_cost: f64,
    pub direct_next_request_savings_input_units: f64,
    pub expected_savings_input_units: f64,
}

#[derive(Clone, Copy)]
struct FlatRolloutRng(u64);

impl FlatRolloutRng {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9e37_79b9_7f4a_7c15)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        self.0
    }

    fn range_inclusive(&mut self, upper: usize) -> usize {
        if upper == 0 {
            0
        } else {
            (self.next_u64() % (upper as u64 + 1)) as usize
        }
    }

    fn range_exclusive(&mut self, upper: usize) -> usize {
        debug_assert!(upper > 0);
        (self.next_u64() % upper as u64) as usize
    }
}

fn sample_ids_without_replacement(
    candidates: &[u64],
    count: usize,
    rng: &mut FlatRolloutRng,
) -> Vec<u64> {
    let mut ids = candidates.to_vec();
    for index in 0..count {
        let selected = index + rng.range_exclusive(ids.len() - index);
        ids.swap(index, selected);
    }
    ids.truncate(count);
    ids
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
pub struct CompactionPolicy {
    pub implicit_cached_tokens: usize,
    pub breakpoints: Vec<PricedBreakpoint>,
    /// Fixed number of requests over which a rewrite is amortized; must be positive.
    pub payoff_requests: u64,
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
    pub minimum_payback_input_units: f64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct NeutralRetentionDecision {
    pub id: u64,
    pub tokens: usize,
    pub score: u64,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RetentionAuditAction {
    Kept,
    Removed,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RetentionAuditReason {
    ActiveKeepLease,
    ExplicitKeep,
    ExpiredKeepLease,
    ExplicitRemovable,
    AutomaticEligible,
    ProtectedBaseline,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct RetentionAuditEntry {
    pub id: u64,
    pub estimated_tokens: usize,
    pub action: RetentionAuditAction,
    pub reason: RetentionAuditReason,
}

#[derive(Debug, Serialize)]
pub(crate) struct KeepLeaseReview {
    pub item_ids: Vec<u64>,
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
    pub retention_audit: Vec<RetentionAuditEntry>,
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
    pub protected_frontier: Option<u64>,
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
        add_tool_with_output(state, &"exact output ".repeat(100))
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
    fn human_direction_defaults_protected_and_explicitly_removable() {
        let mut state = ContextState::new("keep deployment private".into());
        let human = 1;
        let source = add_tool_with_output(&mut state, &"temporary output ".repeat(1_000));

        let default_plan = state
            .plan_compaction_with_neutral_budget(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                    payoff_requests: 1,
                },
                0,
            )
            .unwrap();
        assert!(!default_plan.dropped.contains(&human));

        let change = state.record_signals(&update(&[], &[human], &[]), source);
        assert_eq!(change.drop, vec![human]);
        assert!(change.ignored.is_empty());
        let explicit_plan = state
            .plan_compaction_with_neutral_budget(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                    payoff_requests: 1,
                },
                0,
            )
            .unwrap();
        assert!(explicit_plan.dropped.contains(&human));
        state.compact(explicit_plan);
        assert!(
            !serde_json::to_string(&state.input_items())
                .unwrap()
                .contains("keep deployment private")
        );
    }

    #[test]
    fn non_human_context_is_eligible_for_budget_compaction() {
        let mut state = ContextState::new("initial direction".into());
        let source = add_tool_with_output(&mut state, &"temporary output ".repeat(1_000));
        let summary = "durable summary ".repeat(500);
        let memory = state
            .record_signals(&update(&[], &[], &[&summary]), source)
            .added[0];

        let first_plan = state
            .plan_compaction_with_neutral_budget(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                    payoff_requests: 1,
                },
                0,
            )
            .unwrap();
        assert!(first_plan.dropped.contains(&source));
        assert!(first_plan.dropped.contains(&memory));
        state.compact(first_plan);

        let status = state
            .snapshot()
            .into_iter()
            .find(|item| item.kind == ContextItemKind::Status)
            .unwrap()
            .id;
        let next = add_tool_with_output(&mut state, &"next output ".repeat(1_000));
        let second_plan = state
            .plan_compaction_with_neutral_budget(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                    payoff_requests: 1,
                },
                0,
            )
            .unwrap();
        assert!(second_plan.dropped.contains(&status));
        assert!(second_plan.dropped.contains(&next));
    }

    #[test]
    fn flat_rollout_is_seeded_nonmutating_and_caps_random_drops_at_four() {
        let mut state = ContextState::new("initial".into());
        let oldest = add_tool_with_output(&mut state, &"old ".repeat(4_000));
        let middle = add_tool_with_output(&mut state, &"middle ".repeat(1_000));
        let newest = add_tool_with_output(&mut state, &"new ".repeat(1_000));
        let budget = state.item_estimated_tokens(middle) + state.item_estimated_tokens(newest);
        let policy = CompactionPolicy {
            implicit_cached_tokens: 0,
            breakpoints: Vec::new(),
            payoff_requests: 5,
        };
        let plan = state
            .plan_compaction_with_neutral_budget(&[], policy.clone(), budget)
            .expect("initial plan should remove the old tool result");
        assert!(plan.dropped.contains(&oldest));
        let before = state.encode().unwrap();

        let config = FlatRolloutConfig {
            samples: 16,
            horizon: 5,
            seed: 7,
        };
        let first = state.flat_rollout_estimate(&plan, &[], policy.clone(), config);
        let second = state.flat_rollout_estimate(&plan, &[], policy, config);

        assert_eq!(first, second);
        assert_eq!(first.samples, 16);
        assert!(first.max_sampled_drops <= 4);
        assert!(first.mean_virtual_item_tokens > 0);
        assert_eq!(state.encode().unwrap(), before);
    }

    #[test]
    fn rollout_virtual_item_is_priced_as_a_cache_write() {
        let mut state = ContextState::new("initial".into());
        add_tool_with_output(&mut state, &"x".repeat(4_000));
        let previous_tokens = state.estimated_tokens();
        let cost = simulate_rollout_turn(
            &mut state,
            &[],
            1_000,
            usize::MAX,
            &CompactionPolicy {
                implicit_cached_tokens: 0,
                breakpoints: Vec::new(),
                payoff_requests: 5,
            },
        );
        let current_tokens = state.estimated_tokens();
        let expected = previous_tokens as f64 * CACHE_READ_RATE
            + current_tokens.saturating_sub(previous_tokens) as f64 * CACHE_WRITE_RATE;
        assert_eq!(cost, expected);
    }

    #[test]
    fn five_request_payoff_accepts_a_rewrite_that_one_request_rejects() {
        let policy = CompactionPolicy {
            implicit_cached_tokens: 0,
            breakpoints: Vec::new(),
            payoff_requests: 5,
        };
        assert_eq!(policy.payoff_requests, 5);
        assert!(payoff_savings_input_units(312_141, 313_063, 43_650, 54_562.5, 1) < 0.0);
        assert!(payoff_savings_input_units(312_141, 313_063, 43_650, 54_562.5, 5) > 0.0);
        assert!(direct_next_request_savings(312_141, 313_063, 54_562.5) < 0.0);
    }

    #[test]
    fn payback_threshold_rejects_small_positive_savings() {
        assert!(!meets_payback_threshold(9.9, 10.0));
        assert!(meets_payback_threshold(10.1, 10.0));
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
                    payoff_requests: 1,
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
    fn budget_retained_eligible_items_and_status_are_reconsidered() {
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
                    payoff_requests: 1,
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
                Retention::Eligible
            );
        }
        let status = state
            .snapshot()
            .into_iter()
            .find(|item| item.kind == ContextItemKind::Status)
            .unwrap()
            .id;

        let latest = add_tool_with_output(&mut state, &"latest ".repeat(100));
        let next = state
            .plan_compaction_with_neutral_budget(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                    payoff_requests: 1,
                },
                budget,
            )
            .unwrap();
        assert_eq!(next.dropped, vec![middle, newest]);
        assert_eq!(
            next.neutral_retained
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![latest, status]
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
                    payoff_requests: 1,
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
    fn budget_candidate_prices_the_same_eligible_markers_it_will_commit() {
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
                        payoff_requests: 1,
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
                .chain(std::iter::once(ContextItem::history_status(
                    state.next_id + 1,
                )))
                .collect::<Vec<_>>();
            let expected = estimated_tokens(&state.render_with_compatible_breakpoints(&retained));
            assert_eq!(
                plan.retained_tokens, expected,
                "padding {padding} must price the eligible marker committed by compact"
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
                    payoff_requests: 1,
                },
                usize::MAX,
            )
            .unwrap();

        assert_eq!(plan.dropped, vec![dropped]);
        assert!(plan.neutral_retained.is_empty());
    }

    #[test]
    fn compaction_adds_one_eligible_history_status_and_prices_it() {
        let mut state = ContextState::new("initial".into());
        let first = add_tool_with_output(&mut state, &"first ".repeat(1_000));
        state.record_signals(&update(&[], &[first], &[]), first);

        let plan = state
            .plan_compaction(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                    payoff_requests: 1,
                },
            )
            .unwrap();
        let mut expected_retained = state
            .items
            .iter()
            .filter(|item| !plan.dropped.contains(&item.id))
            .cloned()
            .collect::<Vec<_>>();
        expected_retained.push(ContextItem::history_status(state.next_id + 1));
        assert_eq!(
            plan.retained_tokens,
            estimated_tokens(&state.render_with_compatible_breakpoints(&expected_retained))
        );
        state.compact(plan);

        let status = "[history status: earlier context has been removed by compaction]";
        let rendered = serde_json::to_string(&state.input_items()).unwrap();
        assert_eq!(rendered.matches(status).count(), 1);
        let status_item = state
            .snapshot()
            .into_iter()
            .find(|item| {
                serde_json::to_string(&item.input_items)
                    .unwrap()
                    .contains(status)
            })
            .unwrap();
        assert_eq!(status_item.retention, Retention::Eligible);

        let second = add_tool_with_output(&mut state, &"second ".repeat(1_000));
        state.record_signals(&update(&[], &[second], &[]), second);
        let plan = state
            .plan_compaction(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                    payoff_requests: 1,
                },
            )
            .unwrap();
        state.compact(plan);

        assert_eq!(
            serde_json::to_string(&state.input_items())
                .unwrap()
                .matches(status)
                .count(),
            1
        );
    }

    #[test]
    fn expired_keep_lease_requires_explicit_renewal_before_becoming_neutral() {
        let mut state = ContextState::new("initial".into());
        let protected = add_tool(&mut state);
        let source = add_tool(&mut state);
        let change = state.record_signals(&update(&[protected], &[], &[]), source);
        state.arm_keep_leases(&change.keep, 1);

        state.advance_retention_turn();
        let review = state.attach_due_keep_lease_review();
        assert_eq!(review.item_ids, vec![protected]);
        assert!(
            serde_json::to_string(&state.input_items())
                .unwrap()
                .contains("Previously protected items")
        );

        let expired = state.resolve_keep_lease_review(&[]);
        assert_eq!(expired, vec![protected]);
        assert_eq!(state.signal_for(protected), Some(RetentionSignal::Neutral));
        assert_eq!(
            state
                .items
                .iter()
                .find(|item| item.id == protected)
                .unwrap()
                .retention,
            Retention::Eligible
        );
    }

    #[test]
    fn compaction_audit_distinguishes_expired_leases_from_active_keeps() {
        let mut state = ContextState::new("initial".into());
        let expired = add_tool_with_output(&mut state, &"expired ".repeat(100));
        let active = add_tool_with_output(&mut state, &"active ".repeat(100));
        let source = add_tool(&mut state);
        let change = state.record_signals(&update(&[expired, active], &[], &[]), source);
        state.arm_keep_leases(&change.keep, 2);
        state.advance_retention_turn();
        state.advance_retention_turn();
        state.attach_due_keep_lease_review();
        state.resolve_keep_lease_review(&[active]);
        state.arm_keep_leases(&[active], 2);

        let plan = state
            .plan_compaction_with_neutral_budget(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                    payoff_requests: 1,
                },
                0,
            )
            .unwrap();
        let change = state.compact(plan);

        assert!(change.retention_audit.iter().any(|entry| {
            entry.id == expired
                && entry.action == RetentionAuditAction::Removed
                && entry.reason == RetentionAuditReason::ExpiredKeepLease
        }));
        assert!(change.retention_audit.iter().any(|entry| {
            entry.id == active
                && entry.action == RetentionAuditAction::Kept
                && entry.reason == RetentionAuditReason::ActiveKeepLease
        }));
    }

    #[test]
    fn human_protection_after_an_eligible_item_does_not_move_the_cache_frontier() {
        let mut state = ContextState::new("initial".into());
        let tool = add_tool(&mut state);
        let steering = state.add_user("steer here".into());

        assert_eq!(state.protected_frontier_len(), 1);
        let rendered = state.input_items();
        let texts = rendered
            .iter()
            .filter_map(|item| item["content"][0]["text"].as_str())
            .collect::<Vec<_>>();
        assert!(texts.contains(&"[context 1]"));
        assert!(rendered.iter().any(|item| {
            item["type"] == "function_call_output"
                && item["output"]
                    .as_str()
                    .is_some_and(|output| output.ends_with(&format!("[context {tool}]")))
        }));
        assert!(texts.contains(&format!("[context {steering}]").as_str()));
        assert_eq!(
            rendered
                .iter()
                .filter(|item| item["content"][0].get("prompt_cache_breakpoint").is_some())
                .count(),
            1
        );
    }

    #[test]
    fn rendered_context_markers_hide_lifecycle_details() {
        let mut state = ContextState::new("initial".into());
        let tool = add_tool(&mut state);
        let steering = state.add_user("steer here".into());

        let rendered = serde_json::to_string(&state.input_items()).unwrap();

        assert!(rendered.contains("[context 1]"));
        assert!(rendered.contains(&format!("[context {tool}]")));
        assert!(rendered.contains(&format!("[context {steering}]")));
        assert!(!rendered.contains("stable"));
        assert!(!rendered.contains("volatile"));
    }

    #[test]
    fn memory_is_an_inline_eligible_handle_without_duplicating_its_content() {
        let mut state = ContextState::new("initial".into());
        let tool = add_tool(&mut state);
        let change = state.record_signals(&update(&[], &[], &["durable outcome"]), tool);
        let memory = change.added[0];

        assert_eq!(
            state.items.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![1, tool, memory]
        );
        assert_eq!(state.items[2].retention, Retention::Eligible);
        assert_eq!(state.protected_frontier_len(), 1);

        let rendered = state.input_items();
        let tool_output = rendered
            .iter()
            .find(|item| item["type"] == "function_call_output")
            .unwrap()["output"]
            .as_str()
            .unwrap();
        assert!(tool_output.contains(&format!("[context {tool}]")));
        assert!(tool_output.contains("[memory stored]"));
        assert!(tool_output.contains(&format!("[context {memory}]")));
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
        state.record_signals(&update(&[memory], &[], &[]), tool);

        let plan = state
            .plan_compaction(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                    payoff_requests: 1,
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
                .any(|item| { item["content"][0]["text"] == format!("[context {memory}]") })
        );
    }

    #[test]
    fn planner_prices_memory_materialization_before_dropping_its_source() {
        let mut state = ContextState::new("initial".into());
        let tool = add_tool(&mut state);
        let memory = state
            .record_signals(&update(&[], &[tool], &[&"important ".repeat(2_000)]), tool)
            .added[0];
        state.record_signals(&update(&[memory], &[], &[]), tool);

        assert!(
            state
                .plan_compaction(
                    &[],
                    CompactionPolicy {
                        implicit_cached_tokens: 0,
                        breakpoints: Vec::new(),
                        payoff_requests: 1,
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
                    payoff_requests: 1,
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
                payoff_requests: 1,
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
                        payoff_requests: 1,
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
                    payoff_requests: 1,
                },
            )
            .unwrap();

        assert_eq!(plan.reused_generation, Some(0));
        assert_eq!(plan.dropped, vec![dropped]);
        assert!(plan.rewrite_tokens < plan.retained_tokens);
    }

    #[test]
    fn eligible_items_are_removable_unless_protected() {
        let mut state = ContextState::new("initial".into());
        let disposable = add_tool(&mut state);

        let plan = state
            .plan_compaction(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                    payoff_requests: 1,
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
                        payoff_requests: 1,
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
                    payoff_requests: 1,
                },
            )
            .unwrap();
        let change = state.compact(plan);

        assert_eq!(change.dropped, vec![dropped]);
        assert!(
            state
                .snapshot()
                .iter()
                .filter(|item| item.kind != ContextItemKind::Status)
                .all(|item| item.retention == Retention::Protected)
        );
        assert!(
            state
                .snapshot()
                .iter()
                .any(|item| item.kind == ContextItemKind::Status
                    && item.retention == Retention::Eligible)
        );
        assert_eq!(state.signal_for(retained), Some(RetentionSignal::Neutral));
    }

    #[test]
    fn compaction_can_remove_protected_drop_candidates() {
        let mut state = ContextState::new("initial".into());
        let old_tool = add_tool(&mut state);
        state.force_protected_for_test();
        let new_tool = add_tool(&mut state);
        state.record_signals(&update(&[], &[old_tool, new_tool], &[]), new_tool);

        let plan = state
            .plan_compaction(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                    payoff_requests: 1,
                },
            )
            .unwrap();

        assert_eq!(plan.dropped, vec![old_tool, new_tool]);
    }

    #[test]
    fn compaction_generation_reuses_the_protected_frontier_after_dropping_its_eligible_tail() {
        let mut state = ContextState::new("initial ".repeat(800));
        let disposable = add_tool_with_output(&mut state, &"old ".repeat(600));
        let eligible_tail = add_tool_with_output(&mut state, &"tail ".repeat(2_000));
        let memory = state
            .record_signals(
                &update(&[], &[disposable], &["durable outcome"]),
                eligible_tail,
            )
            .added[0];

        let first_plan = state
            .plan_compaction_with_neutral_budget(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: Vec::new(),
                    payoff_requests: 1,
                },
                usize::MAX,
            )
            .unwrap();
        assert_eq!(
            first_plan
                .neutral_retained
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![memory, eligible_tail]
        );
        let first_change = state.compact(first_plan);
        assert_eq!(first_change.protected_frontier, Some(1));

        state.record_signals(&update(&[], &[eligible_tail], &[]), eligible_tail);
        let second_plan = state
            .plan_compaction_with_neutral_budget(
                &[],
                CompactionPolicy {
                    implicit_cached_tokens: 0,
                    breakpoints: vec![PricedBreakpoint {
                        generation: first_change.generation,
                        cached_tokens: 1_200,
                    }],
                    payoff_requests: 1,
                },
                usize::MAX,
            )
            .unwrap();

        assert_eq!(second_plan.dropped, vec![eligible_tail]);
        assert_eq!(second_plan.reused_generation, Some(first_change.generation));
        assert!(second_plan.rewrite_tokens < second_plan.retained_tokens);
    }

    #[test]
    fn legacy_stable_and_volatile_checkpoint_values_remain_resumable() {
        let mut state = ContextState::new("first task".to_owned());
        add_tool(&mut state);
        let legacy = String::from_utf8(state.encode().unwrap())
            .unwrap()
            .replace("\"protected\"", "\"stable\"")
            .replace("\"eligible\"", "\"volatile\"");

        let restored = ContextState::decode(legacy.as_bytes()).unwrap();
        assert_eq!(restored.input_items(), state.input_items());
    }

    #[test]
    fn context_checkpoint_round_trips_complete_state() {
        let mut state = ContextState::new("first task".to_owned());
        let item = add_tool(&mut state);
        state.record_signals(&update(&[item], &[], &["keep this"]), item);
        let expected = state.input_items();

        let restored = ContextState::decode(&state.encode().unwrap()).unwrap();
        assert_eq!(restored.input_items(), expected);
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
                    payoff_requests: 1,
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
                    payoff_requests: 1,
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
                    payoff_requests: 1,
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

    #[test]
    fn context_state_round_trips_between_task_boundaries() {
        let mut state = ContextState::new("first task".to_owned());
        let tool_id = add_tool_with_output(&mut state, "first result");
        state.record_signals(&update(&[tool_id], &[], &[]), tool_id);

        let encoded = state.encode().unwrap();
        let mut resumed = ContextState::decode(&encoded).unwrap();
        resumed.add_user("second task".to_owned());

        let rendered = resumed.input_items();
        assert!(
            rendered
                .iter()
                .any(|item| item.to_string().contains("first task"))
        );
        assert!(
            rendered
                .iter()
                .any(|item| item.to_string().contains("first result"))
        );
        assert!(
            rendered
                .iter()
                .any(|item| item.to_string().contains("second task"))
        );
    }
}
