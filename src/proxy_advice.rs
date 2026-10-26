//! Completed semantic advice, bound to the entire original observed prefix.
use super::*;
use crate::proxy_state::Memory;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Record {
    pub generation: u64,
    pub at: u64,
    pub len: usize,
    pub opinions: BTreeMap<u64, String>,
    pub drops: BTreeMap<u64, Vec<u64>>,
    pub memories: Vec<Memory>,
    pub anchors: BTreeMap<u64, u64>,
    pub exposed: Vec<u64>,
    pub next_memory_id: u64,
    pub feedback: Value,
}

pub(super) fn scope(config: &ProxyCli, context: &Value) -> String {
    let mut context = context.clone();
    if let Some(object) = context.as_object_mut() {
        object.remove("prompt_cache_key");
    }
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&json!({
                "policy":"carry-prefix-advice-v3", "context":context,
                "upstream":config.upstream_url, "subscription":config.codex_login,
                "reviewer":config.classifier_model, "reviewer_url":config.classifier_url,
                "reasoning":config.classifier_reasoning_effort,
                "reviewer_output":config.classifier_max_output_tokens
            }))
            .unwrap()
        )
    )
}

impl Record {
    pub fn capture(session: &Session) -> Self {
        let mut opinions = session.opinions.clone();
        for group in session.groups().into_iter().filter(|g| g.pinned) {
            for member in group.members {
                opinions.insert(member, "keep".into());
            }
        }
        Self {
            generation: session.review_epoch,
            at: now(),
            len: session.history.len(),
            opinions,
            drops: session.drop_members.clone(),
            memories: session.memories.clone(),
            anchors: session.memory_anchors.clone(),
            next_memory_id: session.next_memory_id,
            feedback: session.review_feedback.clone(),
            exposed: session
                .history
                .iter()
                .filter(|i| i.exposed)
                .map(|i| i.id)
                .collect(),
        }
    }

    pub fn apply(&self, session: &mut Session) {
        session.completed_review_len = self.len;
        if self.generation >= session.review_feedback_generation {
            session.review_feedback = self.feedback.clone();
            session.review_feedback_generation = self.generation;
        }
        // A completed original prefix is already reviewed authority. Newly
        // exposed suffix work follows ordinary admission, not a paid replay
        // bootstrap merely because a cold owner also contains later output.
        session.bootstrap_review_pending = false;
        let groups = session.groups();
        // Original member unions may span several canonical groups after a
        // projected separator omission. Require the entire native closure now;
        // same-call suffix growth invalidates it, unrelated later work does not.
        let valid = |members: &[u64]| {
            !members.is_empty()
                && members.iter().all(|id| {
                    groups
                        .iter()
                        .find(|g| g.members.contains(id))
                        .is_some_and(|g| {
                            g.replayable
                                && !g.pinned
                                && g.members.iter().all(|m| members.contains(m))
                        })
                })
        };
        session.opinions = self.opinions.clone();
        session.drop_members = self
            .drops
            .iter()
            .filter(|(_, members)| valid(members))
            .map(|(id, members)| (*id, members.clone()))
            .collect();
        session.memories = self
            .memories
            .iter()
            .filter(|m| valid(&m.source_ids))
            .cloned()
            .collect();
        session.memory_anchors = self
            .anchors
            .iter()
            .filter(|(id, _)| session.memories.iter().any(|m| m.id == **id))
            .map(|(id, anchor)| (*id, *anchor))
            .collect();
        session.next_memory_id = self.next_memory_id;
        for item in &mut session.history {
            item.exposed |= self.exposed.contains(&item.id);
        }
    }
}
