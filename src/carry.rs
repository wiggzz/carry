use std::collections::HashSet;

use anyhow::{Result, bail};
use serde::Serialize;

use crate::protocol::CarryUpdate;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct MemoryItem {
    id: String,
    text: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CarryState {
    items: Vec<MemoryItem>,
    next_id: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct CarryChange {
    pub kept: Vec<String>,
    pub dropped: Vec<String>,
    pub added: Vec<String>,
    pub bytes: usize,
}

impl CarryState {
    pub fn render(&self) -> String {
        if self.items.is_empty() {
            return "(empty)".to_owned();
        }
        self.items
            .iter()
            .map(|item| format!("[{}] {}", item.id, item.text))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn snapshot(&self) -> &[MemoryItem] {
        &self.items
    }

    /// Apply mark-and-sweep retention: only named IDs survive, then new items append.
    pub fn apply(&mut self, update: &CarryUpdate, budget: usize) -> Result<CarryChange> {
        let existing: HashSet<&str> = self.items.iter().map(|item| item.id.as_str()).collect();
        let mut seen = HashSet::new();
        for id in &update.keep {
            if !existing.contains(id.as_str()) {
                bail!("carry.keep references unknown ID {id}");
            }
            if !seen.insert(id.as_str()) {
                bail!("carry.keep contains duplicate ID {id}");
            }
        }

        let keep: HashSet<&str> = update.keep.iter().map(String::as_str).collect();
        let dropped = self
            .items
            .iter()
            .filter(|item| !keep.contains(item.id.as_str()))
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        let mut candidate = self
            .items
            .iter()
            .filter(|item| keep.contains(item.id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        let mut added = Vec::new();
        let mut next_id = self.next_id;
        for text in &update.add {
            let text = text.trim();
            if text.is_empty() {
                bail!("carry.add contains an empty item");
            }
            next_id += 1;
            let id = format!("m{next_id:04}");
            candidate.push(MemoryItem {
                id: id.clone(),
                text: text.to_owned(),
            });
            added.push(id);
        }

        let bytes = candidate.iter().map(|item| item.text.len()).sum::<usize>();
        if bytes > budget {
            bail!("carry would use {bytes} bytes, exceeding the {budget}-byte budget");
        }

        self.items = candidate;
        self.next_id = next_id;
        Ok(CarryChange {
            kept: update.keep.clone(),
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
    fn carry_is_mark_and_sweep() {
        let mut carry = CarryState::default();
        let first = carry
            .apply(
                &CarryUpdate {
                    keep: vec![],
                    add: vec!["stable".into(), "temporary".into()],
                },
                100,
            )
            .unwrap();
        assert_eq!(first.added, vec!["m0001", "m0002"]);

        let second = carry
            .apply(
                &CarryUpdate {
                    keep: vec!["m0001".into()],
                    add: vec!["new".into()],
                },
                100,
            )
            .unwrap();
        assert_eq!(second.dropped, vec!["m0002"]);
        assert_eq!(carry.render(), "[m0001] stable\n[m0003] new");
    }

    #[test]
    fn rejects_unknown_keep_and_budget_overflow() {
        let mut carry = CarryState::default();
        assert!(
            carry
                .apply(
                    &CarryUpdate {
                        keep: vec!["m9".into()],
                        add: vec![]
                    },
                    100,
                )
                .is_err()
        );
        assert!(
            carry
                .apply(
                    &CarryUpdate {
                        keep: vec![],
                        add: vec!["too long".into()]
                    },
                    3,
                )
                .is_err()
        );
    }
}
