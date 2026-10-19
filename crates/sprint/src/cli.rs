//! Command-line interface definition.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use sprint_host::{ConnectTarget, PrinterProfile};

/// Sprint: a fast, modern 3D printer host.
///
/// Without a subcommand Sprint opens the desktop GUI (optionally with a
/// G-code file).
#[derive(Debug, Parser)]
#[command(
    name = "sprint",
    bin_name = "sprint",
    version,
    about = "Sprint: a fast, modern 3D printer host (GUI, TUI and CLI)",
    long_about = None,
    args_conflicts_with_subcommands = true,
    after_help = "Connection targets (T): a serial port such as COM3 (115200 baud) or \
                  COM3@250000 (COM3@auto detects the rate), /dev/ttyUSB0@115200, \
                  tcp://host:port, or sim for the built-in virtual printer \
                  (sim://?time_scale=50&instant_moves=true sets its options); print and send \
                  also take profile for the profile's printer.\n\
                  Sprint connects only to the target given, or at startup (GUI, TUI and shell) \
                  to the active profile's printer when the profile turns auto_connect on.\n\
                  Profiles: --profile NAME picks another printer profile for tui, shell, print \
                  and send; the shell's profile command lists, switches and saves them.\n\
                  Configuration: SPRINT_CONFIG names another configuration file; every front \
                  end uses it, the GUI included.\n\
                  Logging: set RUST_LOG (for example RUST_LOG=debug)."
)]
pub struct Cli {
    /// What to do (default: open the GUI).
    #[command(subcommand)]
    pub command: Option<Command>,

    /// GUI options when no subcommand is given.
    #[command(flatten)]
    pub gui: GuiArgs,
}

impl Cli {
    /// The command to run, with the GUI as the default.
    pub fn into_command(self) -> Command {
        self.command.unwrap_or(Command::Gui(self.gui))
    }
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Open the desktop GUI (the default).
    Gui(GuiArgs),
    /// Full-screen terminal UI.
    Tui(TuiArgs),
    /// Interactive line shell, like pronsole.
    ///
    /// Type help for the commands (and the differences from pronsole);
    /// G-code is sent as typed. Commands can also come from a pipe (sprint
    /// shell -c T < script.txt) or from -e: they run in order, without a
    /// prompt or questions (eeprom reset then needs yes), and at the end
    /// the shell waits for the queued commands and for a running print to
    /// finish (Ctrl+C cancels the print) before it disconnects; a print
    /// the script paused and did not resume is cancelled.
    Shell(ShellArgs),
    /// Print a file without a UI, showing progress.
    ///
    /// When the print fails or is cancelled, the line it stopped at is
    /// printed as the command that resumes it (--start-line).
    Print(PrintArgs),
    /// Analyze a G-code file and print its statistics.
    Analyze(AnalyzeArgs),
    /// Send commands to a printer and print the replies.
    ///
    /// Each command waits for its "ok". Exit status: 0 when every command
    /// was acknowledged; 1 after a timeout, a lost connection, an error
    /// reply or a command the firmware does not know (Unknown command), so
    /// scripts notice typos; 130 on Ctrl+C.
    Send(SendArgs),
    /// List serial ports (they are not opened).
    Ports,
    /// Run the virtual printer as a TCP server.
    Sim(SimArgs),
}

/// Options of the GUI.
#[derive(Clone, Debug, Default, Args)]
pub struct GuiArgs {
    /// G-code file to open.
    #[arg(value_name = "FILE")]
    pub file: Option<PathBuf>,
    /// Connect to the built-in virtual printer at startup.
    #[arg(long, conflicts_with = "connect")]
    pub sim: bool,
    /// Printer to connect to at startup.
    #[arg(long, short = 'c', value_name = "T", value_parser = parse_target)]
    pub connect: Option<ConnectTarget>,
}

