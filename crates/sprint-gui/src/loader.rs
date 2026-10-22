//! Background file loading: reading, analysis and thumbnail decoding on a
//! worker thread, with progress, cancellation and change detection.
//!
//! [`FileLoader`] runs at most one job. Starting a new one cancels the
//! previous job, whose late messages are ignored (every message carries
//! its job id). The UI polls [`FileLoader::poll`] once per frame and gets
//! [`LoadEvent`]s:
//!
//! ```text
//! start ─► Opened (text in memory, analysis starts) ─► Analyzed
//!      └─► Failed                                  └─► Failed / Cancelled
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime};

use crossbeam_channel::{Receiver, Sender};
use sprint_gcode::{
    AnalyzeOptions, ContentKind, GcodeFile, KinematicLimits, SlicerMetadata, ThumbnailFormat,
    Toolpath,
};
use sprint_host::{PrinterProfile, PrinterState};
use sprint_proto::MotionLimits;

/// Extensions that are always accepted as G-code (other files are
/// accepted when they look like text).
pub const GCODE_EXTENSIONS: &[&str] = &["gcode", "gco", "g", "gc", "nc", "txt"];

/// Files larger than this are refused (a G-code file this big would not
/// fit the analysis in memory anyway).
pub const MAX_FILE_BYTES: u64 = 2 << 30;

/// Largest thumbnail side that is decoded (pixels).
const MAX_THUMBNAIL_SIDE: u32 = 2048;

/// What the loader is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Reading the file from disk.
    Reading,
    /// Analyzing the toolpath.
    Analyzing,
}

/// Progress of the running job.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadStatus {
    /// File name being loaded.
    pub name: String,
    /// Current stage.
    pub stage: Stage,
    /// Progress of the stage in `0..=1` (reading has none).
    pub progress: f32,
}

/// A decoded slicer thumbnail.
#[derive(Clone)]
pub struct Thumbnail {
    /// The image, ready for a texture.
    pub image: egui::ColorImage,
}

impl std::fmt::Debug for Thumbnail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Thumbnail({}x{})",
            self.image.size[0], self.image.size[1]
        )
    }
}

/// What happened since the last [`FileLoader::poll`].
#[derive(Debug)]
pub enum LoadEvent {
    /// The text is loaded; the analysis is starting.
    Opened {
        /// The file.
        file: Arc<GcodeFile>,
        /// Where it came from (None for in-memory text).
        path: Option<PathBuf>,
        /// The same path was loaded before (a reload).
        reload: bool,
        /// The file on disk just before it was read: the baseline a
        /// [`FileWatch`] measures changes against (None for text in memory,
        /// or when the disk does not tell).
        stamp: Option<Stamp>,
    },
    /// Analysis progress.
    Progress(f32),
    /// The analysis finished.
    Analyzed {
        /// The result.
        toolpath: Arc<Toolpath>,
        /// The largest decodable slicer thumbnail.
        thumbnail: Option<Thumbnail>,
        /// Seconds the analysis took.
        seconds: f32,
    },
    /// Reading or analysis failed.
    Failed {
        /// File name.
        name: String,
        /// Human-readable reason.
        error: String,
    },
    /// The job was cancelled.
    Cancelled {
        /// File name.
        name: String,
    },
}

/// Messages from a worker, tagged with the job id.
enum Msg {
    Opened(Arc<GcodeFile>, Option<Stamp>),
    Progress(f32),
    Analyzed(Arc<Toolpath>, Option<Thumbnail>, f32),
    Failed(String),
    Cancelled,
}

struct Job {
    id: u64,
    name: String,
    path: Option<PathBuf>,
    reload: bool,
    stage: Stage,
    progress: f32,
    cancel: Arc<AtomicBool>,
}

/// Runs file loads and analyses on worker threads (see the module docs).
pub struct FileLoader {
    tx: Sender<(u64, Msg)>,
    rx: Receiver<(u64, Msg)>,
    job: Option<Job>,
    /// Events produced outside the workers (a cancellation).
    pending: Vec<LoadEvent>,
    next_id: u64,
    repaint: Option<egui::Context>,
}

impl Default for FileLoader {
    fn default() -> Self {
        FileLoader::new(None)
    }
}

impl FileLoader {
    /// A loader that asks `repaint` for a frame whenever a worker reports.
    pub fn new(repaint: Option<egui::Context>) -> FileLoader {
        let (tx, rx) = crossbeam_channel::unbounded();
        FileLoader {
            tx,
            rx,
            job: None,
            pending: Vec::new(),
            next_id: 1,
            repaint,
        }
    }

