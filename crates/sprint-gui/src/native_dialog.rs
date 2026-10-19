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

/// The editor command for [`open_in_editor`]: `SPRINT_EDITOR`, `VISUAL` or
/// `EDITOR` (a program and its arguments, split at spaces outside double
/// quotes), else the platform's text editor.
pub fn editor_command() -> Vec<String> {
    let configured = ["SPRINT_EDITOR", "VISUAL", "EDITOR"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|value| command_words(&value))
        .find(|words| !words.is_empty());
    configured.unwrap_or_else(|| {
        let default: &[&str] = if cfg!(windows) {
            &["notepad.exe"]
        } else if cfg!(target_os = "macos") {
            &["open", "-t"]
        } else {
            &["xdg-open"]
        };
        default.iter().map(|s| (*s).to_owned()).collect()
    })
}

/// The program and arguments of an editor setting: words split at spaces,
/// double-quoted words kept whole (`"C:\Program Files\…\editor.exe" -n`),
/// and an unquoted path to an existing program taken as it is.
fn command_words(value: &str) -> Vec<String> {
    let value = value.trim();
    if !value.is_empty() && Path::new(value).is_file() {
        return vec![value.to_owned()];
    }
    let mut words = Vec::new();
    let mut word = String::new();
    // Inside a word (an empty quoted word "" counts).
    let mut in_word = false;
    let mut quoted = false;
    for c in value.chars() {
        if c == '"' {
            quoted = !quoted;
            in_word = true;
        } else if c.is_whitespace() && !quoted {
            if in_word {
                words.push(std::mem::take(&mut word));
                in_word = false;
            }
        } else {
            word.push(c);
            in_word = true;
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

/// Open `path` in the external text editor (see [`editor_command`])
/// without waiting for it: a worker thread waits for the editor to exit.
pub fn open_in_editor(path: &Path) -> std::io::Result<()> {
    let command = editor_command();
    let Some((program, args)) = command.split_first() else {
        return Err(std::io::Error::other("no editor configured"));
    };
    let mut child = std::process::Command::new(program)
        .args(args)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // Reap it when it exits (a dropped child is never waited for).
    let _ = std::thread::Builder::new()
        .name("sprint-gui-editor".to_owned())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(())
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

    #[test]
    fn editor_settings_keep_paths_with_spaces_whole() {
        let words = |value: &str| command_words(value);
        assert_eq!(words("code --wait"), ["code", "--wait"]);
        assert_eq!(words("  "), Vec::<String>::new());
        // Quoted, as Windows paths under Program Files are written.
        assert_eq!(
            words(r#""C:\Program Files\Notepad++\notepad++.exe" -multiInst"#),
            [r"C:\Program Files\Notepad++\notepad++.exe", "-multiInst"]
        );
        assert_eq!(words(r#"ed "two words""#), ["ed", "two words"]);
        // An unquoted path to an existing program, spaces and all.
        let dir = std::env::temp_dir().join(format!(
            "sprint-gui-editor-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let program = dir.join("my editor.exe");
        std::fs::write(&program, "").expect("program");
        let value = program.to_string_lossy().into_owned();
        assert_eq!(words(&value), [value]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
