//! The 3D view's Objects card: the labelled objects of the file (the
//! slicer's cancel-object markers, [`Toolpath::objects`]) with their time
//! and filament, and, while the host prints the file, which object the
//! print is in, which are done or skipped, and Skip / Include buttons
//! ([`sprint_host::Printer::exclude_object`] and `include_object`).
//!
//! Hovering a row highlights the object in the viewer; a click selects its
//! first line (in the G-code too).

use egui::{Align, CornerRadius, Layout, RichText, Sense, Stroke, Ui, vec2};
use sprint_gcode::Toolpath;
use sprint_host::{JobInfo, JobKind, JobStatus};

use crate::theme::{self, Palette, TextRole, Tone};
use crate::widgets::{Badge, IconButton};
use crate::{format, icons};

/// Where an object is in the print of its file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectState {
    /// Not reached yet, or between its layers.
    Waiting,
    /// The stream is in it now ([`JobInfo::current_object`]).
    Printing,
    /// Its last line was sent.
    Done,
    /// Excluded: its moves are left out ([`JobInfo::excluded_objects`]).
    Skipped,
}

/// One row of the card.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectRow {
    /// Index into [`Toolpath::objects`].
    pub index: usize,
    /// The object's label.
    pub name: String,
    /// Estimated seconds.
    pub time: f32,
    /// Filament, mm.
    pub filament_mm: f32,
    /// The object's first line (0-based), if it has moves.
    pub first_line: Option<u32>,
    /// Where it is in the print (`None`: the file is not printing).
    pub state: Option<ObjectState>,
}

/// The rows for `toolpath`'s objects; `job` is the active host print of
/// this file, if it prints.
pub fn rows(toolpath: &Toolpath, job: Option<&JobInfo>) -> Vec<ObjectRow> {
    toolpath
        .objects
        .iter()
        .enumerate()
        .map(|(index, o)| {
            let state = job.map(|job| {
                if job.excluded_objects.contains(&o.name) {
                    ObjectState::Skipped
                } else if job.current_object.as_deref() == Some(o.name.as_str()) {
                    ObjectState::Printing
                } else if o
                    .lines
                    .last()
                    .is_some_and(|r| (r.end as usize) <= job.current_line)
                {
                    ObjectState::Done
                } else {
                    ObjectState::Waiting
                }
            });
            ObjectRow {
                index,
                name: o.name.clone(),
                time: o.time,
                filament_mm: o.extruded,
                first_line: o.lines.first().map(|r| r.start),
                state,
            }
        })
        .collect()
}

/// Why `name` cannot be skipped (or printed again) in `job` now, if it
/// cannot.
pub fn skip_blocker(job: &JobInfo, name: &str) -> Option<&'static str> {
    if job.kind != JobKind::HostPrint || !job.status.is_active() {
        Some("Only while Sprint prints the file")
    } else if job.status == JobStatus::Cancelling {
        Some("The print is being cancelled")
    } else if !job.objects.iter().any(|n| n == name) {
        Some("The print does not know this object (its markers differ)")
    } else {
        None
    }
}

