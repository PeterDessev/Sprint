//! Extruder: tool selector, extrude / retract, a cold-extrusion guard with
//! a "heat to preset" shortcut, and filament load / unload.

use crossbeam_channel::Receiver;
use egui::Ui;
use sprint_host::sprint_proto::FirmwareFlavor;
use sprint_host::{Event, Heater, Printer, PrinterState, TempPreset};

use super::temperatures::{MAX_HOTENDS, current_temp, hotend_count, target_temp};
use super::{caption, notice, section};
use crate::context::AppCtx;
use crate::icons;
use crate::theme::Tone;
use crate::widgets::{
    ActionButton, ExtruderAction, ExtruderControl, SegmentedControl, format_temperature,
    section_header,
};

/// Below this the firmware refuses to extrude (Marlin's EXTRUDE_MINTEMP).
pub(super) const MIN_EXTRUDE_TEMP: f32 = 170.0;
/// Longest single extrusion offered (Marlin's EXTRUDE_MAXLENGTH default).
const MAX_EXTRUDE_LENGTH: f32 = 200.0;
/// Hotend temperature for filament changes without a usable preset.
const FALLBACK_HEAT: f32 = 200.0;
/// Scripted load: feed this far quickly, then purge slowly.
const LOAD_FEED_MM: f32 = 60.0;
const LOAD_FEED_MM_MIN: f32 = 480.0;
const LOAD_PURGE_MM: f32 = 30.0;
const LOAD_PURGE_MM_MIN: f32 = 150.0;
/// Scripted unload: push a little to soften the tip, then pull out fast.
const UNLOAD_TIP_MM: f32 = 5.0;
const UNLOAD_TIP_MM_MIN: f32 = 300.0;
const UNLOAD_MM: f32 = 80.0;
const UNLOAD_MM_MIN: f32 = 1800.0;
/// How long a native load/unload is watched for a rejection (s): M109 may
/// heat for a while before the firmware even reads M701.
const NATIVE_WATCH_SECS: f64 = 600.0;

/// UI state of the extruder section.
#[derive(Debug)]
pub(super) struct Extruder {
    /// Extrusion length (mm).
    length: f32,
    /// Extrusion speed (mm/s).
    speed: f32,
    /// `speed` was initialised from the profile.
    seeded: bool,
    /// Tool picked in the selector (`None` = the active tool).
    tool: Option<u8>,
    /// The firmware answered M701/M702 with "Unknown command" on this
    /// connection: use the scripted sequences.
    native_rejected: bool,
    /// A native load/unload in flight, watched for a rejection.
    watch: Option<NativeWatch>,
    was_connected: bool,
}

impl Default for Extruder {
    fn default() -> Self {
        Extruder {
            length: 10.0,
            speed: 5.0,
            seeded: false,
            tool: None,
            native_rejected: false,
            watch: None,
            was_connected: false,
        }
    }
}

/// A native filament change waiting to see whether the firmware knows it.
#[derive(Debug)]
struct NativeWatch {
    op: FilamentOp,
    tool: u8,
    events: Receiver<Event>,
    /// egui time when it was sent.
    started: f64,
}

/// Whether a firmware message rejects the native command of `op`
/// (`Unknown command: "M701"`).
pub(super) fn rejects_native(message: &str, op: FilamentOp) -> bool {
    let message = message.to_ascii_uppercase();
    let code = match op {
        FilamentOp::Load => "M701",
        FilamentOp::Unload => "M702",
    };
    message.contains("UNKNOWN COMMAND") && message.contains(code)
}

/// Loading or unloading filament.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FilamentOp {
    /// Feed filament into the hotend.
    Load,
    /// Pull filament out.
    Unload,
}

/// One step of a filament change.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum FilamentStep {
    /// Raw G-code.
    Gcode(String),
    /// `Printer::extrude` (negative = retract), feedrate in mm/min.
    Extrude { length: f32, feedrate: f32 },
}

/// Whether the firmware has M701/M702: Prusa firmware always does; Marlin
/// does with FILAMENT_LOAD_UNLOAD_GCODES, which also makes M503 report the
/// load/unload lengths (M603); some builds advertise a capability.
pub(super) fn native_load_unload(state: &PrinterState) -> bool {
    let flavor = state.firmware.as_ref().map(|f| f.flavor);
    flavor == Some(FirmwareFlavor::Prusa)
        || state.has_capability("FILAMENT_LOAD_UNLOAD")
        || state.has_capability("ADVANCED_PAUSE")
        || (flavor == Some(FirmwareFlavor::Marlin)
            && state.settings.iter().any(|s| s.code == "M603"))
}