/// Options of the terminal UI.
#[derive(Clone, Debug, Args)]
pub struct TuiArgs {
    /// G-code file to open.
    #[arg(value_name = "FILE")]
    pub file: Option<PathBuf>,
    /// Printer to connect to at startup.
    #[arg(long, short = 'c', value_name = "T", value_parser = parse_target)]
    pub connect: Option<ConnectTarget>,
    /// Printer profile to use instead of the active one (its name, a
    /// unique prefix of it, or its number in the shell's profile list).
    #[arg(long, short = 'p', value_name = "NAME")]
    pub profile: Option<String>,
}

/// Options of the line shell.
#[derive(Clone, Debug, Args)]
pub struct ShellArgs {
    /// G-code file to load at startup.
    #[arg(value_name = "FILE")]
    pub file: Option<PathBuf>,
    /// Run this command (repeatable, in order) instead of reading
    /// commands, then leave as at the end of a script.
    #[arg(long, short = 'e', value_name = "CMD")]
    pub execute: Vec<String>,
    /// Printer to connect to at startup.
    #[arg(long, short = 'c', value_name = "T", value_parser = parse_target)]
    pub connect: Option<ConnectTarget>,
    /// Printer profile to use instead of the active one (its name, a
    /// unique prefix of it, or its number in the shell's profile list).
    #[arg(long, short = 'p', value_name = "NAME")]
    pub profile: Option<String>,
}

/// The printer of `print` and `send`: a connection target, or `profile`
/// for the profile's printer (named, so that no serial port is opened
/// by surprise).
#[derive(Clone, Debug, PartialEq)]
pub enum TargetArg {
    /// This target.
    Target(ConnectTarget),
    /// The printer stored in the profile.
    Profile,
}

impl TargetArg {
    /// The target to connect to with `profile`.
    pub fn resolve(&self, profile: &PrinterProfile) -> ConnectTarget {
        match self {
            TargetArg::Target(target) => target.clone(),
            TargetArg::Profile => profile.connection.clone(),
        }
    }
}

/// Options of a headless print.
#[derive(Clone, Debug, Args)]
pub struct PrintArgs {
    /// G-code file to print.
    #[arg(value_name = "FILE")]
    pub file: PathBuf,
    /// Printer to print on, or "profile" for the profile's printer.
    #[arg(long, short = 'c', value_name = "T", value_parser = parse_target_arg)]
    pub connect: TargetArg,
    /// Printer profile to use instead of the active one (its name, a
    /// unique prefix of it, or its number in the shell's profile list):
    /// its scripts, options and, with --connect profile, its printer.
    #[arg(long, short = 'p', value_name = "NAME")]
    pub profile: Option<String>,
    /// Recover an interrupted print: start at line N of the file (1 = the
    /// first line) after a resume preamble that heats to the temperatures
    /// in effect at line N and waits for them, restores the fan, units and
    /// modes, homes X and Y (never Z: the part is below the nozzle), lifts
    /// the nozzle by --z-clearance, travels to where line N starts and
    /// restores the extruder position and feedrate. A print that fails or
    /// is cancelled prints the N to use. After a power loss or a board
    /// reset add --board-reset.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub start_line: Option<u64>,
    /// With --start-line: the board was reset (a power loss, or a
    /// reconnect that reset an 8-bit board) while the nozzle stayed where
    /// the print stopped, so its Z is declared with G92 Z before lifting.
    /// Without it the firmware's Z is trusted, which after a reset is 0
    /// and would print the rest in the air.
    #[arg(long, requires = "start_line")]
    pub board_reset: bool,
    /// With --start-line: how far to lift the nozzle above the part before
    /// moving to the resume position, in mm (default 2).
    #[arg(long, value_name = "MM", requires = "start_line", value_parser = parse_z_clearance)]
    pub z_clearance: Option<f32>,
    /// With --start-line: do not wait for the temperatures in effect at
    /// line N (they are still set); for heaters that are already hot.
    #[arg(long, requires = "start_line")]
    pub no_wait_temps: bool,
    /// Do not show the progress line on stderr.
    #[arg(long)]
    pub no_progress: bool,
    /// What to do when the printer asks the user something during the
    /// print (a filament change, M0 "insert magnets"): ask shows the
    /// question and reads the answer from standard input (or it is
    /// answered at the printer), continue answers Continue at once (a
    /// pause made at the printer is still resumed there).
    #[arg(long, value_enum, value_name = "HOW", default_value_t = AnswerPrompts::Ask)]
    pub answer_prompts: AnswerPrompts,
}

