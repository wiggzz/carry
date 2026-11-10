//! Observation-time interval reporting over immutable cumulative ledgers.
//! Only one owner's boundary records and fixed-size totals are held at once.
use super::{DashboardUsage, Value, json};
use carry::core::UsageLedger;

#[derive(serde::Deserialize)]
struct Observation {
    version: u32,
    owner: String,
    sequence: u64,
    at: u64,
    order_at: u64,
    primary: UsageLedger,
    reviewer: UsageLedger,
    completed_requests: u64,
    failed_primaries: u64,
}

fn difference(last: &UsageLedger, before: &UsageLedger) -> Option<UsageLedger> {
    let mut delta = UsageLedger::default();
    for (out, last, before) in [
        (&mut delta.calls, last.calls, before.calls),
        (
            &mut delta.input_tokens,
            last.input_tokens,
            before.input_tokens,
        ),
        (
            &mut delta.output_tokens,
            last.output_tokens,
            before.output_tokens,
        ),
        (
            &mut delta.cached_tokens,
            last.cached_tokens,
            before.cached_tokens,
        ),
        (
            &mut delta.cache_write_tokens,
            last.cache_write_tokens,
            before.cache_write_tokens,
        ),
        (
            &mut delta.unavailable_cost_calls,
            last.unavailable_cost_calls,
            before.unavailable_cost_calls,
        ),
        (
            &mut delta.cache_observation.calls,
            last.cache_observation.calls,
            before.cache_observation.calls,
        ),
        (
            &mut delta.cache_observation.input_tokens,
            last.cache_observation.input_tokens,
            before.cache_observation.input_tokens,
        ),
        (
            &mut delta.cache_observation.cached_tokens,
            last.cache_observation.cached_tokens,
            before.cache_observation.cached_tokens,
        ),
    ] {
        *out = last.checked_sub(before)?;
    }
    delta.cost_usd = last.cost_usd - before.cost_usd;
    (last.cost_usd.is_finite()
        && before.cost_usd.is_finite()
        && before.cost_usd >= 0.0
        && delta.cost_usd >= 0.0
        && delta.cost_usd.is_finite()
        && delta.unavailable_cost_calls <= delta.calls)
        .then_some(delta)
}

#[derive(Default)]
struct Owner {
    id: String,
    opening: Option<Observation>,
    closing: Option<Observation>,
    relevant: bool,
    incompatible: bool,
    ambiguous_clock: bool,
}

#[derive(Default, serde::Serialize)]
struct Coverage {
    observed_owners: u64,
    included_owners: u64,
    missing_opening_owners: u64,
    incompatible_owners: u64,
    ambiguous_clock_owners: u64,
}

pub(super) struct Window {
    start: u64,
    end: u64,
    retention_start: u64,
    owner: Option<Owner>,
    coverage: Coverage,
    usage: DashboardUsage,
    completed_requests: u64,
    failed_primaries: u64,
}

impl Window {
    pub(super) fn new(start: u64, end: u64, retention_start: u64) -> Self {
        Self {
            start,
            end,
            retention_start,
            owner: None,
            coverage: Coverage::default(),
            usage: DashboardUsage::default(),
            completed_requests: 0,
            failed_primaries: 0,
        }
    }

    pub(super) fn observe(
        &mut self,
        owner: &str,
        key: &str,
        at: u64,
        data: &[u8],
    ) -> std::io::Result<()> {
        if self
            .owner
            .as_ref()
            .is_none_or(|current| current.id != owner)
        {
            self.finish_owner()?;
            self.owner = Some(Owner {
                id: owner.into(),
                ..Owner::default()
            });
        }
        let observation: Observation = serde_json::from_slice(data)?;
        if observation.version != 1
            || observation.owner != owner
            || observation.order_at != at
            || format!("{:064x}", observation.sequence) != key
        {
            return Err(std::io::Error::other(
                "invalid accounting interval authority",
            ));
        }
        let current = self.owner.as_mut().unwrap();
        current.relevant |= at <= self.end || observation.at <= self.end;
        // Monotonic index ordering cannot cure a backward original clock.
        current.ambiguous_clock |= observation.at != at;
        if at > self.end {
            return Ok(());
        }
        if at <= self.start {
            current.opening = Some(observation);
        } else {
            if let Some(previous) = current.closing.as_ref().or(current.opening.as_ref()) {
                current.incompatible |= difference(&observation.primary, &previous.primary)
                    .is_none()
                    || difference(&observation.reviewer, &previous.reviewer).is_none()
                    || observation.completed_requests < previous.completed_requests
                    || observation.failed_primaries < previous.failed_primaries;
            }
            current.closing = Some(observation);
        }
        Ok(())
    }

    fn finish_owner(&mut self) -> std::io::Result<()> {
        let Some(owner) = self.owner.take().filter(|owner| owner.relevant) else {
            return Ok(());
        };
        self.coverage.observed_owners += 1;
        if owner.ambiguous_clock {
            self.coverage.ambiguous_clock_owners += 1;
        } else if owner.incompatible {
            self.coverage.incompatible_owners += 1;
        } else if let Some(opening) = owner.opening {
            let closing = owner.closing.as_ref().unwrap_or(&opening);
            let primary = difference(&closing.primary, &opening.primary);
            let reviewer = difference(&closing.reviewer, &opening.reviewer);
            if let (Some(primary), Some(reviewer), Some(completed), Some(failed)) = (
                primary,
                reviewer,
                closing
                    .completed_requests
                    .checked_sub(opening.completed_requests),
                closing
                    .failed_primaries
                    .checked_sub(opening.failed_primaries),
            ) {
                self.usage.observe(&primary, &reviewer)?;
                let overflow = || std::io::Error::other("accounting interval totals overflow");
                self.completed_requests = self
                    .completed_requests
                    .checked_add(completed)
                    .ok_or_else(overflow)?;
                self.failed_primaries = self
                    .failed_primaries
                    .checked_add(failed)
                    .ok_or_else(overflow)?;
                self.coverage.included_owners += 1;
            } else {
                self.coverage.incompatible_owners += 1;
            }
        } else {
            // A first retained paid snapshot is not evidence of creation at zero.
            self.coverage.missing_opening_owners += 1;
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> std::io::Result<Value> {
        self.finish_owner()?;
        self.usage.unavailable_owners =
            self.coverage.observed_owners - self.coverage.included_owners;
        Ok(json!({"start_at":self.start,"end_at":self.end,
            "basis":"closing_minus_opening_per_owner_observation_time_start_exclusive_end_inclusive",
            "population":"retained_observed_owners_with_compatible_boundary_evidence",
            "retention_start_at":self.retention_start,
            "retention_truncated":self.start < self.retention_start,
            "usage":self.usage,"completed_requests":self.completed_requests,
            "failed_primaries":self.failed_primaries,"coverage":self.coverage}))
    }
}
