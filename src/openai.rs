use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use reqwest::{Client, StatusCode, header::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::protocol::{Step, tool_definitions};

const MAX_RESPONSE_RETRIES: usize = 5;
const MAX_TOTAL_RETRY_WAIT: Duration = Duration::from_secs(60);

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
    pub cache_write_input_tokens: u64,
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
    pub response_retries: usize,
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
            json!({
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": task,
                    "prompt_cache_breakpoint": { "mode": "explicit" }
                }]
            }),
        ];
        input.extend_from_slice(history);
        input.push(json!({ "role": "developer", "content": control }));

        json!({
            "model": self.model,
            "store": false,
            "prompt_cache_key": prompt_cache_key(&self.model, system, task),
            "prompt_cache_options": { "mode": "implicit" },
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
        let mut retries = 0;
        let mut retry_wait = Duration::ZERO;
        let mut retry_stopped_reason = None;
        let raw = loop {
            let response = self
                .http
                .post(format!("{}/responses", self.api_base))
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await
                .context("Responses API request failed")?;
            let status = response.status();
            let headers = response.headers().clone();
            let response_body = response
                .bytes()
                .await
                .context("Responses API response body read failed")?;
            if status.is_success() {
                break serde_json::from_slice(&response_body)
                    .context("Responses API returned invalid JSON")?;
            }
            if retryable_rate_limit(status, &response_body) && retries < MAX_RESPONSE_RETRIES {
                let delay = retry_delay(&headers, retries);
                if delay > MAX_TOTAL_RETRY_WAIT.saturating_sub(retry_wait) {
                    let reason = format!(
                        "server backoff of {}ms exceeds the remaining retry wait budget",
                        delay.as_millis()
                    );
                    eprintln!("Responses API returned {status}; {reason}");
                    retry_stopped_reason = Some(reason);
                } else {
                    retries += 1;
                    retry_wait += delay;
                    eprintln!(
                        "Responses API returned {status}; retrying in {}ms ({retries}/{MAX_RESPONSE_RETRIES})",
                        delay.as_millis(),
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
            }
            let raw = serde_json::from_slice::<Value>(&response_body).unwrap_or_else(|_| {
                Value::String(String::from_utf8_lossy(&response_body).into_owned())
            });
            if let Some(reason) = retry_stopped_reason {
                bail!(
                    "Responses API returned {status} after {retries} retries and {}ms waiting; {reason}: {raw}",
                    retry_wait.as_millis()
                );
            }
            if retries > 0 {
                bail!(
                    "Responses API returned {status} after {retries} retries and {}ms waiting: {raw}",
                    retry_wait.as_millis()
                );
            }
            bail!("Responses API returned {status}: {raw}");
        };

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
            response_retries: retries,
            raw,
        })
    }
}

fn retryable_rate_limit(status: StatusCode, response_body: &[u8]) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        && serde_json::from_slice::<Value>(response_body)
            .ok()
            .and_then(|body| body["error"]["code"].as_str().map(str::to_owned))
            .as_deref()
            == Some("rate_limit_exceeded")
}

fn retry_delay(headers: &HeaderMap, attempt: usize) -> Duration {
    retry_delay_at(headers, attempt, SystemTime::now())
}

fn retry_delay_at(headers: &HeaderMap, attempt: usize, now: SystemTime) -> Duration {
    let server_delay = headers
        .get("retry-after-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_retry_delay(value, 1_000.0))
        .or_else(|| {
            headers
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| {
                    parse_retry_delay(value, 1.0).or_else(|| {
                        httpdate::parse_http_date(value)
                            .ok()
                            .map(|date| date.duration_since(now).unwrap_or(Duration::ZERO))
                    })
                })
        });
    server_delay.unwrap_or_else(|| Duration::from_secs(1_u64 << attempt.min(4)))
}

