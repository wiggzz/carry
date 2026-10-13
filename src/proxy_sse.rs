use std::collections::BTreeMap;

use serde_json::{Value, json};

/// Persist identifiers only; provider prose can contain private request data.
pub(super) fn safe_error(error: &Value) -> Value {
    let mut safe = serde_json::Map::new();
    for field in ["type", "code", "param"] {
        if let Some(token) = error[field].as_str()
            && !token.is_empty()
            && token.len() <= 64
            && token
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-[]".contains(&c))
        {
            safe.insert(field.into(), json!(token));
        }
    }
    Value::Object(safe)
}

/// Bounded observation only; the relay forwards the original bytes unchanged.
#[derive(Default)]
pub(super) struct Observer {
    pending: Vec<u8>,
    output_items: BTreeMap<u64, Value>,
    output_bytes: usize,
    terminal: bool,
    invalid: bool,
    pub completed: Option<Value>,
    pub observed: Option<Value>,
}

impl Observer {
    pub fn feed(&mut self, bytes: &[u8]) {
        if self.invalid {
            return;
        }
        for byte in bytes {
            self.pending.push(*byte);
            if self.pending.len() > 4 * 1024 * 1024 {
                self.invalid = true;
                self.completed = None;
                return;
            }
            if self.pending.ends_with(b"\n\n") || self.pending.ends_with(b"\r\n\r\n") {
                let event = std::mem::take(&mut self.pending);
                let Ok(text) = std::str::from_utf8(&event) else {
                    self.invalid = true;
                    self.completed = None;
                    return;
                };
                let data = text
                    .lines()
                    .filter_map(|line| {
                        line.strip_prefix("data:")
                            .map(|s| s.strip_prefix(' ').unwrap_or(s))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if let Ok(value) = serde_json::from_str::<Value>(&data) {
                    match value["type"].as_str() {
                        Some("response.output_item.done") if !self.terminal => {
                            if let Some(index) = value["output_index"].as_u64()
                                && value["item"].is_object()
                            {
                                self.output_bytes += data.len();
                                if self.output_bytes > 4 * 1024 * 1024 {
                                    self.invalid = true;
                                    self.output_items.clear();
                                    self.completed = None;
                                    self.observed = None;
                                    return;
                                }
                                self.output_items.insert(index, value["item"].clone());
                            }
                        }
                        Some("response.completed")
                            if !self.terminal && value["response"]["status"] == "completed" =>
                        {
                            self.terminal = true;
                            let mut response = value["response"].clone();
                            if response["output"].is_null()
                                || response["output"].as_array().is_some_and(Vec::is_empty)
                            {
                                response["output"] = Value::Array(
                                    std::mem::take(&mut self.output_items)
                                        .into_values()
                                        .collect(),
                                );
                            }
                            self.completed = Some(response.clone());
                            self.observed = Some(response);
                        }
                        Some("response.failed" | "response.incomplete" | "error")
                            if !self.terminal =>
                        {
                            self.terminal = true;
                            if value["response"].is_object() {
                                let mut response = value["response"].clone();
                                response["error"] = safe_error(&response["error"]);
                                self.observed = Some(response);
                            } else {
                                let error = if value["error"].is_object() {
                                    &value["error"]
                                } else {
                                    &value
                                };
                                self.observed =
                                    Some(json!({"status":"failed","error":safe_error(error)}));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stream_error_retains_only_safe_identifiers() {
        let mut observer = Observer::default();
        observer.feed(b"data: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"code\":\"bad pair\",\"param\":\"input[2]\",\"message\":\"private provider prose\"}}\n\n");
        assert_eq!(
            observer.observed,
            Some(
                json!({"status":"failed","error":{"type":"invalid_request_error","param":"input[2]"}})
            )
        );
        assert!(observer.completed.is_none());
    }

    #[test]
    fn completed_items_supply_output_when_terminal_output_is_empty() {
        let item = json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"{\"protected\":[],\"removable\":[],\"memories\":[]}"}]});
        let events = [
            json!({"type":"response.output_item.done","output_index":0,"item":item}),
            json!({"type":"response.completed","response":{"status":"completed","output":[],"usage":{"output_tokens":12}}}),
        ];
        let mut observer = Observer::default();
        for event in events {
            let bytes = format!("data: {event}\r\n\r\n");
            for chunk in bytes.as_bytes().chunks(7) {
                observer.feed(chunk);
            }
        }
        assert_eq!(observer.observed.as_ref().unwrap()["output"], json!([item]));
        assert_eq!(
            observer.completed.as_ref().unwrap()["usage"]["output_tokens"],
            12
        );
    }

    #[test]
    fn terminal_output_remains_authoritative() {
        let mut observer = Observer::default();
        observer.feed(b"data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"earlier\"}}\n\n");
        observer.feed(b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"id\":\"final\"}]}}\n\n");
        assert_eq!(observer.completed.unwrap()["output"][0]["id"], "final");
    }
}