    /// Load and analyze a file from disk (cancels a running job).
    /// `reload` marks a reload of the file already shown.
    pub fn load(&mut self, path: PathBuf, options: AnalyzeOptions, reload: bool) {
        let name = display_name(&path);
        let (id, cancel) = self.begin(name, Some(path.clone()), reload, Stage::Reading);
        let tx = self.tx.clone();
        let repaint = self.repaint.clone();
        self.spawn(id, move || {
            let send = |msg| {
                let _ = tx.send((id, msg));
                if let Some(ctx) = &repaint {
                    ctx.request_repaint();
                }
            };
            match read_gcode(&path) {
                Ok((file, stamp)) => {
                    if cancel.load(Ordering::Relaxed) {
                        send(Msg::Cancelled);
                        return;
                    }
                    let file = Arc::new(file);
                    send(Msg::Opened(Arc::clone(&file), stamp));
                    run_analysis(&file, &options, &cancel, &send);
                }
                Err(error) => send(Msg::Failed(error)),
            }
        });
    }

    /// Analyze a file that is already in memory (a re-analysis with other
    /// options, or text that did not come from disk). Emits `Opened` with
    /// `reload` set when `path` is the file shown.
    pub fn analyze(
        &mut self,
        file: Arc<GcodeFile>,
        path: Option<PathBuf>,
        options: AnalyzeOptions,
        reload: bool,
    ) {
        let name = file.name().to_owned();
        let (id, cancel) = self.begin(name, path, reload, Stage::Analyzing);
        let tx = self.tx.clone();
        let repaint = self.repaint.clone();
        self.spawn(id, move || {
            let send = |msg| {
                let _ = tx.send((id, msg));
                if let Some(ctx) = &repaint {
                    ctx.request_repaint();
                }
            };
            send(Msg::Opened(Arc::clone(&file), None));
            run_analysis(&file, &options, &cancel, &send);
        });
    }

    fn begin(
        &mut self,
        name: String,
        path: Option<PathBuf>,
        reload: bool,
        stage: Stage,
    ) -> (u64, Arc<AtomicBool>) {
        self.stop_current();
        let id = self.next_id;
        self.next_id += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        self.job = Some(Job {
            id,
            name,
            path,
            reload,
            stage,
            progress: 0.0,
            cancel: Arc::clone(&cancel),
        });
        (id, cancel)
    }

    fn spawn(&mut self, id: u64, work: impl FnOnce() + Send + 'static) {
        let spawned = std::thread::Builder::new()
            .name("sprint-gui-loader".to_owned())
            .spawn(work);
        if let Err(e) = spawned {
            let _ = self.tx.send((
                id,
                Msg::Failed(format!("could not start a worker thread: {e}")),
            ));
        }
    }

    /// Stop the running job without reporting it (it is superseded).
    fn stop_current(&mut self) -> Option<Job> {
        let job = self.job.take()?;
        job.cancel.store(true, Ordering::Relaxed);
        Some(job)
    }

    /// Cancel the running job: its worker stops at the next progress
    /// report, and the next [`FileLoader::poll`] reports `Cancelled`.
    pub fn cancel(&mut self) {
        if let Some(job) = self.stop_current() {
            self.pending.push(LoadEvent::Cancelled { name: job.name });
        }
    }

    /// Drop the running job without a `Cancelled` report (its result is no
    /// longer wanted); late messages of its worker are ignored.
    pub fn abandon(&mut self) {
        self.stop_current();
    }

    /// True while a job runs.
    pub fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    /// The running job.
    pub fn status(&self) -> Option<LoadStatus> {
        self.job.as_ref().map(|j| LoadStatus {
            name: j.name.clone(),
            stage: j.stage,
            progress: j.progress,
        })
    }

