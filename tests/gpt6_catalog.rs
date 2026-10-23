//! Documented API facts are not subscription or gateway entitlement.
use carry::models::{Route, SourceCatalog, resolve_model};
use serde_json::json;

const MODELS: [(&str, f64, f64, f64, f64); 4] = [
    ("gpt-6-luna", 0.10, 0.01, 0.125, 0.50),
    ("gpt-6-sol", 2.00, 0.20, 2.50, 10.00),
    ("gpt-6.1-sol", 2.00, 0.10, 2.50, 10.00),
    ("gpt-6-astra", 10.00, 1.00, 12.50, 50.00),
];

#[test]
fn exact_gpt6_api_entries_resolve_documented_capacity_and_prices() {
    for (model, input, cached, write, output) in MODELS {
        let resolved = resolve_model(
            model,
            Route::new("https://api.openai.com/v1/responses", false),
            None,
            None,
            300000,
            2048,
            10,
        )
        .unwrap();
        assert_eq!(resolved.capacity_tokens, 1050000, "{model}");
        assert_eq!(resolved.ordinary.total_budget, 272000);
        assert_eq!(resolved.cheap_input_boundary, Some(272000));
        let rates = resolved.rates.unwrap();
        assert_eq!(
            (rates.input, rates.cached, rates.write, rates.output),
            (input, cached, write, output)
        );
        let short = resolved.rates_for_input(272000.0).unwrap();
        let long = resolved.rates_for_input(272001.0).unwrap();
        assert_eq!(long.input, short.input * 2.0);
        assert_eq!(long.cached, short.cached * 2.0);
        assert_eq!(long.write, short.write * 2.0);
        assert!((long.output - short.output * 1.5).abs() < 1e-18);
        let url = format!("https://developers.openai.com/api/docs/models/{model}");
        assert_eq!(resolved.provenance["capacity"].source, url);
        assert_eq!(
            resolved.provenance["capacity"].status,
            "documented_official_api"
        );
        assert!(resolved.provenance["capacity"].fallback_reason.is_none());
        assert_eq!(resolved.provenance["prices"].source, url);
        assert_eq!(
            resolved.provenance["prices"].status,
            "documented_api_equivalent_not_route_tariff"
        );
        // No Codex operational defaults, maxima, compaction triggers, or hashes
        // are inferred from a public API context-window specification.
        assert!(resolved.raw.context_window.is_none());
        assert!(resolved.raw.max_context_window.is_none());
        assert!(resolved.raw.auto_compact_token_limit.is_none());
        assert!(resolved.raw.effective_context_window_percent.is_none());
        assert!(resolved.raw.comp_hash.is_none());
    }
}

#[test]
fn public_gpt6_capacity_does_not_fill_missing_subscription_or_gateway_facts() {
    for (model, ..) in MODELS {
        for route in [
            Route::new("https://chatgpt.com/backend-api/codex/responses", true),
            Route::new("http://127.0.0.1:11001/v1/responses", true),
            Route::new("https://gateway.invalid/v1/responses", false),
        ] {
            let mut source = SourceCatalog::from_codex(
                &json!({"models":[{"slug":"gpt-5.6-sol","visibility":"list","context_window":272000,"max_context_window":872000}]}),
                123,
                true,
            ).unwrap();
            source.route = Some(route.clone());
            let resolved =
                resolve_model(model, route, Some(&source), None, 300000, 2048, 10).unwrap();
            assert_eq!(resolved.capacity_tokens, 300000);
            assert_eq!(resolved.provenance["capacity"].status, "fallback");
            assert!(resolved.provenance["capacity"].fallback_reason.is_some());
            assert!(resolved.raw.context_window.is_none());
            assert!(resolved.cache.is_none());
        }
    }
    for model in [
        "gpt-6",
        "gpt-6-sol-latest",
        "gpt-6.1-sol-extra",
        "gpt-6-terra",
    ] {
        let resolved = resolve_model(
            model,
            Route::new("https://api.openai.com/v1/responses", false),
            None,
            None,
            300000,
            2048,
            10,
        )
        .unwrap();
        assert_eq!(resolved.capacity_tokens, 300000);
        assert!(resolved.rates.is_none());
    }
}

#[test]
fn documented_api_capacity_bounds_overrides_without_claiming_other_tiers() {
    for (model, ..) in MODELS {
        let route = Route::new("https://api.openai.com/v1/responses", false);
        assert!(
            resolve_model(model, route.clone(), None, Some(1050001), 300000, 2048, 10).is_err()
        );
        let lower =
            resolve_model(model, route.clone(), None, Some(500000), 300000, 2048, 10).unwrap();
        assert_eq!(lower.capacity_tokens, 500000);
        assert_eq!(lower.provenance["capacity"].status, "operator_override");
        assert_eq!(lower.provenance["capacity"].operator_override, Some(500000));
        let mut other_tier = route;
        other_tier.processing_tier = "unknown-tier".into();
        let other = resolve_model(model, other_tier, None, None, 300000, 2048, 10).unwrap();
        assert_eq!(other.capacity_tokens, 300000);
        assert_eq!(other.provenance["capacity"].status, "fallback");
    }
}
