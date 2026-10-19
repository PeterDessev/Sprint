//! Print jobs: options, the job record in the state, and the progress / ETA
//! model.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use sprint_gcode::Toolpath;

/// Options for [`Printer::start_print`](crate::Printer::start_print).
#[derive(Clone, Default)]
pub struct PrintOptions {
    /// Stream from this line (0-based; 0 = beginning).
    pub start_line: usize,
    /// Commands sent before the first streamed line, as part of the job
    /// (for example [`resume_preamble`](sprint_gcode::resume_preamble)).
    pub preamble: Vec<String>,
    /// Enables time-based progress, ETA, layer and filament tracking.
    pub toolpath: Option<Arc<Toolpath>>,
    /// Overrides the profile's pause script (templated, see
    /// [`script`](crate::script)).
    pub pause_script: Option<String>,
    /// Overrides the profile's resume script.
    pub resume_script: Option<String>,
    /// Overrides the profile's cancel script.
    pub cancel_script: Option<String>,
    /// Send `M73 P<pct> R<min>` whenever the whole percentage changes (also
    /// when the profile's
    /// [`emit_progress`](crate::PrinterProfile::emit_progress) is set).
    pub emit_progress: bool,
}

impl fmt::Debug for PrintOptions {
    // The toolpath can hold millions of segments; summarise it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrintOptions")
            .field("start_line", &self.start_line)
            .field("preamble", &self.preamble)
            .field(
                "toolpath",
                &self
                    .toolpath
                    .as_ref()
                    .map(|t| format!("Toolpath({} segments)", t.segments.len())),
            )
            .field("pause_script", &self.pause_script)
            .field("resume_script", &self.resume_script)
            .field("cancel_script", &self.cancel_script)
            .field("emit_progress", &self.emit_progress)
            .finish()
    }
}

/// What a job does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    /// Streaming a file from the host.
    HostPrint,
    /// Printing a file stored on the printer's SD card.
    SdPrint,
    /// Writing a file to the SD card (M28 ... M29).
    SdUpload,
}

/// Where a job is in its life.
#[derive(Clone, Debug, PartialEq)]
pub enum JobStatus {
    /// Set up; the first command has not been acknowledged yet.
    Starting,
    /// Running.
    Printing,
    /// Waiting for in-flight commands, then running the pause script.
    Pausing,
    /// Paused (by the user or the firmware).
    Paused,
    /// Running the resume script.
    Resuming,
    /// Running the cancel script.
    Cancelling,
    /// Finished normally.
    Completed,
    /// Cancelled by the user or the firmware.
    Cancelled,
    /// Stopped by an error (halt, reset, lost link, ...).
    Failed(String),
}

impl JobStatus {
    /// Not finished yet (`Starting` ..= `Cancelling`).
    pub fn is_active(&self) -> bool {
        !self.is_finished()
    }

    /// `Completed`, `Cancelled` or `Failed`.
    pub fn is_finished(&self) -> bool {
        matches!(
            self,
            JobStatus::Completed | JobStatus::Cancelled | JobStatus::Failed(_)
        )
    }

    /// `Paused` or `Pausing`.
    pub fn is_paused(&self) -> bool {
        matches!(self, JobStatus::Paused | JobStatus::Pausing)
    }

    /// Short label for status bars.
    pub fn label(&self) -> &str {
        match self {
            JobStatus::Starting => "Starting",
            JobStatus::Printing => "Printing",
            JobStatus::Pausing => "Pausing",
            JobStatus::Paused => "Paused",
            JobStatus::Resuming => "Resuming",
            JobStatus::Cancelling => "Cancelling",
            JobStatus::Completed => "Completed",
            JobStatus::Cancelled => "Cancelled",
            JobStatus::Failed(_) => "Failed",
        }
    }
}

