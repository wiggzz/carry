use std::time::Instant;

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::protocol::{Step, step_schema};

#[derive(Clone, Debug)]
pub struct OpenAiClient {
    http: Client,
    api_base: String,
    api_key: String,
    model: String,
    reasoning_effort: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelReply {
    pub response_id: String,
    pub step: Step,
    pub usage: Usage,
    pub latency_ms: u64,
    pub raw: Value,
}

impl OpenAiClient {
    pub fn new(api_base: String, api_key: String, model: String, reasoning_effort: String) -> Self {
        Self {
            http: Client::new(),
            api_base: api_base.trim_end_matches('/').to_owned(),
            api_key,
            model,
            reasoning_effort,
        }
    }

    pub fn request_body(&self, system: &str, task: &str, context: &str) -> Value {
        json!({
            "model": self.model,
            "store": false,
            "input": [
                { "role": "system", "content": system },
                { "role": "user", "content": task },
                { "role": "user", "content": context }
            ],
            "reasoning": { "effort": self.reasoning_effort },
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "carry_step",
                    "strict": true,
                    "schema": step_schema()
                }
            }
        })
    }

    pub async fn step(&self, system: &str, task: &str, context: &str) -> Result<ModelReply> {
        let body = self.request_body(system, task, context);

        let started = Instant::now();
        let response = self
            .http
            .post(format!("{}/responses", self.api_base))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("Responses API request failed")?;
        let status = response.status();
        let raw: Value = response
            .json()
            .await
            .context("Responses API returned invalid JSON")?;
        if !status.is_success() {
            bail!("Responses API returned {status}: {raw}");
        }

        let text = extract_output_text(&raw)
            .context("Responses API response contained no output_text item")?;
        let step: Step = serde_json::from_str(text)
            .with_context(|| format!("structured response was not a valid Step: {text}"))?;
        step.action.validate()?;

        Ok(ModelReply {
            response_id: raw["id"].as_str().unwrap_or("unknown").to_owned(),
            step,
            usage: extract_usage(&raw),
            latency_ms: started.elapsed().as_millis() as u64,
            raw,
        })
    }
}

fn extract_output_text(raw: &Value) -> Option<&str> {
    raw.get("output")?
        .as_array()?
        .iter()
        .filter_map(|item| item.get("content")?.as_array())
        .flatten()
        .find(|content| content.get("type").and_then(Value::as_str) == Some("output_text"))?
        .get("text")?
        .as_str()
}

fn extract_usage(raw: &Value) -> Usage {
    let usage = &raw["usage"];
    Usage {
        input_tokens: usage["input_tokens"].as_u64().unwrap_or_default(),
        cached_input_tokens: usage["input_tokens_details"]["cached_tokens"]
            .as_u64()
            .unwrap_or_default(),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or_default(),
        reasoning_tokens: usage["output_tokens_details"]["reasoning_tokens"]
            .as_u64()
            .unwrap_or_default(),
        total_tokens: usage["total_tokens"].as_u64().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_message_text_and_usage() {
        let raw = json!({
            "output": [{"type":"message","content":[{"type":"output_text","text":"{}"}]}],
            "usage": {
                "input_tokens": 10,
                "input_tokens_details": {"cached_tokens": 4},
                "output_tokens": 3,
                "output_tokens_details": {"reasoning_tokens": 1},
                "total_tokens": 13
            }
        });
        assert_eq!(extract_output_text(&raw), Some("{}"));
        let usage = extract_usage(&raw);
        assert_eq!(usage.cached_input_tokens, 4);
        assert_eq!(usage.reasoning_tokens, 1);
    }
}
