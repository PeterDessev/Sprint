//! Reply parsing: turns lines received from the firmware into [`Reply`]
//! values. See [`ReplyParser`].

mod autotune;
mod mesh_block;
mod position;
mod sd;
mod status;
mod temps;
mod text;
mod types;

pub use position::parse_position;
pub use temps::parse_temperatures;
pub use text::parse_duration;
pub use types::*;

use crate::{BedMesh, SettingCommand, parse_firmware_line, setting_def};
use mesh_block::{MeshBlock, Step};
use text::{
    is_fatal_error, parse_host_action, parse_u32, strip_prefix_ci, tokens_with_offsets, unquote,
};

/// Stateful line parser (handles multi-line blocks: SD file list, mesh
/// reports, PID autotune results). Feed every received line, in order.
///
/// Never panics, whatever the input.
#[derive(Clone, Debug, Default)]
pub struct ReplyParser {
    /// Inside "Begin file list" ... "End file list".
    sd_listing: bool,
    /// An open leveling report.
    mesh: Option<MeshBlock>,
    /// A Prusa-style MBL header ("Z search height:") was seen, so the
    /// following "Measured points:" rows are printed back row first.
    prusa_mbl: bool,
    /// PID autotune session state.
    pid: autotune::PidTracker,
}

impl ReplyParser {
    /// A parser with no pending block.
    pub fn new() -> ReplyParser {
        ReplyParser::default()
    }

    /// Parse one received line (with or without its line ending).
    pub fn feed(&mut self, line: &str) -> Vec<Reply> {
        let mut out = Vec::new();
        let line = line.trim_matches(|c: char| c.is_whitespace() || c.is_control());
        if let Some(block) = self.mesh.as_mut() {
            match block.feed(line) {
                Step::Consumed => return out,
                Step::PassThrough => {}
                Step::Finished(mesh) => {
                    self.mesh = None;
                    out.extend(mesh.map(Reply::Mesh));
                    return out;
                }
                Step::FinishedPassThrough(mesh) => {
                    self.mesh = None;
                    out.extend(mesh.map(Reply::Mesh));
                }
            }
        }
        self.dispatch(line, &mut out);
        out
    }

    /// End of input (e.g. disconnect): emit whatever a pending multi-line
    /// block has collected.
    pub fn flush(&mut self) -> Vec<Reply> {
        self.mesh
            .take()
            .and_then(|mut block| block.finish())
            .map(Reply::Mesh)
            .into_iter()
            .collect()
    }

    /// Forget all pending state (e.g. after reconnecting).
    pub fn reset(&mut self) {
        *self = ReplyParser::default();
    }

    /// Start a mesh block if `line` is a mesh report header.
    fn start_mesh(&mut self, line: &str) -> bool {
        match MeshBlock::start(line, self.prusa_mbl) {
            Some(block) => {
                self.mesh = Some(block);
                self.prusa_mbl = false;
                true
            }
            None => false,
        }
    }

