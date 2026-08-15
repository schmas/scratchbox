//! Helpers for tests that wait on filesystem events.
//!
//! Everything here polls against a deadline. Fixed `sleep()` calls are what rot
//! filesystem-watcher suites: they are simultaneously too short on a loaded CI runner and
//! too long everywhere else.

#![allow(dead_code)]

use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use scratchbox_core::StoreEvent;

/// Generous on purpose: the debounce window is 500ms and CI runners stall.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait before concluding that nothing is coming.
///
/// Must comfortably exceed the debounce window, or the test proves only that the event had
/// not arrived yet.
pub const QUIET: Duration = Duration::from_millis(1500);

/// Wait for an event matching `predicate`, discarding events that do not.
///
/// Returns `None` if the deadline passes first.
pub fn wait_for(
    rx: &Receiver<StoreEvent>,
    predicate: impl Fn(&StoreEvent) -> bool,
) -> Option<StoreEvent> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        match rx.recv_timeout(remaining) {
            Ok(event) if predicate(&event) => return Some(event),
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
}

/// Every event that arrives within [`QUIET`].
///
/// Used to prove both that something happened *once* and that nothing else followed it.
pub fn collect(rx: &Receiver<StoreEvent>) -> Vec<StoreEvent> {
    let deadline = Instant::now() + QUIET;
    let mut events = Vec::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(event) => events.push(event),
            Err(_) => break,
        }
    }
    events
}

/// Wait until the workspace stops reporting anything, discarding whatever arrives.
///
/// Test setup writes files, and those writes generate events that can surface well after
/// the setup call returns — FSEvents in particular runs a second or more behind. Draining
/// to quiet before the part of the test that matters is what stops a stale setup event
/// from being read as the event under test.
pub fn settle(rx: &Receiver<StoreEvent>) {
    while !collect(rx).is_empty() {}
}

/// Assert that nothing arrives. The message names the operation that should have been
/// silent, because a bare "expected 0 events" tells you nothing at 3am.
pub fn expect_silence(rx: &Receiver<StoreEvent>, what: &str) {
    let events = collect(rx);
    assert!(
        events.is_empty(),
        "{what} should produce no events, got {events:?}"
    );
}
