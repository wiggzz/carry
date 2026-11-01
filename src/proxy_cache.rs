//! Exact cache identities without retaining another copy of native source.
use std::io::Write;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

struct IdentityWriter {
    digest: Sha256,
    bytes: usize,
}
impl Write for IdentityWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.digest.update(bytes);
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(super) fn serialized_identity<T: Serialize + ?Sized>(value: &T) -> (usize, String) {
    let mut writer = IdentityWriter {
        digest: Sha256::new(),
        bytes: 0,
    };
    serde_json::to_writer(&mut writer, value).expect("JSON cache identity");
    (writer.bytes, format!("{:x}", writer.digest.finalize()))
}

/// Semantic namespace for a hypothetical prompt. Quantity adapters own their
/// append/projection proof; this owner supplies the same fixed prompt/key/model
/// boundary to both. Policy selects eligibility, each write owns its lifetime,
/// and prices/audit/output controls cannot rename a still-compatible write.
pub(crate) fn modeled_scope(body: &Value, model: &str, source: impl Serialize) -> String {
    serialized_identity(&(
        model,
        source,
        body.get("prompt_cache_key"),
        crate::savings::prompt_overhead(body),
    ))
    .1
}

/// A conditional branch's writes, never provider observations or billed usage.
/// Separate extents are necessary: appending a newly written suffix cannot move
/// an older prefix's deadline. All times use the calling adapter's clock units.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct ModeledCache {
    scope: String,
    writes: Vec<ModeledWrite>,
    // Bounded discarded suffix coverage is uncertainty, never a cache miss.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unresolved: Option<UnresolvedCoverage>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
