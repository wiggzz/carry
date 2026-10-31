use carry::anthropic::{Observer, Status, decode_message, decode_request};
use serde_json::{Value, json};

fn fixture() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/anthropic/events.json")).unwrap()
}
fn wire(events: &[Value]) -> Vec<u8> {
    events
        .iter()
        .map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e))
        .collect::<String>()
        .into_bytes()
}
fn observe(bytes: &[u8]) -> carry::anthropic::Observation {
    let mut observer = Observer::default();
    observer.feed(bytes);
    observer.finish(false)
}

#[test]
fn native_envelopes_preserve_order_signed_blocks_and_extensions() {
    let request: Value =
        serde_json::from_str(include_str!("fixtures/anthropic/request.json")).unwrap();
    assert_eq!(
        decode_request(&serde_json::to_vec(&request).unwrap()).unwrap(),
        request
    );
    let message = json!({"id":"msg_fixture", "type":"message", "role":"assistant", "model":"native-model", "content":request["messages"][0]["content"], "stop_reason":"tool_use", "usage":{"input_tokens":7,"output_tokens":9,"cache_creation_input_tokens":3}, "future":{"opaque":true}});
    assert_eq!(
        decode_message(&serde_json::to_vec(&message).unwrap()).unwrap(),
        message
    );
}

#[test]
fn stream_assembles_native_blocks_at_every_byte_boundary() {
    let events = fixture();
    let bytes = wire(&events);
    for split in 0..=bytes.len() {
        let mut observer = Observer::default();
        observer.feed(&bytes[..split]);
        observer.feed(&bytes[split..]);
        let result = observer.finish(false);
        assert_eq!(result.status, Status::Complete, "split {split}");
        assert_eq!(result.events, events);
        let message = result.message.unwrap();
        assert_eq!(
            message["content"][0],
            json!({"type":"thinking","thinking":"opaque pensée","signature":"sig+/==", "future":true})
        );
        assert_eq!(
            message["content"][1],
            json!({"type":"redacted_thinking","data":"opaque+/=="})
        );
        assert_eq!(message["content"][2]["input"], json!({"city":"東京"}));
        assert_eq!(message["content"][3]["text"], "done");
        assert_eq!(message["usage"]["output_tokens"], 9);
        assert_eq!(message["usage"]["cache_read_input_tokens"], 13);
        assert_eq!(message["usage"]["future"], json!({"native":2}));
        assert_eq!(message["stop_reason"], "tool_use");
    }
}

#[test]
fn incomplete_cancelled_and_error_streams_never_complete() {
    let events = fixture();
    for end in 0..events.len() {
        assert_eq!(observe(&wire(&events[..end])).status, Status::Incomplete);
    }
    let mut observer = Observer::default();
    observer.feed(&wire(&events));
    assert_eq!(observer.finish(true).status, Status::Cancelled);
    let mut failed = events[..3].to_vec();
    failed.push(json!({"type":"error","error":{"type":"overloaded_error","message":"private provider prose","future":1}}));
    let result = observe(&wire(&failed));
    assert_eq!(result.status, Status::Failed);
    assert_eq!(result.events, failed);
}

#[test]
fn bad_lifecycle_and_malformed_data_never_complete() {
    let events = fixture();
    let mut cases = vec![vec![json!({"type":"message_stop"})]];
    let mut duplicate = events.clone();
    duplicate.insert(1, events[0].clone());
    cases.push(duplicate);
    let mut wrong_index = events.clone();
    wrong_index[2]["index"] = json!(99);
    cases.push(wrong_index);
    let mut unclosed = events.clone();
    unclosed.remove(4);
    cases.push(unclosed);
    let mut late = events.clone();
    late.push(events[1].clone());
    cases.push(late);
    let mut wrong_delta = events.clone();
    wrong_delta[2]["delta"] = json!({"type":"text_delta","text":"bad"});
    cases.push(wrong_delta);
    let mut bad_tool = events.clone();
    bad_tool[9]["delta"]["partial_json"] = json!("[]");
    cases.push(bad_tool);
    for case in cases {
        assert_eq!(observe(&wire(&case)).status, Status::Invalid, "{case:?}");
    }
    for bytes in [
        b"data: not json\n\n".as_slice(),
        b"event: ping\ndata: {\"type\":\"message_stop\"}\n\n",
        b"data: \xff\n\n",
    ] {
        assert_eq!(observe(bytes).status, Status::Invalid);
    }
    let mut partial = wire(&events);
    partial.extend_from_slice(b"data: {");
    assert_eq!(observe(&partial).status, Status::Incomplete);
}

