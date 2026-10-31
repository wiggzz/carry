//! Bounded predecessor decoding at the atomic history-index load boundary.
//! No response-ID alias, normal-lookup fallback or historical-money conversion.
use super::*;
use std::collections::VecDeque;

fn invalid() -> std::io::Error {
    std::io::Error::other("unproven or conflicting legacy history")
}

// Frozen predecessor producers. Only an exact old root proves owner, client,
// source and reviewer settings; matching content or an external ID never does.
fn legacy_namespace(
    config: &ProxyCli,
    owner: &Option<String>,
    body: &Value,
    version: u8,
) -> String {
    let mut settings = json!({
        "upstream":config.upstream_url, "subscription":config.codex_login,
        "reviewer":config.classifier_model, "reviewer_url":config.classifier_url,
        "reasoning":config.classifier_reasoning_effort,
        "reviewer_output":config.classifier_max_output_tokens
    });
    if version == 3 {
        let mut context = cache_base(body);
        if let Some(object) = context.as_object_mut() {
            object.remove("prompt_cache_key");
        }
        settings["policy"] = json!("carry-prefix-advice-v3");
        settings["context"] = context;
    } else {
        settings["policy"] = json!("carry-prefix-advice-v4");
        settings["model"] = body.get("model").cloned().unwrap_or(Value::Null);
        settings["prompt"] = crate::savings::prompt_overhead(body);
    }
    hash(&json!({"policy":"carry-prefix-history-v3", "owner":owner,
        "settings":hash(&settings), "client_scope":stable_client_scope(body)}))
}

fn unique<T: PartialEq>(
    map: &mut BTreeMap<String, T>,
    key: String,
    value: T,
) -> std::io::Result<()> {
    if map.get(&key).is_some_and(|old| old != &value) {
        return Err(invalid());
    }
    map.insert(key, value);
    Ok(())
}

