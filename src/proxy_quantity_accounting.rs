// Neutral accounting estimates owned by InputMeasurements, not planner forecasts.
// Equal residual shares are an explicit exchangeability assumption. The sum of
// ALL allocations in one measured block is exactly its native input increment;
// no member's native size, statistical interval or causal savings is asserted.

impl InputMeasurements {
    fn residual_allocation(&self, values: &[Value], input: u64) -> Option<ResidualAllocation> {
        if !values.iter().any(crate::savings::prompt_has_opaque) {
            return None;
        }
        let mut tokens = Vec::new();
        let mut opaque = Vec::new();
        let mut visible = 0u64;
        for (index, value) in values.iter().enumerate() {
            if crate::savings::prompt_has_opaque(value) {
                if !accounting_reasoning(value) {
                    return None;
                }
                opaque.push(index);
            }
            let count = self.text_tokens(value, true)?;
            tokens.push(count);
            visible = visible.checked_add(count)?;
        }
        if opaque.is_empty() {
            return None;
        }
        let residual = input.checked_sub(visible)?;
        let count = u64::try_from(opaque.len()).ok()?;
        for (rank, index) in opaque.into_iter().enumerate() {
            tokens[index] = tokens[index]
                .checked_add(residual / count)?
                .checked_add(u64::from((rank as u64) < residual % count))?;
        }
        Some(ResidualAllocation {
            tokens,
            residual,
            opaque_items: count,
        })
    }

    fn fully_measured(&self, changed: &[&Item]) -> bool {
        self.measured_changes(changed)
            .1
            .into_iter()
            .all(|used| used)
    }

    // All compatible retained native increments contribute; an initial native
    // total cannot donate its unexplained residual to a selected opaque item.
    fn calibrated_residual(&self) -> Option<u64> {
        if self.reasoning_conflict {
            return None;
        }
        let (mut residual, mut count) = (0u64, 0u64);
        for allocation in self.blocks.iter().filter_map(|b| b.allocation.as_ref()) {
            residual = residual.checked_add(allocation.residual)?;
            count = count.checked_add(allocation.opaque_items)?;
        }
        residual.checked_div(count)
    }