    /// Collect what the workers reported since the last call.
    pub fn poll(&mut self) -> Vec<LoadEvent> {
        let mut events = std::mem::take(&mut self.pending);
        while let Ok((id, msg)) = self.rx.try_recv() {
            let Some(job) = self.job.as_mut() else {
                continue;
            };
            if job.id != id {
                continue;
            }
            match msg {
                Msg::Opened(file, stamp) => {
                    job.stage = Stage::Analyzing;
                    job.progress = 0.0;
                    events.push(LoadEvent::Opened {
                        file,
                        path: job.path.clone(),
                        reload: job.reload,
                        stamp,
                    });
                }
                Msg::Progress(p) => {
                    job.progress = if p.is_finite() {
                        p.clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    events.push(LoadEvent::Progress(job.progress));
                }
                Msg::Analyzed(toolpath, thumbnail, seconds) => {
                    events.push(LoadEvent::Analyzed {
                        toolpath,
                        thumbnail,
                        seconds,
                    });
                    self.job = None;
                }
                Msg::Failed(error) => {
                    events.push(LoadEvent::Failed {
                        name: job.name.clone(),
                        error,
                    });
                    self.job = None;
                }
                Msg::Cancelled => {
                    events.push(LoadEvent::Cancelled {
                        name: job.name.clone(),
                    });
                    self.job = None;
                }
            }
        }
        events
    }
}

/// Run the analysis and report it through `send`.
fn run_analysis(
    file: &GcodeFile,
    options: &AnalyzeOptions,
    cancel: &AtomicBool,
    send: &dyn Fn(Msg),
) {
    let started = Instant::now();
    let mut last = -1.0f32;
    let result = sprint_gcode::analyze(file, options, &mut |p| {
        if p - last >= 0.01 || p >= 1.0 {
            last = p;
            send(Msg::Progress(p));
        }
        !cancel.load(Ordering::Relaxed)
    });
    match result {
        Some(toolpath) => {
            let thumbnail = decode_thumbnail(&toolpath.metadata);
            let seconds = started.elapsed().as_secs_f32();
            send(Msg::Analyzed(Arc::new(toolpath), thumbnail, seconds));
        }
        None => send(Msg::Cancelled),
    }
}

/// `path` made absolute against the working directory, with `.` and `..`
/// resolved (lexically, as Windows does): a file opened as `../x.gcode`
/// is remembered so that it opens from anywhere, and once only.
pub fn absolute_path(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                // Above the root stays at the root.
                if !matches!(
                    out.components().next_back(),
                    None | Some(std::path::Component::RootDir | std::path::Component::Prefix(_))
                ) {
                    out.pop();
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Whether two paths name the same file as far as their spelling tells:
/// equal, or equal ignoring case on Windows (its file names are).
pub fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.as_os_str().to_string_lossy().to_lowercase()
            == b.as_os_str().to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// The name shown for a path (its file name).
pub fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Whether a path has one of the [`GCODE_EXTENSIONS`].
pub fn has_gcode_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| GCODE_EXTENSIONS.iter().any(|g| e.eq_ignore_ascii_case(g)))
}

/// Read a file for display and printing, refusing directories, huge files,
/// 3D models and binary data with a readable message. Also its state on
/// disk before it was read: a change after that shows as one.
fn read_gcode(path: &Path) -> Result<(GcodeFile, Option<Stamp>), String> {
    let meta = std::fs::metadata(path).map_err(|e| io_message(path, &e))?;
    let stamp = meta.modified().ok().map(|modified| (modified, meta.len()));
    if meta.is_dir() {
        return Err(format!("{} is a folder, not a G-code file", path.display()));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{} is too large ({}); Sprint opens files up to {}",
            display_name(path),
            crate::format::bytes(meta.len()),
            crate::format::bytes(MAX_FILE_BYTES)
        ));
    }
    // A model or binary data is refused before it is read (whatever its
    // extension): it says what it is and what to do instead.
    let kind = sprint_gcode::sniff_file(path).map_err(|e| io_message(path, &e))?;
    if refuses(kind) {
        return Err(format!(
            "{} is {}. {}",
            display_name(path),
            kind.description(),
            kind.problem().unwrap_or_default()
        ));
    }
    let file = GcodeFile::load(path).map_err(|e| io_message(path, &e))?;
    Ok((file, stamp))
}

/// Content that is not even opened for viewing: 3D models, binary G-code
/// and other binary data. Text that is not G-code (and empty files) opens
/// with a warning, but cannot be printed (see
/// [`crate::panels::job_bar::content_blocker`]).
pub fn refuses(kind: ContentKind) -> bool {
    kind.is_model() || matches!(kind, ContentKind::BinaryGcode | ContentKind::Binary)
}

fn io_message(path: &Path, e: &std::io::Error) -> String {
    let what = match e.kind() {
        std::io::ErrorKind::NotFound => "file not found".to_owned(),
        std::io::ErrorKind::PermissionDenied => "permission denied".to_owned(),
        _ => e.to_string(),
    };
    format!("Could not open {}: {what}", display_name(path))
}

/// Decode the largest thumbnail that decodes (PNG, JPEG or QOI).
pub fn decode_thumbnail(meta: &SlicerMetadata) -> Option<Thumbnail> {
    let mut thumbs: Vec<_> = meta.thumbnails.iter().collect();
    thumbs.sort_by_key(|t| std::cmp::Reverse(u64::from(t.width) * u64::from(t.height)));
    thumbs.into_iter().find_map(|t| {
        let (w, h, rgba) = match t.format {
            ThumbnailFormat::Png => decode_with_image(&t.data, image::ImageFormat::Png)?,
            ThumbnailFormat::Jpeg => decode_with_image(&t.data, image::ImageFormat::Jpeg)?,
            ThumbnailFormat::Qoi => decode_qoi(&t.data)?,
        };
        if w == 0 || h == 0 || w > MAX_THUMBNAIL_SIDE || h > MAX_THUMBNAIL_SIDE {
            return None;
        }
        Some(Thumbnail {
            image: egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba),
        })
    })
}

