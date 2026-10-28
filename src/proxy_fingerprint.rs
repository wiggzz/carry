//! Bounded, content-free identities of reqwest's final serialized reviewer body.
//! No provider comparison, payload persistence, header inspection, or cache claims.
use super::*;

const SCAN_BYTES: usize = 2 * 1024 * 1024;
const HASH_BYTES: usize = 1024 * 1024;
const ITEM_BYTES: usize = 256 * 1024;
const ITEMS: usize = 48;

// Process-private, random key: restart deliberately makes prior comparison
// unavailable. Nothing secret is written alongside the content-free journal.
fn key() -> Option<&'static [u8; 32]> {
    static KEY: std::sync::OnceLock<Option<[u8; 32]>> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).ok().map(|()| bytes)
    })
    .as_ref()
}

// RFC 2104 HMAC-SHA256 with a fixed 32-byte random key, domain-separated.
fn fingerprint(key: &[u8; 32], domain: &[u8], parts: &[&[u8]]) -> String {
    let mut inner_pad = [0x36; 64];
    let mut outer_pad = [0x5c; 64];
    for i in 0..32 {
        inner_pad[i] ^= key[i];
        outer_pad[i] ^= key[i];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(b"carry-review-wire-v1\0");
    inner.update(domain);
    inner.update([0]);
    for part in parts {
        inner.update(part);
    }
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner.finalize());
    format!("{:x}", outer.finalize())
}

// Scan trusted serde_json output without reparsing/copying payloads. Runtime is
// bounded by SCAN_BYTES; strings (including escaped delimiters) remain opaque.
fn value_end(bytes: &[u8], start: usize) -> Option<usize> {
    let first = *bytes.get(start)?;
    if first == b'"' {
        let mut i = start + 1;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 2,
                b'"' => return Some(i + 1),
                _ => i += 1,
            }
        }
        return None;
    }
    if !matches!(first, b'[' | b'{') {
        return Some(
            start
                + bytes[start..]
                    .iter()
                    .position(|b| matches!(b, b',' | b']' | b'}'))
                    .unwrap_or(bytes.len() - start),
        );
    }
    let mut depth = 0usize;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i = value_end(bytes, i)?;
                continue;
            }
            b'[' | b'{' => depth += 1,
            b']' | b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => (),
        }
        i += 1;
    }
    None
}

fn input_span(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut i = 1;
    while i < bytes.len() && bytes[i] != b'}' {
        let end = value_end(bytes, i)?;
        if *bytes.get(end)? != b':' {
            return None;
        }
        let start = end + 1;
        let stop = value_end(bytes, start)?;
        if &bytes[i..end] == b"\"input\"" {
            return Some((start, stop));
        }
        i = stop + 1;
    }
    None
}

fn safe_enum<'a>(value: &'a Value, allowed: &[&str]) -> &'a str {
    value
        .as_str()
        .filter(|s| allowed.contains(s))
        .unwrap_or("other")
}

