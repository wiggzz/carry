//! Durable, content-bound temporary protection. Expiry only permits review.
use super::{Group, Session};
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
        if self
            .keep_leases
            .as_ref()
            .is_none_or(|s| s.version != 1 || s.turns != turns)
        {
            self.keep_leases = Some(KeepLeases {
                version: 1,
                turns,
                next_generation: 0,
                entries: BTreeMap::new(),
                pending: None,
            });
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
            .collect::<Vec<_>>();
        self.keep_leases
            .as_mut()
            .unwrap()
            .entries
            .retain(|key, _| keys.contains(key));
    }

    pub fn renew_group_lease(&mut self, group: &Group) {
        let source_sha256 = self.lease_identity(group);
        let Some(state) = &mut self.keep_leases else {
            return;
        };
        // Exhausted identities cannot be confused with a newer renewal.
        let Some(generation) = state.next_generation.checked_add(1) else {
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
        due.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
        due.into_iter()
            .take(4)
            .map(|(key, lease, _)| (key, lease))
            .collect()
    }

    pub fn resolve_keep_targets(&mut self, pending: &Pending) {
        for (key, target) in &pending.targets {
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
}
