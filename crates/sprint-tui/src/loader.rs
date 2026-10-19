//! Loading and analysing G-code files, in the background (TUI) or with a
//! progress callback (shell, CLI).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, TryRecvError};
use sprint_host::PrinterProfile;
use sprint_host::sprint_gcode::{AnalyzeOptions, Bounds, GcodeFile, Toolpath, analyze};

use crate::widgets::object_bounds;

/// A loaded and analysed G-code file.
#[derive(Clone)]
pub struct LoadedFile {
    /// Where it was loaded from.
    pub path: PathBuf,
    /// The file.
    pub file: Arc<GcodeFile>,
    /// Its analysis.
    pub toolpath: Arc<Toolpath>,
    /// XY extents of the printed object (for the layer view).
    pub object_bounds: Bounds,
    /// How long loading and analysis took.
    pub load_time: Duration,
}

impl std::fmt::Debug for LoadedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedFile")
            .field("path", &self.path)
            .field("lines", &self.file.len())
            .field("layers", &self.toolpath.layers.len())
            .finish_non_exhaustive()
    }
}

impl LoadedFile {
    /// Build from an already analysed file.
    pub fn new(path: PathBuf, file: GcodeFile, toolpath: Toolpath, load_time: Duration) -> Self {
        let bounds = object_bounds(&toolpath);
        LoadedFile {
            path,
            file: Arc::new(file),
            toolpath: Arc::new(toolpath),
            object_bounds: bounds,
            load_time,
        }
    }

    /// The file name for display.
    pub fn name(&self) -> &str {
        self.file.name()
    }
}

/// Analysis options for a printer profile (its filament diameter and
/// motion limits).
pub fn analyze_options(profile: &PrinterProfile) -> AnalyzeOptions {
    let mut options = AnalyzeOptions::default();
    if profile.filament_diameter.is_finite() && profile.filament_diameter > 0.0 {
        options.filament_diameter = profile.filament_diameter;
    }
    if let Some(limits) = profile.kinematics {
        options.limits = limits;
    }
    options
}

/// Read a G-code file with a friendly error message.
pub fn read_file(path: &Path) -> Result<GcodeFile, String> {
    let shown = path.display();
    if path.is_dir() {
        return Err(format!("{shown} is a directory, not a G-code file"));
    }
    GcodeFile::load(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("{shown}: no such file"),
        std::io::ErrorKind::PermissionDenied => format!("{shown}: permission denied"),
        _ => format!("cannot read {shown}: {e}"),
    })
}

/// Load and analyse `path`, reporting progress (0..=1) to `progress`,
/// which returns false to cancel.
pub fn load_with_progress(
    path: &Path,
    options: &AnalyzeOptions,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<LoadedFile, String> {
    let started = Instant::now();
    let file = read_file(path)?;
    let toolpath = analyze(&file, options, progress).ok_or_else(|| "cancelled".to_string())?;
    Ok(LoadedFile::new(
        path.to_path_buf(),
        file,
        toolpath,
        started.elapsed(),
    ))
}

/// A load running on a background thread.
pub struct LoadJob {
    path: PathBuf,
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    result: Receiver<Result<LoadedFile, String>>,
    started: Instant,
}

impl std::fmt::Debug for LoadJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadJob")
            .field("path", &self.path)
            .field("progress", &self.progress())
            .finish_non_exhaustive()
    }
}

impl LoadJob {
    /// Start loading `path` on a new thread.
    pub fn spawn(path: PathBuf, options: AnalyzeOptions) -> LoadJob {
        let progress = Arc::new(AtomicU32::new(0f32.to_bits()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = crossbeam_channel::bounded(1);
        let job = LoadJob {
            path: path.clone(),
            progress: Arc::clone(&progress),
            cancel: Arc::clone(&cancel),
            result: rx,
            started: Instant::now(),
        };
        let failed = tx.clone();
        let spawned = thread::Builder::new()
            .name("sprint-load".into())
            .spawn(move || {
                let result = load_with_progress(&path, &options, &mut |p| {
                    progress.store(p.to_bits(), Ordering::Relaxed);
                    !cancel.load(Ordering::Relaxed)
                });
                let _ = tx.send(result);
            });
        if let Err(e) = spawned {
            // The next poll reports the failure.
            let _ = failed.send(Err(format!("cannot start loading: {e}")));
        }
        job
    }

    /// The file being loaded.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file name being loaded, for display.
    pub fn name(&self) -> String {
        self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    /// Analysis progress, 0..=1.
    pub fn progress(&self) -> f32 {
        let p = f32::from_bits(self.progress.load(Ordering::Relaxed));
        if p.is_finite() {
            p.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Time since the load started.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// The result once the load has finished (`None` while running).
    pub fn poll(&self) -> Option<Result<LoadedFile, String>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("the loader stopped".to_string())),
        }
    }

    /// Ask the analysis to stop; [`poll`](LoadJob::poll) then reports
    /// "cancelled".
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for LoadJob {
    fn drop(&mut self) {
        // A replaced or abandoned load should not keep a core busy.
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(job: &LoadJob) -> Result<LoadedFile, String> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(result) = job.poll() {
                return result;
            }
            assert!(Instant::now() < deadline, "load timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn sample(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../samples")
            .join(name)
    }

    #[test]
    fn loads_the_sample_in_the_background() {
        let job = LoadJob::spawn(sample("calibration-cube.gcode"), AnalyzeOptions::default());
        assert_eq!(job.name(), "calibration-cube.gcode");
        let loaded = wait(&job).expect("loads");
        assert_eq!(loaded.name(), "calibration-cube.gcode");
        assert!(loaded.toolpath.layers.len() > 10);
        assert!(!loaded.object_bounds.is_empty());
        assert!(format!("{loaded:?}").contains("layers"));
        assert!((0.0..=1.0).contains(&job.progress()));
    }

    #[test]
    fn errors_are_friendly() {
        let job = LoadJob::spawn(
            PathBuf::from("no-such-file-91c2.gcode"),
            AnalyzeOptions::default(),
        );
        assert_eq!(
            wait(&job).map(|_| ()),
            Err("no-such-file-91c2.gcode: no such file".to_string())
        );
        let dir = std::env::temp_dir();
        let err = read_file(&dir).map(|_| ()).expect_err("directory");
        assert!(err.ends_with("is a directory, not a G-code file"), "{err}");
    }

    #[test]
    fn a_cancelled_load_reports_it() {
        let mut calls = 0;
        let result = load_with_progress(
            &sample("calibration-cube.gcode"),
            &AnalyzeOptions::default(),
            &mut |_| {
                calls += 1;
                false
            },
        );
        assert_eq!(result.map(|_| ()), Err("cancelled".to_string()));
        assert!(calls >= 1);
    }

    #[test]
    fn profile_options_apply() {
        let mut profile = PrinterProfile {
            filament_diameter: 2.85,
            ..PrinterProfile::default()
        };
        assert_eq!(analyze_options(&profile).filament_diameter, 2.85);
        profile.filament_diameter = f32::NAN;
        assert_eq!(
            analyze_options(&profile).filament_diameter,
            AnalyzeOptions::default().filament_diameter
        );
    }
}
