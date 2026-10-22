//! Durable, content-bound temporary protection. Expiry only permits review.
use super::{Group, Memory, Session};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct Lease {
    pub generation: u64,
    pub renewed_at_round: u64,
    pub members: Vec<u64>,
    pub source_sha256: String,
    pub lineage: u64,
    pub released_at_round: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct Pending {
    pub request_id: String,
    pub targets: BTreeMap<String, Lease>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct KeepLeases {
    pub version: u32,
    pub turns: u64,
    pub next_generation: u64,
    pub entries: BTreeMap<String, Lease>,
    pub pending: Option<Pending>,
    /// At most one additional paid attempt per unchanged target generation.
    /// Ordinary, growth-admitted reviews may still retry these targets.
    #[serde(default)]
    pub extra_attempted: BTreeMap<String, u64>,
}

impl Session {
    fn lease_identity(&self, group: &Group) -> String {
        let items = self
            .history
            .iter()
            .filter(|i| group.members.contains(&i.id))
            .map(|i| (&i.id, &i.cohort, &i.value))
            .collect::<Vec<_>>();
        format!("{:x}", Sha256::digest(serde_json::to_vec(&items).unwrap()))
    }

    pub fn initialize_keep_leases(&mut self, turns: u64) {
        if self.keep_leases.as_ref().is_none_or(|s| s.version != 1) {
            self.keep_leases = Some(KeepLeases {
                version: 1,
                turns,
                next_generation: 0,
                entries: BTreeMap::new(),
                pending: None,
                extra_attempted: BTreeMap::new(),
            });
        }
        if self.keep_leases.as_ref().unwrap().turns != turns {
            // A supported policy override is not a new history or a revocation
            // of accepted release provenance. Preserve released identities;
            // protected groups/facts receive a fresh full lease and generation.
            let groups = self.groups();
            let memories = self
                .memories
                .iter()
                .filter(|m| {
                    self.keep_leases
                        .as_ref()
                        .unwrap()
                        .entries
                        .get(&format!("m{}", m.id))
                        .is_some_and(|l| l.released_at_round.is_none())
                })
                .map(|m| m.id)
                .collect::<Vec<_>>();
            let state = self.keep_leases.as_mut().unwrap();
            state.turns = turns;
            state.pending = None;
            state.extra_attempted.clear();
            for group in &groups {
                if self.opinions.get(&group.id).is_some_and(|s| s == "keep") {
                    self.renew_group_lease(group);
                }
            }
            for id in memories {
                self.renew_memory_lease(id);
            }
        }
        let groups = self.groups();
        for group in &groups {
            let key = format!("g{}", group.id);
            if self.opinions.get(&group.id).is_some_and(|s| s == "keep")
                && !self
                    .keep_leases
                    .as_ref()
                    .unwrap()
                    .entries
                    .contains_key(&key)
            {
                self.renew_group_lease(group);
            }
        }
        let keys = groups
            .iter()
            .map(|g| format!("g{}", g.id))
            .chain(self.memories.iter().map(|m| format!("m{}", m.id)))
            .collect::<Vec<_>>();
        let state = self.keep_leases.as_mut().unwrap();
        state.entries.retain(|key, _| keys.contains(key));
        state.extra_attempted.retain(|key, _| keys.contains(key));
    }

    pub fn renew_group_lease(&mut self, group: &Group) {
        let source_sha256 = self.lease_identity(group);
        let Some(state) = &mut self.keep_leases else {
            return;
        };
        // Exhausted identities cannot be confused with a newer renewal.
        let Some(generation) = state.next_generation.checked_add(1) else {
            // A same-value explicit keep must invalidate every old target even
            // when a fresh generation cannot be represented. Sticky keep wins.
            state.entries.remove(&format!("g{}", group.id));
            return;
        };
        state.next_generation = generation;
        state.entries.insert(
            format!("g{}", group.id),
            Lease {
                generation,
                renewed_at_round: self.completed_requests,
                members: group.members.clone(),
                source_sha256,
                lineage: self.history_rebases,
                released_at_round: None,
            },
        );
    }

    pub fn lease_group_matches(&self, group: &Group, lease: &Lease) -> bool {
        group.replayable
            && group.exposed
            && !group.pinned
            && !group.human
            && lease.members == group.members
            && lease.lineage == self.history_rebases
            && lease.source_sha256 == self.lease_identity(group)
    }

    pub fn released_group(&self, group: &Group) -> bool {
        self.keep_leases
            .as_ref()
            .and_then(|s| s.entries.get(&format!("g{}", group.id)))
            .is_some_and(|lease| {
                lease.released_at_round.is_some() && self.lease_group_matches(group, lease)
            })
    }

    pub fn due_keep_targets(&self) -> BTreeMap<String, Lease> {
        self.keep_targets(4)
    }

    pub fn due_keep_target_count(&self) -> usize {
        self.keep_targets(usize::MAX).len()
    }

    fn keep_targets(&self, limit: usize) -> BTreeMap<String, Lease> {
        let Some(state) = &self.keep_leases else {
            return BTreeMap::new();
        };
        let mut due = self
            .groups()
            .iter()
            .filter_map(|group| {
                let key = format!("g{}", group.id);
                let lease = state.entries.get(&key)?;
                (lease.released_at_round.is_none()
                    && self.lease_group_matches(group, lease)
                    && self.opinions.get(&group.id).is_some_and(|s| s == "keep")
                    && self
                        .completed_requests
                        .saturating_sub(lease.renewed_at_round)
                        >= state.turns)
                    .then(|| {
                        (
                            key,
                            lease.clone(),
                            self.history
                                .iter()
                                .filter(|i| group.members.contains(&i.id))
                                .map(|i| i.value.to_string().len())
                                .sum::<usize>(),
                        )
                    })
            })
            .collect::<Vec<_>>();
        for memory in &self.memories {
            let key = format!("m{}", memory.id);
            if let Some(lease) = state.entries.get(&key)
                && lease.released_at_round.is_none()
                && self.lease_memory_matches(memory, lease)
                && self
                    .completed_requests
                    .saturating_sub(lease.renewed_at_round)
                    >= state.turns
            {
                due.push((key, lease.clone(), memory.text.len()));
            }
        }
        due.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
        due.into_iter()
            .take(limit)
            .map(|(key, lease, _)| (key, lease))
            .collect()
    }

    pub fn resolve_keep_targets(&mut self, pending: &Pending) {
        for (key, target) in &pending.targets {
            if let Some(id) = key.strip_prefix('m').and_then(|s| s.parse::<u64>().ok()) {
                let matches = self
                    .memories
                    .iter()
                    .find(|m| m.id == id)
                    .is_some_and(|m| self.lease_memory_matches(m, target));
                if matches
                    && let Some(state) = &mut self.keep_leases
                    && state.entries.get(key) == Some(target)
                {
                    state.entries.get_mut(key).unwrap().released_at_round =
                        Some(self.completed_requests);
                }
                continue;
            }
            let Some(id) = key.strip_prefix('g').and_then(|s| s.parse::<u64>().ok()) else {
                continue;
            };
            let Some(group) = self.groups().into_iter().find(|g| g.id == id) else {
                continue;
            };
            if !self.lease_group_matches(&group, target) {
                continue;
            }
            let Some(state) = &mut self.keep_leases else {
                continue;
            };
            if state.entries.get(key) != Some(target) {
                continue;
            }
            if self.opinions.get(&id).is_some_and(|s| s == "keep") {
                self.opinions.remove(&id);
                state.entries.get_mut(key).unwrap().released_at_round =
                    Some(self.completed_requests);
            }
        }
    }

    fn memory_identity(memory: &Memory) -> String {
        format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(memory.id, &memory.source_ids, &memory.text)).unwrap()
            )
        )
    }

    fn lease_memory_matches(&self, memory: &Memory, lease: &Lease) -> bool {
        lease.lineage == self.history_rebases
            && lease.members == memory.source_ids
            && lease.source_sha256 == Self::memory_identity(memory)
    }

    pub fn renew_memory_lease(&mut self, id: u64) {
        let Some(memory) = self.memories.iter().find(|m| m.id == id) else {
            return;
        };
        let source_sha256 = Self::memory_identity(memory);
        let Some(state) = &mut self.keep_leases else {
            return;
        };
        let key = format!("m{id}");
        let Some(generation) = state.next_generation.checked_add(1) else {
            state.entries.remove(&key);
            return;
        };
        state.next_generation = generation;
        state.entries.insert(
            key,
            Lease {
                generation,
                renewed_at_round: self.completed_requests,
                members: memory.source_ids.clone(),
                source_sha256,
                lineage: self.history_rebases,
                released_at_round: None,
            },
        );
    }

    pub fn released_memory_ids(&self) -> Vec<u64> {
        self.memories
            .iter()
            .filter(|m| {
                self.keep_leases
                    .as_ref()
                    .and_then(|s| s.entries.get(&format!("m{}", m.id)))
                    .is_some_and(|l| {
                        l.released_at_round.is_some() && self.lease_memory_matches(m, l)
                    })
            })
            .map(|m| m.id)
            .collect()
    }

    pub fn extra_keep_attempt_available(&self) -> bool {
        let targets = self.due_keep_targets();
        !targets.is_empty()
            && self.keep_leases.as_ref().is_some_and(|s| {
                targets
                    .iter()
                    .any(|(key, l)| s.extra_attempted.get(key) != Some(&l.generation))
            })
    }

    pub fn reserve_extra_keep_attempt(&mut self) {
        let targets = self.due_keep_targets();
        if let Some(state) = &mut self.keep_leases {
            for (key, lease) in targets {
                state.extra_attempted.insert(key, lease.generation);
            }
        }
    }
}
