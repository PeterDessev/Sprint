//! Firmware settings stored in the simulated EEPROM, the commands that
//! change them and the Marlin 2.1 M503 report.

use crate::format::{f2, fixed};
use crate::parse::Command;
use crate::planner::Limits;
use crate::thermal::Gains;

/// Marlin's EEPROM layout version.
pub(crate) const EEPROM_VERSION: &str = "V86";

/// A material preheat preset (M145).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Preheat {
    pub hotend: f64,
    pub bed: f64,
    /// Fan speed 0..=255.
    pub fan: f64,
}

/// Every persistent setting.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Settings {
    pub steps_per_mm: [f64; 4],
    pub max_feedrate: [f64; 4],
    pub max_accel: [f64; 4],
    pub print_accel: f64,
    pub retract_accel: f64,
    pub travel_accel: f64,
    pub min_segment_time_us: f64,
    pub min_feedrate: f64,
    pub min_travel_feedrate: f64,
    pub junction_deviation: f64,
    pub home_offset: [f64; 3],
    pub preheat: [Preheat; 2],
    pub hotend_pid: Vec<Gains>,
    pub bed_pid: Gains,
    pub probe_offset: [f64; 3],
    pub lin_advance_k: Vec<f64>,
    pub fade_height: f64,
    pub current_xyz: [f64; 3],
    pub current_e: Vec<f64>,
    pub hybrid_xyz: [f64; 3],
    pub hybrid_e: Vec<f64>,
    pub stealth_xyz: [bool; 3],
    pub stealth_e: Vec<bool>,
    /// Input shaping (frequency Hz, damping) for X and Y.
    pub shaping: [(f64, f64); 2],
    pub retract_length: f64,
    pub retract_swap_length: f64,
    /// mm/s
    pub retract_feedrate: f64,
    pub retract_zraise: f64,
    pub recover_extra: f64,
    pub recover_swap_extra: f64,
    /// mm/s
    pub recover_feedrate: f64,
    pub autoretract: bool,
    pub power_loss_recovery: bool,
    pub runout_enabled: bool,
    pub runout_distance: f64,
    pub volumetric: bool,
    pub filament_diameter: Vec<f64>,
    pub load_length: Vec<f64>,
    pub unload_length: Vec<f64>,
}

/// Default hotend PID (Marlin's DEFAULT_Kp/Ki/Kd).
pub(crate) const DEFAULT_HOTEND_PID: Gains = Gains {
    kp: 22.20,
    ki: 1.08,
    kd: 114.00,
};
/// Default bed PID (tuned for the simulated 240 W bed).
pub(crate) const DEFAULT_BED_PID: Gains = Gains {
    kp: 92.03,
    ki: 3.47,
    kd: 608.21,
};

impl Settings {
    /// Factory defaults ("M502").
    pub(crate) fn defaults(extruders: usize) -> Settings {
        let n = extruders.max(1);
        Settings {
            steps_per_mm: [80.0, 80.0, 400.0, 93.0],
            max_feedrate: [300.0, 300.0, 5.0, 25.0],
            max_accel: [3000.0, 3000.0, 100.0, 10_000.0],
            print_accel: 3000.0,
            retract_accel: 3000.0,
            travel_accel: 3000.0,
            min_segment_time_us: 20_000.0,
            min_feedrate: 0.0,
            min_travel_feedrate: 0.0,
            junction_deviation: 0.013,
            home_offset: [0.0; 3],
            preheat: [
                Preheat {
                    hotend: 180.0,
                    bed: 70.0,
                    fan: 0.0,
                },
                Preheat {
                    hotend: 240.0,
                    bed: 110.0,
                    fan: 0.0,
                },
            ],
            hotend_pid: vec![DEFAULT_HOTEND_PID; n],
            bed_pid: DEFAULT_BED_PID,
            probe_offset: [10.0, 10.0, 0.0],
            lin_advance_k: vec![0.22; n],
            fade_height: 10.0,
            current_xyz: [800.0; 3],
            current_e: vec![800.0; n],
            hybrid_xyz: [100.0, 100.0, 3.0],
            hybrid_e: vec![30.0; n],
            stealth_xyz: [true; 3],
            stealth_e: vec![true; n],
            shaping: [(40.0, 0.15), (40.0, 0.15)],
            retract_length: 3.0,
            retract_swap_length: 13.0,
            retract_feedrate: 45.0,
            retract_zraise: 0.0,
            recover_extra: 0.0,
            recover_swap_extra: 0.0,
            recover_feedrate: 8.0,
            autoretract: false,
            power_loss_recovery: true,
            runout_enabled: true,
            runout_distance: 25.0,
            volumetric: false,
            filament_diameter: vec![1.75; n],
            load_length: vec![0.0; n],
            unload_length: vec![100.0; n],
        }
    }

