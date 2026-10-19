//! The job bar (open / print / pause / cancel, progress and job
//! statistics) and the status bar (link statistics, busy state, the last
//! firmware message and the app version) at the bottom of the window.

use egui::{Align, Layout, RichText, Ui, vec2};
use sprint_host::{JobInfo, JobKind, JobStatus, PrinterState};

use crate::context::{AppAction, AppCtx};
use crate::loader::{LoadStatus, Stage};
use crate::theme::{self, Palette, TextRole, Tone};
use crate::widgets::{Badge, IconButton, LinearProgress, status_dot};
use crate::{format, icons};

/// Why printing is not possible right now, if it is not.
pub fn print_blocker(state: &PrinterState, has_file: bool) -> Option<&'static str> {
    if !has_file {
        Some("Open a G-code file first")
    } else if !state.is_connected() {
        Some("Connect to a printer first")
    } else if state.halted {
        Some("The printer is halted: restart the firmware (M999) or reconnect")
    } else if state.job_active() {
        Some("A job is already running")
    } else {
        None
    }
}

/// The printer waits for the user (M0 / M1, a filament change) and no host
/// prompt offers a way on: its busy reports say "paused for user" (or
/// "for input"). Firmware without host prompt support then waits until
/// it receives M108, which [`continue_button`] sends.
pub fn waiting_for_user(state: &PrinterState) -> bool {
    state.is_connected()
        && !state.halted
        && state.prompt.is_none()
        && state.busy.as_deref().is_some_and(|busy| {
            let busy = busy.to_ascii_lowercase();
            [
                "paused for user",
                "paused for input",
                "wait for user",
                "waiting for user",
            ]
            .iter()
            .any(|w| busy.contains(w))
        })
}

/// What the Continue button says on hover.
pub const CONTINUE_TIP: &str =
    "The printer waits for you (M0, M1 or a filament change): continue (M108)";

/// "Continue" while the printer waits for the user (see
/// [`waiting_for_user`]): sends M108 at once. Returns whether it was
/// clicked.
pub fn continue_button(ui: &mut Ui, ctx: &AppCtx<'_>, label: &str) -> bool {
    let clicked = ui
        .add(
            crate::widgets::ActionButton::new(label)
                .icon(icons::PLAY)
                .primary()
                .small()
                .tooltip(CONTINUE_TIP),
        )
        .clicked();
    if clicked {
        ctx.printer.send_immediate("M108");
    }
    clicked
}

/// Badge text and tone of a job status.
pub fn job_badge(job: &JobInfo) -> (String, Tone) {
    let tone = match &job.status {
        JobStatus::Printing | JobStatus::Starting | JobStatus::Resuming => Tone::Accent,
        JobStatus::Pausing | JobStatus::Paused | JobStatus::Cancelling => Tone::Warning,
        JobStatus::Completed => Tone::Success,
        JobStatus::Cancelled => Tone::Neutral,
        JobStatus::Failed(_) => Tone::Danger,
    };
    let what = match (job.kind, &job.status) {
        (JobKind::SdUpload, s) if s.is_active() => "Uploading".to_owned(),
        (JobKind::SdPrint, JobStatus::Printing) => "SD printing".to_owned(),
        (_, status) => status.label().to_owned(),
    };
    (what.to_uppercase(), tone)
}

/// The job bar.
#[derive(Default)]
pub struct JobBar {}

