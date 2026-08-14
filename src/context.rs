use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

use crate::protocol::ContextManagement;

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ContextItemKind {
    Memory,
    ToolInteraction,
}

/// One immutable selectable unit of context. Generation and checkpoint state
/// live outside `input_items`, so promotion never rewrites the segment itself.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct ContextItem {
    pub id: String,
    pub kind: ContextItemKind,
    pub bytes: usize,
    pub retention_rounds: usize,
    pub checkpoint_after: bool,
    pub input_items: Vec<Value>,
}

impl ContextItem {
    pub fn tool_interaction(
        id: String,
        function_call: Value,
        function_call_output: Value,
    ) -> Result<Self> {
        if function_call["type"].as_str() != Some("function_call") {
            bail!("tool interaction assistant item is not a function_call");
        }
        if function_call_output["type"].as_str() != Some("function_call_output") {
            bail!("tool interaction result item is not a function_call_output");
        }
        let call_id = function_call["call_id"]
            .as_str()
            .context("function_call has no call_id")?;
        if function_call_output["call_id"].as_str() != Some(call_id) {
            bail!("function call and output call_id values do not match");
        }
        let input_items = vec![function_call, function_call_output];
        Ok(Self {
            id,
            kind: ContextItemKind::ToolInteraction,
            bytes: serialized_bytes(&input_items),
            retention_rounds: 0,
            checkpoint_after: false,
            input_items,
        })
    }

    fn memory(id: String, content: String) -> Self {
        let input_items = vec![json!({
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": format!("[retained memory {id}]\n{content}")
            }]
        })];
        Self {
            id,
            kind: ContextItemKind::Memory,
            bytes: serialized_bytes(&input_items),
            retention_rounds: 1,
            checkpoint_after: false,
            input_items,
        }
    }
}

fn cache_checkpoint() -> Value {
    json!({
        "role": "developer",
        "content": [{
            "type": "input_text",
            "text": "[stable generation checkpoint]",
            "prompt_cache_breakpoint": { "mode": "explicit" }
        }]
    })
}

