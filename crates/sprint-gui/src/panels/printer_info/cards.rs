//! The information cards of the Printer tab (thermal health lives in
//! `thermal.rs`).

use std::collections::VecDeque;

use egui::{Align, Layout, RichText, Ui};
use sprint_proto::capability_description;

use crate::context::{AppAction, AppCtx, Tab};
use crate::icons;
use crate::insight::{format, ui as kit};
use crate::theme::{self, TextRole, Tone};
use crate::widgets::{
    ActionButton, Badge, Card, Chip, IconButton, KeyValueGrid, ProgressRing, Sparkline, StatusPill,
};

/// A ghost refresh button for a card header.
fn refresh_button(ui: &mut Ui, tooltip: &str) -> bool {
    ui.add(
        IconButton::new(icons::REFRESH)
            .ghost()
            .small()
            .tooltip(tooltip),
    )
    .clicked()
}

/// A link for a source URL: only plain http(s) addresses (a scheme is
/// added when missing); `None` for anything else.
pub(super) fn source_link(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let lower = url.to_ascii_lowercase();
    let full = if lower.starts_with("https://") || lower.starts_with("http://") {
        url.to_owned()
    } else if lower.contains("://")
        || lower.starts_with("javascript:")
        || lower.starts_with("file:")
    {
        return None;
    } else {
        format!("https://{url}")
    };
    // A host is required: "https://" alone or "https:///x" are not links.
    let host = full.split("://").nth(1)?.split('/').next()?;
    (!host.is_empty() && host.contains('.')).then_some(full)
}

pub(super) fn firmware(ui: &mut Ui, ctx: &mut AppCtx<'_>) {
    let state = ctx.state;
    let printer = ctx.printer;
    let mut copied_uuid = None;
    Card::new("Firmware")
        .icon(icons::FIRMWARE)
        .show_with_header(
            ui,
            |ui| {
                if refresh_button(ui, "Ask the firmware again (M115)") {
                    printer.request_firmware_info();
                }
            },
            |ui| {
                let Some(fw) = state.firmware.as_ref() else {
                    kit::dim(ui, "The firmware has not identified itself yet (M115).");
                    return;
                };
                KeyValueGrid::new("firmware")
                    .key_width(84.0)
                    .show(ui, |kv| {
                        kv.row("Firmware", &fw.name);
                        kv.row("Version", fw.version.as_deref().unwrap_or(""));
                        kv.row_ui("Flavor", |ui| {
                            ui.add(Badge::new(fw.flavor.name()).tone(Tone::Accent));
                        });
                        kv.row("Machine", fw.machine_type.as_deref().unwrap_or(""));
                        kv.row(
                            "Extruders",
                            &fw.extruder_count.map(|n| n.to_string()).unwrap_or_default(),
                        );
                        kv.row("Protocol", fw.protocol_version.as_deref().unwrap_or(""));
                        if let Some(uuid) = fw.uuid.as_deref().filter(|u| !u.trim().is_empty()) {
                            kv.row_ui("UUID", |ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                // Leave room for the copy button.
                                let width = (ui.available_width() - 28.0).max(40.0);
                                ui.allocate_ui(egui::vec2(width, 20.0), |ui| {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(uuid).monospace().size(12.0),
                                        )
                                        .truncate(),
                                    )
                                    .on_hover_text(uuid);
                                });
                                if ui
                                    .add(
                                        IconButton::new(icons::COPY)
                                            .ghost()
                                            .small()
                                            .tooltip("Copy UUID"),
                                    )
                                    .clicked()
                                {
                                    ui.ctx().copy_text(uuid.to_owned());
                                    copied_uuid = Some(());
                                }
                            });
                        }
                        if let Some(url) = fw.source_url.as_deref().filter(|u| !u.trim().is_empty())
                        {
                            kv.row_ui("Source", |ui| match source_link(url) {
                                Some(link) => {
                                    ui.hyperlink_to(RichText::new(url).size(13.0), link)
                                        .on_hover_text("Open the firmware's source code");
                                }
                                None => {
                                    ui.label(RichText::new(url).size(13.0));
                                }
                            });
                        }
                        // Any other M115 fields, verbatim.
                        const SHOWN: [&str; 6] = [
                            "FIRMWARE_NAME",
                            "SOURCE_CODE_URL",
                            "PROTOCOL_VERSION",
                            "MACHINE_TYPE",
                            "EXTRUDER_COUNT",
                            "UUID",
                        ];
                        for (key, value) in &fw.fields {
                            if SHOWN.iter().any(|s| s.eq_ignore_ascii_case(key)) {
                                continue;
                            }
                            kv.row(&title_case(key), value);
                        }
                    });
            },
        );
    if copied_uuid.is_some() {
        ctx.toasts.success("UUID copied");
    }
}

