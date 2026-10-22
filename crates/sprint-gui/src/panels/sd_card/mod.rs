//! SD card tab: the card's files in a sortable, filterable table, printing
//! and deleting them (after a confirmation), uploading the loaded G-code
//! file under a validated 8.3 name, the progress of SD prints and uploads,
//! and initializing / releasing the card (M21 / M22).

mod table;

pub use table::{Sort, SortKey, display_name, natural_cmp, sorted_indices};

use egui::{Align, Layout, RichText, Ui};
use sprint_host::{JobInfo, JobKind, JobStatus, PrinterState};

use crate::context::{AppAction, AppCtx};
use crate::insight::{format, sdname, ui as kit};
use crate::theme::{self, Tone};
use crate::widgets::{
    ActionButton, Card, ConfirmDialog, DialogResult, EmptyState, IconButton, LinearProgress,
    StatusPill, Toggle,
};
use crate::{icons, job_text};

/// Shown after releasing the card.
const RELEASED_HELP: &str = concat!(
    "The firmware let go of the card (M22): it is safe to remove it. Insert a card ",
    "and initialize it to use it again.",
);

/// Shown for firmware built without SD card support.
const NO_SD_SUPPORT: &str = concat!(
    "The firmware reports no SD card support (Cap:SDCARD:0), so it can neither list ",
    "nor print files from a card. Print from Sprint instead.",
);

/// Why Upload is disabled on firmware that cannot write to its card.
const NO_SD_WRITE: &str = "The firmware cannot write to its card (Cap:SD_WRITE:0)";

/// Shown for an empty card when a file is loaded.
const EMPTY_WITH_FILE: &str =
    "The card is empty. Upload the loaded file to print it from the card.";

/// Shown before the file list was read on this connection.
const NOT_READ: &str = concat!(
    "Sprint has not read the card's file list since connecting. It is read ",
    "automatically when no job runs.",
);

/// Why the card cannot be read or written while a job runs.
const BUSY: &str = "Not while a job runs";

/// Body of the print confirmation.
const PRINT_MESSAGE: &str = concat!(
    "The printer reads the file from its card while Sprint shows the progress. Make ",
    "sure the bed is clear and ready.",
);

/// Note in the upload prompt.
const UPLOAD_NOTE: &str = concat!(
    "Every line goes out with a checksum, so uploading over USB takes a while; the ",
    "printer does nothing else meanwhile.",
);

/// How long (UI seconds) the tab may have been away when it sees an upload
/// with "print when done" finish and still start the print. Panels only run
/// while they are shown: an upload that finished while the tab was hidden
/// is not printed when the user comes back, maybe hours later.
const PRINT_AFTER_GRACE: f64 = 5.0;

/// Shown when "print when done" did not start the print.
const NOT_PRINTED_MESSAGE: &str = concat!(
    "It was not started automatically: the upload finished while the SD card ",
    "tab was not open. Print it from the list once the printer is ready.",
);

/// A question waiting for the user.
#[derive(Clone, Debug, PartialEq)]
enum Confirm {
    /// Print a file (8.3 name, shown name).
    Print(String, String),
    /// Delete a file (8.3 name, shown name).
    Delete(String, String),
    /// Cancel the running SD job.
    CancelJob(JobKind),
}

/// The upload prompt.
#[derive(Clone, Debug, PartialEq)]
struct UploadPrompt {
    /// The name being typed.
    name: String,
    /// Start printing once the upload is done.
    print_after: bool,
}

/// SD card tab: file list, printing, upload and deletion.
#[derive(Debug, Default)]
pub struct SdCardTab {
    sort: Sort,
    filter: String,
    confirm: Option<Confirm>,
    upload: Option<UploadPrompt>,
    /// Print this file when its upload completes.
    print_after_upload: Option<String>,
    /// UI time when the tab last saw that upload running.
    upload_seen: Option<f64>,
    /// The listing was requested for this connection.
    auto_requested: bool,
    /// The card was released (M22) from here.
    released: bool,
    /// The SD progress the host still reports after an SD print ended (a
    /// cancelled print's last report stays in `sd.printing`): not a print.
    stale_progress: Option<(u64, u64)>,
    /// The card's print progress this frame, stale reports left out.
    card_printing: Option<(u64, u64)>,
    /// The file list was read on this connection (a listing ended, or files
    /// are known): only then does an empty list mean an empty card.
    read: bool,
    /// A listing was seen running on this connection.
    listing_seen: bool,
    connection: kit::ConnectionWatch,
}