fn decode_with_image(data: &[u8], format: image::ImageFormat) -> Option<(u32, u32, Vec<u8>)> {
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(data), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_THUMBNAIL_SIDE);
    limits.max_image_height = Some(MAX_THUMBNAIL_SIDE);
    limits.max_alloc = Some(64 << 20);
    reader.limits(limits);
    let img = reader.decode().ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some((w, h, img.into_raw()))
}

/// Decode a QOI ("Quite OK Image") file into RGBA8 pixels. `None` for
/// malformed or oversized data.
pub fn decode_qoi(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if data.len() < 14 || &data[..4] != b"qoif" {
        return None;
    }
    let w = u32::from_be_bytes(data[4..8].try_into().ok()?);
    let h = u32::from_be_bytes(data[8..12].try_into().ok()?);
    if w == 0 || h == 0 || w > MAX_THUMBNAIL_SIDE || h > MAX_THUMBNAIL_SIDE {
        return None;
    }
    let pixels = (w as usize) * (h as usize);
    let mut out = Vec::with_capacity(pixels * 4);
    let mut index = [[0u8; 4]; 64];
    let mut px = [0u8, 0, 0, 255];
    let mut pos = 14;
    let mut run = 0u32;
    let hash = |p: [u8; 4]| {
        (usize::from(p[0]) * 3
            + usize::from(p[1]) * 5
            + usize::from(p[2]) * 7
            + usize::from(p[3]) * 11)
            % 64
    };
    while out.len() < pixels * 4 {
        if run > 0 {
            run -= 1;
        } else {
            let b = *data.get(pos)?;
            pos += 1;
            match b {
                0xFE => {
                    px[..3].copy_from_slice(data.get(pos..pos + 3)?);
                    pos += 3;
                }
                0xFF => {
                    px.copy_from_slice(data.get(pos..pos + 4)?);
                    pos += 4;
                }
                _ => match b >> 6 {
                    0 => px = index[usize::from(b & 0x3F)],
                    1 => {
                        px[0] = px[0].wrapping_add(((b >> 4) & 3).wrapping_sub(2));
                        px[1] = px[1].wrapping_add(((b >> 2) & 3).wrapping_sub(2));
                        px[2] = px[2].wrapping_add((b & 3).wrapping_sub(2));
                    }
                    2 => {
                        let b2 = *data.get(pos)?;
                        pos += 1;
                        let dg = (b & 0x3F).wrapping_sub(32);
                        px[0] = px[0].wrapping_add(dg.wrapping_sub(8).wrapping_add(b2 >> 4));
                        px[1] = px[1].wrapping_add(dg);
                        px[2] = px[2].wrapping_add(dg.wrapping_sub(8).wrapping_add(b2 & 0x0F));
                    }
                    _ => run = u32::from(b & 0x3F),
                },
            }
            index[hash(px)] = px;
        }
        out.extend_from_slice(&px);
    }
    Some((w, h, out))
}

/// The analysis options for `profile`: its filament diameter, and its
/// kinematics, else (when connected and the settings report is in) the
/// firmware's motion limits, else the defaults.
pub fn analyze_options(profile: &PrinterProfile, state: &PrinterState) -> AnalyzeOptions {
    let defaults = AnalyzeOptions::default();
    let diameter = profile.filament_diameter;
    AnalyzeOptions {
        filament_diameter: if diameter.is_finite() && (0.5..=5.0).contains(&diameter) {
            diameter
        } else {
            defaults.filament_diameter
        },
        limits: match profile.kinematics {
            Some(limits) => limits,
            None if state.is_connected() && state.settings_loaded => {
                limits_from_firmware(&state.settings.motion_limits())
            }
            None => KinematicLimits::default(),
        },
        ..defaults
    }
}

/// Firmware motion limits mapped onto the estimator's limits; values the
/// firmware did not report (or reported as garbage) keep their defaults.
/// A report with jerk but without junction deviation means classic jerk.
pub fn limits_from_firmware(m: &MotionLimits) -> KinematicLimits {
    fn positive(v: f32) -> bool {
        v.is_finite() && v > 0.0
    }
    fn axes(target: &mut [f32; 4], source: Option<[f32; 4]>) {
        if let Some(values) = source {
            for (t, v) in target.iter_mut().zip(values) {
                if positive(v) {
                    *t = v;
                }
            }
        }
    }
    let mut k = KinematicLimits::default();
    axes(&mut k.max_feedrate, m.max_feedrate);
    axes(&mut k.max_accel, m.max_accel);
    if let Some(v) = m.print_accel.filter(|v| positive(*v)) {
        k.print_accel = v;
    }
    if let Some(v) = m.retract_accel.filter(|v| positive(*v)) {
        k.retract_accel = v;
    }
    if let Some(v) = m.travel_accel.filter(|v| positive(*v)) {
        k.travel_accel = v;
    }
    if let Some(jerk) = m.jerk {
        for (t, v) in k.jerk.iter_mut().zip(jerk) {
            if v.is_finite() && v >= 0.0 {
                *t = v;
            }
        }
    }
    k.junction_deviation = match m.junction_deviation {
        Some(j) if positive(j) => Some(j),
        Some(_) => k.junction_deviation,
        None if m.jerk.is_some() => None,
        None => k.junction_deviation,
    };
    k
}

