//! Shared modeled rates and cache support. Unknown prices/capabilities stay unknown.
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
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

#[derive(Clone, Copy, Debug, Serialize)]
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
