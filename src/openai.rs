use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use reqwest::{
    Client, StatusCode,
    header::{CONTENT_TYPE, HeaderMap},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::protocol::{Step, tool_definitions};

const MAX_RESPONSE_RETRIES: usize = 5;
const MAX_TOTAL_RETRY_WAIT: Duration = Duration::from_secs(60);
#[cfg(test)]
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
#[cfg(test)]
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct PromptCacheCapabilities {
    pub minimum_prefix_tokens: usize,
    pub max_read_breakpoints: usize,
    pub max_write_breakpoints: usize,
    pub implicit_breakpoint_uses_write_slot: bool,
}

const OPENAI_GPT_56_PROMPT_CACHE: PromptCacheCapabilities = PromptCacheCapabilities {
    minimum_prefix_tokens: 1_024,
    max_read_breakpoints: 50,
    max_write_breakpoints: 4,
    implicit_breakpoint_uses_write_slot: true,
};

pub(crate) fn prompt_cache_capabilities(model: &str) -> Option<PromptCacheCapabilities> {
    exact_model_prompt_cache_capabilities(model)
        .or_else(|| model_family_prompt_cache_capabilities(model))
}

fn exact_model_prompt_cache_capabilities(model: &str) -> Option<PromptCacheCapabilities> {
    match model {
        "gpt-5.6-luna" | "gpt-5.6-sol" | "gpt-5.6-terra" => Some(OPENAI_GPT_56_PROMPT_CACHE),
        _ => None,
    }
}

fn model_family_prompt_cache_capabilities(model: &str) -> Option<PromptCacheCapabilities> {
    model
        .starts_with("gpt-5.6-")
        .then_some(OPENAI_GPT_56_PROMPT_CACHE)
}

#[derive(Clone, Debug)]
pub struct OpenAiClient {
    http: Client,
    api_base: String,
    api_key: String,
    model: String,
    reasoning_effort: String,
    prompt_cache_key: String,
    request_timeout: Duration,
    connect_timeout: Duration,
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

#[derive(Clone, Debug, Default, Serialize)]
pub struct ModelProgress {
    /// Estimated while streaming; replaced with the API total on completion.
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub output_events: u64,
}

#[derive(Debug)]
pub struct ModelReply {
    pub response_id: String,
    pub step: Step,
    /// Every native output item, retained byte-for-byte for manual continuation.
    pub output_items: Vec<Value>,
    pub function_call: Value,
    pub usage: Usage,
    pub latency_ms: u64,
    pub response_retries: usize,
    pub raw: Value,
}

impl OpenAiClient {
    #[cfg(test)]
    fn new(api_base: String, api_key: String, model: String, reasoning_effort: String) -> Self {
        Self::with_timeouts(
            api_base,
            api_key,
            model,
            reasoning_effort,
            DEFAULT_REQUEST_TIMEOUT,
            DEFAULT_CONNECT_TIMEOUT,
        )
        .expect("default OpenAI HTTP client configuration is valid")
    }

    #[cfg(test)]
    pub fn with_timeouts(
        api_base: String,
        api_key: String,
        model: String,
        reasoning_effort: String,
        request_timeout: Duration,
        connect_timeout: Duration,
    ) -> Result<Self> {
        Self::with_timeouts_and_prompt_cache_key(
            api_base,
            api_key,
            model,
            reasoning_effort,
            new_prompt_cache_key(),
            request_timeout,
            connect_timeout,
        )
    }

    pub fn with_timeouts_and_prompt_cache_key(
        api_base: String,
        api_key: String,
        model: String,
        reasoning_effort: String,
        prompt_cache_key: String,
        request_timeout: Duration,
        connect_timeout: Duration,
    ) -> Result<Self> {
        if prompt_cache_key.is_empty() {
            bail!("prompt cache key must not be empty");
        }
        let http = Client::builder()
            .timeout(request_timeout)
            .connect_timeout(connect_timeout)
            .build()
            .context("failed to build OpenAI HTTP client")?;
        Ok(Self {
            http,
            api_base: api_base.trim_end_matches('/').to_owned(),
            api_key,
            model,
            reasoning_effort,
            prompt_cache_key,
            request_timeout,
            connect_timeout,
        })
    }

    pub(crate) fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    pub(crate) fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }

    pub(crate) fn prompt_cache_capabilities(&self) -> Option<PromptCacheCapabilities> {
        prompt_cache_capabilities(&self.model)
    }

    pub fn request_body(&self, system: &str, history: &[Value]) -> Value {
        let mut input = vec![json!({ "role": "system", "content": system })];
        input.extend_from_slice(history);

        json!({
            "model": self.model,
            "store": false,
            "prompt_cache_key": self.prompt_cache_key,
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

    #[cfg(test)]
    pub async fn step(&self, system: &str, history: &[Value]) -> Result<ModelReply> {
        self.step_with_progress(system, history, |_| {}).await
    }

    pub async fn step_with_progress<F>(
        &self,
        system: &str,
        history: &[Value],
        mut progress: F,
    ) -> Result<ModelReply>
    where
        F: FnMut(ModelProgress),
    {
        let mut body = self.request_body(system, history);
        body["stream"] = json!(true);

        let started = Instant::now();
        let mut retries = 0;
        let mut retry_wait = Duration::ZERO;
        let mut retry_stopped_reason = None;
        let raw = loop {
            let response = match self
                .http
                .post(format!("{}/responses", self.api_base))
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) if retries < MAX_RESPONSE_RETRIES => {
                    let delay = transport_retry_delay(retries);
                    if delay > MAX_TOTAL_RETRY_WAIT.saturating_sub(retry_wait) {
                        return Err(error).context(format!(
                            "Responses API request failed after {retries} retries and {}ms waiting",
                            retry_wait.as_millis()
                        ));
                    }
                    retries += 1;
                    retry_wait += delay;
                    eprintln!(
                        "Responses API transport error: {error}; retrying in {}ms ({retries}/{MAX_RESPONSE_RETRIES})",
                        delay.as_millis(),
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                Err(error) => {
                    return Err(error).context(format!(
                        "Responses API request failed after {retries} retries and {}ms waiting",
                        retry_wait.as_millis()
                    ));
                }
            };
            let status = response.status();
            let headers = response.headers().clone();
            let is_stream = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("text/event-stream"));
            if status.is_success() && is_stream {
                // Read completed SSE frames as they arrive. `bytes()` would defer all progress
                // until the model has finished its (possibly long) reasoning turn.
                break read_sse_response(response, &mut progress).await?;
            }
            let response_body = match response.bytes().await {
                Ok(body) => body,
                Err(error) if retries < MAX_RESPONSE_RETRIES => {
                    let delay = transport_retry_delay(retries);
                    if delay > MAX_TOTAL_RETRY_WAIT.saturating_sub(retry_wait) {
                        return Err(error).context(format!(
                            "Responses API response body read failed after {retries} retries and {}ms waiting",
                            retry_wait.as_millis()
                        ));
                    }
                    retries += 1;
                    retry_wait += delay;
                    eprintln!(
                        "Responses API response body read failed: {error}; retrying in {}ms ({retries}/{MAX_RESPONSE_RETRIES})",
                        delay.as_millis(),
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                Err(error) => return Err(error).context(format!(
                    "Responses API response body read failed after {retries} retries and {}ms waiting",
                    retry_wait.as_millis()
                )),
            };
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
        let output_items = raw["output"]
            .as_array()
            .context("Responses API response has no output array")?
            .clone();

        Ok(ModelReply {
            response_id: raw["id"].as_str().unwrap_or("unknown").to_owned(),
            step,
            output_items,
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

fn transport_retry_delay(attempt: usize) -> Duration {
    Duration::from_millis(250 * (1_u64 << attempt.min(4)))
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

pub(crate) fn new_prompt_cache_key() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);
    format!("carry-{}-{now}-{sequence}", std::process::id())
}

async fn read_sse_response<F>(mut response: reqwest::Response, progress: &mut F) -> Result<Value>
where
    F: FnMut(ModelProgress),
{
    let mut pending = Vec::new();
    let mut completed = None;
    let mut current = ModelProgress::default();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("Responses API stream read failed")?
    {
        pending.extend_from_slice(&chunk);
        while let Some((end, separator_len)) = sse_frame_end(&pending) {
            let frame: Vec<_> = pending.drain(..end + separator_len).collect();
            process_sse_frame(&frame[..end], &mut completed, &mut current, progress)?;
        }
    }
    let response = completed.context("Responses API stream ended without response.completed")?;
    let usage = extract_usage(&response);
    progress(ModelProgress {
        output_tokens: usage.output_tokens,
        reasoning_output_tokens: usage.reasoning_tokens,
        output_events: current.output_events,
    });
    Ok(response)
}

fn sse_frame_end(pending: &[u8]) -> Option<(usize, usize)> {
    pending
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .map(|index| (index, 2))
        .or_else(|| {
            pending
                .windows(4)
                .position(|sequence| sequence == b"\r\n\r\n")
                .map(|index| (index, 4))
        })
}

fn process_sse_frame<F>(
    frame: &[u8],
    completed: &mut Option<Value>,
    current: &mut ModelProgress,
    progress: &mut F,
) -> Result<()>
where
    F: FnMut(ModelProgress),
{
    let text = std::str::from_utf8(frame).context("Responses API stream was not UTF-8")?;
    let data = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() || data == "[DONE]" {
        return Ok(());
    }
    let event: Value =
        serde_json::from_str(&data).context("Responses API stream contained invalid JSON")?;
    let event_type = event["type"].as_str().unwrap_or_default();
    if let Some(delta) = event["delta"].as_str() {
        // Private reasoning tokens are not exposed as token deltas. This is an explicit
        // approximate activity counter and is corrected by final usage on completion.
        current.output_tokens += (delta.len().max(1) as u64).div_ceil(4);
        if event_type.contains("reasoning") {
            current.reasoning_output_tokens += (delta.len().max(1) as u64).div_ceil(4);
        }
        current.output_events += 1;
        progress(current.clone());
    } else if event_type.contains(".delta") {
        current.output_events += 1;
        progress(current.clone());
    }
    if event_type == "response.completed" {
        *completed = event.get("response").cloned();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_openai_models_resolve_prompt_cache_capabilities() {
        let capabilities = prompt_cache_capabilities("gpt-5.6-luna").unwrap();
        assert_eq!(capabilities.minimum_prefix_tokens, 1_024);
        assert_eq!(capabilities.max_read_breakpoints, 50);
        assert_eq!(capabilities.max_write_breakpoints, 4);
        assert!(capabilities.implicit_breakpoint_uses_write_slot);
        assert_eq!(
            prompt_cache_capabilities("gpt-5.6-future"),
            Some(capabilities)
        );
    }

    #[test]
    fn unknown_models_disable_prompt_cache_assumptions() {
        assert_eq!(prompt_cache_capabilities("custom-model"), None);
    }

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
                stream.set_nonblocking(false).unwrap();
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

    fn sse_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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
                "arguments": "{\"answer\":\"done\",\"context\":{\"protected\":[],\"removable\":[],\"remember\":[]}}"
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
        let reply = client.step("system", &[]).await.unwrap();

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
    async fn retries_when_connection_closes_before_a_response() {
        let success = json!({
            "id": "response-after-disconnect",
            "output": [{
                "type": "function_call",
                "call_id": "call-1",
                "name": "finish",
                "arguments": "{\"answer\":\"done\",\"context\":{\"protected\":[],\"removable\":[],\"remember\":[]}}"
            }],
            "usage": {}
        });
        let (api_base, requests, server) =
            response_server(vec![String::new(), http_response("200 OK", "", &success)]);
        let client = OpenAiClient::new(api_base, "secret".into(), "model".into(), "medium".into());

        let reply = client.step("system", &[]).await.unwrap();

        assert_eq!(reply.response_id, "response-after-disconnect");
        assert_eq!(reply.response_retries, 1);
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

        let error = client.step("system", &[]).await.unwrap_err();

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

        let error = client.step("system", &[]).await.unwrap_err();

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

        let error = client.step("system", &[]).await.unwrap_err();

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

        let error = client.step("system", &[]).await.unwrap_err();

        assert!(
            error
                .to_string()
                .contains("exceeds the remaining retry wait budget")
        );
        requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(requests.try_recv().is_err());
        server.join().unwrap();
    }

    #[tokio::test]
    async fn reads_crlf_delimited_sse_response() {
        let completed = json!({
            "id": "response-crlf",
            "output": [{
                "type":"function_call",
                "call_id":"call-crlf",
                "name":"finish",
                "arguments":"{\"answer\":\"done\",\"context\":{\"protected\":[],\"removable\":[],\"remember\":[]}}"
            }],
            "usage": {"output_tokens": 2}
        });
        let body = format!(
            "data: {{\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}}\r\n\r\ndata: {{\"type\":\"response.completed\",\"response\":{completed}}}\r\n\r\n"
        );
        let (api_base, requests, server) = response_server(vec![sse_response(&body)]);
        let client = OpenAiClient::new(api_base, "secret".into(), "model".into(), "medium".into());

        let reply = client.step("system", &[]).await.unwrap();

        assert_eq!(reply.response_id, "response-crlf");
        requests.recv_timeout(Duration::from_secs(2)).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn sse_frame_boundaries_accept_lf_and_crlf() {
        assert_eq!(sse_frame_end(b"data: one\n\nrest"), Some((9, 2)));
        assert_eq!(sse_frame_end(b"data: one\r\n\r\nrest"), Some((9, 4)));
    }

    #[test]
    fn sse_deltas_report_live_progress_and_completed_response() {
        let mut completed = None;
        let mut current = ModelProgress::default();
        let mut updates = Vec::new();
        process_sse_frame(
            br#"event: response.reasoning_summary_text.delta
data: {"type":"response.reasoning_summary_text.delta","delta":"thinking"}"#,
            &mut completed,
            &mut current,
            &mut |progress| updates.push(progress),
        )
        .unwrap();
        process_sse_frame(
            br#"data: {"type":"response.output_text.delta","delta":"done"}"#,
            &mut completed,
            &mut current,
            &mut |progress| updates.push(progress),
        )
        .unwrap();
        process_sse_frame(
            br#"data: {"type":"response.completed","response":{"id":"response-1","usage":{"output_tokens":9}}}"#,
            &mut completed,
            &mut current,
            &mut |_| {},
        )
        .unwrap();

        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].reasoning_output_tokens, 2);
        assert!(updates[1].output_tokens > updates[0].output_tokens);
        assert_eq!(completed.unwrap()["id"], "response-1");
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
                    "arguments":"{\"answer\":\"done\",\"context\":{\"protected\":[],\"removable\":[],\"remember\":[]}}"
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
    fn client_uses_configured_request_and_connect_timeouts() {
        let client = OpenAiClient::with_timeouts(
            "https://example.invalid/v1".into(),
            "secret".into(),
            "model".into(),
            "medium".into(),
            Duration::from_secs(123),
            Duration::from_secs(7),
        )
        .unwrap();

        assert_eq!(client.request_timeout(), Duration::from_secs(123));
        assert_eq!(client.connect_timeout(), Duration::from_secs(7));
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
        let body = client.request_body("system", &history);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "system");
        assert_eq!(input[1..3], history);
        assert_eq!(body["tool_choice"], "required");
        assert_eq!(body["parallel_tool_calls"], false);
    }

    #[test]
    fn resumed_client_uses_the_persisted_prompt_cache_key() {
        let client = OpenAiClient::with_timeouts_and_prompt_cache_key(
            "https://example.invalid/v1".into(),
            "secret".into(),
            "gpt-5.6-luna".into(),
            "medium".into(),
            "resumable-cache-affinity".into(),
            Duration::from_secs(30),
            Duration::from_secs(5),
        )
        .unwrap();

        let body = client.request_body("system", &[]);
        assert_eq!(body["prompt_cache_key"], "resumable-cache-affinity");
    }

    #[test]
    fn request_combines_implicit_caching_with_a_stable_session_key() {
        let client = OpenAiClient::new(
            "https://example.invalid/v1".into(),
            "secret".into(),
            "gpt-5.6-luna".into(),
            "medium".into(),
        );

        let stable = vec![json!({"role":"user","content":"stable task"})];
        let same_prefix = vec![
            stable[0].clone(),
            json!({"role":"developer","content":"appended"}),
        ];
        let other = vec![json!({"role":"user","content":"other task"})];
        let body = client.request_body("system", &stable);
        let same_task = client.request_body("system", &same_prefix);
        let other_task = client.request_body("system", &other);

        assert_eq!(body["prompt_cache_options"]["mode"], "implicit");
        assert_eq!(body["prompt_cache_key"], same_task["prompt_cache_key"]);
        assert_eq!(body["prompt_cache_key"], other_task["prompt_cache_key"]);
    }
}