/// How `sprint print` answers the printer's questions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum AnswerPrompts {
    /// Show the question and read the answer (a button number, Enter for
    /// the first) from standard input.
    #[default]
    Ask,
    /// Press the button that lets the print go on (Continue) and release
    /// a firmware waiting for the user (M108) at once. A pause made at the
    /// printer (from its menu, or without a question) is left to the
    /// person there.
    Continue,
}

/// Options of `analyze`.
#[derive(Clone, Debug, Args)]
pub struct AnalyzeArgs {
    /// G-code file to analyze.
    #[arg(value_name = "FILE")]
    pub file: PathBuf,
    /// Print JSON (statistics, slicer metadata and bounds) instead of a
    /// report.
    #[arg(long)]
    pub json: bool,
    /// Filament diameter in mm, used when the file does not state one
    /// (default 1.75).
    #[arg(long, value_name = "MM", value_parser = parse_filament_diameter)]
    pub filament_diameter: Option<f32>,
}

/// Options of `send`.
#[derive(Clone, Debug, Args)]
pub struct SendArgs {
    /// Printer to send to, or "profile" for the profile's printer.
    #[arg(long, short = 'c', value_name = "T", value_parser = parse_target_arg)]
    pub connect: TargetArg,
    /// Printer profile to use instead of the active one (its name, a
    /// unique prefix of it, or its number in the shell's profile list):
    /// its options and, with --connect profile, its printer.
    #[arg(long, short = 'p', value_name = "NAME")]
    pub profile: Option<String>,
    /// Seconds the printer may stay silent about a command before Sprint
    /// gives up: busy messages (homing, probing) and, while M109, M190 or
    /// M191 waits for a temperature, temperature reports restart the wait.
    #[arg(long, value_name = "SECS", default_value_t = 30.0, value_parser = parse_timeout)]
    pub timeout: f32,
    /// Commands to send, in order (quote commands with parameters:
    /// "G1 X10"; each argument is one command).
    #[arg(value_name = "CMD", required = true, num_args = 1..)]
    pub commands: Vec<String>,
}

/// Options of the virtual printer server.
#[derive(Clone, Debug, Args)]
pub struct SimArgs {
    /// Address to listen on; port 0 picks a free port.
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:5000")]
    pub tcp: String,
    /// Simulation speed: 1 is real time, 10 is ten times faster.
    #[arg(long, value_name = "X", default_value_t = 1.0, value_parser = parse_time_scale)]
    pub time_scale: f32,
    /// Number of extruders (1 to 8).
    #[arg(long, value_name = "N", default_value_t = 1,
          value_parser = clap::value_parser!(u8).range(1..=8))]
    pub extruders: u8,
    /// Probability (0 to 1) that a numbered line is rejected as corrupted,
    /// to exercise resends.
    #[arg(long, value_name = "R", default_value_t = 0.0, value_parser = parse_rate)]
    pub checksum_error_rate: f32,
    /// Probability (0 to 1) that an "ok" is lost, to exercise timeouts.
    #[arg(long, value_name = "R", default_value_t = 0.0, value_parser = parse_rate)]
    pub drop_ok_rate: f32,
    /// Simulate a printer without a heated bed.
    #[arg(long)]
    pub no_heated_bed: bool,
    /// Simulate a heated chamber.
    #[arg(long)]
    pub heated_chamber: bool,
    /// Moves take no time.
    #[arg(long)]
    pub instant_moves: bool,
    /// Answer "ok N.. P.. B.." (ADVANCED_OK).
    #[arg(long)]
    pub advanced_ok: bool,
    /// Delay before the boot banner, in milliseconds (scaled by the time
    /// scale).
    #[arg(long, value_name = "MS", default_value_t = 300)]
    pub boot_delay_ms: u64,
    /// Seed for fault injection and sensor noise.
    #[arg(long, value_name = "N")]
    pub seed: Option<u64>,
    /// Machine name reported by M115.
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
}

