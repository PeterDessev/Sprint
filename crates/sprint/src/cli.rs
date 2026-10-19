//! Command-line interface definition.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

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
                  /dev/ttyUSB0@115200, tcp://host:port, or sim for the built-in virtual printer.\n\
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
    pub connect: Option<String>,
}

/// Options of the line shell.
#[derive(Clone, Debug, Args)]
pub struct ShellArgs {
    /// Printer to connect to at startup.
    #[arg(long, short = 'c', value_name = "T", value_parser = parse_target)]
    pub connect: Option<String>,
}

/// Options of a headless print.
#[derive(Clone, Debug, Args)]
pub struct PrintArgs {
    /// G-code file to print.
    #[arg(value_name = "FILE")]
    pub file: PathBuf,
    /// Printer to print on.
    #[arg(long, short = 'c', value_name = "T", value_parser = parse_target)]
    pub connect: String,
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
    pub connect: String,
    /// Commands to send, in order (quote commands with parameters:
    /// "G1 X10").
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

/// A connection target: any non-blank text for now (it is validated when
/// connecting).
fn parse_target(text: &str) -> Result<String, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("the connection target is empty (try COM3, tcp://host:port or sim)".into());
    }
    Ok(trimmed.to_string())
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
        assert_eq!(t.connect.as_deref(), Some("sim"));
        assert_eq!(t.file, Some(PathBuf::from("a.gcode")));
        let Ok(Command::Shell(s)) = parse(&["shell", "-c", " COM3@250000 "]) else {
            panic!("shell");
        };
        assert_eq!(s.connect.as_deref(), Some("COM3@250000"));
        let Ok(Command::Print(p)) = parse(&["print", "a.gcode", "--connect", "tcp://h:1"]) else {
            panic!("print");
        };
        assert_eq!(p.connect, "tcp://h:1");
        let Ok(Command::Send(s)) = parse(&["send", "--connect", "sim", "G28", "M105"]) else {
            panic!("send");
        };
        assert_eq!(s.commands, ["G28", "M105"]);
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
