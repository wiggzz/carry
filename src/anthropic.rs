//! Native Messages envelopes and bounded observation, independent of HTTP relay.
//!
//! See `docs/anthropic-protocol.md` for the integration boundary and limitations.
//! Values and events contain private provider content; do not log them directly.
use serde_json::Value;

const MAX_BYTES: usize = 4 * 1024 * 1024;

/// Native non-streaming payload. HTTP status remains authoritative and caller-owned.
#[derive(Debug, PartialEq)]
pub enum Response {
    Message(Value),
    Error(Value),
}

/// Preserve either a message or a provider error, including extension fields.
/// Error prose is not logging-safe. This does not interpret HTTP status/retries.
pub fn decode_response(bytes: &[u8]) -> Result<Response, &'static str> {
    let value = decode(bytes)?;
    if value["type"] == "error" && value["error"].is_object() && value["error"]["type"].is_string()
    {
        Ok(Response::Error(value))
    } else {
        validate_message(value).map(Response::Message)
    }
}

/// Decode the envelope without normalizing content or dropping extension fields.
/// This is not feature admission, tool-dependency validation or a provider schema.
pub fn decode_request(bytes: &[u8]) -> Result<Value, &'static str> {
    let value = decode(bytes)?;
    if !value["model"].is_string()
        || value["max_tokens"].as_u64().is_none()
        || !value["messages"].as_array().is_some_and(|messages| {
            messages.iter().all(|m| {
                matches!(m["role"].as_str(), Some("user" | "assistant"))
                    && (m["content"].is_string() || blocks(&m["content"]))
            })
        })
    {
        return Err("invalid Messages request envelope");
    }
    Ok(value)
}

/// Decode a non-streaming message without converting signed or multimodal blocks.
/// HTTP status, errors, supported features and delivery remain caller-owned.
pub fn decode_message(bytes: &[u8]) -> Result<Value, &'static str> {
    validate_message(decode(bytes)?)
}

fn validate_message(value: Value) -> Result<Value, &'static str> {
    if !message_envelope(&value)
        || !terminal_metadata(&value)
        || !value["content"]
            .as_array()
            .is_some_and(|blocks| blocks.iter().all(closed_block))
    {
        return Err("invalid Messages response envelope");
    }
    Ok(value)
}

fn decode(bytes: &[u8]) -> Result<Value, &'static str> {
    if bytes.len() > MAX_BYTES {
        return Err("Messages observation limit exceeded");
    }
    serde_json::from_slice(bytes).map_err(|_| "invalid Messages JSON")
}

fn blocks(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|blocks| blocks.iter().all(|block| block["type"].is_string()))
}

fn message_envelope(value: &Value) -> bool {
    value["type"] == "message"
        && value["role"] == "assistant"
        && value["id"].is_string()
        && value["model"].is_string()
        && blocks(&value["content"])
        && value.get("usage").is_none_or(Value::is_object)
}

fn valid_stop_metadata(message: &Value) -> bool {
    message.get("stop_reason").is_none_or(|reason| {
        reason.is_null() || reason.as_str().is_some_and(|reason| !reason.is_empty())
    }) && message
        .get("stop_sequence")
        .is_none_or(|s| s.is_null() || s.is_string())
}

fn terminal_metadata(message: &Value) -> bool {
    valid_stop_metadata(message) && message["stop_reason"].is_string()
}

fn closed_block(block: &Value) -> bool {
    match block["type"].as_str() {
        Some("thinking") => {
            block["thinking"].is_string()
                && block["signature"].as_str().is_some_and(|s| !s.is_empty())
        }
        Some("redacted_thinking") => block["data"].is_string(),
        Some("text") => block["text"].is_string(),
        Some("tool_use" | "server_tool_use") => {
            block["id"].is_string() && block["name"].is_string() && block["input"].is_object()
        }
        Some(_) => true, // complete opaque blocks remain unmodified
        None => false,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    Complete,
    Incomplete,
    Cancelled,
    Failed,
    Invalid,
    Unsupported,
    LimitExceeded,
}

/// Only `Complete` contains a fully observed message. Even that does not prove
/// HTTP success or downstream delivery. All other messages are partial evidence.
pub struct Observation {
    pub status: Status,
    pub message: Option<Value>,
    /// Original JSON events, including unknown fields and provider error prose.
    /// Never log this directly. Capture stops at the first failure/unsupported event.
    pub events: Vec<Value>,
}

#[derive(Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Start,
    Content,
    Delta,
    Stopped,
}

