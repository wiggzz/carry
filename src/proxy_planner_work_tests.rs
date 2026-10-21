//! Test-only counters at actual production work seams, isolated per test thread.
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

thread_local! {
    static COUNTS: RefCell<Option<BTreeMap<&'static str, u64>>> = const { RefCell::new(None) };
    static PRIMARY_SCOPE: Cell<Option<&'static str>> = const { Cell::new(None) };
}

pub(crate) fn record(operation: &'static str) {
    COUNTS.with(|counts| {
        if let Some(counts) = counts.borrow_mut().as_mut() {
            *counts.entry(operation).or_default() += 1;
            if operation == "primary_renders" {
                PRIMARY_SCOPE.with(|scope| {
                    if let Some(scope) = scope.get() {
                        *counts.entry(scope).or_default() += 1;
                    }
                });
            }
        }
    });
}

pub(super) struct PrimaryScope(Option<&'static str>);

impl Drop for PrimaryScope {
    fn drop(&mut self) {
        PRIMARY_SCOPE.with(|scope| scope.set(self.0));
    }
}

// Scope only the explicit raw renders, never the intervening forecasts/plans.
pub(super) fn primary_scope(name: &'static str) -> PrimaryScope {
    PrimaryScope(PRIMARY_SCOPE.with(|scope| scope.replace(Some(name))))
}

pub(super) fn start() {
    COUNTS.with(|counts| {
        assert!(counts.borrow().is_none());
        *counts.borrow_mut() = Some(BTreeMap::new());
    });
}

pub(super) fn finish() -> BTreeMap<&'static str, u64> {
    COUNTS.with(|counts| counts.borrow_mut().take().unwrap())
}
