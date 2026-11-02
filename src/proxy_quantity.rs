//! Provider-input observations independent of cost/comparison eligibility.
//! Persist only quantities and identities, never another copy of source bodies.
//! Visible deltas use additive per-item local units (v2), not whole-JSON BPE.
use carry::models::ResolvedModel;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

const MAX_MEASURED_ITEMS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EstimateBasis {
    ProviderReceipt,
    ReceiptMeasuredBlockProjection,
    ReceiptAnchoredTextDelta,
    LocalVisibleText,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InputEstimate {
    pub tokens: u64,
    pub basis: EstimateBasis,
}

#[derive(Clone, Default, Debug, Deserialize, Serialize)]
pub(crate) struct InputMeasurements {
    contract: String,
    anchor: Option<Anchor>,
    blocks: VecDeque<MeasuredBlock>,
    // Pure section quantities survive session clones, never process restart.
    // They cannot grant receipt/cache/lineage authority and retain no source.
    #[serde(skip)]
    text: Arc<Mutex<BTreeMap<String, u64>>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Item {
    identity: String,
    opaque: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Prompt {
    identity: String,
    item_count: usize,
    items: Option<Vec<Item>>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Anchor {
    prompt: Prompt,
    input: u64,
    at: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
struct MeasuredBlock {
    items: Vec<String>,
    input: u64,
}

fn identity(value: &impl Serialize) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}
fn contract(body: &Value, route: &str, resolved: Option<&ResolvedModel>) -> String {
    let settings = body.as_object().map(|body| {
        body.iter()
            .filter(|(key, _)| {
                !matches!(
                    key.as_str(),
                    "input"
                        | "stream"
                        | "store"
                        | "metadata"
                        | "client_metadata"
                        | "include"
                        | "max_output_tokens"
                        | "prompt_cache_key"
                        | "prompt_cache_options"
                        | "prompt_cache_retention"
                        | "prompt_cache_breakpoint"
                )
            })
            .collect::<BTreeMap<_, _>>()
    });
    identity(&(
        2,
        settings,
        route,
        resolved.map(|model| (&model.model, &model.route, &model.raw.comp_hash)),
    ))
}
fn prompt(input: &[Value]) -> Prompt {
    Prompt {
        identity: identity(&input),
        item_count: input.len(),
        items: (input.len() <= MAX_MEASURED_ITEMS).then(|| {
            input
                .iter()
                .map(|item| Item {
                    identity: identity(item),
                    opaque: crate::savings::prompt_has_opaque(item),
                })
                .collect()
        }),
    }
}
fn items(body: &Value) -> Option<Vec<Value>> {
    Some(crate::openai::semantic_prompt_input(
        body["input"].as_array()?,
    ))
}
fn same_items(left: &[Item], right: &[Item]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.identity == right.identity)
}

/// Retained members must remain in order. Changed opaque members can only be
/// attributed as complete measured blocks, never split into guessed item sizes.
fn changes<'a>(from: &'a [Item], to: &'a [Item]) -> Option<(Vec<&'a Item>, Vec<&'a Item>)> {
    use std::collections::BTreeMap;
    let counts = |items: &'a [Item]| {
        let mut counts = BTreeMap::<&str, usize>::new();
        for item in items {
            *counts.entry(&item.identity).or_default() += 1;
        }
        counts
    };
    let from_counts = counts(from);
    let to_counts = counts(to);
    let mut common = BTreeMap::<&str, usize>::new();
    for (id, count) in &from_counts {
        common.insert(*id, (*count).min(*to_counts.get(id).unwrap_or(&0)));
    }
    let split = |items: &'a [Item]| {
        let mut remaining = common.clone();
        let mut retained = Vec::new();
        let mut changed = Vec::new();
        for item in items {
            let count = remaining.entry(&item.identity).or_default();
            if *count > 0 {
                *count -= 1;
                retained.push(item.identity.as_str());
            } else {
                changed.push(item);
            }
        }
        (retained, changed)
    };
    let (from_retained, removed) = split(from);
    let (to_retained, added) = split(to);
    if from_retained != to_retained {
        return None;
    }
    if removed.iter().chain(&added).any(|item| {
        item.opaque
            && (from_counts
                .get(item.identity.as_str())
                .copied()
                .unwrap_or(0)
                > 1
                || to_counts.get(item.identity.as_str()).copied().unwrap_or(0) > 1)
    }) {
        return None;
    }
    Some((removed, added))
}