/// "EXTRUDER_COUNT" -> "Extruder count".
fn title_case(key: &str) -> String {
    let lower = key.replace('_', " ").to_lowercase();
    let mut chars = lower.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The group a capability is shown in.
pub(super) fn capability_group(name: &str) -> &'static str {
    match name.to_ascii_uppercase().as_str() {
        "AUTOREPORT_TEMP"
        | "AUTOREPORT_POS"
        | "AUTOREPORT_POSITION"
        | "AUTOREPORT_FANS"
        | "AUTOREPORT_SD_STATUS"
        | "PROGRESS"
        | "BUILD_PERCENT"
        | "PRINT_JOB" => "Reporting",
        "EMERGENCY_PARSER"
        | "HOST_ACTION_COMMANDS"
        | "PROMPT_SUPPORT"
        | "SERIAL_XON_XOFF"
        | "BINARY_FILE_TRANSFER"
        | "ADVANCED_OK"
        | "MEATPACK"
        | "CONFIG_EXPORT"
        | "PAREN_COMMENTS"
        | "GCODE_QUOTED_STRINGS"
        | "HOST_RESCUE" => "Host protocol",
        "SDCARD"
        | "SD_WRITE"
        | "LONG_FILENAME"
        | "LFN_WRITE"
        | "MULTI_VOLUME"
        | "EXTENDED_M20"
        | "CUSTOM_FIRMWARE_UPLOAD"
        | "REPEAT"
        | "EEPROM" => "Storage",
        "AUTOLEVEL" | "Z_PROBE" | "LEVELING_DATA" | "ARCS" | "BABYSTEPPING" | "MOTION_MODES"
        | "INPUT_SHAPING" | "LINEAR_ADVANCE" | "VOLUMETRIC" => "Motion & leveling",
        "THERMAL_PROTECTION"
        | "CHAMBER_TEMPERATURE"
        | "COOLER_TEMPERATURE"
        | "SOFTWARE_POWER"
        | "TOGGLE_LIGHTS"
        | "CASE_LIGHT_BRIGHTNESS"
        | "RUNOUT"
        | "FAN_COUNT"
        | "SINGLENOZZLE"
        | "SPINDLE"
        | "LASER"
        | "MMU"
        | "PRUSA_MMU2"
        | "POWER_LOSS_RECOVERY" => "Hardware",
        _ => "Other",
    }
}

const CAPABILITY_GROUPS: [&str; 6] = [
    "Reporting",
    "Host protocol",
    "Storage",
    "Motion & leveling",
    "Hardware",
    "Other",
];

pub(super) fn capabilities(ui: &mut Ui, ctx: &mut AppCtx<'_>, show_disabled: &mut bool) {
    let caps = &ctx.state.capabilities;
    let enabled = caps.iter().filter(|(_, on)| *on).count();
    let subtitle = if caps.is_empty() {
        String::new()
    } else {
        format!("{enabled} of {} supported", caps.len())
    };
    let show = *show_disabled;
    Card::new("Capabilities")
        .icon(icons::CHECK)
        .subtitle(&subtitle)
        .show_with_header(
            ui,
            |ui| {
                if !caps.is_empty()
                    && ui
                        .add(Chip::new("Unsupported").selected(*show_disabled))
                        .on_hover_text("Also list what the firmware lacks")
                        .clicked()
                {
                    *show_disabled = !*show_disabled;
                }
            },
            |ui| {
                if caps.is_empty() {
                    kit::dim(
                        ui,
                        "The firmware reported no capabilities (M115 \u{201c}Cap:\u{201d} lines).",
                    );
                    return;
                }
                let mut first = true;
                for group in CAPABILITY_GROUPS {
                    let items: Vec<&(String, bool)> = caps
                        .iter()
                        .filter(|(name, on)| capability_group(name) == group && (*on || show))
                        .collect();
                    if items.is_empty() {
                        continue;
                    }
                    if !first {
                        ui.add_space(6.0);
                    }
                    first = false;
                    kit::caption(ui, group);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                        for (name, on) in items {
                            let tone = if *on { Tone::Success } else { Tone::Neutral };
                            let r = ui.add(Badge::new(name).tone(tone));
                            let state = if *on { "Supported" } else { "Not supported" };
                            let text = match capability_description(name) {
                                Some(desc) => format!("{desc}\n{state}"),
                                None => state.to_owned(),
                            };
                            r.on_hover_text(text);
                        }
                    });
                }
                let missing = caps.len() - enabled;
                if !show && missing > 0 {
                    ui.add_space(4.0);
                    kit::caption(
                        ui,
                        &format!(
                            "{missing} unsupported capabilit{} hidden",
                            if missing == 1 { "y" } else { "ies" }
                        ),
                    );
                }
            },
        );
}

