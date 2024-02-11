use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};

enum EchoState {
    STDOUT = 0,
    STDERR = 1,
    QUIET = 2,
}

impl From<u8> for EchoState {
    fn from(v: u8) -> Self {
        match v {
            0 => EchoState::STDOUT,
            1 => EchoState::STDERR,
            2 => EchoState::QUIET,
            _ => panic!("invalid echo state"),
        }
    }
}

static ECHO_STATE: AtomicU8 = AtomicU8::new(EchoState::STDOUT as u8);

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    // use eprintln and println so that tests can still intercept this
    match EchoState::from(ECHO_STATE.load(Ordering::Relaxed)) {
        EchoState::STDOUT => {
            println!("{}", args);
        }
        EchoState::STDERR => {
            eprintln!("{}", args);
        }
        EchoState::QUIET => {}
    }
}

/// Until the guard is dropped, echo goes to stderr.
pub fn redirect_to_stderr(yes: bool) -> RedirectGuard {
    let old = ECHO_STATE.load(Ordering::Relaxed);
    let state = if yes {
        EchoState::STDERR
    } else {
        EchoState::STDOUT
    };
    ECHO_STATE.store(state as u8, Ordering::Relaxed);
    RedirectGuard(old)
}

pub fn quiet(yes: bool) -> RedirectGuard {
    let old = ECHO_STATE.load(Ordering::Relaxed);
    if yes {
        ECHO_STATE.store(EchoState::QUIET as u8, Ordering::Relaxed);
    } else {
        ECHO_STATE.store(EchoState::STDOUT as u8, Ordering::Relaxed);
    }
    ECHO_STATE.store(EchoState::QUIET as u8, Ordering::Relaxed);
    RedirectGuard(old)
}

#[must_use]
pub struct RedirectGuard(u8);

impl Drop for RedirectGuard {
    fn drop(&mut self) {
        ECHO_STATE.store(self.0, Ordering::Relaxed);
    }
}

/// Echo a line to the output stream (usually stdout).
macro_rules! echo {
    () => {
        $crate::tui::_print(format_args!(""))
    };
    ($($arg:tt)+) => {
        // TODO: this is bloaty, but this way capturing of outputs
        // for stdout works in tests still.
        $crate::tui::_print(format_args!($($arg)*))
    }
}

/// Like echo but always goes to stderr.
macro_rules! elog {
    ($($arg:tt)*) => { eprintln!($($arg)*) }
}

/// Emits a warning
macro_rules! warn {
    ($($arg:tt)+) => {
        elog!(
            "{} {}",
            console::style("warning:").yellow().bold(),
            format_args!($($arg)*)
        )
    }
}

/// Logs errors
macro_rules! error {
    ($($arg:tt)+) => {
        elog!(
            "{} {}",
            console::style("error:").red().bold(),
            format_args!($($arg)*)
        )
    }
}