impl InputMeasurements {
    /// Count a visible section only when a consumer asks for that quantity.
    /// The memo is bounded and content addressed; no native facts are inferred.
    pub(crate) fn visible_text_tokens(&self, value: &Value) -> u64 {
        self.text_tokens(value, true).unwrap()
    }

    fn text_tokens(&self, value: &Value, allow_count: bool) -> Option<u64> {
        let key = identity(value);
        if let Some(tokens) = self.text.lock().unwrap().get(&key) {
            return Some(*tokens);
        }
        if !allow_count {
            return None;
        }
        let tokens = crate::savings::local_text_tokens(value);
        let mut text = self.text.lock().unwrap();
        if text.len() >= MAX_MEASURED_ITEMS {
            text.pop_first();
        }
        text.insert(key, tokens);
        Some(tokens)
    }

    /// Additive local section units. Opaque placeholders are a visible floor,
    /// not a native image/reasoning token estimate.
    pub(crate) fn visible_prompt_tokens(&self, body: &Value) -> Option<u64> {
        self.visible_prompt_tokens_with(body, true)
    }

    fn visible_prompt_tokens_with(&self, body: &Value, allow_count: bool) -> Option<u64> {
        let input = items(body)?;
        let mut tokens = self.text_tokens(&crate::savings::prompt_overhead(body), allow_count)?;
        for item in &input {
            tokens = tokens.saturating_add(self.text_tokens(item, allow_count)?);
        }
        Some(tokens)
    }

    /// Settling a receipt records identities and native deltas, never text sizes.
    pub(crate) fn observe(
        &mut self,
        body: &Value,
        usage: &Value,
        route: &str,
        resolved: Option<&ResolvedModel>,
        at: u64,
    ) {
        let (Some(input), Some(values)) = (usage["input_tokens"].as_u64(), items(body)) else {
            return;
        };
        let prompt = prompt(&values);
        let contract = contract(body, route, resolved);
        if self.contract != contract {
            self.contract = contract;
            self.anchor = None;
            self.blocks.clear();
        }
        if let Some(previous) = &self.anchor {
            if at < previous.at {
                return;
            }
            if previous.prompt.identity == prompt.identity {
                if input != previous.input {
                    self.blocks.clear();
                }
            } else if let Some((previous_items, items)) = previous
                .prompt
                .items
                .as_deref()
                .zip(prompt.items.as_deref())
            {
                let appended = items.len() > previous_items.len()
                    && same_items(previous_items, &items[..previous_items.len()]);
                if appended && input < previous.input {
                    self.blocks.clear();
                } else if appended {
                    self.blocks.push_back(MeasuredBlock {
                        items: items[previous_items.len()..]
                            .iter()
                            .map(|item| item.identity.clone())
                            .collect(),
                        input: input - previous.input,
                    });
                    while self.blocks.len() > 128
                        || self
                            .blocks
                            .iter()
                            .map(|block| block.items.len())
                            .sum::<usize>()
                            > MAX_MEASURED_ITEMS
                    {
                        self.blocks.pop_front();
                    }
                }
            }
        }
        self.anchor = Some(Anchor { prompt, input, at });
    }

    fn changed_quantity(
        &self,
        changed: &[&Item],
        values: &[Value],
        measured: bool,
        allow_count: bool,
    ) -> Option<(i128, bool)> {
        let mut used = vec![false; changed.len()];
        let mut quantity = 0i128;
        let mut used_measurement = false;
        if measured {
            for block in self.blocks.iter().rev() {
                if block.items.is_empty() || block.items.len() > changed.len() {
                    continue;
                }
                let start =
                    changed
                        .windows(block.items.len())
                        .enumerate()
                        .find_map(|(start, items)| {
                            (!used[start..start + items.len()].iter().any(|used| *used)
                                && items
                                    .iter()
                                    .zip(&block.items)
                                    .all(|(item, id)| item.identity == *id))
                            .then_some(start)
                        });
                if let Some(start) = start {
                    used[start..start + block.items.len()].fill(true);
                    quantity += block.input as i128;
                    used_measurement = true;
                }
            }
        }
        // Decide unknown before invoking any text tokenizer.
        if changed
            .iter()
            .zip(&used)
            .any(|(item, used)| item.opaque && !*used)
        {
            return None;
        }
        let mut available = BTreeMap::new();
        for value in values {
            available.entry(identity(value)).or_insert(value);
        }
        for (item, used) in changed.iter().zip(used) {
            if !used {
                let cached = self.text.lock().unwrap().get(&item.identity).copied();
                let tokens = match cached {
                    Some(tokens) => tokens,
                    None => self.text_tokens(available.get(&item.identity)?, allow_count)?,
                };
                quantity += tokens as i128;
            }
        }
        Some((quantity, used_measurement))
    }