/// Room the "Open a file to upload…" button needs next to Initialize and
/// Release (points).
const OPEN_FILE_WIDTH: f32 = 190.0;

/// The button that asks the shell to open a G-code file.
fn open_file_button() -> ActionButton<'static> {
    ActionButton::new("Open a file to upload…")
        .icon(icons::OPEN)
        .ghost()
        .small()
}

/// The SD job in the state, if the last job was one.
fn sd_job(state: &PrinterState) -> Option<&JobInfo> {
    state
        .job
        .as_ref()
        .filter(|j| matches!(j.kind, JobKind::SdPrint | JobKind::SdUpload))
}

/// Tone of a job status.
fn status_tone(status: &JobStatus) -> Tone {
    match status {
        JobStatus::Completed => Tone::Success,
        JobStatus::Failed(_) => Tone::Danger,
        JobStatus::Cancelled | JobStatus::Cancelling => Tone::Neutral,
        JobStatus::Paused | JobStatus::Pausing => Tone::Warning,
        _ => Tone::Accent,
    }
}

impl SdCardTab {
    /// Draw the panel.
    pub fn ui(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        self.track(ui, ctx);
        self.header(ui, ctx);
        ui.add_space(theme::GAP);
        if !ctx.connected() {
            ui.add_space(40.0);
            let _ = EmptyState::new(icons::SD_CARD, "Not connected")
                .message("Connect to a printer to see the files on its SD card.")
                .show(ui);
            return;
        }
        if kit::lacks_capability(ctx.state, "SDCARD") {
            ui.add_space(40.0);
            let _ = EmptyState::new(icons::SD_CARD, "No SD card support")
                .message(NO_SD_SUPPORT)
                .show(ui);
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("sd-card-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.job_card(ui, ctx) {
                    ui.add_space(theme::GAP_LG);
                }
                if self.released {
                    if kit::notice(
                        ui,
                        Tone::Info,
                        icons::INFO,
                        "Card released",
                        Some(RELEASED_HELP),
                        Some("Initialize card"),
                    ) && ctx.command_blocker().is_none()
                    {
                        self.init_card(ctx);
                    }
                    ui.add_space(theme::GAP_LG);
                }
                self.files_card(ui, ctx);
                ui.add_space(theme::GAP_LG);
            });
        self.dialogs(ui, ctx);
    }

    /// The table's sort order.
    pub fn sort(&self) -> Sort {
        self.sort
    }

    /// Filter the table (case-insensitive, long or 8.3 names).
    pub fn set_filter(&mut self, filter: &str) {
        self.filter = filter.to_owned();
    }

    /// Open the upload prompt for the loaded file (no-op without one).
    pub fn open_upload(&mut self, ctx: &AppCtx<'_>) {
        if let Some(file) = ctx.file {
            let taken: Vec<&str> = ctx.state.sd.files.iter().map(|f| f.name.as_str()).collect();
            self.upload = Some(UploadPrompt {
                name: sdname::suggest(file.file.name(), &taken),
                print_after: false,
            });
        }
    }

    /// Whether the upload prompt is open.
    pub fn upload_open(&self) -> bool {
        self.upload.is_some()
    }

    /// A file listing ended on this connection (the shell forwards the
    /// printer's [`Event::SdFiles`](sprint_host::Event::SdFiles)): an empty
    /// list now means an empty card. The listing of an empty card begins
    /// and ends in one batch of replies, between two frames, so the state
    /// alone never shows it.
    pub fn listing_finished(&mut self) {
        self.read = true;
    }

    fn init_card(&mut self, ctx: &AppCtx<'_>) {
        ctx.printer.send("M21");
        ctx.printer.request_sd_files();
        self.released = false;
    }

    /// The card's print progress: `sd.printing`, unless it is what an SD
    /// print that has ended last reported. The host keeps that report when
    /// the print is cancelled, which would otherwise read as a print
    /// started on the printer and keep the card busy for good.
    fn live_card_progress(&mut self, state: &PrinterState) -> Option<(u64, u64)> {
        let reported = state.sd.printing;
        if reported.is_none() {
            self.stale_progress = None;
            return None;
        }
        let ended = state
            .job
            .as_ref()
            .is_some_and(|j| j.kind == JobKind::SdPrint && j.status.is_finished());
        if ended && self.stale_progress.is_none() {
            self.stale_progress = reported;
        }
        // A new report (a print started on the printer) is live again.
        reported.filter(|p| Some(*p) != self.stale_progress)
    }

    /// Connection bookkeeping, the automatic listing and print-after-upload.
    fn track(&mut self, ui: &Ui, ctx: &mut AppCtx<'_>) {
        self.card_printing = self.live_card_progress(ctx.state);
        // Questions and the upload prompt belong to the connection they
        // were opened on: a "Delete …?" must not come back on the next one.
        if self.connection.is_new(ctx.state) {
            self.auto_requested = false;
            self.released = false;
            self.confirm = None;
            self.upload = None;
            self.read = false;
            self.listing_seen = false;
        }
        if !ctx.connected() {
            self.read = false;
            self.listing_seen = false;
            self.auto_requested = false;
            self.print_after_upload = None;
            self.upload_seen = None;
            self.released = false;
            self.confirm = None;
            self.upload = None;
            return;
        }
        let sd = &ctx.state.sd;
        if sd.listing {
            self.listing_seen = true;
        } else if self.listing_seen || !sd.files.is_empty() {
            self.read = true;
        }
        if !self.auto_requested
            && !ctx.job_active()
            && ctx.command_blocker().is_none()
            && !kit::lacks_capability(ctx.state, "SDCARD")
            && sd.card_present != Some(false)
            && sd.files.is_empty()
            && !sd.listing
        {
            ctx.printer.request_sd_files();
            self.auto_requested = true;
        }
        let Some(wanted) = self.print_after_upload.clone() else {
            return;
        };
        let Some(job) = ctx.state.job.as_ref() else {
            return;
        };
        if job.kind != JobKind::SdUpload || !job.name.eq_ignore_ascii_case(&wanted) {
            return;
        }
        let now = ui.input(|i| i.time);
        match &job.status {
            JobStatus::Completed => {
                self.print_after_upload = None;
                let watched = self
                    .upload_seen
                    .take()
                    .is_some_and(|seen| (0.0..=PRINT_AFTER_GRACE).contains(&(now - seen)));
                if !watched {
                    ctx.toasts
                        .info(format!("{wanted} is on the card"))
                        .message(NOT_PRINTED_MESSAGE);
                    return;
                }
                match ctx.printer.start_sd_print(&wanted) {
                    Ok(_) => {
                        ctx.toasts
                            .success(format!("Printing {wanted}"))
                            .message("The upload finished; the printer prints from its card now.");
                    }
                    Err(err) => {
                        ctx.toasts
                            .error(format!("Could not print {wanted}"))
                            .message(err.to_string());
                    }
                }
            }
            JobStatus::Failed(_) | JobStatus::Cancelled => {
                self.print_after_upload = None;
                self.upload_seen = None;
            }
            _ => self.upload_seen = Some(now),
        }
    }

    /// Why the loaded file cannot be uploaded now (`None`: it can).
    fn upload_blocker(&self, ctx: &AppCtx<'_>) -> Option<&'static str> {
        let state = ctx.state;
        if ctx.file.is_none() {
            Some("Open a G-code file to upload it")
        } else if let Some(problem) = ctx
            .file
            .and_then(|f| crate::panels::job_bar::content_blocker(&f.file))
        {
            Some(problem)
        } else if let Some(reason) = ctx.command_blocker() {
            Some(reason)
        } else if kit::lacks_capability(state, "SDCARD") || kit::lacks_capability(state, "SD_WRITE")
        {
            Some(NO_SD_WRITE)
        } else if ctx.job_active() || self.card_printing.is_some() {
            Some(BUSY)
        } else if self.released {
            Some("Initialize the card first")
        } else {
            None
        }
    }

    /// Why the file list cannot be read now (`None`: it can). Not while a
    /// job runs: the listing would come between its lines, and M20 can
    /// disturb a print from the card.
    fn read_blocker(&self, ctx: &AppCtx<'_>) -> Option<&'static str> {
        ctx.command_blocker()
            .or_else(|| (ctx.job_active() || self.card_printing.is_some()).then_some(BUSY))
    }

    fn header(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let state = ctx.state;
        let sd = &state.sd;
        let total: u64 = sd.files.iter().filter_map(|f| f.size).sum();
        let no_sd = kit::lacks_capability(state, "SDCARD");
        let subtitle = if !ctx.connected() {
            "Files on the printer's card".to_owned()
        } else if no_sd {
            "Not supported by the firmware".to_owned()
        } else if sd.card_present == Some(false) {
            "No card".to_owned()
        } else if self.released {
            "Released".to_owned()
        } else if sd.files.is_empty() && sd.listing {
            "Reading the card…".to_owned()
        } else if sd.files.is_empty() && !self.read {
            "Not read yet".to_owned()
        } else {
            format!(
                "{} file{} · {}",
                sd.files.len(),
                if sd.files.len() == 1 { "" } else { "s" },
                format::bytes(total)
            )
        };
        let connected = ctx.connected();
        let upload_blocker = self.upload_blocker(ctx);
        let read_blocker = self.read_blocker(ctx);
        let file_name = ctx.file.map(|f| f.file.name().to_owned());
        let mut refresh = false;
        let mut upload = false;
        kit::page_header(ui, icons::SD_CARD, "SD card", Some(&subtitle), |ui| {
            let upload_tip = match (upload_blocker, &file_name) {
                (Some(reason), _) => reason.to_owned(),
                (None, Some(name)) => format!("Write {name} to the card"),
                (None, None) => String::new(),
            };
            if ui
                .add_enabled(
                    upload_blocker.is_none(),
                    ActionButton::new("Upload")
                        .icon(icons::UPLOAD)
                        .primary()
                        .tooltip(upload_tip),
                )
                .clicked()
            {
                upload = true;
            }
            if ui
                .add_enabled(
                    read_blocker.is_none() && !no_sd,
                    IconButton::new(icons::REFRESH).tooltip("Read the file list again (M20)"),
                )
                .on_disabled_hover_text(read_blocker.unwrap_or_default())
                .clicked()
            {
                refresh = true;
            }
            if connected && !no_sd {
                let (text, tone) = match sd.card_present {
                    _ if self.released => ("Released", Tone::Neutral),
                    Some(true) => ("Card ready", Tone::Success),
                    Some(false) => ("No card", Tone::Warning),
                    None => ("Card unknown", Tone::Neutral),
                };
                ui.add(StatusPill::new(text, tone).icon(icons::SD_CARD));
            }
        });
        if refresh {
            if self.released {
                // The card was let go of: mount it again to read it.
                self.init_card(ctx);
            } else {
                ctx.printer.request_sd_files();
            }
        }
        if upload {
            self.open_upload(ctx);
        }
    }

    /// The running (or last) SD print or upload; returns whether a card
    /// was drawn.
    fn job_card(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) -> bool {
        let state = ctx.state;
        if let Some(job) = sd_job(state) {
            let job = job.clone();
            self.job_progress(ui, ctx, &job);
            return true;
        }
        // Printing from the card without a host job (started on the
        // printer's display).
        if let Some((done, total)) = self.card_printing.filter(|(_, t)| *t > 0) {
            let name = state
                .sd
                .selected
                .clone()
                .unwrap_or_else(|| "SD print".to_owned());
            Card::new("Printing from the card")
                .icon(icons::SD_CARD)
                .subtitle(&name)
                .show(ui, |ui| {
                    let fraction = done as f32 / total as f32;
                    ui.add(
                        LinearProgress::new(fraction)
                            .label(&format!(
                                "{} of {}",
                                format::bytes(done),
                                format::bytes(total)
                            ))
                            .show_percentage(true),
                    );
                    ui.add_space(4.0);
                    kit::caption(
                        ui,
                        "Started on the printer; control it there or from the console.",
                    );
                });
            return true;
        }
        false
    }

    fn job_progress(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>, job: &JobInfo) {
        let upload = job.kind == JobKind::SdUpload;
        let title = if upload { "Upload" } else { "SD print" };
        let icon = if upload { icons::UPLOAD } else { icons::PLAY };
        let active = job.status.is_active();
        let p = theme::ui_palette(ui);
        Card::new(title)
            .icon(icon)
            .subtitle(&job.name)
            .show_with_header(
                ui,
                |ui| {
                    ui.add(
                        StatusPill::new(job.status.label(), status_tone(&job.status))
                            .pulse(active && !job.status.is_paused()),
                    );
                },
                |ui| {
                    let fraction = if job.progress.is_finite() {
                        job.progress.clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let label = if job.bytes_total > 0 {
                        format!(
                            "{} of {}",
                            format::bytes(job.bytes_done.min(job.bytes_total)),
                            format::bytes(job.bytes_total)
                        )
                    } else if job.total_lines > 0 {
                        format!(
                            "Line {} of {}",
                            format::count(job.current_line as u64),
                            format::count(job.total_lines as u64)
                        )
                    } else {
                        String::new()
                    };
                    ui.add(
                        LinearProgress::new(fraction)
                            .label(&label)
                            .show_percentage(true),
                    );
                    ui.add_space(8.0);
                    let elapsed = job.elapsed.as_secs_f64();
                    let rate = if elapsed > 0.5 && job.bytes_done > 0 {
                        format::rate(job.bytes_done as f64 / 1024.0 / elapsed, "KB/s")
                    } else {
                        format::MISSING.to_owned()
                    };
                    let eta = job.eta.filter(|_| active).map_or_else(
                        || format::MISSING.to_owned(),
                        |d| format::duration(d.as_secs_f64()),
                    );
                    kit::metric_grid(
                        ui,
                        3,
                        &[
                            ("Elapsed", format::duration(elapsed), None),
                            ("Remaining", eta, None),
                            (
                                if upload { "Transfer rate" } else { "Read rate" },
                                rate,
                                None,
                            ),
                        ],
                    );
                    if let JobStatus::Failed(reason) = &job.status {
                        ui.add_space(4.0);
                        ui.label(RichText::new(reason).color(p.danger).size(12.5));
                    }
                    if !active {
                        return;
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if !upload {
                            if job.status.is_paused() {
                                if ui
                                    .add(ActionButton::new("Resume").icon(icons::PLAY).primary())
                                    .clicked()
                                {
                                    ctx.printer.resume();
                                }
                            } else if ui
                                .add(ActionButton::new("Pause").icon(icons::PAUSE))
                                .clicked()
                            {
                                ctx.printer.pause();
                            }
                        }
                        let label = if upload {
                            "Cancel upload"
                        } else {
                            "Cancel print"
                        };
                        if ui
                            .add(ActionButton::new(label).icon(icons::STOP).danger())
                            .clicked()
                        {
                            self.confirm = Some(Confirm::CancelJob(job.kind));
                        }
                    });
                },
            );
    }

    fn files_card(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let state = ctx.state;
        let sd = &state.sd;
        let blocker = ctx.command_blocker();
        let ready = blocker.is_none();
        let sd_busy =
            sd_job(state).is_some_and(|j| j.status.is_active()) || self.card_printing.is_some();
        // No new print while any job runs, nor while the card prints a
        // file started on the printer (M23 would swap it).
        let job_active = ctx.job_active() || self.card_printing.is_some();
        // After M22 the files can be neither printed nor deleted until the
        // card is initialized again.
        let released = self.released;
        let wants_file = ctx.file.is_none();
        let mut open_room = true;
        let mut open_file = false;
        // Edited in the header, read by the table.
        let filter = std::cell::RefCell::new(std::mem::take(&mut self.filter));
        Card::new("Files")
            .icon(icons::FILE)
            .subtitle(if sd.listing {
                "reading…"
            } else {
                "on the card"
            })
            .show_with_header(
                ui,
                |ui| {
                    if !sd.files.is_empty() {
                        ui.add(
                            egui::TextEdit::singleline(&mut *filter.borrow_mut())
                                .hint_text(format!("{} Filter", icons::SEARCH))
                                .desired_width(170.0),
                        );
                    }
                },
                |ui| {
                    if sd.listing {
                        ui.add(LinearProgress::indeterminate().label("Reading the card…"));
                        ui.add_space(8.0);
                    }
                    if sd.card_present == Some(false) {
                        let mut empty = EmptyState::new(icons::SD_CARD, "No SD card")
                            .message("The printer reports no card. Insert one and initialize it.");
                        if ready {
                            empty = empty.action("Initialize card");
                        }
                        if empty.show(ui).action_clicked() && ready {
                            self.init_card(ctx);
                        }
                    } else if sd.files.is_empty() && !sd.listing && !self.read {
                        // Nothing read yet (a job ran since connecting): not
                        // an empty card.
                        let reason = self.read_blocker(ctx);
                        let r = EmptyState::new(icons::SD_CARD, "Not read yet")
                            .message(NOT_READ)
                            .action("Read the card")
                            .action_icon(icons::REFRESH)
                            .action_blocked(reason)
                            .show(ui);
                        if r.action_clicked() && reason.is_none() {
                            ctx.printer.request_sd_files();
                        }
                    } else if sd.files.is_empty() && !sd.listing {
                        let has_file = ctx.file.is_some();
                        let mut empty =
                            EmptyState::new(icons::FILE, "No files").message(if has_file {
                                EMPTY_WITH_FILE
                            } else {
                                "The card is empty."
                            });
                        let reason = self.upload_blocker(ctx);
                        if has_file {
                            empty = empty.action("Upload loaded file").action_blocked(reason);
                        }
                        if empty.show(ui).action_clicked() && reason.is_none() {
                            self.open_upload(ctx);
                        }
                    } else if !sd.files.is_empty() {
                        table::header(ui, &mut self.sort);
                        let filter = filter.borrow().clone();
                        let rows = sorted_indices(&sd.files, self.sort, &filter);
                        if rows.is_empty() {
                            ui.add_space(8.0);
                            kit::caption(
                                ui,
                                &format!("No file matches \u{201c}{}\u{201d}.", filter.trim()),
                            );
                        }
                        let selected = sd.selected.as_deref();
                        for (n, &i) in rows.iter().enumerate() {
                            // Rows scrolled out of view only take their
                            // space: a card with thousands of files stays
                            // cheap to draw.
                            let row_rect = egui::Rect::from_min_size(
                                ui.cursor().min,
                                egui::vec2(ui.available_width(), table::ROW_H),
                            );
                            if !ui.is_rect_visible(row_rect) {
                                ui.allocate_exact_size(row_rect.size(), egui::Sense::hover());
                                continue;
                            }
                            let file = &sd.files[i];
                            let is_selected = selected.is_some_and(|s| {
                                s.eq_ignore_ascii_case(&file.name)
                                    || file
                                        .long_name
                                        .as_deref()
                                        .is_some_and(|l| l.eq_ignore_ascii_case(s))
                            });
                            let action = ui
                                .push_id(("sd-file", &file.name), |ui| {
                                    table::row(
                                        ui,
                                        file,
                                        is_selected,
                                        n % 2 == 1,
                                        (
                                            ready && !job_active && !released,
                                            ready && !sd_busy && !released,
                                        ),
                                    )
                                })
                                .inner;
                            let label = display_name(file).to_owned();
                            match action {
                                Some(table::RowAction::Print) => {
                                    self.confirm = Some(Confirm::Print(file.name.clone(), label));
                                }
                                Some(table::RowAction::Delete) => {
                                    self.confirm = Some(Confirm::Delete(file.name.clone(), label));
                                }
                                None => {}
                            }
                        }
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        // M21 remounts the card: never under a running SD
                        // print or upload.
                        if ui
                            .add_enabled(
                                ready && !sd_busy,
                                ActionButton::new("Initialize")
                                    .icon(icons::SD_CARD)
                                    .ghost()
                                    .small()
                                    .tooltip("M21: mount the card (after inserting it)"),
                            )
                            .on_disabled_hover_text(
                                blocker.unwrap_or("Not while the card is in use"),
                            )
                            .clicked()
                        {
                            self.init_card(ctx);
                        }
                        if ui
                            .add_enabled(
                                ready && !sd_busy && !released,
                                ActionButton::new("Release")
                                    .icon(icons::phosphor::EJECT)
                                    .ghost()
                                    .small()
                                    .tooltip("M22: let go of the card so it can be removed safely"),
                            )
                            .clicked()
                        {
                            ctx.printer.send("M22");
                            self.released = true;
                        }
                        // Right-aligned when it fits, else on the next row
                        // (instead of over Release).
                        open_room = ui.available_width() >= OPEN_FILE_WIDTH;
                        if wants_file && open_room {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                open_file = ui.add(open_file_button()).clicked();
                            });
                        }
                    });
                    if wants_file && !open_room {
                        ui.add_space(2.0);
                        open_file = ui.add(open_file_button()).clicked();
                    }
                },
            );
        if open_file {
            ctx.actions.push(AppAction::OpenFileDialog);
        }
        self.filter = filter.into_inner();
    }

    fn dialogs(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        self.upload_dialog(ui, ctx);
        let Some(confirm) = self.confirm.clone() else {
            return;
        };
        // The job asked about ended while the question was open: confirming
        // would cancel whatever runs by then.
        if let Confirm::CancelJob(kind) = confirm
            && !sd_job(ctx.state).is_some_and(|j| j.kind == kind && j.status.is_active())
        {
            self.confirm = None;
            return;
        }
        let answer = match &confirm {
            Confirm::Print(name, label) => {
                let title = format!("Print {label}?");
                let message = PRINT_MESSAGE;
                ConfirmDialog::new("sd-print", &title)
                    .message(message)
                    .details(&format!("M23 {name}\nM24"))
                    .confirm_label("Start print")
                    .icon(icons::PLAY)
                    .show(ui.ctx())
            }
            Confirm::Delete(name, label) => {
                let title = format!("Delete {label}?");
                ConfirmDialog::new("sd-delete", &title)
                    .message("The file is removed from the SD card. This cannot be undone.")
                    .details(&format!("M30 {name}"))
                    .confirm_label("Delete")
                    .danger(true)
                    .icon(icons::DELETE)
                    .show(ui.ctx())
            }
            Confirm::CancelJob(kind) => {
                // Worded like the job bar's question (job_text).
                let (name, percent) = sd_job(ctx.state).map_or((String::new(), 0), |j| {
                    (j.name.clone(), crate::format::progress_percent(j.progress))
                });
                let question = job_text::cancel_question(
                    *kind,
                    &name,
                    percent,
                    &ctx.config.active().cancel_script,
                );
                ConfirmDialog::new("sd-cancel", &question.title)
                    .message(&question.message)
                    .confirm_label(question.confirm)
                    .cancel_label(question.keep)
                    .danger(true)
                    .show(ui.ctx())
            }
        };
        match answer {
            Some(DialogResult::Confirmed) => {
                self.confirm = None;
                match confirm {
                    Confirm::Print(name, label) => match ctx.printer.start_sd_print(&name) {
                        Ok(_) => {
                            ctx.toasts.success(format!("Printing {label}"));
                        }
                        Err(err) => {
                            ctx.toasts
                                .error(format!("Could not print {label}"))
                                .message(err.to_string());
                        }
                    },
                    Confirm::Delete(name, label) => {
                        ctx.printer.delete_sd_file(&name);
                        ctx.toasts.info(format!("Deleting {label}"));
                    }
                    Confirm::CancelJob(_) => ctx.printer.cancel(),
                }
            }
            Some(DialogResult::Cancelled) => self.confirm = None,
            None => {}
        }
    }

    fn upload_dialog(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let Some(prompt) = self.upload.as_mut() else {
            return;
        };
        let Some(loaded) = ctx.file else {
            self.upload = None;
            return;
        };
        let p = theme::palette(ui.ctx());
        let file = &loaded.file;
        let validated = sdname::validate(&prompt.name);
        let exists = validated.as_ref().ok().is_some_and(|n| {
            ctx.state
                .sd
                .files
                .iter()
                .any(|f| f.name.trim().eq_ignore_ascii_case(n))
        });
        let can_upload = validated.is_ok()
            && ctx.command_blocker().is_none()
            && !ctx.job_active()
            && self.card_printing.is_none()
            && !self.released
            && !kit::lacks_capability(ctx.state, "SDCARD")
            && !kit::lacks_capability(ctx.state, "SD_WRITE");
        let mut submit = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("sd-upload"))
            .frame(kit::modal_frame(ui.ctx()))
            .show(ui.ctx(), |ui| {
                ui.set_width(420.0);
                ui.spacing_mut().item_spacing.y = 8.0;
                kit::dialog_title(ui, icons::UPLOAD, "Upload to the SD card", p.accent);
                kit::dim(
                    ui,
                    &format!(
                        "{} · {} · {} lines",
                        file.name(),
                        format::bytes(file.source_len()),
                        format::count(file.len() as u64)
                    ),
                );
                ui.add_space(2.0);
                kit::caption(ui, "NAME ON THE CARD");
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut prompt.name)
                        .hint_text("NAME.GCO")
                        .char_limit(12)
                        .desired_width(f32::INFINITY)
                        .margin(egui::Margin::symmetric(8, 6))
                        .font(egui::TextStyle::Monospace),
                );
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                name_feedback(ui, &validated, exists);
                ui.add(
                    Toggle::new(&mut prompt.print_after).label("Print it when the upload finishes"),
                );
                kit::caption(ui, UPLOAD_NOTE);
                ui.add_space(4.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let label = match &validated {
                        Ok(name) => format!("Upload as {name}"),
                        Err(_) => "Upload".to_owned(),
                    };
                    if ui
                        .add_enabled(
                            can_upload,
                            ActionButton::new(&label)
                                .icon(icons::UPLOAD)
                                .primary()
                                .min_width(96.0),
                        )
                        .clicked()
                    {
                        submit = true;
                    }
                    if ui
                        .add(ActionButton::new("Cancel").min_width(96.0))
                        .clicked()
                    {
                        cancel = true;
                    }
                });
            });
        if modal.should_close() {
            cancel = true;
        }
        if cancel {
            self.upload = None;
            return;
        }
        if !(submit && can_upload) {
            return;
        }
        let Ok(name) = validated else {
            return;
        };
        let print_after = prompt.print_after;
        self.upload = None;
        match ctx.printer.upload_to_sd(file.clone(), &name) {
            Ok(()) => {
                self.released = false;
                self.print_after_upload = print_after.then(|| name.clone());
                self.upload_seen = print_after.then(|| ui.input(|i| i.time));
                ctx.toasts
                    .info(format!("Uploading {name}"))
                    .message(if print_after {
                        "Keep this tab open: the print starts when the upload is done."
                    } else {
                        "The progress shows at the top of this tab."
                    });
            }
            Err(err) => {
                ctx.toasts
                    .error(format!("Could not upload {name}"))
                    .message(err.to_string());
            }
        }
    }
}

