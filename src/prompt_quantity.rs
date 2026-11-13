//! Local prompt text quantities. Native opaque payloads have no text-token size.
use serde_json::Value;

fn native_payload(kind: Option<&str>) -> bool {
    matches!(
        kind,
        Some(
            "input_image"
                | "image_url"
                | "input_audio"
                | "input_video"
                | "input_file"
                | "item_reference"
                | "computer_screenshot"
        )
    )
}

fn sanitized_array(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(sanitized_item).collect()),
        _ => value.clone(),
    }
}

fn sanitized_item(value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    let kind = object.get("type").and_then(Value::as_str);
    if native_payload(kind) {
        // Keep a typed placeholder, never encoded bytes, URLs, or file IDs.
        return serde_json::json!({"type":object["type"]});
    }
    let encrypted = matches!(kind, Some("reasoning" | "compaction"));
    Value::Object(
        object
            .iter()
            .filter(|(key, _)| !encrypted || key.as_str() != "encrypted_content")
            .map(|(key, value)| {
                let value = match (kind, key.as_str()) {
                    // Only typed arrays are native tool output; objects and
                    // strings remain quoted source data, including lookalikes.
                    (Some("function_call_output" | "custom_tool_call_output"), "output") => {
                        sanitized_array(value)
                    }
                    (Some("computer_call_output"), "output") => sanitized_item(value),
                    (
                        Some(
                            "function_call_output"
                            | "custom_tool_call_output"
                            | "computer_call_output",
                        ),
                        _,
                    ) => value.clone(),
                    (_, "content") => sanitized_array(value),
                    _ => value.clone(),
                };
                (key.clone(), value)
            })
            .collect(),
    )
}

fn sanitized(value: &Value) -> Value {
    match value {
        Value::Array(_) => sanitized_array(value),
        Value::Object(object) if object.contains_key("input") => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        if key == "input" {
                            sanitized_array(value)
                        } else {
                            value.clone()
                        },
                    )
                })
                .collect(),
        ),
        _ => sanitized_item(value),
    }
}

pub(crate) fn local_text_tokens(value: &Value) -> u64 {
    let rendered = sanitized(value).to_string();
    #[cfg(test)]
    TOKEN_WORK.with(|work| {
        let (calls, bytes) = work.get();
        work.set((calls + 1, bytes + rendered.len()));
    });
    tiktoken_rs::o200k_base_singleton()
        .encode_ordinary(&rendered)
        .len() as u64
}

fn item_has_opaque(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let kind = object.get("type").and_then(Value::as_str);
    if native_payload(kind) {
        return true;
    }
    match kind {
        Some("reasoning" | "compaction") if object.contains_key("encrypted_content") => {
            return true;
        }
        Some("function_call_output" | "custom_tool_call_output") => {
            return object
                .get("output")
                .and_then(Value::as_array)
                .is_some_and(|parts| parts.iter().any(item_has_opaque));
        }
        Some("computer_call_output") => return object.get("output").is_some_and(item_has_opaque),
        _ => {}
    }
    object
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|parts| parts.iter().any(item_has_opaque))
}