struct UnresolvedCoverage {
    start: u64,
    end: u64,
    since: u64,
    through: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
struct ModeledWrite {
    start: u64,
    end: u64,
    at: u64,
    lifetime: u64,
}
impl ModeledWrite {
    fn eligible_at(&self, at: u64) -> bool {
        write_eligible_at(self.at, self.at, self.lifetime, at)
    }
}

/// A bounded write is usable only after its latest possible completion and
/// before its earliest possible write's deadline. The same pure rule serves
/// modeled point writes and observed transport intervals; reads never renew it.
fn write_eligible_at(not_before: u64, not_after: u64, lifetime: u64, at: u64) -> bool {
    not_before <= not_after && at >= not_after && at - not_before < lifetime
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct WriteWindow {
    pub not_before: u64,
    pub not_after: u64,
    pub lifetime: u64,
}
impl WriteWindow {
    pub(super) fn eligible_at(&self, at: u64) -> bool {
        write_eligible_at(self.not_before, self.not_after, self.lifetime, at)
    }
}

/// Terminal time is an observation, not a write. Dispatch supplies only a lower
/// bound for a receipt reporting a complete write during that request.
#[derive(Clone, Copy)]
pub(super) struct ObservationTime {
    pub at: u64,
    dispatched_at: Option<u64>,
}
impl ObservationTime {
    pub fn during(dispatched_at: u64, at: u64) -> Self {
        Self {
            at,
            dispatched_at: Some(dispatched_at),
        }
    }
}
impl From<u64> for ObservationTime {
    fn from(at: u64) -> Self {
        Self {
            at,
            dispatched_at: None,
        }
    }
}
impl ModeledCache {
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Pure read-time contiguous-prefix query. Neither expiry nor a cache read
    /// records a write, refreshes a deadline, or consults today's policy TTL.
    pub(crate) fn eligible_at(&self, scope: &str, at: u64, input: u64) -> Option<u64> {
        if self.scope != scope {
            return Some(0);
        }
        let mut prefix = 0;
        for write in &self.writes {
            if write.start > prefix {
                break;
            }
            if write.eligible_at(at) {
                prefix = prefix.max(write.end);
            }
        }
        if self.unresolved.as_ref().is_some_and(|unknown| {
            at >= unknown.since
                && at <= unknown.through
                && unknown.start <= prefix
                && unknown.end > prefix
                && input > prefix
        }) {
            return None;
        }
        Some(prefix.min(input))
    }

    fn retain_uncertainty(&mut self, start: u64, end: u64, at: u64, lifetime: u64) {
        let through = at.saturating_add(lifetime - 1);
        if let Some(unknown) = &mut self.unresolved {
            unknown.start = unknown.start.min(start);
            unknown.end = unknown.end.max(end);
            unknown.since = unknown.since.min(at);
            unknown.through = unknown.through.max(through);
        } else {
            self.unresolved = Some(UnresolvedCoverage {
                start,
                end,
                since: at,
                through,
            });
        }
    }

    /// Record the hypothetical request's uncached extent. The owner derives it;
    /// callers cannot turn a missing/retired forecast into a new cold write.
    /// There is no supported read-refresh contract in this whole-prefix model.
    pub(crate) fn written(&mut self, scope: String, at: u64, lifetime: u64, input: u64) {
        if lifetime == 0 {
            return;
        }
        let cached = self.eligible_at(&scope, at, input);
        let Some(cached) = cached else {
            // The unknown partition may have written any of this suffix. Keep
            // that bound through its possible lifetime, not a fabricated write.
            let start = self.unresolved.as_ref().unwrap().start;
            self.retain_uncertainty(start, input, at, lifetime);
            return;
        };
        if self.scope != scope || cached == 0 {
            self.clear();
            self.scope = scope;
        }
        if input <= cached {
            return;
        }
        // Pruning/coalescing is a write-boundary operation, never a read effect.
        self.writes.retain(|write| write.eligible_at(at));
        if self
            .unresolved
            .as_ref()
            .is_some_and(|unknown| at > unknown.through)
        {
            self.unresolved = None;
        }
        if let Some(last) = self.writes.last_mut()
            && last.end == cached
            && last.at == at
            && last.lifetime == lifetime
        {
            // Exactly the same interval: no deadline, backward-time behavior or
            // prefix eligibility is lost by representing contiguous extents once.
            last.end = input;
            return;
        }
        self.writes.push(ModeledWrite {
            start: cached,
            end: input,
            at,
            lifetime,
        });
        if self.writes.len() > 128 {
            // Preserve the known leading prefix. Non-coalescible suffix loss is
            // explicit unknown, not zero credit consumed as a cold-write tariff.
            let discarded = self.writes.pop().unwrap();
            self.retain_uncertainty(
                discarded.start,
                discarded.end,
                discarded.at,
                discarded.lifetime,
            );
        }
    }
}

/// Actual dispatched wire identity. Purpose is deliberately absent.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct CacheEvidence {
    pub route: Option<carry::models::Route>,
    pub credential_sha256: String,
    pub base_sha256: String,
    pub input_sha256: String,
    pub input_len: usize,
    pub estimated_tokens: f64,
    pub at: u64,
    pub ttl_seconds: u64,
    pub native_cached_fraction: f64,
    // Raw receipt quantities prove a whole-input write; aggregate partial
    // writes cannot locate any particular prefix in local estimated units.
    // Legacy observations lack this proof and remain observations only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_write_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_window: Option<WriteWindow>,
}

pub(super) fn base(body: &Value) -> Value {
    let mut base = body.clone();
    if let Some(object) = base.as_object_mut() {
        for key in ["input", "stream"] {
            object.remove(key);
        }
    }
    base
}

fn input_identity(input: &[Value]) -> String {
    let mut input = input.to_vec();
    for item in &mut input {
        if let Some(blocks) = item.get_mut("content").and_then(Value::as_array_mut) {
            for block in blocks {
                if block["prompt_cache_breakpoint"]["mode"] == "explicit" {
                    block
                        .as_object_mut()
                        .unwrap()
                        .remove("prompt_cache_breakpoint");
                }
            }
        }
    }
    serialized_identity(&input).1
}

impl CacheEvidence {
    pub fn matches(
        &self,
        body: &Value,
        resolved: Option<&carry::models::ResolvedModel>,
        credential: &str,
        at: u64,
    ) -> bool {
        let Some(input) = body["input"].as_array() else {
            return false;
        };
        resolved.is_some_and(|model| model.cache.is_some())
            && self.at <= at
            && self.route.as_ref() == resolved.map(|r| &r.route)
            && self.credential_sha256 == credential
            && self.input_len <= input.len()
            && self.base_sha256 == serialized_identity(&base(body)).1
            && self.input_sha256 == input_identity(&input[..self.input_len])
            && (0.0..=1.0).contains(&self.native_cached_fraction)
    }