fn parse_retry_delay(value: &str, units_per_second: f64) -> Option<Duration> {
    let seconds = value.parse::<f64>().ok()? / units_per_second;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    if seconds > MAX_TOTAL_RETRY_WAIT.as_secs_f64() {
        return Some(MAX_TOTAL_RETRY_WAIT + Duration::from_millis(1));
    }
    Some(Duration::from_secs_f64(seconds))
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
        cache_write_input_tokens: usage["input_tokens_details"]["cache_write_tokens"]
            .as_u64()
            .unwrap_or_default(),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or_default(),
        reasoning_tokens: usage["output_tokens_details"]["reasoning_tokens"]
            .as_u64()
            .unwrap_or_default(),
        total_tokens: usage["total_tokens"].as_u64().unwrap_or_default(),
    }
}

fn prompt_cache_key(model: &str, system: &str, task: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in model
        .bytes()
        .chain([0])
        .chain(system.bytes())
        .chain([0])
        .chain(task.bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("carry-{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{ErrorKind, Read, Write},
        net::TcpListener,
        sync::mpsc,
        thread,
        time::Instant as StdInstant,
    };

    fn response_server(
        responses: Vec<String>,
    ) -> (String, mpsc::Receiver<Vec<u8>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            for response in responses {
                let deadline = StdInstant::now() + Duration::from_secs(2);
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error)
                            if error.kind() == ErrorKind::WouldBlock
                                && StdInstant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("test server accept failed: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut chunk = [0_u8; 4096];
                loop {
                    let read = stream.read(&mut chunk).unwrap();
                    assert_ne!(read, 0, "client closed before sending a complete request");
                    request.extend_from_slice(&chunk[..read]);
                    let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
                    else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or_default();
                    if request.len() >= header_end + 4 + content_length {
                        sender
                            .send(request[header_end + 4..header_end + 4 + content_length].to_vec())
                            .unwrap();
                        break;
                    }
                }
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (format!("http://{address}"), receiver, server)
    }

    fn http_response(status: &str, headers: &str, body: &Value) -> String {
        let body = body.to_string();
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn retries_429_after_server_delay_with_identical_request_body() {
        let success = json!({
            "id": "response-1",
            "output": [{
                "type": "function_call",
                "call_id": "call-1",
                "name": "finish",
                "arguments": "{\"answer\":\"done\",\"context_management\":{\"retain_volatile_ids\":[],\"release_stable_ids\":[],\"add_memories\":[]}}"
            }],
            "usage": {}
        });
        let (api_base, requests, server) = response_server(vec![
            http_response(
                "429 Too Many Requests",
                "Retry-After-Ms: 20\r\nRetry-After: 30\r\n",
                &json!({"error": {"code": "rate_limit_exceeded"}}),
            ),
            http_response("200 OK", "", &success),
        ]);
        let client = OpenAiClient::new(api_base, "secret".into(), "model".into(), "medium".into());

        let started = Instant::now();
        let reply = client.step("system", "task", &[], "control").await.unwrap();

        assert_eq!(reply.response_id, "response-1");
        assert_eq!(reply.response_retries, 1);
        assert!(started.elapsed() >= Duration::from_millis(15));
        assert_eq!(
            requests.recv_timeout(Duration::from_secs(2)).unwrap(),
            requests.recv_timeout(Duration::from_secs(2)).unwrap()
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn does_not_retry_ambiguous_server_error() {
        let (api_base, requests, server) = response_server(vec![
            "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 0\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbusy".to_owned(),
        ]);
        let client = OpenAiClient::new(api_base, "secret".into(), "model".into(), "medium".into());

        let error = client
            .step("system", "task", &[], "control")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("busy"));
        requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(requests.try_recv().is_err());
        server.join().unwrap();
    }

    #[test]
    fn invalid_server_retry_delay_uses_bounded_fallback() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "NaN".parse().unwrap());

        assert_eq!(retry_delay(&headers, 0), Duration::from_secs(1));
    }

    #[test]
    fn honors_http_date_retry_after() {
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut headers = HeaderMap::new();
        headers.insert(
            "retry-after",
            httpdate::fmt_http_date(now + Duration::from_secs(17))
                .parse()
                .unwrap(),
        );

        assert_eq!(retry_delay_at(&headers, 0, now), Duration::from_secs(17));
    }

    #[tokio::test]
    async fn stops_after_five_response_retries() {
        let rate_limit = http_response(
            "429 Too Many Requests",
            "Retry-After: 0\r\n",
            &json!({"error": {"code": "rate_limit_exceeded"}}),
        );
        let (api_base, requests, server) = response_server(vec![rate_limit; 6]);
        let client = OpenAiClient::new(api_base, "secret".into(), "model".into(), "medium".into());

        let error = client
            .step("system", "task", &[], "control")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("429 Too Many Requests"));
        assert!(error.to_string().contains("after 5 retries"));
        for _ in 0..6 {
            requests.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        assert!(requests.try_recv().is_err());
        server.join().unwrap();
    }

    #[tokio::test]
    async fn does_not_retry_quota_429() {
        let (api_base, requests, server) = response_server(vec![http_response(
            "429 Too Many Requests",
            "Retry-After: 0\r\n",
            &json!({"error": {"code": "insufficient_quota"}}),
        )]);
        let client = OpenAiClient::new(api_base, "secret".into(), "model".into(), "medium".into());

        let error = client
            .step("system", "task", &[], "control")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("insufficient_quota"));
        requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(requests.try_recv().is_err());
        server.join().unwrap();
    }

    #[tokio::test]
    async fn does_not_send_earlier_than_server_delay_budget() {
        let (api_base, requests, server) = response_server(vec![http_response(
            "429 Too Many Requests",
            "Retry-After: 61\r\n",
            &json!({"error": {"code": "rate_limit_exceeded"}}),
        )]);
        let client = OpenAiClient::new(api_base, "secret".into(), "model".into(), "medium".into());

        let error = client
            .step("system", "task", &[], "control")
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("exceeds the remaining retry wait budget")
        );
        requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(requests.try_recv().is_err());
        server.join().unwrap();
    }

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
                "input_tokens_details": {"cached_tokens": 4, "cache_write_tokens": 6},
                "output_tokens": 3,
                "output_tokens_details": {"reasoning_tokens": 1},
                "total_tokens": 13
            }
        });
        assert_eq!(function_calls(&raw).len(), 1);
        let usage = extract_usage(&raw);
        assert_eq!(usage.cached_input_tokens, 4);
        assert_eq!(usage.cache_write_input_tokens, 6);
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
        assert_eq!(input[1]["content"][0]["text"], "task");
        assert_eq!(input[2..4], history);
        assert_eq!(input[4]["role"], "developer");
        assert_eq!(input[4]["content"], "control");
        assert_eq!(body["tool_choice"], "required");
        assert_eq!(body["parallel_tool_calls"], false);
    }

    #[test]
    fn request_combines_implicit_caching_with_the_explicit_task_fallback() {
        let client = OpenAiClient::new(
            "https://example.invalid/v1".into(),
            "secret".into(),
            "gpt-5.6-luna".into(),
            "medium".into(),
        );

        let body = client.request_body("system", "stable task", &[], "changing control");
        let same_task = client.request_body("system", "stable task", &[], "other control");
        let other_task = client.request_body("system", "other task", &[], "changing control");

        assert_eq!(body["prompt_cache_options"]["mode"], "implicit");
        assert_eq!(
            body["input"][1]["content"][0]["prompt_cache_breakpoint"]["mode"],
            "explicit"
        );
        assert_eq!(body["input"][1]["content"][0]["text"], "stable task");
        assert_eq!(body["prompt_cache_key"], same_task["prompt_cache_key"]);
        assert_ne!(body["prompt_cache_key"], other_task["prompt_cache_key"]);
    }
}