fn summary(
    bytes: &[u8],
    body: &Value,
    route: &str,
    prior: Option<&Value>,
    context: &ReviewWire<'_>,
    transport_attempt: usize,
    key: &[u8; 32],
) -> Value {
    let input = body["input"].as_array();
    let count = input.map_or(0, Vec::len);
    let scope = fingerprint(key, b"key-scope", &[]);
    let prior = prior.filter(|p| p["key_scope"] == scope && p["format_version"] == 1);
    let mut data = json!({"format_version":1,"algorithm":"hmac-sha256-v1","key_scope":scope,
        "key_lifetime":"process","wire_bytes":bytes.len(),"item_count":count,"items":[],
        "coverage_complete":false,"omitted_items":count,"prior_prefix_equal":null,
        "settings_equal":null,"model_equal":null,"cache_key_equal":null,"route_equal":null,
        "review_epoch":context.epoch,"transport_attempt":transport_attempt,"rewrite_reason":match context.reason {
            Some("control_observation_migration") => "control_observation_migration",
            Some("history_rebase") => "history_rebase",
            Some("primary_instructions_changed") => "primary_instructions_changed",
            Some("shared_projection_or_cleanup") => "shared_projection_or_cleanup",
            _ => "none",
        },"comparison_basis":if prior.is_some(){"prior_retained_transport"}else{"unavailable_restart_eviction_or_first"}});
    if bytes.len() > SCAN_BYTES {
        data["coverage_reason"] = json!("scan_byte_limit");
        return data;
    }
    let Some((start, end)) = input_span(bytes) else {
        data["coverage_reason"] = json!("non_array_or_unrecognized_wire");
        return data;
    };
    if bytes.get(start) != Some(&b'[') || bytes.get(end - 1) != Some(&b']') {
        return data;
    }
    let settings = fingerprint(key, b"settings", &[&bytes[..start], &bytes[end..]]);
    data["settings_fingerprint"] = json!(settings);
    for (name, value) in [
        ("model", body["model"].as_str().unwrap_or("")),
        ("cache_key", body["prompt_cache_key"].as_str().unwrap_or("")),
        ("route", route),
    ] {
        let digest = fingerprint(key, name.as_bytes(), &[value.as_bytes()]);
        data[format!("{name}_fingerprint")] = json!(digest);
        if let Some(p) = prior {
            data[format!("{name}_equal")] = json!(p[format!("{name}_fingerprint")] == digest);
        }
    }
    if let Some(p) = prior {
        data["settings_equal"] = json!(p["settings_fingerprint"] == settings);
    }
    let inner = &bytes[start + 1..end - 1];
    data["input_bytes"] = json!(inner.len());
    if inner.len() <= HASH_BYTES {
        data["input_fingerprint"] = json!(fingerprint(key, b"input-prefix", &[inner]));
    }
    let mut cursor = start + 1;
    let mut spent = 0;
    let mut items = Vec::new();
    let mut prior_end = (prior.and_then(|p| p["item_count"].as_u64()) == Some(0)).then_some(cursor);
    // Compare the actual latest eligible USER boundary, not mutable trailing
    // assistant-side proxy state. This is the implicit write candidate, not
    // evidence that the provider wrote or retained it. Item detail limits do
    // not restrict identity; byte/hash limits still fail closed to unknown.
    let stable_count = input
        .and_then(|items| items.iter().rposition(|item| item["role"] == "user"))
        .map_or(0, |index| index + 1);
    let mut stable_end = None;
    let mut prior_stable_end = None;
    data["prior_stable_prefix_equal"] = Value::Null;
    for (index, item) in input.into_iter().flatten().enumerate() {
        let Some(stop) = value_end(bytes, cursor) else {
            break;
        };
        if index + 1 == stable_count {
            stable_end = Some(stop);
        }
        if prior.and_then(|p| p["stable_prefix_items"].as_u64()) == Some((index + 1) as u64) {
            prior_stable_end = Some(stop);
        }
        if prior.and_then(|p| p["item_count"].as_u64()) == Some((index + 1) as u64) {
            prior_end = Some(stop);
        }
        if index >= ITEMS {
            cursor = stop + 1;
            continue;
        }
        let raw = &bytes[cursor..stop];
        let digest = if raw.len() <= ITEM_BYTES && spent + raw.len() <= HASH_BYTES {
            spent += raw.len();
            Some(fingerprint(key, b"item", &[raw]))
        } else {
            None
        };
        items.push(json!({"type":safe_enum(&item["type"], &["message","reasoning","function_call","function_call_output","custom_tool_call","custom_tool_call_output","item_reference"]),
            "role":safe_enum(&item["role"], &["system","developer","user","assistant","tool"]),"bytes":raw.len(),"fingerprint":digest}));
        cursor = stop + 1;
    }
    if let Some(stop) = stable_end.filter(|stop| stop.saturating_sub(start + 1) <= HASH_BYTES) {
        data["stable_prefix_items"] = json!(stable_count);
        data["stable_prefix_bytes"] = json!(stop - start - 1);
        data["stable_prefix_fingerprint"] = json!(fingerprint(
            key,
            b"stable-input-prefix",
            &[&bytes[start + 1..stop]]
        ));
    }
    if let Some(p) = prior.filter(|p| p["stable_prefix_fingerprint"].is_string())
        && let Some(stop) =
            prior_stable_end.filter(|stop| stop.saturating_sub(start + 1) <= HASH_BYTES)
    {
        data["prior_stable_prefix_items"] = p["stable_prefix_items"].clone();
        data["prior_stable_prefix_bytes"] = p["stable_prefix_bytes"].clone();
        data["prior_stable_prefix_equal"] = json!(
            p["stable_prefix_fingerprint"]
                == fingerprint(key, b"stable-input-prefix", &[&bytes[start + 1..stop]])
        );
    }
    let complete = items.len() == count
        && items.iter().all(|i| i["fingerprint"].is_string())
        && inner.len() <= HASH_BYTES;
    data["omitted_items"] = json!(count.saturating_sub(items.len()));
    data["unfingerprinted_items"] =
        json!(items.iter().filter(|i| i["fingerprint"].is_null()).count());
    data["coverage_complete"] = json!(complete);
    data["coverage_reason"] = json!(if complete {
        "complete"
    } else {
        "item_or_hash_budget"
    });
    data["items"] = json!(items);
    if let Some(p) = prior.filter(|p| p["coverage_complete"] == true)
        && let Some(stop) = prior_end.filter(|stop| stop.saturating_sub(start + 1) <= HASH_BYTES)
    {
        let digest = fingerprint(key, b"input-prefix", &[&bytes[start + 1..stop]]);
        data["prior_prefix_equal"] = json!(p["input_fingerprint"] == digest);
        data["prior_item_boundary"] = p["item_count"].clone();
    }
    data
}