/// A connection target ([`ConnectTarget::parse`]); parsing never opens it.
fn parse_target(text: &str) -> Result<ConnectTarget, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("the connection target is empty (try COM3, tcp://host:port or sim)".into());
    }
    ConnectTarget::parse(trimmed).map_err(|e| e.to_string())
}

/// A target, or `profile` ([`TargetArg`]).
fn parse_target_arg(text: &str) -> Result<TargetArg, String> {
    if text.trim().eq_ignore_ascii_case("profile") {
        return Ok(TargetArg::Profile);
    }
    parse_target(text).map(TargetArg::Target)
}

fn parse_timeout(text: &str) -> Result<f32, String> {
    parse_number(text, "the timeout", 0.1, 86_400.0)
}

/// A finite number in `min..=max`.
fn parse_number(text: &str, what: &str, min: f32, max: f32) -> Result<f32, String> {
    let value: f32 = text
        .trim()
        .parse()
        .map_err(|_| format!("'{text}' is not a number"))?;
    if !value.is_finite() || value < min || value > max {
        return Err(format!("{what} must be between {min} and {max}"));
    }
    Ok(value)
}

fn parse_filament_diameter(text: &str) -> Result<f32, String> {
    let value = parse_number(text, "the filament diameter", 0.1, 10.0)?;
    Ok(value)
}

fn parse_z_clearance(text: &str) -> Result<f32, String> {
    parse_number(text, "the Z clearance", 0.5, 50.0)
}

fn parse_time_scale(text: &str) -> Result<f32, String> {
    parse_number(text, "the time scale", 0.01, 10_000.0)
}