    fn delta(
        &self,
        from: &Prompt,
        to: &Prompt,
        from_values: &[Value],
        to_values: &[Value],
        measured: bool,
        allow_count: bool,
    ) -> Option<(i128, bool)> {
        if from.identity == to.identity {
            return Some((0, false));
        }
        let (removed, added) = changes(from.items.as_deref()?, to.items.as_deref()?)?;
        let (removed, removed_measured) =
            self.changed_quantity(&removed, from_values, measured, allow_count)?;
        let (added, added_measured) =
            self.changed_quantity(&added, to_values, measured, allow_count)?;
        Some((added - removed, removed_measured || added_measured))
    }

    pub(crate) fn predict(
        &self,
        body: &Value,
        route: &str,
        resolved: Option<&ResolvedModel>,
    ) -> Option<InputEstimate> {
        self.predict_with(body, route, resolved, true)
    }

    fn predict_with(
        &self,
        body: &Value,
        route: &str,
        resolved: Option<&ResolvedModel>,
        allow_count: bool,
    ) -> Option<InputEstimate> {
        if self.contract != contract(body, route, resolved) {
            return None;
        }
        let anchor = self.anchor.as_ref()?;
        let values = items(body)?;
        let target = prompt(&values);
        if anchor.prompt.identity == target.identity {
            return Some(InputEstimate {
                tokens: anchor.input,
                basis: EstimateBasis::ProviderReceipt,
            });
        }
        let (delta, measured) =
            self.delta(&anchor.prompt, &target, &[], &values, true, allow_count)?;
        Some(InputEstimate {
            tokens: u64::try_from(anchor.input as i128 + delta).ok()?,
            basis: if measured {
                EstimateBasis::ReceiptMeasuredBlockProjection
            } else {
                EstimateBasis::ReceiptAnchoredTextDelta
            },
        })
    }

    pub(crate) fn estimate(
        &self,
        body: &Value,
        route: &str,
        resolved: Option<&ResolvedModel>,
    ) -> Option<InputEstimate> {
        if let Some(estimate) = self.predict(body, route, resolved) {
            return Some(estimate);
        }
        if crate::savings::prompt_has_opaque(body) {
            return None;
        }
        self.visible_prompt_tokens(body)
            .map(|tokens| InputEstimate {
                tokens,
                basis: EstimateBasis::LocalVisibleText,
            })
    }

    /// Telemetry may reuse native or already counted local quantities, never
    /// initiate text tokenization merely to populate a dashboard field.
    pub(crate) fn known_estimate(
        &self,
        body: &Value,
        route: &str,
        resolved: Option<&ResolvedModel>,
    ) -> Option<InputEstimate> {
        if let Some(estimate) = self.predict_with(body, route, resolved, false) {
            return Some(estimate);
        }
        if crate::savings::prompt_has_opaque(body) {
            return None;
        }
        self.visible_prompt_tokens_with(body, false)
            .map(|tokens| InputEstimate {
                tokens,
                basis: EstimateBasis::LocalVisibleText,
            })
    }

    /// Evaluate a candidate from a currently available source prompt. Only its
    /// changed sections need text quantities, including old sections removed by
    /// the candidate. Raw source remains owned by the caller for this request.
    pub(crate) fn projected_estimate(
        &self,
        target: &Value,
        source: &Value,
        route: &str,
        resolved: Option<&ResolvedModel>,
    ) -> Option<InputEstimate> {
        if let Some(estimate) = self.predict(target, route, resolved) {
            return Some(estimate);
        }
        let source_estimate = self.estimate(source, route, resolved)?;
        if source_estimate.basis == EstimateBasis::LocalVisibleText {
            // Native batch quantities cannot be subtracted from local section
            // units. Reuse those local sections for a purely local candidate.
            return self.estimate(target, route, resolved);
        }
        let (restoration, measured) = self.restoration_with(source, target, route, resolved)?;
        Some(InputEstimate {
            tokens: u64::try_from(source_estimate.tokens as i128 - restoration).ok()?,
            basis: if measured {
                EstimateBasis::ReceiptMeasuredBlockProjection
            } else {
                EstimateBasis::ReceiptAnchoredTextDelta
            },
        })
    }

