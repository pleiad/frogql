//! A wall-clock budget the search consults, so a query that will not
//! finish stops instead of being waited on.
//!
//! # Why this is not a timeout
//!
//! Nothing here interrupts anything. The budget is a flag the evaluation
//! loops *ask*, and it can only be honoured where somebody asked. That
//! makes it a cooperative budget, and the distinction matters twice:
//!
//! - **Coverage is a list, not a guarantee.** The LTJ search and every
//!   vector-search walk consult it (see *Where it is honoured*). The
//!   hash-join fallback, the repetition enumerators and the shortest-path
//!   searches do not, so a query whose cost lives there runs to
//!   completion regardless. Claiming otherwise would be worse than having
//!   no budget: a sweep would report a bounded run it did not get.
//! - **Granularity is coarse on purpose.** `Instant::now()` costs tens of
//!   nanoseconds and the innermost loops run billions of times, so the
//!   clock is read once every `STRIDE` asks. A budget can therefore
//!   overrun by however long `STRIDE` iterations take.
//!
//! # What an expired budget produces
//!
//! Whatever the search had reached — a **partial** result, which is not
//! an answer. `Runtime::query_timed_out()` reports it, and every caller
//! that sets a budget must consult it and label the output. A benchmark
//! row that prints a partial result as a latency is a lie of exactly the
//! kind `stats.arm` exists to prevent, which is why nothing sets a budget
//! by default and the REPL sets none at all: a partial answer typed back
//! at a user is indistinguishable from a real one.
//!
//! # Cancellation
//!
//! The same flag answers a second question: "stop, because somebody
//! asked". An embedder registers an `AtomicBool` it can set from
//! anywhere — a signal handler, another thread — and the budget reads it
//! on the same amortised schedule. The REPL uses this for Ctrl-C, so an
//! interrupted query leaves the session alive instead of taking the
//! process with it. An interrupted result is partial for exactly the same
//! reason a timed-out one is, and `tripped` reports both.
//!
//! # Where it is honoured
//!
//! - `LtjAlgorithm::search` — per candidate at every level, so the join
//!   itself is bounded, including the descents `memo` resumes and the
//!   ones `interleave` makes inside its walk.
//! - `LtjAlgorithm::walk_ranking` — `memo`'s phase-2 ranking walk.
//! - the `Interleave` arm's per-visit ranking walk, in the same function.
//! - `post_filter`'s candidate and corpus walks, `pre_filter`'s
//!   per-neighbour re-runs, and the correlated arm's per-partition loop.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Asks between clock reads. Large enough that the read is amortised to
/// nothing, small enough that the overrun is a fraction of a second on
/// any loop worth bounding.
const STRIDE: u32 = 4096;

pub struct Budget {
    /// `None` is an unlimited deadline. With no deadline *and* no cancel
    /// flag every method short-circuits — an unbudgeted query must not
    /// pay for this at all.
    deadline: Cell<Option<Instant>>,
    /// Set by whoever wants the query to stop. Read on the same
    /// amortised schedule as the clock, so it costs the same nothing.
    cancel: std::cell::RefCell<Option<Arc<AtomicBool>>>,
    countdown: Cell<u32>,
    tripped: Cell<bool>,
}

impl Default for Budget {
    fn default() -> Self {
        Budget::unlimited()
    }
}

impl Budget {
    pub fn unlimited() -> Budget {
        Budget {
            deadline: Cell::new(None),
            cancel: std::cell::RefCell::new(None),
            countdown: Cell::new(STRIDE),
            tripped: Cell::new(false),
        }
    }

    /// Register a flag that stops the search when it turns true. `None`
    /// removes it. Whoever sets the flag is responsible for clearing it
    /// before the next query — the budget only reads it.
    pub fn set_cancel_flag(&self, flag: Option<Arc<AtomicBool>>) {
        *self.cancel.borrow_mut() = flag;
    }

    fn cancelled(&self) -> bool {
        match self.cancel.borrow().as_ref() {
            Some(f) => f.load(Ordering::Relaxed),
            None => false,
        }
    }

