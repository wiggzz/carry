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

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PrefixEvidence {
    base_sha256: String,
    input_sha256: String,
    input_len: usize,
    estimated_tokens: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct CacheEvidence {
    // Destination, auth class and processing tier of the dispatched receipt.
    // Missing historical qualification is unknown, never a frozen-route hit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<carry::models::Route>,
    // Decode former full-prompt checkpoints; compact them before using state.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub base: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<Value>,
    pub at: u64,
    pub native_cached_fraction: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prefix: Option<PrefixEvidence>,
}
impl CacheEvidence {
    pub fn new(base: &Value, input: &[Value], at: u64, native_cached_fraction: f64) -> Self {
        Self {
            route: None,
            base: Value::Null,
            input: Vec::new(),
            at,
            native_cached_fraction,
            prefix: Some(PrefixEvidence {
                base_sha256: serialized_identity(base).1,
                input_sha256: serialized_identity(input).1,
                input_len: input.len(),
                estimated_tokens: crate::proxy_savings::token_units(&json!(input)) as f64,
            }),
        }
    }
    pub fn compact(&mut self) {
        if self.prefix.is_none() {
            let mut compact = Self::new(
                &self.base,
                &self.input,
                self.at,
                self.native_cached_fraction,
            );
            compact.route = self.route.clone();
            *self = compact;
        }
    }
    pub fn input_len(&self) -> usize {
        self.prefix
            .as_ref()
            .map_or(self.input.len(), |p| p.input_len)
    }
    pub fn estimated_tokens(&self) -> f64 {
        self.prefix.as_ref().map_or_else(
            || crate::proxy_savings::token_units(&json!(self.input)) as f64,
            |p| p.estimated_tokens,
        )
    }
    pub fn matches(&self, base: &Value, input: &[Value]) -> bool {
        if !(0.0..=1.0).contains(&self.native_cached_fraction) {
            return false;
        }
        if let Some(prefix) = &self.prefix {
            input.len() >= prefix.input_len
                && serialized_identity(base).1 == prefix.base_sha256
                && serialized_identity(&input[..prefix.input_len]).1 == prefix.input_sha256
        } else {
            self.base == *base && input.starts_with(&self.input)
        }
    }
    pub fn matches_route(&self, route: Option<&carry::models::Route>) -> bool {
        self.route.as_ref() == route
    }
    #[cfg(test)]
    pub fn legacy(base: Value, input: Vec<Value>, at: u64, native_cached_fraction: f64) -> Self {
        Self {
            route: None,
            base,
            input,
            at,
            native_cached_fraction,
            prefix: None,
        }
    }
}
