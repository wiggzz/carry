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

/// One selectable unit of context. Its native Responses input items are replayed
/// unchanged and in creation order whenever the model retains its ID.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct ContextItem {
    pub id: String,
    pub kind: ContextItemKind,
    pub bytes: usize,
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
            input_items,
        })
    }

    fn memory(id: String, content: String) -> Self {
        let input_items = vec![json!({
            "role": "user",
            "content": format!("[retained memory {id}]\n{content}")
        })];
        Self {
            id,
            kind: ContextItemKind::Memory,
            bytes: serialized_bytes(&input_items),
            input_items,
        }
    }
}

fn serialized_bytes(items: &[Value]) -> usize {
    serde_json::to_vec(items).map_or(usize::MAX, |bytes| bytes.len())
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ContextState {
    retained: Vec<ContextItem>,
    next_memory_id: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct ContextChange {
    pub retained: Vec<String>,
    pub dropped: Vec<String>,
    pub added: Vec<String>,
    pub bytes: usize,
}

impl ContextState {
    pub fn input_items(&self, latest: Option<&ContextItem>) -> Vec<Value> {
        let mut input = Vec::new();
        for item in &self.retained {
            input.extend(item.input_items.iter().cloned());
        }
        if let Some(item) = latest
            && !self.retained.iter().any(|retained| retained.id == item.id)
        {
            input.extend(item.input_items.iter().cloned());
        }
        input
    }

    pub fn retained_ids(&self) -> Vec<&str> {
        self.retained.iter().map(|item| item.id.as_str()).collect()
    }

    pub fn snapshot(&self) -> &[ContextItem] {
        &self.retained
    }

    pub fn retained_bytes(&self) -> usize {
        self.retained.iter().map(|item| item.bytes).sum()
    }

    /// `retain_ids` is the complete set of existing items that survives.
    /// The latest tool interaction is eligible even though it is not yet retained.
    pub fn apply(
        &mut self,
        update: &ContextManagement,
        latest: Option<&ContextItem>,
    ) -> Result<ContextChange> {
        let mut available = self
            .retained
            .iter()
            .map(|item| item.id.as_str())
            .collect::<HashSet<_>>();
        if let Some(item) = latest {
            available.insert(item.id.as_str());
        }

        let mut seen = HashSet::new();
        for id in &update.retain_ids {
            if !available.contains(id.as_str()) {
                bail!("context_management.retain_ids references unknown ID {id}");
            }
            if !seen.insert(id.as_str()) {
                bail!("context_management.retain_ids contains duplicate ID {id}");
            }
        }

        let retain = update
            .retain_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let dropped = self
            .retained
            .iter()
            .filter(|item| !retain.contains(item.id.as_str()))
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        let mut candidate = self
            .retained
            .iter()
            .filter(|item| retain.contains(item.id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if let Some(item) = latest
            && retain.contains(item.id.as_str())
            && !candidate.iter().any(|existing| existing.id == item.id)
        {
            candidate.push(item.clone());
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
            candidate.push(ContextItem::memory(id.clone(), content.to_owned()));
            added.push(id);
        }

        let bytes = candidate.iter().map(|item| item.bytes).sum::<usize>();

        self.retained = candidate;
        self.next_memory_id = next_memory_id;
        Ok(ContextChange {
            retained: update.retain_ids.clone(),
            dropped,
            added,
            bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(id: &str) -> ContextItem {
        ContextItem::tool_interaction(
            id.into(),
            json!({
                "type": "function_call",
                "call_id": "call_1",
                "name": "shell",
                "arguments": "{\"command\":\"cat file\"}"
            }),
            json!({
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "exact output"
            }),
        )
        .unwrap()
    }

    #[test]
    fn replays_exact_tool_pair_and_memory_in_order() {
        let mut state = ContextState::default();
        let tool = tool("t0001");
        let original = tool.input_items.clone();
        let first = state
            .apply(
                &ContextManagement {
                    retain_ids: vec!["t0001".into()],
                    add_memories: vec!["a conclusion".into()],
                },
                Some(&tool),
            )
            .unwrap();
        assert_eq!(first.added, vec!["m0001"]);
        let replay = state.input_items(None);
        assert_eq!(&replay[..2], original.as_slice());
        assert_eq!(replay[2]["role"], "user");

        state
            .apply(
                &ContextManagement {
                    retain_ids: vec!["t0001".into()],
                    add_memories: vec![],
                },
                None,
            )
            .unwrap();
        assert_eq!(state.input_items(None), original);
    }

    #[test]
    fn rejects_unknown_ids() {
        let mut state = ContextState::default();
        assert!(
            state
                .apply(
                    &ContextManagement {
                        retain_ids: vec!["t9999".into()],
                        add_memories: vec![],
                    },
                    None,
                )
                .is_err()
        );
    }
}
