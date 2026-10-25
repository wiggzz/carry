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
    pub native_cached_fraction: f64,
}

pub(super) fn base(body: &Value) -> Value {
    let mut base = body.clone();
    if let Some(object) = base.as_object_mut() {
        for key in [
            "input",
            "stream",
            "store",
            "metadata",
            "client_metadata",
            "include",
        ] {
            object.remove(key);
        }
    }
    base
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
        self.at <= at
            && at - self.at < 1800
            && self.route.as_ref() == resolved.map(|r| &r.route)
            && self.credential_sha256 == credential
            && self.input_len <= input.len()
            && self.base_sha256 == serialized_identity(&base(body)).1
            && self.input_sha256 == serialized_identity(&input[..self.input_len]).1
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
    if resolved.is_none_or(|r| r.cache.is_none()) {
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
) {
    let Some(input) = body["input"].as_array() else {
        return;
    };
    let (Some(total), Some(cached)) = (
        usage["input_tokens"].as_u64(),
        usage["input_tokens_details"]["cached_tokens"].as_u64(),
    ) else {
        return;
    };
    if total == 0 || cached > total {
        return;
    }
    let e = CacheEvidence {
        route: resolved.map(|r| r.route.clone()),
        credential_sha256: credential.into(),
        base_sha256: serialized_identity(&base(body)).1,
        input_sha256: serialized_identity(input).1,
        input_len: input.len(),
        estimated_tokens: crate::proxy_savings::token_units(&json!(input)) as f64,
        at,
        native_cached_fraction: cached as f64 / total as f64,
    };
    // A zero read records potential history, never confirmed cache activity.
    cache.retain(|old| {
        old.base_sha256 != e.base_sha256
            || old.input_sha256 != e.input_sha256
            || old.route != e.route
            || old.credential_sha256 != e.credential_sha256
    });
    cache.push(e);
    trim(cache, at);
}

pub(super) fn trim(cache: &mut Vec<CacheEvidence>, at: u64) {
    cache.retain(|e| e.at <= at && at - e.at < 1800);
    cache.sort_by_key(|e| e.at);
    while cache.len() > 128 || serde_json::to_vec(cache).unwrap().len() > 65536 {
        cache.remove(0);
    }
}
