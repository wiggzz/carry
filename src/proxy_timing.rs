//! Request-local monotonic evidence, never policy/accounting authority.
use super::*;
use serde::Serialize;

#[derive(Serialize)]
pub(super) struct Transport {
    ordinal: usize,
    dispatch_us: u64,
    headers_us: Option<u64>,
    http_status: Option<u16>,
    first_body_us: Option<u64>,
    body_complete_us: Option<u64>,
    provider_request_id: Option<String>,
    pub error_category: Option<&'static str>,
    pub receipt: Option<Value>,
}

pub(super) struct Phases {
    service: Arc<Service>,
    policy_context: &'static str,
    pub session_id: String,
    request_id: String,
    review_attempt_id: Option<String>,
    pub native_attempt_id: Option<String>,
    role: &'static str,
    background: bool,
    started: Instant,
    pub outcome: &'static str,
    pub active_phase: &'static str,
    pub primary_lock_wait_us: Option<u64>,
    pub state_load_us: Option<u64>,
    pub planning_us: Option<u64>,
    pub blocking_review_us: Option<u64>,
    pub preparation_done_us: Option<u64>,
    pub settlement_us: Option<u64>,
    pub relay_send_wait_us: Option<u64>,
    attempts: Vec<Transport>,
    receipt: Option<Value>,
    persistence: &'static str,
    downstream_disconnected: bool,
    failure_phase: Option<&'static str>,
    auth_wait_us: u64,
    auth_started_us: Option<u64>,
}

impl Phases {
    pub fn new(
        service: Arc<Service>,
        request_id: &str,
        review: Option<(&str, &str, bool)>,
    ) -> Self {
        Self {
            policy_context: if review.is_some() {
                "effective"
            } else if service.config.mode == Mode::Off {
                "configured_off"
            } else {
                "configured_unresolved"
            },
            service,
            // Pre-identity failures/unscoped off requests share one bounded lane.
            session_id: review.map_or_else(
                || format!("{:x}", Sha256::digest(b"carry-unscoped-timing")),
                |(id, _, _)| id.into(),
            ),
            request_id: request_id.into(),
            review_attempt_id: review.map(|(_, attempt, _)| attempt.into()),
            native_attempt_id: None,
            role: if review.is_some() {
                "reviewer"
            } else {
                "primary"
            },
            background: review.is_some_and(|(_, _, background)| background),
            started: Instant::now(),
            outcome: "dropped_before_terminal",
            active_phase: "preparation",
            primary_lock_wait_us: None,
            state_load_us: None,
            planning_us: None,
            blocking_review_us: None,
            preparation_done_us: None,
            settlement_us: None,
            relay_send_wait_us: None,
            attempts: Vec::with_capacity(2),
            receipt: None,
            persistence: "not_attempted",
            downstream_disconnected: false,
            failure_phase: None,
            auth_wait_us: 0,
            auth_started_us: None,
        }
    }

    pub fn effective_service(&mut self, service: Arc<Service>) {
        self.service = service;
        self.policy_context = "effective";
    }

    pub fn elapsed_us(&self) -> u64 {
        self.started.elapsed().as_micros() as u64
    }

    pub fn add(target: &mut Option<u64>, started: Instant) {
        *target = Some(
            target
                .unwrap_or(0)
                .saturating_add(started.elapsed().as_micros() as u64),
        );
    }

    pub fn auth_started(&mut self) {
        self.active_phase = "auth";
        self.auth_started_us = Some(self.elapsed_us());
    }
    pub fn auth_finished(&mut self) {
        if let Some(started) = self.auth_started_us.take() {
            self.auth_wait_us = self
                .auth_wait_us
                .saturating_add(self.elapsed_us().saturating_sub(started));
        }
        self.active_phase = "preparation";
    }
    pub fn attempt_count(&self) -> usize {
        self.attempts.len()
    }

    pub fn dispatch(&mut self) {
        self.active_phase = "upstream_headers";
        // The existing subscription owner allows at most two HTTP attempts.
        self.attempts.push(Transport {
            ordinal: self.attempts.len(),
            dispatch_us: self.elapsed_us(),
            headers_us: None,
            http_status: None,
            first_body_us: None,
            body_complete_us: None,
            provider_request_id: None,
            error_category: None,
            receipt: None,
        });
    }