/// The temperature a filament change heats to: the current target when it
/// is hot enough, else the first preset that is, else 200 °C.
pub(super) fn heat_temperature(target: f32, presets: &[TempPreset]) -> f32 {
    if target.is_finite() && target >= MIN_EXTRUDE_TEMP {
        return target;
    }
    presets
        .iter()
        .map(|p| p.hotend)
        .find(|t| t.is_finite() && *t >= MIN_EXTRUDE_TEMP)
        .unwrap_or(FALLBACK_HEAT)
}

/// The preset used by "heat to …" (name, °C), if any is hot enough.
pub(super) fn heat_preset(presets: &[TempPreset]) -> Option<(&str, f32)> {
    presets
        .iter()
        .find(|p| p.hotend.is_finite() && p.hotend >= MIN_EXTRUDE_TEMP)
        .map(|p| (p.name.as_str(), p.hotend))
}

/// The steps of a filament change: heat and wait, then either the
/// firmware's M701/M702 or a scripted feed / pull.
pub(super) fn filament_plan(
    op: FilamentOp,
    tool: u8,
    multi_tool: bool,
    native: bool,
    heat_to: f32,
) -> Vec<FilamentStep> {
    let t = if multi_tool {
        format!(" T{tool}")
    } else {
        String::new()
    };
    let heat = if heat_to.is_finite() {
        heat_to.clamp(MIN_EXTRUDE_TEMP, 400.0)
    } else {
        FALLBACK_HEAT
    };
    let mut steps = vec![FilamentStep::Gcode(format!(
        "M109{t} S{}",
        sprint_host::format_number(f64::from(heat), 1)
    ))];
    match (op, native) {
        (FilamentOp::Load, true) => steps.push(FilamentStep::Gcode(format!("M701{t}"))),
        (FilamentOp::Unload, true) => steps.push(FilamentStep::Gcode(format!("M702{t}"))),
        (FilamentOp::Load, false) => steps.extend([
            FilamentStep::Extrude {
                length: LOAD_FEED_MM,
                feedrate: LOAD_FEED_MM_MIN,
            },
            FilamentStep::Extrude {
                length: LOAD_PURGE_MM,
                feedrate: LOAD_PURGE_MM_MIN,
            },
        ]),
        (FilamentOp::Unload, false) => steps.extend([
            FilamentStep::Extrude {
                length: UNLOAD_TIP_MM,
                feedrate: UNLOAD_TIP_MM_MIN,
            },
            FilamentStep::Extrude {
                length: -UNLOAD_MM,
                feedrate: UNLOAD_MM_MIN,
            },
        ]),
    }
    steps
}

/// Send a filament change, in order.
fn run_plan(printer: &Printer, tool: u8, steps: &[FilamentStep]) {
    for step in steps {
        match step {
            FilamentStep::Gcode(gcode) => printer.send(gcode),
            FilamentStep::Extrude { length, feedrate } => printer.extrude(tool, *length, *feedrate),
        }
    }
}

/// Why extrusion is blocked by the hotend temperature, if it is.
pub(super) fn cold_reason(current: f32) -> Option<String> {
    if !current.is_finite() {
        Some("No hotend temperature reading yet.".to_owned())
    } else if current < MIN_EXTRUDE_TEMP {
        Some(format!(
            "Hotend at {} °C: extruding needs {} °C.",
            format_temperature(current).trim_end_matches(".0"),
            MIN_EXTRUDE_TEMP
        ))
    } else {
        None
    }
}

