use std::collections::BTreeMap;

use serde_json::Value;

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
                                self.observed = Some(value["response"].clone());
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
