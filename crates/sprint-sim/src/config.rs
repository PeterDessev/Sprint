//! Configuration of a virtual printer.

use serde::{Deserialize, Serialize};

/// Largest synthetic SD file the simulator generates content for (bytes).
/// Seeded `sd_files` entries larger than this are clamped.
pub const MAX_SYNTHETIC_FILE_SIZE: u64 = 32 * 1024 * 1024;

/// Configuration of a virtual printer.
///
/// Every field has a sensible default (see [`SimConfig::default`]); values
/// outside their valid range are clamped when the simulator starts, so a
/// hand-edited or corrupt configuration never makes the simulator panic.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SimConfig {
    /// Reported as `MACHINE_TYPE` by M115. Default "Sprint Virtual Printer".
    pub machine_name: String,
    /// Number of hotends/extruders (1..=8). Default 1.
    pub extruders: u8,
    /// Whether the printer has a heated bed. Default true.
    pub heated_bed: bool,
    /// Whether the printer has a heated chamber. Default false.
    pub heated_chamber: bool,
    /// Build volume X, Y, Z in mm. Default `[220, 220, 250]`.
    pub build_volume: [f32; 3],
    /// Simulation speed: 1.0 = real time, 10.0 = ten times faster. Applies
    /// to heating, moves, dwells, autotune, busy/keepalive intervals,
    /// auto-report intervals and the boot delay. Default 1.0.
    pub time_scale: f32,
    /// Moves (including homing and probing) take zero time. Default false.
    pub instant_moves: bool,
    /// Delay before "start" and the boot banner, in (scaled) milliseconds.
    /// Default 300.
    pub boot_delay_ms: u64,
    /// Probability (0..=1) that a valid numbered line is treated as
    /// corrupted (checksum mismatch + resend). Default 0.
    pub checksum_error_rate: f32,
    /// Probability (0..=1) that an "ok" is lost. Default 0.
    pub drop_ok_rate: f32,
    /// Planner buffer size in moves. Default 16.
    pub planner_buffer: usize,
    /// Command buffer size (Marlin BUFSIZE). Default 4.
    pub rx_buffer: usize,
    /// Advertise and honour M155 temperature auto-reports. Default true.
    pub auto_report_temps: bool,
    /// Advertise and honour M154 position auto-reports. Default true.
    pub auto_report_position: bool,
    /// Reply `ok N<line> P<planner free> B<buffer free>`. Default false.
    pub advanced_ok: bool,
    /// G29 grid size (columns, rows), each 2..=15. Default (5, 5).
    pub mesh_size: (usize, usize),
    /// Initial SD card contents as (long file name, size in bytes); the
    /// files get synthetic G-code content of exactly that size.
    pub sd_files: Vec<(String, u64)>,
    /// Ambient temperature in °C. Default 22.
    pub ambient: f32,
    /// Seed for fault injection, sensor noise and the simulated bed shape.
    pub seed: u64,
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig {
            machine_name: "Sprint Virtual Printer".to_string(),
            extruders: 1,
            heated_bed: true,
            heated_chamber: false,
            build_volume: [220.0, 220.0, 250.0],
            time_scale: 1.0,
            instant_moves: false,
            boot_delay_ms: 300,
            checksum_error_rate: 0.0,
            drop_ok_rate: 0.0,
            planner_buffer: 16,
            rx_buffer: 4,
            auto_report_temps: true,
            auto_report_position: true,
            advanced_ok: false,
            mesh_size: (5, 5),
            sd_files: vec![
                ("calibration_cube.gcode".to_string(), 48_213),
                ("benchy_0.2mm_PLA.gcode".to_string(), 1_843_271),
                ("first layer test.gcode".to_string(), 12_630),
            ],
            ambient: 22.0,
            seed: 0x5350_5249_4e54, // "SPRINT"
        }
    }
}

/// Clamp a probability to 0..=1 (NaN becomes 0).
pub(crate) fn sanitize_rate(rate: f32) -> f32 {
    if rate.is_finite() {
        rate.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Clamp a time scale to a usable range (non-finite or non-positive -> 1).
pub(crate) fn sanitize_time_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        scale.clamp(0.001, 100_000.0)
    } else {
        1.0
    }
}