impl JobBar {
    /// Draw the bar. `load` is the file loader's running job.
    pub fn ui(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>, load: Option<&LoadStatus>) {
        let p = theme::ui_palette(ui);
        let job = ctx.state.job.clone();
        let active = job.as_ref().is_some_and(|j| j.status.is_active());
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            self.buttons(ui, ctx, job.as_ref(), active);
            ui.add_space(10.0);
            // Statistics from the right, the summary takes what is left.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                match job.as_ref().filter(|_| active) {
                    Some(job) => job_stats(ui, job, &p),
                    None => file_stats(ui, ctx, &p),
                }
                ui.add_space(6.0);
                let size = vec2(ui.available_width(), ui.available_height());
                ui.allocate_ui_with_layout(size, Layout::top_down(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    self.summary(ui, ctx, job.as_ref(), active, load, &p);
                });
            });
        });
    }

    fn buttons(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>, job: Option<&JobInfo>, active: bool) {
        if ui
            .add(IconButton::new(icons::OPEN).tooltip("Open G-code… (Ctrl+O)"))
            .clicked()
        {
            ctx.actions.push(AppAction::OpenFileDialog);
        }
        let blocker = print_blocker(ctx.state, ctx.file.is_some());
        let print = ui
            .add_enabled(
                blocker.is_none(),
                IconButton::new(icons::PLAY)
                    .primary()
                    .tooltip("Start print… (Ctrl+P)"),
            )
            .on_disabled_hover_text(blocker.unwrap_or(""));
        if print.clicked() {
            ctx.actions
                .push(AppAction::StartPrintDialog { start_line: 0 });
        }
        let paused = job.is_some_and(|j| j.status.is_paused());
        let can_pause = active
            && job.is_some_and(|j| {
                matches!(
                    j.status,
                    JobStatus::Printing | JobStatus::Paused | JobStatus::Starting
                ) && j.kind != JobKind::SdUpload
            });
        let (icon, tip) = if paused {
            (icons::PLAY, "Resume (Space)")
        } else {
            (icons::PAUSE, "Pause (Space)")
        };
        let pause = ui
            .add_enabled(can_pause, IconButton::new(icon).tooltip(tip))
            .on_disabled_hover_text("No running print");
        if pause.clicked() {
            ctx.actions.push(if paused {
                AppAction::ResumeJob
            } else {
                AppAction::PauseJob
            });
        }
        let cancel = ui
            .add_enabled(
                active && job.is_some_and(|j| j.status != JobStatus::Cancelling),
                IconButton::new(icons::STOP)
                    .danger()
                    .tooltip("Cancel the job…"),
            )
            .on_disabled_hover_text("No running job");
        if cancel.clicked() {
            ctx.actions.push(AppAction::CancelJob);
        }
        if waiting_for_user(ctx.state) {
            continue_button(ui, ctx, "Continue");
        }
    }

    fn summary(
        &mut self,
        ui: &mut Ui,
        ctx: &mut AppCtx<'_>,
        job: Option<&JobInfo>,
        active: bool,
        load: Option<&LoadStatus>,
        p: &Palette,
    ) {
        let title = theme::font(ui, TextRole::Title);
        ui.add_space(2.0);
        if let (Some(job), true) = (job, active) {
            ui.horizontal(|ui| {
                let (text, tone) = job_badge(job);
                ui.add(Badge::new(&text).tone(tone));
                // Names are cut to the room left of the statistics.
                ui.add(
                    egui::Label::new(
                        RichText::new(format::sanitize(&job.name, 120))
                            .font(title.clone())
                            .color(p.text),
                    )
                    .truncate(),
                );
            });
            let fraction = job.progress.clamp(0.0, 1.0);
            let tone = if job.status.is_paused() {
                Tone::Warning
            } else {
                Tone::Accent
            };
            ui.add(
                LinearProgress::new(fraction)
                    .tone(tone)
                    .height(6.0)
                    .show_percentage(true),
            );
            return;
        }
        if let Some(load) = load {
            ui.horizontal(|ui| {
                // Room for the cancel button right of the name.
                ui.scope(|ui| {
                    ui.set_max_width((ui.available_width() - 30.0).max(40.0));
                    ui.add(
                        egui::Label::new(
                            RichText::new(format::sanitize(&load.name, 120))
                                .font(title.clone())
                                .color(p.text),
                        )
                        .truncate(),
                    );
                });
                if ui
                    .add(
                        IconButton::new(icons::CLOSE)
                            .ghost()
                            .small()
                            .tooltip("Cancel loading"),
                    )
                    .clicked()
                {
                    ctx.actions.push(AppAction::CancelLoading);
                }
            });
            match load.stage {
                Stage::Reading => {
                    ui.add(
                        LinearProgress::indeterminate()
                            .height(6.0)
                            .label("Reading file…"),
                    );
                }
                Stage::Analyzing => {
                    ui.add(
                        LinearProgress::new(load.progress)
                            .height(6.0)
                            .label("Analyzing toolpath")
                            .show_percentage(true),
                    );
                }
            }
            return;
        }
        match ctx.file {
            Some(file) => {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(icons::with_text(
                                icons::FILE,
                                &format::sanitize(file.file.name(), 120),
                            ))
                            .font(title.clone())
                            .color(p.text),
                        )
                        .truncate(),
                    );
                });
                ui.horizontal(|ui| match job.filter(|j| j.status.is_finished()) {
                    Some(last) => {
                        let (text, tone) = job_badge(last);
                        ui.add(Badge::new(&text).tone(tone));
                        let detail = match &last.status {
                            JobStatus::Failed(reason) => format::sanitize(reason, 80),
                            _ => format!(
                                "{} · {} after {}",
                                format::sanitize(&last.name, 40),
                                format::progress_percent(last.progress).to_string() + "%",
                                format::duration_of(last.elapsed)
                            ),
                        };
                        ui.add(
                            egui::Label::new(RichText::new(detail).color(p.text_dim).size(12.0))
                                .truncate(),
                        );
                    }
                    None => {
                        let text = if file.toolpath.is_none() {
                            "Not analyzed"
                        } else if ctx.state.halted && ctx.state.status.is_open() {
                            "Printer halted"
                        } else if ctx.state.is_connected() {
                            "Ready to print"
                        } else {
                            "Connect a printer to print"
                        };
                        ui.label(RichText::new(text).color(p.text_dim).size(12.0));
                    }
                });
            }
            None => {
                ui.label(
                    RichText::new("No file loaded")
                        .font(title.clone())
                        .color(p.text_dim),
                );
                ui.label(
                    RichText::new("Open a G-code file or drop one onto the window")
                        .color(p.text_faint)
                        .size(12.0),
                );
            }
        }
    }
}

