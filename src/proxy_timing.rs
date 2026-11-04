//! Request-local monotonic evidence, never policy/accounting authority.
use super::*;
use serde::Serialize;

#[derive(Serialize)]
pub(super) struct Transport {
    ordinal: usize,
    dispatch_us: u64,
    headers_us: Option<u64>,
    status: Option<u16>,
    first_body_us: Option<u64>,
    body_complete_us: Option<u64>,
}

pub(super) struct Phases {
    service: Arc<Service>,
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
}

impl Phases {
    pub fn new(
        service: Arc<Service>,
        request_id: &str,
        review: Option<(&str, &str, bool)>,
    ) -> Self {
        Self {
            service,
            // Pre-identity failures/unscoped off requests share one bounded lane.
            session_id: format!("{:x}", Sha256::digest(b"carry-unscoped-timing")),
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
            outcome: "cancelled",
            active_phase: "preparation",
            primary_lock_wait_us: None,
            state_load_us: None,
            planning_us: None,
            blocking_review_us: None,
            preparation_done_us: None,
            settlement_us: None,
            relay_send_wait_us: None,
            attempts: Vec::with_capacity(2),
        }
        .with_review_session(review)
    }

    fn with_review_session(mut self, review: Option<(&str, &str, bool)>) -> Self {
        if let Some((id, _, _)) = review {
            self.session_id = id.into();
        }
        self
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

    pub fn dispatch(&mut self) {
        self.active_phase = "upstream_headers";
        // The existing subscription owner allows at most two HTTP attempts.
        debug_assert!(self.attempts.len() < 2);
        self.attempts.push(Transport {
            ordinal: self.attempts.len(),
            dispatch_us: self.elapsed_us(),
            headers_us: None,
            status: None,
            first_body_us: None,
            body_complete_us: None,
        });
    }

    pub fn headers(&mut self, status: StatusCode) {
        let at = self.elapsed_us();
        if let Some(attempt) = self.attempts.last_mut() {
            attempt.headers_us = Some(at);
            attempt.status = Some(status.as_u16());
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
        let last = self.attempts.last();
        let data = json!({
            "schema_version":1,"basis":"monotonic_forward_or_review_chunk_entry_us",
            "role":self.role,"background":self.background,"outcome":self.outcome,
            "incomplete":self.outcome == "cancelled" || last.is_some_and(|a|a.body_complete_us.is_none()),
            "active_phase":self.active_phase,"native_attempt_id":self.native_attempt_id,
            "primary_lock_wait_us":self.primary_lock_wait_us,"state_load_us":self.state_load_us,
            "planning_us":self.planning_us,"blocking_review_us":self.blocking_review_us,
            "preparation_done_us":self.preparation_done_us,"attempts":self.attempts,
            "first_body_us":last.and_then(|a|a.first_body_us),
            "body_complete_us":last.and_then(|a|a.body_complete_us),
            "settlement_us":self.settlement_us,"relay_send_wait_us":self.relay_send_wait_us,
            "total_us":self.elapsed_us()
        });
        if let Some(attempt) = &self.review_attempt_id {
            logging::journal_attempt(
                &self.service,
                &self.session_id,
                &self.request_id,
                attempt,
                "phase_timing",
                &data,
            );
        } else {
            journal(
                &self.service,
                &self.session_id,
                &self.request_id,
                "phase_timing",
                &data,
            );
        }
    }
}