impl SimConfig {
    /// A copy with every value clamped to its valid range.
    pub(crate) fn sanitized(&self) -> SimConfig {
        let mut c = self.clone();
        c.extruders = c.extruders.clamp(1, 8);
        let defaults = [220.0, 220.0, 250.0];
        for (v, d) in c.build_volume.iter_mut().zip(defaults) {
            if !v.is_finite() || *v <= 1.0 {
                *v = d;
            }
            *v = v.min(10_000.0);
        }
        c.time_scale = sanitize_time_scale(c.time_scale);
        c.boot_delay_ms = c.boot_delay_ms.min(600_000);
        c.checksum_error_rate = sanitize_rate(c.checksum_error_rate);
        c.drop_ok_rate = sanitize_rate(c.drop_ok_rate);
        c.planner_buffer = c.planner_buffer.clamp(1, 256);
        c.rx_buffer = c.rx_buffer.clamp(1, 64);
        c.mesh_size = (c.mesh_size.0.clamp(2, 15), c.mesh_size.1.clamp(2, 15));
        for (_, size) in &mut c.sd_files {
            *size = (*size).min(MAX_SYNTHETIC_FILE_SIZE);
        }
        c.sd_files.retain(|(name, _)| !name.trim().is_empty());
        c.sd_files.truncate(256);
        if !c.ambient.is_finite() {
            c.ambient = 22.0;
        }
        c.ambient = c.ambient.clamp(-40.0, 80.0);
        c.machine_name = c.machine_name.replace(['\r', '\n'], " ");
        if c.machine_name.trim().is_empty() {
            c.machine_name = "Sprint Virtual Printer".to_string();
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_contract() {
        let c = SimConfig::default();
        assert_eq!(c.machine_name, "Sprint Virtual Printer");
        assert_eq!(c.extruders, 1);
        assert!(c.heated_bed);
        assert!(!c.heated_chamber);
        assert_eq!(c.build_volume, [220.0, 220.0, 250.0]);
        assert_eq!(c.time_scale, 1.0);
        assert!(!c.instant_moves);
        assert_eq!(c.boot_delay_ms, 300);
        assert_eq!(c.planner_buffer, 16);
        assert_eq!(c.rx_buffer, 4);
        assert!(c.auto_report_temps && c.auto_report_position);
        assert!(!c.advanced_ok);
        assert_eq!(c.mesh_size, (5, 5));
        assert!(!c.sd_files.is_empty());
        assert_eq!(c.ambient, 22.0);
    }

    #[test]
    fn sanitize_clamps_garbage() {
        let c = SimConfig {
            extruders: 0,
            build_volume: [f32::NAN, -5.0, 1e9],
            time_scale: -3.0,
            checksum_error_rate: 7.0,
            drop_ok_rate: f32::NAN,
            planner_buffer: 0,
            rx_buffer: 0,
            mesh_size: (0, 100),
            sd_files: vec![("  ".into(), 5), ("big".into(), u64::MAX)],
            ambient: f32::INFINITY,
            machine_name: "a\nb".into(),
            ..SimConfig::default()
        }
        .sanitized();
        assert_eq!(c.extruders, 1);
        assert_eq!(c.build_volume, [220.0, 220.0, 10_000.0]);
        assert_eq!(c.time_scale, 1.0);
        assert_eq!(c.checksum_error_rate, 1.0);
        assert_eq!(c.drop_ok_rate, 0.0);
        assert_eq!(c.planner_buffer, 1);
        assert_eq!(c.rx_buffer, 1);
        assert_eq!(c.mesh_size, (2, 15));
        assert_eq!(
            c.sd_files,
            vec![("big".to_string(), MAX_SYNTHETIC_FILE_SIZE)]
        );
        assert_eq!(c.ambient, 22.0);
        assert_eq!(c.machine_name, "a b");
    }
}