/// The current or last job.
#[derive(Clone, Debug, PartialEq)]
pub struct JobInfo {
    /// Host print, SD print or SD upload.
    pub kind: JobKind,
    /// File name.
    pub name: String,
    /// Host prints and uploads: the file on disk
    /// ([`GcodeFile::path`](sprint_gcode::GcodeFile::path)), so a UI can
    /// tell exactly which loaded file the job is; `None` for files not
    /// loaded from disk and for SD prints.
    pub path: Option<std::path::PathBuf>,
    /// Status.
    pub status: JobStatus,
    /// Lines in the file (0 for SD prints).
    pub total_lines: usize,
    /// Lines processed: the 1-based number of the last file line the
    /// printer acknowledged (comment lines before it included), so it
    /// reaches `total_lines` when the job completes. Starts at
    /// `start_line`.
    pub current_line: usize,
    /// File size in bytes.
    pub bytes_total: u64,
    /// Bytes up to and including `current_line` (SD prints: as reported by
    /// M27).
    pub bytes_done: u64,
    /// Running time, excluding time spent paused.
    pub elapsed: Duration,
    /// 0..=1 of the whole file; time-based when a toolpath is known,
    /// otherwise by bytes.
    pub progress: f32,
    /// Remaining time, corrected by the actual/estimated speed ratio.
    pub eta: Option<Duration>,
    /// `elapsed + eta`, or the analysis estimate before an ETA exists.
    pub estimated_total: Option<Duration>,
    /// (current layer index, layer count), with a toolpath.
    pub layer: Option<(usize, usize)>,
    /// Filament pushed so far (mm), with a toolpath.
    pub filament_used_mm: f32,
    /// Host prints: 0-based file lines the job pauses before (with the
    /// pause script), still ahead of the stream, ascending
    /// ([`Printer::pause_at_lines`](crate::Printer::pause_at_lines)).
    pub pause_at: Vec<usize>,
    /// Host prints: the objects of the file (object labels: Klipper
    /// `EXCLUDE_OBJECT_*`, Marlin `M486`, `; printing object`, Cura
    /// `;MESH:`), in first-seen order.
    pub objects: Vec<String>,
    /// The objects excluded while printing
    /// ([`Printer::exclude_object`](crate::Printer::exclude_object)).
    pub excluded_objects: Vec<String>,
    /// The object the stream is in (as sent to the printer).
    pub current_object: Option<String>,
    /// A failed or cancelled host print: the 0-based index of the first
    /// file line the printer did not acknowledge, where a recovery resumes
    /// ([`PrintOptions::start_line`] with
    /// [`resume_preamble`](sprint_gcode::resume_preamble)). Kept with the
    /// job record until the next job starts; `None` otherwise.
    pub stopped_at: Option<usize>,
}

impl JobInfo {
    /// A `Starting` job.
    pub fn new(
        kind: JobKind,
        name: impl Into<String>,
        total_lines: usize,
        bytes_total: u64,
    ) -> JobInfo {
        JobInfo {
            kind,
            name: name.into(),
            path: None,
            status: JobStatus::Starting,
            total_lines,
            current_line: 0,
            bytes_total,
            bytes_done: 0,
            elapsed: Duration::ZERO,
            progress: 0.0,
            eta: None,
            estimated_total: None,
            layer: None,
            filament_used_mm: 0.0,
            pause_at: Vec::new(),
            objects: Vec::new(),
            excluded_objects: Vec::new(),
            current_object: None,
            stopped_at: None,
        }
    }
}

/// Fraction of the job below which the ETA uses the raw estimate.
const RAW_ETA_FRACTION: f64 = 0.02;
/// Bounds of the actual/estimated speed ratio.
const RATIO_MIN: f64 = 0.5;
const RATIO_MAX: f64 = 3.0;
/// Smoothing: the ratio (or byte-based ETA) moves this fraction of the way
/// to its new value per [`SMOOTH_PERIOD`] of job time.
const SMOOTH_ALPHA: f64 = 0.2;
const SMOOTH_PERIOD: f64 = 0.5;

/// Progress values computed by [`ProgressModel::update`] (the same
/// meaning as the [`JobInfo`] fields of the same names).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Progress {
    /// 0..=1 of the whole file.
    pub progress: f32,
    /// Remaining time.
    pub eta: Option<Duration>,
    /// Elapsed plus remaining time.
    pub estimated_total: Option<Duration>,
    /// (current layer index, layer count).
    pub layer: Option<(usize, usize)>,
    /// Filament pushed since the job started (mm).
    pub filament_used_mm: f32,
}

/// Progress, ETA, layer and filament for a running job.
///
/// With a toolpath, progress is the estimated print time of the
/// acknowledged lines over the total estimate. The ETA is the remaining
/// estimate times the measured speed ratio (actual running time without
/// the waits estimates count as zero, over the estimated time of what was
/// printed), clamped to 0.5..=3 and smoothed; during the first 2% of the
/// job the raw estimate is used. Without a toolpath, progress is by bytes and the ETA is a
/// smoothed linear extrapolation once 2% are done.
pub struct ProgressModel {
    toolpath: Option<Arc<Toolpath>>,
    /// `filament[i]` = filament of segments `0..i` (len = segments + 1).
    filament: Vec<f32>,
    /// Segment index where the job starts (first segment at or after
    /// `start_line`).
    start_segment: usize,
    start_line: usize,
    bytes_start: u64,
    bytes_total: u64,
    smoothed: Option<f64>,
    last_smooth: f64,
}

