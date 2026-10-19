//! Command-line interface definition.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use sprint_host::ConnectTarget;

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
    after_help = "Connection targets (T): a serial port such as COM3 or COM3@250000, \
                  /dev/ttyUSB0@115200, tcp://host:port, or sim for the built-in virtual printer \
                  (sim://?time_scale=50&instant_moves=true sets its options).\n\
                  Sprint connects only to the target given, or in the TUI and the shell to the \
                  profile's printer when the profile turns auto_connect on.\n\
                  Configuration: SPRINT_CONFIG names another configuration file.\n\
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
    Shell(ShellArgs),
    /// Print a file without a UI, showing progress.
    Print(PrintArgs),
    /// Analyze a G-code file and print its statistics.
    Analyze(AnalyzeArgs),
    /// Send commands to a printer and print the replies.
    Send(SendArgs),
    /// List serial ports (they are not opened).
    Ports,
    /// Run the virtual printer as a TCP server.
    Sim(SimArgs),
}

impl Command {
    /// The subcommand name, for messages.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Gui(_) => "gui",
            Command::Tui(_) => "tui",
            Command::Shell(_) => "shell",
            Command::Print(_) => "print",
            Command::Analyze(_) => "analyze",
            Command::Send(_) => "send",
            Command::Ports => "ports",
            Command::Sim(_) => "sim",
        }
    }
}

/// Options of the GUI.
#[derive(Clone, Debug, Default, Args)]
pub struct GuiArgs {
    /// G-code file to open.
    #[arg(value_name = "FILE")]
    pub file: Option<PathBuf>,
    /// Connect to the built-in virtual printer at startup.
    #[arg(long)]
    pub sim: bool,
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
}

/// Options of the line shell.
#[derive(Clone, Debug, Args)]
pub struct ShellArgs {
    /// Printer to connect to at startup.
    #[arg(long, short = 'c', value_name = "T", value_parser = parse_target)]
    pub connect: Option<ConnectTarget>,
}

/// Options of a headless print.
#[derive(Clone, Debug, Args)]
pub struct PrintArgs {
    /// G-code file to print.
    #[arg(value_name = "FILE")]
    pub file: PathBuf,
    /// Printer to print on.
    #[arg(long, short = 'c', value_name = "T", value_parser = parse_target)]
    pub connect: ConnectTarget,
    /// Resume an interrupted print from line N of the file (1 = the first
    /// line): heat, home X and Y, and move back to where line N starts.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub start_line: Option<u64>,
    /// Do not show the progress line on stderr.
    #[arg(long)]
    pub no_progress: bool,
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
    /// Printer to send to.
    #[arg(long, short = 'c', value_name = "T", value_parser = parse_target)]
    pub connect: ConnectTarget,
    /// Seconds to wait for each command's "ok" (homing and heating take a
    /// while).
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
            ConnectTarget::Tcp {
                host: "h".into(),
                port: 1
            }
        );
        assert_eq!(p.start_line, None);
        assert!(!p.no_progress);
        let Ok(Command::Print(p)) = parse(&[
            "print",
            "a.gcode",
            "-c",
            "sim://?time_scale=50&instant_moves=true",
            "--start-line",
            "120",
            "--no-progress",
        ]) else {
            panic!("print");
        };
        assert!(p.connect.is_simulator());
        assert_eq!(p.start_line, Some(120));
        assert!(p.no_progress);
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