pub(super) fn statistics(ui: &mut Ui, ctx: &mut AppCtx<'_>) {
    let printer = ctx.printer;
    let stats = ctx.state.stats.as_ref();
    Card::new("Statistics")
        .icon(icons::STATS)
        .subtitle("lifetime (M78)")
        .show_with_header(
            ui,
            |ui| {
                if refresh_button(ui, "Load the print statistics (M78)") {
                    printer.request_stats();
                }
            },
            |ui| {
                let Some(stats) = stats else {
                    kit::dim(
                        ui,
                        "No statistics loaded. Firmware built with PRINTCOUNTER reports prints, print time and filament used.",
                    );
                    ui.add_space(4.0);
                    if ui
                        .add(ActionButton::new("Load statistics").icon(icons::STATS).small())
                        .clicked()
                    {
                        printer.request_stats();
                    }
                    return;
                };
                let finished = stats.finished.unwrap_or(0);
                let failed = stats.failed.unwrap_or(0);
                let ended = finished + failed;
                let rate = (ended > 0).then(|| finished as f32 / ended as f32);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 16.0;
                    let ring = match rate {
                        Some(r) => ProgressRing::new(r)
                            .size(76.0)
                            .thickness(7.0)
                            .tone(if r >= 0.9 {
                                Tone::Success
                            } else if r >= 0.7 {
                                Tone::Warning
                            } else {
                                Tone::Danger
                            })
                            .sublabel("success"),
                        None => ProgressRing::new(0.0)
                            .size(76.0)
                            .thickness(7.0)
                            .text("—")
                            .sublabel("success"),
                    };
                    ui.add(ring);
                    ui.vertical(|ui| {
                        let count = |v: Option<u32>| {
                            v.map_or_else(|| format::MISSING.to_owned(), |n| format::count(u64::from(n)))
                        };
                        let p = theme::ui_palette(ui);
                        kit::metric_grid(
                            ui,
                            3,
                            &[
                                ("Prints", count(stats.prints), None),
                                ("Finished", count(stats.finished), Some(p.success)),
                                (
                                    "Failed",
                                    count(stats.failed),
                                    (failed > 0).then_some(p.danger),
                                ),
                            ],
                        );
                    });
                });
                ui.add_space(6.0);
                let secs = |v: Option<u64>| {
                    v.map_or_else(|| format::MISSING.to_owned(), |s| format::duration(s as f64))
                };
                let average = match (stats.total_time, ended) {
                    (Some(t), n) if n > 0 => format::duration(t as f64 / f64::from(n)),
                    _ => format::MISSING.to_owned(),
                };
                kit::metric_grid(
                    ui,
                    2,
                    &[
                        ("Total print time", secs(stats.total_time), None),
                        ("Longest job", secs(stats.longest_job), None),
                        (
                            "Filament used",
                            stats
                                .filament_used_mm
                                .map_or_else(|| format::MISSING.to_owned(), format::length_mm),
                            None,
                        ),
                        ("Average job", average, None),
                    ],
                );
            },
        );
}