/// The job bar's tooltip about a print's objects: the one it prints and
/// the skipped ones (`None` when the print has no objects to tell about).
pub fn job_objects_tooltip(job: &JobInfo) -> Option<String> {
    let mut lines = Vec::new();
    if let Some(current) = &job.current_object {
        lines.push(format!(
            "Printing object: {}",
            format::sanitize(current, 60)
        ));
    }
    if !job.excluded_objects.is_empty() {
        let names: Vec<String> = job
            .excluded_objects
            .iter()
            .map(|n| format::sanitize(n, 40))
            .collect();
        lines.push(format!(
            "Skipped object{}: {}",
            if names.len() == 1 { "" } else { "s" },
            names.join(", ")
        ));
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// What the user did in the card this frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardOutcome {
    /// The row under the pointer.
    pub hovered: Option<usize>,
    /// A row was clicked: select this line.
    pub select: Option<u32>,
    /// Skip (true) or print again (false) the named object.
    pub toggle: Option<(String, bool)>,
}

/// Draw the card (collapsible; `open` opens or collapses it once).
pub fn objects_card(
    ui: &mut Ui,
    toolpath: &Toolpath,
    job: Option<&JobInfo>,
    open: Option<bool>,
    p: &Palette,
) -> CardOutcome {
    let mut out = CardOutcome::default();
    let rows = rows(toolpath, job);
    super::viewer_tab::overlay_frame(p).show(ui, |ui| {
        ui.set_width(260.0);
        let id = ui.make_persistent_id("sprint-objects-open");
        let mut state =
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true);
        if let Some(open) = open {
            state.set_open(open);
        }
        let skipped = rows
            .iter()
            .filter(|r| r.state == Some(ObjectState::Skipped))
            .count();
        let header = ui.horizontal(|ui| {
            ui.label(RichText::new(icons::phosphor::SQUARES_FOUR).color(p.accent));
            ui.label(
                RichText::new(format!("Objects · {}", rows.len()))
                    .font(theme::font(ui, TextRole::Title))
                    .color(p.text),
            );
            if skipped > 0 {
                ui.add(Badge::new(&format!("{skipped} SKIPPED")).tone(Tone::Warning));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let icon = if state.is_open() {
                    icons::CARET_UP
                } else {
                    icons::CARET_DOWN
                };
                ui.add(
                    IconButton::new(icon)
                        .ghost()
                        .small()
                        .tooltip("Collapse / expand"),
                )
                .clicked()
            })
            .inner
        });
        if header.inner {
            state.toggle(ui);
        }
        state.show_body_unindented(ui, |ui| {
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .id_salt("sprint-objects-scroll")
                .max_height(ui.available_height().max(40.0))
                .show(ui, |ui| {
                    for row in &rows {
                        object_row(ui, row, job, p, &mut out);
                    }
                });
            if job.is_none() {
                ui.add_space(2.0);
                ui.label(
                    RichText::new("Objects can be skipped while Sprint prints the file.")
                        .size(11.5)
                        .color(p.text_faint),
                );
            }
        });
    });
    out
}

