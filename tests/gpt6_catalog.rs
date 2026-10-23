//! Catalog policy is shared across routes; raw discovery stays route-bound.
use carry::models::{Route, SourceCatalog, resolve_model};
use serde_json::json;

const MODELS: [(&str, f64, f64, f64, f64); 4] = [
    ("gpt-6-luna", 0.10, 0.01, 0.125, 0.50),
    ("gpt-6-sol", 2.00, 0.20, 2.50, 10.00),
    ("gpt-6.1-sol", 2.00, 0.10, 2.50, 10.00),
    ("gpt-6-astra", 10.00, 1.00, 12.50, 50.00),
];

#[test]
fn exact_gpt6_entries_share_capacity_and_prices_across_routes() {
    for (model, input, cached, write, output) in MODELS {
        for route in [
            Route::new("https://api.openai.com/v1/responses", false),
            Route::new("https://chatgpt.com/backend-api/codex/responses", true),
            Route::new("http://127.0.0.1:11001/v1/responses", true),
            Route::new("https://gateway.invalid/v1/responses", false),
        ] {
            let official = route.class == carry::models::RouteClass::OfficialApi;
            let mut source = SourceCatalog::from_codex(
                &json!({"models":[{"slug":"gpt-5.6-sol","visibility":"list","context_window":272000,"max_context_window":872000}]}),
                123,
                true,
            ).unwrap();
            source.route = Some(route.clone());
            let resolved = resolve_model(model, route, Some(&source), None, 2048, 10).unwrap();
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
                "catalog_model_policy_not_route_entitlement"
            );
            assert!(resolved.provenance["capacity"].fallback_reason.is_none());
            assert_eq!(resolved.provenance["prices"].source, url);
            assert_eq!(
                resolved.provenance["prices"].status,
                "catalog_api_equivalent_not_route_tariff"
            );
            // A static model policy is not an advertisement, alias, subscription
            // compaction instruction, or cache-transport entitlement.
            assert!(resolved.raw.context_window.is_none());
            assert!(resolved.raw.max_context_window.is_none());
            assert!(resolved.raw.auto_compact_token_limit.is_none());
            assert!(resolved.raw.effective_context_window_percent.is_none());
            assert!(resolved.raw.comp_hash.is_none());
            if !official {
                assert!(resolved.cache.is_none());
            }
        }
    }
}

#[test]
fn unknown_models_need_metadata_or_explicit_limits_not_role_fallbacks() {
    for model in [
        "gpt-6",
        "gpt-6-sol-latest",
        "gpt-6.1-sol-extra",
        "gpt-6-terra",
    ] {
        let route = Route::new("https://api.openai.com/v1/responses", false);
        assert!(resolve_model(model, route.clone(), None, None, 2048, 10).is_err());
        let explicit = resolve_model(model, route, None, Some(300000), 2048, 10).unwrap();
        assert_eq!(explicit.capacity_tokens, 300000);
        assert!(explicit.rates.is_none());
        assert_eq!(explicit.provenance["capacity"].status, "operator_override");
    }
}

#[test]
fn catalog_capacity_bounds_overrides_without_manufacturing_route_tariffs() {
    for (model, ..) in MODELS {
        let route = Route::new("https://api.openai.com/v1/responses", false);
        assert!(resolve_model(model, route.clone(), None, Some(1050001), 2048, 10).is_err());
        let lower = resolve_model(model, route.clone(), None, Some(500000), 2048, 10).unwrap();
        assert_eq!(lower.capacity_tokens, 500000);
        assert_eq!(lower.provenance["capacity"].status, "operator_override");
        assert_eq!(lower.provenance["capacity"].operator_override, Some(500000));
        let mut other_tier = route;
        other_tier.processing_tier = "unknown-tier".into();
        let other = resolve_model(model, other_tier, None, None, 2048, 10).unwrap();
        assert_eq!(other.capacity_tokens, 1050000);
        assert_eq!(
            other.provenance["prices"].status,
            "catalog_api_equivalent_not_route_tariff"
        );
    }
}
