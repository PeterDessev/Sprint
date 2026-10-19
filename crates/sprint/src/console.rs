//! The console window of a GUI launch on Windows.
//!
//! `sprint` is a console program, so Windows opens a console window for
//! it when it is started from Explorer (double-click, file association,
//! drag and drop). In GUI mode that window would linger behind the GUI.
//! When the console belongs to this process alone it is released, and
//! errors and panics are then shown in a message box instead
//! ([`show_error`], [`show_panics`]). A console shared with a shell
//! (`sprint` typed in a terminal) is left alone. On other platforms these
//! functions do nothing.

use std::panic::{self, PanicHookInfo};
use std::thread;

/// Number of processes attached to this process's console (0 without a
/// console, always 0 on other platforms).
pub fn console_process_count() -> u32 {
    imp::console_process_count()
}

/// Detach from the console when no other process uses it (the window
/// then closes). Returns true when the console was released; stdout and
/// stderr are gone afterwards.
pub fn release_own_console() -> bool {
    if console_process_count() != 1 {
        return false;
    }
    imp::free_console()
}

/// Show an error to a user who has no console: a message box on Windows.
pub fn show_error(title: &str, message: &str) {
    imp::show_error(title, message);
}

/// After [`release_own_console`]: show panics in a message box as well,
/// since their report on stderr goes nowhere. The previous hook still
/// runs first. A panic on the main thread ends the program, so its box is
/// shown before the program goes on unwinding; one on another thread
/// (which the GUI may survive) is shown from a helper thread, so the
/// panicking thread does not keep the locks it holds while the box is
/// open.
pub fn show_panics(title: &'static str) {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        previous(info);
        let current = thread::current();
        let main = current.name() == Some("main");
        let message = panic_message(current.name(), main, info);
        if main {
            show_error(title, &message);
        } else {
            let _ = thread::Builder::new()
                .name("sprint-error-box".into())
                .spawn(move || show_error(title, &message));
        }
    }));
}

/// The text of a panic report for a message box.
fn panic_message(thread: Option<&str>, main: bool, info: &PanicHookInfo<'_>) -> String {
    let what = info.payload_as_str().unwrap_or("an unknown error");
    let location = info
        .location()
        .map(|l| format!("{}:{}", l.file(), l.line()));
    panic_text(thread, main, what, location.as_deref())
}

/// See [`panic_message`].
fn panic_text(thread: Option<&str>, main: bool, what: &str, location: Option<&str>) -> String {
    let mut text = if main {
        format!("Sprint stopped because of an internal error:\n\n{what}")
    } else {
        let name = thread.unwrap_or("a background thread");
        format!("Internal error in {name}:\n\n{what}")
    };
    if let Some(location) = location {
        text.push_str(&format!("\n\n(at {location})"));
    }
    text
}

#[cfg(windows)]
mod imp {
    use std::iter::once;

    use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

    pub fn console_process_count() -> u32 {
        let mut pids = [0u32; 4];
        let len = u32::try_from(pids.len()).unwrap_or(1);
        // SAFETY: `pids` is valid for writes of `len` u32 values. When it is
        // too small the call writes nothing and returns the count needed,
        // which is all we want.
        unsafe { GetConsoleProcessList(pids.as_mut_ptr(), len) }
    }

    pub fn free_console() -> bool {
        // SAFETY: FreeConsole has no preconditions; afterwards the standard
        // handles are invalid, which Rust's stdio treats as a sink.
        unsafe { FreeConsole() != 0 }
    }

    /// A NUL-terminated UTF-16 copy of `text` (interior NULs become spaces).
    fn wide(text: &str) -> Vec<u16> {
        text.replace('\0', " ")
            .encode_utf16()
            .chain(once(0))
            .collect()
    }

    pub fn show_error(title: &str, message: &str) {
        let title = wide(title);
        let message = wide(message);
        // SAFETY: both strings are NUL-terminated UTF-16 buffers that live
        // across the call; a null owner window is allowed.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                message.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn wide_strings_are_terminated() {
            assert_eq!(wide("ok"), vec![u16::from(b'o'), u16::from(b'k'), 0]);
            assert_eq!(wide("a\0b"), vec![97, 32, 98, 0]);
            assert_eq!(wide(""), vec![0]);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn console_process_count() -> u32 {
        0
    }

    pub fn free_console() -> bool {
        false
    }

    pub fn show_error(_title: &str, _message: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_reports_name_the_error_and_where() {
        assert_eq!(
            panic_text(Some("main"), true, "boom", Some("src/app.rs:12")),
            "Sprint stopped because of an internal error:\n\nboom\n\n(at src/app.rs:12)"
        );
        assert_eq!(
            panic_text(Some("sprint-load"), false, "bad file", None),
            "Internal error in sprint-load:\n\nbad file"
        );
        assert_eq!(
            panic_text(None, false, "x", None),
            "Internal error in a background thread:\n\nx"
        );
    }

    #[test]
    fn a_shared_console_is_kept() {
        // Under `cargo test` the console (if any) is shared with cargo, so
        // it must not be released.
        let count = console_process_count();
        if count != 1 {
            assert!(!release_own_console());
        }
    }
}
