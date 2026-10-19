//! The `sprint` binary: the GUI (default), the terminal UI and shell, and
//! headless commands. See `ARCHITECTURE.md` ("sprint (binary)").
//!
//! Exit codes: 0 success, 1 the command failed, 2 usage errors (clap's),
//! 130 Ctrl+C (`print` cancels the job first).
//!
//! Nothing connects to a printer unless a command is given a `--connect`
//! target (or, in the TUI and the shell, the active profile turns
//! `auto_connect` on).

mod analyze;
mod cli;
mod console;
mod ports;
mod print;
mod send;
mod sim;

use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command, GuiArgs};

/// Exit code of a command that failed.
const EXIT_FAILURE: u8 = 1;

fn main() -> ExitCode {
    let command = Cli::parse().into_command();
    // Started from Explorer in GUI mode: do not leave a console window
    // behind the GUI.
    let detached = matches!(command, Command::Gui(_)) && console::release_own_console();
    init_logging(&command);
    match run(command) {
        Ok(code) => code,
        Err(err) => {
            let message = format!("{err:#}");
            if detached {
                console::show_error("Sprint", &message);
            } else {
                eprintln!("sprint: error: {message}");
            }
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// Log to stderr, filtered by `RUST_LOG` (default: warnings).
fn init_logging(command: &Command) {
    let default = match command {
        // Client connects and disconnects are worth seeing.
        Command::Sim(_) => "warn,sprint_sim=info",
        // Log lines would be drawn over the full-screen UI.
        Command::Tui(_) => "off",
        _ => "warn",
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default)).init();
}

fn run(command: Command) -> Result<ExitCode> {
    match command {
        Command::Gui(args) => run_gui(args),
        Command::Tui(args) => sprint_tui::run_tui(sprint_tui::TuiOptions {
            file: args.file,
            connect: args.connect,
        })
        .map(|()| ExitCode::SUCCESS),
        Command::Shell(args) => sprint_tui::run_shell(sprint_tui::TuiOptions {
            file: None,
            connect: args.connect,
        })
        .map(|()| ExitCode::SUCCESS),
        Command::Print(args) => print::run(&args),
        Command::Analyze(args) => analyze::run(&args).map(|()| ExitCode::SUCCESS),
        Command::Send(args) => send::run(&args),
        Command::Ports => ports::run().map(|()| ExitCode::SUCCESS),
        Command::Sim(args) => sim::run(&args).map(|()| ExitCode::SUCCESS),
    }
}

fn run_gui(args: GuiArgs) -> Result<ExitCode> {
    sprint_gui::run(sprint_gui::GuiOptions {
        file: args.file,
        simulator: args.sim,
    })?;
    Ok(ExitCode::SUCCESS)
}