fn serialized_bytes(items: &[Value]) -> usize {
    serde_json::to_vec(items).map_or(usize::MAX, |bytes| bytes.len())
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ContextState {
    stable: Vec<ContextItem>,
    volatile: Vec<ContextItem>,
    next_memory_id: usize,
    round: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct ContextChange {
    pub stable: Vec<String>,
    pub volatile: Vec<String>,
    pub dropped: Vec<String>,
    pub released: Vec<String>,
    pub added: Vec<String>,
    pub promoted: Vec<String>,
    pub collection_round: bool,
    pub stable_bytes: usize,
    pub volatile_bytes: usize,
    pub bytes: usize,
}

impl ContextState {
    pub fn input_items(&self, latest: Option<&ContextItem>) -> Vec<Value> {
        let mut input = Vec::new();
        for item in &self.stable {
            input.extend(item.input_items.iter().cloned());
            if item.checkpoint_after {
                input.push(cache_checkpoint());
            }
        }
        for item in &self.volatile {
            input.extend(item.input_items.iter().cloned());
        }
        if let Some(item) = latest
            && !self
                .stable
                .iter()
                .chain(&self.volatile)
                .any(|old| old.id == item.id)
        {
            input.extend(item.input_items.iter().cloned());
        }
        input
    }

    pub fn stable_ids(&self) -> Vec<&str> {
        self.stable.iter().map(|item| item.id.as_str()).collect()
    }

    pub fn volatile_status(&self) -> Vec<(&str, usize)> {
        self.volatile
            .iter()
            .map(|item| (item.id.as_str(), item.retention_rounds))
            .collect()
    }

    pub fn snapshot(&self) -> Vec<&ContextItem> {
        self.stable.iter().chain(&self.volatile).collect()
    }

    pub fn stable_bytes(&self) -> usize {
        let checkpoint_bytes = serialized_bytes(&[cache_checkpoint()]);
        self.stable
            .iter()
            .map(|item| item.bytes + usize::from(item.checkpoint_after) * checkpoint_bytes)
            .sum()
    }

    pub fn volatile_bytes(&self) -> usize {
        self.volatile.iter().map(|item| item.bytes).sum()
    }

    pub fn retained_bytes(&self) -> usize {
        self.stable_bytes() + self.volatile_bytes()
    }

    /// Stable items survive by default. Volatile items must be explicitly
    /// retained, and only the oldest contiguous eligible volatile prefix can
    /// promote so history order never changes.
    pub fn apply(
        &mut self,
        update: &ContextManagement,
        latest: Option<&ContextItem>,
        promotion_age: usize,
        collection_interval: usize,
    ) -> Result<ContextChange> {
        validate_unique("retain_volatile_ids", &update.retain_volatile_ids)?;
        validate_unique("release_stable_ids", &update.release_stable_ids)?;

        let retain_available = self
            .volatile
            .iter()
            .map(|item| item.id.as_str())
            .chain(self.stable.iter().map(|item| item.id.as_str()))
            .chain(latest.map(|item| item.id.as_str()))
            .collect::<HashSet<_>>();
        for id in &update.retain_volatile_ids {
            if !retain_available.contains(id.as_str()) {
                bail!("context_management.retain_volatile_ids references unknown ID {id}");
            }
        }
        let stable_available = self
            .stable
            .iter()
            .map(|item| item.id.as_str())
            .collect::<HashSet<_>>();
        for id in &update.release_stable_ids {
            if !stable_available.contains(id.as_str()) {
                bail!(
                    "context_management.release_stable_ids references non-stable or unknown ID {id}"
                );
            }
        }

        let release = update
            .release_stable_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let retain = update
            .retain_volatile_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();

        let released = self
            .stable
            .iter()
            .filter(|item| release.contains(item.id.as_str()))
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        let mut stable = self
            .stable
            .iter()
            .filter(|item| !release.contains(item.id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        let dropped = self
            .volatile
            .iter()
            .filter(|item| !retain.contains(item.id.as_str()))
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        let mut volatile = self
            .volatile
            .iter()
            .filter(|item| retain.contains(item.id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        for item in &mut volatile {
            item.retention_rounds = item.retention_rounds.saturating_add(1);
        }
        if let Some(item) = latest
            && retain.contains(item.id.as_str())
            && !volatile.iter().any(|old| old.id == item.id)
        {
            let mut item = item.clone();
            item.retention_rounds = 1;
            volatile.push(item);
        }

        let mut added = Vec::new();
        let mut next_memory_id = self.next_memory_id;
        for content in &update.add_memories {
            let content = content.trim();
            if content.is_empty() {
                bail!("context_management.add_memories contains an empty item");
            }
            next_memory_id += 1;
            let id = format!("m{next_memory_id:04}");
            volatile.push(ContextItem::memory(id.clone(), content.to_owned()));
            added.push(id);
        }

        let age = promotion_age.max(1);
        let next_round = self.round.saturating_add(1);
        let collection_round = next_round.is_multiple_of(collection_interval.max(1));
        let promote_count = if collection_round {
            volatile
                .iter()
                .take_while(|item| item.retention_rounds >= age)
                .count()
        } else {
            0
        };
        let mut promoted_items = volatile.drain(..promote_count).collect::<Vec<_>>();
        let promoted = promoted_items
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        if let Some(last) = promoted_items.last_mut() {
            last.checkpoint_after = true;
        }
        stable.append(&mut promoted_items);

        self.stable = stable;
        self.volatile = volatile;
        self.next_memory_id = next_memory_id;
        self.round = next_round;
        let stable_bytes = self.stable_bytes();
        let volatile_bytes = self.volatile_bytes();
        Ok(ContextChange {
            stable: self.stable.iter().map(|item| item.id.clone()).collect(),
            volatile: self.volatile.iter().map(|item| item.id.clone()).collect(),
            dropped,
            released,
            added,
            promoted,
            collection_round,
            stable_bytes,
            volatile_bytes,
            bytes: stable_bytes + volatile_bytes,
        })
    }
}

fn validate_unique(field: &str, ids: &[String]) -> Result<()> {
    let mut seen = HashSet::new();
    for id in ids {
        if !seen.insert(id) {
            bail!("context_management.{field} contains duplicate ID {id}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(id: &str) -> ContextItem {
        ContextItem::tool_interaction(
            id.into(),
            json!({"type":"function_call","call_id":id,"name":"shell","arguments":"{}"}),
            json!({"type":"function_call_output","call_id":id,"output":"exact output"}),
        )
        .unwrap()
    }

    fn update(retain: &[&str], release: &[&str], memories: &[&str]) -> ContextManagement {
        ContextManagement {
            retain_volatile_ids: retain.iter().map(|value| (*value).into()).collect(),
            release_stable_ids: release.iter().map(|value| (*value).into()).collect(),
            add_memories: memories.iter().map(|value| (*value).into()).collect(),
        }
    }

    #[test]
    fn volatile_items_expire_when_omitted() {
        let mut state = ContextState::default();
        state
            .apply(&update(&["t1"], &[], &[]), Some(&tool("t1")), 3, 3)
            .unwrap();
        assert_eq!(state.volatile_status(), vec![("t1", 1)]);
        let change = state.apply(&update(&[], &[], &[]), None, 3, 3).unwrap();
        assert_eq!(change.dropped, vec!["t1"]);
        assert!(state.snapshot().is_empty());
    }

    #[test]
    fn promoted_stable_items_persist_until_explicitly_released() {
        let mut state = ContextState::default();
        state
            .apply(&update(&["t1"], &[], &[]), Some(&tool("t1")), 3, 3)
            .unwrap();
        state.apply(&update(&["t1"], &[], &[]), None, 3, 3).unwrap();
        let change = state.apply(&update(&["t1"], &[], &[]), None, 3, 3).unwrap();
        assert_eq!(change.promoted, vec!["t1"]);
        assert_eq!(state.stable_ids(), vec!["t1"]);
        state.apply(&update(&[], &[], &[]), None, 3, 3).unwrap();
        assert_eq!(state.stable_ids(), vec!["t1"]);
        let change = state.apply(&update(&[], &["t1"], &[]), None, 3, 3).unwrap();
        assert_eq!(change.released, vec!["t1"]);
        assert!(state.stable_ids().is_empty());
    }

    #[test]
    fn only_contiguous_volatile_prefix_promotes_and_markers_are_preserved() {
        let mut state = ContextState::default();
        state
            .apply(&update(&["t1"], &[], &[]), Some(&tool("t1")), 3, 3)
            .unwrap();
        state
            .apply(&update(&["t1", "t2"], &[], &[]), Some(&tool("t2")), 3, 3)
            .unwrap();
        state
            .apply(&update(&["t1", "t2"], &[], &[]), None, 3, 3)
            .unwrap();
        assert_eq!(state.stable_ids(), vec!["t1"]);
        state.apply(&update(&["t2"], &[], &[]), None, 3, 3).unwrap();
        assert_eq!(state.stable_ids(), vec!["t1"]);
        state.apply(&update(&["t2"], &[], &[]), None, 3, 3).unwrap();
        state.apply(&update(&["t2"], &[], &[]), None, 3, 3).unwrap();
        assert_eq!(state.stable_ids(), vec!["t1", "t2"]);
        let markers = state
            .input_items(None)
            .iter()
            .filter(|item| item["content"][0]["text"] == "[stable generation checkpoint]")
            .count();
        assert_eq!(markers, 2);
    }

    #[test]
    fn younger_head_blocks_older_looking_tail_from_promotion() {
        let mut state = ContextState {
            volatile: vec![tool("young"), tool("old")],
            ..ContextState::default()
        };
        state.volatile[0].retention_rounds = 1;
        state.volatile[1].retention_rounds = 5;
        state
            .apply(&update(&["young", "old"], &[], &[]), None, 3, 3)
            .unwrap();
        assert!(state.stable_ids().is_empty());
        assert_eq!(state.volatile_status(), vec![("young", 2), ("old", 6)]);
    }

    #[test]
    fn promotion_age_and_collection_interval_are_independent() {
        let mut state = ContextState::default();
        state
            .apply(&update(&["t1"], &[], &[]), Some(&tool("t1")), 2, 3)
            .unwrap();
        state.apply(&update(&["t1"], &[], &[]), None, 2, 3).unwrap();
        assert!(state.stable_ids().is_empty());
        let change = state.apply(&update(&["t1"], &[], &[]), None, 2, 3).unwrap();
        assert_eq!(change.promoted, vec!["t1"]);
    }

    #[test]
    fn new_memories_enter_volatile_tail_without_cache_metadata() {
        let mut state = ContextState::default();
        let change = state
            .apply(&update(&[], &[], &["remember this"]), None, 3, 3)
            .unwrap();
        assert_eq!(change.added, vec!["m0001"]);
        assert_eq!(state.volatile_status(), vec![("m0001", 1)]);
        assert!(
            state.input_items(None)[0]["content"][0]
                .get("prompt_cache_breakpoint")
                .is_none()
        );
    }

    #[test]
    fn redundant_stable_retains_are_harmless_but_invalid_releases_are_rejected() {
        let mut state = ContextState::default();
        state
            .apply(&update(&["t1"], &[], &[]), Some(&tool("t1")), 1, 1)
            .unwrap();
        let change = state.apply(&update(&["t1"], &[], &[]), None, 3, 3).unwrap();
        assert!(change.dropped.is_empty());
        assert_eq!(state.stable_ids(), vec!["t1"]);
        assert!(
            state
                .apply(&update(&[], &["missing"], &[]), None, 3, 3)
                .is_err()
        );
        let duplicate = ContextManagement {
            retain_volatile_ids: vec!["x".into(), "x".into()],
            ..ContextManagement::default()
        };
        assert!(state.apply(&duplicate, None, 3, 3).is_err());
    }
}
