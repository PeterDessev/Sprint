//! The command set shared by the line shell ([`run_shell`](crate::run_shell))
//! and the command palette of the full-screen UI.
//!
//! [`parse`] turns one input line into a [`Command`], raw G-code, or an
//! error with the command's usage. Command words are matched without regard
//! to case (`Home`, `HOME`); lines whose first word looks like G-code
//! (`G28`, `m105`, `T1`, Klipper's `BED_MESH_CALIBRATE`) are passed through
//! as raw G-code, and a leading `!` forces any line to be sent raw
//! (`!status` sends `status` to the printer). A line starting with `;` is
//! a comment.

use std::fmt;
use std::path::PathBuf;

use sprint_host::sprint_proto::{SettingCommand, setting_def};
use sprint_host::{Axes, Axis, Heater};

/// Which part of the help a command is listed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Group {
    /// Connecting to printers.
    Connection,
    /// Files and print jobs.
    Job,
    /// Heaters and fans.
    Temperature,
    /// Moving the machine.
    Motion,
    /// Firmware settings and reports.
    Printer,
    /// The shell itself.
    Shell,
}

impl Group {
    /// Every group, in help order.
    pub const ALL: [Group; 6] = [
        Group::Connection,
        Group::Job,
        Group::Temperature,
        Group::Motion,
        Group::Printer,
        Group::Shell,
    ];

    /// Section title in the help.
    pub fn title(self) -> &'static str {
        match self {
            Group::Connection => "Connection",
            Group::Job => "Files and jobs",
            Group::Temperature => "Temperatures and fans",
            Group::Motion => "Motion",
            Group::Printer => "Printer settings and reports",
            Group::Shell => "Shell",
        }
    }
}

/// Static description of one command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    /// The command word.
    pub name: &'static str,
    /// Other words for the same command.
    pub aliases: &'static [&'static str],
    /// Usage line, e.g. `"temp <celsius|off> [tool]"`.
    pub usage: &'static str,
    /// One-line description.
    pub summary: &'static str,
    /// Help section.
    pub group: Group,
}

/// Every command, in help order.
pub static COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "connect",
        aliases: &[],
        usage: "connect [target] [baud] [--save]",
        summary: "Connect (sim, COM3, /dev/ttyUSB0@250000, tcp://host:port; default: the profile's printer); --save makes the target the profile's printer",
        group: Group::Connection,
    },
    CommandSpec {
        name: "profile",
        aliases: &["profiles"],
        usage: "profile [name|number]",
        summary: "List the printer profiles, or switch to one (saved as the active profile)",
        group: Group::Connection,
    },
    CommandSpec {
        name: "disconnect",
        aliases: &[],
        usage: "disconnect",
        summary: "Close the connection",
        group: Group::Connection,
    },
    CommandSpec {
        name: "ports",
        aliases: &[],
        usage: "ports",
        summary: "List serial ports (they are not opened)",
        group: Group::Connection,
    },
    CommandSpec {
        name: "reset",
        aliases: &[],
        usage: "reset",
        summary: "Reset the board (DTR on a serial port): recovers after an emergency stop, which M999 does not",
        group: Group::Connection,
    },
    CommandSpec {
        name: "block_until_online",
        aliases: &["wait_online"],
        usage: "block_until_online",
        summary: "Wait until the printer is connected (scripts; Ctrl+C stops)",
        group: Group::Connection,
    },
    CommandSpec {
        name: "status",
        aliases: &[],
        usage: "status",
        summary: "Connection, temperatures, position and job at a glance",
        group: Group::Connection,
    },
    CommandSpec {
        name: "monitor",
        aliases: &[],
        usage: "monitor [secs]",
        summary: "Live status line every secs seconds (default 2) until Ctrl+C",
        group: Group::Connection,
    },
    CommandSpec {
        name: "load",
        aliases: &["open"],
        usage: "load <file>",
        summary: "Load and analyze a G-code file",
        group: Group::Job,
    },
    CommandSpec {
        name: "info",
        aliases: &[],
        usage: "info",
        summary: "Statistics of the loaded file",
        group: Group::Job,
    },
    CommandSpec {
        name: "print",
        aliases: &[],
        usage: "print [--pause-at-layer L[,L…]]",
        summary: "Print the loaded file, pausing before the layers given (1 = the first)",
        group: Group::Job,
    },
    CommandSpec {
        name: "recover",
        aliases: &[],
        usage: "recover [line]",
        summary: "Print the loaded file again from the line its failed or cancelled print stopped at (or from a line), after a resume preamble",
        group: Group::Job,
    },
    CommandSpec {
        name: "pause",
        aliases: &[],
        usage: "pause",
        summary: "Pause the job",
        group: Group::Job,
    },
    CommandSpec {
        name: "resume",
        aliases: &[],
        usage: "resume",
        summary: "Resume a paused job",
        group: Group::Job,
    },
    CommandSpec {
        name: "cancel",
        aliases: &[],
        usage: "cancel",
        summary: "Cancel the job",
        group: Group::Job,
    },
    CommandSpec {
        name: "sd",
        aliases: &[],
        usage: "sd ls | print <name> | upload [file] [name] | rm <name>",
        summary: "SD card: list, print, upload the loaded (or a given) file, delete",
        group: Group::Job,
    },
    CommandSpec {
        name: "eta",
        aliases: &[],
        usage: "eta",
        summary: "Progress, elapsed and remaining time of the job",
        group: Group::Job,
    },
    CommandSpec {
        name: "source",
        aliases: &[],
        usage: "source <file>",
        summary: "Run the commands in a file, one per line, like a script",
        group: Group::Job,
    },
    CommandSpec {
        name: "temp",
        aliases: &["settemp"],
        usage: "temp <celsius|preset|off> [tool]",
        summary: "Set a hotend target (the active tool unless given)",
        group: Group::Temperature,
    },
    CommandSpec {
        name: "bedtemp",
        aliases: &["bed"],
        usage: "bedtemp <celsius|preset|off>",
        summary: "Set the bed target",
        group: Group::Temperature,
    },
    CommandSpec {
        name: "gettemp",
        aliases: &["temps"],
        usage: "gettemp",
        summary: "Show the temperatures and targets",
        group: Group::Temperature,
    },
    CommandSpec {
        name: "preheat",
        aliases: &[],
        usage: "preheat <preset>",
        summary: "Heat hotend and bed to a profile preset (PLA, PETG, ...)",
        group: Group::Temperature,
    },
    CommandSpec {
        name: "cooldown",
        aliases: &[],
        usage: "cooldown",
        summary: "Turn every heater off",
        group: Group::Temperature,
    },
    CommandSpec {
        name: "fan",
        aliases: &[],
        usage: "fan <0-100|off>",
        summary: "Part cooling fan speed in percent",
        group: Group::Temperature,
    },
    CommandSpec {
        name: "off",
        aliases: &[],
        usage: "off",
        summary: "Turn everything off: heaters, fans and motors (M84)",
        group: Group::Temperature,
    },
    CommandSpec {
        name: "autotune",
        aliases: &["pid"],
        usage: "autotune <hotend|bed> <celsius> [cycles]",
        summary: "PID autotune (M303); the result is applied by the firmware",
        group: Group::Temperature,
    },
    CommandSpec {
        name: "home",
        aliases: &[],
        usage: "home [x] [y] [z] [e]",
        summary: "Home all axes or the ones given; e sets the extruder position to 0 (G92 E0)",
        group: Group::Motion,
    },
    CommandSpec {
        name: "tool",
        aliases: &[],
        usage: "tool <n>",
        summary: "Select the active extruder (T<n>)",
        group: Group::Motion,
    },
    CommandSpec {
        name: "move",
        aliases: &[],
        usage: "move <x|y|z|e> <mm> [feed mm/min]",
        summary: "Move one axis relative to its position",
        group: Group::Motion,
    },
    CommandSpec {
        name: "extrude",
        aliases: &[],
        usage: "extrude <mm> [feed mm/min]",
        summary: "Push filament with the active tool",
        group: Group::Motion,
    },
    CommandSpec {
        name: "reverse",
        aliases: &["retract"],
        usage: "reverse <mm> [feed mm/min]",
        summary: "Pull filament back",
        group: Group::Motion,
    },
    CommandSpec {
        name: "speed",
        aliases: &[],
        usage: "speed <percent>",
        summary: "Feedrate override (M220)",
        group: Group::Motion,
    },
    CommandSpec {
        name: "flow",
        aliases: &[],
        usage: "flow <percent>",
        summary: "Flow override of the active tool (M221)",
        group: Group::Motion,
    },
    CommandSpec {
        name: "babystep",
        aliases: &[],
        usage: "babystep <mm>",
        summary: "Nudge Z while printing (M290)",
        group: Group::Motion,
    },
    CommandSpec {
        name: "settings",
        aliases: &[],
        usage: "settings [code|category]",
        summary: "Firmware settings from M503, as a table",
        group: Group::Printer,
    },
    CommandSpec {
        name: "set",
        aliases: &[],
        usage: "set <code> <params>",
        summary: "Change a setting, e.g. set M92 E415 (eeprom save keeps it)",
        group: Group::Printer,
    },
    CommandSpec {
        name: "eeprom",
        aliases: &[],
        usage: "eeprom save | load | reset",
        summary: "Store (M500), reload (M501) or factory-reset (M502) the settings",
        group: Group::Printer,
    },
    CommandSpec {
        name: "firmware",
        aliases: &[],
        usage: "firmware",
        summary: "Firmware name, version and capabilities (M115)",
        group: Group::Printer,
    },
    CommandSpec {
        name: "endstops",
        aliases: &[],
        usage: "endstops",
        summary: "Endstop and probe states (M119)",
        group: Group::Printer,
    },
    CommandSpec {
        name: "mesh",
        aliases: &[],
        usage: "mesh [probe]",
        summary: "Bed leveling mesh as a heatmap (probe runs G29 first)",
        group: Group::Printer,
    },
    CommandSpec {
        name: "stats",
        aliases: &[],
        usage: "stats",
        summary: "Printer statistics (M78) and link statistics",
        group: Group::Printer,
    },
    CommandSpec {
        name: "macro",
        aliases: &["macros"],
        usage: "macro [name [args]] | define <name> [gcode | gcode] | edit <name> | rm <name>",
        summary: "List the macros, run one ({0}, {1}, {args} take the arguments), or define, edit or delete one",
        group: Group::Printer,
    },
    CommandSpec {
        name: "help",
        aliases: &["?"],
        usage: "help [command|G-code]",
        summary: "List commands, or explain one command or G-code",
        group: Group::Shell,
    },
    CommandSpec {
        name: "log",
        aliases: &[],
        usage: "log [on|off]",
        summary: "Echo all printer traffic (sent lines, polling) or only replies",
        group: Group::Shell,
    },
    CommandSpec {
        name: "exit",
        aliases: &["quit"],
        usage: "exit",
        summary: "Leave (asks first while a job runs)",
        group: Group::Shell,
    },
];

