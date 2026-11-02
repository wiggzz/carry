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

const SHORT_CACHE_LIFETIME_SECONDS: u64 = 1800;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct Rates {
    pub input: f64,
    pub cached: f64,
    pub write: f64,
    pub output: f64,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct LongContext {
    pub threshold: f64,
    pub input_multiplier: f64,
    pub output_multiplier: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct ModelSpec {
    pub context_tokens: Option<u32>,
    pub source: Option<&'static str>,
    /// Standard USD rates per million tokens.
    pub rates: Option<Rates>,
    pub long_context: Option<LongContext>,
    pub cache: Option<PromptCacheCapabilities>,
    pub explicit_reviewer_cache: bool,
}

#[derive(Deserialize)]
struct CatalogEntry {
    context_tokens: u32,
    rates: Rates,
    long_context: Option<LongContext>,
    cache: bool,
    explicit_reviewer_cache: bool,
    source: String,
}

pub const DEFAULT_REVIEWER_MODEL: &str = "gpt-6-luna";

/// Initial scratch configuration only; requests resolve their actual model.
/// Even this initial value is read from the shared catalog, not a role default.
pub fn default_context_tokens() -> u32 {
    model_spec(DEFAULT_REVIEWER_MODEL)
        .and_then(|s| s.context_tokens)
        .expect("default reviewer is present in the embedded catalog")
}

pub fn default_ordinary_context_tokens() -> u32 {
    let spec = model_spec(DEFAULT_REVIEWER_MODEL).expect("default model catalog entry");
    ordinary_policy(
        default_context_tokens(),
        spec.long_context.map(|tier| tier.threshold as u32),
        0,
        0,
    )
    .total_budget
}

pub fn model_spec(model: &str) -> Option<ModelSpec> {
    static CATALOG: std::sync::OnceLock<BTreeMap<String, CatalogEntry>> =
        std::sync::OnceLock::new();
    let catalog = CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("model_catalog.json"))
            .expect("valid embedded model catalog")
    });
    if let Some(entry) = catalog.get(model) {
        return Some(ModelSpec {
            context_tokens: Some(entry.context_tokens),
            source: Some(&entry.source),
            rates: Some(entry.rates),
            long_context: entry.long_context,
            cache: entry.cache.then_some(OPENAI_CACHE),
            explicit_reviewer_cache: entry.explicit_reviewer_cache,
        });
    }
    // Historical family-level cache support is not a context or price guess.
    model.starts_with("gpt-5.6-").then_some(ModelSpec {
        context_tokens: None,
        source: None,
        rates: None,
        long_context: None,
        cache: Some(OPENAI_CACHE),
        explicit_reviewer_cache: false,
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
    /// Missing on historical projections: unbound facts cannot establish a
    /// destination's entitlement. Includes auth class, exact path and service tier.
    #[serde(default)]
    pub route: Option<Route>,
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
            if statuses["context_window"] == "valid"
                && statuses["max_context_window"] == "valid"
                && let (Some(default), Some(maximum)) = (raw.context_window, raw.max_context_window)
                && default > maximum
            {
                statuses.insert("context_window".into(), "invalid_relationship".into());
            }
            if statuses["auto_compact_token_limit"] == "valid"
                && statuses["context_window"] == "valid"
                && raw.auto_compact_token_limit > raw.context_window
            {
                statuses.insert(
                    "auto_compact_token_limit".into(),
                    "invalid_relationship".into(),
                );
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
            route: None,
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
            route: None,
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

/// Explicit client caps override configured feasibility reserves, including zero.
/// This allowance is not expected or billable generated output.
pub fn output_allowance(requested: Option<u64>, configured_reserve: u64) -> u64 {
    requested.unwrap_or(configured_reserve)
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

/// API-equivalent counterfactual assumptions, never transport entitlement.
/// Frozen beside rates; absence on an old snapshot retains legacy provenance.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScenarioCachePolicy {
    pub version: u32,
    pub minimum_prefix_tokens: Option<u64>,
    pub ttl_seconds: u64,
}
/// Request-level population contract, separate from eligibility and tariffs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScenarioCacheMode {
    Implicit,
    Disabled,
    Unknown,
}

/// Whether this request's write lifetime has a supported tariff. This is not
/// cache eligibility or transport entitlement: zero-write receipts remain priced
/// even when the request's population/expiry contract is unknown.
pub fn cache_write_tariff_known(body: &Value) -> bool {
    short_cache_lifetimes(body)
}

// One typed lifetime validator for the currently supported 30-minute contract.
// Callers still own separate monetary or route/observation prerequisites.
fn short_cache_lifetimes(body: &Value) -> bool {
    fn supported_lifetime(options: &Value) -> bool {
        options
            .as_object()
            .is_some_and(|options| options.get("ttl").is_none_or(|ttl| ttl == "30m"))
    }
    fn supported_breakpoint(item: &Value) -> bool {
        item.get("prompt_cache_breakpoint")
            .is_none_or(supported_lifetime)
    }
    body.get("prompt_cache_retention").is_none()
        && body
            .get("prompt_cache_options")
            .is_none_or(supported_lifetime)
        // Breakpoints override the request default on input items, content blocks,
        // and typed tool-output blocks. Do not recursively interpret arbitrary
        // tool-result JSON or schemas as metadata. Native usage does not partition
        // writes by lifetime, so any unsupported lifetime makes a nonzero write
        // cost unknown (including mixed-lifetime requests).
        && body["input"].as_array().is_none_or(|items| {
            items.iter().all(|item| {
                supported_breakpoint(item)
                    && item["content"]
                        .as_array()
                        .is_none_or(|blocks| blocks.iter().all(supported_breakpoint))
                    && (!matches!(
                        item["type"].as_str(),
                        Some("function_call_output" | "custom_tool_call_output")
                    ) || item["output"].as_array().is_none_or(|blocks| {
                        blocks.iter().all(|block| {
                            !matches!(
                                block["type"].as_str(),
                                Some("input_text" | "input_image" | "input_file")
                            ) || supported_breakpoint(block)
                        })
                    }))
            })
        })
}

impl ScenarioCachePolicy {
    pub const VERSION: u32 = 2;

    /// The comparison is API-equivalent even on a subscription route; this
    /// declares modeled billing, never transport support or observed activity.
    pub fn request_mode(&self, body: &Value) -> ScenarioCacheMode {
        if self.version < Self::VERSION {
            return ScenarioCacheMode::Implicit;
        }
        fn has_breakpoint(value: &Value) -> bool {
            match value {
                Value::Object(fields) => {
                    fields.contains_key("prompt_cache_breakpoint")
                        || fields.values().any(has_breakpoint)
                }
                Value::Array(items) => items.iter().any(has_breakpoint),
                _ => false,
            }
        }
        if body.get("prompt_cache_retention").is_some() || has_breakpoint(&body["input"]) {
            // Explicit extents and legacy retention settings are not priced by
            // the whole-prompt, 30-minute conditional scenario.
            return ScenarioCacheMode::Unknown;
        }
        let Some(options) = body.get("prompt_cache_options") else {
            return ScenarioCacheMode::Implicit;
        };
        let Some(options) = options.as_object() else {
            return ScenarioCacheMode::Unknown;
        };
        if options.get("ttl").is_some_and(|ttl| ttl != "30m") {
            // Longer-lived writes and explicit-prefix extents need their own
            // frozen tariff/partition model; never price them as free writes.
            return ScenarioCacheMode::Unknown;
        }
        match options.get("mode").and_then(Value::as_str) {
            None if !options.contains_key("mode") => ScenarioCacheMode::Implicit,
            Some("implicit") => ScenarioCacheMode::Implicit,
            Some("explicit") => ScenarioCacheMode::Disabled,
            _ => ScenarioCacheMode::Unknown,
        }
    }

    /// Conditional API-equivalent coverage, not this route's entitlement or an
    /// observed native hit. Only the frozen supported short implicit policy can
    /// refresh a hypothetical read; custom/unknown policies keep their clocks.
    pub fn read_refresh_seconds(&self, model: &str, body: &Value) -> Option<u64> {
        (self.version == Self::VERSION
            && self.ttl_seconds == SHORT_CACHE_LIFETIME_SECONDS
            && self.minimum_prefix_tokens.is_some()
            && model_spec(model).is_some_and(|spec| spec.cache.is_some())
            && self.request_mode(body) == ScenarioCacheMode::Implicit)
            .then_some(SHORT_CACHE_LIFETIME_SECONDS)
    }

    pub fn for_model(model: &str) -> Self {
        Self {
            version: Self::VERSION,
            minimum_prefix_tokens: model_spec(model)
                .and_then(|s| s.cache)
                .map(|c| c.minimum_prefix_tokens as u64),
            ttl_seconds: SHORT_CACHE_LIFETIME_SECONDS,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResolvedModel {
    pub schema_version: u32,
    pub model: String,
    pub route: Route,
    pub raw: RawContext,
    pub capacity_tokens: Option<u32>,
    pub ordinary: OrdinaryPolicy,
    pub rates: Option<Rates>,
    pub rate_units: String,
    pub price_basis: String,
    pub cheap_input_boundary: Option<u32>,
    pub long_input_multiplier: Option<f64>,
    pub long_output_multiplier: Option<f64>,
    pub cache: Option<PromptCacheCapabilities>,
    #[serde(default)]
    pub scenario_cache_policy: Option<ScenarioCachePolicy>,
    pub explicit_cache_transport: bool,
    pub provenance: BTreeMap<String, Provenance>,
    pub raw_field_status: BTreeMap<String, String>,
}
impl ResolvedModel {
    /// Documented billing coverage from a complete read, not another write or
    /// physical KV residency. Scenario policy/capability injection alone cannot
    /// confer this provider contract on an unknown route, auth class or model.
    pub fn cache_read_refresh_seconds(&self, body: &Value) -> Option<u64> {
        (self.route == Route::new("https://api.openai.com/v1/responses", false)
            && self.cache.is_some()
            && model_spec(&self.model).is_some_and(|spec| spec.cache.is_some())
            && body["model"].as_str() == Some(self.model.as_str())
            && short_cache_lifetimes(body)
            && body["prompt_cache_options"]
                .get("mode")
                .is_none_or(|mode| matches!(mode.as_str(), Some("implicit" | "explicit"))))
        .then_some(SHORT_CACHE_LIFETIME_SECONDS)
    }

    /// Upgrade only prospective assumptions at the resolution boundary. Existing
    /// tariffs, route capability and historical monetary sums remain frozen.
    pub fn with_scenario_cache_policy(mut self) -> Self {
        if self
            .scenario_cache_policy
            .as_ref()
            .is_none_or(|p| p.version == 1)
        {
            let mut policy = self
                .scenario_cache_policy
                .take()
                .unwrap_or_else(|| ScenarioCachePolicy::for_model(&self.model));
            policy.version = ScenarioCachePolicy::VERSION;
            let known_model = model_spec(&self.model).is_some();
            let status = if policy.minimum_prefix_tokens.is_some() {
                "conditional_api_equivalent_model_policy_not_route_entitlement"
            } else if known_model {
                "no_modeled_cache_support"
            } else {
                "unknown_model_cache_policy"
            };
            self.scenario_cache_policy = Some(policy);
            self.provenance.insert(
                "scenario_cache".into(),
                Provenance {
                    source: "https://developers.openai.com/api/docs/guides/prompt-caching".into(),
                    status: status.into(),
                    revision: "traditional-cache-v2-request-mode-2026-10-28".into(),
                    fetched_at: None,
                    fallback_reason: None,
                    operator_override: None,
                },
            );
        }
        self
    }

    /// Prospective comparison uses the destination's cache-population semantics.
    /// Codex credits have no separate cache-write charge; values remain model
    /// API-equivalent USD valuations, not subscription invoices. Frozen receipt
    /// pricing continues to use `rates_for_input` and its reported partitions.
    pub fn forecast_rates_for_input(&self, input: f64) -> Option<Rates> {
        let mut rates = self.rates_for_input(input)?;
        if matches!(
            self.route.class,
            RouteClass::CodexSubscription | RouteClass::SyntheticSubscription
        ) {
            rates.write = rates.input;
        }
        Some(rates)
    }

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
    let source = source.filter(|s| subscribed && s.route.as_ref() == Some(&route));
    let entry = source.and_then(|s| s.models.iter().find(|entry| entry.model == model));
    let raw = entry.map(|e| e.raw.clone()).unwrap_or_default();
    let spec = model_spec(model);
    let catalog_capacity = spec.and_then(|s| s.context_tokens).map(u64::from);
    let advertised_capacity = entry
        .filter(|e| {
            e.field_status
                .get("context_window")
                .is_some_and(|s| s == "valid")
        })
        .and_then(|e| e.raw.context_window)
        .filter(|_| source.is_some_and(|s| s.status == "fresh_at_resolution"));
    // Discovery may constrain a model's operational window. Its absence does
    // not select a different primary/reviewer default or fabricate raw facts.
    let capacity = match (advertised_capacity, catalog_capacity) {
        (Some(advertised), Some(known)) => Some(advertised.min(known)),
        (advertised, known) => advertised.or(known),
    };
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
    let capacity_tokens = override_tokens.or(capacity.map(|v| v as u32));
    let rates = spec.and_then(|s| s.rates);
    let long = spec.and_then(|s| s.long_context);
    let provenance = |name: &str, status: &str, override_value| Provenance {
        source: name.into(),
        status: status.into(),
        revision: "route-catalog-v1".into(),
        fetched_at: None,
        fallback_reason: None,
        operator_override: override_value,
    };
    let mut capacity_provenance = provenance(
        spec.and_then(|s| s.source).unwrap_or("operator_context"),
        if capacity_tokens.is_some() {
            "catalog_model_policy_not_route_entitlement"
        } else {
            "unknown"
        },
        override_tokens,
    );
    capacity_provenance.revision = "shared-model-catalog-v2".into();
    if let Some(source) = source
        && advertised_capacity.is_some()
        && capacity == advertised_capacity
    {
        capacity_provenance.revision = source.revision.clone();
        capacity_provenance.fetched_at = Some(source.fetched_at);
        capacity_provenance.source = source.source.clone();
        capacity_provenance.status = source.status.clone();
    }
    if override_tokens.is_some() {
        capacity_provenance.status = "operator_override".into();
    }
    let mut price_provenance = provenance(
        "carry/src/model_catalog.json",
        if rates.is_some() {
            "catalog_api_equivalent_not_route_tariff"
        } else {
            "unknown"
        },
        None,
    );
    if let Some(source) = spec.and_then(|s| s.source) {
        price_provenance.source = source.into();
        price_provenance.revision = "shared-model-catalog-v2".into();
    }
    let cache = matches!(route.class, RouteClass::OfficialApi)
        .then(|| spec.and_then(|s| s.cache))
        .flatten();
    Ok(ResolvedModel {
        schema_version: 1,
        model: model.into(),
        route,
        raw,
        capacity_tokens,
        ordinary: ordinary_policy(
            capacity_tokens.unwrap_or(0),
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
        scenario_cache_policy: None,
        explicit_cache_transport: cache.is_some()
            && spec.is_some_and(|s| s.explicit_reviewer_cache),
        provenance: BTreeMap::from([
            ("capacity".into(), capacity_provenance),
            ("prices".into(), price_provenance),
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
    }
    .with_scenario_cache_policy())
}
