//! Local prompt text quantities. Native opaque payloads have no text-token size.
use serde_json::Value;

fn native_payload(kind: Option<&str>) -> bool {
    matches!(
        kind,
        Some(
            "input_image"
                | "image"
                | "image_url"
                | "input_audio"
                | "audio"
                | "resource"
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
    if kind == Some("image_generation_call") {
        return serde_json::json!({"type":"image_generation_call"});
    }
    let encrypted = matches!(kind, Some("reasoning" | "compaction"));
    Value::Object(
        object
            .iter()
            .filter(|(key, _)| {
                (!encrypted || key.as_str() != "encrypted_content")
                    && (kind != Some("program") || key.as_str() != "fingerprint")
            })
            .map(|(key, value)| {
                let value = match (kind, key.as_str()) {
                    // Only typed arrays are native tool output; objects and
                    // strings remain quoted source data, including lookalikes.
                    (Some("function_call_output" | "custom_tool_call_output"), "output") => {
                        sanitized_array(value)
                    }
                    (Some("computer_call_output"), "output") => sanitized_item(value),
                    (Some("mcp_call" | "mcp_list_tools"), "error") => sanitized_item(value),
                    (Some("code_interpreter_call"), "outputs") => value.as_array().map_or_else(
                        || value.clone(),
                        |outputs| {
                            Value::Array(
                                outputs
                                    .iter()
                                    .map(|output| {
                                        if output["type"] != "logs" {
                                            serde_json::json!({"type":output["type"]})
                                        } else {
                                            output.clone()
                                        }
                                    })
                                    .collect(),
                            )
                        },
                    ),
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
        Some("image_generation_call") => return true,
        Some("program") if object.contains_key("fingerprint") => return true,
        Some("code_interpreter_call") => {
            return object
                .get("outputs")
                .and_then(Value::as_array)
                .is_some_and(|outputs| outputs.iter().any(|output| output["type"] != "logs"));
        }
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
        Some("mcp_call" | "mcp_list_tools") => {
            return object.get("error").is_some_and(item_has_opaque);
        }
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

/// Local semantic units for material retained from a response. Provider output
/// billing, native replay state and encoded media are different quantities.
/// This projection shares the prompt text tokenizer and opaque-payload owner.
pub(crate) fn retained_output_tokens(output: &Value) -> Option<u64> {
    let mut text = Vec::<Value>::new();
    fn push(text: &mut Vec<Value>, value: &Value) -> Option<()> {
        text.push(Value::String(value.as_str()?.to_owned()));
        Some(())
    }
    for item in output.as_array()? {
        match item["type"].as_str()? {
            "message" => {
                if item["content"].is_string() {
                    push(&mut text, &item["content"])?;
                } else {
                    for part in item["content"].as_array()? {
                        push(
                            &mut text,
                            match part["type"].as_str()? {
                                "output_text" | "input_text" => &part["text"],
                                "refusal" => &part["refusal"],
                                _ => return None,
                            },
                        )?;
                    }
                }
            }
            "reasoning" => {
                if let Some(summary) = item.get("summary") {
                    for part in summary.as_array()? {
                        push(&mut text, &part["text"])?;
                    }
                }
            }
            "function_call" => {
                push(&mut text, &item["name"])?;
                push(&mut text, &item["arguments"])?;
            }
            "custom_tool_call" => {
                push(&mut text, &item["name"])?;
                push(&mut text, &item["input"])?;
            }
            _ => {
                let projection = native_text_projection(item)?;
                // Keep each structured native record whole, rather than
                // flattening its arrays, duplicating its text or counting IDs.
                text.push(projection);
            }
        }
    }
    Some(if text.is_empty() {
        0
    } else {
        local_text_tokens(&Value::Array(text))
    })
}

fn native_text_projection(item: &Value) -> Option<Value> {
    if !matches!(
        item["type"].as_str(),
        Some(
            "web_search_call"
                | "file_search_call"
                | "computer_call"
                | "code_interpreter_call"
                | "local_shell_call"
                | "local_shell_call_output"
                | "shell_call"
                | "shell_call_output"
                | "apply_patch_call"
                | "apply_patch_call_output"
                | "mcp_call"
                | "mcp_list_tools"
                | "mcp_approval_request"
                | "mcp_approval_response"
                | "tool_search_call"
                | "tool_search_output"
                | "program_output"
                | "function_call_output"
                | "custom_tool_call_output"
        )
    ) || prompt_has_opaque(item)
    {
        return None;
    }
    // Estimate the structured native record through the same text owner as
    // prompt input. Protocol validation and execution belong to their callers;
    // this is a local context estimate, not a second per-tool schema decoder.
    let mut projection = item.as_object()?.clone();
    for key in ["id", "call_id", "status", "container_id"] {
        projection.remove(key);
    }
    Some(Value::Object(projection))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_opaque_payload_growth_never_changes_visible_local_units() {
        for (item, pointer) in [
            (
                json!({"type":"mcp_call","name":"lookup","arguments":"{}","error":{"type":"mcp_tool_execution_error","content":[{"type":"image","data":"small","mimeType":"image/png"}]}}),
                "/error/content/0/data",
            ),
            (
                json!({"type":"image_generation_call","result":"small"}),
                "/result",
            ),
            (
                json!({"type":"program","code":"text(2 + 2)","fingerprint":"small"}),
                "/fingerprint",
            ),
            (
                json!({"type":"code_interpreter_call","code":"plot()","outputs":[{"type":"image","url":"small"}]}),
                "/outputs/0/url",
            ),
        ] {
            let mut larger = item.clone();
            *larger.pointer_mut(pointer).unwrap() = json!("encoded payload".repeat(10000));
            for value in [&item, &larger] {
                assert!(
                    prompt_has_opaque(value),
                    "opaque native item {}",
                    value["type"]
                );
                assert!(retained_output_tokens(&json!([value])).is_none());
            }
            assert_eq!(
                local_text_tokens(&item),
                local_text_tokens(&larger),
                "encoded bytes are not visible token quantities for {}",
                item["type"]
            );
        }
    }

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