    pub(crate) fn restoration(
        &self,
        canonical: &Value,
        submitted: &Value,
        route: &str,
        resolved: Option<&ResolvedModel>,
    ) -> Option<i128> {
        self.restoration_with(canonical, submitted, route, resolved)
            .map(|(delta, _)| delta)
    }

    fn restoration_with(
        &self,
        canonical: &Value,
        submitted: &Value,
        route: &str,
        resolved: Option<&ResolvedModel>,
    ) -> Option<(i128, bool)> {
        let expected = contract(canonical, route, resolved);
        if expected != contract(submitted, route, resolved) {
            return None;
        }
        let canonical_values = items(canonical)?;
        let submitted_values = items(submitted)?;
        self.delta(
            &prompt(&submitted_values),
            &prompt(&canonical_values),
            &submitted_values,
            &canonical_values,
            self.contract == expected,
            true,
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn body() -> Value {
        json!({"model":"gpt-6.1-sol","instructions":"stable", "input":[{"role":"user","content":"initial"}]})
    }
    fn usage(input: u64) -> Value {
        json!({"input_tokens":input})
    }
    fn image(value: &str) -> Value {
        json!({"role":"user","content":[{"type":"input_image","image_url":format!("data:image/png;base64,{value}")}]})
    }
    #[test]
    fn receipt_input_survives_opaque_comparison_and_restart() {
        let mut body = body();
        body["input"]
            .as_array_mut()
            .unwrap()
            .push(image(&"abCD".repeat(20000)));
        let mut measured = InputMeasurements::default();
        assert!(measured.estimate(&body, "route", None).is_none());
        measured.observe(&body, &usage(55888), "route", None, 100);
        let saved = serde_json::to_string(&measured).unwrap();
        assert!(!saved.contains("image_url"));
        assert!(saved.len() < 3000);
        let measured: InputMeasurements = serde_json::from_str(&saved).unwrap();
        assert_eq!(
            measured.predict(&body, "route", None).unwrap().tokens,
            55888
        );
        body["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"continue"}));
        let delta =
            crate::savings::local_text_tokens(body["input"].as_array().unwrap().last().unwrap());
        assert_eq!(
            measured.predict(&body, "route", None).unwrap().tokens,
            55888 + delta
        );
    }
    #[test]
    fn new_opaque_input_needs_another_receipt() {
        let mut body = body();
        let mut measured = InputMeasurements::default();
        measured.observe(&body, &usage(1000), "route", None, 100);
        body["input"].as_array_mut().unwrap().push(image("new"));
        assert!(measured.predict(&body, "route", None).is_none());
        measured.observe(&body, &usage(3000), "route", None, 101);
        assert_eq!(measured.predict(&body, "route", None).unwrap().tokens, 3000);
    }
    #[test]
    fn whole_measured_append_block_can_be_removed_and_restored() {
        let initial = body();
        let mut later = initial.clone();
        let mut measured = InputMeasurements::default();
        measured.observe(&initial, &usage(1000), "route", None, 100);
        later["input"].as_array_mut().unwrap().extend([
            image("image"),
            json!({"role":"user","content":"batch sibling"}),
        ]);
        measured.observe(&later, &usage(9000), "route", None, 101);
        assert_eq!(
            measured.predict(&initial, "route", None).unwrap().tokens,
            1000
        );
        assert_eq!(
            measured.restoration(&later, &initial, "route", None),
            Some(8000)
        );
        let mut partial = later.clone();
        partial["input"].as_array_mut().unwrap().remove(1);
        assert!(
            measured.predict(&partial, "route", None).is_none(),
            "partial native batch cannot be attributed"
        );
        measured.observe(&initial, &usage(1000), "route", None, 102);
        assert_eq!(
            measured.predict(&later, "route", None).unwrap().tokens,
            9000
        );
    }
    #[test]
    fn unmeasured_opaque_prefix_can_cancel_for_text_only_projection() {
        let mut canonical = body();
        canonical["input"].as_array_mut().unwrap().extend([
            image("native"),
            json!({"role":"assistant","content":"old text ".repeat(50)}),
        ]);
        let mut submitted = canonical.clone();
        submitted["input"].as_array_mut().unwrap().pop();
        let measured = InputMeasurements::default();
        assert!(
            measured
                .restoration(&canonical, &submitted, "route", None)
                .unwrap()
                > 0
        );
    }
    #[test]
    fn identity_fences_observations_without_invented_scaling() {
        let original = body();
        let mut measured = InputMeasurements::default();
        measured.observe(&original, &usage(40000), "route", None, 100);
        assert!(measured.predict(&original, "other-route", None).is_none());
        let mut changed = original.clone();
        changed["instructions"] = json!("changed");
        assert!(measured.predict(&changed, "route", None).is_none());
        changed = original.clone();
        changed["model"] = json!("gpt-6-luna");
        assert!(measured.predict(&changed, "route", None).is_none());
        changed = original.clone();
        changed["max_output_tokens"] = json!(500);
        assert_eq!(
            measured.predict(&changed, "route", None).unwrap().tokens,
            40000
        );
    }
}

#[cfg(test)]
mod evidence_consistency_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn changed_native_count_for_unchanged_prompt_invalidates_prior_batch_attribution() {
        let initial = json!({"model":"gpt-6.1-sol","input":[{"role":"user","content":"initial"}]});
        let mut expanded = initial.clone();
        expanded["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":[
                {"type":"input_image","image_url":"data:image/png;base64,native"}
            ]}));
        let mut measured = InputMeasurements::default();
        measured.observe(&initial, &json!({"input_tokens":1000}), "route", None, 100);
        measured.observe(&expanded, &json!({"input_tokens":3000}), "route", None, 101);
        assert_eq!(
            measured.restoration(&expanded, &initial, "route", None),
            Some(2000)
        );
        measured.observe(&expanded, &json!({"input_tokens":4000}), "route", None, 102);
        assert_eq!(
            measured.predict(&expanded, "route", None).unwrap().tokens,
            4000
        );
        assert!(
            measured.predict(&initial, "route", None).is_none(),
            "native drift must invalidate earlier batch attribution, not just replace the total anchor"
        );
        assert!(
            measured
                .restoration(&expanded, &initial, "route", None)
                .is_none()
        );
    }