impl Extruder {
    /// Draw the section.
    pub(super) fn show(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let printer = ctx.printer;
        let state = ctx.state;
        let connected = ctx.connected();
        let job_active = ctx.job_active();
        let lock = super::motion_lock(ctx);
        let profile = ctx.config.active();
        if !self.seeded {
            let feed = profile.extrude_feedrate;
            if feed.is_finite() && feed > 0.0 {
                self.speed = (feed / 60.0).clamp(0.1, 100.0);
            }
            self.seeded = true;
        }
        let hotends = hotend_count(state, profile).min(MAX_HOTENDS);
        let last_tool = u8::try_from(hotends - 1).unwrap_or(0);
        let tool = self.tool.unwrap_or(state.active_tool).min(last_tool);
        let heater = Heater::Hotend(tool);
        let current = current_temp(state, heater);
        let target = target_temp(state, heater);
        let cold = if connected {
            cold_reason(current)
        } else {
            None
        };
        if self.was_connected && !connected {
            // Another connection may be another printer.
            self.native_rejected = false;
            self.watch = None;
        }
        self.was_connected = connected;
        let now = ui.input(|i| i.time);
        if let Some(watch) = &self.watch {
            let rejected = watch
                .events
                .try_iter()
                .any(|e| matches!(e, Event::FirmwareMessage(ref m) if rejects_native(m, watch.op)));
            if rejected {
                let (op, tool) = (watch.op, watch.tool);
                self.native_rejected = true;
                self.watch = None;
                let title = match op {
                    FilamentOp::Load => "The printer has no M701",
                    FilamentOp::Unload => "The printer has no M702",
                };
                if job_active {
                    ctx.toasts.warning(title);
                } else {
                    // Heating already happened: just the scripted moves.
                    let plan = filament_plan(op, tool, false, false, f32::NAN);
                    run_plan(printer, tool, plan.get(1..).unwrap_or_default());
                    ctx.toasts
                        .info(title)
                        .message("Used Sprint's scripted sequence instead.");
                }
            } else if now - watch.started > NATIVE_WATCH_SECS {
                self.watch = None;
            }
        }
        let presets = &profile.temp_presets;
        let native = !self.native_rejected && native_load_unload(state);
        let mut started = None;
        section(
            ui,
            icons::FILAMENT,
            "Extruder",
            |_| {},
            |ui| {
                if job_active && connected {
                    notice(ui, icons::LOCK, "Locked while a job runs", Tone::Warning)
                        .on_hover_text(lock.unwrap_or_default());
                }
                ui.add_enabled_ui(lock.is_none(), |ui| {
                    if hotends > 1 {
                        let options: Vec<(u8, String)> =
                            (0..=last_tool).map(|t| (t, format!("T{t}"))).collect();
                        let mut picked = tool;
                        let r = ui
                            .add(
                                SegmentedControl::new(&mut picked, &options)
                                    .fill_width(true)
                                    .id_salt("sprint-controls-tool"),
                            )
                            .on_hover_text(
                                "Tool to extrude with (the printer switches to it first)",
                            );
                        if r.changed() {
                            self.tool = Some(picked);
                        }
                    }
                    let control = ExtruderControl::new(&mut self.length, &mut self.speed)
                        .limits(MAX_EXTRUDE_LENGTH, 100.0)
                        .blocked(cold.as_deref())
                        .show(ui);
                    match control.action {
                        Some(ExtruderAction::Extrude { length, speed }) => {
                            printer.extrude(tool, length, speed * 60.0);
                        }
                        Some(ExtruderAction::Retract { length, speed }) => {
                            printer.extrude(tool, -length, speed * 60.0);
                        }
                        None => {}
                    }
                    if cold.is_some() {
                        heat_shortcut(ui, printer, heater, target, presets);
                    }
                    ui.add_space(2.0);
                    section_header(ui, "Filament");
                    let heat = heat_temperature(target, presets);
                    let how = if native {
                        format!(
                            "Heats to {} °C, then runs the firmware's M701 / M702.",
                            format_temperature(heat).trim_end_matches(".0")
                        )
                    } else {
                        format!(
                            "Heats to {} °C, then feeds {} mm or pulls {} mm out.",
                            format_temperature(heat).trim_end_matches(".0"),
                            LOAD_FEED_MM + LOAD_PURGE_MM,
                            UNLOAD_MM
                        )
                    };
                    let mut run = None;
                    ui.columns(2, |cols| {
                        let load = cols[0].add(
                            ActionButton::new("Load")
                                .icon(icons::DOWNLOAD)
                                .fill_width(true)
                                .tooltip(format!("Load filament into T{tool}. {how}")),
                        );
                        if load.clicked() {
                            run = Some(FilamentOp::Load);
                        }
                        let unload = cols[1].add(
                            ActionButton::new("Unload")
                                .icon(icons::UPLOAD)
                                .fill_width(true)
                                .tooltip(format!("Unload filament from T{tool}. {how}")),
                        );
                        if unload.clicked() {
                            run = Some(FilamentOp::Unload);
                        }
                    });
                    if let Some(op) = run {
                        if native {
                            // Listen before sending so the reply is not missed.
                            started = Some(NativeWatch {
                                op,
                                tool,
                                events: printer.subscribe(),
                                started: now,
                            });
                        }
                        let plan = filament_plan(op, tool, hotends > 1, native, heat);
                        run_plan(printer, tool, &plan);
                    }
                    caption(ui, &how);
                });
            },
        );
        if started.is_some() {
            self.watch = started;
        }
    }
}