/// The spec of a command word or alias (case-insensitive).
pub fn command_spec(word: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|c| {
        c.name.eq_ignore_ascii_case(word) || c.aliases.iter().any(|a| a.eq_ignore_ascii_case(word))
    })
}

/// Commands whose name starts with `prefix` (case-insensitive), in help
/// order. Aliases match too but the command is listed once.
pub fn command_candidates(prefix: &str) -> Vec<&'static CommandSpec> {
    let prefix = prefix.to_ascii_lowercase();
    COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(&prefix) || c.aliases.iter().any(|a| a.starts_with(&prefix)))
        .collect()
}

/// A pronsole command that Sprint names differently, or does not have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PronsoleWord {
    /// pronsole's command word.
    pub word: &'static str,
    /// The Sprint command it stands for (the rest of the line follows
    /// it), or `None` when Sprint has no such command.
    pub sprint: Option<&'static str>,
    /// What it does in Sprint, or why there is no such command.
    pub note: &'static str,
}

/// pronsole commands with another name in Sprint, or none. pronsole's
/// other commands (connect, load, print, pause, resume, home, move,
/// extrude, reverse, settemp, bedtemp, gettemp, eta, tool, off, reset,
/// monitor, macro, block_until_online, help, exit) work as in pronsole.
pub static PRONSOLE: &[PronsoleWord] = &[
    PronsoleWord {
        word: "ls",
        sprint: Some("sd ls"),
        note: "list the SD card",
    },
    PronsoleWord {
        word: "sdprint",
        sprint: Some("sd print"),
        note: "print a file from the SD card",
    },
    PronsoleWord {
        word: "upload",
        sprint: Some("sd upload"),
        note: "write a file to the SD card",
    },
    PronsoleWord {
        word: "run_script",
        sprint: None,
        note: "Sprint does not run other programs; source <file> runs shell commands from a file",
    },
    PronsoleWord {
        word: "run_gcode_script",
        sprint: None,
        note: "Sprint does not run other programs; source <file> runs G-code and commands from a file",
    },
    PronsoleWord {
        word: "slice",
        sprint: None,
        note: "Sprint does not slice: slice in your slicer, then load the G-code",
    },
    PronsoleWord {
        word: "skein",
        sprint: None,
        note: "Sprint does not slice: slice in your slicer, then load the G-code",
    },
    PronsoleWord {
        word: "loadrc",
        sprint: None,
        note: "Sprint reads no .pronsolerc: profiles, macros and presets live in the configuration file",
    },
];

/// The pronsole entry for `word`, unless it is a Sprint command.
pub fn pronsole_word(word: &str) -> Option<&'static PronsoleWord> {
    if command_spec(word).is_some() {
        return None;
    }
    PRONSOLE.iter().find(|p| p.word.eq_ignore_ascii_case(word))
}

/// The closest command name to a mistyped word: one edit away (a
/// swapped pair of letters counts as one), or two for words of five
/// letters or more. Short words are not matched loosely: `eta` is not a
/// typo of `set`.
pub fn suggest(word: &str) -> Option<&'static str> {
    let word = word.to_ascii_lowercase();
    let most = if word.chars().count() >= 5 { 2 } else { 1 };
    COMMANDS
        .iter()
        .flat_map(|c| std::iter::once(c.name).chain(c.aliases.iter().copied()))
        .map(|name| (edit_distance(&word, name), name))
        .filter(|(d, _)| *d <= most)
        .min_by_key(|(d, _)| *d)
        .and_then(|(_, name)| command_spec(name).map(|c| c.name))
}

/// Edit distance with adjacent transpositions (optimal string
/// alignment); ASCII case already folded by the caller.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    // d[i][j]: distance between a[..i] and b[..j].
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[a.len()][b.len()]
}

/// A heater target as typed.
#[derive(Clone, Debug, PartialEq)]
pub enum TempTarget {
    /// Degrees Celsius in 0..=500 (0 = off).
    Celsius(f32),
    /// The hotend or bed temperature of the profile's preset with this
    /// name (pronsole's `settemp pla`).
    Preset(String),
}

/// What `eeprom` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eeprom {
    /// M500.
    Save,
    /// M501 (then M503).
    Load,
    /// M502 (then M503). `confirmed` when the line said `yes` already.
    Reset {
        /// `eeprom reset yes`: do not ask again.
        confirmed: bool,
    },
}

/// What `sd` does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SdCommand {
    /// List the card's files.
    List,
    /// Print a file from the card.
    Print(String),
    /// Write a local file (the loaded one when `None`) to the card as
    /// `name` (derived from the file name when `None`).
    Upload {
        /// Local file.
        file: Option<PathBuf>,
        /// Name on the card.
        name: Option<String>,
    },
    /// Delete a file from the card.
    Remove(String),
}