    #[test]
    fn reordered_or_duplicate_opaque_members_do_not_receive_per_item_attribution() {
        let initial = json!({"model":"gpt-6.1-sol","input":[{"role":"user","content":"initial"}]});
        let image = json!({"role":"user","content":[{"type":"input_image","image_url":"native"}]});
        let text = json!({"role":"assistant","content":"batch sibling"});
        let mut expanded = initial.clone();
        expanded["input"]
            .as_array_mut()
            .unwrap()
            .extend([image.clone(), text.clone()]);
        let mut measured = InputMeasurements::default();
        measured.observe(&initial, &json!({"input_tokens":1000}), "route", None, 100);
        measured.observe(&expanded, &json!({"input_tokens":3000}), "route", None, 101);
        let mut reordered = initial.clone();
        reordered["input"]
            .as_array_mut()
            .unwrap()
            .extend([text, image.clone()]);
        assert!(measured.predict(&reordered, "route", None).is_none());
        let mut duplicate = expanded.clone();
        duplicate["input"].as_array_mut().unwrap().push(image);
        assert!(measured.predict(&duplicate, "route", None).is_none());
    }
}

#[cfg(test)]
mod bounded_anchor_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn long_opaque_history_keeps_bounded_identity_and_exact_receipt_without_item_guesses() {
        let mut input =
            vec![json!({"role":"user","content":[{"type":"input_image","image_url":"native"}]})];
        input.extend((0..4096).map(|n| json!({"role":"assistant","content":format!("item {n}")})));
        let body = json!({"model":"gpt-6.1-sol","input":input});
        let mut measured = InputMeasurements::default();
        measured.observe(&body, &json!({"input_tokens":30000}), "route", None, 100);
        let saved = serde_json::to_string(&measured).unwrap();
        assert!(
            saved.len() < 3000,
            "anchor membership must not expand checkpoint with full long history"
        );
        let measured: InputMeasurements = serde_json::from_str(&saved).unwrap();
        assert_eq!(
            measured.predict(&body, "route", None).unwrap().tokens,
            30000
        );
        let mut changed = body.clone();
        changed["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"append"}));
        assert!(
            measured.predict(&changed, "route", None).is_none(),
            "changed opaque history beyond the membership bound must be unknown"
        );
    }
}

