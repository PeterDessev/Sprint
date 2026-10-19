//! CSV export off the UI thread: the native save dialog and the file write
//! run on a worker thread; the tab polls for the outcome each frame.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::task::{Context, Poll, Wake, Waker};

/// How an export ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Written to this file.
    Saved(PathBuf),
    /// The user closed the dialog.
    Cancelled,
    /// Writing failed (message for the user).
    Failed(String),
}

/// A running export.
#[derive(Debug)]
pub(crate) struct Export {
    rx: Receiver<Outcome>,
}

impl Export {
    /// Ask where to save `csv` (suggesting `file_name`) and write it, on a
    /// worker thread. `ctx` is repainted when it is done.
    pub(crate) fn start(ctx: &egui::Context, csv: String, file_name: String) -> Export {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("sprint-csv-export".into())
            .spawn({
                let tx = tx.clone();
                move || {
                    let outcome = ask_and_save(&csv, &file_name);
                    let _ = tx.send(outcome);
                    ctx.request_repaint();
                }
            });
        if let Err(err) = spawned {
            let _ = tx.send(Outcome::Failed(format!(
                "could not start the export: {err}"
            )));
        }
        Export { rx }
    }

    /// The outcome, once there is one.
    pub(crate) fn poll(&self) -> Option<Outcome> {
        match self.rx.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Outcome::Failed(
                "the export stopped unexpectedly".to_owned(),
            )),
        }
    }
}

/// Show the save dialog, then write the file.
fn ask_and_save(csv: &str, file_name: &str) -> Outcome {
    let dialog = rfd::AsyncFileDialog::new()
        .set_title("Export temperature history")
        .set_file_name(file_name)
        .add_filter("CSV", &["csv"]);
    match block_on(dialog.save_file()) {
        None => Outcome::Cancelled,
        Some(handle) => write_csv(handle.path(), csv),
    }
}

/// Write `csv` to `path`.
pub(crate) fn write_csv(path: &Path, csv: &str) -> Outcome {
    match std::fs::write(path, csv) {
        Ok(()) => Outcome::Saved(path.to_path_buf()),
        Err(err) => Outcome::Failed(format!("{}: {err}", path.display())),
    }
}

/// Run a future to completion on this (worker) thread.
fn block_on<F: Future>(future: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        std::thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_on_runs_ready_and_woken_futures() {
        assert_eq!(block_on(async { 7 }), 7);
        // A future that wakes itself once before finishing.
        struct Twice(bool);
        impl Future for Twice {
            type Output = &'static str;
            fn poll(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                if self.0 {
                    Poll::Ready("done")
                } else {
                    self.0 = true;
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
        }
        assert_eq!(block_on(Twice(false)), "done");
    }

    #[test]
    fn writes_and_reports_failures() {
        let dir = std::env::temp_dir().join(format!("sprint-gui-csv-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("history.csv");
        assert_eq!(write_csv(&path, "time_s\n"), Outcome::Saved(path.clone()));
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some("time_s\n")
        );
        // A directory cannot be written as a file.
        assert!(matches!(write_csv(&dir, "x"), Outcome::Failed(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dropped_worker_is_a_failure() {
        let (tx, rx) = mpsc::channel();
        drop(tx);
        let export = Export { rx };
        assert!(matches!(export.poll(), Some(Outcome::Failed(_))));
        let (tx, rx) = mpsc::channel::<Outcome>();
        let export = Export { rx };
        assert_eq!(export.poll(), None);
        let _ = tx.send(Outcome::Cancelled);
        assert_eq!(export.poll(), Some(Outcome::Cancelled));
    }
}
