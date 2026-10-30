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
        at >= self.at && at - self.at < self.lifetime
    }
}
impl ModeledCache {
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Pure read-time contiguous-prefix query. Neither expiry nor a cache read
    /// records a write, refreshes a deadline, or consults today's policy TTL.
    pub(crate) fn eligible_at(&self, scope: &str, at: u64, input: u64) -> u64 {
        if self.scope != scope {
            return 0;
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
        prefix.min(input)
    }

    /// Record only the uncached extent written by the hypothetical request.
    /// There is no supported read-refresh contract in this whole-prefix model.
    pub(crate) fn written(
        &mut self,
        scope: String,
        at: u64,
        lifetime: u64,
        input: u64,
        cached: u64,
    ) {
        if self.scope != scope || cached == 0 {
            self.clear();
            self.scope = scope;
        }
        if input <= cached || lifetime == 0 {
            return;
        }
        // Pruning is a write-boundary operation, never a side effect of lookup.
        self.writes.retain(|write| write.eligible_at(at));
        self.writes.push(ModeledWrite {
            start: cached,
            end: input,
            at,
            lifetime,
        });
        // Bounded state; losing an old extent can only reduce conditional credit.
        if self.writes.len() > 128 {
            self.writes.remove(0);
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
            && at - self.at < self.ttl_seconds
            && self.route.as_ref() == resolved.map(|r| &r.route)
            && self.credential_sha256 == credential
            && self.input_len <= input.len()
            && self.base_sha256 == serialized_identity(&base(body)).1
            && self.input_sha256 == input_identity(&input[..self.input_len])
            && (0.0..=1.0).contains(&self.native_cached_fraction)
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
        .filter(|e| e.matches(body, resolved, credential, at))
        .map(|e| e.estimated_tokens * e.native_cached_fraction)
        .fold(0.0, f64::max)
        .min(crate::proxy_savings::token_units(body) as f64)
}

pub(super) fn observe(
    cache: &mut Vec<CacheEvidence>,
    body: &Value,
    usage: &Value,
    resolved: Option<&carry::models::ResolvedModel>,
    credential: &str,
    at: u64,
) -> bool {
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
    let (Some(total), Some(cached)) = (
        usage["input_tokens"].as_u64(),
        usage["input_tokens_details"]["cached_tokens"].as_u64(),
    ) else {
        return false;
    };
    if total < capability.minimum_prefix_tokens as u64 || cached > total {
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
    };
    // This terminal covers every compatible older prefix, not only an equal
    // input hash. Mutation order (under history::observe_cache), not TTL clock
    // ties, decides which observation owns its forecast. Unrelated branches and
    // namespaces remain independent; a later genuine rewarm can replace this.
    cache.retain(|old| {
        old.base_sha256 != e.base_sha256
            || old.route != e.route
            || old.credential_sha256 != e.credential_sha256
            || old.input_len > input.len()
            || old.input_sha256 != input_identity(&input[..old.input_len])
    });
    cache.push(e);
    trim(cache, at);
    true
}

pub(super) fn trim(cache: &mut Vec<CacheEvidence>, at: u64) {
    cache.retain(|e| e.at <= at && at - e.at < e.ttl_seconds);
    cache.sort_by_key(|e| e.at);
    while cache.len() > 128 || serde_json::to_vec(cache).unwrap().len() > 65536 {
        cache.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Boundary acceptance for the shared semantic owner. The real-coordinator
    // differential separately reproduces the original equivalent-options bug.
    #[test]
    fn modeled_namespace_preserves_writes_only_for_the_same_semantic_prompt_source() {
        let body = json!({"model":"gpt-6.1-sol","prompt_cache_key":"synthetic-key",
            "instructions":"fixed", "tools":[], "input":[{"role":"user","content":"prefix"}]});
        let scope = modeled_scope(&body, "gpt-6.1-sol", "source");
        let mut cache = ModeledCache::default();
        cache.written(scope.clone(), 100, 7, 2000, 0);
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
            assert_eq!(cache.eligible_at(&next_scope, 106, 2000), 2000);
            // Even a newly requested TTL does not renew the original event.
            assert_eq!(cache.eligible_at(&next_scope, 107, 2000), 0);
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
                cache.eligible_at(&modeled_scope(&next, "gpt-6.1-sol", "source"), 106, 2000),
                0
            );
        }
        assert_eq!(
            cache.eligible_at(&modeled_scope(&body, "gpt-6-sol", "source"), 106, 2000),
            0
        );
        assert_eq!(
            cache.eligible_at(
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
        cache.written("scope".into(), 100, 7, 2000, 0);
        let original = serde_json::to_vec(&cache).unwrap();
        for at in [100, 106, 107, 10000] {
            assert_eq!(
                cache.eligible_at("scope", at, 3000),
                if at < 107 { 2000 } else { 0 }
            );
            assert_eq!(serde_json::to_vec(&cache).unwrap(), original);
        }
        assert_eq!(cache.eligible_at("other", 106, 3000), 0);
        assert_eq!(cache.eligible_at("scope", 99, 3000), 0);
        cache.written("scope".into(), 106, 100, 2100, 2000);
        cache = serde_json::from_slice(&serde_json::to_vec(&cache).unwrap()).unwrap();
        assert_eq!(cache.eligible_at("scope", 106, 2100), 2100);
        assert_eq!(cache.eligible_at("scope", 107, 2100), 0);
        // A later cold request really writes the prefix again, not a refresh
        // invented by an earlier read. Unknown/other namespaces cannot borrow it.
        cache.written("scope".into(), 107, 7, 2100, 0);
        assert_eq!(cache.eligible_at("scope", 113, 2100), 2100);
        assert_eq!(cache.eligible_at("scope", 114, 2100), 0);
    }

    #[test]
    fn modeled_write_storage_is_bounded_numeric_and_restart_safe() {
        let mut cache = ModeledCache::default();
        let mut maximum = 0;
        for input in 2000..2160 {
            let cached = cache.eligible_at("synthetic", 100, input);
            cache.written("synthetic".into(), 100, 1800, input, cached);
            let value = serde_json::to_value(&cache).unwrap();
            let count = value["writes"].as_array().unwrap().len();
            assert!(count <= 128);
            maximum = maximum.max(count);
        }
        assert_eq!(maximum, 128);
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
            let cached = cache.eligible_at("synthetic-pressure", 100, input);
            assert_eq!(cached, if input == 2000 { 0 } else { input - 1 });
            cache.written("synthetic-pressure".into(), 100, 1800, input, cached);
        }
        let encoded = serde_json::to_vec(&cache).unwrap();
        let resumed: ModeledCache = serde_json::from_slice(&encoded).unwrap();
        assert!(encoded.len() < 8192);
        assert_eq!(
            resumed.eligible_at("synthetic-pressure", 100, 2128),
            2128,
            "CACHE_RETENTION_COVERAGE: evicting numeric records is not a cold-write event"
        );
        for at in [100, 1899] {
            assert_eq!(resumed.eligible_at("synthetic-pressure", at, 2128), 2128);
        }
        assert_eq!(resumed.eligible_at("synthetic-pressure", 1900, 2128), 0);
        assert_eq!(resumed.eligible_at("other-scope", 100, 2128), 0);
        assert_eq!(resumed.eligible_at("synthetic-pressure", 99, 2128), 0);
        assert_eq!(serde_json::to_vec(&resumed).unwrap(), encoded);
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
            100
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