/// A two-line statistic: dim caption over a value. Painted at a fixed
/// size, so it lays out the same in any parent layout.
fn stat(ui: &mut Ui, icon: &str, caption: &str, value: &str, p: &Palette) {
    let value_font = egui::FontId::new(14.0, theme::font(ui, TextRole::Title).family);
    let caption_galley = ui.painter().layout_no_wrap(
        icons::with_text(icon, caption),
        egui::FontId::proportional(11.0),
        p.text_dim,
    );
    let value_galley = ui
        .painter()
        .layout_no_wrap(value.to_owned(), value_font, p.text);
    let width = caption_galley.size().x.max(value_galley.size().x);
    let height = caption_galley.size().y + 1.0 + value_galley.size().y;
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), egui::Sense::hover());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("{caption}: {value}"))
    });
    let value_y = rect.top() + caption_galley.size().y + 1.0;
    ui.painter()
        .galley(rect.left_top(), caption_galley, p.text_dim);
    ui.painter()
        .galley(egui::pos2(rect.left(), value_y), value_galley, p.text);
}

/// Statistics of a running job (drawn right to left).
fn job_stats(ui: &mut Ui, job: &JobInfo, p: &Palette) {
    if job.kind == JobKind::HostPrint && job.filament_used_mm > 0.0 {
        stat(
            ui,
            icons::FILAMENT,
            "Filament",
            &format::length_mm(f64::from(job.filament_used_mm)),
            p,
        );
    }
    if job.total_lines > 0 {
        stat(
            ui,
            icons::CODE,
            "Line",
            &format!(
                "{} / {}",
                format::count(job.current_line as u64),
                format::count(job.total_lines as u64)
            ),
            p,
        );
    } else if job.bytes_total > 0 {
        stat(
            ui,
            icons::SD_CARD,
            "Read",
            &format!(
                "{} / {}",
                format::bytes(job.bytes_done),
                format::bytes(job.bytes_total)
            ),
            p,
        );
    }
    if let Some((index, total)) = job.layer
        && total > 0
    {
        stat(
            ui,
            icons::LAYERS,
            "Layer",
            &format!("{} / {}", (index + 1).min(total), total),
            p,
        );
    }
    let finish = match (&job.status, job.eta) {
        (JobStatus::Paused | JobStatus::Pausing, _) => "paused".to_owned(),
        (_, Some(eta)) => format::clock_after(eta),
        _ => format::UNKNOWN.to_owned(),
    };
    stat(ui, icons::CLOCK, "Finishes", &finish, p);
    let left = job
        .eta
        .map_or_else(|| format::UNKNOWN.to_owned(), format::duration_of);
    stat(ui, icons::TIMER, "Remaining", &left, p);
    stat(
        ui,
        icons::HOURGLASS,
        "Elapsed",
        &format::stopwatch(job.elapsed),
        p,
    );
}

/// Statistics of the loaded file (drawn right to left).
fn file_stats(ui: &mut Ui, ctx: &AppCtx<'_>, p: &Palette) {
    let Some(file) = ctx.file else {
        return;
    };
    stat(
        ui,
        icons::CODE,
        "Lines",
        &format::count(file.file.len() as u64),
        p,
    );
    if let Some(tp) = &file.toolpath {
        stat(
            ui,
            icons::LAYERS,
            "Layers",
            &format::count(tp.layers.len() as u64),
            p,
        );
        stat(
            ui,
            icons::FILAMENT,
            "Filament",
            &format::length_mm(f64::from(tp.stats.filament_length)),
            p,
        );
        stat(
            ui,
            icons::TIMER,
            "Estimate",
            &format::duration(f64::from(tp.stats.estimated_time)),
            p,
        );
    }
}