fn parse_rate(text: &str) -> Result<f32, String> {
    parse_number(text, "the rate", 0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Result<Command, clap::Error> {
        let mut argv = vec!["sprint"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(Cli::into_command)
    }

    #[test]
    fn definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn gui_is_the_default() {
        let Ok(Command::Gui(args)) = parse(&[]) else {
            panic!("no gui");
        };
        assert_eq!(args.file, None);
        assert!(!args.sim);
        let Ok(Command::Gui(args)) = parse(&["cube.gcode", "--sim"]) else {
            panic!("no gui");
        };
        assert_eq!(args.file, Some(PathBuf::from("cube.gcode")));
        assert!(args.sim);
        let Ok(Command::Gui(args)) = parse(&["gui", "--sim", "x.gcode"]) else {
            panic!("no gui");
        };
        assert_eq!(args.file, Some(PathBuf::from("x.gcode")));
        assert_eq!(args.connect, None);
        // --connect T, with or without the subcommand.
        let Ok(Command::Gui(args)) = parse(&["gui", "--connect", "tcp://h:23", "x.gcode"]) else {
            panic!("no gui");
        };
        assert_eq!(
            args.connect,
            Some(ConnectTarget::Tcp {
                host: "h".into(),
                port: 23
            })
        );
        let Ok(Command::Gui(args)) = parse(&["-c", "sim"]) else {
            panic!("no gui");
        };
        assert!(
            args.connect
                .as_ref()
                .is_some_and(ConnectTarget::is_simulator)
        );
        // A target or the virtual printer, not both; nothing blank.
        assert!(parse(&["gui", "--sim", "--connect", "sim"]).is_err());
        assert!(parse(&["gui", "--connect", "  "]).is_err());
    }

    #[test]
    fn subcommands_parse() {
        let Ok(Command::Tui(t)) = parse(&["tui", "a.gcode", "--connect", "sim"]) else {
            panic!("tui");
        };
        assert!(t.connect.as_ref().is_some_and(ConnectTarget::is_simulator));
        assert_eq!(t.file, Some(PathBuf::from("a.gcode")));
        // Parsing a serial target only reads the text; nothing is opened.
        let Ok(Command::Shell(s)) = parse(&["shell", "-c", " COM3@250000 "]) else {
            panic!("shell");
        };
        assert_eq!(
            s.connect,
            Some(ConnectTarget::Serial {
                port: "COM3".into(),
                baud: 250_000
            })
        );
        let Ok(Command::Print(p)) = parse(&["print", "a.gcode", "--connect", "tcp://h:1"]) else {
            panic!("print");
        };
        assert_eq!(
            p.connect,
            TargetArg::Target(ConnectTarget::Tcp {
                host: "h".into(),
                port: 1
            })
        );
        assert_eq!(p.profile, None);
        assert_eq!(p.start_line, None);
        assert!(!p.no_progress);
        assert_eq!(p.answer_prompts, AnswerPrompts::Ask);
        let Ok(Command::Print(p)) = parse(&[
            "print",
            "a.gcode",
            "-c",
            "sim://?time_scale=50&instant_moves=true",
            "--start-line",
            "120",
            "--no-progress",
            "--answer-prompts",
            "continue",
        ]) else {
            panic!("print");
        };
        assert!(matches!(&p.connect, TargetArg::Target(t) if t.is_simulator()));
        assert_eq!(p.start_line, Some(120));
        assert!(p.no_progress);
        assert_eq!(p.answer_prompts, AnswerPrompts::Continue);
        let Ok(Command::Send(s)) = parse(&["send", "--connect", "sim", "G28", "M105"]) else {
            panic!("send");
        };
        assert_eq!(s.commands, ["G28", "M105"]);
        assert_eq!(s.timeout, 30.0);
        let Ok(Command::Send(s)) = parse(&["send", "-c", "sim", "--timeout", "2.5", "G1 X10"])
        else {
            panic!("send");
        };
        assert_eq!(s.commands, ["G1 X10"]);
        assert_eq!(s.timeout, 2.5);
        // The profile's printer only when asked for by name.
        let Ok(Command::Send(s)) = parse(&["send", "-c", "Profile", "-p", "Ender 3", "M115"])
        else {
            panic!("send");
        };
        assert_eq!(s.connect, TargetArg::Profile);
        assert_eq!(s.profile.as_deref(), Some("Ender 3"));
        let Ok(Command::Print(p)) = parse(&["print", "a.gcode", "--connect", "profile"]) else {
            panic!("print");
        };
        assert_eq!(p.connect, TargetArg::Profile);
        let profile = PrinterProfile::default();
        assert_eq!(p.connect.resolve(&profile), profile.connection);
        let Ok(Command::Tui(t)) = parse(&["tui", "--profile", "2"]) else {
            panic!("tui");
        };
        assert_eq!(t.profile.as_deref(), Some("2"));
        let Ok(Command::Shell(s)) = parse(&["shell", "-p", "MK3"]) else {
            panic!("shell");
        };
        assert_eq!(s.profile.as_deref(), Some("MK3"));
        assert_eq!(s.file, None);
        assert!(s.execute.is_empty());
        // pronsole's FILE argument and -e.
        let Ok(Command::Shell(s)) = parse(&[
            "shell",
            "cube.gcode",
            "-e",
            "temp 200",
            "--execute",
            "status",
        ]) else {
            panic!("shell");
        };
        assert_eq!(s.file, Some(PathBuf::from("cube.gcode")));
        assert_eq!(s.execute, ["temp 200", "status"]);
        assert!(matches!(parse(&["ports"]), Ok(Command::Ports)));
        let Ok(Command::Analyze(a)) = parse(&["analyze", "a.gcode", "--json"]) else {
            panic!("analyze");
        };
        assert!(a.json);
        assert_eq!(a.filament_diameter, None);
        let Ok(Command::Analyze(a)) = parse(&["analyze", "a.gcode", "--filament-diameter", "2.85"])
        else {
            panic!("analyze");
        };
        assert_eq!(a.filament_diameter, Some(2.85));
    }

    #[test]
    fn sim_defaults_and_options() {
        let Ok(Command::Sim(s)) = parse(&["sim"]) else {
            panic!("sim");
        };
        assert_eq!(s.tcp, "127.0.0.1:5000");
        assert_eq!(s.time_scale, 1.0);
        assert_eq!(s.extruders, 1);
        assert_eq!(s.boot_delay_ms, 300);
        let Ok(Command::Sim(s)) = parse(&[
            "sim",
            "--tcp",
            "0.0.0.0:0",
            "--time-scale",
            "20",
            "--extruders",
            "2",
            "--checksum-error-rate",
            "0.05",
            "--heated-chamber",
            "--seed",
            "7",
        ]) else {
            panic!("sim");
        };
        assert_eq!(s.tcp, "0.0.0.0:0");
        assert_eq!(s.time_scale, 20.0);
        assert_eq!(s.extruders, 2);
        assert_eq!(s.checksum_error_rate, 0.05);
        assert!(s.heated_chamber);
        assert_eq!(s.seed, Some(7));
    }

    #[test]
    fn bad_values_are_usage_errors() {
        for args in [
            &["analyze"][..],
            &["analyze", "a.gcode", "--filament-diameter", "-1"],
            &["analyze", "a.gcode", "--filament-diameter", "NaN"],
            &["analyze", "a.gcode", "--filament-diameter", "abc"],
            &["print", "a.gcode"],
            &["send", "--connect", "sim"],
            &["send", "--connect", "  ", "G28"],
            &["send", "--connect", "ftp://x", "G28"],
            &["send", "--connect", "sim://?nonsense=1", "G28"],
            &["send", "--connect", "sim", "--timeout", "0", "G28"],
            &["print", "a.gcode", "-c", "sim", "--start-line", "0"],
            &["print", "a.gcode", "-c", "sim", "--start-line", "x"],
            &["print", "a.gcode", "-c", "sim", "--answer-prompts", "maybe"],
            &["print", "a.gcode", "-c", "sim", "--board-reset"],
            &["print", "a.gcode", "-c", "sim", "--no-wait-temps"],
            &[
                "print",
                "a.gcode",
                "-c",
                "sim",
                "--start-line",
                "9",
                "--z-clearance",
                "0.1",
            ],
            &[
                "print",
                "a.gcode",
                "-c",
                "sim",
                "--start-line",
                "9",
                "--z-clearance",
                "x",
            ],
            &["tui", "--connect", "COM3@fast"],
            &["sim", "--extruders", "0"],
            &["sim", "--extruders", "9"],
            &["sim", "--time-scale", "0"],
            &["sim", "--checksum-error-rate", "1.5"],
            &["a.gcode", "b.gcode"],
        ] {
            let err = parse(args).expect_err(&format!("{args:?} should fail"));
            assert_eq!(err.exit_code(), 2, "{args:?}: {err}");
        }
    }

    #[test]
    fn number_parsing() {
        assert_eq!(parse_rate("0.5"), Ok(0.5));
        assert_eq!(parse_rate(" 1 "), Ok(1.0));
        assert!(parse_rate("inf").is_err());
        assert!(parse_time_scale("-2").is_err());
        assert_eq!(parse_filament_diameter("1.75"), Ok(1.75));
        assert!(parse_target("").is_err());
    }
}
