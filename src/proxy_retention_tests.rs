// Retention promises cannot be overridden by the continuation working-set cap.
#[test]
fn retention_reference_pressure_preserves_inactive_owner_and_all_observed_ids() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = checkpoint_service(directory.path());
    service.config.history_max_references = 4;
    let namespace = "retention-pressure";
    let mut frozen = None;
    for n in 0..20 {
        let id = if n == 0 { "inactive" } else { "busy" };
        let input = vec![json!({"role":"user","content":id})];
        let mut session = if n > 1 {
            history::restore(&service, id).unwrap()
        } else {
            let mut session = Session {
                history_shared: true,
                history_namespace: Some(namespace.into()),
                ..Session::default()
            };
            session.ingest(&input).unwrap();
            session
        };
        session.completed_requests += 1;
        let response = json!({"id":format!("response-{n}"), "output":[
            {"role":"assistant","content":format!("observed-{n}")} ]});
        history::committed(&service, id, &session, Some((&input, &response))).unwrap();
        if n == 0 {
            frozen = Some(serde_json::to_value(&session).unwrap());
        }
    }
    drop(service);
    let service = checkpoint_service(directory.path());
    assert_eq!(
        serde_json::to_value(
            history::restore(&service, "inactive")
                .expect("seven-day inactive owner must survive reference pressure and restart")
        )
        .unwrap(),
        frozen.unwrap()
    );
    for n in 0..20 {
        let owner = if n == 0 { "inactive" } else { "busy" };
        let delta = json!({"role":"user","content":"resume"});
        let mut body = json!({"input":[delta],"previous_response_id":format!("response-{n}")});
        history::expand(&service, namespace, &mut body)
            .expect("every observed identifier inside retention remains expandable");
        assert_eq!(
            body["input"],
            json!([
            {"role":"user","content":owner},
            {"role":"assistant","content":format!("observed-{n}")}, delta])
        );
    }
}
