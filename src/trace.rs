//! Startup timings, printed to stderr when `ALIAS_CALC_TRACE` is set.
//!
//! Each mark prints the milliseconds since `main` started, so a launch shows
//! where its time goes (connect, globals, first configure, first commit).

use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Option<Instant>> = OnceLock::new();

/// Starts the clock. Call first thing in `main`.
pub fn start() {
    START.get_or_init(|| std::env::var_os("ALIAS_CALC_TRACE").map(|_| Instant::now()));
}

/// Whether `ALIAS_CALC_TRACE` is set, so callers can skip formatting.
pub fn on() -> bool {
    matches!(START.get(), Some(Some(_)))
}

/// Prints `what` with the time since `start`, if tracing is on.
pub fn mark(what: &str) {
    if let Some(Some(start)) = START.get() {
        eprintln!(
            "[trace] {:>7.3} ms  {what}",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}