    /// Planner limits derived from M201/M203/M204/M205.
    pub(crate) fn limits(&self) -> Limits {
        Limits {
            max_feedrate: self.max_feedrate,
            max_accel: self.max_accel,
            print_accel: self.print_accel,
            retract_accel: self.retract_accel,
            travel_accel: self.travel_accel,
            junction_deviation: self.junction_deviation,
            min_feedrate: self.min_feedrate,
            min_travel_feedrate: self.min_travel_feedrate,
        }
    }

    /// Serialized EEPROM image (for the size/CRC Marlin prints).
    pub(crate) fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(512);
        out.extend_from_slice(EEPROM_VERSION.as_bytes());
        let mut put = |v: f64| out.extend_from_slice(&(v as f32).to_le_bytes());
        for v in self
            .steps_per_mm
            .iter()
            .chain(&self.max_feedrate)
            .chain(&self.max_accel)
        {
            put(*v);
        }
        for v in [
            self.print_accel,
            self.retract_accel,
            self.travel_accel,
            self.min_segment_time_us,
            self.min_feedrate,
            self.min_travel_feedrate,
            self.junction_deviation,
        ] {
            put(v);
        }
        for v in self.home_offset.iter().chain(&self.probe_offset) {
            put(*v);
        }
        for p in &self.preheat {
            put(p.hotend);
            put(p.bed);
            put(p.fan);
        }
        for g in self.hotend_pid.iter().chain(std::iter::once(&self.bed_pid)) {
            put(g.kp);
            put(g.ki);
            put(g.kd);
        }
        for v in self
            .lin_advance_k
            .iter()
            .chain(&self.current_xyz)
            .chain(&self.current_e)
            .chain(&self.hybrid_xyz)
            .chain(&self.hybrid_e)
            .chain(&self.filament_diameter)
            .chain(&self.load_length)
            .chain(&self.unload_length)
        {
            put(*v);
        }
        for (f, d) in self.shaping {
            put(f);
            put(d);
        }
        for v in [
            self.fade_height,
            self.retract_length,
            self.retract_swap_length,
            self.retract_feedrate,
            self.retract_zraise,
            self.recover_extra,
            self.recover_swap_extra,
            self.recover_feedrate,
            self.runout_distance,
        ] {
            put(v);
        }
        for b in self.stealth_xyz.iter().chain(&self.stealth_e).chain([
            &self.autoretract,
            &self.power_loss_recovery,
            &self.runout_enabled,
            &self.volumetric,
        ]) {
            out.push(u8::from(*b));
        }
        out
    }
}

/// Context the report needs besides the stored settings.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReportContext {
    pub extruders: usize,
    pub heated_bed: bool,
    pub leveling_active: bool,
}

/// One section of the M503 report.
pub(crate) struct Section {
    /// Command code that reports / sets the section ("M92").
    pub code: &'static str,
    /// Heading printed as "echo:; <heading>".
    pub heading: String,
    /// Lines, starting with two spaces ("  M92 X80.00 ...").
    pub lines: Vec<String>,
}

fn int(v: f64) -> String {
    fixed(v.round(), 0)
}

