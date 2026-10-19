//! Ctrl+C as a request instead of the end of the process.
//!
//! The line shell and the headless commands stream to a printer; being
//! killed by Ctrl+C would cut a print off mid-move. [`global`] installs a
//! handler (once per process) that only counts the presses, and long
//! operations poll the count: the shell stops `monitor` or a wait, `sprint
//! print` cancels the job on the first press and gives up on the second.
//!
//! While a line editor reads a line the terminal is in raw mode and Ctrl+C
//! arrives as a key instead.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

/// Counts Ctrl+C presses (or simulated ones in tests).
#[derive(Clone, Debug, Default)]
pub struct Interrupt {
    count: Arc<AtomicU32>,
}

impl Interrupt {
    /// A counter that only [`Interrupt::raise`] increments (tests, or
    /// programs without a handler).
    pub fn new() -> Interrupt {
        Interrupt::default()
    }

    /// Record one press.
    pub fn raise(&self) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }

    /// Presses since the last [`Interrupt::reset`] or [`Interrupt::take`].
    pub fn count(&self) -> u32 {
        self.count.load(Ordering::SeqCst)
    }

    /// Whether Ctrl+C was pressed (without consuming it).
    pub fn is_raised(&self) -> bool {
        self.count() > 0
    }

    /// Whether Ctrl+C was pressed, consuming the presses.
    pub fn take(&self) -> bool {
        self.count.swap(0, Ordering::SeqCst) > 0
    }

    /// Forget earlier presses (before starting something interruptible).
    pub fn reset(&self) {
        self.count.store(0, Ordering::SeqCst);
    }
}

/// The process-wide counter, fed by a Ctrl+C handler installed on first
/// use. When the handler cannot be installed (another one exists), Ctrl+C
/// keeps its default meaning and the counter only moves through
/// [`Interrupt::raise`].
pub fn global() -> Interrupt {
    static GLOBAL: OnceLock<Interrupt> = OnceLock::new();
    GLOBAL
        .get_or_init(|| {
            let interrupt = Interrupt::new();
            let handler = interrupt.clone();
            if let Err(e) = ctrlc::set_handler(move || handler.raise()) {
                log::warn!("cannot handle Ctrl+C: {e}");
            }
            interrupt
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presses_are_counted_and_taken() {
        let interrupt = Interrupt::new();
        assert!(!interrupt.is_raised());
        interrupt.raise();
        interrupt.clone().raise();
        assert_eq!(interrupt.count(), 2);
        assert!(interrupt.take());
        assert!(!interrupt.take());
        interrupt.raise();
        interrupt.reset();
        assert_eq!(interrupt.count(), 0);
    }
}