fn duration(secs: f64) -> Duration {
    if secs.is_finite() && secs > 0.0 {
        Duration::try_from_secs_f64(secs).unwrap_or(Duration::MAX)
    } else {
        Duration::ZERO
    }
}

fn line_u32(line: usize) -> u32 {
    u32::try_from(line).unwrap_or(u32::MAX)
}

impl ProgressModel {
    /// A model for a job streaming from `start_line` (byte offset
    /// `bytes_start`) of a file of `bytes_total` bytes.
    pub fn new(
        toolpath: Option<Arc<Toolpath>>,
        start_line: usize,
        bytes_start: u64,
        bytes_total: u64,
    ) -> ProgressModel {
        let toolpath = toolpath.filter(|t| t.stats.estimated_time.is_finite());
        let (filament, start_segment) = match &toolpath {
            Some(t) => {
                let mut prefix = Vec::with_capacity(t.segments.len() + 1);
                let mut sum = 0.0f64;
                prefix.push(0.0f32);
                for s in &t.segments {
                    if s.extruded.is_finite() && s.extruded > 0.0 {
                        sum += f64::from(s.extruded);
                    }
                    prefix.push(sum as f32);
                }
                let start = line_u32(start_line);
                let start_segment = t.segments.partition_point(|s| s.line < start);
                (prefix, start_segment)
            }
            None => (Vec::new(), 0),
        };
        ProgressModel {
            toolpath,
            filament,
            start_segment,
            start_line,
            bytes_start,
            bytes_total,
            smoothed: None,
            last_smooth: f64::NEG_INFINITY,
        }
    }

    /// Exponential smoothing at a fixed cadence of job time.
    fn smooth(&mut self, value: f64, elapsed: f64) -> f64 {
        match self.smoothed {
            None => {
                self.smoothed = Some(value);
                self.last_smooth = elapsed;
                value
            }
            Some(prev) => {
                if elapsed - self.last_smooth >= SMOOTH_PERIOD {
                    let next = prev + (value - prev) * SMOOTH_ALPHA;
                    self.smoothed = Some(next);
                    self.last_smooth = elapsed;
                    next
                } else {
                    prev
                }
            }
        }
    }

    /// Recompute. `done_line` is the 0-based last acknowledged file line,
    /// `elapsed` the running time excluding pauses and `waiting` the part
    /// of it spent in commands estimates count as zero (heat waits,
    /// homing, probing, waits for the user).
    pub fn update(
        &mut self,
        done_line: Option<usize>,
        bytes_done: u64,
        elapsed: f64,
        waiting: f64,
    ) -> Progress {
        let elapsed = if elapsed.is_finite() {
            elapsed.max(0.0)
        } else {
            0.0
        };
        let actual = (elapsed - waiting.max(0.0)).max(0.0);
        if let Some(t) = self.toolpath.clone()
            && t.stats.estimated_time > 0.0
        {
            return self.update_toolpath(&t, done_line, elapsed, actual);
        }
        self.update_bytes(bytes_done, elapsed, actual)
    }

    fn update_toolpath(
        &mut self,
        t: &Toolpath,
        done_line: Option<usize>,
        elapsed: f64,
        actual: f64,
    ) -> Progress {
        let total = f64::from(t.stats.estimated_time);
        let est_start = match self.start_line.checked_sub(1) {
            Some(l) => f64::from(t.time_at_line(line_u32(l))),
            None => 0.0,
        };
        let est_at = done_line
            .map_or(est_start, |l| f64::from(t.time_at_line(line_u32(l))))
            .max(est_start);
        let remaining = (total - est_at).max(0.0);
        let done_est = est_at - est_start;
        let span = total - est_start;
        let fraction = if span > 0.0 { done_est / span } else { 1.0 };
        let eta = if fraction < RAW_ETA_FRACTION || done_est <= 0.0 {
            remaining
        } else {
            let ratio = (actual / done_est).clamp(RATIO_MIN, RATIO_MAX);
            remaining * self.smooth(ratio, elapsed)
        };
        let line = done_line.unwrap_or(self.start_line);
        let layer = t.layer_at_line(line_u32(line)).map(|i| (i, t.layers.len()));
        let filament = match done_line {
            Some(l) => {
                let end = t.segments.partition_point(|s| s.line <= line_u32(l));
                let used = self.filament.get(end).copied().unwrap_or(0.0)
                    - self
                        .filament
                        .get(self.start_segment)
                        .copied()
                        .unwrap_or(0.0);
                used.max(0.0)
            }
            None => 0.0,
        };
        Progress {
            progress: (est_at / total).clamp(0.0, 1.0) as f32,
            eta: Some(duration(eta)),
            estimated_total: Some(duration(elapsed + eta)),
            layer,
            filament_used_mm: filament,
        }
    }

