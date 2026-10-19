//! Where shell lines come from: the line editor on a terminal (history,
//! completion), standard input otherwise (scripts and pipes), or a list of
//! lines in tests.

use std::borrow::Cow;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::Sender;

use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::{FileHistory, History};
use rustyline::validate::Validator;
use rustyline::{CompletionType, Config, Context, Editor, ExternalPrinter, Helper};
use sprint_host::{BAUD_RATES, list_ports};

use super::output::Sink;
use crate::commands::{COMMANDS, command_spec, looks_like_gcode};
use crate::paths::complete_path;
use crate::report::{ReportLine, Tone};
use crate::theme::ColorSupport;
use crate::widgets::gcode_candidates;

/// History entries kept.
const HISTORY_SIZE: usize = 1000;

/// What reading a line gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Read {
    /// A line (without its line break).
    Line(String),
    /// Ctrl+C at the prompt.
    Interrupted,
    /// End of input (Ctrl+D, Ctrl+Z on Windows, end of a script).
    Eof,
}

/// A source of shell lines.
pub(crate) trait LineSource {
    /// The next line, showing `prompt` when interactive.
    fn read(&mut self, prompt: &str) -> Read;
    /// Whether a person types the lines (prompts, confirmations, history).
    fn interactive(&self) -> bool;
    /// Remember a line in the history.
    fn add_history(&mut self, _line: &str) {}
    /// Store the history (on exit).
    fn save_history(&mut self) {}
}

/// Lines given up front (`sprint shell -e`, tests).
pub(crate) struct Script {
    lines: std::collections::VecDeque<String>,
    interactive: bool,
}

impl Script {
    /// `lines` in order; `interactive` makes the shell ask confirmations
    /// (answered by the following lines).
    pub(crate) fn new<I, S>(lines: I, interactive: bool) -> Script
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Script {
            lines: lines.into_iter().map(Into::into).collect(),
            interactive,
        }
    }
}

impl LineSource for Script {
    fn read(&mut self, _prompt: &str) -> Read {
        self.lines.pop_front().map_or(Read::Eof, Read::Line)
    }

    fn interactive(&self) -> bool {
        self.interactive
    }
}

/// Standard input that is not a terminal: one command per line, no
/// prompt.
pub(crate) struct Stdin {
    reader: Box<dyn BufRead + Send>,
}

impl Stdin {
    pub(crate) fn new() -> Stdin {
        Stdin {
            reader: Box::new(std::io::BufReader::new(std::io::stdin())),
        }
    }
}

impl LineSource for Stdin {
    fn read(&mut self, _prompt: &str) -> Read {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) | Err(_) => Read::Eof,
            Ok(_) => Read::Line(line.trim_end_matches(['\n', '\r']).to_string()),
        }
    }

    fn interactive(&self) -> bool {
        false
    }
}

/// The line editor on a terminal.
pub(crate) struct Terminal {
    editor: Editor<ShellHelper, FileHistory>,
    history: Option<PathBuf>,
    /// The editor is reading a line (output then goes above the prompt).
    reading: Arc<AtomicBool>,
}

impl Terminal {
    /// An editor completing with `helper`, keeping its history in
    /// `history` (loaded now, saved by [`LineSource::save_history`]).
    pub(crate) fn new(
        helper: ShellHelper,
        history: Option<PathBuf>,
    ) -> rustyline::Result<Terminal> {
        let config = Config::builder()
            .max_history_size(HISTORY_SIZE)?
            .history_ignore_dups(true)?
            .history_ignore_space(true)
            .completion_type(CompletionType::List)
            .auto_add_history(false)
            .build();
        let mut editor = Editor::with_config(config)?;
        editor.set_helper(Some(helper));
        if let Some(path) = &history
            && path.exists()
            && let Err(e) = editor.load_history(path)
        {
            log::warn!("cannot read the shell history {}: {e}", path.display());
        }
        Ok(Terminal {
            editor,
            history,
            reading: Arc::new(AtomicBool::new(false)),
        })
    }

    /// A sink printing above the prompt while a line is edited.
    pub(crate) fn sink(&mut self) -> rustyline::Result<Box<dyn Sink>> {
        let printer = self.editor.create_external_printer()?;
        Ok(Box::new(EditorSink::new(
            printer,
            Arc::clone(&self.reading),
            Box::new(std::io::stdout()),
        )))
    }
}