/// The state of a file on disk: modification time and size.
pub type Stamp = (SystemTime, u64);

/// What a [`FileWatch`] look at the disk is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Look {
    /// The state changes are measured against.
    Baseline,
    /// A check for changes.
    Check,
}

/// A look at the disk running on a worker thread.
#[derive(Debug)]
struct PendingLook {
    look: Look,
    rx: Receiver<Option<Stamp>>,
}

/// Watches the loaded file for changes on disk (modification time and
/// size, checked every [`FileWatch::INTERVAL`] seconds).
///
/// The disk is read on a worker thread: on a network share that went away
/// a metadata call can hang for many seconds, which must not freeze the
/// window.
#[derive(Debug, Default)]
pub struct FileWatch {
    path: Option<PathBuf>,
    /// `Some` once the baseline is known (the inner `None`: no file).
    stamp: Option<Option<Stamp>>,
    pending: Option<PendingLook>,
    last_check: Option<f64>,
    changed: bool,
    repaint: Option<egui::Context>,
}

impl FileWatch {
    /// Seconds between checks.
    pub const INTERVAL: f64 = 2.0;

    /// A watch that asks `repaint` for a frame when a look at the disk is
    /// done.
    pub fn new(repaint: Option<egui::Context>) -> FileWatch {
        FileWatch {
            repaint,
            ..FileWatch::default()
        }
    }

    /// Watch `path` (None stops watching) for changes from `baseline`
    /// (the file as it was read, [`LoadEvent::Opened`]), or without one
    /// from its state at the next [`FileWatch::poll`].
    pub fn watch(&mut self, path: Option<PathBuf>, baseline: Option<Stamp>) {
        self.stamp = baseline.filter(|_| path.is_some()).map(Some);
        self.path = path;
        self.pending = None;
        self.changed = false;
        self.last_check = None;
    }

    /// Collect a finished look at the disk and start the next one when the
    /// interval has passed (`now` in seconds, any monotonic clock). Returns
    /// true when the file has changed since the baseline; stays true until
    /// [`FileWatch::watch`] or [`FileWatch::dismiss`].
    pub fn poll(&mut self, now: f64) -> bool {
        if let Some(pending) = &self.pending {
            match pending.rx.try_recv() {
                Ok(current) => {
                    let look = pending.look;
                    self.pending = None;
                    self.take(look, current);
                }
                Err(crossbeam_channel::TryRecvError::Empty) => return self.changed,
                Err(crossbeam_channel::TryRecvError::Disconnected) => self.pending = None,
            }
        }
        let Some(path) = &self.path else {
            return false;
        };
        if self.changed {
            return true;
        }
        let due = self
            .last_check
            .is_none_or(|t| now - t >= Self::INTERVAL || now < t);
        if due {
            self.last_check = Some(now);
            let look = if self.stamp.is_some() {
                Look::Check
            } else {
                Look::Baseline
            };
            let (tx, rx) = crossbeam_channel::bounded(1);
            let path = path.clone();
            let repaint = self.repaint.clone();
            let spawned = std::thread::Builder::new()
                .name("sprint-gui-file-watch".to_owned())
                .spawn(move || {
                    let _ = tx.send(stamp(&path));
                    if let Some(ctx) = repaint {
                        ctx.request_repaint();
                    }
                });
            if spawned.is_ok() {
                self.pending = Some(PendingLook { look, rx });
            }
        }
        self.changed
    }

    /// Apply the result of a look at the disk.
    fn take(&mut self, look: Look, current: Option<Stamp>) {
        match look {
            Look::Baseline => self.stamp = Some(current),
            // A file that disappeared is not "changed" (nothing to
            // reload); it counts once it is back with other contents.
            Look::Check => {
                if let Some(baseline) = self.stamp
                    && current.is_some()
                    && current != baseline
                {
                    self.changed = true;
                }
            }
        }
    }

    /// Wait (at most `timeout`) for the look at the disk in progress and
    /// apply it (tests).
    pub fn finish_pending(&mut self, timeout: std::time::Duration) {
        if let Some(pending) = self.pending.take()
            && let Ok(current) = pending.rx.recv_timeout(timeout)
        {
            self.take(pending.look, current);
        }
    }

    /// Whether a change was detected.
    pub fn is_changed(&self) -> bool {
        self.changed
    }

    /// Accept the file as it is now as the new baseline (the user chose
    /// not to reload).
    pub fn dismiss(&mut self) {
        self.stamp = None;
        self.pending = None;
        self.changed = false;
        self.last_check = None;
    }
}

fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn paths_are_made_absolute_and_normalized() {
        let cwd = std::env::current_dir().expect("cwd");
        let parent = cwd.parent().expect("parent").to_path_buf();
        assert_eq!(
            absolute_path(Path::new("../x.gcode")),
            parent.join("x.gcode")
        );
        assert_eq!(
            absolute_path(Path::new("./a/./b/../x.gcode")),
            cwd.join("a").join("x.gcode")
        );
        let root = cwd.ancestors().last().expect("root").to_path_buf();
        let deep: PathBuf = std::iter::repeat_n("..", 64).collect();
        assert_eq!(absolute_path(&deep.join("x.gcode")), root.join("x.gcode"));
        let done = absolute_path(Path::new("x.gcode"));
        assert!(done.is_absolute());
        assert_eq!(absolute_path(&done), done);
        assert!(same_path(&done, &done));
        assert_eq!(
            same_path(Path::new("C:/A/x.GCODE"), Path::new("c:/a/X.gcode")),
            cfg!(windows)
        );
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sprint-gui-loader-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// Poll until `done` says so (bounded).
    fn drain(loader: &mut FileLoader, done: impl Fn(&[LoadEvent]) -> bool) -> Vec<LoadEvent> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut events = Vec::new();
        while Instant::now() < deadline {
            events.extend(loader.poll());
            if done(&events) {
                return events;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("loader did not finish: {events:?}");
    }

    fn finished(events: &[LoadEvent]) -> bool {
        events.iter().any(|e| {
            matches!(
                e,
                LoadEvent::Analyzed { .. } | LoadEvent::Failed { .. } | LoadEvent::Cancelled { .. }
            )
        })
    }

    #[test]
    fn loads_and_analyzes_a_sample() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples/calibration-cube.gcode");
        let mut loader = FileLoader::default();
        loader.load(path.clone(), AnalyzeOptions::default(), false);
        assert!(loader.is_busy());
        assert_eq!(loader.status().map(|s| s.stage), Some(Stage::Reading));
        let events = drain(&mut loader, finished);
        assert!(!loader.is_busy());
        let opened = events.iter().find_map(|e| match e {
            LoadEvent::Opened {
                file, path, reload, ..
            } => Some((file, path, reload)),
            _ => None,
        });
        let (file, opened_path, reload) = opened.expect("opened");
        assert!(file.len() > 1000);
        assert_eq!(opened_path.as_deref(), Some(path.as_path()));
        assert!(!reload);
        let analyzed = events.iter().find_map(|e| match e {
            LoadEvent::Analyzed {
                toolpath,
                thumbnail,
                ..
            } => Some((toolpath, thumbnail)),
            _ => None,
        });
        let (toolpath, thumbnail) = analyzed.expect("analyzed");
        assert!(!toolpath.layers.is_empty());
        // The sample carries a 16x16 and a 220x124 PNG: the largest wins.
        let thumb = thumbnail.as_ref().expect("thumbnail");
        assert_eq!(thumb.image.size, [220, 124]);
        assert!(events.iter().any(|e| matches!(e, LoadEvent::Progress(_))));
    }

    #[test]
    fn missing_files_and_folders_fail_with_a_message() {
        let mut loader = FileLoader::default();
        loader.load(
            PathBuf::from("Z:/definitely/not/here.gcode"),
            AnalyzeOptions::default(),
            false,
        );
        let events = drain(&mut loader, finished);
        let error = events.iter().find_map(|e| match e {
            LoadEvent::Failed { error, .. } => Some(error.clone()),
            _ => None,
        });
        assert!(error.expect("failed").contains("here.gcode"));

        let dir = temp_dir("folder");
        loader.load(dir.clone(), AnalyzeOptions::default(), false);
        let events = drain(&mut loader, finished);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LoadEvent::Failed { error, .. } if error.contains("folder")))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn models_and_binary_data_are_refused_whatever_their_name() {
        let dir = temp_dir("binary");
        let refused = |loader: &mut FileLoader, path: PathBuf| -> String {
            loader.load(path, AnalyzeOptions::default(), false);
            let events = drain(loader, finished);
            events
                .iter()
                .find_map(|e| match e {
                    LoadEvent::Failed { error, .. } => Some(error.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("not refused: {events:?}"))
        };
        let mut loader = FileLoader::default();
        let bin = dir.join("picture.bin");
        std::fs::write(&bin, [0u8, 1, 2, 3, 0, 255, 0, 7]).expect("write");
        let error = refused(&mut loader, bin);
        assert!(error.contains("binary file"), "{error}");
        // A model saved with a G-code name.
        let stl = dir.join("cube.gcode");
        std::fs::write(
            &stl,
            "solid cube\n facet normal 0 0 1\n  outer loop\n   vertex 0 0 0\n   vertex 1 0 0\n   vertex 0 1 0\n  endloop\n endfacet\nendsolid cube\n",
        )
        .expect("write");
        let error = refused(&mut loader, stl);
        assert!(
            error.starts_with("cube.gcode is an ASCII STL model.")
                && error.ends_with("slice it first."),
            "{error}"
        );
        let project = dir.join("plate.3mf");
        let mut zip = b"PK\x03\x04".to_vec();
        zip.extend(std::iter::repeat_n(7u8, 200));
        std::fs::write(&project, zip).expect("write");
        let error = refused(&mut loader, project);
        assert!(error.contains("3MF"), "{error}");
        // G-code under any name, and text that is not G-code, open.
        let text = dir.join("notes.cfg");
        std::fs::write(&text, "G28\nG1 X10 Y10 F3000\n").expect("write");
        loader.load(text, AnalyzeOptions::default(), true);
        let events = drain(&mut loader, finished);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LoadEvent::Opened { reload: true, .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LoadEvent::Analyzed { .. }))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_job_supersedes_and_cancel_reports_once() {
        let big: String = (0..200_000)
            .map(|i| format!("G1 X{} Y{} E{}\n", i % 200, (i / 200) % 200, i))
            .collect();
        let file = Arc::new(GcodeFile::from_text("big.gcode", big));
        let small = Arc::new(GcodeFile::from_text("small.gcode", "G1 X1 E1\nG1 X2 E2\n"));
        let mut loader = FileLoader::default();
        loader.analyze(Arc::clone(&file), None, AnalyzeOptions::default(), false);
        loader.cancel();
        assert!(!loader.is_busy());
        assert_eq!(loader.status(), None);
        let events = drain(&mut loader, |e| !e.is_empty());
        let cancels = events
            .iter()
            .filter(|e| matches!(e, LoadEvent::Cancelled { .. }))
            .count();
        assert_eq!(cancels, 1, "{events:?}");
        // Late messages of the cancelled worker are ignored.
        std::thread::sleep(Duration::from_millis(50));
        assert!(loader.poll().is_empty());

        loader.analyze(Arc::clone(&file), None, AnalyzeOptions::default(), false);
        loader.analyze(small, None, AnalyzeOptions::default(), false);
        let events = drain(&mut loader, |e| {
            e.iter().any(|e| matches!(e, LoadEvent::Analyzed { .. }))
        });
        let names: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                LoadEvent::Opened { file, .. } => Some(file.name().to_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["small.gcode"], "{events:?}");
    }

    #[test]
    fn what_is_refused() {
        for kind in [
            ContentKind::AsciiStl,
            ContentKind::BinaryStl,
            ContentKind::Zip3mf,
            ContentKind::Obj,
            ContentKind::Step,
            ContentKind::BinaryGcode,
            ContentKind::Binary,
        ] {
            assert!(refuses(kind), "{kind:?}");
        }
        for kind in [
            ContentKind::Gcode,
            ContentKind::Empty,
            ContentKind::OtherText,
        ] {
            assert!(!refuses(kind), "{kind:?}");
        }
    }

    #[test]
    fn names() {
        assert!(has_gcode_extension(Path::new("a/b.GCODE")));
        assert!(has_gcode_extension(Path::new("b.gco")));
        assert!(!has_gcode_extension(Path::new("b.stl")));
        assert!(!has_gcode_extension(Path::new("noext")));
        assert_eq!(display_name(Path::new("a/b/c.gcode")), "c.gcode");
    }

    /// A 2x2 QOI image: RGB op, a run, then an index op.
    #[test]
    fn decodes_qoi() {
        let mut data = b"qoif".to_vec();
        data.extend_from_slice(&2u32.to_be_bytes());
        data.extend_from_slice(&2u32.to_be_bytes());
        data.extend_from_slice(&[4, 0]);
        data.extend_from_slice(&[0xFE, 10, 20, 30]); // pixel 0
        data.push(0xC0 | 1); // run of 2: pixels 1-2
        data.extend_from_slice(&[0xFF, 1, 2, 3, 4]); // pixel 3
        data.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        let (w, h, px) = decode_qoi(&data).expect("decodes");
        assert_eq!((w, h), (2, 2));
        assert_eq!(&px[..4], &[10, 20, 30, 255]);
        assert_eq!(&px[4..8], &[10, 20, 30, 255]);
        assert_eq!(&px[8..12], &[10, 20, 30, 255]);
        assert_eq!(&px[12..], &[1, 2, 3, 4]);
        // Truncated or bogus data never panics.
        for cut in 0..data.len() {
            let _ = decode_qoi(&data[..cut]);
        }
        assert!(decode_qoi(b"qoifxxxxxxxxxxxxxxxx").is_none());
    }

    #[test]
    fn firmware_limits_map_onto_the_estimator() {
        let m = MotionLimits {
            max_feedrate: Some([300.0, 300.0, 5.0, f32::NAN]),
            max_accel: Some([1000.0, 1000.0, 100.0, 5000.0]),
            print_accel: Some(800.0),
            retract_accel: Some(-1.0),
            travel_accel: None,
            junction_deviation: None,
            jerk: Some([10.0, 10.0, 0.3, 5.0]),
        };
        let k = limits_from_firmware(&m);
        let d = KinematicLimits::default();
        assert_eq!(k.max_feedrate, [300.0, 300.0, 5.0, d.max_feedrate[3]]);
        assert_eq!(k.max_accel, [1000.0, 1000.0, 100.0, 5000.0]);
        assert_eq!(k.print_accel, 800.0);
        assert_eq!(k.retract_accel, d.retract_accel);
        assert_eq!(k.travel_accel, d.travel_accel);
        assert_eq!(k.junction_deviation, None, "jerk without J = classic jerk");
        assert_eq!(k.jerk, [10.0, 10.0, 0.3, 5.0]);
        let jd = limits_from_firmware(&MotionLimits {
            junction_deviation: Some(0.02),
            ..MotionLimits::default()
        });
        assert_eq!(jd.junction_deviation, Some(0.02));
        assert_eq!(limits_from_firmware(&MotionLimits::default()), d);
    }

    #[test]
    fn analyze_options_follow_profile_then_firmware() {
        let mut profile = PrinterProfile {
            filament_diameter: 2.85,
            ..PrinterProfile::default()
        };
        let mut state = PrinterState::default();
        let o = analyze_options(&profile, &state);
        assert_eq!(o.filament_diameter, 2.85);
        assert_eq!(o.limits, KinematicLimits::default());

        state.status = sprint_host::ConnectionStatus::Connected;
        state.settings_loaded = true;
        state
            .settings
            .insert(sprint_proto::SettingCommand::parse("M203 X100 Y100 Z4 E50").expect("parse"));
        let o = analyze_options(&profile, &state);
        assert_eq!(o.limits.max_feedrate, [100.0, 100.0, 4.0, 50.0]);

        let custom = KinematicLimits {
            print_accel: 4242.0,
            ..KinematicLimits::default()
        };
        profile.kinematics = Some(custom);
        profile.filament_diameter = f32::NAN;
        let o = analyze_options(&profile, &state);
        assert_eq!(o.limits, custom);
        assert_eq!(o.filament_diameter, 1.75);
    }

    #[test]
    fn file_watch_detects_changes() {
        let dir = temp_dir("watch");
        let path = dir.join("watched.gcode");
        std::fs::write(
            &path, "G28
",
        )
        .expect("write");
        let wait = Duration::from_secs(10);
        let mut watch = FileWatch::default();
        assert!(!watch.poll(0.0));
        watch.watch(Some(path.clone()), None);
        // Without a baseline the first look takes it (on a worker thread).
        assert!(!watch.poll(0.0));
        watch.finish_pending(wait);
        std::fs::write(
            &path,
            "G28
G1 X10
",
        )
        .expect("rewrite");
        // Not due yet.
        assert!(!watch.poll(1.0));
        assert!(!watch.poll(2.5), "the check runs in the background");
        watch.finish_pending(wait);
        assert!(watch.poll(2.6));
        assert!(watch.is_changed());
        // Sticky until dismissed.
        assert!(watch.poll(2.7));
        watch.dismiss();
        assert!(!watch.poll(10.0));
        watch.finish_pending(wait);
        assert!(!watch.poll(12.5));
        watch.finish_pending(wait);
        assert!(!watch.poll(12.6), "the dismissed state is the new baseline");
        // A deleted file is not a change.
        std::fs::remove_file(&path).expect("remove");
        assert!(!watch.poll(20.0));
        watch.finish_pending(wait);
        assert!(!watch.poll(20.1));
        // Results for a file no longer watched are dropped.
        std::fs::write(
            &path,
            "G28
G1 X20
",
        )
        .expect("rewrite");
        assert!(!watch.poll(30.0));
        watch.watch(None, None);
        watch.finish_pending(wait);
        assert!(!watch.poll(40.0));
        assert!(!watch.is_changed());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_save_before_the_first_look_counts_against_the_file_as_read() {
        let dir = temp_dir("watch-read");
        let path = dir.join("read.gcode");
        std::fs::write(&path, "G28
").expect("write");
        let (_, read) = read_gcode(&path).expect("read");
        // Saved again before the watch ever looked at the disk.
        std::fs::write(&path, "G28
G1 X10
").expect("rewrite");
        let mut watch = FileWatch::default();
        watch.watch(Some(path.clone()), read);
        assert!(!watch.poll(0.0), "the check runs in the background");
        watch.finish_pending(Duration::from_secs(10));
        assert!(watch.poll(0.1));
        let _ = std::fs::remove_dir_all(dir);
    }
}