    pub fn response_headers(&mut self, response: &reqwest::Response) {
        self.headers(response.status());
        if let Some(attempt) = self.attempts.last_mut() {
            attempt.provider_request_id = response
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .and_then(Self::safe_id);
        }
    }
    fn safe_id(value: &str) -> Option<String> {
        (!value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)))
        .then(|| value.into())
    }
    pub fn failed(&mut self, category: &'static str) {
        self.outcome = category;
        self.failure_phase = Some(self.active_phase);
    }
    pub fn failure_phase_for_body(&mut self) {
        self.failure_phase.get_or_insert("upstream_body");
    }
    pub fn transport_error(&mut self, error: &reqwest::Error) {
        let category = if error.is_timeout() {
            "upstream_timeout"
        } else if error.is_connect() {
            "upstream_connect_error"
        } else if error.is_body() {
            "upstream_body_error"
        } else {
            "upstream_transport_error"
        };
        self.failure_phase = Some(self.active_phase);
        self.outcome = category;
        if let Some(attempt) = self.attempts.last_mut() {
            attempt.error_category = Some(category);
        }
    }
    pub fn receipt(&mut self, receipt: &ClientReceipt) {
        self.downstream_disconnected |= receipt.downstream_disconnected;
        self.persistence = "see_receipt_persistence_events";
        if receipt
            .provider_status
            .as_deref()
            .is_some_and(|s| s == "failed" || s == "incomplete" || s == "cancelled")
        {
            self.failure_phase.get_or_insert("upstream_body");
        }
        self.receipt = Some(json!({
            "attempt_id":receipt.request_id,"parent_request_id":receipt.parent_request_id,
            "model":receipt.model,"route_fingerprint":receipt.route,
            "contract_fingerprint":receipt.forecast.configuration,
            "provider_status":receipt.provider_status,"error":receipt.error,
            "usage_present":!receipt.usage.as_object().is_none_or(|v| v.is_empty()),
            "usage_status":if receipt.usage.as_object().is_none_or(|v| v.is_empty()) { "missing" }
                else if crate::openai::Usage::from_receipt(&receipt.usage).is_some() { "numeric" } else { "invalid" },
            "native_usage_valid":crate::openai::Usage::from_receipt(&receipt.usage).is_some(),
            "pricing_available":receipt.cost.is_some(),"service_tier":receipt.service_tier,
            "version":env!("CARGO_PKG_VERSION"),"transport":receipt.transport,
        }));
        if let Some(attempt) = self
            .attempts
            .get_mut(usize::from(receipt.transport_ordinal))
        {
            attempt.receipt = self.receipt.clone();
        }
    }

    pub fn headers(&mut self, status: StatusCode) {
        let at = self.elapsed_us();
        if let Some(attempt) = self.attempts.last_mut() {
            attempt.headers_us = Some(at);
            attempt.http_status = Some(status.as_u16());
        }
        self.active_phase = "upstream_body";
    }

    pub fn first_body(&mut self, bytes: &Bytes) {
        if bytes.is_empty() {
            return;
        }
        let at = self.elapsed_us();
        if let Some(attempt) = self.attempts.last_mut() {
            attempt.first_body_us.get_or_insert(at);
        }
    }

    pub fn body_complete(&mut self) {
        let at = self.elapsed_us();
        if let Some(attempt) = self.attempts.last_mut() {
            attempt.body_complete_us = Some(at);
        }
        self.active_phase = "settlement";
    }
}

impl Drop for Phases {
    fn drop(&mut self) {
        // Measurement endpoint precedes diagnostic serialization/handoff/I/O.
        let total_us = self.elapsed_us();
        let last = self.attempts.last();
        let data = json!({
            "schema_version":2,"basis":"monotonic_forward_or_review_chunk_entry_us",
            "role":self.role,"background":self.background,"outcome":self.outcome,
            "policy_context":self.policy_context,
            "incomplete":self.outcome == "dropped_before_terminal" || (self.outcome != "rejected" && last.is_none_or(|a|a.body_complete_us.is_none())),
            "active_phase":self.active_phase,"failure_phase":self.failure_phase,"native_attempt_id":self.native_attempt_id,
            "auth_wait_us":self.auth_wait_us.saturating_add(self.auth_started_us.map_or(0, |s| self.elapsed_us().saturating_sub(s))),
            "primary_lock_wait_us":self.primary_lock_wait_us,"state_load_us":self.state_load_us,
            "planning_us":self.planning_us,"blocking_review_us":self.blocking_review_us,
            "preparation_done_us":self.preparation_done_us,"attempts":self.attempts,
            "first_body_us":last.and_then(|a|a.first_body_us),
            "body_complete_us":last.and_then(|a|a.body_complete_us),
            "settlement_us":self.settlement_us,"relay_send_wait_us":self.relay_send_wait_us,
            "total_us":total_us,
            "actual_dispatch":!self.attempts.is_empty(),
            "explicit_local_cancellation":Value::Null,
            "downstream_disconnected":self.downstream_disconnected,
            "receipt":self.receipt,"persistence":self.persistence
        });
        logging::phase_summary(
            &self.service,
            &self.session_id,
            &self.request_id,
            self.review_attempt_id.as_deref(),
            &data,
        );
    }
}