    fn written_prefix_at(&self, at: u64) -> Option<f64> {
        let total = self.native_input_tokens?;
        (total > 0
            && self.native_write_tokens == Some(total)
            && self.native_cached_fraction == 0.0
            && self
                .write_window
                .is_some_and(|window| window.eligible_at(at)))
        .then_some(self.estimated_tokens)
    }

    /// The same scoped proof owns both monetary forecast credit and planner
    /// prefix preservation. Identity alone is not evidence of live retention.
    pub fn reusable_at(
        &self,
        body: &Value,
        resolved: Option<&carry::models::ResolvedModel>,
        credential: &str,
        at: u64,
    ) -> Option<f64> {
        self.matches(body, resolved, credential, at)
            .then(|| self.written_prefix_at(at))
            .flatten()
    }
}

pub(super) fn reusable(
    cache: &[CacheEvidence],
    body: &Value,
    resolved: Option<&carry::models::ResolvedModel>,
    credential: &str,
    at: u64,
) -> f64 {
    if resolved.is_none() {
        return 0.0;
    }
    cache
        .iter()
        .filter_map(|e| e.reusable_at(body, resolved, credential, at))
        .fold(0.0, f64::max)
        .min(crate::proxy_savings::token_units(body) as f64)
}

pub(super) fn observe(
    cache: &mut Vec<CacheEvidence>,
    body: &Value,
    usage: &Value,
    resolved: Option<&carry::models::ResolvedModel>,
    credential: &str,
    time: ObservationTime,
) -> bool {
    let at = time.at;
    let Some(input) = body["input"].as_array() else {
        return false;
    };
    let Some(model) = resolved else {
        return false;
    };
    let Some(capability) = model.cache else {
        return false;
    };
    let ttl_seconds = model
        .scenario_cache_policy
        .as_ref()
        .map_or(0, |p| p.ttl_seconds);
    if ttl_seconds == 0 {
        return false;
    }
    let quantities = carry::core::CacheUsage::from_receipt(usage);
    let Some((total, cached)) = quantities.read() else {
        return false;
    };
    if total < capability.minimum_prefix_tokens as u64 {
        return false;
    }
    let end = if body["prompt_cache_options"]["mode"] == "explicit" {
        input
            .iter()
            .rposition(|item| {
                item["content"].as_array().is_some_and(|blocks| {
                    blocks
                        .iter()
                        .any(|block| block["prompt_cache_breakpoint"]["mode"] == "explicit")
                })
            })
            .map_or(input.len(), |i| i + 1)
    } else {
        input.len()
    };
    let input = &input[..end];
    let written = quantities.reported_write();
    let write_window = time.dispatched_at.and_then(|not_before| {
        // Capability plus a full native write receipt supplies request-interval
        // proof. Unknown/mixed lifetime and explicitly limited write extents do
        // not establish whole-input retention. Never infer population from hits.
        (quantities.whole_written_input() == Some(total)
            && end == body["input"].as_array().unwrap().len()
            && carry::models::cache_write_tariff_known(body)
            && not_before <= at)
            .then_some(WriteWindow {
                not_before,
                not_after: at,
                lifetime: ttl_seconds,
            })
    });
    let e = CacheEvidence {
        route: resolved.map(|r| r.route.clone()),
        credential_sha256: credential.into(),
        base_sha256: serialized_identity(&base(body)).1,
        input_sha256: input_identity(input),
        input_len: input.len(),
        estimated_tokens: crate::proxy_savings::token_units(&json!(input)) as f64,
        at,
        ttl_seconds,
        native_cached_fraction: cached as f64 / total as f64,
        native_input_tokens: Some(total),
        native_write_tokens: written,
        write_window,
    };
    // Keep an original timed write across full reads, never rewrite its raw
    // quantities or expiry with the new hit. New misses/unlocated partial hits
    // revoke compatible prefixes. Mutation order in the atomic history owner,
    // not timestamp ties, orders observations. Keep one latest raw observation
    // beside the immutable write fact; unrelated namespaces stay independent.
    cache.retain(|old| {
        old.base_sha256 != e.base_sha256
            || old.route != e.route
            || old.credential_sha256 != e.credential_sha256
            || old.input_len > input.len()
            || old.input_sha256 != input_identity(&input[..old.input_len])
            || (quantities.read_covers_whole_input() == Some(true)
                && old.written_prefix_at(at).is_some())
    });
    cache.push(e);
    trim(cache, at);
    true
}

pub(super) fn trim(cache: &mut Vec<CacheEvidence>, at: u64) {
    cache
        .retain(|e| e.at <= at && (e.written_prefix_at(at).is_some() || at - e.at < e.ttl_seconds));
    cache.sort_by_key(|e| e.at);
    while cache.len() > 128 || serde_json::to_vec(cache).unwrap().len() > 65536 {
        cache.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    include!("proxy_cache_partition_tests.rs");

    // Existing exact-credit controls must remain known, never accept None as0.
    fn eligible(cache: &ModeledCache, scope: &str, at: u64, input: u64) -> u64 {
        cache
            .eligible_at(scope, at, input)
            .expect("known modeled prefix")
    }

    // Boundary acceptance for the shared semantic owner. The real-coordinator
    // differential separately reproduces the original equivalent-options bug.
    #[test]
    fn modeled_namespace_preserves_writes_only_for_the_same_semantic_prompt_source() {
        let body = json!({"model":"gpt-6.1-sol","prompt_cache_key":"synthetic-key",
            "instructions":"fixed", "tools":[], "input":[{"role":"user","content":"prefix"}]});
        let scope = modeled_scope(&body, "gpt-6.1-sol", "source");
        let mut cache = ModeledCache::default();
        cache.written(scope.clone(), 100, 7, 2000);
        let original = serde_json::to_vec(&cache).unwrap();
        for controls in [
            json!({"prompt_cache_options":{}}),
            json!({"prompt_cache_options":{"mode":"implicit","ttl":"30m"}}),
            json!({"prompt_cache_options":{"mode":"implicit","ttl":"2h"}}),
            json!({"max_output_tokens":2,"temperature":0.5,"reasoning":{"effort":"high"},
                "metadata":{"synthetic":"audit"},"store":false,"stream":true,"include":[]}),
        ] {
            let mut next = body.clone();
            next.as_object_mut()
                .unwrap()
                .extend(controls.as_object().unwrap().clone());
            let next_scope = modeled_scope(&next, "gpt-6.1-sol", "source");
            assert_eq!(eligible(&cache, &next_scope, 106, 2000), 2000);
            // Even a newly requested TTL does not renew the original event.
            assert_eq!(eligible(&cache, &next_scope, 107, 2000), 0);
            assert_eq!(serde_json::to_vec(&cache).unwrap(), original);
        }
        for settings in [
            json!({"prompt_cache_key":"different-key"}),
            json!({"instructions":"different fixed input"}),
            json!({"tools":[{"type":"function","name":"different-tool"}]}),
            json!({"text":{"format":{"type":"json_object"}}}),
        ] {
            let mut next = body.clone();
            next.as_object_mut()
                .unwrap()
                .extend(settings.as_object().unwrap().clone());
            assert_eq!(
                eligible(
                    &cache,
                    &modeled_scope(&next, "gpt-6.1-sol", "source"),
                    106,
                    2000
                ),
                0
            );
        }
        assert_eq!(
            eligible(
                &cache,
                &modeled_scope(&body, "gpt-6-sol", "source"),
                106,
                2000
            ),
            0
        );
        assert_eq!(
            eligible(
                &cache,
                &modeled_scope(&body, "gpt-6.1-sol", "other-source"),
                106,
                2000
            ),
            0
        );
        assert_eq!(serde_json::to_vec(&cache).unwrap(), original);
    }

    #[test]
    fn modeled_prefix_queries_are_pure_and_suffix_writes_keep_original_deadlines() {
        let mut cache = ModeledCache::default();
        cache.written("scope".into(), 100, 7, 2000);
        let original = serde_json::to_vec(&cache).unwrap();
        for at in [100, 106, 107, 10000] {
            assert_eq!(
                eligible(&cache, "scope", at, 3000),
                if at < 107 { 2000 } else { 0 }
            );
            assert_eq!(serde_json::to_vec(&cache).unwrap(), original);
        }
        assert_eq!(eligible(&cache, "other", 106, 3000), 0);
        assert_eq!(eligible(&cache, "scope", 99, 3000), 0);
        cache.written("scope".into(), 106, 100, 2100);
        cache = serde_json::from_slice(&serde_json::to_vec(&cache).unwrap()).unwrap();
        assert_eq!(eligible(&cache, "scope", 106, 2100), 2100);
        assert_eq!(eligible(&cache, "scope", 107, 2100), 0);
        // A later cold request really writes the prefix again, not a refresh
        // invented by an earlier read. Unknown/other namespaces cannot borrow it.
        cache.written("scope".into(), 107, 7, 2100);
        assert_eq!(eligible(&cache, "scope", 113, 2100), 2100);
        assert_eq!(eligible(&cache, "scope", 114, 2100), 0);
    }

    #[test]
    fn modeled_write_storage_is_bounded_numeric_and_restart_safe() {
        let mut cache = ModeledCache::default();
        let mut maximum = 0;
        for input in 2000..2160 {
            assert_eq!(
                eligible(&cache, "synthetic", 100, input),
                if input == 2000 { 0 } else { input - 1 }
            );
            cache.written("synthetic".into(), 100, 1800, input);
            let value = serde_json::to_value(&cache).unwrap();
            let count = value["writes"].as_array().unwrap().len();
            assert!(count <= 128);
            maximum = maximum.max(count);
        }
        assert_eq!(maximum, 1, "identical write intervals coalesce exactly");
        let value = serde_json::to_value(&cache).unwrap();
        let events = value["writes"].as_array().unwrap();
        assert!(serde_json::to_vec(events).unwrap().len() < 8192);
        assert!(
            events
                .iter()
                .all(|event| event.as_object().unwrap().values().all(Value::is_u64))
        );
        let resumed: ModeledCache = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(resumed).unwrap(), value);
    }

    // Review5716439062: retention is not a cache miss. The original pressure
    // test covers storage bounds; this checks the retained coverage itself.
    #[test]
    fn modeled_write_pressure_preserves_unexpired_root_coverage_after_restart() {
        let mut cache = ModeledCache::default();
        for input in 2000..=2128 {
            let cached = eligible(&cache, "synthetic-pressure", 100, input);
            assert_eq!(cached, if input == 2000 { 0 } else { input - 1 });
            cache.written("synthetic-pressure".into(), 100, 1800, input);
        }
        let encoded = serde_json::to_vec(&cache).unwrap();
        let resumed: ModeledCache = serde_json::from_slice(&encoded).unwrap();
        assert!(encoded.len() < 8192);
        assert_eq!(
            eligible(&resumed, "synthetic-pressure", 100, 2128),
            2128,
            "CACHE_RETENTION_COVERAGE: evicting numeric records is not a cold-write event"
        );
        for at in [100, 1899] {
            assert_eq!(eligible(&resumed, "synthetic-pressure", at, 2128), 2128);
        }
        assert_eq!(eligible(&resumed, "synthetic-pressure", 1900, 2128), 0);
        assert_eq!(eligible(&resumed, "other-scope", 100, 2128), 0);
        assert_eq!(eligible(&resumed, "synthetic-pressure", 99, 2128), 0);
        assert_eq!(serde_json::to_vec(&resumed).unwrap(), encoded);
    }

    #[test]
    fn modeled_distinct_write_pressure_retains_prefix_and_marks_only_lost_suffix_unknown() {
        let mut cache = ModeledCache::default();
        for step in 0..129 {
            let input = 2000 + step;
            assert_eq!(
                eligible(&cache, "pressure", 100 + step, input),
                if step == 0 { 0 } else { input - 1 }
            );
            cache.written("pressure".into(), 100 + step, 1800, input);
        }
        let encoded = serde_json::to_vec(&cache).unwrap();
        let mut resumed: ModeledCache = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(resumed.writes.len(), 128);
        assert!(encoded.len() < 8192);
        assert_eq!(resumed.eligible_at("pressure", 228, 2127), Some(2127));
        assert_eq!(resumed.eligible_at("pressure", 228, 2128), None);
        assert_eq!(resumed.eligible_at("other", 228, 2128), Some(0));
        assert_eq!(resumed.eligible_at("pressure", 99, 2128), Some(0));
        assert_eq!(serde_json::to_vec(&resumed).unwrap(), encoded);
        // An unknown partition cannot become a known cold suffix on continuation.
        resumed.written("pressure".into(), 229, 3600, 2129);
        assert_eq!(resumed.eligible_at("pressure", 229, 2129), None);
        assert_eq!(resumed.eligible_at("pressure", 229, 2127), Some(2127));
        assert_eq!(resumed.writes.len(), 128);
        // The known original root really expires at1900. A full cold rewrite is
        // now justified; the unknown suffix never renews that root's deadline.
        assert_eq!(resumed.eligible_at("pressure", 1900, 2129), Some(0));
        resumed.written("pressure".into(), 1900, 7, 2129);
        assert_eq!(resumed.eligible_at("pressure", 1906, 2129), Some(2129));
        assert_eq!(resumed.eligible_at("pressure", 1907, 2129), Some(0));
        assert!(resumed.unresolved.is_none());
    }

    // One fake-clock persisted-owner lifecycle. A full reported write alone has
    // unknown timing. The separately supplied synthetic write-window proof is a
    // new fixture contract, NOT a claim the existing transport records it. Its
    // instantaneous bounds collapse dispatch/terminal time only in this fixture;
    // real producer/transport-bound acceptance remains a separate gate. No read
    // refresh contract is assumed. Baseline readers ignore the new window and
    // reach the intended positive-credit assertion, rather than failing to build.
    #[test]
    fn observed_full_write_then_reads_keep_original_lifetime_across_restart() {
        use carry::models::{Route, resolve_model};
        let mut model = resolve_model(
            "gpt-6.1-sol",
            Route::new("https://api.openai.com/v1/responses", false),
            None,
            None,
            0,
            0,
        )
        .unwrap()
        .with_scenario_cache_policy();
        model.scenario_cache_policy.as_mut().unwrap().ttl_seconds = 7;
        let body = json!({"model":"gpt-6.1-sol", "input":[
            {"role":"user","content":"synthetic written prefix ".repeat(2000)}]});
        let write = json!({"input_tokens":2000,"output_tokens":1,
            "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":2000}});
        let hit = json!({"input_tokens":2000,"output_tokens":1,
            "input_tokens_details":{"cached_tokens":2000,"cache_write_tokens":0}});
        let miss = json!({"input_tokens":2000,"output_tokens":1,
            "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0}});
        let credit = crate::proxy_savings::token_units(&body["input"]) as f64;
        assert!(credit > 0.0);
        let mut cache = Vec::new();
        assert!(observe(
            &mut cache,
            &body,
            &write,
            Some(&model),
            "scope",
            100u64.into()
        ));
        assert_eq!(cache.len(), 1);
        assert_eq!(
            cache[0].native_cached_fraction, 0.0,
            "write is not a reported read"
        );
        assert_eq!(
            reusable(&cache, &body, Some(&model), "scope", 101),
            0.0,
            "an unbounded terminal timestamp alone cannot locate the write"
        );
        let install_window = |cache: &mut Vec<CacheEvidence>, at| {
            let mut saved = serde_json::to_value(&*cache).unwrap();
            saved[0]["write_window"] = json!({"not_before":at,"not_after":at,"lifetime":7});
            *cache = serde_json::from_value(saved).unwrap();
        };
        install_window(&mut cache, 100);
        assert_eq!(
            reusable(&cache, &body, Some(&model), "scope", 101),
            credit,
            "OBSERVED_WRITE_LIFETIME: a complete write must supply bounded forecast evidence, not globally disabled cache credit"
        );

        // Queries and restart cannot change the original event or paid usage.
        let encoded = serde_json::to_vec(&cache).unwrap();
        cache = serde_json::from_slice(&encoded).unwrap();
        for at in [100, 106, 107, 200] {
            assert_eq!(
                reusable(&cache, &body, Some(&model), "scope", at),
                if at < 107 { credit } else { 0.0 }
            );
            assert_eq!(serde_json::to_vec(&cache).unwrap(), encoded);
        }
        assert_eq!(reusable(&cache, &body, Some(&model), "other", 101), 0.0);
        assert_eq!(reusable(&cache, &body, Some(&model), "scope", 99), 0.0);

        // Neither changing today's TTL nor reporting another read establishes
        // a new write. The old event still expires at100+7, not106+100.
        model.scenario_cache_policy.as_mut().unwrap().ttl_seconds = 100;
        for at in [105, 106] {
            assert!(observe(
                &mut cache,
                &body,
                &hit,
                Some(&model),
                "scope",
                at.into()
            ));
            assert_eq!(reusable(&cache, &body, Some(&model), "scope", at), credit);
            cache = serde_json::from_slice(&serde_json::to_vec(&cache).unwrap()).unwrap();
        }
        assert_eq!(
            reusable(&cache, &body, Some(&model), "scope", 107),
            0.0,
            "OBSERVED_WRITE_LIFETIME: reads and a new policy cannot extend the original deadline"
        );

        // Terminal ordering still revokes a positive prefix. A later hit with
        // unknown timing cannot resurrect it; an actual full write can rewarm.
        assert!(observe(
            &mut cache,
            &body,
            &miss,
            Some(&model),
            "scope",
            106u64.into()
        ));
        assert_eq!(reusable(&cache, &body, Some(&model), "scope", 106), 0.0);
        assert!(observe(
            &mut cache,
            &body,
            &hit,
            Some(&model),
            "scope",
            106u64.into()
        ));
        assert_eq!(reusable(&cache, &body, Some(&model), "scope", 106), 0.0);
        model.scenario_cache_policy.as_mut().unwrap().ttl_seconds = 7;
        assert!(observe(
            &mut cache,
            &body,
            &write,
            Some(&model),
            "scope",
            107u64.into()
        ));
        install_window(&mut cache, 107);
        cache = serde_json::from_slice(&serde_json::to_vec(&cache).unwrap()).unwrap();
        assert_eq!(reusable(&cache, &body, Some(&model), "scope", 113), credit);
        assert_eq!(reusable(&cache, &body, Some(&model), "scope", 114), 0.0);
        assert_eq!(write["input_tokens_details"]["cache_write_tokens"], 2000);
        assert_eq!(hit["input_tokens_details"]["cache_write_tokens"], 0);
    }

    // Non-instantaneous bounds are not a point write at response completion.
    // The conservative deadline is earliest possible write + frozen lifetime;
    // no positive credit is justified before the whole write could finish.
    fn bounded_observation(
        usage: Value,
        window: Value,
    ) -> (Vec<CacheEvidence>, Value, carry::models::ResolvedModel) {
        use carry::models::{Route, resolve_model};
        let mut model = resolve_model(
            "gpt-6.1-sol",
            Route::new("https://api.openai.com/v1/responses", false),
            None,
            None,
            0,
            0,
        )
        .unwrap()
        .with_scenario_cache_policy();
        model.scenario_cache_policy.as_mut().unwrap().ttl_seconds = 7;
        let body = json!({"model":"gpt-6.1-sol", "input":[
            {"role":"user","content":"synthetic bounded prefix ".repeat(2000)}]});
        let mut cache = Vec::new();
        assert!(observe(
            &mut cache,
            &body,
            &usage,
            Some(&model),
            "scope",
            103u64.into()
        ));
        assert_eq!(cache.len(), 1);
        let mut saved = serde_json::to_value(cache).unwrap();
        saved[0]["write_window"] = window;
        (serde_json::from_value(saved).unwrap(), body, model)
    }

    #[test]
    fn observed_noninstantaneous_write_uses_earliest_expiry_and_latest_completion() {
        let (cache, body, mut model) = bounded_observation(
            json!({"input_tokens":2000,"input_tokens_details":{"cached_tokens":0,"cache_write_tokens":2000}}),
            json!({"not_before":100,"not_after":103,"lifetime":7}),
        );
        let encoded = serde_json::to_vec(&cache).unwrap();
        let cache: Vec<CacheEvidence> = serde_json::from_slice(&encoded).unwrap();
        let credit = crate::proxy_savings::token_units(&body["input"]) as f64;
        assert!(credit > 0.0);
        for at in [99, 100, 102] {
            assert_eq!(reusable(&cache, &body, Some(&model), "scope", at), 0.0);
        }
        assert_eq!(
            reusable(&cache, &body, Some(&model), "scope", 103),
            credit,
            "OBSERVED_WRITE_BOUNDS: a whole write completed within [100,103] must be usable at103"
        );
        model.scenario_cache_policy.as_mut().unwrap().ttl_seconds = 100;
        for at in [106, 107, 109, 110] {
            assert_eq!(
                reusable(&cache, &body, Some(&model), "scope", at),
                if at < 107 { credit } else { 0.0 }
            );
            assert_eq!(serde_json::to_vec(&cache).unwrap(), encoded);
        }
    }

    #[test]
    fn observed_partial_native_write_cannot_locate_a_local_prefix_even_with_time_bounds() {
        let (cache, body, model) = bounded_observation(
            json!({"input_tokens":2000,"input_tokens_details":{"cached_tokens":1000,"cache_write_tokens":1000}}),
            json!({"not_before":100,"not_after":103,"lifetime":7}),
        );
        assert_eq!(
            cache[0].native_cached_fraction, 0.5,
            "raw usage remains an observation"
        );
        let encoded = serde_json::to_vec(&cache).unwrap();
        assert_eq!(
            reusable(&cache, &body, Some(&model), "scope", 104),
            0.0,
            "OBSERVED_WRITE_EXTENT: aggregate partial counts plus time bounds cannot identify local cached positions"
        );
        assert_eq!(serde_json::to_vec(&cache).unwrap(), encoded);
    }

    #[test]
    fn observed_invalid_or_expired_by_completion_bounds_never_supply_credit() {
        for window in [
            json!({"not_before":104,"not_after":103,"lifetime":7}),
            json!({"not_before":100,"not_after":103,"lifetime":0}),
            json!({"not_before":100,"not_after":107,"lifetime":7}),
            json!({"not_before":100,"not_after":108,"lifetime":7}),
        ] {
            let (cache, body, model) = bounded_observation(
                json!({"input_tokens":2000,"input_tokens_details":{"cached_tokens":0,"cache_write_tokens":2000}}),
                window,
            );
            let encoded = serde_json::to_vec(&cache).unwrap();
            for at in [99, 100, 103, 104, 106, 107, 108, 110] {
                assert_eq!(reusable(&cache, &body, Some(&model), "scope", at), 0.0);
                assert_eq!(serde_json::to_vec(&cache).unwrap(), encoded);
            }
        }
    }

    // A native read receipt proves this request's hit, not its write timestamp.
    // Keep it as an observation; a future guarantee needs timing/refresh proof.
    #[test]
    fn observed_hit_without_write_timing_is_not_a_guaranteed_future_hit() {
        use carry::models::{Route, resolve_model};
        let resolved = resolve_model(
            "gpt-6.1-sol",
            Route::new("https://api.openai.com/v1/responses", false),
            None,
            None,
            0,
            0,
        )
        .unwrap();
        let body = json!({"model":"gpt-6.1-sol", "input":[
            {"role":"user","content":"synthetic prefix ".repeat(2000)}]});
        let mut observations = Vec::new();
        assert!(observe(
            &mut observations,
            &body,
            &json!({"input_tokens":2000,"input_tokens_details":{"cached_tokens":2000,"cache_write_tokens":0}}),
            Some(&resolved),
            "synthetic-credential-scope",
            100u64.into()
        ));
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].native_cached_fraction, 1.0);
        let before = serde_json::to_vec(&observations).unwrap();
        assert_eq!(
            reusable(
                &observations,
                &body,
                Some(&resolved),
                "synthetic-credential-scope",
                101
            ),
            0.0,
            "UNKNOWN_WRITE_TIME: an observation timestamp is not a write timestamp"
        );
        assert_eq!(serde_json::to_vec(&observations).unwrap(), before);
    }
}