/// One object: state dot, name, time and filament, and Skip / Include
/// while the file prints.
fn object_row(
    ui: &mut Ui,
    row: &ObjectRow,
    job: Option<&JobInfo>,
    p: &Palette,
    out: &mut CardOutcome,
) {
    let skipped = row.state == Some(ObjectState::Skipped);
    let dim = skipped || row.state == Some(ObjectState::Done);
    let response = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let (dot, _) = ui.allocate_exact_size(vec2(10.0, 18.0), Sense::hover());
            let color = match row.state {
                Some(ObjectState::Printing) => p.accent,
                Some(ObjectState::Done) => p.success,
                Some(ObjectState::Skipped) => p.warning,
                _ => p.text_faint,
            };
            ui.painter().circle_filled(dot.center(), 3.5, color);
            // Buttons from the right, the name takes the rest.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(job) = job {
                    let blocker = skip_blocker(job, &row.name);
                    let (icon, tip) = if skipped {
                        (
                            icons::phosphor::ARROW_COUNTER_CLOCKWISE,
                            format!("Print “{}” again", format::sanitize(&row.name, 40)),
                        )
                    } else {
                        (
                            icons::phosphor::SKIP_FORWARD,
                            format!(
                                "Skip “{}” for the rest of the print",
                                format::sanitize(&row.name, 40)
                            ),
                        )
                    };
                    let done = row.state == Some(ObjectState::Done);
                    let button = ui
                        .add_enabled(
                            blocker.is_none() && !done,
                            IconButton::new(icon).ghost().small().tooltip(tip),
                        )
                        .on_disabled_hover_text(blocker.unwrap_or("Already printed"));
                    if button.clicked() {
                        out.toggle = Some((row.name.clone(), !skipped));
                    }
                }
                let detail = format!(
                    "{} · {}",
                    format::duration(f64::from(row.time)),
                    format::length_mm(f64::from(row.filament_mm))
                );
                ui.label(RichText::new(detail).size(11.5).color(p.text_dim));
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let mut name = RichText::new(format::sanitize(&row.name, 60))
                        .size(13.0)
                        .color(if dim { p.text_dim } else { p.text });
                    if skipped {
                        name = name.strikethrough();
                    }
                    let state = match row.state {
                        Some(ObjectState::Printing) => "Printing now. ",
                        Some(ObjectState::Done) => "Printed. ",
                        Some(ObjectState::Skipped) => "Skipped. ",
                        _ => "",
                    };
                    let label = ui
                        .add(egui::Label::new(name).truncate().sense(Sense::click()))
                        .on_hover_text(format!("{state}Click to select its first line"));
                    if label.clicked()
                        && let Some(line) = row.first_line
                    {
                        out.select = Some(line);
                    }
                });
            });
        })
        .response;
    // Hovering anywhere on the row highlights the object (its buttons
    // keep their own clicks).
    if ui.rect_contains_pointer(response.rect) {
        out.hovered = Some(row.index);
        ui.painter().rect_stroke(
            response.rect.expand(2.0),
            CornerRadius::same(theme::RADIUS_SM),
            Stroke::new(1.0, theme::with_alpha(p.accent, 0.6)),
            egui::StrokeKind::Outside,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_gcode::ObjectInfo;

    fn toolpath() -> Toolpath {
        let object = |name: &str, lines: Vec<std::ops::Range<u32>>, time: f32| ObjectInfo {
            name: name.into(),
            lines,
            time,
            extruded: 100.0,
            ..Default::default()
        };
        Toolpath {
            objects: vec![
                object("left", vec![10..20, 50..60], 30.0),
                object("right", vec![20..30, 60..70], 40.0),
                object("tag", std::iter::once(30..40).collect(), 5.0),
            ],
            ..Default::default()
        }
    }

    fn job(current_line: usize) -> JobInfo {
        let mut job = JobInfo::new(JobKind::HostPrint, "plate.gcode", 100, 1000);
        job.status = JobStatus::Printing;
        job.current_line = current_line;
        job.objects = vec!["left".into(), "right".into(), "tag".into()];
        job
    }

    #[test]
    fn rows_follow_the_print() {
        let tp = toolpath();
        let idle = rows(&tp, None);
        assert_eq!(idle.len(), 3);
        assert!(idle.iter().all(|r| r.state.is_none()));
        assert_eq!(idle[1].first_line, Some(20));
        let mut j = job(55);
        j.current_object = Some("left".into());
        j.excluded_objects = vec!["right".into()];
        let states: Vec<_> = rows(&tp, Some(&j)).iter().map(|r| r.state).collect();
        assert_eq!(
            states,
            [
                Some(ObjectState::Printing),
                Some(ObjectState::Skipped),
                Some(ObjectState::Done)
            ]
        );
        let states: Vec<_> = rows(&tp, Some(&job(5))).iter().map(|r| r.state).collect();
        assert!(states.iter().all(|s| *s == Some(ObjectState::Waiting)));
    }

    #[test]
    fn skipping_needs_a_running_host_print_that_knows_the_object() {
        let mut j = job(5);
        assert_eq!(skip_blocker(&j, "left"), None);
        assert!(skip_blocker(&j, "unknown").is_some());
        j.status = JobStatus::Paused;
        assert_eq!(skip_blocker(&j, "left"), None);
        j.status = JobStatus::Cancelling;
        assert!(skip_blocker(&j, "left").is_some());
        j.status = JobStatus::Completed;
        assert!(skip_blocker(&j, "left").is_some());
        j.status = JobStatus::Printing;
        j.kind = JobKind::SdPrint;
        assert!(skip_blocker(&j, "left").is_some());
    }

    #[test]
    fn the_job_bar_tells_the_current_and_skipped_objects() {
        let mut j = job(5);
        assert_eq!(job_objects_tooltip(&j), None);
        j.current_object = Some("left".into());
        assert_eq!(
            job_objects_tooltip(&j).as_deref(),
            Some("Printing object: left")
        );
        j.excluded_objects = vec!["right".into(), "tag".into()];
        assert_eq!(
            job_objects_tooltip(&j).as_deref(),
            Some("Printing object: left\nSkipped objects: right, tag")
        );
    }
}