/// Bounded native SSE observer. Relay original bytes separately and unchanged.
#[derive(Default)]
pub struct Observer {
    bytes: usize,
    line: Vec<u8>,
    after_cr: bool,
    first_line: bool,
    data: String,
    event_name: Option<String>,
    phase: Phase,
    active: Option<usize>,
    tool_json: Option<String>,
    message: Option<Value>,
    events: Vec<Value>,
    failure: Option<Status>,
}

impl Observer {
    pub fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.failure.is_some() {
                return;
            }
            self.bytes += 1;
            if self.bytes > MAX_BYTES {
                self.failure = Some(Status::LimitExceeded);
                return;
            }
            if self.after_cr && byte == b'\n' {
                self.after_cr = false;
                continue;
            }
            self.after_cr = byte == b'\r';
            if byte == b'\r' || byte == b'\n' {
                if let Err(status) = self.line() {
                    self.failure = Some(status);
                }
            } else {
                self.line.push(byte);
            }
        }
    }

    /// Call at EOF. Pass true on transport error, cancellation or disconnect,
    /// including after message_stop; those are not successful delivery evidence.
    pub fn finish(self, cancelled: bool) -> Observation {
        let status = self.failure.unwrap_or_else(|| {
            if cancelled {
                Status::Cancelled
            } else if self.phase == Phase::Stopped
                && self.line.is_empty()
                && self.data.is_empty()
                && self.event_name.is_none()
            {
                Status::Complete
            } else {
                Status::Incomplete
            }
        });
        Observation {
            status,
            message: self.message,
            events: self.events,
        }
    }

    fn line(&mut self) -> Result<(), Status> {
        let bytes = std::mem::take(&mut self.line);
        let line = std::str::from_utf8(&bytes).map_err(|_| Status::Invalid)?;
        let line = if !self.first_line {
            self.first_line = true;
            line.strip_prefix('\u{feff}').unwrap_or(line)
        } else {
            line
        };
        if line.is_empty() {
            let name = self.event_name.take();
            if self.data.is_empty() {
                return Ok(());
            }
            let data = std::mem::take(&mut self.data);
            let event: Value = serde_json::from_str(&data).map_err(|_| Status::Invalid)?;
            if name
                .as_deref()
                .is_some_and(|name| Some(name) != event["type"].as_str())
            {
                return Err(Status::Invalid);
            }
            let result = self.event(&event);
            self.events.push(event);
            return result;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => {
                self.data.push_str(value);
                self.data.push('\n');
            }
            "event" => self.event_name = Some(value.to_owned()),
            _ => {} // comments and SSE transport metadata do not change the message
        }
        Ok(())
    }

    fn event(&mut self, event: &Value) -> Result<(), Status> {
        let kind = event["type"].as_str().ok_or(Status::Invalid)?;
        if kind == "ping" {
            return Ok(());
        }
        if kind == "error" {
            return Err(if event["error"].is_object() {
                Status::Failed
            } else {
                Status::Invalid
            });
        }
        if self.phase == Phase::Stopped {
            return Err(Status::Invalid);
        }
        match kind {
            "message_start" => {
                let message = &event["message"];
                if self.phase != Phase::Start
                    || !message_envelope(message)
                    || message["content"].as_array().is_none_or(|c| !c.is_empty())
                    // Terminal evidence must originate in message_delta, never
                    // in the initial envelope. Missing fields remain compatible.
                    || !message["stop_reason"].is_null()
                    || !message["stop_sequence"].is_null()
                {
                    return Err(Status::Invalid);
                }
                self.message = Some(message.clone());
                self.phase = Phase::Content;
            }
            "content_block_start" => {
                if self.phase != Phase::Content || self.active.is_some() {
                    return Err(Status::Invalid);
                }
                let content = self.message.as_mut().ok_or(Status::Invalid)?["content"]
                    .as_array_mut()
                    .ok_or(Status::Invalid)?;
                if event["index"].as_u64() != Some(content.len() as u64)
                    || !event["content_block"]["type"].is_string()
                {
                    return Err(Status::Invalid);
                }
                self.active = Some(content.len());
                content.push(event["content_block"].clone());
            }
            "content_block_delta" => {
                let index = self.active_index(event)?;
                let block = &mut self.message.as_mut().ok_or(Status::Invalid)?["content"][index];
                let delta = &event["delta"];
                match delta["type"].as_str().ok_or(Status::Invalid)? {
                    "text_delta" => append(block, "text", "text", delta)?,
                    "thinking_delta" => append(block, "thinking", "thinking", delta)?,
                    "signature_delta" => {
                        if block["type"] != "thinking" || !delta["signature"].is_string() {
                            return Err(Status::Invalid);
                        }
                        // Native SDK semantics: a signature is an opaque replacement,
                        // not a text fragment to concatenate or re-encode.
                        block["signature"] = delta["signature"].clone();
                    }
                    "input_json_delta" => {
                        if !matches!(block["type"].as_str(), Some("tool_use" | "server_tool_use"))
                            || block["input"].as_object().is_none_or(|o| !o.is_empty())
                        {
                            return Err(Status::Invalid);
                        }
                        self.tool_json
                            .get_or_insert_default()
                            .push_str(delta["partial_json"].as_str().ok_or(Status::Invalid)?);
                    }
                    _ => return Err(Status::Unsupported),
                }
            }
            "content_block_stop" => {
                let index = self.active_index(event)?;
                if let Some(json) = self.tool_json.take().filter(|json| !json.is_empty()) {
                    let input: Value = serde_json::from_str(&json).map_err(|_| Status::Invalid)?;
                    if !input.is_object() {
                        return Err(Status::Invalid);
                    }
                    self.message.as_mut().ok_or(Status::Invalid)?["content"][index]["input"] =
                        input;
                }
                if !closed_block(&self.message.as_ref().ok_or(Status::Invalid)?["content"][index]) {
                    return Err(Status::Invalid);
                }
                self.active = None;
            }
            "message_delta" => {
                if !matches!(self.phase, Phase::Content | Phase::Delta) || self.active.is_some() {
                    return Err(Status::Invalid);
                }
                let delta = event["delta"].as_object().ok_or(Status::Invalid)?;
                // Only documented top-level updates are assembled. Unknown updates
                // remain in events and must not silently produce a complete message.
                if delta
                    .keys()
                    .any(|key| !matches!(key.as_str(), "stop_reason" | "stop_sequence"))
                {
                    return Err(Status::Unsupported);
                }
                // Validate each update before merging it: a later delta must
                // not turn malformed earlier metadata into a successful stream.
                // Empty/usage-only and nullable updates are still valid here.
                if !valid_stop_metadata(&event["delta"]) {
                    return Err(Status::Invalid);
                }
                let message = self.message.as_mut().ok_or(Status::Invalid)?;
                for (key, value) in delta {
                    message[key] = value.clone();
                }
                if let Some(usage) = event.get("usage") {
                    let usage = usage.as_object().ok_or(Status::Invalid)?;
                    if message.get("usage").is_none() {
                        message["usage"] = serde_json::json!({});
                    }
                    let target = message["usage"].as_object_mut().ok_or(Status::Invalid)?;
                    // These are cumulative native facts, not per-event increments.
                    target.extend(usage.clone());
                }
                self.phase = Phase::Delta;
            }
            "message_stop" => {
                if self.phase != Phase::Delta || self.active.is_some() {
                    return Err(Status::Invalid);
                }
                if !terminal_metadata(self.message.as_ref().ok_or(Status::Invalid)?) {
                    return Err(Status::Invalid);
                }
                self.phase = Phase::Stopped;
            }
            _ => return Err(Status::Unsupported),
        }
        Ok(())
    }

    fn active_index(&self, event: &Value) -> Result<usize, Status> {
        let index = self.active.ok_or(Status::Invalid)?;
        if self.phase != Phase::Content || event["index"].as_u64() != Some(index as u64) {
            return Err(Status::Invalid);
        }
        Ok(index)
    }
}

fn append(block: &mut Value, kind: &str, field: &str, delta: &Value) -> Result<(), Status> {
    if block["type"] != kind {
        return Err(Status::Invalid);
    }
    let suffix = delta[field].as_str().ok_or(Status::Invalid)?;
    let target = block.get_mut(field).ok_or(Status::Invalid)?;
    if let Value::String(text) = target {
        text.push_str(suffix);
        Ok(())
    } else {
        Err(Status::Invalid)
    }
}
