//! Test-only counters at actual production work seams, isolated per test thread.
use std::cell::RefCell;
use std::collections::BTreeMap;

thread_local! {
    static COUNTS: RefCell<Option<BTreeMap<&'static str, u64>>> = const { RefCell::new(None) };
}

pub(super) fn record(operation: &'static str) {
    COUNTS.with(|counts| {
        if let Some(counts) = counts.borrow_mut().as_mut() {
            *counts.entry(operation).or_default() += 1;
        }
    });
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
