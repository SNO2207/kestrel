//! For the end-to-end tests: a query held back on its way in, so that a query its page sent
//! after it gets there first.
//!
//! `palette_query` runs on a pool of threads, so two queries a palette sends in quick
//! succession can reach [`Palette::query_sent`](super::Palette::query_sent) in either order;
//! under load the older one sometimes arrived second and cancelled the newer, whose answer is
//! the one the palette waits for. `palette_dev_late` (development builds only) arranges that
//! order on purpose: the next query for a given text waits before it is registered until
//! another query has been registered, and that other query waits in turn, before it searches,
//! until the held one has been through, so the held one finds it still running.

use std::time::Duration;

use parking_lot::{Condvar, Mutex};

/// The longest either query waits for the other, so a test that goes wrong never hangs one.
const WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Off,
    /// The next query for the text is to be held back.
    Armed,
    /// It is waiting for another query to be registered.
    Holding,
    /// Another query has been registered and waits for the held one to go through.
    Overtaken,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Off => "off",
            Phase::Armed => "armed",
            Phase::Holding => "holding",
            Phase::Overtaken => "overtaken",
        }
    }
}

pub struct Late {
    state: Mutex<(Phase, String)>,
    changed: Condvar,
}

impl Default for Late {
    fn default() -> Self {
        Self { state: Mutex::new((Phase::Off, String::new())), changed: Condvar::new() }
    }
}

impl Late {
    /// Holds the next query for `text` back; `None` lets go of any hold. Returns the phase.
    pub fn arm(&self, text: Option<String>) -> &'static str {
        let mut state = self.state.lock();
        *state = match text {
            Some(text) => (Phase::Armed, text),
            None => (Phase::Off, String::new()),
        };
        self.changed.notify_all();
        state.0.name()
    }

    /// Where the hold is: `off`, `armed`, `holding` (the query is waiting) or `overtaken`.
    pub fn phase(&self) -> &'static str {
        self.state.lock().0.name()
    }

    /// Called by every query before it is registered: the one held back waits here until
    /// another query has been registered, and `true` says it was held.
    pub fn arrive(&self, text: &str) -> bool {
        if !cfg!(debug_assertions) {
            return false;
        }
        let mut state = self.state.lock();
        if state.0 != Phase::Armed || state.1 != text {
            return false;
        }
        state.0 = Phase::Holding;
        self.changed.notify_all();
        let _ = self.changed.wait_while_for(&mut state, |(phase, _)| *phase == Phase::Holding, WAIT);
        true
    }

    /// Called by the held query once it has been registered (or refused).
    pub fn through(&self) {
        let mut state = self.state.lock();
        *state = (Phase::Off, String::new());
        self.changed.notify_all();
    }

    /// Called by every other query once it has been registered: while a query is held back,
    /// lets it go and waits until it has been through.
    pub fn overtake(&self) {
        if !cfg!(debug_assertions) {
            return;
        }
        let mut state = self.state.lock();
        if state.0 != Phase::Holding {
            return;
        }
        state.0 = Phase::Overtaken;
        self.changed.notify_all();
        let _ = self.changed.wait_while_for(&mut state, |(phase, _)| *phase == Phase::Overtaken, WAIT);
    }
}
