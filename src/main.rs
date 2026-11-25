//! Alias Calc: a tiny, instant calculator for aliasOS.
//!
//! One thread, no toolkit, no GPU: a plain Wayland client that draws on the
//! CPU into a shared-memory buffer and sleeps until something happens.

mod engine;
mod input;
mod render;
mod theme;
mod trace;
mod ui;
mod window;

use std::process::ExitCode;

fn main() -> ExitCode {
    trace::start();
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => {
            println!("alias-calc {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some("--help" | "-h") => {
            println!("Usage: alias-calc\n\nSet ALIAS_CALC_TRACE=1 to print startup timings.");
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("alias-calc: unknown argument {other:?} (try --help)");
            return ExitCode::from(2);
        }
        None => {}
    }

    match window::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("alias-calc: {err}");
            ExitCode::FAILURE
        }
    }
}