impl Settings {
    /// Every report section in Marlin 2.1 order.
    pub(crate) fn sections(&self, ctx: ReportContext) -> Vec<Section> {
        let n = ctx.extruders.max(1);
        let multi = n > 1;
        let mut s = Vec::new();
        let mut add = |code: &'static str, heading: &str, lines: Vec<String>| {
            s.push(Section {
                code,
                heading: heading.to_string(),
                lines,
            });
        };
        add("G21", "Linear Units:", vec!["  G21 ; (mm)".to_string()]);
        let mut m200 = vec![format!(
            "  M200 S{} D{}",
            u8::from(self.volumetric),
            f2(self.filament_diameter.first().copied().unwrap_or(1.75))
        )];
        if multi {
            m200 = (0..n)
                .map(|e| {
                    format!(
                        "  M200 T{e} D{}",
                        f2(self.filament_diameter.get(e).copied().unwrap_or(1.75))
                    )
                })
                .collect();
            m200.insert(0, format!("  M200 S{}", u8::from(self.volumetric)));
        }
        add(
            "M200",
            if self.volumetric {
                "Filament settings:"
            } else {
                "Filament settings (Disabled):"
            },
            m200,
        );
        let [x, y, z, e] = self.steps_per_mm;
        add(
            "M92",
            "Steps per unit:",
            vec![format!("  M92 X{} Y{} Z{} E{}", f2(x), f2(y), f2(z), f2(e))],
        );
        let [x, y, z, e] = self.max_feedrate;
        add(
            "M203",
            "Max feedrates (units/s):",
            vec![format!(
                "  M203 X{} Y{} Z{} E{}",
                f2(x),
                f2(y),
                f2(z),
                f2(e)
            )],
        );
        let [x, y, z, e] = self.max_accel;
        add(
            "M201",
            "Max Acceleration (units/s2):",
            vec![format!(
                "  M201 X{} Y{} Z{} E{}",
                f2(x),
                f2(y),
                f2(z),
                f2(e)
            )],
        );
        add(
            "M204",
            "Acceleration (units/s2) (P<print-accel> R<retract-accel> T<travel-accel>):",
            vec![format!(
                "  M204 P{} R{} T{}",
                f2(self.print_accel),
                f2(self.retract_accel),
                f2(self.travel_accel)
            )],
        );
        add(
            "M205",
            "Advanced (B<min_segment_time_us> S<min_feedrate> T<min_travel_feedrate> J<junc_dev>):",
            vec![format!(
                "  M205 B{} S{} T{} J{}",
                f2(self.min_segment_time_us),
                f2(self.min_feedrate),
                f2(self.min_travel_feedrate),
                fixed(self.junction_deviation, 3)
            )],
        );
        let [x, y, z] = self.home_offset;
        add(
            "M206",
            "Home offset:",
            vec![format!("  M206 X{} Y{} Z{}", f2(x), f2(y), f2(z))],
        );
        add(
            "M420",
            "Auto Bed Leveling:",
            vec![format!(
                "  M420 S{} Z{} ; Leveling {}",
                u8::from(ctx.leveling_active),
                f2(self.fade_height),
                if ctx.leveling_active { "ON" } else { "OFF" }
            )],
        );
        add(
            "M145",
            "Material heatup parameters:",
            self.preheat
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    format!(
                        "  M145 S{i} H{} B{} F{}",
                        f2(p.hotend),
                        f2(p.bed),
                        int(p.fan * 100.0 / 255.0)
                    )
                })
                .collect(),
        );
        add(
            "M301",
            "Hotend PID:",
            (0..n)
                .map(|e| {
                    let g = self
                        .hotend_pid
                        .get(e)
                        .copied()
                        .unwrap_or(DEFAULT_HOTEND_PID);
                    if multi {
                        format!("  M301 E{e} P{} I{} D{}", f2(g.kp), f2(g.ki), f2(g.kd))
                    } else {
                        format!("  M301 P{} I{} D{}", f2(g.kp), f2(g.ki), f2(g.kd))
                    }
                })
                .collect(),
        );
        if ctx.heated_bed {
            let g = self.bed_pid;
            add(
                "M304",
                "Bed PID:",
                vec![format!("  M304 P{} I{} D{}", f2(g.kp), f2(g.ki), f2(g.kd))],
            );
        }
        add(
            "M413",
            "Power-loss recovery:",
            vec![format!(
                "  M413 S{} ; {}",
                u8::from(self.power_loss_recovery),
                if self.power_loss_recovery {
                    "ON"
                } else {
                    "OFF"
                }
            )],
        );
        add(
            "M207",
            "Retract (S<length> F<units/m> Z<lift>):",
            vec![format!(
                "  M207 S{} W{} F{} Z{}",
                f2(self.retract_length),
                f2(self.retract_swap_length),
                f2(self.retract_feedrate * 60.0),
                f2(self.retract_zraise)
            )],
        );
        add(
            "M208",
            "Recover (S<length> F<units/m>):",
            vec![format!(
                "  M208 S{} W{} F{}",
                f2(self.recover_extra),
                f2(self.recover_swap_extra),
                f2(self.recover_feedrate * 60.0)
            )],
        );
        add(
            "M209",
            "Auto-Retract: S=0 to disable, 1 to interpret E-only moves as retract/recover",
            vec![format!("  M209 S{}", u8::from(self.autoretract))],
        );
        let [x, y, z] = self.probe_offset;
        add(
            "M851",
            "Z-Probe Offset:",
            vec![format!("  M851 X{} Y{} Z{} ; (mm)", f2(x), f2(y), f2(z))],
        );
        let [x, y, z] = self.current_xyz;
        let mut m906 = vec![format!("  M906 X{} Y{} Z{}", int(x), int(y), int(z))];
        for e in 0..n {
            m906.push(format!(
                "  M906 T{e} E{}",
                int(self.current_e.get(e).copied().unwrap_or(800.0))
            ));
        }
        add("M906", "Stepper driver current:", m906);
        let [x, y, z] = self.hybrid_xyz;
        let mut m913 = vec![format!("  M913 X{} Y{} Z{}", int(x), int(y), int(z))];
        for e in 0..n {
            m913.push(format!(
                "  M913 T{e} E{}",
                int(self.hybrid_e.get(e).copied().unwrap_or(30.0))
            ));
        }
        add("M913", "Hybrid Threshold:", m913);
        let mut m569 = Vec::new();
        for mode in [true, false] {
            let axes: Vec<&str> = ["X", "Y", "Z"]
                .iter()
                .zip(self.stealth_xyz)
                .filter(|(_, s)| *s == mode)
                .map(|(a, _)| *a)
                .collect();
            if !axes.is_empty() {
                m569.push(format!("  M569 S{} {}", u8::from(mode), axes.join(" ")));
            }
        }
        for e in 0..n {
            let mode = self.stealth_e.get(e).copied().unwrap_or(true);
            m569.push(format!("  M569 S{} T{e} E", u8::from(mode)));
        }
        add("M569", "Driver stepping mode:", m569);
        add(
            "M900",
            "Linear Advance:",
            (0..n)
                .map(|e| {
                    let k = self.lin_advance_k.get(e).copied().unwrap_or(0.0);
                    if multi {
                        format!("  M900 T{e} K{}", f2(k))
                    } else {
                        format!("  M900 K{}", f2(k))
                    }
                })
                .collect(),
        );
        add(
            "M603",
            "Filament load/unload:",
            (0..n)
                .map(|e| {
                    let l = self.load_length.get(e).copied().unwrap_or(0.0);
                    let u = self.unload_length.get(e).copied().unwrap_or(100.0);
                    if multi {
                        format!("  M603 T{e} L{} U{} ; (mm)", f2(l), f2(u))
                    } else {
                        format!("  M603 L{} U{} ; (mm)", f2(l), f2(u))
                    }
                })
                .collect(),
        );
        add(
            "M412",
            "Filament runout sensor:",
            vec![format!(
                "  M412 S{} D{} ; Sensor {}",
                u8::from(self.runout_enabled),
                f2(self.runout_distance),
                if self.runout_enabled { "ON" } else { "OFF" }
            )],
        );
        add(
            "M593",
            "Input Shaping:",
            ["X", "Y"]
                .iter()
                .zip(self.shaping)
                .map(|(a, (f, d))| format!("  M593 {a} F{} D{}", f2(f), f2(d)))
                .collect(),
        );
        s
    }

    /// Format sections like Marlin: headings and "echo:" prefixes unless
    /// `for_replay` (M503 S0).
    pub(crate) fn format_sections(sections: &[Section], for_replay: bool) -> Vec<String> {
        let mut out = Vec::new();
        for s in sections {
            if !for_replay {
                out.push(format!("echo:; {}", s.heading));
            }
            for l in &s.lines {
                if for_replay {
                    out.push(l.clone());
                } else {
                    out.push(format!("echo:{l}"));
                }
            }
        }
        out
    }

    /// The full M503 report.
    pub(crate) fn report(&self, ctx: ReportContext, for_replay: bool) -> Vec<String> {
        Settings::format_sections(&self.sections(ctx), for_replay)
    }

    /// The report for a single command code ("M92"), used when a setting
    /// command is sent without parameters.
    pub(crate) fn report_one(&self, code: &str, ctx: ReportContext) -> Vec<String> {
        let sections: Vec<Section> = self
            .sections(ctx)
            .into_iter()
            .filter(|s| s.code == code)
            .collect();
        Settings::format_sections(&sections, false)
    }
}