/// A parsed command.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// `help [topic]`.
    Help(Option<String>),
    /// `connect [target] [baud] [--save]` (`baud` 0 = auto-detect).
    Connect {
        /// Target text (parsed when connecting); `None` = the profile's.
        target: Option<String>,
        /// Serial baud rate override.
        baud: Option<u32>,
        /// Store the target as the active profile's printer.
        save: bool,
    },
    /// `profile [name]`: list the profiles, or switch to one.
    Profile(Option<String>),
    /// `disconnect`.
    Disconnect,
    /// `ports`.
    Ports,
    /// `load <file>`.
    Load(PathBuf),
    /// `info`.
    Info,
    /// `print [--pause-at-layer L]`: print the loaded file, pausing
    /// (with the pause script) before these layers (1-based).
    Print {
        /// Layers to pause before.
        pause_at: Vec<u64>,
    },
    /// `pause`.
    Pause,
    /// `resume`.
    Resume,
    /// `cancel`.
    Cancel,
    /// `recover [line]`: print the loaded file again from the line its
    /// last print stopped at, or from this 1-based line.
    Recover(Option<u64>),
    /// `status`.
    Status,
    /// `temp <celsius|preset> [tool]`.
    Temp {
        /// Target.
        target: TempTarget,
        /// Tool; `None` = the active one.
        tool: Option<u8>,
    },
    /// `bedtemp <celsius|preset>`.
    BedTemp(TempTarget),
    /// `gettemp`.
    GetTemp,
    /// `preheat [preset]`.
    Preheat(Option<String>),
    /// `cooldown`.
    Cooldown,
    /// `off`: heaters, fans and motors off.
    Off,
    /// `home [x] [y] [z] [e]`.
    Home {
        /// Axes to home (none when only `e` was given).
        axes: Axes,
        /// `e`: set the extruder position to 0 (G92 E0).
        zero_e: bool,
    },
    /// `tool <n>`.
    Tool(u8),
    /// `eta`.
    Eta,
    /// `source <file>`.
    Source(PathBuf),
    /// `reset`.
    Reset,
    /// `block_until_online`.
    WaitOnline,
    /// `move <axis> <mm> [feed]`.
    Move {
        /// Axis.
        axis: Axis,
        /// Distance (mm, signed).
        mm: f32,
        /// Feedrate in mm/min; `None` = the profile's jog feedrate.
        feedrate: Option<f32>,
    },
    /// `extrude <mm> [feed]` (negative for `reverse`).
    Extrude {
        /// Length (mm), negative to retract.
        mm: f32,
        /// Feedrate in mm/min; `None` = the profile's extrude feedrate.
        feedrate: Option<f32>,
    },
    /// `fan <percent>`.
    Fan(f32),
    /// `speed <percent>`.
    Speed(f32),
    /// `flow <percent>`.
    Flow(f32),
    /// `babystep <mm>`.
    Babystep(f32),
    /// `monitor [secs]`.
    Monitor(Option<f32>),
    /// `settings [filter]`.
    Settings(Option<String>),
    /// `set <code> <params>`.
    Set(SettingCommand),
    /// `eeprom ...`.
    Eeprom(Eeprom),
    /// `firmware`.
    Firmware,
    /// `endstops`.
    Endstops,
    /// `mesh [probe]`.
    Mesh {
        /// Probe the bed (G29) first.
        probe: bool,
    },
    /// `stats`.
    Stats,
    /// `sd ...`.
    Sd(SdCommand),
    /// `autotune <heater> <celsius> [cycles]`.
    Autotune {
        /// Heater to tune.
        heater: Heater,
        /// Target (°C).
        celsius: f32,
        /// Cycles.
        cycles: u32,
    },
    /// `macro [name [args]]`: list the macros, or run one (the text is
    /// the name and the arguments, see [`crate::actions::macro_call`]).
    Macro(Option<String>),
    /// `macro define <name> [gcode]` or `macro edit <name> [gcode]`.
    MacroDefine {
        /// The macro's name.
        name: String,
        /// Its G-code, a line per `|`; `None` reads the lines that follow
        /// (the shell), up to a blank line.
        gcode: Option<String>,
        /// `edit`: the macro must exist already.
        edit: bool,
    },
    /// `macro rm <name>`.
    MacroRemove(String),
    /// `log [on|off]` (`None` reports the current mode).
    Log(Option<bool>),
    /// `exit` / `quit`.
    Exit,
}

/// One parsed input line.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// Blank line or comment.
    Empty,
    /// A shell command.
    Command(Command),
    /// G-code to send as typed.
    Gcode(String),
}

/// A line that could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// What is wrong.
    pub message: String,
    /// Usage of the command, when the command word was recognised.
    pub usage: Option<&'static str>,
}

impl ParseError {
    fn new(message: impl Into<String>) -> ParseError {
        ParseError {
            message: message.into(),
            usage: None,
        }
    }