impl LineSource for Terminal {
    fn read(&mut self, prompt: &str) -> Read {
        self.reading.store(true, Ordering::SeqCst);
        let result = self.editor.readline(prompt);
        self.reading.store(false, Ordering::SeqCst);
        match result {
            Ok(line) => Read::Line(line),
            Err(ReadlineError::Interrupted) => Read::Interrupted,
            Err(ReadlineError::Eof) => Read::Eof,
            Err(e) => {
                log::warn!("cannot read the terminal: {e}");
                Read::Eof
            }
        }
    }

    fn interactive(&self) -> bool {
        true
    }

    fn add_history(&mut self, line: &str) {
        let _ = self.editor.add_history_entry(line);
    }

    fn save_history(&mut self) {
        let Some(path) = &self.history else {
            return;
        };
        if self.editor.history().is_empty() {
            return;
        }
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = self.editor.save_history(path) {
            log::warn!("cannot save the shell history {}: {e}", path.display());
        }
    }
}

/// Output for an interactive shell: above the prompt while the editor
/// reads a line, straight to the terminal otherwise.
///
/// The editor's external printer may block until the editor takes a line
/// (it holds one at a time, and one left over when a line was entered
/// waits for the next prompt). Writers hold the shell's output lock, so
/// the printer is driven from a thread of its own: writing never waits,
/// and a stuck line cannot stop the shell or its echo thread.
struct EditorSink {
    /// Lines for the editor's printer, in order.
    queue: Sender<String>,
    reading: Arc<AtomicBool>,
    direct: Box<dyn Write + Send>,
}

impl EditorSink {
    fn new<P: ExternalPrinter + Send + 'static>(
        mut printer: P,
        reading: Arc<AtomicBool>,
        direct: Box<dyn Write + Send>,
    ) -> EditorSink {
        let (queue, lines) = crossbeam_channel::unbounded::<String>();
        let spawned = std::thread::Builder::new()
            .name("sprint-shell-printer".into())
            .spawn(move || {
                while let Ok(first) = lines.recv() {
                    // Everything queued meanwhile goes in one piece.
                    let mut text = first;
                    text.extend(lines.try_iter());
                    if printer.print(text).is_err() {
                        break;
                    }
                }
            });
        if let Err(e) = spawned {
            // The receiver is gone with the closure: queued lines are
            // dropped and everything is written directly instead.
            log::warn!("cannot start the shell's output thread: {e}");
        }
        EditorSink {
            queue,
            reading,
            direct,
        }
    }
}

impl Sink for EditorSink {
    fn write(&mut self, text: &str) {
        if self.reading.load(Ordering::SeqCst) && self.queue.send(text.to_string()).is_ok() {
            return;
        }
        let _ = self.direct.write_all(text.as_bytes());
        let _ = self.direct.flush();
    }
}

/// Completion and prompt colors for the line editor.
pub(crate) struct ShellHelper {
    /// Temperature preset names (`preheat`).
    pub presets: Vec<String>,
    /// Macro names (`macro`).
    pub macros: Vec<String>,
    /// Directory relative paths are completed in.
    pub base: PathBuf,
    /// Colors for the prompt.
    pub support: ColorSupport,
}

impl Completer for ShellHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        Ok(complete_line(line, pos, self))
    }
}

impl Hinter for ShellHelper {
    type Hint = String;
}

impl Highlighter for ShellHelper {
    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        _default: bool,
    ) -> Cow<'b, str> {
        if self.support.has_color() {
            Cow::Owned(ReportLine::toned(prompt, Tone::Accent).ansi(self.support))
        } else {
            Cow::Borrowed(prompt)
        }
    }
}

impl Validator for ShellHelper {}

impl Helper for ShellHelper {}

fn pair(display: impl Into<String>, replacement: impl Into<String>) -> Pair {
    Pair {
        display: display.into(),
        replacement: replacement.into(),
    }
}

/// Words starting with `prefix` (case-insensitive) as candidates.
fn words<'a>(prefix: &str, options: impl IntoIterator<Item = &'a str>) -> Vec<Pair> {
    let lower = prefix.to_lowercase();
    options
        .into_iter()
        .filter(|o| o.to_lowercase().starts_with(&lower))
        .map(|o| pair(o, o))
        .collect()
}