/// The status bar.
pub fn status_bar(ui: &mut Ui, ctx: &AppCtx<'_>) {
    let p = theme::ui_palette(ui);
    let state = ctx.state;
    let small = |text: String| RichText::new(text).size(12.0).color(p.text_dim);
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        let tone = match &state.status {
            s if state.halted && s.is_open() => Tone::Danger,
            sprint_host::ConnectionStatus::Connected => Tone::Success,
            sprint_host::ConnectionStatus::Connecting
            | sprint_host::ConnectionStatus::Handshaking => Tone::Warning,
            sprint_host::ConnectionStatus::Failed(_) => Tone::Danger,
            sprint_host::ConnectionStatus::Disconnected => Tone::Neutral,
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            status_dot(ui, tone, 7.0);
            let target = match (&state.target, state.status.is_open()) {
                (Some(t), true) => format::sanitize(t, 40),
                _ => "Not connected".to_owned(),
            };
            ui.label(small(target));
        });
        if state.status.is_connected() {
            let link = &state.link;
            ui.label(small(format!("{:.0} lines/s", link.lines_per_sec)))
                .on_hover_text("Commands acknowledged per second");
            ui.label(small(format!("RTT {:.1} ms", link.rtt_avg_ms)))
                .on_hover_text(format!(
                    "Round trip per command: last {:.1} ms, max {:.1} ms",
                    link.rtt_last_ms, link.rtt_max_ms
                ));
            let bad = link.resends + link.errors + link.timeouts;
            let text = format!(
                "{} resends · {} errors",
                format::count(link.resends),
                format::count(link.errors + link.timeouts)
            );
            let label = if bad > 0 {
                RichText::new(text).size(12.0).color(p.warning)
            } else {
                small(text)
            };
            ui.label(label).on_hover_text(format!(
                "Resends {} · checksum/line errors {} · timeouts {} · queued {} · in flight {}",
                link.resends, link.errors, link.timeouts, link.queue_len, link.in_flight
            ));
        }
        if let Some(busy) = &state.busy {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.add(egui::Spinner::new().size(12.0));
                ui.label(
                    RichText::new(format!("Busy: {}", format::sanitize(busy, 40)))
                        .size(12.0)
                        .color(p.warning),
                );
            });
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!("Sprint {}", env!("CARGO_PKG_VERSION")))
                    .size(12.0)
                    .color(p.text_faint),
            );
            if let Some(message) = state.firmware_messages.back() {
                let text = format::sanitize(message, 120);
                ui.add(egui::Label::new(small(icons::with_text(icons::BELL, &text))).truncate())
                    .on_hover_text(format::sanitize(message, 400));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_host::ConnectionStatus;

    #[test]
    fn print_blockers() {
        let mut state = PrinterState::default();
        assert_eq!(
            print_blocker(&state, false),
            Some("Open a G-code file first")
        );
        assert!(print_blocker(&state, true).is_some_and(|b| b.contains("Connect")));
        state.status = ConnectionStatus::Connected;
        assert_eq!(print_blocker(&state, true), None);
        state.halted = true;
        assert!(print_blocker(&state, true).is_some_and(|b| b.contains("halted")));
        state.halted = false;
        state.job = Some(JobInfo::new(JobKind::HostPrint, "a", 1, 1));
        assert!(print_blocker(&state, true).is_some_and(|b| b.contains("running")));
    }

    #[test]
    fn badges() {
        let mut job = JobInfo::new(JobKind::HostPrint, "a", 1, 1);
        job.status = JobStatus::Printing;
        assert_eq!(job_badge(&job), ("PRINTING".to_owned(), Tone::Accent));
        job.status = JobStatus::Paused;
        assert_eq!(job_badge(&job).1, Tone::Warning);
        job.status = JobStatus::Failed("x".into());
        assert_eq!(job_badge(&job).1, Tone::Danger);
        job.kind = JobKind::SdUpload;
        job.status = JobStatus::Printing;
        assert_eq!(job_badge(&job).0, "UPLOADING");
        job.kind = JobKind::SdPrint;
        assert_eq!(job_badge(&job).0, "SD PRINTING");
    }
}