/// "x_min" -> "X min", "z_probe" -> "Z probe", "filament" -> "Filament".
pub(super) fn endstop_label(name: &str) -> String {
    let words: Vec<String> = name
        .split(['_', ' '])
        .filter(|w| !w.is_empty())
        .enumerate()
        .map(|(i, w)| {
            // Axis names ("x", "z2") are upper-cased.
            let mut chars = w.chars();
            let axis = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
                && chars.all(|c| c.is_ascii_digit());
            if axis {
                w.to_uppercase()
            } else if i == 0 {
                let mut c = w.chars();
                c.next()
                    .map(|f| f.to_uppercase().chain(c).collect())
                    .unwrap_or_default()
            } else {
                w.to_lowercase()
            }
        })
        .collect();
    if words.is_empty() {
        name.to_owned()
    } else {
        words.join(" ")
    }
}

pub(super) fn endstops(ui: &mut Ui, ctx: &mut AppCtx<'_>, live: &mut bool) {
    let printer = ctx.printer;
    let endstops = &ctx.state.endstops;
    let triggered = endstops.iter().filter(|(_, t)| *t).count();
    let subtitle = if endstops.is_empty() {
        String::new()
    } else if triggered == 0 {
        "all open".to_owned()
    } else {
        format!("{triggered} triggered")
    };
    Card::new("Endstops")
        .icon(icons::ENDSTOP)
        .subtitle(&subtitle)
        .show_with_header(
            ui,
            |ui| {
                if refresh_button(ui, "Query the endstops (M119)") {
                    printer.request_endstops();
                }
                if ui
                    .add(Chip::new("Live").selected(*live))
                    .on_hover_text("Query the endstops every second while this tab is open")
                    .clicked()
                {
                    *live = !*live;
                }
            },
            |ui| {
                if endstops.is_empty() {
                    kit::dim(
                        ui,
                        "Not queried yet. Refresh to read the switch states (M119).",
                    );
                    return;
                }
                let p = theme::ui_palette(ui);
                for (name, triggered) in endstops {
                    ui.horizontal(|ui| {
                        let color = if *triggered { p.warning } else { p.success };
                        kit::dot_label(ui, color, &endstop_label(name), false);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if *triggered {
                                ui.add(Badge::new("TRIGGERED").tone(Tone::Warning).filled(true));
                            } else {
                                ui.add(Badge::new("open").tone(Tone::Success));
                            }
                        });
                    });
                }
            },
        );
}

pub(super) fn position(ui: &mut Ui, ctx: &mut AppCtx<'_>) {
    let printer = ctx.printer;
    let pos = ctx.state.position.as_ref();
    Card::new("Position")
        .icon(icons::POSITION)
        .show_with_header(
            ui,
            |ui| {
                if refresh_button(ui, "Report the position (M114)") {
                    printer.request_position();
                }
            },
            |ui| {
                let Some(pos) = pos else {
                    kit::dim(ui, "No position reported yet (M114).");
                    return;
                };
                let p = theme::ui_palette(ui);
                let width = ui.available_width();
                let cell = ((width - 3.0 * 8.0) / 4.0).max(40.0);
                ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for (axis, value, color) in [
                        ("X", pos.x, p.plot_color(1)),
                        ("Y", pos.y, p.plot_color(2)),
                        ("Z", pos.z, p.plot_color(0)),
                        ("E", pos.e, p.plot_color(3)),
                    ] {
                        axis_readout(ui, axis, value, color, cell);
                    }
                });
                if let Some(counts) = pos.stepper_counts.as_ref().filter(|c| !c.is_empty()) {
                    ui.add_space(6.0);
                    let letters = ["X", "Y", "Z", "E", "A", "B", "C"];
                    let text = counts
                        .iter()
                        .enumerate()
                        .map(|(i, c)| format!("{} {c}", letters.get(i).copied().unwrap_or("?")))
                        .collect::<Vec<_>>()
                        .join("   ");
                    ui.horizontal(|ui| {
                        kit::caption(ui, "Steppers");
                        ui.label(RichText::new(text).monospace().size(12.0).color(p.text_dim));
                    });
                }
            },
        );
}