fn previous(service: &Service, id: &str) -> Option<Value> {
    let _guard = LOG_LOCK.lock().ok()?;
    let path = directory(service, false).ok()?.join(format!("{id}.jsonl"));
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    if !file.metadata().ok()?.is_file() || file.metadata().ok()?.len() > JOURNAL_LANE {
        return None;
    }
    let mut reader = BufReader::new(file.take(JOURNAL_LANE));
    let mut line = Vec::new();
    let mut previous = None;
    loop {
        line.clear();
        let n = reader
            .by_ref()
            .take((JOURNAL_RECORD + 1) as u64)
            .read_until(b'\n', &mut line)
            .ok()?;
        if n == 0 {
            break;
        }
        if n > JOURNAL_RECORD {
            return None;
        }
        if let Ok(value) = serde_json::from_slice::<Value>(&line)
            && value["event"] == "review_wire"
        {
            previous = value.get("data").cloned();
        }
    }
    previous
}

pub(in crate::proxy) struct ReviewWire<'a> {
    pub id: &'a str,
    pub request_id: &'a str,
    pub attempt_id: &'a str,
    pub epoch: u64,
    pub reason: Option<&'a str>,
}

pub(in crate::proxy) fn review_wire(
    service: &Service,
    context: &ReviewWire<'_>,
    request: &reqwest::Request,
    body: &Value,
    transport_attempt: usize,
) {
    let Some(key) = key() else {
        JOURNAL_DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let Some(bytes) = request.body().and_then(reqwest::Body::as_bytes) else {
        JOURNAL_DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let prior = previous(service, context.id);
    let data = summary(
        bytes,
        body,
        request.url().as_str(),
        prior.as_ref(),
        context,
        transport_attempt,
        key,
    );
    journal_attempt(
        service,
        context.id,
        context.request_id,
        context.attempt_id,
        "review_wire",
        &data,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(body: &Value, prior: Option<&Value>, key: &[u8; 32]) -> Value {
        let request = reqwest::Client::new()
            .post("http://127.0.0.1/PRIVATE_ROUTE")
            .json(body)
            .build()
            .unwrap();
        let context = ReviewWire {
            id: "lane",
            request_id: "request",
            attempt_id: "attempt",
            epoch: 2,
            reason: None,
        };
        summary(
            request.body().unwrap().as_bytes().unwrap(),
            body,
            request.url().as_str(),
            prior,
            &context,
            0,
            key,
        )
    }

    #[test]
    fn hmac_matches_independent_python_implementation() {
        assert_eq!(
            fingerprint(&[7; 32], b"item", &[b"fixture"]),
            "912ae436bcf3d6b06f6a652fb376beedc32c5f0eca73483c2ce2d4acb72d4b25"
        );
        assert_ne!(
            fingerprint(&[7; 32], b"item", &[b"fixture"]),
            fingerprint(&[7; 32], b"settings", &[b"fixture"])
        );
    }

    #[test]
    fn final_raw_body_order_changes_and_key_scope_are_observable_without_content() {
        let a = json!({"role":"user","content":"PRIVATE_SENTINEL \\\" [{ } ] \\n"});
        let b = json!({"type":"reasoning","encrypted_content":"PRIVATE_CIPHER"});
        let body = json!({"model":"PRIVATE_MODEL","prompt_cache_key":"PRIVATE_KEY","instructions":"PRIVATE_INSTRUCTION","input":[a,b]});
        let first = capture(&body, None, &[7; 32]);
        assert_eq!(first["coverage_complete"], true);
        assert_eq!(first["item_count"], 2);
        let mut appended = body.clone();
        appended["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"assistant","content":"tail"}));
        let next = capture(&appended, Some(&first), &[7; 32]);
        assert_eq!(next["prior_prefix_equal"], true);
        assert_eq!(next["settings_equal"], true);
        appended["input"].as_array_mut().unwrap().swap(0, 1);
        let reordered = capture(&appended, Some(&first), &[7; 32]);
        assert_eq!(reordered["prior_prefix_equal"], false);
        assert_ne!(
            reordered["items"][0]["fingerprint"],
            first["items"][0]["fingerprint"]
        );
        appended["model"] = json!("changed");
        let changed = capture(&appended, Some(&first), &[7; 32]);
        assert_eq!(changed["settings_equal"], false);
        assert_eq!(changed["model_equal"], false);
        let restart = capture(&body, Some(&first), &[8; 32]);
        assert!(restart["prior_prefix_equal"].is_null());
        assert!(restart["settings_equal"].is_null());
        assert!(first.to_string().find("PRIVATE").is_none());
        let transformed = crate::proxy::codex_body(&body);
        let raw = capture(&transformed, None, &[7; 32]);
        assert_ne!(raw["settings_fingerprint"], first["settings_fingerprint"]);
    }

    #[test]
    fn stable_observation_prefix_ignores_detail_cap_but_detects_real_mutations() {
        let mut items = (0..100)
            .map(|i| json!({"role":"user","content":format!("source {i}")}))
            .collect::<Vec<_>>();
        items.push(json!({"role":"assistant","content":"old ledger"}));
        let first_body = json!({"input":items});
        let first = capture(&first_body, None, &[7; 32]);
        assert_eq!(first["coverage_complete"], false);
        assert_eq!(first["stable_prefix_items"], 100);
        assert!(first["stable_prefix_fingerprint"].is_string());
        let mut next_body = first_body.clone();
        let items = next_body["input"].as_array_mut().unwrap();
        items.pop();
        items.push(json!({"role":"user","content":"new source"}));
        items.push(json!({"role":"assistant","content":"changed ledger"}));
        let next = capture(&next_body, Some(&first), &[7; 32]);
        assert_eq!(next["prior_stable_prefix_equal"], true);
        assert_eq!(next["prior_stable_prefix_items"], 100);
        assert!(next["prior_prefix_equal"].is_null());
        next_body["input"].as_array_mut().unwrap().swap(50, 51);
        let changed = capture(&next_body, Some(&first), &[7; 32]);
        assert_eq!(changed["prior_stable_prefix_equal"], false);
        let restart = capture(&next_body, Some(&first), &[8; 32]);
        assert!(restart["prior_stable_prefix_equal"].is_null());
        let oversized = json!({"input":[{"role":"user","content":"x".repeat(HASH_BYTES + 1)}, {"role":"assistant","content":"ledger"}]});
        let unknown = capture(&oversized, None, &[7; 32]);
        assert!(unknown["stable_prefix_fingerprint"].is_null());
        assert!(
            capture(&oversized, Some(&unknown), &[7; 32])["prior_stable_prefix_equal"].is_null()
        );
    }

    #[test]
    fn giant_items_many_items_and_incomplete_prefix_are_explicitly_bounded() {
        let giant = json!({"input":[{"role":"user","content":"G".repeat(ITEM_BYTES+1)}]});
        let first = capture(&giant, None, &[7; 32]);
        assert_eq!(first["coverage_complete"], false);
        assert!(first["items"][0]["fingerprint"].is_null());
        let next = capture(&giant, Some(&first), &[7; 32]);
        assert!(next["prior_prefix_equal"].is_null());
        let many = json!({"input":vec![json!({"type":"PRIVATE_TYPE","role":"PRIVATE_ROLE","content":"small"});10000]});
        let data = capture(&many, None, &[7; 32]);
        assert_eq!(data["item_count"], 10000);
        assert_eq!(data["items"].as_array().unwrap().len(), ITEMS);
        assert_eq!(data["omitted_items"], 10000 - ITEMS);
        assert!(data.to_string().len() < JOURNAL_RECORD - 2048);
        assert!(!data.to_string().contains("PRIVATE"));
        let huge = json!({"input":[{"role":"user","content":"x".repeat(SCAN_BYTES+1)}]});
        let data = capture(&huge, None, &[7; 32]);
        assert_eq!(data["coverage_reason"], "scan_byte_limit");
        assert!(data["items"].as_array().unwrap().is_empty());
        assert!(data["prior_prefix_equal"].is_null());
    }
}
