//! `sprint analyze`: statistics of a G-code file, as a report or JSON.

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use sprint_gcode::{AnalyzeOptions, Bounds, GcodeFile, SlicerMetadata, Stats, Toolpath};
use sprint_tui::format;
use sprint_tui::widgets::stats::label_width;
use sprint_tui::{feature_rows, part_bounds, summary_rows};

use crate::cli::AnalyzeArgs;

/// Run `sprint analyze`.
pub fn run(args: &AnalyzeArgs) -> Result<()> {
    let file = load(&args.file)?;
    if file.command_count() == 0 {
        eprintln!(
            "sprint: warning: {} contains no G-code commands",
            args.file.display()
        );
    }
    let mut options = AnalyzeOptions::default();
    if let Some(diameter) = args.filament_diameter {
        options.filament_diameter = diameter;
    }
    let started = Instant::now();
    let stderr = io::stderr();
    let mut progress = Progress::new(stderr.is_terminal().then_some(stderr), file.name());
    let toolpath = sprint_gcode::analyze(&file, &options, &mut |fraction| {
        progress.update(fraction);
        true
    });
    progress.finish();
    let toolpath = toolpath.ok_or_else(|| anyhow!("the analysis was cancelled"))?;
    let elapsed = started.elapsed();

    let mut out = io::BufWriter::new(io::stdout().lock());
    let written = if args.json {
        write_json(&mut out, &file, &toolpath)
    } else {
        write_report(&mut out, &file, &toolpath, Some(elapsed))
    }
    .and_then(|()| out.flush());
    match written {
        // `sprint analyze x | head` is not an error.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => other.context("cannot write the output"),
    }
}

/// Load a G-code file with a friendly error.
pub fn load(path: &Path) -> Result<GcodeFile> {
    let shown = path.display();
    if path.is_dir() {
        return Err(anyhow!("{shown} is a directory, not a G-code file"));
    }
    GcodeFile::load(path).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => anyhow!("{shown}: no such file"),
        io::ErrorKind::PermissionDenied => anyhow!("{shown}: permission denied"),
        _ => anyhow::Error::new(e).context(format!("cannot read {shown}")),
    })
}

/// Do not show progress for analyses faster than this.
const PROGRESS_DELAY: Duration = Duration::from_millis(150);

/// "Analyzing name… 42%" on one terminal line, redrawn in place.
struct Progress<W: Write> {
    out: Option<W>,
    label: String,
    started: Instant,
    last: Option<u32>,
    drawn: usize,
}

impl<W: Write> Progress<W> {
    /// Progress written to `out` (`None` disables it).
    fn new(out: Option<W>, name: &str) -> Progress<W> {
        Progress {
            out,
            label: format!("Analyzing {name}"),
            started: Instant::now(),
            last: None,
            drawn: 0,
        }
    }

    fn update(&mut self, fraction: f32) {
        if self.started.elapsed() < PROGRESS_DELAY {
            return;
        }
        self.draw(fraction);
    }

    fn draw(&mut self, fraction: f32) {
        let Some(out) = self.out.as_mut() else {
            return;
        };
        let fraction = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        // In 0..=100 by the clamp above.
        let percent = (fraction * 100.0).floor() as u32;
        if self.last == Some(percent) {
            return;
        }
        self.last = Some(percent);
        let text = format!("{}… {percent:>3}%", self.label);
        // Progress is best effort: a closed stderr must not fail the run.
        if write!(out, "\r{text}").and_then(|()| out.flush()).is_ok() {
            self.drawn = text.chars().count();
        }
    }

    /// Erase the progress line.
    fn finish(&mut self) {
        if let Some(out) = self.out.as_mut()
            && self.drawn > 0
        {
            let blank = " ".repeat(self.drawn);
            let _ = write!(out, "\r{blank}\r").and_then(|()| out.flush());
            self.drawn = 0;
        }
    }
}