    fn update_bytes(&mut self, bytes_done: u64, elapsed: f64, actual: f64) -> Progress {
        let total = self.bytes_total;
        if total == 0 {
            return Progress::default();
        }
        let done = bytes_done.min(total);
        let progress = done as f64 / total as f64;
        let span = total.saturating_sub(self.bytes_start);
        let fraction = if span > 0 {
            done.saturating_sub(self.bytes_start) as f64 / span as f64
        } else {
            1.0
        };
        let eta = if fraction < RAW_ETA_FRACTION {
            None
        } else if fraction >= 1.0 {
            Some(0.0)
        } else {
            let raw = actual * (1.0 - fraction) / fraction;
            Some(self.smooth(raw, elapsed))
        };
        Progress {
            progress: progress as f32,
            eta: eta.map(duration),
            estimated_total: eta.map(|e| duration(elapsed + e)),
            layer: None,
            filament_used_mm: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_gcode::{AnalyzeOptions, GcodeFile, analyze_simple};

    fn toolpath() -> (GcodeFile, Arc<Toolpath>) {
        let mut text = String::from("G28\nG1 Z0.2 F600\nM83\n");
        for layer in 0..10 {
            text.push_str(&format!("G1 Z{:.1} F600\n", 0.2 + 0.2 * f64::from(layer)));
            for i in 0..10 {
                let x = if i % 2 == 0 { 10.0 } else { 110.0 };
                text.push_str(&format!("G1 X{x} Y{} E2 F1800\n", 10 + i * 5));
            }
        }
        let file = GcodeFile::from_text("t.gcode", text);
        let tp = analyze_simple(&file, &AnalyzeOptions::default());
        (file, Arc::new(tp))
    }

    #[test]
    fn statuses() {
        assert!(JobStatus::Starting.is_active());
        assert!(JobStatus::Paused.is_active() && JobStatus::Paused.is_paused());
        assert!(JobStatus::Failed("x".into()).is_finished());
        assert!(!JobStatus::Completed.is_active());
        assert_eq!(JobStatus::Cancelling.label(), "Cancelling");
    }

    #[test]
    fn toolpath_progress_layers_and_filament() {
        let (file, tp) = toolpath();
        let total = tp.stats.estimated_time;
        assert!(total > 0.0);
        let mut m = ProgressModel::new(Some(tp.clone()), 0, 0, file.byte_len() as u64);
        let p = m.update(None, 0, 0.0, 0.0);
        assert_eq!(p.progress, 0.0);
        assert_eq!(p.filament_used_mm, 0.0);
        let raw = p.eta.unwrap().as_secs_f32();
        assert!((raw - total).abs() < 0.01, "{raw} vs {total}");

        let last = file.len() - 1;
        let p = m.update(Some(last), 0, f64::from(total), 0.0);
        assert!((p.progress - 1.0).abs() < 1e-6);
        assert_eq!(p.eta, Some(Duration::ZERO));
        assert_eq!(p.layer, Some((tp.layers.len() - 1, tp.layers.len())));
        assert!(
            (p.filament_used_mm - 200.0).abs() < 0.01,
            "{}",
            p.filament_used_mm
        );
    }

    #[test]
    fn eta_is_corrected_by_the_measured_speed() {
        let (file, tp) = toolpath();
        let total = f64::from(tp.stats.estimated_time);
        // Find the line at about half of the estimated time.
        let half = (0..file.len())
            .find(|&l| f64::from(tp.time_at_line(l as u32)) >= total / 2.0)
            .unwrap();
        let est_half = f64::from(tp.time_at_line(half as u32));
        let remaining = total - est_half;

        // Twice as slow as estimated: ETA doubles.
        let mut m = ProgressModel::new(Some(tp.clone()), 0, 0, 1);
        let p = m.update(Some(half), 0, est_half * 2.0, 0.0);
        let eta = p.eta.unwrap().as_secs_f64();
        assert!(
            (eta - remaining * 2.0).abs() < 0.01,
            "{eta} vs {}",
            remaining * 2.0
        );
        assert!((p.estimated_total.unwrap().as_secs_f64() - (est_half * 2.0 + eta)).abs() < 0.01);

        // Heat-wait time does not count as slowness.
        let mut m = ProgressModel::new(Some(tp.clone()), 0, 0, 1);
        let p = m.update(Some(half), 0, est_half + 300.0, 300.0);
        assert!((p.eta.unwrap().as_secs_f64() - remaining).abs() < 0.01);

        // The ratio is clamped to 0.5..3.
        let mut m = ProgressModel::new(Some(tp.clone()), 0, 0, 1);
        let p = m.update(Some(half), 0, est_half * 100.0, 0.0);
        assert!((p.eta.unwrap().as_secs_f64() - remaining * 3.0).abs() < 0.01);
        let mut m = ProgressModel::new(Some(tp), 0, 0, 1);
        let p = m.update(Some(half), 0, est_half * 0.01, 0.0);
        assert!((p.eta.unwrap().as_secs_f64() - remaining * 0.5).abs() < 0.01);
    }

    #[test]
    fn ratio_is_smoothed() {
        let (file, tp) = toolpath();
        let total = f64::from(tp.stats.estimated_time);
        let half = (0..file.len())
            .find(|&l| f64::from(tp.time_at_line(l as u32)) >= total / 2.0)
            .unwrap();
        let est = f64::from(tp.time_at_line(half as u32));
        let remaining = total - est;
        let mut m = ProgressModel::new(Some(tp), 0, 0, 1);
        let first = m.update(Some(half), 0, est, 0.0).eta.unwrap().as_secs_f64();
        assert!((first - remaining).abs() < 0.01);
        // A sudden 3x ratio moves the ETA only part of the way.
        let next = m
            .update(Some(half), 0, est * 3.0, 0.0)
            .eta
            .unwrap()
            .as_secs_f64();
        assert!(next > first && next < remaining * 3.0, "{first} {next}");
    }

    #[test]
    fn starting_mid_file_counts_from_the_start_line() {
        let (file, tp) = toolpath();
        let start = file.len() / 2;
        let mut m = ProgressModel::new(Some(tp.clone()), start, 0, 1);
        let p = m.update(None, 0, 0.0, 0.0);
        assert!(p.progress > 0.3 && p.progress < 0.7, "{}", p.progress);
        assert_eq!(p.filament_used_mm, 0.0);
        let p = m.update(Some(file.len() - 1), 0, 10.0, 0.0);
        assert!(p.filament_used_mm > 50.0 && p.filament_used_mm < 150.0);
    }

    #[test]
    fn byte_progress_without_toolpath() {
        let mut m = ProgressModel::new(None, 0, 0, 1000);
        let p = m.update(None, 10, 5.0, 0.0);
        assert_eq!(p.progress, 0.01);
        assert_eq!(p.eta, None, "no ETA during the first 2%");
        let p = m.update(None, 500, 100.0, 0.0);
        assert_eq!(p.progress, 0.5);
        assert_eq!(p.eta, Some(Duration::from_secs(100)));
        assert_eq!(p.estimated_total, Some(Duration::from_secs(200)));
        let p = m.update(None, 5000, 200.0, 0.0);
        assert_eq!(p.progress, 1.0);
        assert_eq!(p.eta, Some(Duration::ZERO));
        assert_eq!(
            ProgressModel::new(None, 0, 0, 0).update(None, 0, 1.0, 0.0),
            Progress::default()
        );
    }

    #[test]
    fn garbage_inputs_are_harmless() {
        let (_, tp) = toolpath();
        let mut m = ProgressModel::new(Some(tp), usize::MAX, u64::MAX, 3);
        let p = m.update(Some(usize::MAX), u64::MAX, f64::NAN, f64::INFINITY);
        assert!(p.progress.is_finite());
        let mut m = ProgressModel::new(Some(Arc::new(Toolpath::default())), 0, 0, 10);
        let p = m.update(Some(3), 5, 1.0, 0.0);
        assert_eq!(p.progress, 0.5);
    }

    #[test]
    fn debug_summarises_the_toolpath() {
        let (_, tp) = toolpath();
        let o = PrintOptions {
            toolpath: Some(tp),
            ..PrintOptions::default()
        };
        let s = format!("{o:?}");
        assert!(s.contains("segments)") && s.len() < 400, "{s}");
    }
}