    fn usage(spec: &'static CommandSpec, message: impl Into<String>) -> ParseError {
        ParseError {
            message: message.into(),
            usage: Some(spec.usage),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.usage {
            Some(usage) => write!(f, "{} (usage: {usage})", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ParseError {}

/// Whether `word` starts a G-code line: a letter and a number (`G1`,
/// `M104`, `T0`, `N10`, `G1X10`), or an upper-case extended command name
/// with an underscore or digit (`BED_MESH_CALIBRATE`, `M486`) as Klipper and
/// RepRapFirmware macros use.
pub fn looks_like_gcode(word: &str) -> bool {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    let rest = chars.as_str();
    if rest
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit() || c == '-' || c == '.')
    {
        return true;
    }
    word.len() > 1
        && word
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && word.chars().any(|c| c == '_' || c.is_ascii_digit())
}

/// Split arguments on whitespace; `"..."` or `'...'` quote an argument
/// with spaces (no escapes, so Windows paths work as typed).
pub fn split_args(text: &str) -> Result<Vec<String>, ParseError> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(&c) = chars.peek() else {
            return Ok(out);
        };
        let mut arg = String::new();
        if c == '"' || c == '\'' {
            chars.next();
            let mut closed = false;
            for ch in chars.by_ref() {
                if ch == c {
                    closed = true;
                    break;
                }
                arg.push(ch);
            }
            if !closed {
                return Err(ParseError::new(format!("missing closing {c}")));
            }
        } else {
            while let Some(ch) = chars.next_if(|c| !c.is_whitespace()) {
                arg.push(ch);
            }
        }
        out.push(arg);
    }
}

/// The rest of a line as one path: surrounding quotes are removed, inner
/// spaces kept.
fn path_arg(rest: &str) -> Option<PathBuf> {
    let rest = rest.trim();
    let unquoted = ['"', '\'']
        .iter()
        .find_map(|q| {
            rest.strip_prefix(*q)
                .and_then(|r| r.strip_suffix(*q))
                .filter(|inner| !inner.contains(*q))
        })
        .unwrap_or(rest);
    (!unquoted.is_empty()).then(|| PathBuf::from(unquoted))
}

/// A finite number, optionally followed by a unit suffix (`%`, `mm`, `C`,
/// `°C`).
fn number(text: &str) -> Option<f32> {
    let lower = text.to_ascii_lowercase();
    let trimmed = lower
        .trim_end_matches('%')
        .trim_end_matches("mm")
        .trim_end_matches("°c")
        .trim_end_matches('c')
        .trim_end_matches('°');
    trimmed.parse::<f32>().ok().filter(|v| v.is_finite())
}

/// A temperature: a number in 0..=500, or `off`.
fn temperature(spec: &'static CommandSpec, text: &str) -> Result<TempTarget, ParseError> {
    if text.eq_ignore_ascii_case("off") {
        return Ok(TempTarget::Celsius(0.0));
    }
    // A word is a preset's name (checked against the profile later).
    if text.starts_with(|c: char| c.is_alphabetic()) {
        return Ok(TempTarget::Preset(text.to_string()));
    }
    match number(text) {
        Some(v) if (0.0..=500.0).contains(&v) => Ok(TempTarget::Celsius(v)),
        Some(_) => Err(ParseError::usage(
            spec,
            format!("{text} °C is out of range (0 to 500)"),
        )),
        None => Err(ParseError::usage(
            spec,
            format!("'{text}' is not a temperature"),
        )),
    }
}

/// A number in `min..=max` named `what`.
fn ranged(
    spec: &'static CommandSpec,
    text: &str,
    what: &str,
    min: f32,
    max: f32,
) -> Result<f32, ParseError> {
    match number(text) {
        Some(v) if (min..=max).contains(&v) => Ok(v),
        Some(_) => Err(ParseError::usage(
            spec,
            format!("{what} {text} is out of range ({min} to {max})"),
        )),
        None => Err(ParseError::usage(
            spec,
            format!("'{text}' is not a valid {what}"),
        )),
    }
}

/// A tool number: `1` or `T1`, 0..=7.
fn tool(spec: &'static CommandSpec, text: &str) -> Result<u8, ParseError> {
    let digits = text.strip_prefix(['t', 'T', 'e', 'E']).unwrap_or(text);
    match digits.parse::<u8>() {
        Ok(t) if t < 8 => Ok(t),
        _ => Err(ParseError::usage(
            spec,
            format!("'{text}' is not a tool (0 to 7)"),
        )),
    }
}

/// An optional feedrate in mm/min.
fn feedrate(spec: &'static CommandSpec, arg: Option<&String>) -> Result<Option<f32>, ParseError> {
    arg.map(|f| ranged(spec, f, "feedrate", 1.0, 100_000.0))
        .transpose()
}

/// Fail when more than `max` arguments were given.
fn at_most(spec: &'static CommandSpec, args: &[String], max: usize) -> Result<(), ParseError> {
    match args.get(max) {
        Some(extra) => Err(ParseError::usage(
            spec,
            format!("unexpected argument '{extra}'"),
        )),
        None => Ok(()),
    }
}

/// The heater named by `text` (`hotend`, `T1`, `e0`, `bed`, `chamber`).
fn heater(spec: &'static CommandSpec, text: &str) -> Result<Heater, ParseError> {
    let lower = text.to_ascii_lowercase();
    match lower.as_str() {
        "hotend" | "extruder" | "nozzle" | "h" | "e" | "t" => return Ok(Heater::Hotend(0)),
        "bed" | "b" => return Ok(Heater::Bed),
        "chamber" | "c" => return Ok(Heater::Chamber),
        _ => {}
    }
    let digits = lower
        .strip_prefix("hotend")
        .or_else(|| lower.strip_prefix("extruder"))
        .or_else(|| lower.strip_prefix('t'))
        .or_else(|| lower.strip_prefix('e'));
    match digits.and_then(|d| d.parse::<u8>().ok()) {
        Some(t) if t < 8 => Ok(Heater::Hotend(t)),
        _ => Err(ParseError::usage(
            spec,
            format!("'{text}' is not a heater (hotend, T1, bed or chamber)"),
        )),
    }
}

/// Parse one input line.
pub fn parse(line: &str) -> Result<Input, ParseError> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(';') {
        return Ok(Input::Empty);
    }
    if let Some(raw) = line.strip_prefix('!') {
        let raw = raw.trim();
        return Ok(if raw.is_empty() {
            Input::Empty
        } else {
            Input::Gcode(raw.to_string())
        });
    }
    let (word, rest) = match line.find(char::is_whitespace) {
        Some(i) => (&line[..i], line[i..].trim()),
        None => (line, ""),
    };
    if let Some(spec) = command_spec(word) {
        return parse_command(spec, rest).map(Input::Command);
    }
    if looks_like_gcode(word) {
        return Ok(Input::Gcode(line.to_string()));
    }
    if let Some(pronsole) = pronsole_word(word) {
        // The Sprint command starts with a command word: no recursion.
        return match pronsole.sprint {
            Some(sprint) => parse(&format!("{sprint} {rest}")),
            None => Err(ParseError::new(format!(
                "{word} is a pronsole command: {}",
                pronsole.note
            ))),
        };
    }
    let mut message = format!("unknown command '{word}'");
    match suggest(word) {
        Some(name) => message.push_str(&format!("; did you mean '{name}'?")),
        None => message.push_str("; type help for the list"),
    }
    message.push_str(" (prefix a line with ! to send it to the printer as is)");
    Err(ParseError::new(message))
}

/// Parse the arguments of a recognised command.
fn parse_command(spec: &'static CommandSpec, rest: &str) -> Result<Command, ParseError> {
    // `load` and `source` take the whole rest as a path (spaces allowed
    // without quotes).
    if spec.name == "load" {
        return path_arg(rest)
            .map(Command::Load)
            .ok_or_else(|| ParseError::usage(spec, "which file?"));
    }
    if spec.name == "source" {
        return path_arg(rest)
            .map(Command::Source)
            .ok_or_else(|| ParseError::usage(spec, "which file?"));
    }
    let args = split_args(rest).map_err(|e| ParseError::usage(spec, e.message))?;
    let arg = |i: usize| args.get(i);
    let required = |i: usize, what: &str| {
        args.get(i)
            .ok_or_else(|| ParseError::usage(spec, format!("missing {what}")))
    };
    let none = |cmd: Command| at_most(spec, &args, 0).map(|()| cmd);
    match spec.name {
        "connect" => {
            let save = args.iter().any(|a| a.eq_ignore_ascii_case("--save"));
            let args: Vec<String> = args
                .iter()
                .filter(|a| !a.eq_ignore_ascii_case("--save"))
                .cloned()
                .collect();
            at_most(spec, &args, 2)?;
            let baud =
                match args.get(1) {
                    None => None,
                    Some(b) if b.eq_ignore_ascii_case("auto") => Some(0),
                    Some(b) => Some(b.parse::<u32>().map_err(|_| {
                        ParseError::usage(spec, format!("'{b}' is not a baud rate"))
                    })?),
                };
            Ok(Command::Connect {
                target: args.first().cloned(),
                baud,
                save,
            })
        }
        "profile" => Ok(Command::Profile(
            path_arg(rest).map(|p| p.to_string_lossy().into_owned()),
        )),
        "disconnect" => none(Command::Disconnect),
        "ports" => none(Command::Ports),
        "info" => none(Command::Info),
        "print" => parse_print(spec, &args),
        "pause" => none(Command::Pause),
        "resume" => none(Command::Resume),
        "cancel" => none(Command::Cancel),
        "recover" => {
            at_most(spec, &args, 1)?;
            match arg(0) {
                None => Ok(Command::Recover(None)),
                Some(text) => match text.parse::<u64>() {
                    Ok(n) if n >= 1 => Ok(Command::Recover(Some(n))),
                    _ => Err(ParseError::usage(
                        spec,
                        format!("'{text}' is not a line (1 is the first)"),
                    )),
                },
            }
        }
        "status" => none(Command::Status),
        "cooldown" => none(Command::Cooldown),
        "firmware" => none(Command::Firmware),
        "endstops" => none(Command::Endstops),
        "stats" => none(Command::Stats),
        "exit" => none(Command::Exit),
        "temp" => {
            at_most(spec, &args, 2)?;
            let target = temperature(spec, required(0, "the temperature")?)?;
            let tool = arg(1).map(|t| tool(spec, t)).transpose()?;
            Ok(Command::Temp { target, tool })
        }
        "gettemp" => none(Command::GetTemp),
        "off" => none(Command::Off),
        "eta" => none(Command::Eta),
        "reset" => none(Command::Reset),
        "block_until_online" => none(Command::WaitOnline),
        "tool" => {
            at_most(spec, &args, 1)?;
            Ok(Command::Tool(tool(spec, required(0, "the tool")?)?))
        }
        "bedtemp" => {
            at_most(spec, &args, 1)?;
            Ok(Command::BedTemp(temperature(
                spec,
                required(0, "the temperature")?,
            )?))
        }
        "preheat" => Ok(Command::Preheat(
            (!rest.is_empty()).then(|| rest.to_string()),
        )),
        "fan" => {
            at_most(spec, &args, 1)?;
            let text = required(0, "the speed")?;
            let pct = match text.to_ascii_lowercase().as_str() {
                "off" => 0.0,
                "on" | "full" => 100.0,
                _ => ranged(spec, text, "fan speed", 0.0, 100.0)?,
            };
            Ok(Command::Fan(pct))
        }
        "autotune" => {
            at_most(spec, &args, 3)?;
            let heater = heater(spec, required(0, "the heater")?)?;
            let celsius = ranged(
                spec,
                required(1, "the temperature")?,
                "temperature",
                30.0,
                400.0,
            )?;
            let cycles = match arg(2) {
                None => 5,
                Some(c) => match c.parse::<u32>() {
                    Ok(n) if (1..=100).contains(&n) => n,
                    _ => {
                        return Err(ParseError::usage(
                            spec,
                            format!("'{c}' is not a cycle count (1 to 100)"),
                        ));
                    }
                },
            };
            Ok(Command::Autotune {
                heater,
                celsius,
                cycles,
            })
        }
        "home" => {
            let mut axes = Axes::default();
            let mut zero_e = false;
            for a in &args {
                let lower = a.to_ascii_lowercase();
                if lower == "all" {
                    axes = Axes::ALL;
                    continue;
                }
                if lower.is_empty() || !lower.chars().all(|c| matches!(c, 'x' | 'y' | 'z' | 'e')) {
                    return Err(ParseError::usage(
                        spec,
                        format!("'{a}' is not an axis (x, y, z, e)"),
                    ));
                }
                for c in lower.chars() {
                    match c {
                        'x' => axes.x = true,
                        'y' => axes.y = true,
                        'z' => axes.z = true,
                        _ => zero_e = true,
                    }
                }
            }
            if axes == Axes::default() && !zero_e {
                axes = Axes::ALL;
            }
            Ok(Command::Home { axes, zero_e })
        }
        "move" => {
            at_most(spec, &args, 3)?;
            let axis_text = required(0, "the axis")?;
            let axis = match axis_text.to_ascii_lowercase().as_str() {
                "x" => Axis::X,
                "y" => Axis::Y,
                "z" => Axis::Z,
                "e" => Axis::E,
                _ => {
                    return Err(ParseError::usage(
                        spec,
                        format!("'{axis_text}' is not an axis (x, y, z, e)"),
                    ));
                }
            };
            let mm = ranged(
                spec,
                required(1, "the distance")?,
                "distance",
                -1000.0,
                1000.0,
            )?;
            if mm == 0.0 {
                return Err(ParseError::usage(spec, "the distance must not be 0"));
            }
            Ok(Command::Move {
                axis,
                mm,
                feedrate: feedrate(spec, arg(2))?,
            })
        }
        "extrude" | "reverse" => {
            at_most(spec, &args, 2)?;
            let mm = ranged(spec, required(0, "the length")?, "length", 0.0, 1000.0)?;
            if mm == 0.0 {
                return Err(ParseError::usage(spec, "the length must not be 0"));
            }
            let mm = if spec.name == "reverse" { -mm } else { mm };
            Ok(Command::Extrude {
                mm,
                feedrate: feedrate(spec, arg(1))?,
            })
        }
        "speed" | "flow" => {
            at_most(spec, &args, 1)?;
            let pct = ranged(
                spec,
                required(0, "the percentage")?,
                "percentage",
                1.0,
                1000.0,
            )?;
            Ok(if spec.name == "speed" {
                Command::Speed(pct)
            } else {
                Command::Flow(pct)
            })
        }
        "babystep" => {
            at_most(spec, &args, 1)?;
            let mm = ranged(spec, required(0, "the distance")?, "distance", -5.0, 5.0)?;
            if mm == 0.0 {
                return Err(ParseError::usage(spec, "the distance must not be 0"));
            }
            Ok(Command::Babystep(mm))
        }
        "monitor" => {
            at_most(spec, &args, 1)?;
            Ok(Command::Monitor(
                arg(0)
                    .map(|s| ranged(spec, s, "interval", 0.2, 3600.0))
                    .transpose()?,
            ))
        }
        "settings" => Ok(Command::Settings(
            (!rest.is_empty()).then(|| rest.to_string()),
        )),
        "set" => parse_set(spec, rest),
        "eeprom" => {
            at_most(spec, &args, 2)?;
            let action = required(0, "save, load or reset")?.to_ascii_lowercase();
            let yes = arg(1).map(|a| a.to_ascii_lowercase());
            let confirmed = match yes.as_deref() {
                None => false,
                Some("yes" | "-y" | "--yes" | "y") if action == "reset" => true,
                Some(other) => {
                    return Err(ParseError::usage(
                        spec,
                        format!("unexpected argument '{other}'"),
                    ));
                }
            };
            match action.as_str() {
                "save" | "store" => Ok(Command::Eeprom(Eeprom::Save)),
                "load" | "restore" => Ok(Command::Eeprom(Eeprom::Load)),
                "reset" | "defaults" | "factory" => {
                    Ok(Command::Eeprom(Eeprom::Reset { confirmed }))
                }
                _ => Err(ParseError::usage(
                    spec,
                    format!("'{action}' is not save, load or reset"),
                )),
            }
        }
        "mesh" => {
            at_most(spec, &args, 1)?;
            match arg(0).map(|a| a.to_ascii_lowercase()) {
                None => Ok(Command::Mesh { probe: false }),
                Some(a) if a == "probe" || a == "g29" => Ok(Command::Mesh { probe: true }),
                Some(a) => Err(ParseError::usage(
                    spec,
                    format!("unexpected argument '{a}'"),
                )),
            }
        }
        "sd" => parse_sd(spec, &args),
        "macro" => parse_macro(spec, rest),
        "log" => {
            at_most(spec, &args, 1)?;
            match arg(0).map(|a| a.to_ascii_lowercase()) {
                None => Ok(Command::Log(None)),
                Some(a) if matches!(a.as_str(), "on" | "1" | "true" | "yes") => {
                    Ok(Command::Log(Some(true)))
                }
                Some(a) if matches!(a.as_str(), "off" | "0" | "false" | "no") => {
                    Ok(Command::Log(Some(false)))
                }
                Some(a) => Err(ParseError::usage(spec, format!("'{a}' is not on or off"))),
            }
        }
        "help" => {
            at_most(spec, &args, 1)?;
            Ok(Command::Help(arg(0).cloned()))
        }
        _ => Err(ParseError::new(format!(
            "{} is not available here",
            spec.name
        ))),
    }
}

/// `print [--pause-at-layer L[,L…]]` (the option may repeat).
fn parse_print(spec: &'static CommandSpec, args: &[String]) -> Result<Command, ParseError> {
    let mut pause_at = Vec::new();
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let value = match word.split_once('=') {
            Some((flag, value)) if flag.eq_ignore_ascii_case("--pause-at-layer") => value,
            _ if word.eq_ignore_ascii_case("--pause-at-layer") => words
                .next()
                .ok_or_else(|| ParseError::usage(spec, "--pause-at-layer needs a layer"))?,
            _ => {
                return Err(ParseError::usage(
                    spec,
                    format!("unexpected argument '{word}'"),
                ));
            }
        };
        for layer in value.split(',').map(str::trim).filter(|l| !l.is_empty()) {
            match layer.parse::<u64>() {
                Ok(n) if n >= 1 => pause_at.push(n),
                _ => {
                    return Err(ParseError::usage(
                        spec,
                        format!("'{layer}' is not a layer (1 is the first)"),
                    ));
                }
            }
        }
    }
    Ok(Command::Print { pause_at })
}

