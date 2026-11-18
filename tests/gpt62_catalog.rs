//! Exact GPT-6.2 facts; synthetic route discovery is not entitlement proof.
use carry::models::{Route, SourceCatalog, resolve_model};
use serde_json::json;

#[test]
fn gpt62_catalog_rates_capacity_and_subscription_defaults_are_distinct() {
    for (id, default, input, cached, write, output) in [
        ("gpt-6.2-sol", 500000, 2.0, 0.1, 2.5, 10.0),
        ("gpt-6.2-astra", 272000, 8.0, 0.4, 10.0, 40.0),
    ] {
        let api = Route::new("https://api.openai.com/v1/responses", false);
        let m = resolve_model(id, api, None, None, 16384, 10).unwrap();
        assert_eq!(m.capacity_tokens, Some(1050000), "{id}");
        assert_eq!(m.cheap_input_boundary, Some(272000));
        let rates = m.rates.unwrap();
        assert_eq!(
            (rates.input, rates.cached, rates.write, rates.output),
            (input, cached, write, output)
        );
        let short = m.rates_for_input(272000.0).unwrap();
        let long = m.rates_for_input(272001.0).unwrap();
        assert_eq!(long.input, short.input * 2.0);
        assert_eq!(long.cached, short.cached * 2.0);
        assert_eq!(long.write, short.write * 2.0);
        assert!((long.output - short.output * 1.5).abs() < 1e-18);
        assert!(m.cache.is_some());
        let route = Route::new("https://chatgpt.com/backend-api/codex/responses", true);
        let mut source = SourceCatalog::from_codex(
            &json!({"models":[{
                "slug":id,"visibility":"list","context_window":default,"max_context_window":872000
            }]}),
            123,
            "synthetic_loopback_codex_metadata",
        )
        .unwrap();
        source.route = Some(route.clone());
        let m = resolve_model(id, route.clone(), Some(&source), None, 16384, 10).unwrap();
        assert_eq!(m.capacity_tokens, Some(default));
        assert_eq!(m.raw.max_context_window, Some(872000));
        assert_eq!(m.ordinary.total_budget, 272000);
        assert!(m.cache.is_none());
        assert!(!m.explicit_cache_transport);
        assert_eq!(m.price_basis, "legacy_api_equivalent_model");
        assert!(resolve_model(id, route, Some(&source), Some(default + 1), 16384, 10).is_err());
    }
}

#[test]
fn gpt62_unknown_suffixes_keep_unknown_facts() {
    for id in ["gpt-6.2", "gpt-6.2-sol-extra", "gpt-6.2-astra-900k"] {
        let m = resolve_model(
            id,
            Route::new("https://api.openai.com/v1/responses", false),
            None,
            None,
            0,
            0,
        )
        .unwrap();
        assert!(m.capacity_tokens.is_none());
        assert!(m.rates.is_none());
    }
}