    /// Arm the budget for `limit` from now, or disarm it with `None`.
    /// Also clears the tripped flag, so one expired query does not report
    /// the next one as expired too.
    pub fn arm(&self, limit: Option<Duration>) {
        self.deadline.set(limit.map(|d| Instant::now() + d));
        self.countdown.set(STRIDE);
        self.tripped.set(false);
    }

    /// Restart the clock without changing the limit. Called at the top of
    /// every top-level execution so a budget set once covers each query
    /// separately rather than the session.
    pub fn restart(&self, limit: Option<Duration>) {
        self.arm(limit);
    }

    /// Has the budget run out? Reads the clock once every `STRIDE` calls;
    /// the answer is sticky once true.
    pub fn expired(&self) -> bool {
        if self.tripped.get() {
            return true;
        }
        let armed = self.deadline.get().is_some() || self.cancel.borrow().is_some();
        if !armed {
            return false;
        }
        let n = self.countdown.get();
        if n > 0 {
            self.countdown.set(n - 1);
            return false;
        }
        self.countdown.set(STRIDE);
        let out_of_time = matches!(self.deadline.get(), Some(d) if Instant::now() >= d);
        if out_of_time || self.cancelled() {
            self.tripped.set(true);
            return true;
        }
        false
    }

    /// Did the budget run out during the last execution? Free — no clock
    /// read — so a caller can consult it per result rather than per loop.
    pub fn tripped(&self) -> bool {
        self.tripped.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unlimited_budget_never_expires_and_never_reads_the_clock() {
        let b = Budget::unlimited();
        for _ in 0..(STRIDE as usize * 3) {
            assert!(!b.expired());
        }
        assert!(!b.tripped());
    }

    #[test]
    fn an_armed_budget_expires_and_stays_expired() {
        let b = Budget::unlimited();
        b.arm(Some(Duration::from_nanos(1)));
        // The clock is only read every STRIDE asks, so the first STRIDE
        // are free by construction; that is the documented granularity.
        for _ in 0..=(STRIDE as usize) {
            b.expired();
        }
        assert!(b.expired(), "past the deadline, once the clock was read");
        assert!(b.tripped());
        assert!(b.expired(), "the verdict is sticky");
    }

    #[test]
    fn arming_again_clears_the_previous_verdict() {
        let b = Budget::unlimited();
        b.arm(Some(Duration::from_nanos(1)));
        for _ in 0..=(STRIDE as usize) {
            b.expired();
        }
        assert!(b.tripped());
        b.arm(None);
        assert!(!b.tripped(), "one expired query must not taint the next");
        assert!(!b.expired());
    }

    #[test]
    fn a_generous_budget_does_not_expire() {
        let b = Budget::unlimited();
        b.arm(Some(Duration::from_secs(3600)));
        for _ in 0..(STRIDE as usize * 3) {
            assert!(!b.expired());
        }
        assert!(!b.tripped());
    }
}

#[cfg(test)]
mod cancel_tests {
    use super::*;

    #[test]
    fn a_raised_cancel_flag_stops_the_search() {
        let b = Budget::unlimited();
        let flag = Arc::new(AtomicBool::new(false));
        b.set_cancel_flag(Some(flag.clone()));
        for _ in 0..=(STRIDE as usize) {
            assert!(!b.expired());
        }
        flag.store(true, Ordering::Relaxed);
        for _ in 0..=(STRIDE as usize) {
            b.expired();
        }
        assert!(b.tripped(), "the flag must be honoured");
    }

    #[test]
    fn arming_clears_the_verdict_but_keeps_the_flag() {
        let b = Budget::unlimited();
        let flag = Arc::new(AtomicBool::new(true));
        b.set_cancel_flag(Some(flag.clone()));
        for _ in 0..=(STRIDE as usize) {
            b.expired();
        }
        assert!(b.tripped());
        // The embedder clears its own flag; re-arming clears the verdict.
        flag.store(false, Ordering::Relaxed);
        b.arm(None);
        assert!(!b.tripped());
        for _ in 0..=(STRIDE as usize) {
            assert!(!b.expired(), "a lowered flag must not keep stopping it");
        }
    }
}