pub(super) fn prepare(
    service: &Service,
    index: &mut Index,
    owner: &Option<String>,
    body: &Value,
    namespace: &str,
) -> std::io::Result<bool> {
    let roots = index
        .tips
        .values()
        .map(|r| r.namespace.as_str())
        .chain(index.transcripts.values().map(|r| r.namespace.as_str()))
        .chain(
            index
                .snapshots
                .values()
                .filter_map(|s| s.history_namespace.as_deref()),
        )
        .filter(|root| !root.starts_with("observed-v1:"))
        .collect::<HashSet<_>>();
    if roots.is_empty() {
        return Ok(false);
    }
    let mut contexts = vec![body.clone()];
    contexts.extend(
        index
            .snapshots
            .values()
            .filter(|s| {
                s.history_namespace
                    .as_deref()
                    .is_some_and(|root| roots.contains(root))
            })
            .map(|s| s.review_context.clone()),
    );
    // Explicit sessions are file-authoritative. Read only this exact owner's
    // bounded checkpoint, never enumerate/adopt diagnostic mirrors as authority.
    if let Some(owner) = owner {
        match File::open(service.config.state_dir.join(format!("{owner}.json"))) {
            Ok(file) => {
                if file.metadata()?.len() > CHECKPOINT_MAX_BYTES as u64 {
                    return Err(invalid());
                }
                let session: Session = serde_json::from_reader(std::io::Read::take(
                    file,
                    CHECKPOINT_MAX_BYTES as u64 + 1,
                ))
                .map_err(std::io::Error::other)?;
                if !session.history_shared && (session.version == 0 || session.version == 3) {
                    contexts.push(session.review_context);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let mut replacements = BTreeMap::new();
    for mut context in contexts {
        if !context.is_object() {
            continue;
        }
        // cache_base omits client metadata; the old root itself must prove this
        // request's exact client scope. Never borrow a snapshot's owner/scope.
        if let Some(metadata) = body.get("client_metadata") {
            context["client_metadata"] = metadata.clone();
        }
        if observed_namespace(&service.config, owner, &context) != namespace {
            continue;
        }
        for version in [3, 4] {
            let old = legacy_namespace(&service.config, owner, &context, version);
            if roots.contains(old.as_str()) {
                replacements.insert(old, namespace.to_owned());
            }
        }
    }
    if replacements.is_empty() {
        return Ok(false);
    }
    // A running old authority cannot publish its prior root after the upgrade.
    // Wait for ordinary request retry rather than creating another paid owner.
    let pins = pinned(service);
    if index
        .tips
        .values()
        .any(|r| replacements.contains_key(&r.namespace) && pins.contains(&r.session))
        || index
            .transcripts
            .values()
            .any(|r| replacements.contains_key(&r.namespace) && pins.contains(&r.session))
    {
        return Err(invalid());
    }

    // Linear traversal of the bounded immutable graph. Validate hashes while
    // rewriting roots, then update every referencing authority in this one
    // generation. No expiry, receipt, trajectory or session ID is rewritten.
    let mut children: BTreeMap<&str, Vec<(&String, &Node)>> = BTreeMap::new();
    for (key, node) in &index.nodes {
        children
            .entry(node.parent.as_str())
            .or_default()
            .push((key, node));
    }
    let mut queue = replacements.keys().cloned().collect::<VecDeque<_>>();
    let mut nodes = BTreeMap::new();
    while let Some(parent) = queue.pop_front() {
        for (key, node) in children.get(parent.as_str()).into_iter().flatten() {
            if hash(&(&node.parent, &node.value)) != key.as_str() {
                return Err(invalid());
            }
            let rewritten_parent = replacements.get(&parent).ok_or_else(invalid)?.clone();
            let rewritten = hash(&(&rewritten_parent, &node.value));
            if replacements
                .insert(key.to_string(), rewritten.clone())
                .is_some()
            {
                return Err(invalid());
            }
            unique(
                &mut nodes,
                rewritten,
                Node {
                    parent: rewritten_parent,
                    value: node.value.clone(),
                },
            )?;
            queue.push_back(key.to_string());
        }
    }
    drop(children);
    let rewrite = |key: &str| replacements.get(key).cloned().ok_or_else(invalid);
    let mut tips = BTreeMap::new();
    for (key, mut reference) in std::mem::take(&mut index.tips) {
        let key = if replacements.contains_key(&reference.namespace) {
            reference.namespace = namespace.to_owned();
            rewrite(&key)?
        } else {
            key
        };
        unique(&mut tips, key, reference)?;
    }
    let mut transcripts = BTreeMap::new();
    for (key, mut reference) in std::mem::take(&mut index.transcripts) {
        let key = if replacements.contains_key(&reference.namespace) {
            let kind = ["response", "conversation"]
                .into_iter()
                .find(|kind| hash(&(&reference.namespace, kind, &reference.external_id)) == key)
                .ok_or_else(invalid)?;
            reference.frontier = rewrite(&reference.frontier)?;
            reference.namespace = namespace.to_owned();
            hash(&(namespace, kind, &reference.external_id))
        } else {
            key
        };
        unique(&mut transcripts, key, reference)?;
    }
    for snapshot in index.snapshots.values_mut() {
        if snapshot
            .history_namespace
            .as_ref()
            .is_some_and(|root| replacements.contains_key(root))
        {
            if let Some(frontier) = &snapshot.history_frontier {
                snapshot.history_frontier = Some(rewrite(frontier)?);
            } else if !snapshot.history.is_empty() {
                return Err(invalid());
            }
            snapshot.history_namespace = Some(namespace.to_owned());
        }
    }
    index.tips = tips;
    index.transcripts = transcripts;
    // Advice from predecessor schemas has no independent reviewer-validity
    // proof. Retire it; do not grant it the current reviewer contract.
    index
        .reviews
        .retain(|key, _| !replacements.contains_key(key));
    index.nodes.retain(|key, _| !replacements.contains_key(key));
    for (key, node) in nodes {
        unique(&mut index.nodes, key, node)?;
    }
    Ok(true)
}