#[test]
fn unknown_events_and_deltas_fail_closed_without_rewriting_them() {
    for event in [
        json!({"type":"future_event","opaque":true}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"future_delta","opaque":true}}),
    ] {
        let events = vec![fixture()[0].clone(), fixture()[1].clone(), event.clone()];
        let result = observe(&wire(&events));
        assert_eq!(result.status, Status::Unsupported);
        assert_eq!(result.events.last(), Some(&event));
    }
}

#[test]
fn framing_handles_cr_lf_comments_multiline_data_and_single_byte_chunks() {
    let bytes = String::from_utf8(wire(&fixture()))
        .unwrap()
        .replace("data: {", ": comment\ndata: {\ndata: ");
    for newline in ["\n", "\r\n", "\r"] {
        let bytes = bytes.replace('\n', newline);
        let mut observer = Observer::default();
        for byte in bytes.as_bytes() {
            observer.feed(&[*byte]);
        }
        assert_eq!(observer.finish(false).status, Status::Complete);
    }
}

#[test]
fn observation_is_bounded_including_many_small_events() {
    let mut observer = Observer::default();
    observer.feed(&vec![b'x'; 4 * 1024 * 1024 + 1]);
    assert_eq!(observer.finish(false).status, Status::LimitExceeded);
    let mut observer = Observer::default();
    for _ in 0..150_000 {
        observer.feed(b"data: {\"type\":\"ping\"}\n\n: padding\n\n");
    }
    assert_eq!(observer.finish(false).status, Status::LimitExceeded);
}

#[test]
fn malformed_envelopes_are_rejected() {
    for value in [
        json!({}),
        json!({"model":"m","max_tokens":1,"messages":[{"role":"system","content":"x"}]}),
        json!({"model":"m","max_tokens":1,"messages":[{"role":"user","content":false}]}),
    ] {
        assert!(decode_request(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    assert!(decode_message(br#"{"type":"error","error":{"type":"overloaded_error"}}"#).is_err());
}

#[test]
fn terminal_metadata_and_signed_block_shapes_are_validated() {
    let mut cases = Vec::new();
    let mut no_stop = fixture();
    for e in &mut no_stop {
        if e["type"] == "message_delta" {
            e["delta"] = json!({});
        }
    }
    cases.push(no_stop);
    let mut bad_stop = fixture();
    bad_stop[16]["delta"]["stop_reason"] = json!(false);
    cases.push(bad_stop);
    let mut no_signature = fixture();
    no_signature.remove(3);
    cases.push(no_signature);
    let mut bad_redacted = fixture();
    bad_redacted[5]["content_block"]["data"] = json!({"not":"opaque text"});
    cases.push(bad_redacted);
    for case in cases {
        assert_eq!(observe(&wire(&case)).status, Status::Invalid);
    }
}

#[test]
fn empty_tool_deltas_preserve_an_empty_input_object() {
    let events = vec![
        fixture()[0].clone(),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_empty","name":"no_args","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":""}}),
        json!({"type":"content_block_stop","index":0}),
        fixture()[15].clone(),
        fixture()[17].clone(),
    ];
    let result = observe(&wire(&events));
    assert_eq!(result.status, Status::Complete);
    assert_eq!(result.message.unwrap()["content"][0]["input"], json!({}));
}

#[test]
fn signature_delta_is_an_opaque_replacement_as_in_native_sdk() {
    let mut events = fixture();
    events[1]["content_block"]["signature"] = json!("old-opaque-signature");
    let result = observe(&wire(&events));
    assert_eq!(result.status, Status::Complete);
    assert_eq!(
        result.message.unwrap()["content"][0]["signature"],
        "sig+/=="
    );
}

#[test]
fn native_error_envelope_retains_details_without_treating_it_as_a_message() {
    use carry::anthropic::{Response, decode_response};
    let error = json!({"type":"error", "error":{"type":"rate_limit_error","message":"provider prose","future":{"opaque":true}},"request_id":"req_fixture"});
    assert_eq!(
        decode_response(&serde_json::to_vec(&error).unwrap()).unwrap(),
        Response::Error(error)
    );
    let message = observe(&wire(&fixture())).message.unwrap();
    assert_eq!(
        decode_response(&serde_json::to_vec(&message).unwrap()).unwrap(),
        Response::Message(message)
    );
}