/// One axis: a colored letter and the value in the readout font.
fn axis_readout(ui: &mut Ui, axis: &str, value: f32, color: egui::Color32, width: f32) {
    let p = theme::ui_palette(ui);
    let text = format::fixed(f64::from(value), 2);
    ui.allocate_ui_with_layout(egui::vec2(width, 0.0), Layout::top_down(Align::Min), |ui| {
        ui.set_width(width);
        let frame = egui::Frame::new()
            .fill(p.raised)
            .corner_radius(egui::CornerRadius::same(theme::RADIUS))
            .inner_margin(egui::Margin::symmetric(8, 6));
        frame.show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(
                RichText::new(axis)
                    .font(theme::font(ui, TextRole::Section))
                    .color(color),
            );
            // Shrink long values (E after a long print) rather than cut them.
            let family = theme::font(ui, TextRole::Readout).family;
            let fits = |size: f32| {
                ui.painter()
                    .layout_no_wrap(
                        text.clone(),
                        egui::FontId::new(size, family.clone()),
                        p.text,
                    )
                    .size()
                    .x
                    <= ui.available_width()
            };
            let size = [18.0, 15.0, 13.0]
                .into_iter()
                .find(|s| fits(*s))
                .unwrap_or(12.0);
            ui.add(
                egui::Label::new(
                    RichText::new(&text)
                        .font(egui::FontId::new(size, family.clone()))
                        .color(p.text),
                )
                .truncate(),
            );
        });
    });
}

/// Link quality from resend rate and timeouts: label and tone.
pub(super) fn link_quality(link: &sprint_host::LinkStats) -> (&'static str, Tone) {
    let sent = link.lines_sent.max(1) as f64;
    let resend_rate = link.resends as f64 / sent;
    if link.lines_sent == 0 {
        ("Idle", Tone::Neutral)
    } else if resend_rate < 0.001 && link.timeouts == 0 && link.errors == 0 {
        ("Healthy", Tone::Success)
    } else if resend_rate < 0.01 && link.timeouts <= 2 {
        ("Minor retries", Tone::Warning)
    } else {
        ("Unreliable", Tone::Danger)
    }
}

pub(super) fn link(ui: &mut Ui, ctx: &mut AppCtx<'_>, rtt_history: &VecDeque<f32>) {
    let link = &ctx.state.link;
    let (quality, tone) = link_quality(link);
    Card::new("Link")
        .icon(icons::LINK)
        .subtitle(ctx.state.target.as_deref().unwrap_or(""))
        .show_with_header(
            ui,
            |ui| {
                ui.add(StatusPill::new(quality, tone));
            },
            |ui| {
                let p = theme::ui_palette(ui);
                let history: Vec<f32> = rtt_history.iter().copied().collect();
                ui.horizontal(|ui| {
                    kit::caption(ui, "Round trip");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        kit::caption(
                            ui,
                            &format!(
                                "last {} · avg {} · max {} ms",
                                format::fixed(f64::from(link.rtt_last_ms), 1),
                                format::fixed(f64::from(link.rtt_avg_ms), 1),
                                format::fixed(f64::from(link.rtt_max_ms), 1)
                            ),
                        );
                    });
                });
                if history.len() >= 2 {
                    ui.add(
                        Sparkline::new(&history)
                            .height(38.0)
                            .color(p.info)
                            .min_span(1.0)
                            .reference(Some(link.rtt_avg_ms).filter(|v| v.is_finite() && *v > 0.0)),
                    );
                } else {
                    kit::caption(ui, "Collecting round-trip samples…");
                }
                ui.add_space(6.0);
                let resend_rate = if link.lines_sent > 0 {
                    format!(
                        "{} ({})",
                        format::count(link.resends),
                        format::percent(link.resends as f64 / link.lines_sent as f64, 2)
                    )
                } else {
                    format::count(link.resends)
                };
                let warn = |bad: bool| bad.then_some(p.warning);
                kit::metric_grid(
                    ui,
                    3,
                    &[
                        ("Connected", format::duration(link.connected_secs), None),
                        (
                            "Throughput",
                            format::rate(f64::from(link.lines_per_sec), "lines/s"),
                            None,
                        ),
                        (
                            "Queue",
                            format!("{} · {} in flight", link.queue_len, link.in_flight),
                            None,
                        ),
                        ("Lines sent", format::count(link.lines_sent), None),
                        ("OKs", format::count(link.oks), None),
                        ("Resends", resend_rate, warn(link.resends > 0)),
                        ("Sent", format::bytes(link.bytes_sent), None),
                        ("Received", format::bytes(link.bytes_received), None),
                        (
                            "Errors · timeouts",
                            format!(
                                "{} · {}",
                                format::count(link.errors),
                                format::count(link.timeouts)
                            ),
                            warn(link.errors > 0 || link.timeouts > 0),
                        ),
                    ],
                );
            },
        );
}