/// The human-readable report.
pub fn write_report(
    out: &mut impl Write,
    file: &GcodeFile,
    toolpath: &Toolpath,
    elapsed: Option<Duration>,
) -> io::Result<()> {
    writeln!(out, "{}", file.name())?;
    if let Some(path) = file.path() {
        writeln!(out, "  {}", path.display())?;
    }
    let mut facts = format!(
        "  {}, {} lines ({} commands)",
        format::bytes(u64::try_from(file.byte_len()).unwrap_or(u64::MAX)),
        format::count(file.len()),
        format::count(file.command_count()),
    );
    if let Some(elapsed) = elapsed {
        facts.push_str(&format!(", analyzed in {}", format_elapsed(elapsed)));
    }
    writeln!(out, "{facts}")?;
    writeln!(out)?;

    let bounds = part_bounds(toolpath);
    let rows = summary_rows(&toolpath.stats, &toolpath.metadata, Some(&bounds));
    let label_cols = label_width(&rows) + 2;
    for row in &rows {
        match &row.note {
            Some(note) => writeln!(out, "{:<label_cols$}{}  ({note})", row.label, row.value)?,
            None => writeln!(out, "{:<label_cols$}{}", row.label, row.value)?,
        }
    }

    let features = feature_rows(&toolpath.stats);
    if features.is_empty() {
        return Ok(());
    }
    let name_cols = features
        .iter()
        .map(|f| f.name.len())
        .chain(["Feature".len(), "Total".len()])
        .max()
        .unwrap_or(0);
    writeln!(out)?;
    writeln!(
        out,
        "{:<name_cols$}  {:>11}  {:>6}  {:>9}",
        "Feature", "Time", "Share", "Filament"
    )?;
    for f in &features {
        let filament = if f.feature.is_some() {
            format::length_mm(f64::from(f.filament_mm))
        } else {
            format::MISSING.to_string()
        };
        writeln!(
            out,
            "{:<name_cols$}  {:>11}  {:>6}  {:>9}",
            f.name,
            format::duration(f64::from(f.time)),
            format::percent(f64::from(f.share)),
            filament
        )?;
    }
    let stats = &toolpath.stats;
    let total_share = if stats.estimated_time > 0.0 {
        format::percent(1.0)
    } else {
        format::MISSING.to_string()
    };
    writeln!(
        out,
        "{:<name_cols$}  {:>11}  {:>6}  {:>9}",
        "Total",
        format::duration(f64::from(stats.estimated_time)),
        total_share,
        format::length_mm(f64::from(stats.filament_length))
    )?;
    Ok(())
}

/// "35 ms", "1.2 s".
fn format_elapsed(elapsed: Duration) -> String {
    let ms = elapsed.as_secs_f64() * 1000.0;
    if ms < 1000.0 {
        format!("{} ms", format::fixed(ms, 0))
    } else {
        format!("{} s", format::fixed(ms / 1000.0, 1))
    }
}

/// File facts in the JSON output.
#[derive(Debug, Serialize)]
struct JsonFile<'a> {
    name: &'a str,
    path: Option<String>,
    bytes: usize,
    lines: usize,
    commands: usize,
}

/// One layer in the JSON output.
#[derive(Debug, Serialize)]
struct JsonLayer {
    z: f32,
    height: f32,
    time_start: f32,
    duration: f32,
    filament_mm: f32,
    first_line: u32,
}

/// The JSON document: statistics, slicer metadata (thumbnails without
/// their image bytes), bounds and layers. Empty bounds and non-finite
/// numbers are `null`.
#[derive(Debug, Serialize)]
struct JsonReport<'a> {
    file: JsonFile<'a>,
    stats: &'a Stats,
    metadata: &'a SlicerMetadata,
    /// Bounds of every extrusion.
    bounds: &'a Bounds,
    /// Bounds of every move, travel included.
    travel_bounds: &'a Bounds,
    /// Bounds of the part without purge lines, skirt and wipe tower.
    part_bounds: Bounds,
    layers: Vec<JsonLayer>,
}