    /// One neutral quantity owner for planning and accounting. Whole measured
    /// extents win; partial/inherited residual shares remain explicit estimates.
    /// Missing evidence remains None. The result is never a native measurement.
    pub(crate) fn accounting_restoration(
        &self,
        canonical: &Value,
        submitted: &Value,
        route: &str,
        resolved: Option<&ResolvedModel>,
    ) -> Option<RestorationEstimate> {
        let expected = contract(canonical, route, resolved);
        if expected != contract(submitted, route, resolved) {
            return None;
        }
        let canonical_values = items(canonical)?;
        let submitted_values = items(submitted)?;
        let canonical_prompt = prompt(&canonical_values);
        let submitted_prompt = prompt(&submitted_values);
        let (removed, added) = changes(
            submitted_prompt.items.as_deref()?,
            canonical_prompt.items.as_deref()?,
        )?;
        let mut available = BTreeMap::new();
        for value in canonical_values.iter().chain(&submitted_values) {
            available.insert(identity(value), value);
        }
        // A known native block without a valid internal allocation is NOT an
        // unmeasured item. In particular a rejected visible-floor budget must
        // never borrow another block's residual. Legacy blocks can recover above
        // only from complete membership; otherwise partial attribution stays
        // unavailable. Whole-block native authority is independent of this.
        for changed in [&removed, &added] {
            for block in &self.blocks {
                if block.opaque
                    && block.allocation.is_none()
                    && changed
                        .iter()
                        .any(|item| block.items.contains(&item.identity))
                    && !changed.windows(block.items.len().max(1)).any(|window| {
                        window.len() == block.items.len()
                            && window
                                .iter()
                                .zip(&block.items)
                                .all(|(item, id)| item.identity == *id)
                    })
                {
                    return None;
                }
            }
        }
        if let Some(tokens) = self.restoration(canonical, submitted, route, resolved) {
            let measured = (removed.is_empty() && added.is_empty())
                || (self.contract == expected
                    && self.fully_measured(&removed)
                    && self.fully_measured(&added));
            return Some(RestorationEstimate {
                tokens,
                measured,
                estimated_opaque_items: 0,
            });
        }
        if self.reasoning_conflict || self.contract != expected {
            return None;
        }
        // A complete inherited native prefix is also one residual cohort.
        // Account for fixed overhead/protected items and complete measured
        // blocks first. No ciphertext length or planner-specific weighting.
        let inherited = self
            .unmeasured_pool(&available, canonical)
            .and_then(|(ids, budget)| {
                let values = ids
                    .iter()
                    .map(|id| available.get(id).map(|v| (*v).clone()))
                    .collect::<Option<Vec<_>>>()?;
                Some((ids, self.residual_allocation(&values, budget)?))
            });
        let mut pool_residual = 0u64;
        let mut pool_count = 0u64;
        for allocation in self
            .blocks
            .iter()
            .filter_map(|block| block.allocation.as_ref())
        {
            pool_residual = pool_residual.checked_add(allocation.residual)?;
            pool_count = pool_count.checked_add(allocation.opaque_items)?;
        }
        if let Some((_, allocation)) = &inherited {
            pool_residual = pool_residual.checked_add(allocation.residual)?;
            pool_count = pool_count.checked_add(allocation.opaque_items)?;
        }
        let changed_quantity = |changed: &[&Item]| -> Option<(i128, u64)> {
            // Whole measured blocks win over all member allocations.
            let (mut quantity, used) = self.measured_changes(changed);
            let mut estimated = 0u64;
            for (item, used) in changed.iter().zip(used) {
                if used {
                    continue;
                }
                let value = *available.get(&item.identity)?;
                if !item.opaque {
                    quantity = quantity.checked_add(self.text_tokens(value, true)? as i128)?;
                    continue;
                }
                if !accounting_reasoning(value) {
                    return None;
                }
                let mut native_allocated = None;
                for block in &self.blocks {
                    if let Some(allocation) = &block.allocation {
                        for (index, id) in block.items.iter().enumerate() {
                            if id == &item.identity {
                                let n = *allocation.tokens.get(index)?;
                                if native_allocated.is_some_and(|prior| prior != n) {
                                    return None;
                                }
                                native_allocated = Some(n);
                            }
                        }
                    }
                }
                if native_allocated.is_none()
                    && let Some((ids, allocation)) = &inherited
                {
                    native_allocated = ids
                        .iter()
                        .position(|id| id == &item.identity)
                        .and_then(|index| allocation.tokens.get(index).copied());
                }
                let count = match native_allocated {
                    Some(n) => n,
                    None => self
                        .text_tokens(value, true)?
                        .checked_add(pool_residual.checked_div(pool_count)?)?,
                };
                quantity = quantity.checked_add(count as i128)?;
                estimated = estimated.checked_add(1)?;
            }
            Some((quantity, estimated))
        };
        let (removed, removed_estimated) = changed_quantity(&removed)?;
        let (added, added_estimated) = changed_quantity(&added)?;
        Some(RestorationEstimate {
            tokens: added.checked_sub(removed)?,
            measured: false,
            estimated_opaque_items: added_estimated.checked_add(removed_estimated)?,
        })
    }

    /// Hidden retained OUTPUT uses earlier native INPUT-increment calibration,
    /// not billed reasoning output tokens or ciphertext bytes. A previously
    /// unseen/initial opaque output without calibration is explicitly unknown.
    pub(crate) fn retained_reasoning_units(&self, count: u64) -> Option<u64> {
        if count == 0 {
            return Some(0);
        }
        self.calibrated_residual()?.checked_mul(count)
    }

    pub(crate) fn retained_reasoning_extra(&self, output: &Value) -> Option<(u64, u64)> {
        let count = retained_reasoning_count(output)?;
        Some((self.retained_reasoning_units(count)?, count))
    }
}

pub(crate) fn retained_reasoning_count(output: &Value) -> Option<u64> {
    let mut count = 0u64;
    for value in output.as_array()? {
        if crate::savings::prompt_has_opaque(value) {
            if !accounting_reasoning(value) {
                return None;
            }
            count = count.checked_add(1)?;
        }
    }
    Some(count)
}

pub(crate) fn accounting_retirement_supported(body: &Value) -> bool {
    !crate::savings::prompt_has_opaque(&crate::savings::prompt_overhead(body))
        && items(body).is_some_and(|values| {
            values.iter().all(|value| {
                !crate::savings::prompt_has_opaque(value) || accounting_reasoning(value)
            })
        })
}

// Presence/type validation only. Never count or weight the encrypted payload.
fn accounting_reasoning(value: &Value) -> bool {
    if value["type"] != "reasoning"
        || !value["encrypted_content"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    {
        return false;
    }
    let mut visible = value.clone();
    visible.as_object_mut().unwrap().remove("encrypted_content");
    !crate::savings::prompt_has_opaque(&visible)
}