/// "X 80 · Y 80 · Z 400 · E 93" with an optional unit.
fn axes_text(values: Option<[f32; 4]>, decimals: usize, unit: &str) -> String {
    let Some(v) = values else {
        return String::new();
    };
    let text = ["X", "Y", "Z", "E"]
        .iter()
        .zip(v)
        .map(|(a, v)| format!("{a} {}", format::trimmed(f64::from(v), decimals)))
        .collect::<Vec<_>>()
        .join(" · ");
    if unit.is_empty() {
        text
    } else {
        format!("{text} {unit}")
    }
}

fn pid_text(pid: Option<(f32, f32, f32)>) -> String {
    pid.map_or_else(String::new, |(p, i, d)| {
        format!(
            "Kp {} · Ki {} · Kd {}",
            format::fixed(f64::from(p), 2),
            format::fixed(f64::from(i), 2),
            format::fixed(f64::from(d), 2)
        )
    })
}

pub(super) fn settings_summary(ui: &mut Ui, ctx: &mut AppCtx<'_>) {
    let printer = ctx.printer;
    let state = ctx.state;
    let mut open_tuning = false;
    Card::new("Settings")
        .icon(icons::TUNING)
        .subtitle("active values (M503)")
        .show_with_header(
            ui,
            |ui| {
                if ui
                    .add(
                        IconButton::new(icons::EDIT)
                            .ghost()
                            .small()
                            .tooltip("Edit in the Tuning tab"),
                    )
                    .clicked()
                {
                    open_tuning = true;
                }
                if refresh_button(ui, "Reload the settings (M503)") {
                    printer.request_settings();
                }
            },
            |ui| {
                let s = &state.settings;
                if s.is_empty() {
                    let text = if state
                        .firmware
                        .as_ref()
                        .is_some_and(|f| !f.flavor.supports_m503())
                    {
                        "This firmware does not report its settings with M503."
                    } else if state.settings_loaded {
                        "The firmware reported no settings."
                    } else {
                        "Settings not loaded yet (M503)."
                    };
                    kit::dim(ui, text);
                    return;
                }
                let motion = s.motion_limits();
                KeyValueGrid::new("settings-summary")
                    .key_width(104.0)
                    .copyable(false)
                    .show(ui, |kv| {
                        kv.row("Steps/mm", &axes_text(s.steps_per_mm(), 2, ""));
                        kv.row("Max feedrate", &axes_text(s.max_feedrate(), 1, "mm/s"));
                        kv.row("Max accel", &axes_text(s.max_accel(), 0, "mm/s²"));
                        let accel = [
                            ("Print", motion.print_accel),
                            ("Retract", motion.retract_accel),
                            ("Travel", motion.travel_accel),
                        ]
                        .iter()
                        .filter_map(|(n, v)| {
                            v.map(|v| format!("{n} {}", format::trimmed(f64::from(v), 0)))
                        })
                        .collect::<Vec<_>>()
                        .join(" · ");
                        if !accel.is_empty() {
                            kv.row("Acceleration", &format!("{accel} mm/s²"));
                        }
                        match (motion.junction_deviation, motion.jerk) {
                            (Some(j), _) => {
                                kv.row(
                                    "Junction dev.",
                                    &format!("{} mm", format::trimmed(f64::from(j), 3)),
                                );
                            }
                            (None, Some(jerk)) => {
                                kv.row("Jerk", &axes_text(Some(jerk), 2, "mm/s"));
                            }
                            (None, None) => {}
                        }
                        let tools = state
                            .firmware
                            .as_ref()
                            .and_then(|f| f.extruder_count)
                            .unwrap_or(1)
                            .clamp(1, 8);
                        for tool in 0..tools {
                            let pid = s.hotend_pid(tool);
                            if pid.is_none() {
                                continue;
                            }
                            let key = if tools > 1 {
                                format!("Hotend {tool} PID")
                            } else {
                                "Hotend PID".to_owned()
                            };
                            kv.row(&key, &pid_text(pid));
                        }
                        if let Some(pid) = s.bed_pid() {
                            kv.row("Bed PID", &pid_text(Some(pid)));
                        }
                        if let Some([x, y, z]) = s.probe_offset() {
                            kv.row(
                                "Probe offset",
                                &format!(
                                    "X {} · Y {} · Z {} mm",
                                    format::trimmed(f64::from(x), 2),
                                    format::trimmed(f64::from(y), 2),
                                    format::fixed(f64::from(z), 2)
                                ),
                            );
                        }
                        if let Some(k) = s.linear_advance() {
                            kv.row(
                                "Linear advance",
                                &format!("K {}", format::trimmed(f64::from(k), 3)),
                            );
                        }
                    });
            },
        );
    if open_tuning {
        ctx.actions.push(AppAction::ShowTab(Tab::Tuning));
    }
}

