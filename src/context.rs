use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

use crate::protocol::ContextManagement;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Retention {
    Stable,
    Volatile,
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

/// One chronological unit of context. Native API items remain byte-for-byte
/// unchanged. The compact status marker is regenerated only when a collection
/// changes an item's retention class and the cache is being rewritten anyway.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct ContextItem {
    pub id: u64,
    pub kind: ContextItemKind,
    pub retention: Retention,
    pub bytes: usize,
    pub retention_rounds: usize,
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
            0,
        )
    }

    fn memory(id: u64, content: String) -> Self {
        Self::new(
            id,
            ContextItemKind::Memory,
            Retention::Volatile,
            vec![json!({
                "role": "user",
                "content": [{ "type": "input_text", "text": format!("[memory]\n{content}") }]
            })],
            1,
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
            0,
        ))
    }

    fn new(
        id: u64,
        kind: ContextItemKind,
        retention: Retention,
        input_items: Vec<Value>,
        retention_rounds: usize,
    ) -> Self {
        Self {
            id,
            kind,
            retention,
            bytes: serialized_bytes(&input_items),
            retention_rounds,
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
}

fn serialized_bytes(items: &[Value]) -> usize {
    serde_json::to_vec(items).map_or(usize::MAX, |bytes| bytes.len())
}

#[derive(Clone, Debug)]
pub(crate) struct ContextState {
    items: Vec<ContextItem>,
    next_id: u64,
    round: usize,
}

impl ContextState {
    pub fn new(initial_prompt: String) -> Self {
        Self {
            items: vec![ContextItem::user(1, initial_prompt)],
            next_id: 1,
            round: 0,
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

    /// Render the chronological ledger. Only the final item in the contiguous
    /// stable prefix receives an explicit cache breakpoint.
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

    pub fn stable_ids(&self) -> Vec<u64> {
        self.items
            .iter()
            .filter(|item| item.retention == Retention::Stable)
            .map(|item| item.id)
            .collect()
    }

    pub fn volatile_status(&self) -> Vec<(u64, usize)> {
        self.items
            .iter()
            .filter(|item| item.retention == Retention::Volatile)
            .map(|item| (item.id, item.retention_rounds))
            .collect()
    }

    pub fn stable_frontier_len(&self) -> usize {
        self.items
            .iter()
            .take_while(|item| item.retention == Retention::Stable)
            .count()
    }

    pub fn stable_bytes(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.retention == Retention::Stable)
            .map(|item| item.bytes)
            .sum()
    }

    pub fn volatile_bytes(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.retention == Retention::Volatile)
            .map(|item| item.bytes)
            .sum()
    }

    pub fn retained_bytes(&self) -> usize {
        self.stable_bytes() + self.volatile_bytes()
    }

    pub fn validate(&self, update: &ContextManagement, protected: &[u64]) -> Result<()> {
        validate_unique("keep", &update.keep)?;
        validate_unique("drop", &update.drop)?;
        let protected = protected.iter().copied().collect::<HashSet<_>>();
        let volatile = self
            .items
            .iter()
            .filter(|item| item.retention == Retention::Volatile)
            .map(|item| item.id)
            .chain(protected.iter().copied())
            .collect::<HashSet<_>>();
        for id in &update.keep {
            if !volatile.contains(id) {
                bail!("context.keep references non-volatile or unknown ID {id}");
            }
        }
        let stable = self
            .items
            .iter()
            .filter(|item| item.retention == Retention::Stable)
            .map(|item| item.id)
            .collect::<HashSet<_>>();
        for id in &update.drop {
            if !stable.contains(id) {
                bail!("context.drop references non-stable or unknown ID {id}");
            }
        }
        if update.keep.iter().any(|id| update.drop.contains(id)) {
            bail!("a context ID cannot appear in both keep and drop");
        }
        for memory in &update.remember {
            if memory.trim().is_empty() {
                bail!("context.remember contains an empty item");
            }
        }
        Ok(())
    }

    pub fn apply(
        &mut self,
        update: &ContextManagement,
        protected: &[u64],
        promotion_age: usize,
        collection_interval: usize,
    ) -> Result<ContextChange> {
        self.validate(update, protected)?;
        let keep = update.keep.iter().copied().collect::<HashSet<_>>();
        let drop = update.drop.iter().copied().collect::<HashSet<_>>();
        let protected = protected.iter().copied().collect::<HashSet<_>>();

        let dropped_volatile = self
            .items
            .iter()
            .filter(|item| {
                item.retention == Retention::Volatile
                    && !keep.contains(&item.id)
                    && !protected.contains(&item.id)
            })
            .map(|item| item.id)
            .collect::<Vec<_>>();
        let dropped_stable = self
            .items
            .iter()
            .filter(|item| item.retention == Retention::Stable && drop.contains(&item.id))
            .map(|item| item.id)
            .collect::<Vec<_>>();

        self.items.retain(|item| {
            !drop.contains(&item.id)
                && (item.retention == Retention::Stable
                    || keep.contains(&item.id)
                    || protected.contains(&item.id))
        });
        for item in &mut self.items {
            if item.retention == Retention::Volatile
                && keep.contains(&item.id)
                && !protected.contains(&item.id)
            {
                item.retention_rounds = item.retention_rounds.saturating_add(1);
            }
        }

        let mut added = Vec::new();
        for content in &update.remember {
            let id = self.allocate_id();
            self.items
                .push(ContextItem::memory(id, content.trim().to_owned()));
            added.push(id);
        }

        let next_round = self.round.saturating_add(1);
        let collection_round = next_round.is_multiple_of(collection_interval.max(1));
        let mut promoted = Vec::new();
        if collection_round {
            for item in &mut self.items {
                if item.retention == Retention::Volatile
                    && item.retention_rounds >= promotion_age.max(1)
                {
                    item.retention = Retention::Stable;
                    promoted.push(item.id);
                }
            }
        }
        self.round = next_round;

        Ok(ContextChange {
            stable: self.stable_ids(),
            volatile: self
                .volatile_status()
                .into_iter()
                .map(|(id, _)| id)
                .collect(),
            dropped_volatile,
            dropped_stable,
            added,
            promoted,
            collection_round,
            stable_frontier: self
                .stable_frontier_len()
                .checked_sub(1)
                .and_then(|index| self.items.get(index))
                .map(|item| item.id),
            stable_bytes: self.stable_bytes(),
            volatile_bytes: self.volatile_bytes(),
            bytes: self.retained_bytes(),
        })
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ContextChange {
    pub stable: Vec<u64>,
    pub volatile: Vec<u64>,
    pub dropped_volatile: Vec<u64>,
    pub dropped_stable: Vec<u64>,
    pub added: Vec<u64>,
    pub promoted: Vec<u64>,
    pub collection_round: bool,
    pub stable_frontier: Option<u64>,
    pub stable_bytes: usize,
    pub volatile_bytes: usize,
    pub bytes: usize,
}

fn validate_unique(field: &str, ids: &[u64]) -> Result<()> {
    let mut seen = HashSet::new();
    for id in ids {
        if !seen.insert(id) {
            bail!("context.{field} contains duplicate ID {id}");
        }
    }
    Ok(())
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

        assert_eq!(state.stable_ids(), vec![1, steering]);
        assert_eq!(state.volatile_status(), vec![(tool, 0)]);
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
    fn collection_promotes_only_independently_aged_volatile_items() {
        let mut state = ContextState::new("initial".into());
        let first = add_tool(&mut state);
        let steering = state.add_user("later stable".into());
        let second = add_tool(&mut state);

        state
            .apply(&update(&[first, second], &[], &[]), &[], 2, 2)
            .unwrap();
        state
            .items
            .iter_mut()
            .find(|item| item.id == second)
            .unwrap()
            .retention_rounds = 0;
        let change = state
            .apply(&update(&[first, second], &[], &[]), &[], 2, 2)
            .unwrap();

        assert_eq!(change.promoted, vec![first]);
        assert_eq!(state.stable_frontier_len(), 3);
        assert_eq!(state.items[1].id, first);
        assert_eq!(state.items[2].id, steering);
        assert_eq!(state.items[3].id, second);
        assert_eq!(state.items[3].retention, Retention::Volatile);
    }

    #[test]
    fn dropping_context_preserves_chronological_order_and_can_add_a_memory() {
        let mut state = ContextState::new("initial".into());
        let old_tool = add_tool(&mut state);
        let steering = state.add_user("later instruction".into());
        let kept_tool = add_tool(&mut state);

        let change = state
            .apply(&update(&[kept_tool], &[], &["durable outcome"]), &[], 3, 3)
            .unwrap();

        assert_eq!(change.dropped_volatile, vec![old_tool]);
        assert_eq!(
            state.items.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![1, steering, kept_tool, change.added[0]]
        );
        assert_eq!(state.stable_frontier_len(), 2);
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
    fn frontier_is_absent_when_the_first_surviving_item_is_volatile() {
        let mut state = ContextState::new("initial".into());
        let tool = add_tool(&mut state);
        let change = state.apply(&update(&[tool], &[1], &[]), &[], 3, 3).unwrap();

        assert_eq!(state.stable_frontier_len(), 0);
        assert_eq!(change.stable_frontier, None);
        assert!(
            state
                .input_items()
                .iter()
                .all(|item| item["content"][0].get("prompt_cache_breakpoint").is_none())
        );
    }
}
