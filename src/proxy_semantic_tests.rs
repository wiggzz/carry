// Direct controls for the prefix-advice/request-projection model.
fn semantic_subject() -> Session {
    let mut s = Session::default();
    s.ingest(&[json!({"role":"user","content":"goal"}),
        json!({"role":"assistant","content":"large evidence ".repeat(3000)}),
        json!({"role":"user","content":"current task"})]).unwrap();
    for item in &mut s.history { item.exposed = true; }
    s.review_context = json!({"model":"gpt-6-luna"});
    s.history_namespace = Some("fixture-owner".into());
    s
}

#[test]
fn canonical_source_survives_disposable_projection_and_owner_serialization() {
    let config = reviewer_config();
    let mut s = semantic_subject();
    let canonical = s.render_primary();
    s.apply_advice(&s.groups(),&json!({"protected":[],"removable":["g2"],"memories":[{"source_ids":["g2"],"text":"fact"}]})).unwrap();
    let record = semantic::Record::capture(&s);
    let omitted = HashSet::from([2]);
    let projection = s.render_projection(&omitted);
    assert!(!projection.contains(&canonical[1]));
    assert!(json!(projection).to_string().contains("fact"));
    assert_eq!(s.render_primary(),canonical);
    let serialized = serde_json::to_value(&s).unwrap();
    assert!(serialized.get("opinions").is_none() && serialized.get("memories").is_none());
    let mut restored: Session = serde_json::from_value(serialized).unwrap();
    record.apply(&mut restored);
    assert!(restored.drop_authorized(&restored.groups()[1]));
    assert_eq!(restored.render_primary(),canonical);
    assert_eq!(config.history_max_age_secs,604800);
}

#[test]
fn merged_projected_group_authorizes_original_member_union_not_separator() {
    let mut s = Session::default();
    s.ingest(&[json!({"role":"user","content":"goal"}),json!({"role":"assistant","content":"first"}),
        json!({"role":"user","content":"separator"}),json!({"role":"assistant","content":"second"}),
        json!({"role":"user","content":"task"})]).unwrap();
    for item in &mut s.history { item.exposed = true; }
    let groups = s.groups_for(&HashSet::from([3]));
    assert_eq!(groups.iter().find(|g|g.id == 2).unwrap().members,vec![2,4]);
    s.apply_advice(&groups,&json!({"protected":[],"removable":["g2"],"memories":[{"source_ids":["g2"],"text":"union fact"}]})).unwrap();
    assert_eq!(s.memories[0].source_ids,vec![2,4]);
    assert_eq!(s.memory_anchors[&s.memories[0].id],4);
    let r = semantic::Record::capture(&s);
    let mut restored = s.clone(); r.apply(&mut restored);
    assert!(restored.groups().iter().filter(|g|[2,4].contains(&g.id)).all(|g|restored.drop_authorized(g)));
    assert!(!restored.drop_authorized(&restored.groups()[2]));
    let sent = restored.render_projection(&HashSet::from([2,4]));
    assert_eq!(sent.last().unwrap()["content"],"task");
    assert!(sent.iter().position(|i|i.to_string().contains("union fact")).unwrap() > sent.iter().position(|i|i["content"] == "separator").unwrap());
}

#[test]
fn native_suffix_dependency_extension_rejects_drop_and_fact_but_unrelated_growth_does_not() {
    for ty in ["function_call","custom_tool_call"] {
        let mut s = Session::default();
        let mut h = vec![json!({"role":"user","content":"goal"}),
            json!({"type":ty,"call_id":"x","name":"tool","arguments":"{}"}),
            json!({"type":format!("{ty}_output"),"call_id":"x","output":"done"}),
            json!({"role":"user","content":"task"})];
        s.ingest(&h).unwrap(); for i in &mut s.history { i.exposed = true; }
        s.apply_advice(&s.groups(),&json!({"protected":[],"removable":["g2"],"memories":[{"source_ids":["g2"],"text":"fact"}]})).unwrap();
        let r = semantic::Record::capture(&s);
        h.push(json!({"role":"user","content":"unrelated"})); s.ingest(&h).unwrap(); r.apply(&mut s);
        assert!(s.groups().iter().any(|g|s.drop_authorized(g))); assert_eq!(s.memories.len(),1);
        h.push(json!({"type":format!("{ty}_output"),"call_id":"x","output":"late duplicate"}));
        s.ingest(&h).unwrap(); r.apply(&mut s);
        assert!(!s.groups().iter().any(|g|s.drop_authorized(g))); assert!(s.memories.is_empty());
    }
}

