use std::collections::BTreeMap;

use serde_json::{Value, json};

/// JSON observation has the same bound as SSE, independently of byte relay.
#[derive(Default)]
pub(super) struct JsonObserver {
    bytes: Vec<u8>,
    invalid: bool,
}

impl JsonObserver {
    pub fn feed(&mut self, chunk: &[u8]) {
        if self.invalid {
            return;
        }
        if self.bytes.len() + chunk.len() > 4 * 1024 * 1024 {
            self.bytes.clear();
            self.invalid = true;
        } else {
            self.bytes.extend_from_slice(chunk);
        }
    }

    pub fn observed(self) -> Option<Value> {
        if self.invalid {
            None
        } else {
            serde_json::from_slice(&self.bytes).ok()
        }
    }
}

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

#[derive(Default, PartialEq)]
enum ObservationState {
    #[default]
    Pending,
    Completed,
    Failed,
    Invalid,
}

/// Bounded observation only; the relay forwards the original bytes unchanged.
#[derive(Default)]
pub(super) struct Observer {
    pending: Vec<u8>,
    output_items: BTreeMap<u64, Value>,
    output_bytes: usize,
    state: ObservationState,
    pub observed: Option<Value>,
}

impl Observer {
    pub fn completed(&self) -> bool {
        self.state == ObservationState::Completed
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        if self.state == ObservationState::Invalid {
            return;
        }
        for byte in bytes {
            self.pending.push(*byte);
            if self.pending.len() > 4 * 1024 * 1024 {
                self.state = ObservationState::Invalid;
                self.observed = None;
                return;
            }
            if self.pending.ends_with(b"\n\n") || self.pending.ends_with(b"\r\n\r\n") {
                let event = std::mem::take(&mut self.pending);
                let Ok(text) = std::str::from_utf8(&event) else {
                    self.state = ObservationState::Invalid;
                    self.observed = None;
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
                        Some("response.output_item.done")
                            if self.state == ObservationState::Pending =>
                        {
                            if let Some(index) = value["output_index"].as_u64()
                                && value["item"].is_object()
                            {
                                self.output_bytes += data.len();
                                if self.output_bytes > 4 * 1024 * 1024 {
                                    self.state = ObservationState::Invalid;
                                    self.output_items.clear();
                                    self.observed = None;
                                    return;
                                }
                                self.output_items.insert(index, value["item"].clone());
                            }
                        }
                        Some("response.completed")
                            if self.state == ObservationState::Pending
                                && value["response"]["status"] == "completed" =>
                        {
                            self.state = ObservationState::Completed;
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
                            self.observed = Some(response);
                        }
                        Some("response.failed" | "response.incomplete" | "error")
                            if self.state == ObservationState::Pending =>
                        {
                            self.state = ObservationState::Failed;
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
    fn oversized_json_never_becomes_a_shortened_completed_response() {
        let mut observer = JsonObserver::default();
        observer.feed(b"{\"status\":\"completed\",\"output\":[{\"text\":\"");
        observer.feed(&vec![b'x'; 4 * 1024 * 1024 + 1]);
        observer.feed(b"\"}],\"usage\":{\"input_tokens\":10,\"output_tokens\":2}}");
        assert!(
            observer.observed().is_none(),
            "never commit shortened output after capture overflow"
        );
    }

    #[test]
    fn bounded_json_observation_preserves_fragmented_native_output() {
        let value = json!({"status":"completed","output":[{"type":"message","text":"é 🦀"}],"usage":{"input_tokens":10,"output_tokens":2}});
        let mut observer = JsonObserver::default();
        for chunk in value.to_string().as_bytes().chunks(3) {
            observer.feed(chunk);
        }
        assert_eq!(observer.observed(), Some(value));
    }

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
        assert!(!observer.completed());
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
        assert!(observer.completed());
        assert_eq!(
            observer.observed.as_ref().unwrap()["usage"]["output_tokens"],
            12
        );
    }

    #[test]
    fn first_terminal_disposition_wins_over_payload_status() {
        for event_type in [
            "response.completed",
            "response.failed",
            "response.incomplete",
            "error",
        ] {
            let mut observer = Observer::default();
            let response = json!({"id":"first", "status":"completed", "error":{"type":"safe", "message":"private prose"}});
            observer.feed(
                format!(
                    "data: {}\n\n",
                    json!({"type":event_type,"response":response})
                )
                .as_bytes(),
            );
            observer.feed(b"data: {\"type\":\"response.completed\",\"response\":{\"id\":\"later\",\"status\":\"completed\"}}\n\n");
            assert_eq!(observer.observed.as_ref().unwrap()["id"], "first");
            assert_eq!(observer.completed(), event_type == "response.completed");
            if event_type != "response.completed" {
                assert_eq!(
                    observer.observed.as_ref().unwrap()["error"],
                    json!({"type":"safe"})
                );
            }
        }
    }

    #[test]
    fn trailing_invalid_input_discards_completed_observation() {
        for oversized in [false, true] {
            let mut observer = Observer::default();
            observer.feed(b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n");
            assert!(observer.completed());
            if oversized {
                observer.feed(&vec![b'x'; 4 * 1024 * 1024 + 1]);
            } else {
                observer.feed(b"data: \xff\n\n");
            }
            observer.feed(b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n");
            assert!(!observer.completed());
            assert!(observer.observed.is_none());
        }
    }

    #[test]
    fn terminal_output_remains_authoritative() {
        let mut observer = Observer::default();
        observer.feed(b"data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"earlier\"}}\n\n");
        observer.feed(b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"id\":\"final\"}]}}\n\n");
        assert!(observer.completed());
        assert_eq!(observer.observed.unwrap()["output"][0]["id"], "final");
    }
}
