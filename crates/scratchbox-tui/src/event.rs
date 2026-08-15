//! One stream out of two sources: the keyboard and the store.

use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Select, bounded};
use crossterm::event::Event as TerminalEvent;
use scratchbox_core::StoreEvent;

#[derive(Debug)]
pub enum AppEvent {
    Terminal(TerminalEvent),
    Store(StoreEvent),
    /// The deadline passed with nothing to report — the autosave timer firing.
    Tick,
}

pub struct Events {
    terminal: Receiver<TerminalEvent>,
    store: Receiver<StoreEvent>,
}

impl Events {
    pub fn new(store: Receiver<StoreEvent>) -> Self {
        Self {
            terminal: spawn_reader(),
            store,
        }
    }

    /// Wait for the next event.
    ///
    /// `timeout` of `None` blocks indefinitely, which is the normal state: with nothing
    /// waiting to be saved there is no reason to wake up, and an idle scratchpad should
    /// cost nothing. A caller with unsaved work passes its deadline and gets [`AppEvent::Tick`]
    /// when it arrives.
    ///
    /// Returns `None` once both sources are gone, which is how the loop ends.
    pub fn next(&self, timeout: Option<Duration>) -> Option<AppEvent> {
        let mut select = Select::new();
        let terminal = select.recv(&self.terminal);
        let store = select.recv(&self.store);

        let operation = match timeout {
            Some(timeout) => match select.select_timeout(timeout) {
                Ok(operation) => operation,
                Err(_) => return Some(AppEvent::Tick),
            },
            None => select.select(),
        };

        match operation.index() {
            index if index == terminal => {
                operation.recv(&self.terminal).ok().map(AppEvent::Terminal)
            }
            index if index == store => operation.recv(&self.store).ok().map(AppEvent::Store),
            _ => None,
        }
    }
}

/// Read the keyboard on its own thread.
///
/// `read()` blocks, which is the point: no polling loop, no timer, nothing burning CPU
/// while the user thinks.
fn spawn_reader() -> Receiver<TerminalEvent> {
    // Small but not zero: a burst of paste events should not block the reader on a UI that
    // is mid-render, and an unbounded queue would just hide a stall.
    let (tx, rx) = bounded(64);

    thread::spawn(move || {
        while let Ok(event) = crossterm::event::read() {
            if tx.send(event).is_err() {
                break;
            }
        }
    });

    rx
}