/// Completions for `line` with the cursor at `pos`: the start of the text
/// they replace and the candidates.
pub(crate) fn complete_line(line: &str, pos: usize, helper: &ShellHelper) -> (usize, Vec<Pair>) {
    let pos = pos.min(line.len());
    let Some(before) = line.get(..pos) else {
        return (pos, Vec::new());
    };
    let lead = before.len() - before.trim_start().len();
    let first_end = before[lead..].find(char::is_whitespace).map(|i| lead + i);
    let Some(first_end) = first_end else {
        // The command word itself.
        let word = &before[lead..];
        let mut out: Vec<Pair> = if looks_like_gcode(word) {
            Vec::new()
        } else {
            COMMANDS
                .iter()
                .filter(|c| c.name.starts_with(&word.to_ascii_lowercase()))
                .map(|c| pair(c.name, c.name))
                .collect()
        };
        if out.is_empty() && !word.is_empty() {
            out = gcode_candidates(word)
                .into_iter()
                .map(|c| pair(format!("{}  {}", c.insert, c.description), c.insert))
                .collect();
        }
        return (lead, out);
    };
    let command = &before[lead..first_end];
    let Some(spec) = command_spec(command) else {
        return (pos, Vec::new());
    };
    let args_start =
        first_end + (before[first_end..].len() - before[first_end..].trim_start().len());
    let args = &before[args_start..];
    // The word under the cursor and how many words precede it (a space may
    // be several bytes long, such as a no-break space).
    let word_start = args
        .char_indices()
        .rfind(|(_, c)| c.is_whitespace())
        .map_or(args_start, |(i, c)| args_start + i + c.len_utf8());
    let word = &before[word_start..];
    let index = args[..word_start - args_start].split_whitespace().count();
    let candidates = match (spec.name, index) {
        // The whole rest of the line is the path.
        ("load", _) => {
            let paths = complete_path(args, &helper.base, true);
            return (
                args_start,
                paths
                    .into_iter()
                    .map(|p| pair(p.name, p.completion))
                    .collect(),
            );
        }
        ("sd", 0) => words(word, ["ls", "print", "upload", "rm"]),
        ("sd", 1) if args.trim_start().to_ascii_lowercase().starts_with("upload") => {
            complete_path(word, &helper.base, true)
                .into_iter()
                .map(|p| pair(p.name, p.completion))
                .collect()
        }
        ("connect", 0) => {
            let ports = list_ports();
            let mut options = vec![
                "sim".to_string(),
                "sim://".to_string(),
                "tcp://".to_string(),
            ];
            options.extend(ports.into_iter().map(|p| p.name));
            words(word, options.iter().map(String::as_str))
        }
        ("connect", 1) => {
            let rates: Vec<String> = std::iter::once("auto".to_string())
                .chain(BAUD_RATES.iter().map(u32::to_string))
                .collect();
            words(word, rates.iter().map(String::as_str))
        }
        ("preheat", _) => words(word, helper.presets.iter().map(String::as_str)),
        ("macro", _) => words(word, helper.macros.iter().map(String::as_str)),
        ("help", 0) => words(word, COMMANDS.iter().map(|c| c.name)),
        ("eeprom", 0) => words(word, ["save", "load", "reset"]),
        ("log", 0) => words(word, ["on", "off"]),
        ("mesh", 0) => words(word, ["probe"]),
        ("home", _) => words(word, ["x", "y", "z"]),
        ("move", 0) => words(word, ["x", "y", "z", "e"]),
        ("autotune", 0) => words(word, ["hotend", "bed", "chamber"]),
        ("fan", 0) => words(word, ["off", "50", "100"]),
        _ => Vec::new(),
    };
    (word_start, candidates)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn helper() -> ShellHelper {
        ShellHelper {
            presets: vec!["PLA".into(), "PETG".into()],
            macros: vec!["Home all".into()],
            base: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples"),
            support: ColorSupport::None,
        }
    }

    fn complete(line: &str) -> (usize, Vec<String>) {
        let (start, pairs) = complete_line(line, line.len(), &helper());
        (start, pairs.into_iter().map(|p| p.replacement).collect())
    }

    #[test]
    fn command_words_then_gcode() {
        let (start, got) = complete("se");
        assert_eq!(start, 0);
        assert_eq!(got, ["settings", "set"]);
        let (start, got) = complete("  pre");
        assert_eq!((start, got), (2, vec!["preheat".to_string()]));
        let (_, got) = complete("M10");
        assert!(got.contains(&"M104".to_string()), "{got:?}");
        let (_, got) = complete("zzz");
        assert!(got.is_empty());
    }

    #[test]
    fn arguments_complete_by_command() {
        assert_eq!(
            complete("preheat p"),
            (8, vec!["PLA".into(), "PETG".into()])
        );
        assert_eq!(complete("eeprom r"), (7, vec!["reset".into()]));
        assert_eq!(complete("sd up"), (3, vec!["upload".into()]));
        assert_eq!(complete("log o"), (4, vec!["on".into(), "off".into()]));
        assert_eq!(complete("home x y"), (7, vec!["y".into()]));
        let (start, got) = complete("connect s");
        assert_eq!(start, 8);
        assert!(got.contains(&"sim".to_string()), "{got:?}");
        let (_, got) = complete("connect COM3 25");
        assert_eq!(got, ["250000"]);
        assert!(complete("G28 X").1.is_empty());
        assert!(complete("nosuch x").1.is_empty());
    }

    #[test]
    fn paths_complete_for_load() {
        let (start, got) = complete("load calib");
        assert_eq!(start, 5);
        assert_eq!(got, ["calibration-cube.gcode"]);
        let (start, got) = complete("sd upload calib");
        assert_eq!(start, 10);
        assert_eq!(got, ["calibration-cube.gcode"]);
        assert!(helper().base.exists());
    }

    #[test]
    fn scripts_and_odd_positions() {
        let mut script = Script::new(["a", "b"], false);
        assert_eq!(script.read("> "), Read::Line("a".into()));
        assert_eq!(script.read("> "), Read::Line("b".into()));
        assert_eq!(script.read("> "), Read::Eof);
        assert!(!script.interactive());
        // A cursor inside a multi-byte character or past the end is safe.
        let _ = complete_line("préheat", 3, &helper());
        let _ = complete_line("load x", 99, &helper());
        let _ = complete_line("", 0, &helper());
    }

    /// An editor printer that takes a line only when the test lets it,
    /// like the editor's while it handles keys or after a line was entered.
    struct GatedPrinter {
        gate: crossbeam_channel::Receiver<()>,
        got: Arc<std::sync::Mutex<String>>,
    }

    impl ExternalPrinter for GatedPrinter {
        fn print(&mut self, msg: String) -> rustyline::Result<()> {
            let _ = self.gate.recv();
            self.got
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push_str(&msg);
            Ok(())
        }
    }

    #[test]
    fn a_busy_editor_never_blocks_the_output() {
        use super::super::output::{Buffer, Out};
        use std::time::{Duration, Instant};

        let (open, gate) = crossbeam_channel::unbounded();
        let got = Arc::new(std::sync::Mutex::new(String::new()));
        let reading = Arc::new(AtomicBool::new(true));
        let direct = Buffer::default();
        let sink = EditorSink::new(
            GatedPrinter {
                gate,
                got: Arc::clone(&got),
            },
            Arc::clone(&reading),
            Box::new(direct.clone()),
        );
        let out = Out::new(Box::new(sink), ColorSupport::None, false);
        // Writes on another thread, failing (not hanging) when they block.
        let finishes = |write: Box<dyn FnOnce(&Out) + Send>| {
            let out = out.clone();
            let (done, finished) = crossbeam_channel::bounded(1);
            std::thread::spawn(move || {
                write(&out);
                let _ = done.send(());
            });
            finished.recv_timeout(Duration::from_secs(5)).is_ok()
        };
        // While a line is edited the printer is stuck, yet neither the
        // echo thread nor the shell waits for it.
        assert!(
            finishes(Box::new(|out| {
                for i in 0..50 {
                    out.plain(format!("echo {i}"));
                }
            })),
            "the echo thread waited for the editor"
        );
        // The line was entered: output goes straight to the terminal.
        reading.store(false, Ordering::SeqCst);
        assert!(
            finishes(Box::new(|out| out.plain("report"))),
            "the shell waited for the editor"
        );
        assert_eq!(
            direct.text(),
            "report
"
        );
        // The next prompt takes the queued lines, in order.
        for _ in 0..50 {
            let _ = open.send(());
        }
        let want: String = (0..50)
            .map(|i| {
                format!(
                    "echo {i}
"
                )
            })
            .collect();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let text = got
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if text == want {
                break;
            }
            assert!(Instant::now() < deadline, "{text:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn wide_spaces_between_arguments_are_safe() {
        // A no-break space (2 bytes) or an ideographic space (3 bytes),
        // as pasted from a web page, separates words like a space.
        assert_eq!(complete("home x\u{a0}y"), (8, vec!["y".into()]));
        assert_eq!(
            complete("home x\u{3000}"),
            (9, vec!["x".into(), "y".into(), "z".into()])
        );
        assert_eq!(
            complete("sd\u{a0}upload\u{3000}calib").1,
            ["calibration-cube.gcode"]
        );
        for line in [
            "connect sim\u{a0}2",
            "log\u{3000}o\u{a0}",
            "eeprom \u{2003}\u{2003}",
        ] {
            for pos in 0..=line.len() {
                let _ = complete_line(line, pos, &helper());
            }
        }
    }
}