/// A positive, finite parameter value.
fn positive(c: &Command, letter: char) -> Option<f64> {
    c.value(letter).filter(|v| v.is_finite() && *v > 0.0)
}

/// A non-negative, finite parameter value.
fn non_negative(c: &Command, letter: char) -> Option<f64> {
    c.value(letter).filter(|v| v.is_finite() && *v >= 0.0)
}

/// Resolve the extruder index from a T (or other) parameter.
fn extruder_index(c: &Command, letter: char, n: usize) -> Option<usize> {
    match c.int(letter) {
        None => Some(0),
        Some(i) if i >= 0 && (i as usize) < n => Some(i as usize),
        Some(_) => None,
    }
}

/// Result of applying a settings command.
#[derive(Debug, Default)]
pub(crate) struct Applied {
    /// Lines to print (errors or reports).
    pub output: Vec<String>,
}

impl Applied {
    fn line(s: impl Into<String>) -> Applied {
        Applied {
            output: vec![s.into()],
        }
    }
}

impl Settings {
    /// Apply a settings command (M92 M201 M203 M204 M205 M206 M145 M301
    /// M304 M851 M900 M906 M913 M569 M593 M207 M208 M209 M413 M412 M200
    /// M603). Returns `None` when the command is not a settings command.
    /// Commands without parameters print their report section.
    pub(crate) fn apply(&mut self, c: &Command, ctx: ReportContext) -> Option<Applied> {
        if c.letter != b'M' {
            return None;
        }
        let n = ctx.extruders.max(1);
        let code = match c.code {
            92 => "M92",
            200 => "M200",
            201 => "M201",
            203 => "M203",
            204 => "M204",
            205 => "M205",
            206 => "M206",
            145 => "M145",
            207 => "M207",
            208 => "M208",
            209 => "M209",
            301 => "M301",
            304 => "M304",
            412 => "M412",
            413 => "M413",
            569 => "M569",
            593 => "M593",
            603 => "M603",
            851 => "M851",
            900 => "M900",
            906 => "M906",
            913 => "M913",
            _ => return None,
        };
        if code == "M304" && !ctx.heated_bed {
            return None;
        }
        let selector_only = match code {
            "M301" => !(c.seen('P') || c.seen('I') || c.seen('D')),
            "M900" => !c.seen('K'),
            _ => !c.has_params(),
        };
        if selector_only {
            return Some(Applied {
                output: self.report_one(code, ctx),
            });
        }
        const AXES: [char; 4] = ['X', 'Y', 'Z', 'E'];
        match code {
            "M92" => {
                for (i, a) in AXES.iter().enumerate() {
                    if let Some(v) = positive(c, *a) {
                        self.steps_per_mm[i] = v;
                    }
                }
            }
            "M203" => {
                for (i, a) in AXES.iter().enumerate() {
                    if let Some(v) = positive(c, *a) {
                        self.max_feedrate[i] = v;
                    }
                }
            }
            "M201" => {
                for (i, a) in AXES.iter().enumerate() {
                    if let Some(v) = positive(c, *a) {
                        self.max_accel[i] = v;
                    }
                }
            }
            "M204" => {
                if let Some(v) = positive(c, 'S') {
                    self.print_accel = v;
                    self.travel_accel = v;
                }
                if let Some(v) = positive(c, 'P') {
                    self.print_accel = v;
                }
                if let Some(v) = positive(c, 'R') {
                    self.retract_accel = v;
                }
                if let Some(v) = positive(c, 'T') {
                    self.travel_accel = v;
                }
            }
            "M205" => {
                if let Some(v) = non_negative(c, 'B') {
                    self.min_segment_time_us = v;
                }
                if let Some(v) = non_negative(c, 'S') {
                    self.min_feedrate = v;
                }
                if let Some(v) = non_negative(c, 'T') {
                    self.min_travel_feedrate = v;
                }
                if let Some(v) = c.value('J') {
                    if (0.01..=0.3).contains(&v) {
                        self.junction_deviation = v;
                    } else {
                        return Some(Applied::line("Error:?J out of range (0.01 to 0.3)"));
                    }
                }
            }
            "M206" => {
                for (i, a) in AXES[..3].iter().enumerate() {
                    if let Some(v) = c.value(*a) {
                        self.home_offset[i] = v.clamp(-1000.0, 1000.0);
                    }
                }
            }
            "M145" => {
                let idx = c.int('S').unwrap_or(0);
                let Some(p) = usize::try_from(idx)
                    .ok()
                    .and_then(|i| self.preheat.get_mut(i))
                else {
                    return Some(Applied::line("Error:?Invalid material."));
                };
                if let Some(v) = non_negative(c, 'H') {
                    p.hotend = v.min(260.0);
                }
                if let Some(v) = non_negative(c, 'B') {
                    p.bed = v.min(140.0);
                }
                if let Some(v) = non_negative(c, 'F') {
                    p.fan = v.min(255.0);
                }
            }
            "M301" => {
                let Some(e) = extruder_index(c, 'E', n) else {
                    return Some(Applied::line("Error:Invalid extruder"));
                };
                if let Some(g) = self.hotend_pid.get_mut(e) {
                    if let Some(v) = non_negative(c, 'P') {
                        g.kp = v;
                    }
                    if let Some(v) = non_negative(c, 'I') {
                        g.ki = v;
                    }
                    if let Some(v) = non_negative(c, 'D') {
                        g.kd = v;
                    }
                }
            }
            "M304" => {
                if let Some(v) = non_negative(c, 'P') {
                    self.bed_pid.kp = v;
                }
                if let Some(v) = non_negative(c, 'I') {
                    self.bed_pid.ki = v;
                }
                if let Some(v) = non_negative(c, 'D') {
                    self.bed_pid.kd = v;
                }
            }
            "M851" => {
                for (i, a) in AXES[..2].iter().enumerate() {
                    if let Some(v) = c.value(*a) {
                        if (-1000.0..=1000.0).contains(&v) {
                            self.probe_offset[i] = v;
                        } else {
                            return Some(Applied::line(format!(
                                "Error:?{a} out of range (-1000 to 1000)"
                            )));
                        }
                    }
                }
                if let Some(v) = c.value('Z') {
                    if (-20.0..=20.0).contains(&v) {
                        self.probe_offset[2] = v;
                    } else {
                        return Some(Applied::line("Error:?Z out of range (-20 to 20)"));
                    }
                }
            }
            "M900" => {
                let Some(e) = extruder_index(c, 'T', n) else {
                    return Some(Applied::line("Error:Invalid extruder"));
                };
                if let Some(k) = c.value('K') {
                    if (0.0..=10.0).contains(&k) {
                        if let Some(slot) = self.lin_advance_k.get_mut(e) {
                            *slot = k;
                        }
                    } else {
                        return Some(Applied::line("Error:?K value out of range (0-10)."));
                    }
                }
            }
            "M906" | "M913" => {
                let Some(e) = extruder_index(c, 'T', n) else {
                    return Some(Applied::line("Error:Invalid extruder"));
                };
                let (xyz, ev) = if code == "M906" {
                    (&mut self.current_xyz, &mut self.current_e)
                } else {
                    (&mut self.hybrid_xyz, &mut self.hybrid_e)
                };
                for (i, a) in AXES[..3].iter().enumerate() {
                    if let Some(v) = positive(c, *a) {
                        xyz[i] = v.min(5000.0).round();
                    }
                }
                if let (Some(v), Some(slot)) = (positive(c, 'E'), ev.get_mut(e)) {
                    *slot = v.min(5000.0).round();
                }
            }
            "M569" => {
                let mode = c.flag('S');
                for (i, a) in AXES[..3].iter().enumerate() {
                    if c.seen(*a) {
                        self.stealth_xyz[i] = mode;
                    }
                }
                if c.seen('E') {
                    let Some(e) = extruder_index(c, 'T', n) else {
                        return Some(Applied::line("Error:Invalid extruder"));
                    };
                    if let Some(slot) = self.stealth_e.get_mut(e) {
                        *slot = mode;
                    }
                }
            }
            "M593" => {
                let (x, y) = match (c.seen('X'), c.seen('Y')) {
                    (false, false) => (true, true),
                    other => other,
                };
                for (i, sel) in [x, y].into_iter().enumerate() {
                    if !sel {
                        continue;
                    }
                    if let Some(f) = c.value('F') {
                        if f == 0.0 || (5.0..=200.0).contains(&f) {
                            self.shaping[i].0 = f;
                        } else {
                            return Some(Applied::line(
                                "Error:?Frequency (F) must be greater than 5.00 or 0 to disable",
                            ));
                        }
                    }
                    if let Some(d) = c.value('D') {
                        if (0.0..=1.0).contains(&d) {
                            self.shaping[i].1 = d;
                        } else {
                            return Some(Applied::line("Error:?Zeta (D) value out of range (0-1)"));
                        }
                    }
                }
            }
            "M207" => {
                if let Some(v) = non_negative(c, 'S') {
                    self.retract_length = v.min(100.0);
                }
                if let Some(v) = non_negative(c, 'W') {
                    self.retract_swap_length = v.min(100.0);
                }
                if let Some(v) = positive(c, 'F') {
                    self.retract_feedrate = v / 60.0;
                }
                if let Some(v) = non_negative(c, 'Z') {
                    self.retract_zraise = v.min(10.0);
                }
            }
            "M208" => {
                if let Some(v) = c.value('S') {
                    self.recover_extra = v.clamp(-100.0, 100.0);
                }
                if let Some(v) = c.value('W') {
                    self.recover_swap_extra = v.clamp(-100.0, 100.0);
                }
                if let Some(v) = positive(c, 'F') {
                    self.recover_feedrate = v / 60.0;
                }
            }
            "M209" => {
                if c.seen('S') {
                    self.autoretract = c.flag('S');
                }
            }
            "M413" => {
                if c.seen('S') {
                    self.power_loss_recovery = c.flag('S');
                }
            }
            "M412" => {
                if c.seen('S') {
                    self.runout_enabled = c.flag('S');
                }
                if let Some(v) = non_negative(c, 'D') {
                    self.runout_distance = v.min(1000.0);
                }
            }
            "M200" => {
                let Some(e) = extruder_index(c, 'T', n) else {
                    return Some(Applied::line("Error:Invalid extruder"));
                };
                if let Some(d) = non_negative(c, 'D') {
                    if let Some(slot) = self.filament_diameter.get_mut(e) {
                        *slot = d.min(5.0);
                    }
                    self.volumetric = d > 0.0;
                }
                if c.seen('S') {
                    self.volumetric = c.flag('S');
                }
            }
            "M603" => {
                let Some(e) = extruder_index(c, 'T', n) else {
                    return Some(Applied::line("Error:Invalid extruder"));
                };
                if let (Some(v), Some(slot)) = (non_negative(c, 'L'), self.load_length.get_mut(e)) {
                    *slot = v.min(1000.0);
                }
                if let (Some(v), Some(slot)) = (non_negative(c, 'U'), self.unload_length.get_mut(e))
                {
                    *slot = v.min(1000.0);
                }
            }
            _ => return None,
        }
        Some(Applied::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(n: usize) -> ReportContext {
        ReportContext {
            extruders: n,
            heated_bed: true,
            leveling_active: false,
        }
    }

    #[test]
    fn report_has_marlin_headers_and_values() {
        let s = Settings::defaults(1);
        let r = s.report(ctx(1), false);
        assert!(r.contains(&"echo:; Steps per unit:".to_string()));
        assert!(r.contains(&"echo:  M92 X80.00 Y80.00 Z400.00 E93.00".to_string()));
        assert!(r.contains(&"echo:  M203 X300.00 Y300.00 Z5.00 E25.00".to_string()));
        assert!(r.contains(&"echo:  M201 X3000.00 Y3000.00 Z100.00 E10000.00".to_string()));
        assert!(r.contains(&"echo:  M204 P3000.00 R3000.00 T3000.00".to_string()));
        assert!(r.contains(&"echo:  M205 B20000.00 S0.00 T0.00 J0.013".to_string()));
        assert!(r.contains(&"echo:  M301 P22.20 I1.08 D114.00".to_string()));
        assert!(r.contains(&"echo:  M420 S0 Z10.00 ; Leveling OFF".to_string()));
        assert!(r.contains(&"echo:  M851 X10.00 Y10.00 Z0.00 ; (mm)".to_string()));
        assert!(r.contains(&"echo:  M900 K0.22".to_string()));
        assert!(r.contains(&"echo:  M569 S1 X Y Z".to_string()));
        for code in [
            "M92", "M203", "M201", "M204", "M205", "M206", "M145", "M301", "M304", "M851", "M900",
            "M420", "M906", "M913", "M569", "M593", "M207", "M208", "M209", "M413", "M412",
        ] {
            assert!(
                r.iter().any(|l| l.starts_with(&format!("echo:  {code} "))),
                "missing {code}"
            );
        }
        let replay = s.report(ctx(1), true);
        assert!(replay.iter().all(|l| l.starts_with("  ")));
    }

    #[test]
    fn multi_extruder_pid_lines() {
        let s = Settings::defaults(2);
        let r = s.report(ctx(2), false);
        assert!(r.contains(&"echo:  M301 E0 P22.20 I1.08 D114.00".to_string()));
        assert!(r.contains(&"echo:  M301 E1 P22.20 I1.08 D114.00".to_string()));
        assert!(r.contains(&"echo:  M900 T1 K0.22".to_string()));
    }

    #[test]
    fn apply_updates_values_and_reports() {
        let mut s = Settings::defaults(2);
        s.apply(&Command::parse("M92 X100 E-5 Z0"), ctx(2)).unwrap();
        assert_eq!(s.steps_per_mm, [100.0, 80.0, 400.0, 93.0]);
        s.apply(&Command::parse("M301 E1 P10 I0.5 D50"), ctx(2))
            .unwrap();
        assert_eq!(
            s.hotend_pid[1],
            Gains {
                kp: 10.0,
                ki: 0.5,
                kd: 50.0
            }
        );
        assert_eq!(s.hotend_pid[0], DEFAULT_HOTEND_PID);
        let out = s.apply(&Command::parse("M301 E5 P1"), ctx(2)).unwrap();
        assert_eq!(out.output, vec!["Error:Invalid extruder".to_string()]);
        s.apply(&Command::parse("M204 S1200"), ctx(2)).unwrap();
        assert_eq!((s.print_accel, s.travel_accel), (1200.0, 1200.0));
        let out = s.apply(&Command::parse("M205 J5"), ctx(2)).unwrap();
        assert!(out.output[0].starts_with("Error:"));
        let out = s.apply(&Command::parse("M92"), ctx(2)).unwrap();
        assert_eq!(
            out.output,
            vec![
                "echo:; Steps per unit:".to_string(),
                "echo:  M92 X100.00 Y80.00 Z400.00 E93.00".to_string()
            ]
        );
        s.apply(&Command::parse("M569 S0 X"), ctx(2)).unwrap();
        let r = s.report_one("M569", ctx(2));
        assert!(r.contains(&"echo:  M569 S1 Y Z".to_string()));
        assert!(r.contains(&"echo:  M569 S0 X".to_string()));
        s.apply(&Command::parse("M207 S4 F1800"), ctx(2)).unwrap();
        assert_eq!(s.retract_length, 4.0);
        assert!((s.retract_feedrate - 30.0).abs() < 1e-9);
        assert!(s.apply(&Command::parse("M104 S200"), ctx(2)).is_none());
        assert!(s.apply(&Command::parse("G92 X0"), ctx(2)).is_none());
    }

    #[test]
    fn serialization_changes_with_values() {
        let a = Settings::defaults(1);
        let mut b = a.clone();
        b.steps_per_mm[0] = 81.0;
        assert_eq!(a.serialize().len(), b.serialize().len());
        assert_ne!(
            crate::rng::crc16(&a.serialize()),
            crate::rng::crc16(&b.serialize())
        );
    }
}
