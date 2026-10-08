use serde_json::Value;

/// Bounded observation only; the relay forwards the original bytes unchanged.
#[derive(Default)]
pub(super) struct Observer {
    pending: Vec<u8>,
    terminal: bool,
    invalid: bool,
    pub completed: Option<Value>,
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
                let data = text.lines().filter_map(|line| line.strip_prefix("data:").map(|s| s.strip_prefix(' ').unwrap_or(s))).collect::<Vec<_>>().join("\n");
                if let Ok(value) = serde_json::from_str::<Value>(&data) {
                    match value["type"].as_str() {
                        Some("response.completed") if !self.terminal && value["response"]["status"] == "completed" => {
                            self.terminal = true;
                            self.completed = Some(value["response"].clone());
                        }
                        Some("response.failed" | "response.incomplete" | "error") if !self.terminal => self.terminal = true,
                        _ => {}
                    }
                }
            }
        }
    }
}
