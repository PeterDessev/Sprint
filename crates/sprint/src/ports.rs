//! `sprint ports`: the serial ports of this machine. Ports are only
//! enumerated, never opened.

use std::io::{self, Write};

use anyhow::{Context, Result};
use sprint_tui::report::{self, ReportLine, Tone};

/// Run `sprint ports`.
pub fn run() -> Result<()> {
    let ports = sprint_host::list_ports();
    let mut lines = report::ports_lines(&ports);
    if let Some(port) = ports
        .iter()
        .find(|p| p.likely_printer)
        .or_else(|| ports.first())
    {
        lines.push(ReportLine::new());
        lines.push(ReportLine::toned(
            format!(
                "Connect with: sprint tui --connect {}@115200 (or @250000, @auto)",
                port.name
            ),
            Tone::Dim,
        ));
    }
    let text = report::render(&lines, report::stdout_colors());
    let mut out = io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        // `sprint ports | head` is not an error.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => other.context("cannot write the output"),
    }
}