/// The JSON output, pretty-printed, with a final newline.
pub fn write_json(out: &mut impl Write, file: &GcodeFile, toolpath: &Toolpath) -> io::Result<()> {
    let report = JsonReport {
        file: JsonFile {
            name: file.name(),
            path: file.path().map(|p| p.display().to_string()),
            bytes: file.byte_len(),
            lines: file.len(),
            commands: file.command_count(),
        },
        stats: &toolpath.stats,
        metadata: &toolpath.metadata,
        bounds: &toolpath.bounds,
        travel_bounds: &toolpath.travel_bounds,
        part_bounds: part_bounds(toolpath),
        layers: toolpath
            .layers
            .iter()
            .map(|l| JsonLayer {
                z: l.z,
                height: l.height,
                time_start: l.time_start,
                duration: l.duration,
                filament_mm: l.extruded,
                first_line: l.lines.start,
            })
            .collect(),
    };
    serde_json::to_writer_pretty(&mut *out, &report).map_err(io::Error::from)?;
    writeln!(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_gcode::analyze_simple;

    const CUBE: &str = "\
; generated by PrusaSlicer 2.7.1\n\
M140 S60\nM104 S215\nM190 S60\nM109 S215\nG28\nG90\nM83\n\
;TYPE:Skirt\nG1 Z0.2 F600\nG1 X0 Y0 F9000\nG1 X40 Y0 E2 F1200\n\
;TYPE:External perimeter\nG1 X10 Y10 F9000\n\
G1 X30 Y10 E1 F1200\nG1 X30 Y30 E1\nG1 X10 Y30 E1\nG1 X10 Y10 E1\n\
G1 Z0.4 F600\nG1 X30 Y10 E1 F1200\nG2 X30 Y30 I0 J10 E1\n\
; estimated printing time (normal mode) = 1m 30s\n\
; filament used [mm] = 9.0\n";

    fn cube() -> (GcodeFile, Toolpath) {
        let file = GcodeFile::from_text("cube.gcode", CUBE);
        let toolpath = analyze_simple(&file, &AnalyzeOptions::default());
        (file, toolpath)
    }

    #[test]
    fn report_lists_the_key_facts() {
        let (file, toolpath) = cube();
        let mut out = Vec::new();
        write_report(&mut out, &file, &toolpath, Some(Duration::from_millis(12))).expect("write");
        let text = String::from_utf8(out).expect("utf-8");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "cube.gcode");
        assert!(
            lines[1].contains("lines") && lines[1].ends_with("analyzed in 12 ms"),
            "{text}"
        );
        for fact in [
            "Slicer       PrusaSlicer 2.7.1",
            "Print time   ",
            "(slicer 1m 30s, Sprint ",
            "Filament     ",
            "(slicer 9.0 mm)",
            "Size         20.0 x 20.0 x 0.4 mm",
            "Layers       2, 0.20 mm",
            "Hotend       215 °C",
            "Bed          60 °C",
            "Thumbnails   none",
            "Flags        relative E, arcs (G2/G3), 2 heat waits",
            "External perimeter",
            "Skirt / brim",
            "Travel",
            "Total",
        ] {
            assert!(text.contains(fact), "missing {fact:?} in\n{text}");
        }
        // The total row adds up to 100%.
        let total = lines
            .iter()
            .find(|l| l.starts_with("Total"))
            .expect("total");
        assert!(total.contains("100.0%"), "{total}");
    }

    #[test]
    fn report_of_an_empty_file() {
        let file = GcodeFile::from_text("empty.gcode", "");
        let toolpath = analyze_simple(&file, &AnalyzeOptions::default());
        let mut out = Vec::new();
        write_report(&mut out, &file, &toolpath, None).expect("write");
        let text = String::from_utf8(out).expect("utf-8");
        assert!(text.contains("0 B, 0 lines (0 commands)"), "{text}");
        assert!(
            text.lines()
                .any(|l| l.starts_with("Slicer ") && l.ends_with(" unknown")),
            "{text}"
        );
        assert!(!text.contains("Total"), "{text}");
    }

    #[test]
    fn json_matches_the_library() {
        let (file, toolpath) = cube();
        let mut out = Vec::new();
        write_json(&mut out, &file, &toolpath).expect("write");
        let json: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
        let number = |v: &serde_json::Value| v.as_f64().map(|n| n as f32);
        assert_eq!(json["file"]["name"], "cube.gcode");
        assert_eq!(json["file"]["lines"], file.len());
        assert_eq!(
            number(&json["stats"]["estimated_time"]),
            Some(toolpath.stats.estimated_time)
        );
        assert_eq!(
            number(&json["stats"]["filament_length"]),
            Some(toolpath.stats.filament_length)
        );
        assert_eq!(json["stats"]["layer_count"], toolpath.stats.layer_count);
        assert_eq!(json["stats"]["uses_arcs"], true);
        assert_eq!(json["metadata"]["slicer"], "PrusaSlicer 2.7.1");
        assert_eq!(
            number(&json["bounds"]["max"][0]),
            Some(toolpath.bounds.max[0])
        );
        assert_eq!(number(&json["part_bounds"]["min"][0]), Some(10.0));
        assert_eq!(
            json["layers"].as_array().map(Vec::len),
            Some(toolpath.layers.len())
        );
        assert!(out.ends_with(b"}\n"));
    }

    #[test]
    fn json_turns_empty_bounds_into_null() {
        let file = GcodeFile::from_text("travel.gcode", "G28\nG1 X10 Y10\n");
        let toolpath = analyze_simple(&file, &AnalyzeOptions::default());
        let mut out = Vec::new();
        write_json(&mut out, &file, &toolpath).expect("write");
        let json: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
        assert!(json["bounds"]["min"][0].is_null(), "{json}");
        assert!(json["travel_bounds"]["max"][0].is_number(), "{json}");
    }

    #[test]
    fn json_leaves_out_thumbnail_bytes() {
        let text = "; thumbnail begin 2x2 4\n; AAAA\n; thumbnail end\nG28\n";
        let file = GcodeFile::from_text("thumb.gcode", text);
        let toolpath = analyze_simple(&file, &AnalyzeOptions::default());
        let mut out = Vec::new();
        write_json(&mut out, &file, &toolpath).expect("write");
        let json: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
        let thumbs = json["metadata"]["thumbnails"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for thumb in thumbs {
            assert!(thumb.get("data").is_none(), "{thumb}");
        }
    }

    #[test]
    fn load_errors_are_friendly() {
        let missing = Path::new("definitely-not-here-4b1d.gcode");
        let err = load(missing).expect_err("missing file").to_string();
        assert_eq!(err, "definitely-not-here-4b1d.gcode: no such file");
        let dir = std::env::temp_dir();
        let err = load(&dir).expect_err("directory").to_string();
        assert!(err.ends_with("is a directory, not a G-code file"), "{err}");
    }

    #[test]
    fn progress_draws_percentages_and_erases_itself() {
        let mut progress = Progress::new(Some(Vec::new()), "cube.gcode");
        progress.draw(f32::NAN);
        progress.draw(0.004);
        progress.draw(0.425);
        progress.draw(0.429);
        progress.draw(7.0);
        progress.finish();
        let text = String::from_utf8(progress.out.take().unwrap_or_default()).expect("utf-8");
        assert_eq!(text.matches("Analyzing cube.gcode…").count(), 3, "{text:?}");
        assert!(text.contains("  0%") && text.contains(" 42%") && text.contains("100%"));
        assert!(
            text.ends_with(&format!("\r{}\r", " ".repeat(26))),
            "{text:?}"
        );
        // Disabled progress writes nothing and does not fail.
        let mut off: Progress<Vec<u8>> = Progress::new(None, "x");
        off.draw(0.5);
        off.finish();
        // Fast analyses never show progress.
        let mut quick = Progress::new(Some(Vec::new()), "x");
        quick.update(0.5);
        quick.finish();
        assert!(quick.out.take().is_some_and(|o| o.is_empty()));
    }

    #[test]
    fn elapsed_formatting() {
        assert_eq!(format_elapsed(Duration::from_millis(35)), "35 ms");
        assert_eq!(format_elapsed(Duration::from_millis(1250)), "1.2 s");
    }
}
