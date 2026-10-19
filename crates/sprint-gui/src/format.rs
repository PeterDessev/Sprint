//! Text formatting shared by the app shell: durations, clock times, sizes,
//! counts and the window title.
//!
//! Every function accepts garbage (NaN, infinities, negative values) and
//! answers with a placeholder instead of panicking.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sprint_gcode::GcodeFile;
use sprint_host::{JobInfo, JobKind, JobStatus};

/// Shown for values that are unknown or not finite.
pub const UNKNOWN: &str = "—";

/// A duration in seconds as `"1h 02m"`, `"12m 05s"` or `"45s"`
/// (days as `"2d 03h"`); [`UNKNOWN`] for negative or non-finite input.
pub fn duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return UNKNOWN.to_owned();
    }
    let total = seconds.round() as u64;
    let (d, h, m, s) = (
        total / 86_400,
        (total / 3600) % 24,
        (total / 60) % 60,
        total % 60,
    );
    if d > 0 {
        format!("{d}d {h:02}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// [`duration`] of a [`Duration`].
pub fn duration_of(d: Duration) -> String {
    duration(d.as_secs_f64())
}

/// A stopwatch reading: `"1:02:03"` or `"02:03"`.
pub fn stopwatch(d: Duration) -> String {
    let total = d.as_secs();
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Seconds east of UTC of the local time zone, looked up once (UTC when the
/// platform cannot tell).
fn local_offset_seconds() -> i64 {
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        time::UtcOffset::current_local_offset()
            .map(|o| i64::from(o.whole_seconds()))
            .unwrap_or(0)
    })
}

/// No frozen clock (see [`freeze_clock`]).
const NOT_FROZEN: i64 = i64::MIN;
static FROZEN_CLOCK: AtomicI64 = AtomicI64::new(NOT_FROZEN);

/// Make [`clock_after`] count from a fixed local time (seconds since the
/// epoch, already in local time) instead of the wall clock, or go back to
/// the wall clock with `None`. For reproducible screenshots and tests.
pub fn freeze_clock(local_now: Option<i64>) {
    FROZEN_CLOCK.store(local_now.unwrap_or(NOT_FROZEN), Ordering::Relaxed);
}

/// The local wall-clock time `after` from now, as `"14:05"`; with a day
/// suffix (`"14:05 +1d"`) when it falls on a later day.
pub fn clock_after(after: Duration) -> String {
    let after = after.as_secs().min(10 * 365 * 86_400) as i64;
    let frozen = FROZEN_CLOCK.load(Ordering::Relaxed);
    if frozen != NOT_FROZEN {
        return clock_at(frozen, after, 0);
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    clock_at(now, after, local_offset_seconds())
}

/// Clock time of `now + after` (unix seconds) in a zone `offset` seconds
/// east of UTC.
pub fn clock_at(now: i64, after: i64, offset: i64) -> String {
    let local_now = now.saturating_add(offset);
    let local_then = local_now.saturating_add(after.max(0));
    let day_now = local_now.div_euclid(86_400);
    let day_then = local_then.div_euclid(86_400);
    let secs = local_then.rem_euclid(86_400);
    let (h, m) = (secs / 3600, (secs / 60) % 60);
    let days = day_then - day_now;
    if days > 0 {
        format!("{h:02}:{m:02} +{days}d")
    } else {
        format!("{h:02}:{m:02}")
    }
}

/// A filament length: `"456 mm"` below a metre, `"3.21 m"` above.
pub fn length_mm(mm: f64) -> String {
    if !mm.is_finite() {
        return UNKNOWN.to_owned();
    }
    if mm.abs() < 1000.0 {
        format!("{mm:.0} mm")
    } else {
        format!("{:.2} m", mm / 1000.0)
    }
}

/// A byte count: `"512 B"`, `"12.3 KB"`, `"4.56 MB"`, `"1.20 GB"`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.2} {}", UNITS[unit])
    } else if value < 100.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// An integer with thousands separators: `"1,234,567"`.
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A millimetre value with up to `decimals` decimals and trailing zeros
/// trimmed: `"0.2"`, `"12.35"`, `"20"`.
pub fn mm(value: f32, decimals: usize) -> String {
    if !value.is_finite() {
        return UNKNOWN.to_owned();
    }
    let s = format!("{value:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        s
    }
}

/// Relative difference of `ours` against `theirs` as `"+4%"` / `"−3%"`;
/// `None` when either is missing or `theirs` is zero.
pub fn relative_difference(ours: f64, theirs: f64) -> Option<String> {
    if !ours.is_finite() || !theirs.is_finite() || theirs.abs() < 1e-9 {
        return None;
    }
    let pct = (ours / theirs - 1.0) * 100.0;
    let rounded = pct.round() as i64;
    Some(match rounded.signum() {
        1 => format!("+{rounded}%"),
        -1 => format!("\u{2212}{}%", -rounded),
        _ => "±0%".to_owned(),
    })
}

/// Whether `job` is a host print or upload of `file`: the same file on
/// disk (see [`same_source`]).
pub fn job_matches_file(job: &JobInfo, file: &GcodeFile) -> bool {
    job.kind != JobKind::SdPrint && same_source(job, file)
}

/// Whether `job` prints `file` from the host (an upload to the SD card
/// streams the file too, but nothing is printed).
pub fn job_prints_file(job: &JobInfo, file: &GcodeFile) -> bool {
    job.kind == JobKind::HostPrint && same_source(job, file)
}

/// Whether `job` streams (or streamed) `file`: the same path on disk
/// ([`JobInfo::path`]; two files of the same name in different folders
/// are different files), or, for text that never came from disk, the same
/// name.
pub fn same_source(job: &JobInfo, file: &GcodeFile) -> bool {
    match (job.path.as_deref(), file.path()) {
        (Some(job_path), Some(path)) => crate::loader::same_path(job_path, path),
        (None, None) => job.name == file.name(),
        _ => false,
    }
}

/// The window title: `"Sprint"`, `"Sprint — cube.gcode"`, and while a job
/// runs `"Sprint — cube.gcode — 42% · layer 12/100"` (`"Paused 42%"` while
/// paused). Job percentages are floored so 100% only shows when done.
pub fn window_title(file: Option<&str>, job: Option<&JobInfo>) -> String {
    let mut title = String::from("Sprint");
    let active = job.filter(|j| j.status.is_active());
    let name = match active {
        Some(job) => Some(job.name.as_str()),
        None => file,
    };
    if let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) {
        title.push_str(" — ");
        title.push_str(&sanitize(name, 80));
    }
    if let Some(job) = active {
        let pct = progress_percent(job.progress);
        title.push_str(" — ");
        match job.status {
            JobStatus::Paused | JobStatus::Pausing => title.push_str(&format!("Paused {pct}%")),
            _ if job.kind == JobKind::SdUpload => title.push_str(&format!("Uploading {pct}%")),
            _ => title.push_str(&format!("{pct}%")),
        }
        if let Some((index, total)) = job.layer
            && total > 0
        {
            title.push_str(&format!(" · layer {}/{}", (index + 1).min(total), total));
        }
    }
    title
}

/// A progress fraction as a whole percentage, floored (so a job reads 100%
/// only when complete); 0 for garbage.
pub fn progress_percent(fraction: f32) -> u32 {
    if fraction.is_finite() {
        (fraction.clamp(0.0, 1.0) * 100.0).floor() as u32
    } else {
        0
    }
}

/// `text` with control characters replaced and cut to `max_chars`
/// characters (an ellipsis marks the cut). For untrusted names and printer
/// messages.
pub fn sanitize(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max_chars * 4));
    for (n, c) in text.chars().enumerate() {
        if n >= max_chars {
            out.push('…');
            break;
        }
        out.push(if c.is_control() { ' ' } else { c });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration(0.0), "0s");
        assert_eq!(duration(44.6), "45s");
        assert_eq!(duration(725.0), "12m 05s");
        assert_eq!(duration(3_720.0), "1h 02m");
        assert_eq!(duration(2.0 * 86_400.0 + 3.0 * 3600.0), "2d 03h");
        assert_eq!(duration(f64::NAN), UNKNOWN);
        assert_eq!(duration(-1.0), UNKNOWN);
        assert_eq!(duration(f64::INFINITY), UNKNOWN);
        assert_eq!(stopwatch(Duration::from_secs(3723)), "1:02:03");
        assert_eq!(stopwatch(Duration::from_secs(123)), "02:03");
    }

    #[test]
    fn clock_times() {
        // 2024-01-01 12:00:00 UTC.
        let noon = 1_704_110_400;
        assert_eq!(clock_at(noon, 0, 0), "12:00");
        assert_eq!(clock_at(noon, 90 * 60, 0), "13:30");
        assert_eq!(clock_at(noon, 0, 2 * 3600), "14:00");
        assert_eq!(clock_at(noon, 13 * 3600, 0), "01:00 +1d");
        assert_eq!(clock_at(noon, -50, -7 * 3600), "05:00");
        // Never panics on extremes.
        let _ = clock_at(i64::MAX, i64::MAX, i64::MAX);
        let _ = clock_after(Duration::MAX);
        // A frozen clock is reproducible.
        freeze_clock(Some(noon));
        assert_eq!(clock_after(Duration::from_secs(90 * 60)), "13:30");
        freeze_clock(None);
    }

    #[test]
    fn sizes_and_counts() {
        assert_eq!(length_mm(456.4), "456 mm");
        assert_eq!(length_mm(3210.0), "3.21 m");
        assert_eq!(length_mm(f64::NAN), UNKNOWN);
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(12_600), "12.3 KB");
        assert_eq!(bytes(4_780_000), "4.56 MB");
        assert_eq!(bytes(u64::MAX), "16777216 TB");
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1,000");
        assert_eq!(count(1_234_567), "1,234,567");
        assert_eq!(mm(0.2, 2), "0.2");
        assert_eq!(mm(20.0, 2), "20");
        assert_eq!(mm(12.345, 2), "12.35");
        assert_eq!(mm(f32::NAN, 2), UNKNOWN);
    }

    #[test]
    fn relative_differences() {
        assert_eq!(relative_difference(104.0, 100.0).as_deref(), Some("+4%"));
        assert_eq!(
            relative_difference(97.0, 100.0).as_deref(),
            Some("\u{2212}3%")
        );
        assert_eq!(relative_difference(100.2, 100.0).as_deref(), Some("±0%"));
        assert_eq!(relative_difference(1.0, 0.0), None);
        assert_eq!(relative_difference(f64::NAN, 1.0), None);
    }

    fn job(status: JobStatus, progress: f32, layer: Option<(usize, usize)>) -> JobInfo {
        JobInfo {
            status,
            progress,
            layer,
            ..JobInfo::new(JobKind::HostPrint, "cube.gcode", 100, 1000)
        }
    }

    #[test]
    fn window_titles() {
        assert_eq!(window_title(None, None), "Sprint");
        assert_eq!(window_title(Some("a.gcode"), None), "Sprint — a.gcode");
        let printing = job(JobStatus::Printing, 0.429, Some((11, 100)));
        assert_eq!(
            window_title(Some("a.gcode"), Some(&printing)),
            "Sprint — cube.gcode — 42% · layer 12/100"
        );
        let paused = job(JobStatus::Paused, 0.5, None);
        assert_eq!(
            window_title(None, Some(&paused)),
            "Sprint — cube.gcode — Paused 50%"
        );
        // Finished jobs no longer show progress.
        let done = job(JobStatus::Completed, 1.0, Some((99, 100)));
        assert_eq!(
            window_title(Some("a.gcode"), Some(&done)),
            "Sprint — a.gcode"
        );
        // Garbage progress and names are tamed.
        let weird = JobInfo {
            name: "bad\nname".into(),
            ..job(JobStatus::Printing, f32::NAN, Some((5, 0)))
        };
        assert_eq!(window_title(None, Some(&weird)), "Sprint — bad name — 0%");
        assert_eq!(window_title(Some("   "), None), "Sprint");
    }

    #[test]
    fn sanitizing() {
        assert_eq!(sanitize("a\tb", 10), "a b");
        assert_eq!(sanitize("abcdef", 3), "abc…");
        assert_eq!(progress_percent(0.999), 99);
        assert_eq!(progress_percent(1.0), 100);
        assert_eq!(progress_percent(7.0), 100);
        assert_eq!(progress_percent(-1.0), 0);
    }

    /// A file "loaded" from `path` (the text does not matter here).
    fn file_at(path: &str) -> GcodeFile {
        let dir = std::env::temp_dir().join(format!(
            "sprint-gui-format-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let full = dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("dir");
        }
        std::fs::write(&full, "G28\n").expect("write");
        let file = GcodeFile::load(&full).expect("load");
        let _ = std::fs::remove_dir_all(&dir);
        file
    }

    #[test]
    fn job_file_matching() {
        // Jobs are matched by path: same name in another folder is another
        // file.
        let cube = file_at("a/cube.gcode");
        let other_cube = file_at("b/cube.gcode");
        let host = JobInfo {
            path: cube.path().map(|p| p.to_path_buf()),
            ..job(JobStatus::Printing, 0.0, None)
        };
        assert!(job_matches_file(&host, &cube));
        assert!(!job_matches_file(&host, &other_cube));
        assert!(job_prints_file(&host, &cube));
        assert!(!job_prints_file(&host, &other_cube));
        // Files that never came from disk match by name.
        let text = GcodeFile::from_text("cube.gcode", "G28\n");
        assert!(!job_matches_file(&host, &text));
        let unsaved = JobInfo {
            path: None,
            ..host.clone()
        };
        assert!(job_matches_file(&unsaved, &text));
        assert!(!job_matches_file(&unsaved, &cube));
        assert!(!job_matches_file(
            &unsaved,
            &GcodeFile::from_text("other.gcode", "G28\n")
        ));
        let upload = JobInfo {
            kind: JobKind::SdUpload,
            ..host.clone()
        };
        assert!(job_matches_file(&upload, &cube));
        assert!(!job_prints_file(&upload, &cube));
        let sd = JobInfo {
            kind: JobKind::SdPrint,
            ..host
        };
        assert!(!job_matches_file(&sd, &cube));
        assert!(!job_prints_file(&sd, &cube));
    }
}
