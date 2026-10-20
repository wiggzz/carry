// Joined fork/lease controls. Advice admission, publication, hydration and lease
// lifecycle execute production code; only elapsed primary rounds are staged.
#[test]
fn lease_join_cold_child_keeps_cut_authority_with_fresh_branch_local_leases() {
    let config = reviewer_config();
    let mut raw = Session::default();
    let history = vec![
        json!({"role":"user","content":"goal"}),
        json!({"role":"assistant","content":"historically protected evidence"}),
        json!({"role":"user","content":"old direction"}),
        json!({"role":"assistant","content":"removable evidence"}),
        json!({"role":"user","content":"cut boundary"}),
    ];
    raw.ingest(&history).unwrap();
    for item in &mut raw.history {
        item.exposed = true;
    }
    let body = json!({"model":"gpt-6-sol","input":history});
    raw.review_context = cache_base(&body);
    raw.initialize_keep_leases(8);
    let mut parent = raw.clone();
    parent
        .apply_advice(
            &parent.groups(),
            &json!({"protected":["g2"],"removable":["g4"],
        "memories":[{"source_ids":["g4"],"text":"portable cut fact"}]}),
        )
        .unwrap();
    forking::publish(&config, &mut parent, &raw, true);
    assert_eq!(parent.fork_points.len(), 1);
    let point = parent.fork_points[0].clone();
    // Later parent-only protection must not leak into the immutable raw cut.
    let fact = format!("m{}", parent.memories[0].id);
    parent
        .apply_advice(
            &parent.groups(),
            &json!({"protected":["g2",fact],"removable":[],"memories":[]}),
        )
        .unwrap();
    parent
        .apply_advice(
            &parent.groups(),
            &json!({"protected":["g2",fact],"removable":[],"memories":[]}),
        )
        .unwrap();
    parent.completed_requests = 40;
    parent.reserve_extra_keep_attempt();
    let old_pending = crate::proxy_state::PendingKeepReview {
        request_id: "parent-only-pending".into(),
        targets: parent.due_keep_targets(),
    };
    parent.keep_leases.as_mut().unwrap().pending = Some(old_pending.clone());
    parent.background_review_pending = true;
    parent.primary.calls = 20;
    parent.shadow.calls = 4;
    let parent_before = serde_json::to_value(&parent).unwrap();
    let mut child_body = body.clone();
    child_body["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"child C"}));
    let mut child = Session::default();
    assert!(forking::hydrate(&config, &mut child, "parent/cut", &point, &child_body).unwrap());
    child.initialize_keep_leases(8);
    assert_eq!(child.memories[0].text, "portable cut fact");
    assert!(child.drop_authorized(&child.groups()[3]));
    assert_eq!(child.opinions[&2], "keep");
    assert_eq!(child.completed_requests, 0);
    assert_eq!((child.primary.calls, child.shadow.calls), (0, 0));
    assert!(child.primary_cache.is_empty() && child.reviewer_cache.is_empty());
    assert!(!child.background_review_pending);
    let leases = child.keep_leases.as_ref().unwrap();
    assert!(leases.pending.is_none() && leases.extra_attempted.is_empty());
    assert!(
        !leases.entries.contains_key(&fact),
        "later parent fact protection cannot resurrect in child"
    );
    assert_eq!(leases.entries["g2"].renewed_at_round, 0);
    assert!(
        leases.entries["g2"].generation
            < parent.keep_leases.as_ref().unwrap().entries["g2"].generation
    );
    child.completed_requests = 7;
    assert!(!child.due_keep_targets().contains_key("g2"));
    child.completed_requests = 8;
    assert!(child.due_keep_targets().contains_key("g2"));
    child.resolve_keep_targets(&old_pending);
    assert_eq!(
        child.opinions[&2], "keep",
        "parent target generation cannot release child protection"
    );
    let restored: Session = serde_json::from_slice(&serde_json::to_vec(&child).unwrap()).unwrap();
    assert_eq!(restored.due_keep_targets(), child.due_keep_targets());
    assert_eq!(serde_json::to_value(&parent).unwrap(), parent_before);
}
