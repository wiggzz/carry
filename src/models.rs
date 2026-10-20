//! Shared modeled rates and cache support. Unknown prices/capabilities stay unknown.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct PromptCacheCapabilities {
    pub minimum_prefix_tokens: usize,
    pub max_read_breakpoints: usize,
    pub max_write_breakpoints: usize,
    pub implicit_breakpoint_uses_write_slot: bool,
}

const OPENAI_CACHE: PromptCacheCapabilities = PromptCacheCapabilities {
    minimum_prefix_tokens: 1_024,
    max_read_breakpoints: 50,
    max_write_breakpoints: 4,
    implicit_breakpoint_uses_write_slot: true,
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct Rates {
    pub input: f64,
    pub cached: f64,
    pub write: f64,
    pub output: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct LongContext {
    pub threshold: f64,
    pub input_multiplier: f64,
    pub output_multiplier: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct ModelSpec {
    /// Standard USD rates per million tokens.
    pub rates: Option<Rates>,
    pub long_context: Option<LongContext>,
    pub cache: Option<PromptCacheCapabilities>,
    pub explicit_reviewer_cache: bool,
}

pub fn model_spec(model: &str) -> Option<ModelSpec> {
    let (rates, long_context, cache, explicit_reviewer_cache) = match model {
        "gpt-5.6-luna" => (Some((0.20, 0.02, 0.25, 1.20)), false, true, false),
        "gpt-5.6-sol" => (Some((4.00, 0.40, 5.00, 20.00)), false, true, false),
        "gpt-5.6-terra" => (Some((2.00, 0.20, 2.50, 12.00)), false, true, false),
        "gpt-6-astra" => (Some((10.00, 1.00, 12.50, 50.00)), true, false, false),
        "gpt-6-luna" => (Some((0.10, 0.01, 0.125, 0.50)), true, true, true),
        "gpt-6-sol" => (Some((2.00, 0.20, 2.50, 10.00)), true, true, true),
        "gpt-6.1-sol" => (Some((2.00, 0.10, 2.50, 10.00)), true, true, true),
        // Existing family cache support does not imply a known price.
        name if name.starts_with("gpt-5.6-") => (None, false, true, false),
        _ => return None,
    };
    Some(ModelSpec {
        rates: rates.map(|(input, cached, write, output)| Rates {
            input,
            cached,
            write,
            output,
        }),
        long_context: long_context.then_some(LongContext {
            threshold: 272_000.0,
            input_multiplier: 2.0,
            output_multiplier: 1.5,
        }),
        cache: cache.then_some(OPENAI_CACHE),
        explicit_reviewer_cache,
    })
}

impl ModelSpec {
    pub fn rates_for_input(self, input_tokens: f64) -> Option<Rates> {
        let mut rates = self.rates?;
        if let Some(long) = self.long_context
            && input_tokens > long.threshold
        {
            rates.input *= long.input_multiplier;
            rates.cached *= long.input_multiplier;
            rates.write *= long.input_multiplier;
            rates.output *= long.output_multiplier;
        }
        Some(Rates {
            input: rates.input / 1_000_000.0,
            cached: rates.cached / 1_000_000.0,
            write: rates.write / 1_000_000.0,
            output: rates.output / 1_000_000.0,
        })
    }
}

impl Rates {
    pub fn for_model(model: &str, input_tokens: f64) -> Option<Self> {
        model_spec(model)?.rates_for_input(input_tokens)
    }

    pub fn cache_ratios(self) -> (f64, f64) {
        (self.cached / self.input, self.write / self.input)
    }
}

/// Route identity is content-free. A gateway slug never certifies an official
/// entitlement. Loopback subscription routes are explicitly synthetic fixtures.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteClass {
    OfficialApi,
    CodexSubscription,
    SyntheticSubscription,
    UnknownGateway,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Route {
    pub class: RouteClass,
    pub destination_sha256: String,
    pub processing_tier: String,
}
impl Route {
    pub fn new(endpoint: &str, subscription: bool) -> Self {
        let class = if subscription && endpoint == "https://chatgpt.com/backend-api/codex/responses"
        {
            RouteClass::CodexSubscription
        } else if subscription
            && url::Url::parse(endpoint).ok().is_some_and(|url| {
                url.scheme() == "http"
                    && url
                        .host_str()
                        .and_then(|h| h.parse::<std::net::IpAddr>().ok())
                        .is_some_and(|ip| ip.is_loopback())
            })
        {
            RouteClass::SyntheticSubscription
        } else if !subscription && endpoint == "https://api.openai.com/v1/responses" {
            RouteClass::OfficialApi
        } else {
            RouteClass::UnknownGateway
        };
        Self {
            class,
            destination_sha256: format!("{:x}", Sha256::digest(endpoint.as_bytes())),
            processing_tier: "default".into(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RawContext {
    pub context_window: Option<u64>,
    /// Maximum permitted configuration override, NOT an operational default.
    pub max_context_window: Option<u64>,
    pub auto_compact_token_limit: Option<u64>,
    pub effective_context_window_percent: Option<u64>,
    pub comp_hash: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SourceModel {
    pub model: String,
    pub raw: RawContext,
    pub field_status: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SourceCatalog {
    pub models: Vec<SourceModel>,
    pub revision: String,
    pub fetched_at: u64,
    pub source: String,
    pub status: String,
}
fn bounded_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.:/".contains(&c))
}
impl SourceCatalog {
    /// Bounded, typed allowlist projection. No provider instructions/diagnostics
    /// survive ingestion. Duplicate visible slugs invalidate ambiguous facts.
    pub fn from_codex(value: &Value, at: u64, synthetic: bool) -> Result<Self, &'static str> {
        let entries = value["models"].as_array().ok_or("missing models")?;
        if entries.len() > 2048 {
            return Err("too many models");
        }
        let mut models = Vec::new();
        let mut seen = BTreeSet::new();
        for entry in entries {
            let id = entry["slug"]
                .as_str()
                .filter(|id| bounded_id(id))
                .ok_or("invalid model ID")?;
            match entry["visibility"].as_str() {
                Some("hide" | "none") => continue,
                Some("list") => {}
                _ => return Err("invalid visibility"),
            }
            if !seen.insert(id) {
                return Err("duplicate visible model");
            }
            let mut statuses = BTreeMap::new();
            let mut number = |name: &str, upper: u64| {
                let raw = entry.get(name);
                let parsed = raw.and_then(Value::as_u64);
                let status = if raw.is_none() || raw.is_some_and(Value::is_null) {
                    "missing"
                } else if parsed.is_some_and(|v| v > 0 && v <= upper) {
                    "valid"
                } else {
                    "invalid"
                };
                statuses.insert(name.into(), status.into());
                parsed
            };
            let mut raw = RawContext {
                context_window: number("context_window", 2_000_000),
                max_context_window: number("max_context_window", 2_000_000),
                auto_compact_token_limit: number("auto_compact_token_limit", 2_000_000),
                effective_context_window_percent: number("effective_context_window_percent", 100),
                comp_hash: entry["comp_hash"]
                    .as_str()
                    .filter(|s| bounded_id(s))
                    .map(str::to_owned),
            };
            statuses.insert(
                "comp_hash".into(),
                if entry.get("comp_hash").is_none_or(Value::is_null) {
                    "missing"
                } else if raw.comp_hash.is_some() {
                    "valid"
                } else {
                    "invalid"
                }
                .into(),
            );
            if let (Some(default), Some(maximum)) = (raw.context_window, raw.max_context_window)
                && default > maximum
            {
                statuses.insert("context_window".into(), "invalid_relationship".into());
            }
            // Keep explicit zero values for provenance, but never select them.
            if raw.comp_hash.as_ref().is_some_and(|s| s.len() > 128) {
                raw.comp_hash = None;
            }
            models.push(SourceModel {
                model: id.into(),
                raw,
                field_status: statuses,
            });
        }
        models.sort_by(|a, b| a.model.cmp(&b.model));
        let revision = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&models).expect("typed projection"))
        );
        Ok(Self {
            models,
            revision,
            fetched_at: at,
            source: if synthetic {
                "synthetic_loopback_codex_metadata"
            } else {
                "https://chatgpt.com/backend-api/codex/models?client_version=0.147.0"
            }
            .into(),
            status: "fresh_at_resolution".into(),
        })
    }
    pub fn unavailable(at: u64) -> Self {
        Self {
            models: vec![],
            revision: "unavailable-v1".into(),
            fetched_at: at,
            source: "codex_metadata".into(),
            status: "unavailable".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Provenance {
    pub source: String,
    pub status: String,
    pub revision: String,
    pub fetched_at: Option<u64>,
    pub fallback_reason: Option<String>,
    pub operator_override: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OrdinaryPolicy {
    pub derivation: String,
    pub token_basis: String,
    pub total_budget: u32,
    pub input_target: u32,
    pub output_reserve: u32,
    pub estimation_margin_percent: u8,
}
/// Capacity is a feasibility bound; ordinary policy is a conservative target.
/// The raw cheap-input threshold is intentionally interpreted as a total budget
/// only by this named policy, not rewritten as a vendor context limit.
pub fn ordinary_policy(
    capacity: u32,
    cheap_input_boundary: Option<u32>,
    reserve: u32,
    margin: u8,
) -> OrdinaryPolicy {
    let total_budget = capacity.min(cheap_input_boundary.unwrap_or(capacity));
    let input_target =
        (u64::from(total_budget.saturating_sub(reserve)) * 100 / (100 + u64::from(margin))) as u32;
    OrdinaryPolicy {
        derivation: "conservative-cheap-total-v1".into(),
        token_basis: "local_prompt_estimate_plus_output_allowance".into(),
        total_budget,
        input_target,
        output_reserve: reserve,
        estimation_margin_percent: margin,
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResolvedModel {
    pub schema_version: u32,
    pub model: String,
    pub route: Route,
    pub raw: RawContext,
    pub capacity_tokens: u32,
    pub ordinary: OrdinaryPolicy,
    pub rates: Option<Rates>,
    pub rate_units: String,
    pub price_basis: String,
    pub cheap_input_boundary: Option<u32>,
    pub long_input_multiplier: Option<f64>,
    pub long_output_multiplier: Option<f64>,
    pub cache: Option<PromptCacheCapabilities>,
    pub explicit_cache_transport: bool,
    pub provenance: BTreeMap<String, Provenance>,
    pub raw_field_status: BTreeMap<String, String>,
}
impl ResolvedModel {
    pub fn rates_for_input(&self, input: f64) -> Option<Rates> {
        let mut rates = self.rates?;
        if self
            .cheap_input_boundary
            .is_some_and(|boundary| input > f64::from(boundary))
        {
            let i = self.long_input_multiplier?;
            rates.input *= i;
            rates.cached *= i;
            rates.write *= i;
            rates.output *= self.long_output_multiplier?;
        }
        Some(Rates {
            input: rates.input / 1e6,
            cached: rates.cached / 1e6,
            write: rates.write / 1e6,
            output: rates.output / 1e6,
        })
    }
}

/// Saved once per route/model configuration epoch, shared by export/admission.
/// A checkpoint without this additive field is a legacy interpretation, never
/// permission to apply newly fetched facts to its earlier usage/cost history.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResolvedPair {
    pub schema_version: u32,
    pub primary: ResolvedModel,
    pub reviewer: ResolvedModel,
}

pub fn resolve_model(
    model: &str,
    route: Route,
    source: Option<&SourceCatalog>,
    override_tokens: Option<u32>,
    fallback: u32,
    reserve: u32,
    margin: u8,
) -> Result<ResolvedModel, &'static str> {
    if !bounded_id(model) {
        return Err("invalid model ID");
    }
    let subscribed = matches!(
        route.class,
        RouteClass::CodexSubscription | RouteClass::SyntheticSubscription
    );
    let source = source.filter(|_| subscribed);
    let entry = source.and_then(|s| s.models.iter().find(|entry| entry.model == model));
    let raw = entry.map(|e| e.raw.clone()).unwrap_or_default();
    let capacity = entry
        .filter(|e| {
            e.field_status
                .get("context_window")
                .is_some_and(|s| s == "valid")
        })
        .and_then(|e| e.raw.context_window)
        .filter(|_| source.is_some_and(|s| s.status == "fresh_at_resolution"));
    if override_tokens.is_some_and(|v| v == 0 || v > 2_000_000) {
        return Err("invalid context override");
    }
    // A published maximum override does not prove entitlement to expand above
    // the advertised default. Expansion needs a separately attested contract.
    if let (Some(explicit), Some(advertised)) = (override_tokens, capacity)
        && u64::from(explicit) > advertised
    {
        return Err("context override exceeds advertised route capacity");
    }
    let capacity_tokens = override_tokens.unwrap_or(capacity.map(|v| v as u32).unwrap_or(fallback));
    let legacy = model_spec(model);
    let rates = legacy.and_then(|s| s.rates);
    let long = legacy.and_then(|s| s.long_context);
    let provenance = |name: &str, status: &str, override_value| Provenance {
        source: name.into(),
        status: status.into(),
        revision: "route-catalog-v1".into(),
        fetched_at: None,
        fallback_reason: None,
        operator_override: override_value,
    };
    let mut capacity_provenance =
        provenance("conservative-role-capacity-v1", "fallback", override_tokens);
    capacity_provenance.fallback_reason = capacity
        .is_none()
        .then(|| "no_valid_fresh_exact_route_advertised_default".into());
    if let Some(source) = source {
        capacity_provenance.revision = source.revision.clone();
        capacity_provenance.fetched_at = Some(source.fetched_at);
        if capacity.is_some() {
            capacity_provenance.source = source.source.clone();
            capacity_provenance.status = source.status.clone();
        }
    }
    if override_tokens.is_some() {
        capacity_provenance.status = "operator_override".into();
    }
    let cache = matches!(route.class, RouteClass::OfficialApi)
        .then(|| legacy.and_then(|s| s.cache))
        .flatten();
    Ok(ResolvedModel {
        schema_version: 1,
        model: model.into(),
        route,
        raw,
        capacity_tokens,
        ordinary: ordinary_policy(
            capacity_tokens,
            long.map(|l| l.threshold as u32),
            reserve,
            margin,
        ),
        rates,
        rate_units: "USD_per_million_tokens_whole_request_tier_not_invoice".into(),
        price_basis: if rates.is_some() {
            "legacy_api_equivalent_model"
        } else {
            "unknown"
        }
        .into(),
        cheap_input_boundary: long.map(|l| l.threshold as u32),
        long_input_multiplier: long.map(|l| l.input_multiplier),
        long_output_multiplier: long.map(|l| l.output_multiplier),
        cache,
        explicit_cache_transport: cache.is_some()
            && legacy.is_some_and(|s| s.explicit_reviewer_cache),
        provenance: BTreeMap::from([
            ("capacity".into(), capacity_provenance),
            (
                "prices".into(),
                provenance(
                    "carry/src/models.rs:legacy-table",
                    if rates.is_some() {
                        "legacy_modeling_basis_not_route_tariff"
                    } else {
                        "unknown"
                    },
                    None,
                ),
            ),
            (
                "cache".into(),
                provenance(
                    "exact_official_api_route_legacy_capability",
                    if cache.is_some() {
                        "legacy_route_capability"
                    } else {
                        "unknown_or_transport_disabled"
                    },
                    None,
                ),
            ),
        ]),
        raw_field_status: entry.map(|e| e.field_status.clone()).unwrap_or_default(),
    })
}