pub(crate) fn prompt_has_opaque(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(item_has_opaque),
        Value::Object(object) if object.contains_key("input") => object
            .get("input")
            .and_then(Value::as_array)
            .is_some_and(|items| items.iter().any(item_has_opaque)),
        _ => item_has_opaque(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_envelopes_and_quoted_data_keep_their_exact_text_projection() {
        let check = |body: Value, expected: Value, opaque: bool| {
            let frozen = body.clone();
            let tokens = tiktoken_rs::o200k_base_singleton()
                .encode_ordinary(&expected.to_string())
                .len() as u64;
            assert_eq!(local_text_tokens(&body), tokens, "{body}");
            assert_eq!(prompt_has_opaque(&body), opaque, "{body}");
            assert_eq!(body, frozen);
        };
        for kind in [
            "input_image",
            "image_url",
            "input_audio",
            "input_video",
            "input_file",
            "item_reference",
            "computer_screenshot",
            "reasoning",
            "compaction",
        ] {
            let native = json!({"type":kind,"encrypted_content":"private payload",
                "image_url":"private URL","file_id":"private ID","summary":[]});
            let visible = if matches!(kind, "reasoning" | "compaction") {
                json!({"type":kind,"image_url":"private URL","file_id":"private ID","summary":[]})
            } else {
                json!({"type":kind})
            };
            for (body, expected) in [
                (native.clone(), visible.clone()),
                (json!([native]), json!([visible])),
                (
                    json!({"input":[native],"metadata":{"keep":"ordinary data"}}),
                    json!({"input":[visible],"metadata":{"keep":"ordinary data"}}),
                ),
                (
                    json!({"role":"user","content":[native]}),
                    json!({"role":"user","content":[visible]}),
                ),
                (
                    json!({"type":"computer_call_output","output":native}),
                    json!({"type":"computer_call_output","output":visible}),
                ),
            ] {
                check(body, expected, true);
            }
            for tool in ["function_call_output", "custom_tool_call_output"] {
                check(
                    json!({"type":tool,"output":[native]}),
                    json!({"type":tool,"output":[visible]}),
                    true,
                );
                let quoted = json!({"type":tool,"output":native,"content":[native]});
                check(quoted.clone(), quoted, false);
            }
            let malformed = json!({"input":native,"content":[native]});
            check(malformed.clone(), malformed, false);
        }
        for body in [
            json!(null),
            json!(7),
            json!("quoted input_image"),
            json!({"type":"unknown","encrypted_content":"quoted data"}),
            json!({"type":"computer_call_output","output":[{"type":"input_image"}]}),
            json!({"role":"user","content":{"type":"input_image"}}),
        ] {
            check(body.clone(), body, false);
        }
    }

    #[test]
    fn native_payload_sizes_do_not_become_text_tokens() {
        for payload in ["abcd".to_owned(), "abcd".repeat(20000)] {
            let body = json!({"input":[
                {"type":"reasoning","summary":[],"encrypted_content":payload},
                {"role":"user","content":[{"type":"input_image","image_url":format!("data:image/png;base64,{payload}")}]}
            ]});
            assert!(
                local_text_tokens(&body) < 100,
                "native payload bytes are not model text"
            );
            assert!(prompt_has_opaque(&body));
        }
    }

    #[test]
    fn quoted_native_shapes_remain_text() {
        let small = json!({"input":[{"type":"function_call_output","call_id":"c", "output":{"type":"input_image","image_url":"a"}}]});
        let mut large = small.clone();
        large["input"][0]["output"]["image_url"] = json!("ab ".repeat(500));
        assert!(local_text_tokens(&large) > local_text_tokens(&small) + 400);
        assert!(
            !prompt_has_opaque(&large),
            "tool-returned JSON is source text"
        );
        let quoted = json!({"input":[{"role":"user","content":"{\"type\":\"reasoning\",\"encrypted_content\":\"quoted\"}"}]});
        assert!(!prompt_has_opaque(&quoted));
    }

    #[test]
    fn native_payload_identity_is_not_modified() {
        let value = json!({"input":[{"type":"reasoning","encrypted_content":"native bytes", "summary":[]}]});
        let before = value.clone();
        local_text_tokens(&value);
        prompt_has_opaque(&value);
        assert_eq!(value, before);
    }
}

#[cfg(test)]
mod native_tool_output_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn typed_tool_output_images_are_native_not_json_text() {
        for kind in ["function_call_output", "custom_tool_call_output"] {
            let body = json!({"input":[{"type":kind,"call_id":"c","output":[
                {"type":"input_text","text":"Screenshot from the tool"},
                {"type":"input_image","image_url":format!("data:image/png;base64,{}","abcd".repeat(20000))}
            ]}]});
            assert!(
                prompt_has_opaque(&body),
                "typed output arrays carry native images"
            );
            assert!(
                local_text_tokens(&body) < 100,
                "encoded screenshots are not model-visible text"
            );
        }
    }
}

#[cfg(test)]
mod computer_screenshot_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn computer_screenshot_output_is_native_not_base64_text() {
        let body = json!({"input":[{"type":"computer_call_output","call_id":"c","output":{
            "type":"computer_screenshot","image_url":format!("data:image/png;base64,{}","abcd".repeat(20000))
        }}]});
        assert!(prompt_has_opaque(&body));
        assert!(local_text_tokens(&body) < 100);
    }
}

#[cfg(test)]
thread_local! { static TOKEN_WORK: std::cell::Cell<(usize,usize)> = const {std::cell::Cell::new((0,0))}; }
#[cfg(test)]
pub(crate) fn reset_token_work() {
    TOKEN_WORK.with(|work| work.set((0, 0)));
}
#[cfg(test)]
pub(crate) fn token_work() -> (usize, usize) {
    TOKEN_WORK.with(std::cell::Cell::get)
}
#[cfg(test)]
pub(crate) fn reset_tokenized_bytes() {
    reset_token_work();
}
#[cfg(test)]
pub(crate) fn tokenized_bytes() -> usize {
    token_work().1
}