/// `macro [name [args]] | define <name> [gcode] | edit <name> [gcode] |
/// rm <name>`.
fn parse_macro(spec: &'static CommandSpec, rest: &str) -> Result<Command, ParseError> {
    let (sub, after) = match rest.find(char::is_whitespace) {
        Some(i) => (&rest[..i], rest[i..].trim()),
        None => (rest, ""),
    };
    let edit = match sub.to_ascii_lowercase().as_str() {
        "define" | "def" | "new" => false,
        "edit" => true,
        "rm" | "del" | "delete" | "remove" => {
            return path_arg(after)
                .map(|name| Command::MacroRemove(name.to_string_lossy().into_owned()))
                .ok_or_else(|| ParseError::usage(spec, "which macro?"));
        }
        _ => return Ok(Command::Macro((!rest.is_empty()).then(|| rest.to_string()))),
    };
    // A quoted name may have spaces; the rest of the line is the G-code.
    let (name, gcode) = match after.chars().next() {
        Some(q @ ('"' | '\'')) => match after[1..].find(q) {
            Some(end) => (&after[1..=end], after[end + 2..].trim()),
            None => return Err(ParseError::usage(spec, format!("missing closing {q}"))),
        },
        _ => match after.find(char::is_whitespace) {
            Some(i) => (&after[..i], after[i..].trim()),
            None => (after, ""),
        },
    };
    let name = name.trim();
    if name.is_empty() {
        return Err(ParseError::usage(spec, "which macro?"));
    }
    let gcode = (!gcode.is_empty()).then(|| {
        gcode
            .split('|')
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(Command::MacroDefine {
        name: name.to_string(),
        gcode,
        edit,
    })
}

/// pronsole's own options (its `set` command), not firmware settings.
const PRONSOLE_OPTIONS: &[&str] = &[
    "port",
    "baudrate",
    "dtr",
    "tcp_streaming_mode",
    "bedtemp_abs",
    "bedtemp_pla",
    "temperature_abs",
    "temperature_pla",
    "xy_feedrate",
    "z_feedrate",
    "e_feedrate",
    "slicecommand",
    "sliceoptscommand",
    "final_command",
    "start_command",
    "error_command",
    "log_path",
    "recentfiles",
];

/// `set <code> <params>`: a setting command checked against the catalog.
fn parse_set(spec: &'static CommandSpec, rest: &str) -> Result<Command, ParseError> {
    if rest.is_empty() {
        return Err(ParseError::usage(spec, "which setting?"));
    }
    // pronsole's `set` changed pronsole's own options.
    let first = rest.split_whitespace().next().unwrap_or_default();
    if PRONSOLE_OPTIONS
        .iter()
        .any(|o| o.eq_ignore_ascii_case(first))
    {
        return Err(ParseError::usage(
            spec,
            format!(
                "set changes firmware settings; '{first}' is a pronsole option: \
                 connect <port> [baud] --save stores the printer, and the other options \
                 live in the profile (profile lists them)"
            ),
        ));
    }
    let Some(cmd) = SettingCommand::parse(rest) else {
        return Err(ParseError::usage(
            spec,
            format!("'{rest}' is not a setting command such as M92 E415"),
        ));
    };
    if cmd.params.is_empty() {
        return Err(ParseError::usage(
            spec,
            format!("{} needs at least one parameter", cmd.code),
        ));
    }
    if let Some(def) = setting_def(&cmd.code) {
        for &(letter, value) in &cmd.params {
            let Some(param) = def.param(letter) else {
                return Err(ParseError::usage(
                    spec,
                    format!("{} has no parameter {letter}", cmd.code),
                ));
            };
            if value.is_finite() && !(param.min..=param.max).contains(&value) {
                return Err(ParseError::usage(
                    spec,
                    format!(
                        "{}{letter} {value} is out of range ({} to {})",
                        cmd.code, param.min, param.max
                    ),
                ));
            }
        }
    }
    Ok(Command::Set(cmd))
}

/// `sd ls | print <name> | upload [file] [name] | rm <name>`.
fn parse_sd(spec: &'static CommandSpec, args: &[String]) -> Result<Command, ParseError> {
    let Some(sub) = args.first() else {
        return Ok(Command::Sd(SdCommand::List));
    };
    let name = |what: &str| {
        args.get(1)
            .cloned()
            .ok_or_else(|| ParseError::usage(spec, format!("which file to {what}?")))
    };
    let sub_lower = sub.to_ascii_lowercase();
    let max = match sub_lower.as_str() {
        "ls" | "list" | "dir" => 1,
        "upload" | "put" => 3,
        _ => 2,
    };
    at_most(spec, args, max)?;
    match sub_lower.as_str() {
        "ls" | "list" | "dir" => Ok(Command::Sd(SdCommand::List)),
        "print" | "start" => Ok(Command::Sd(SdCommand::Print(name("print")?))),
        "rm" | "del" | "delete" => Ok(Command::Sd(SdCommand::Remove(name("delete")?))),
        "upload" | "put" => Ok(Command::Sd(SdCommand::Upload {
            file: args.get(1).map(PathBuf::from),
            name: args.get(2).cloned(),
        })),
        _ => Err(ParseError::usage(
            spec,
            format!("'{sub}' is not ls, print, upload or rm"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(line: &str) -> Command {
        match parse(line) {
            Ok(Input::Command(c)) => c,
            other => panic!("{line:?} gave {other:?}"),
        }
    }

    fn err(line: &str) -> ParseError {
        match parse(line) {
            Err(e) => e,
            other => panic!("{line:?} should fail, gave {other:?}"),
        }
    }

    #[test]
    fn every_command_has_a_unique_name() {
        let mut names: Vec<&str> = COMMANDS
            .iter()
            .flat_map(|c| std::iter::once(c.name).chain(c.aliases.iter().copied()))
            .collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate command words");
        for c in COMMANDS {
            assert!(c.usage.starts_with(c.name), "{}", c.name);
            assert!(!looks_like_gcode(c.name), "{} reads as G-code", c.name);
            assert_eq!(command_spec(c.name), Some(c));
        }
    }

    #[test]
    fn commands_parse() {
        let table: Vec<(&str, Command)> = vec![
            ("help", Command::Help(None)),
            ("HELP move", Command::Help(Some("move".into()))),
            ("? G28", Command::Help(Some("G28".into()))),
            (
                "connect",
                Command::Connect {
                    target: None,
                    baud: None,
                    save: false,
                },
            ),
            (
                "connect sim",
                Command::Connect {
                    target: Some("sim".into()),
                    baud: None,
                    save: false,
                },
            ),
            (
                "connect COM3 250000",
                Command::Connect {
                    target: Some("COM3".into()),
                    baud: Some(250_000),
                    save: false,
                },
            ),
            (
                "connect /dev/ttyUSB0 auto",
                Command::Connect {
                    target: Some("/dev/ttyUSB0".into()),
                    baud: Some(0),
                    save: false,
                },
            ),
            (
                "connect COM5 --save 250000",
                Command::Connect {
                    target: Some("COM5".into()),
                    baud: Some(250_000),
                    save: true,
                },
            ),
            (
                "connect --SAVE",
                Command::Connect {
                    target: None,
                    baud: None,
                    save: true,
                },
            ),
            ("profile", Command::Profile(None)),
            ("profiles", Command::Profile(None)),
            ("profile Ender 3", Command::Profile(Some("Ender 3".into()))),
            (
                "profile \"My MK3\"",
                Command::Profile(Some("My MK3".into())),
            ),
            ("disconnect", Command::Disconnect),
            ("ports", Command::Ports),
            ("load cube.gcode", Command::Load("cube.gcode".into())),
            (
                "open  my part.gcode ",
                Command::Load("my part.gcode".into()),
            ),
            (
                "load \"C:\\prints\\a b.gcode\"",
                Command::Load("C:\\prints\\a b.gcode".into()),
            ),
            ("info", Command::Info),
            ("print", Command::Print { pause_at: vec![] }),
            (
                "print --pause-at-layer 5 --pause-at-layer=2,10",
                Command::Print {
                    pause_at: vec![5, 2, 10],
                },
            ),
            ("Pause", Command::Pause),
            ("resume", Command::Resume),
            ("cancel", Command::Cancel),
            ("recover", Command::Recover(None)),
            ("recover 1200", Command::Recover(Some(1200))),
            ("status", Command::Status),
            (
                "temp 210",
                Command::Temp {
                    target: TempTarget::Celsius(210.0),
                    tool: None,
                },
            ),
            (
                "temp 200 1",
                Command::Temp {
                    target: TempTarget::Celsius(200.0),
                    tool: Some(1),
                },
            ),
            (
                "settemp off T2",
                Command::Temp {
                    target: TempTarget::Celsius(0.0),
                    tool: Some(2),
                },
            ),
            (
                "temp 215°C",
                Command::Temp {
                    target: TempTarget::Celsius(215.0),
                    tool: None,
                },
            ),
            ("bedtemp 60", Command::BedTemp(TempTarget::Celsius(60.0))),
            ("bed off", Command::BedTemp(TempTarget::Celsius(0.0))),
            ("preheat", Command::Preheat(None)),
            ("preheat pla", Command::Preheat(Some("pla".into()))),
            ("cooldown", Command::Cooldown),
            ("fan 50", Command::Fan(50.0)),
            ("fan 75%", Command::Fan(75.0)),
            ("fan off", Command::Fan(0.0)),
            ("fan on", Command::Fan(100.0)),
            (
                "home",
                Command::Home {
                    axes: Axes::ALL,
                    zero_e: false,
                },
            ),
            (
                "home x",
                Command::Home {
                    axes: Axes::X,
                    zero_e: false,
                },
            ),
            (
                "home X y",
                Command::Home {
                    axes: Axes::XY,
                    zero_e: false,
                },
            ),
            (
                "home xy",
                Command::Home {
                    axes: Axes::XY,
                    zero_e: false,
                },
            ),
            (
                "home z",
                Command::Home {
                    axes: Axes::Z,
                    zero_e: false,
                },
            ),
            (
                "home all",
                Command::Home {
                    axes: Axes::ALL,
                    zero_e: false,
                },
            ),
            // pronsole: `home e` zeroes the extruder, with or without axes.
            (
                "home e",
                Command::Home {
                    axes: Axes::default(),
                    zero_e: true,
                },
            ),
            (
                "home x E",
                Command::Home {
                    axes: Axes::X,
                    zero_e: true,
                },
            ),
            (
                "settemp pla",
                Command::Temp {
                    target: TempTarget::Preset("pla".into()),
                    tool: None,
                },
            ),
            (
                "bedtemp PETG",
                Command::BedTemp(TempTarget::Preset("PETG".into())),
            ),
            ("gettemp", Command::GetTemp),
            ("eta", Command::Eta),
            ("off", Command::Off),
            ("tool 1", Command::Tool(1)),
            ("tool T0", Command::Tool(0)),
            ("reset", Command::Reset),
            ("block_until_online", Command::WaitOnline),
            (
                "source my script.txt",
                Command::Source("my script.txt".into()),
            ),
            // pronsole's names for the SD card commands.
            ("ls", Command::Sd(SdCommand::List)),
            (
                "sdprint CUBE.GCO",
                Command::Sd(SdCommand::Print("CUBE.GCO".into())),
            ),
            (
                "upload a.gcode A.GCO",
                Command::Sd(SdCommand::Upload {
                    file: Some("a.gcode".into()),
                    name: Some("A.GCO".into()),
                }),
            ),
            (
                "move x 10",
                Command::Move {
                    axis: Axis::X,
                    mm: 10.0,
                    feedrate: None,
                },
            ),
            (
                "move Z -0.5 300",
                Command::Move {
                    axis: Axis::Z,
                    mm: -0.5,
                    feedrate: Some(300.0),
                },
            ),
            (
                "move e 5mm",
                Command::Move {
                    axis: Axis::E,
                    mm: 5.0,
                    feedrate: None,
                },
            ),
            (
                "extrude 5",
                Command::Extrude {
                    mm: 5.0,
                    feedrate: None,
                },
            ),
            (
                "extrude 50 120",
                Command::Extrude {
                    mm: 50.0,
                    feedrate: Some(120.0),
                },
            ),
            (
                "reverse 2",
                Command::Extrude {
                    mm: -2.0,
                    feedrate: None,
                },
            ),
            (
                "retract 1.5 2400",
                Command::Extrude {
                    mm: -1.5,
                    feedrate: Some(2400.0),
                },
            ),
            ("speed 150", Command::Speed(150.0)),
            ("flow 95%", Command::Flow(95.0)),
            ("babystep -0.05", Command::Babystep(-0.05)),
            ("monitor", Command::Monitor(None)),
            ("monitor 5", Command::Monitor(Some(5.0))),
            ("settings", Command::Settings(None)),
            ("settings motion", Command::Settings(Some("motion".into()))),
            ("eeprom save", Command::Eeprom(Eeprom::Save)),
            ("eeprom LOAD", Command::Eeprom(Eeprom::Load)),
            (
                "eeprom reset",
                Command::Eeprom(Eeprom::Reset { confirmed: false }),
            ),
            (
                "eeprom reset yes",
                Command::Eeprom(Eeprom::Reset { confirmed: true }),
            ),
            ("firmware", Command::Firmware),
            ("endstops", Command::Endstops),
            ("mesh", Command::Mesh { probe: false }),
            ("mesh probe", Command::Mesh { probe: true }),
            ("stats", Command::Stats),
            ("sd", Command::Sd(SdCommand::List)),
            ("sd ls", Command::Sd(SdCommand::List)),
            (
                "sd print CUBE.GCO",
                Command::Sd(SdCommand::Print("CUBE.GCO".into())),
            ),
            (
                "sd rm OLD.GCO",
                Command::Sd(SdCommand::Remove("OLD.GCO".into())),
            ),
            (
                "sd upload",
                Command::Sd(SdCommand::Upload {
                    file: None,
                    name: None,
                }),
            ),
            (
                "sd upload \"my cube.gcode\" CUBE.GCO",
                Command::Sd(SdCommand::Upload {
                    file: Some("my cube.gcode".into()),
                    name: Some("CUBE.GCO".into()),
                }),
            ),
            (
                "autotune hotend 210",
                Command::Autotune {
                    heater: Heater::Hotend(0),
                    celsius: 210.0,
                    cycles: 5,
                },
            ),
            (
                "autotune bed 60 8",
                Command::Autotune {
                    heater: Heater::Bed,
                    celsius: 60.0,
                    cycles: 8,
                },
            ),
            (
                "pid T1 230",
                Command::Autotune {
                    heater: Heater::Hotend(1),
                    celsius: 230.0,
                    cycles: 5,
                },
            ),
            ("macro", Command::Macro(None)),
            ("macro Bed mesh", Command::Macro(Some("Bed mesh".into()))),
            (
                "macro heat 210 60",
                Command::Macro(Some("heat 210 60".into())),
            ),
            (
                "macro define park G91 | G1 Z{0=10} |  | G90",
                Command::MacroDefine {
                    name: "park".into(),
                    gcode: Some("G91\nG1 Z{0=10}\nG90".into()),
                    edit: false,
                },
            ),
            (
                "macro define \"Bed level\"",
                Command::MacroDefine {
                    name: "Bed level".into(),
                    gcode: None,
                    edit: false,
                },
            ),
            (
                "macro EDIT 'Bed level' G29",
                Command::MacroDefine {
                    name: "Bed level".into(),
                    gcode: Some("G29".into()),
                    edit: true,
                },
            ),
            ("macro rm Bed mesh", Command::MacroRemove("Bed mesh".into())),
            ("log", Command::Log(None)),
            ("log on", Command::Log(Some(true))),
            ("log OFF", Command::Log(Some(false))),
            ("exit", Command::Exit),
            ("quit", Command::Exit),
        ];
        for (line, want) in table {
            assert_eq!(cmd(line), want, "{line}");
        }
        let Command::Set(set) = cmd("set M92 E415.5") else {
            panic!("set");
        };
        assert_eq!(set.to_gcode(), "M92 E415.5");
        let Command::Set(set) = cmd("set m301 p22.2 i1.08 d114") else {
            panic!("set");
        };
        assert_eq!(set.to_gcode(), "M301 P22.2 I1.08 D114");
    }

    #[test]
    fn gcode_passes_through() {
        for line in [
            "G28",
            "g1 x10 y10",
            "M104 S210",
            "m105",
            "T1",
            "G1X10Y20",
            "N10 G1 X1",
            "BED_MESH_CALIBRATE",
            "SET_FAN_SPEED FAN=part SPEED=0.5",
            "M117 Hello; world",
        ] {
            assert_eq!(parse(line), Ok(Input::Gcode(line.to_string())), "{line}");
        }
        assert_eq!(parse("!status"), Ok(Input::Gcode("status".into())));
        assert_eq!(parse("! G28 X"), Ok(Input::Gcode("G28 X".into())));
        assert_eq!(parse("!"), Ok(Input::Empty));
        assert_eq!(parse("   "), Ok(Input::Empty));
        assert_eq!(parse("; just a comment"), Ok(Input::Empty));
    }

    #[test]
    fn gcode_words() {
        for word in [
            "G1",
            "M104",
            "T0",
            "G1X10",
            "M486",
            "BED_MESH_CALIBRATE",
            "G-1",
        ] {
            assert!(looks_like_gcode(word), "{word}");
        }
        for word in ["home", "PAUSE", "x", "", "1G", "Status", "hello_world"] {
            assert!(!looks_like_gcode(word), "{word}");
        }
    }

    #[test]
    fn errors_name_the_problem_and_the_usage() {
        let table: Vec<(&str, &str, Option<&str>)> = vec![
            (
                "temp",
                "missing the temperature",
                Some("temp <celsius|preset|off> [tool]"),
            ),
            (
                "temp 2hot",
                "'2hot' is not a temperature",
                Some("temp <celsius|preset|off> [tool]"),
            ),
            (
                "temp 900",
                "900 °C is out of range (0 to 500)",
                Some("temp <celsius|preset|off> [tool]"),
            ),
            (
                "temp 200 9",
                "'9' is not a tool (0 to 7)",
                Some("temp <celsius|preset|off> [tool]"),
            ),
            (
                "temp 200 1 2",
                "unexpected argument '2'",
                Some("temp <celsius|preset|off> [tool]"),
            ),
            (
                "print --pause-at-layer 0",
                "'0' is not a layer (1 is the first)",
                Some("print [--pause-at-layer L[,L…]]"),
            ),
            (
                "print --pause-at-layer",
                "--pause-at-layer needs a layer",
                Some("print [--pause-at-layer L[,L…]]"),
            ),
            (
                "print now",
                "unexpected argument 'now'",
                Some("print [--pause-at-layer L[,L…]]"),
            ),
            (
                "bedtemp -5",
                "-5 °C is out of range (0 to 500)",
                Some("bedtemp <celsius|preset|off>"),
            ),
            (
                "fan 150",
                "fan speed 150 is out of range (0 to 100)",
                Some("fan <0-100|off>"),
            ),
            (
                "home q",
                "'q' is not an axis (x, y, z, e)",
                Some("home [x] [y] [z] [e]"),
            ),
            (
                "move",
                "missing the axis",
                Some("move <x|y|z|e> <mm> [feed mm/min]"),
            ),
            (
                "move w 10",
                "'w' is not an axis (x, y, z, e)",
                Some("move <x|y|z|e> <mm> [feed mm/min]"),
            ),
            (
                "move x",
                "missing the distance",
                Some("move <x|y|z|e> <mm> [feed mm/min]"),
            ),
            (
                "move x 0",
                "the distance must not be 0",
                Some("move <x|y|z|e> <mm> [feed mm/min]"),
            ),
            (
                "move x 10 -5",
                "feedrate -5 is out of range (1 to 100000)",
                Some("move <x|y|z|e> <mm> [feed mm/min]"),
            ),
            (
                "extrude -5",
                "length -5 is out of range (0 to 1000)",
                Some("extrude <mm> [feed mm/min]"),
            ),
            (
                "speed 0",
                "percentage 0 is out of range (1 to 1000)",
                Some("speed <percent>"),
            ),
            (
                "babystep 9",
                "distance 9 is out of range (-5 to 5)",
                Some("babystep <mm>"),
            ),
            ("load", "which file?", Some("load <file>")),
            ("load \"\"", "which file?", Some("load <file>")),
            (
                "eeprom",
                "missing save, load or reset",
                Some("eeprom save | load | reset"),
            ),
            (
                "eeprom burn",
                "'burn' is not save, load or reset",
                Some("eeprom save | load | reset"),
            ),
            (
                "eeprom save yes",
                "unexpected argument 'yes'",
                Some("eeprom save | load | reset"),
            ),
            ("set", "which setting?", Some("set <code> <params>")),
            (
                "set M92",
                "M92 needs at least one parameter",
                Some("set <code> <params>"),
            ),
            (
                "set hello",
                "'hello' is not a setting command such as M92 E415",
                Some("set <code> <params>"),
            ),
            (
                "set M92 Q5",
                "M92 has no parameter Q",
                Some("set <code> <params>"),
            ),
            (
                "sd print",
                "which file to print?",
                Some("sd ls | print <name> | upload [file] [name] | rm <name>"),
            ),
            (
                "sd format",
                "'format' is not ls, print, upload or rm",
                Some("sd ls | print <name> | upload [file] [name] | rm <name>"),
            ),
            (
                "autotune oven 200",
                "'oven' is not a heater (hotend, T1, bed or chamber)",
                Some("autotune <hotend|bed> <celsius> [cycles]"),
            ),
            (
                "autotune bed 60 0",
                "'0' is not a cycle count (1 to 100)",
                Some("autotune <hotend|bed> <celsius> [cycles]"),
            ),
            (
                "log maybe",
                "'maybe' is not on or off",
                Some("log [on|off]"),
            ),
            (
                "connect COM3 fast",
                "'fast' is not a baud rate",
                Some("connect [target] [baud] [--save]"),
            ),
            ("status now", "unexpected argument 'now'", Some("status")),
            (
                "macro define",
                "which macro?",
                Some(
                    "macro [name [args]] | define <name> [gcode | gcode] | edit <name> | rm <name>",
                ),
            ),
            (
                "macro rm",
                "which macro?",
                Some(
                    "macro [name [args]] | define <name> [gcode | gcode] | edit <name> | rm <name>",
                ),
            ),
            (
                "macro define \"open",
                "missing closing \"",
                Some(
                    "macro [name [args]] | define <name> [gcode | gcode] | edit <name> | rm <name>",
                ),
            ),
            (
                "sd upload \"a.gcode",
                "missing closing \"",
                Some("sd ls | print <name> | upload [file] [name] | rm <name>"),
            ),
        ];
        for (line, message, usage) in table {
            let e = err(line);
            assert_eq!(e.message, message, "{line}");
            assert_eq!(e.usage, usage, "{line}");
        }
        assert_eq!(
            err("temp").to_string(),
            "missing the temperature (usage: temp <celsius|preset|off> [tool])"
        );
    }

    #[test]
    fn unknown_words_get_a_suggestion() {
        let e = err("hoem");
        assert!(
            e.message
                .starts_with("unknown command 'hoem'; did you mean 'home'?"),
            "{e}"
        );
        assert!(e.message.contains("prefix a line with !"), "{e}");
        assert!(err("xyzzy").message.contains("type help for the list"));
        assert_eq!(suggest("setings"), Some("settings"));
        assert_eq!(suggest("quti"), Some("exit"));
        assert_eq!(suggest("completely-different"), None);
        // Short words are not stretched into commands that do something
        // else: eta is not a typo of set.
        assert_eq!(suggest("eat"), Some("eta"));
        assert_eq!(suggest("sdt"), Some("sd"));
        assert_eq!(suggest("abc"), None);
        assert_eq!(suggest("rst"), None);
        assert_eq!(suggest("stat"), Some("stats"));
    }

    #[test]
    fn pronsole_words_map_to_sprint_commands_or_explain() {
        for line in [
            "run_script make.sh",
            "run_gcode_script x",
            "slice a.stl",
            "loadrc",
        ] {
            let e = err(line);
            assert!(e.message.contains("is a pronsole command"), "{e}");
            assert!(!e.message.contains("did you mean"), "{e}");
        }
        assert!(err("run_script x").message.contains("source <file>"));
        // A pronsole `set` option is not a firmware setting.
        let e = err("set port COM5");
        assert!(e.message.contains("pronsole option"), "{e}");
        assert!(e.message.contains("--save"), "{e}");
        // Every Sprint name a pronsole word stands for parses.
        for p in PRONSOLE {
            if let Some(sprint) = p.sprint {
                let word = sprint.split_whitespace().next().unwrap_or_default();
                assert!(command_spec(word).is_some(), "{}", p.word);
            }
            assert!(command_spec(p.word).is_none(), "{} is a command", p.word);
        }
        assert!(pronsole_word("ls").is_some());
        assert!(pronsole_word("print").is_none());
    }

    #[test]
    fn candidates_match_prefixes_and_aliases() {
        let names: Vec<&str> = command_candidates("s").iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            [
                "status", "sd", "source", "temp", "speed", "settings", "set", "stats"
            ]
        );
        let names: Vec<&str> = command_candidates("RE").iter().map(|c| c.name).collect();
        assert_eq!(names, ["reset", "recover", "resume", "reverse"]);
        assert!(command_candidates("zzz").is_empty());
        assert_eq!(command_candidates("").len(), COMMANDS.len());
    }

    #[test]
    fn arguments_split_with_quotes() {
        assert_eq!(
            split_args(r#"a "b c" 'd e'  f"#),
            Ok(vec!["a".into(), "b c".into(), "d e".into(), "f".into()])
        );
        assert_eq!(split_args(""), Ok(Vec::<String>::new()));
        assert_eq!(split_args(r#""""#), Ok(vec![String::new()]));
        assert!(split_args("'open").is_err());
    }

    #[test]
    fn hostile_input_never_panics() {
        for line in [
            "temp \u{0}",
            "move x NaN",
            "move x inf",
            "temp 1e40",
            "fan -0",
            "set M92 X1e400",
            "home \u{202e}",
            "sd upload a b c d",
            "connect a b c",
            "日本語",
            "load ''",
            "\"",
            "!!!!",
        ] {
            let _ = parse(line);
        }
        assert!(parse("move x NaN").is_err());
        assert!(parse("move x inf").is_err());
        assert!(parse("temp 1e40").is_err());
    }
}