#[test]
fn generic_wire_cache_has_no_purpose_and_requires_exact_actual_namespace() {
    let primary = resolve_model("gpt-6-luna",Route::new("https://api.openai.com/v1/responses",false),None,None,2048,0).unwrap();
    let reviewer = primary.clone();
    let body = json!({"model":"gpt-6-luna","instructions":"direction","prompt_cache_key":"affinity","input":[{"role":"user","content":"data ".repeat(4000)}]});
    let usage = json!({"input_tokens":10000,"input_tokens_details":{"cached_tokens":9000}});
    let mut cache = Vec::new();
    crate::proxy_cache::observe(&mut cache,&body,&usage,Some(&reviewer),"same-auth",100);
    let lookup = |body: &Value,r: &ResolvedModel,key: &str,at| crate::proxy_cache::reusable(&cache,body,Some(r),key,at);
    assert!(lookup(&body,&primary,"same-auth",101) > 0.0,"reviewer receipt is primary evidence for identical wire");
    for (field,value) in [("model",json!("gpt-6-sol")),("instructions",json!("different")),("prompt_cache_key",json!("different")),("reasoning",json!({"effort":"high"})),("max_output_tokens",json!(99))] {
        let mut changed = body.clone(); changed[field] = value; assert_eq!(lookup(&changed,&primary,"same-auth",101),0.0);
    }
    let mut changed = body.clone(); changed["input"][0]["content"] = json!("different");
    assert_eq!(lookup(&changed,&primary,"same-auth",101),0.0);
    assert_eq!(lookup(&body,&primary,"different-auth",101),0.0);
    assert_eq!(lookup(&body,&primary,"same-auth",1900),0.0);
    assert_eq!(lookup(&body,&primary,"same-auth",99),0.0);
    let mut route = primary.clone(); route.route = Route::new("https://other.invalid/v1/responses",false);
    assert_eq!(lookup(&body,&route,"same-auth",101),0.0);
    route = primary.clone(); route.cache = None;
    assert_eq!(lookup(&body,&route,"same-auth",101),0.0);
    let mut unknown = Vec::new(); crate::proxy_cache::observe(&mut unknown,&body,&Value::Null,Some(&primary),"same-auth",100);
    assert!(unknown.is_empty());
    crate::proxy_cache::observe(&mut unknown,&body,&json!({"input_tokens":100,"input_tokens_details":{"cached_tokens":0}}),Some(&primary),"same-auth",100);
    assert_eq!(crate::proxy_cache::reusable(&unknown,&body,Some(&primary),"same-auth",101),0.0);
}

#[tokio::test]
async fn exact_prefix_empty_completion_supersedes_old_fact_and_stale_generation() {
    let dir = tempfile::tempdir().unwrap(); let service = checkpoint_service(dir.path());
    let mut s = semantic_subject(); s.review_epoch = 10;
    s.apply_advice(&s.groups(),&json!({"protected":[],"removable":["g2"],"memories":[{"source_ids":["g2"],"text":"retire me"}]})).unwrap();
    history::publish(&service,&s,&s).unwrap();
    let mut latest = s.clone(); latest.review_epoch = 11;
    latest.apply_advice(&latest.groups(),&json!({"protected":[],"removable":[format!("m{}",latest.memories[0].id)],"memories":[]})).unwrap();
    history::publish(&service,&s,&latest).unwrap();
    history::publish(&service,&s,&s).unwrap();
    let h = s.render_primary(); let r = history::advice(&service,"fixture-owner",&h).unwrap().unwrap();
    assert_eq!(r.generation,11); assert!(r.memories.is_empty());
    let mut sibling = h.clone(); sibling.push(json!({"role":"user","content":"B"}));
    assert_eq!(history::advice(&service,"fixture-owner",&sibling).unwrap().unwrap().generation,11);
    let mut divergent = h.clone(); divergent[2]["content"] = json!("A");
    assert!(history::advice(&service,"fixture-owner",&divergent).unwrap().is_none());
}
