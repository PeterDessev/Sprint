//! Native file dialogs (rfd) that keep the window responsive.
//!
//! The dialogs are synchronous. On Windows and Linux they run on a worker
//! thread and the answer arrives through a channel, so the UI keeps
//! repainting (print progress, temperatures) while a dialog is open. macOS
//! only allows them on the main thread, where they run inline.

use std::path::{Path, PathBuf};

use crossbeam_channel::Receiver;

/// Run `dialog` off the UI thread where the platform allows it; the result
/// arrives on the returned channel (and `repaint` is asked for a frame).
/// When no thread can be started the channel is disconnected without a
/// value, which callers treat like a cancelled dialog.
pub fn spawn<T: Send + 'static>(
    repaint: Option<egui::Context>,
    dialog: impl FnOnce() -> T + Send + 'static,
) -> Receiver<T> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let run = move || {
        let _ = tx.send(dialog());
        if let Some(ctx) = repaint {
            ctx.request_repaint();
        }
    };
    if cfg!(target_os = "macos") {
        run();
    } else if let Err(e) = std::thread::Builder::new()
        .name("sprint-gui-dialog".to_owned())
        .spawn(run)
    {
        log::warn!("could not start a thread for a file dialog: {e}");
    }
    rx
}

/// The "Open G-code" dialog, starting in `directory` when given.
pub fn pick_gcode_file(directory: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new()
        .set_title("Open G-code")
        .add_filter("G-code", crate::loader::GCODE_EXTENSIONS)
        .add_filter("All files", &["*"]);
    if let Some(dir) = directory.filter(|d| d.is_dir()) {
        dialog = dialog.set_directory(dir);
    }
    dialog.pick_file()
}

/// A "Save as" dialog for a text file.
pub fn save_text_file(title: &str, file_name: &str) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(title)
        .set_file_name(file_name)
        .add_filter("Text", &["txt", "log"])
        .save_file()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn results_arrive_through_the_channel() {
        let rx = spawn(None, || 42);
        assert_eq!(rx.recv_timeout(Duration::from_secs(10)), Ok(42));
    }
}