pub(super) fn messages(ui: &mut Ui, ctx: &mut AppCtx<'_>) {
    let messages = &ctx.state.firmware_messages;
    let subtitle = if messages.is_empty() {
        String::new()
    } else {
        format!("{} recent", messages.len())
    };
    Card::new("Firmware messages")
        .icon(icons::BELL)
        .subtitle(&subtitle)
        .show(ui, |ui| {
            if messages.is_empty() {
                kit::dim(ui, "No messages from the firmware yet.");
                return;
            }
            let p = theme::ui_palette(ui);
            egui::ScrollArea::vertical()
                .id_salt("firmware-messages")
                .max_height(220.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    for message in messages.iter().rev().take(50) {
                        let text: String = message.chars().filter(|c| !c.is_control()).collect();
                        ui.add(
                            egui::Label::new(
                                RichText::new(text).monospace().size(12.0).color(p.text),
                            )
                            .wrap(),
                        );
                    }
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_proto::known_capabilities;

    #[test]
    fn source_links_are_sanitized() {
        assert_eq!(
            source_link("github.com/MarlinFirmware/Marlin").as_deref(),
            Some("https://github.com/MarlinFirmware/Marlin")
        );
        assert_eq!(
            source_link("http://example.org/x").as_deref(),
            Some("http://example.org/x")
        );
        assert_eq!(source_link("javascript:alert(1)"), None);
        assert_eq!(source_link("file:///etc/passwd"), None);
        assert_eq!(source_link("ftp://host.org/x"), None);
        assert_eq!(source_link("not a url"), None);
        assert_eq!(source_link("localhost"), None);
        assert_eq!(source_link(""), None);
        assert_eq!(source_link("https://"), None);
    }

    #[test]
    fn capability_groups() {
        assert_eq!(capability_group("AUTOREPORT_TEMP"), "Reporting");
        assert_eq!(capability_group("eeprom"), "Storage");
        assert_eq!(capability_group("EMERGENCY_PARSER"), "Host protocol");
        assert_eq!(capability_group("Z_PROBE"), "Motion & leveling");
        assert_eq!(capability_group("THERMAL_PROTECTION"), "Hardware");
        assert_eq!(capability_group("SOMETHING_NEW"), "Other");
        // Every known capability has a real group.
        for (name, _) in known_capabilities() {
            assert_ne!(capability_group(name), "Other", "{name}");
        }
    }

    #[test]
    fn endstop_labels() {
        assert_eq!(endstop_label("x_min"), "X min");
        assert_eq!(endstop_label("z_probe"), "Z probe");
        assert_eq!(endstop_label("filament"), "Filament");
        assert_eq!(endstop_label("z2_max"), "Z2 max");
        assert_eq!(endstop_label(""), "");
        assert_eq!(endstop_label("___"), "___");
    }

    #[test]
    fn link_quality_levels() {
        let mut link = sprint_host::LinkStats::default();
        assert_eq!(link_quality(&link).0, "Idle");
        link.lines_sent = 10_000;
        assert_eq!(link_quality(&link).1, Tone::Success);
        link.resends = 50;
        assert_eq!(link_quality(&link).1, Tone::Warning);
        link.resends = 500;
        assert_eq!(link_quality(&link).1, Tone::Danger);
    }

    #[test]
    fn titles_and_axes() {
        assert_eq!(title_case("KINEMATICS"), "Kinematics");
        assert_eq!(title_case("AREA_X"), "Area x");
        assert_eq!(title_case(""), "");
        assert_eq!(
            axes_text(Some([80.0, 80.0, 400.0, 93.0]), 2, ""),
            "X 80 · Y 80 · Z 400 · E 93"
        );
        assert_eq!(axes_text(None, 2, "mm/s"), "");
    }
}
