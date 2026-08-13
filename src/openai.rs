use std::time::Instant;

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::protocol::{Step, tool_definitions};

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
    /// The native assistant function-call item, retained byte-for-byte for replay.
    pub function_call: Value,
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

    pub fn request_body(
        &self,
        system: &str,
        task: &str,
        history: &[Value],
        control: &str,
    ) -> Value {
        let mut input = vec![
            json!({ "role": "system", "content": system }),
            json!({ "role": "user", "content": task }),
        ];
        input.extend_from_slice(history);
        input.push(json!({ "role": "user", "content": control }));

        json!({
            "model": self.model,
            "store": false,
            "input": input,
            "reasoning": {
                "effort": self.reasoning_effort,
                "context": "current_turn"
            },
            "tools": tool_definitions(),
            "tool_choice": "required",
            "parallel_tool_calls": false
        })
    }

    pub async fn step(
        &self,
        system: &str,
        task: &str,
        history: &[Value],
        control: &str,
    ) -> Result<ModelReply> {
        let body = self.request_body(system, task, history, control);

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

        let calls = function_calls(&raw);
        if calls.len() != 1 {
            bail!(
                "Responses API returned {} function calls; expected exactly one",
                calls.len()
            );
        }
        let function_call = calls[0].clone();
        let step = Step::from_function_call(&function_call)?;

        Ok(ModelReply {
            response_id: raw["id"].as_str().unwrap_or("unknown").to_owned(),
            step,
            function_call,
            usage: extract_usage(&raw),
            latency_ms: started.elapsed().as_millis() as u64,
            raw,
        })
    }
}

fn function_calls(raw: &Value) -> Vec<&Value> {
    raw.get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .collect()
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
    fn extracts_one_function_call_and_usage() {
        let raw = json!({
            "output": [
                {"type":"reasoning","encrypted_content":"opaque"},
                {
                    "type":"function_call",
                    "call_id":"call_1",
                    "name":"finish",
                    "arguments":"{\"answer\":\"done\",\"context_management\":{\"retain_ids\":[],\"add_memories\":[]}}"
                }
            ],
            "usage": {
                "input_tokens": 10,
                "input_tokens_details": {"cached_tokens": 4},
                "output_tokens": 3,
                "output_tokens_details": {"reasoning_tokens": 1},
                "total_tokens": 13
            }
        });
        assert_eq!(function_calls(&raw).len(), 1);
        let usage = extract_usage(&raw);
        assert_eq!(usage.cached_input_tokens, 4);
        assert_eq!(usage.reasoning_tokens, 1);
    }

    #[test]
    fn request_keeps_static_prefix_and_native_history_order() {
        let client = OpenAiClient::new(
            "https://example.invalid/v1".into(),
            "secret".into(),
            "gpt-5.6-luna".into(),
            "medium".into(),
        );
        let history = vec![
            json!({"type":"function_call","call_id":"call_1","name":"shell","arguments":"{}"}),
            json!({"type":"function_call_output","call_id":"call_1","output":"verbatim"}),
        ];
        let body = client.request_body("system", "task", &history, "control");
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "system");
        assert_eq!(input[1]["content"], "task");
        assert_eq!(input[2..4], history);
        assert_eq!(input[4]["content"], "control");
        assert_eq!(body["tool_choice"], "required");
        assert_eq!(body["parallel_tool_calls"], false);
    }
}