/// What the upload prompt says about the typed name: why it is invalid,
/// that it replaces a file, or how it is stored.
fn name_feedback(ui: &mut Ui, validated: &Result<String, sdname::NameError>, exists: bool) {
    let p = theme::ui_palette(ui);
    match validated {
        Err(err) => {
            ui.label(RichText::new(err.message()).color(p.danger).size(12.5));
        }
        Ok(name) if exists => {
            let text = format!("{name} is already on the card and will be replaced.");
            ui.label(RichText::new(text).color(p.warning).size(12.5));
        }
        Ok(name) => {
            let text = format!(
                "Stored as {name}: up to 8 characters, a dot and 3 more (8.3), which every \
                 firmware can open."
            );
            kit::caption(ui, &text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_tones() {
        assert_eq!(status_tone(&JobStatus::Completed), Tone::Success);
        assert_eq!(status_tone(&JobStatus::Failed("x".into())), Tone::Danger);
        assert_eq!(status_tone(&JobStatus::Paused), Tone::Warning);
        assert_eq!(status_tone(&JobStatus::Printing), Tone::Accent);
    }

    #[test]
    fn sd_jobs_only() {
        let mut state = PrinterState::default();
        assert!(sd_job(&state).is_none());
        state.job = Some(JobInfo::new(JobKind::HostPrint, "x.gcode", 10, 100));
        assert!(sd_job(&state).is_none());
        state.job = Some(JobInfo::new(JobKind::SdUpload, "X.GCO", 10, 100));
        assert!(sd_job(&state).is_some());
    }
}