#[cfg(test)]
mod lazy_quantity_tests {
    use super::*;
    use crate::prompt_quantity::{reset_tokenized_bytes, tokenized_bytes};
    use serde_json::json;

    #[test]
    fn receipt_reuse_and_small_appends_do_not_retokenize_the_prefix() {
        let mut body = json!({"model":"gpt-6.1-sol","input":[
            {"role":"developer","content":"large stable prefix ".repeat(10000)},
            {"role":"user","content":"start"}
        ]});
        let mut measured = InputMeasurements::default();
        reset_tokenized_bytes();
        measured.observe(&body, &json!({"input_tokens":40000}), "route", None, 100);
        assert_eq!(
            tokenized_bytes(),
            0,
            "receipt settlement records native measurements without speculative text counting"
        );
        assert_eq!(
            measured.predict(&body, "route", None).unwrap().tokens,
            40000
        );
        assert_eq!(
            tokenized_bytes(),
            0,
            "exact receipt reuse must not invoke the tokenizer"
        );
        body["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":"small append"}));
        assert!(measured.predict(&body, "route", None).unwrap().tokens > 40000);
        assert!(
            tokenized_bytes() < 1000,
            "only the appended section needs text tokenization"
        );
        measured.observe(&body, &json!({"input_tokens":40020}), "route", None, 101);
        assert!(
            tokenized_bytes() < 2000,
            "settling an append must reuse existing item quantities"
        );
        reset_tokenized_bytes();
        assert_eq!(
            measured.predict(&body, "route", None).unwrap().tokens,
            40020
        );
        assert_eq!(tokenized_bytes(), 0);
    }

    #[test]
    fn reporting_only_uses_previously_known_quantities() {
        let body = json!({"model":"gpt-6.1-sol","input":[{"role":"user","content":"new large prompt ".repeat(10000)}]});
        let measured = InputMeasurements::default();
        reset_tokenized_bytes();
        assert!(measured.known_estimate(&body, "route", None).is_none());
        assert_eq!(
            tokenized_bytes(),
            0,
            "reporting must not trigger speculative tokenization"
        );
        let counted = measured.estimate(&body, "route", None).unwrap();
        reset_tokenized_bytes();
        assert_eq!(measured.known_estimate(&body, "route", None), Some(counted));
        assert_eq!(tokenized_bytes(), 0);
    }

    #[test]
    fn unmeasured_opaque_input_is_unknown_without_tokenizing_visible_history() {
        let body = json!({"model":"gpt-6.1-sol","input":[
            {"role":"developer","content":"large stable prefix ".repeat(10000)},
            {"role":"user","content":[{"type":"input_image","image_url":"native"}]}
        ]});
        reset_tokenized_bytes();
        assert!(
            InputMeasurements::default()
                .estimate(&body, "route", None)
                .is_none()
        );
        assert_eq!(tokenized_bytes(), 0);
    }
}

#[cfg(test)]
mod projection_domain_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn local_projection_never_subtracts_a_native_batch_from_local_text_units() {
        let initial = json!({"model":"gpt-6.1-sol","input":[{"role":"user","content":"initial"}]});
        let mut source = initial.clone();
        source["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"assistant","content":"measured batch"}));
        let other = json!({"model":"gpt-6.1-sol","input":[{"role":"user","content":"different observed prompt"}]});
        let mut quantities = InputMeasurements::default();
        quantities.observe(&initial, &json!({"input_tokens":1000}), "route", None, 100);
        quantities.observe(&source, &json!({"input_tokens":4000}), "route", None, 101);
        quantities.observe(&other, &json!({"input_tokens":1200}), "route", None, 102);
        // Restart clears speculative section quantities, not native block facts.
        let quantities: InputMeasurements =
            serde_json::from_value(serde_json::to_value(quantities).unwrap()).unwrap();
        assert!(quantities.predict(&source, "route", None).is_none());
        assert!(quantities.predict(&initial, "route", None).is_none());
        let expected = quantities.visible_prompt_tokens(&initial).unwrap();
        let projected = quantities.projected_estimate(&initial, &source, "route", None);
        assert_eq!(
            projected,
            Some(InputEstimate {
                tokens: expected,
                basis: EstimateBasis::LocalVisibleText
            }),
            "a local source requires a local target quantity, not native-block subtraction"
        );
    }
}