    /// Classify one trimmed line.
    fn dispatch(&mut self, line: &str, out: &mut Vec<Reply>) {
        if line.is_empty() {
            return;
        }
        if self.sd_listing {
            if sd::is_list_end(line) {
                self.sd_listing = false;
                out.push(Reply::SdCard(SdReply::ListEnd));
                return;
            }
            if ok_remainder(line).is_some() {
                // The "End file list" line was lost; close the listing.
                self.sd_listing = false;
                out.push(Reply::SdCard(SdReply::ListEnd));
            } else if is_file_candidate(line)
                && let Some(file) = sd::parse_sd_file(line)
            {
                out.push(Reply::SdCard(SdReply::File(file)));
                return;
            }
        }
        if let Some(rest) = ok_remainder(line) {
            self.parse_ok(rest, out);
            return;
        }
        if let Some(n) = parse_resend(line) {
            out.push(Reply::Resend(n));
            return;
        }
        if let Some(reply) = parse_error(line) {
            out.push(reply);
            return;
        }
        if let Some(body) = strip_prefix_ci(line, "echo:") {
            let body = body.trim();
            if let Some(replies) = self.pid.feed(body, line) {
                out.extend(replies);
                return;
            }
            if self.classify_message(body, out) || self.start_mesh(body) {
                return;
            }
            // M503 lines: "echo:  M92 X80.00 ..." (any G/M code inside echo).
            if body.starts_with(['G', 'M', 'g', 'm'])
                && let Some(cmd) = SettingCommand::parse(body)
            {
                out.push(Reply::Setting(cmd));
                return;
            }
            out.push(Reply::Echo(body.to_string()));
            return;
        }
        if let Some(body) = line.strip_prefix("//") {
            let body = body.trim();
            if let Some(action) = strip_prefix_ci(body, "action:") {
                out.push(Reply::HostAction(parse_host_action(action)));
            } else if let Some(cmd) = strip_prefix_ci(body, "Unknown command:") {
                out.push(Reply::UnknownCommand(unquote(cmd).to_string()));
            } else if let Some(mesh) = inline_klipper_matrix(body) {
                out.push(Reply::Mesh(mesh));
            } else if let Some(result) = autotune::parse_klipper_pid(body) {
                out.push(result);
            } else if !self.start_mesh(line) {
                out.push(Reply::Info(body.to_string()));
            }
            return;
        }
        if line.eq_ignore_ascii_case("wait") {
            out.push(Reply::Wait);
            return;
        }
        if line.eq_ignore_ascii_case("start") {
            out.push(Reply::Start);
            return;
        }
        if let Some(info) = parse_firmware_line(line) {
            out.push(Reply::Firmware(info));
            return;
        }
        if let Some(cap) = parse_capability(line) {
            out.push(cap);
            return;
        }
        if let Some(report) = parse_temperatures(line) {
            out.push(Reply::Temperatures(report));
            return;
        }
        if let Some(report) = parse_position(line) {
            out.push(Reply::Position(report));
            return;
        }
        if let Some(replies) = self.pid.feed(line, line) {
            out.extend(replies);
            return;
        }
        if self.classify_message(line, out) || self.start_mesh(line) {
            return;
        }
        if strip_prefix_ci(line, "Num X,Y:").is_some() {
            self.prusa_mbl = false;
        } else if strip_prefix_ci(line, "Z search height:").is_some() {
            self.prusa_mbl = true;
        }
        if let Some(endstops) = status::parse_endstops(line) {
            out.extend(
                endstops
                    .into_iter()
                    .map(|(name, triggered)| Reply::Endstop { name, triggered }),
            );
            return;
        }
        // Settings printed without "echo:" (Smoothieware, RRF config dumps):
        // only codes the catalog knows, so stray G-code echoes are not
        // mistaken for settings.
        if let Some(cmd) = SettingCommand::parse(line).filter(|c| setting_def(&c.code).is_some()) {
            out.push(Reply::Setting(cmd));
            return;
        }
        out.push(Reply::Other(line.to_string()));
    }

    /// Messages that appear both with and without an `echo:` prefix. Returns
    /// false when `body` is none of them.
    fn classify_message(&mut self, body: &str, out: &mut Vec<Reply>) -> bool {
        if let Some(msg) = strip_prefix_ci(body, "busy:") {
            out.push(Reply::Busy(msg.trim().to_string()));
            return true;
        }
        if let Some(cmd) = strip_prefix_ci(body, "Unknown command:") {
            out.push(Reply::UnknownCommand(unquote(cmd).to_string()));
            return true;
        }
        if let Some(n) = strip_prefix_ci(body, "Active Extruder:") {
            if let Some(n) = parse_u32(n.trim()).and_then(|n| u8::try_from(n).ok()) {
                out.push(Reply::ActiveExtruder(n));
                return true;
            }
            return false;
        }
        if let Some(reply) = sd::parse_sd_message(body) {
            match reply {
                Reply::SdCard(SdReply::ListBegin) => self.sd_listing = true,
                Reply::SdCard(SdReply::ListEnd) => self.sd_listing = false,
                _ => {}
            }
            out.push(reply);
            return true;
        }
        if let Some(secs) = status::parse_print_time(body) {
            out.push(Reply::PrintTime(secs));
            return true;
        }
        if let Some(stats) = status::parse_stats(body) {
            out.push(Reply::Stats(stats));
            return true;
        }
        false
    }

