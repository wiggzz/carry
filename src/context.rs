use std::collections::HashSet;

use anyhow::{Result, bail};
use serde::Serialize;

use crate::protocol::ContextManagement;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ContextItemKind {
    Memory,
    ToolResult,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ContextItem {
    pub id: String,
    pub kind: ContextItemKind,
    pub content: String,
}

impl ContextItem {
    pub fn tool_result(id: String, content: String) -> Self {
        Self {
            id,
            kind: ContextItemKind::ToolResult,
            content,
        }
    }

    fn memory(id: String, content: String) -> Self {
        Self {
            id,
            kind: ContextItemKind::Memory,
            content,
        }
    }

    fn render(&self) -> String {
        let kind = match self.kind {
            ContextItemKind::Memory => "memory",
            ContextItemKind::ToolResult => "tool_result",
        };
        format!(
            "<item id=\"{}\" kind=\"{kind}\" bytes=\"{}\">\n{}\n</item>",
            self.id,
            self.content.len(),
            self.content
        )
    }
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
    pub fn render_retained(&self) -> String {
        if self.retained.is_empty() {
            return "(empty)".to_owned();
        }
        self.retained
            .iter()
            .map(ContextItem::render)
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn render_latest(item: Option<&ContextItem>) -> String {
        match item {
            Some(item) => format!(
                "<item id=\"{}\" kind=\"tool_result\" bytes=\"{}\" retention=\"automatic_for_this_step_only\">\n{}\n</item>",
                item.id,
                item.content.len(),
                item.content
            ),
            None => "(none; no shell action has run yet)".to_owned(),
        }
    }

    pub fn snapshot(&self) -> &[ContextItem] {
        &self.retained
    }

    pub fn retained_bytes(&self) -> usize {
        self.retained.iter().map(|item| item.content.len()).sum()
    }

    /// `retain_ids` is the complete set of existing items that survives.
    /// The latest tool result is eligible for retention even though it is not yet retained.
    pub fn apply(
        &mut self,
        update: &ContextManagement,
        latest: Option<&ContextItem>,
        budget: usize,
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

        let bytes = candidate
            .iter()
            .map(|item| item.content.len())
            .sum::<usize>();
        if bytes > budget {
            bail!("retained context would use {bytes} bytes, exceeding the {budget}-byte budget");
        }

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

    #[test]
    fn retains_exact_tool_results_and_memories_by_id() {
        let mut state = ContextState::default();
        let tool = ContextItem::tool_result("t0001".into(), "exact output".into());
        let first = state
            .apply(
                &ContextManagement {
                    retain_ids: vec!["t0001".into()],
                    add_memories: vec!["a conclusion".into()],
                },
                Some(&tool),
                100,
            )
            .unwrap();
        assert_eq!(first.added, vec!["m0001"]);

        state
            .apply(
                &ContextManagement {
                    retain_ids: vec!["t0001".into()],
                    add_memories: vec![],
                },
                None,
                100,
            )
            .unwrap();
        assert!(state.render_retained().contains("exact output"));
        assert!(!state.render_retained().contains("a conclusion"));
    }

    #[test]
    fn rejects_unknown_ids_and_budget_overflow() {
        let mut state = ContextState::default();
        assert!(
            state
                .apply(
                    &ContextManagement {
                        retain_ids: vec!["t9999".into()],
                        add_memories: vec![],
                    },
                    None,
                    100,
                )
                .is_err()
        );
        assert!(
            state
                .apply(
                    &ContextManagement {
                        retain_ids: vec![],
                        add_memories: vec!["too long".into()],
                    },
                    None,
                    3,
                )
                .is_err()
        );
    }
}