/// "Heat to PLA · 205 °C" under a cold hotend, or the heating progress.
fn heat_shortcut(
    ui: &mut Ui,
    printer: &Printer,
    heater: Heater,
    target: f32,
    presets: &[TempPreset],
) {
    if target >= MIN_EXTRUDE_TEMP {
        caption(
            ui,
            &format!(
                "Heating to {} °C: extrusion unlocks at {} °C.",
                format_temperature(target).trim_end_matches(".0"),
                MIN_EXTRUDE_TEMP
            ),
        );
        return;
    }
    let (name, temp) = heat_preset(presets).unwrap_or(("default", FALLBACK_HEAT));
    let label = format!(
        "Heat to {name} · {} °C",
        format_temperature(temp).trim_end_matches(".0")
    );
    let r = ui.add(
        ActionButton::new(&label)
            .icon(icons::HEATING)
            .fill_width(true)
            .tooltip("Set the hotend target so the filament can be extruded"),
    );
    if r.clicked() {
        printer.set_temperature(heater, temp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_host::sprint_proto::{FirmwareInfo, SettingCommand};

    fn presets() -> Vec<TempPreset> {
        vec![
            TempPreset {
                name: "Warm".into(),
                hotend: 60.0,
                bed: 0.0,
            },
            TempPreset {
                name: "PETG".into(),
                hotend: 240.0,
                bed: 80.0,
            },
        ]
    }

    #[test]
    fn heat_temperature_prefers_a_hot_target() {
        assert_eq!(heat_temperature(230.0, &presets()), 230.0);
        assert_eq!(heat_temperature(0.0, &presets()), 240.0);
        assert_eq!(heat_temperature(150.0, &presets()), 240.0);
        assert_eq!(heat_temperature(f32::NAN, &[]), FALLBACK_HEAT);
        assert_eq!(heat_preset(&presets()), Some(("PETG", 240.0)));
        assert_eq!(heat_preset(&presets()[..1]), None);
    }

    #[test]
    fn scripted_load_and_unload() {
        let load = filament_plan(FilamentOp::Load, 0, false, false, 205.0);
        assert_eq!(
            load,
            [
                FilamentStep::Gcode("M109 S205".into()),
                FilamentStep::Extrude {
                    length: LOAD_FEED_MM,
                    feedrate: LOAD_FEED_MM_MIN
                },
                FilamentStep::Extrude {
                    length: LOAD_PURGE_MM,
                    feedrate: LOAD_PURGE_MM_MIN
                },
            ]
        );
        let unload = filament_plan(FilamentOp::Unload, 1, true, false, 240.5);
        assert_eq!(unload[0], FilamentStep::Gcode("M109 T1 S240.5".into()));
        assert!(matches!(
            unload.last(),
            Some(FilamentStep::Extrude { length, .. }) if *length < 0.0
        ));
    }

    #[test]
    fn native_load_and_unload() {
        assert_eq!(
            filament_plan(FilamentOp::Load, 0, false, true, 205.0),
            [
                FilamentStep::Gcode("M109 S205".into()),
                FilamentStep::Gcode("M701".into())
            ]
        );
        assert_eq!(
            filament_plan(FilamentOp::Unload, 2, true, true, f32::NAN)[1],
            FilamentStep::Gcode("M702 T2".into())
        );
        // Silly temperatures are bounded.
        assert_eq!(
            filament_plan(FilamentOp::Load, 0, false, true, 20.0)[0],
            FilamentStep::Gcode("M109 S170".into())
        );
    }

    #[test]
    fn native_support_detection() {
        let mut state = PrinterState::default();
        assert!(!native_load_unload(&state));
        state.firmware = Some(FirmwareInfo {
            flavor: FirmwareFlavor::Marlin,
            ..FirmwareInfo::default()
        });
        assert!(!native_load_unload(&state));
        if let Some(m603) = SettingCommand::parse("M603 T0 L0 U100") {
            state.settings.insert(m603);
        }
        assert!(native_load_unload(&state));
        let prusa = PrinterState {
            firmware: Some(FirmwareInfo {
                flavor: FirmwareFlavor::Prusa,
                ..FirmwareInfo::default()
            }),
            ..PrinterState::default()
        };
        assert!(native_load_unload(&prusa));
        let caps = PrinterState {
            capabilities: vec![("FILAMENT_LOAD_UNLOAD".into(), true)],
            ..PrinterState::default()
        };
        assert!(native_load_unload(&caps));
    }

    #[test]
    fn native_rejections_are_recognised() {
        assert!(rejects_native(
            "Unknown command: \"M701\"",
            FilamentOp::Load
        ));
        assert!(rejects_native(
            "unknown command: M702 T1",
            FilamentOp::Unload
        ));
        assert!(!rejects_native(
            "Unknown command: \"M702\"",
            FilamentOp::Load
        ));
        assert!(!rejects_native("M701 done", FilamentOp::Load));
    }

    #[test]
    fn cold_reasons() {
        assert!(cold_reason(f32::NAN).is_some());
        assert_eq!(
            cold_reason(24.0).as_deref(),
            Some("Hotend at 24 °C: extruding needs 170 °C.")
        );
        assert_eq!(cold_reason(170.0), None);
        assert_eq!(cold_reason(215.0), None);
    }
}