    /// `ok` plus whatever follows it on the line.
    fn parse_ok(&mut self, rest: &str, out: &mut Vec<Reply>) {
        let mut line = None;
        let mut planner_free = None;
        let mut buffer_free = None;
        let mut remainder = "";
        for (idx, (offset, tok)) in tokens_with_offsets(rest).into_iter().enumerate() {
            let (first, tail) = tok.split_at(tok.chars().next().map_or(0, char::len_utf8));
            let value = parse_u32(tail);
            match (first, value) {
                ("N" | "n", Some(v)) if line.is_none() => line = Some(v),
                ("P" | "p", Some(v)) if planner_free.is_none() => planner_free = Some(v),
                ("B" | "b", Some(v)) if buffer_free.is_none() => buffer_free = Some(v),
                _ if idx == 0 && line.is_none() && parse_u32(tok).is_some() => {
                    // Repetier: "ok 123".
                    line = parse_u32(tok);
                }
                _ => {
                    remainder = rest[offset..].trim();
                    break;
                }
            }
        }
        out.extend(self.pid.on_ok());
        out.push(Reply::Ok {
            line,
            planner_free,
            buffer_free,
        });
        if remainder.is_empty() {
            return;
        }
        if let Some(report) = parse_temperatures(remainder) {
            out.push(Reply::Temperatures(report));
        } else if let Some(report) = parse_position(remainder) {
            out.push(Reply::Position(report));
        } else if let Some(info) = parse_firmware_line(remainder) {
            out.push(Reply::Firmware(info));
        }
    }
}

/// Klipper `probed_matrix: [[...], [...]]` / `mesh_matrix: [[...]]` on one line.
fn inline_klipper_matrix(body: &str) -> Option<BedMesh> {
    let lower = body.to_ascii_lowercase();
    let kind = if lower.starts_with("probed_matrix") {
        "klipper_probed"
    } else if lower.starts_with("mesh_matrix") {
        "klipper_mesh"
    } else {
        return None;
    };
    let start = body.find('[')?;
    let rows = mesh_block::parse_inline_matrix(&body[start..])?;
    Some(BedMesh::from_rows(rows, kind))
}

/// Whether a line inside a file listing can be a file entry (and not an
/// interleaved status line, a resend request or one of Prusa's `DIR_ENTER:`
/// / `DIR_EXIT` directory markers).
fn is_file_candidate(line: &str) -> bool {
    const PREFIXES: [&str; 7] = [
        "echo:",
        "//",
        "!!",
        "error:",
        "busy:",
        "resend:",
        "dir_enter:",
    ];
    !PREFIXES.iter().any(|p| strip_prefix_ci(line, p).is_some())
        && !line.eq_ignore_ascii_case("wait")
        && !line.eq_ignore_ascii_case("start")
        && !line.eq_ignore_ascii_case("dir_exit")
        && parse_resend(line).is_none()
}

/// The text after a leading `ok` token, if the line starts with one.
fn ok_remainder(line: &str) -> Option<&str> {
    let rest = strip_prefix_ci(line, "ok")?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then_some(rest)
}

/// "Resend: 3", "Resend:3", "Resend: N3", "rs N3", "rs 3".
fn parse_resend(line: &str) -> Option<u32> {
    let rest = if let Some(r) = strip_prefix_ci(line, "Resend:") {
        r
    } else {
        let r = strip_prefix_ci(line, "rs")?;
        if !r.starts_with(char::is_whitespace) {
            return None;
        }
        r
    };
    let rest = rest.trim();
    let rest = rest.strip_prefix(['N', 'n']).unwrap_or(rest);
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    parse_u32(&rest[..digits])
}

/// "Error:...", RRF "Error: ...", Klipper "!! ...".
fn parse_error(line: &str) -> Option<Reply> {
    let message = if let Some(m) = strip_prefix_ci(line, "Error:") {
        m.trim()
    } else {
        line.strip_prefix("!!")?.trim()
    };
    if let Some(cmd) = strip_prefix_ci(message, "Unknown command:") {
        return Some(Reply::UnknownCommand(unquote(cmd).to_string()));
    }
    Some(Reply::Error {
        message: message.to_string(),
        fatal: is_fatal_error(message),
    })
}

/// "Cap:AUTOREPORT_TEMP:1".
fn parse_capability(line: &str) -> Option<Reply> {
    let rest = strip_prefix_ci(line, "Cap:")?;
    let (name, value) = rest.rsplit_once(':')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let enabled = match value.trim() {
        "1" => true,
        "0" => false,
        _ => return None,
    };
    Some(Reply::Capability {
        name: name.to_string(),
        enabled,
    })
}

#[cfg(test)]
mod tests;
