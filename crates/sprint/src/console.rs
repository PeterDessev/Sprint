//! The console window of a GUI launch on Windows.
//!
//! `sprint` is a console program, so Windows opens a console window for
//! it when it is started from Explorer (double-click, file association,
//! drag and drop). In GUI mode that window would linger behind the GUI.
//! When the console belongs to this process alone it is released, and
//! errors are then shown in a message box instead. A console shared with
//! a shell (`sprint` typed in a terminal) is left alone. On other
//! platforms these functions do nothing.

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
    fn a_shared_console_is_kept() {
        // Under `cargo test` the console (if any) is shared with cargo, so
        // it must not be released.
        let count = console_process_count();
        if count != 1 {
            assert!(!release_own_console());
        }
    }
}
