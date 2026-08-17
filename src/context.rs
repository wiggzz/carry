use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

use crate::protocol::ContextManagement;

const ESTIMATED_BYTES_PER_TOKEN: usize = 4;
const CACHE_READ_RATE: f64 = 0.10;
const CACHE_WRITE_RATE: f64 = 1.25;

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

impl ContextItemKind {
    fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Memory => "memory",
            Self::Tool => "tool",
        }
    }
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

    fn memory(id: u64, content: String) -> Self {
        Self::new(
            id,
            ContextItemKind::Memory,
            Retention::Stable,
            vec![json!({
                "role": "user",
                "content": [{ "type": "input_text", "text": format!("[memory]\n{content}") }]
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
        }
    }

    fn marker(&self, checkpoint: bool) -> Value {
        let mut block = json!({
            "type": "input_text",
            "text": format!(
                "[{} {} {}]",
                self.id,
                self.kind.label(),
                self.retention.label()
            )
        });
        if checkpoint {
            block["prompt_cache_breakpoint"] = json!({ "mode": "explicit" });
        }
        json!({ "role": "developer", "content": [block] })
    }

    fn estimated_tokens(&self) -> usize {
        self.bytes.div_ceil(ESTIMATED_BYTES_PER_TOKEN)
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
}

impl ContextState {
    pub fn new(initial_prompt: String) -> Self {
        Self {
            items: vec![ContextItem::user(1, initial_prompt)],
            next_id: 1,
            generation: 0,
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
        let frontier = self.stable_frontier_len();
        let mut input = Vec::new();
        for (index, item) in self.items.iter().enumerate() {
            input.extend(item.input_items.iter().cloned());
            input.push(item.marker(frontier > 0 && index + 1 == frontier));
        }
        input
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
        self.items.iter().map(ContextItem::estimated_tokens).sum()
    }

    pub fn record_signals(&mut self, update: &ContextManagement) -> SignalChange {
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
            self.items.push(ContextItem::memory(id, content.to_owned()));
            added.push(id);
        }

        SignalChange {
            keep,
            drop,
            ignored: unique_ids(&ignored),
            added,
        }
    }

    pub fn plan_compaction(
        &self,
        protected: &[u64],
        policy: CompactionPolicy,
    ) -> Option<CompactionPlan> {
        let protected = protected.iter().copied().collect::<HashSet<_>>();
        let minor_drops = self
            .items
            .iter()
            .filter(|item| {
                item.retention == Retention::Volatile
                    && item.signal == RetentionSignal::Drop
                    && !protected.contains(&item.id)
            })
            .map(|item| item.id)
            .collect::<Vec<_>>();
        let major_drops = self
            .items
            .iter()
            .filter(|item| item.signal == RetentionSignal::Drop && !protected.contains(&item.id))
            .map(|item| item.id)
            .collect::<Vec<_>>();

        let mut candidates = Vec::new();
        if !minor_drops.is_empty() {
            candidates.push(self.compaction_candidate(CompactionKind::Minor, minor_drops, policy));
        }
        if major_drops.iter().any(|id| {
            self.items
                .iter()
                .any(|item| item.id == *id && item.retention == Retention::Stable)
        }) {
            candidates.push(self.compaction_candidate(CompactionKind::Major, major_drops, policy));
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
        kind: CompactionKind,
        dropped: Vec<u64>,
        policy: CompactionPolicy,
    ) -> CompactionPlan {
        let dropped_set = dropped.iter().copied().collect::<HashSet<_>>();
        let current_tokens = self.estimated_tokens();
        let dropped_tokens = self
            .items
            .iter()
            .filter(|item| dropped_set.contains(&item.id))
            .map(ContextItem::estimated_tokens)
            .sum::<usize>();
        let retained_tokens = current_tokens.saturating_sub(dropped_tokens);
        let frontier_tokens = self
            .items
            .iter()
            .take(self.stable_frontier_len())
            .map(ContextItem::estimated_tokens)
            .sum::<usize>()
            .min(retained_tokens);
        let rewrite_tokens = if kind == CompactionKind::Minor && policy.stable_cache_alive {
            retained_tokens.saturating_sub(frontier_tokens)
        } else {
            retained_tokens
        };
        let horizon = policy.horizon_turns.max(1) as f64;
        let baseline_first_rate = if policy.implicit_cache_alive {
            CACHE_READ_RATE
        } else {
            CACHE_WRITE_RATE
        };
        let baseline =
            current_tokens as f64 * (baseline_first_rate + CACHE_READ_RATE * (horizon - 1.0));
        let candidate_first = if kind == CompactionKind::Minor && policy.stable_cache_alive {
            frontier_tokens as f64 * CACHE_READ_RATE + rewrite_tokens as f64 * CACHE_WRITE_RATE
        } else {
            retained_tokens as f64 * CACHE_WRITE_RATE
        };
        let candidate =
            candidate_first + retained_tokens as f64 * CACHE_READ_RATE * (horizon - 1.0);
        let immediate_penalty = candidate_first - current_tokens as f64 * baseline_first_rate;
        let future_savings = dropped_tokens as f64 * CACHE_READ_RATE;
        let break_even_turns = if !policy.implicit_cache_alive || immediate_penalty <= 0.0 {
            1
        } else if future_savings > 0.0 {
            (immediate_penalty / future_savings).ceil() as usize + 1
        } else {
            usize::MAX
        };

        CompactionPlan {
            kind,
            dropped,
            dropped_tokens,
            retained_tokens,
            rewrite_tokens,
            break_even_turns,
            estimated_savings_input_units: baseline - candidate,
        }
    }

    pub fn compact(&mut self, plan: CompactionPlan) -> ContextChange {
        let dropped = plan.dropped.iter().copied().collect::<HashSet<_>>();
        self.items.retain(|item| !dropped.contains(&item.id));
        for item in &mut self.items {
            item.retention = Retention::Stable;
            if item.signal == RetentionSignal::Keep {
                item.signal = RetentionSignal::Neutral;
            }
        }
        self.generation = self.generation.saturating_add(1);

        ContextChange {
            kind: plan.kind,
            dropped: plan.dropped,
            dropped_tokens: plan.dropped_tokens,
            retained_tokens: plan.retained_tokens,
            rewrite_tokens: plan.rewrite_tokens,
            break_even_turns: plan.break_even_turns,
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

#[derive(Clone, Copy, Debug)]
pub(crate) struct CompactionPolicy {
    pub horizon_turns: usize,
    pub implicit_cache_alive: bool,
    pub stable_cache_alive: bool,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompactionKind {
    Minor,
    Major,
}

impl CompactionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Minor => "minor",
            Self::Major => "major",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CompactionPlan {
    pub kind: CompactionKind,
    pub dropped: Vec<u64>,
    pub dropped_tokens: usize,
    pub retained_tokens: usize,
    pub rewrite_tokens: usize,
    pub break_even_turns: usize,
    pub estimated_savings_input_units: f64,
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
    pub kind: CompactionKind,
    pub dropped: Vec<u64>,
    pub dropped_tokens: usize,
    pub retained_tokens: usize,
    pub rewrite_tokens: usize,
    pub break_even_turns: usize,
    pub estimated_savings_input_units: f64,
    pub generation: u64,
    pub stable_frontier: Option<u64>,
    pub retained_bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(keep: &[u64], drop: &[u64], remember: &[&str]) -> ContextManagement {
        ContextManagement {
            keep: keep.to_vec(),
            drop: drop.to_vec(),
            remember: remember.iter().map(|value| (*value).into()).collect(),
        }
    }

    fn add_tool(state: &mut ContextState) -> u64 {
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
                    "output": "exact output"
                }),
            )
            .unwrap()
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
        assert!(texts.contains(&"[1 user stable]"));
        assert!(texts.contains(&format!("[{tool} tool volatile]").as_str()));
        assert!(texts.contains(&format!("[{steering} user stable]").as_str()));
        assert_eq!(
            rendered
                .iter()
                .filter(|item| item["content"][0].get("prompt_cache_breakpoint").is_some())
                .count(),
            1
        );
    }

    #[test]
    fn memory_starts_stable_without_reordering_history() {
        let mut state = ContextState::new("initial".into());
        let tool = add_tool(&mut state);
        let change = state.record_signals(&update(&[], &[], &["durable outcome"]));
        let memory = change.added[0];

        assert_eq!(
            state.items.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![1, tool, memory]
        );
        assert_eq!(state.items[2].retention, Retention::Stable);
        assert_eq!(state.stable_frontier_len(), 1);
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

        state.record_signals(&update(&[], &[tool], &[]));
        assert_eq!(state.signal_for(tool), Some(RetentionSignal::Drop));
        assert!(state.snapshot().iter().any(|item| item.id == tool));

        state.record_signals(&update(&[tool], &[], &[]));
        assert_eq!(state.signal_for(tool), Some(RetentionSignal::Keep));
        assert!(state.snapshot().iter().any(|item| item.id == tool));
    }

    #[test]
    fn contradictory_and_unknown_signals_are_idempotent() {
        let mut state = ContextState::new("initial".into());
        let tool = add_tool(&mut state);

        let change = state.record_signals(&update(&[tool, 999], &[tool, 998], &[]));

        assert_eq!(state.signal_for(tool), Some(RetentionSignal::Keep));
        assert_eq!(change.ignored, vec![998, 999]);
    }

    #[test]
    fn warm_minor_compaction_waits_until_rewrite_cost_breaks_even() {
        let mut state = ContextState::new("initial".into());
        let dropped = add_tool(&mut state);
        let retained = add_tool(&mut state);
        state.record_signals(&update(&[], &[dropped], &[]));

        let short = state.plan_compaction(
            &[],
            CompactionPolicy {
                horizon_turns: 1,
                implicit_cache_alive: true,
                stable_cache_alive: true,
            },
        );
        assert!(short.is_none());

        state.record_signals(&update(&[retained], &[], &[]));
        let long = state
            .plan_compaction(
                &[],
                CompactionPolicy {
                    horizon_turns: 100,
                    implicit_cache_alive: true,
                    stable_cache_alive: true,
                },
            )
            .unwrap();
        assert_eq!(long.kind, CompactionKind::Minor);
        assert_eq!(long.dropped, vec![dropped]);
    }

    #[test]
    fn expired_cache_applies_drop_signals_and_builds_a_new_generation() {
        let mut state = ContextState::new("initial".into());
        let dropped = add_tool(&mut state);
        let retained = add_tool(&mut state);
        state.record_signals(&update(&[retained], &[dropped], &[]));

        let plan = state
            .plan_compaction(
                &[],
                CompactionPolicy {
                    horizon_turns: 1,
                    implicit_cache_alive: false,
                    stable_cache_alive: false,
                },
            )
            .unwrap();
        let change = state.compact(plan);

        assert_eq!(change.kind, CompactionKind::Minor);
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
    fn major_compaction_can_remove_stable_drop_candidates() {
        let mut state = ContextState::new("initial".into());
        let old_tool = add_tool(&mut state);
        state.force_stable_for_test();
        let new_tool = add_tool(&mut state);
        state.record_signals(&update(&[], &[old_tool, new_tool], &[]));

        let plan = state
            .plan_compaction(
                &[],
                CompactionPolicy {
                    horizon_turns: 1,
                    implicit_cache_alive: false,
                    stable_cache_alive: false,
                },
            )
            .unwrap();

        assert_eq!(plan.kind, CompactionKind::Major);
        assert_eq!(plan.dropped, vec![old_tool, new_tool]);
    }
}
