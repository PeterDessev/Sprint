//! Application configuration: printer profiles, connection options, user
//! macros and recent files, stored as TOML.
//!
//! [`AppConfig::save_to`] writes atomically (temporary file + rename), so a
//! crash never leaves a half-written file behind. [`AppConfig::load`] never
//! fails: a missing file gives the defaults, and a corrupt one is moved
//! aside to `config.toml.bak` before the defaults are used.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use sprint_gcode::{KinematicLimits, MachineState};

use crate::error::HostError;
use crate::script::{
    DEFAULT_CANCEL_SCRIPT, DEFAULT_PAUSE_SCRIPT, DEFAULT_RESUME_SCRIPT, ScriptContext,
};
use crate::transport::ConnectTarget;

/// Most entries kept in [`AppConfig::recent_files`].
pub const MAX_RECENT_FILES: usize = 12;
/// Default [`AppConfig::console_max_lines`].
pub const DEFAULT_CONSOLE_LINES: usize = 5000;
/// Largest accepted [`ConnectOptions::max_in_flight`].
pub const MAX_IN_FLIGHT_LIMIT: usize = 32;

/// When outgoing lines carry `N<n> ... *<checksum>` framing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChecksumMode {
    /// Every command except emergency commands (default; enables resends).
    #[default]
    Always,
    /// Only while a print or SD upload is running.
    PrintOnly,
    /// Never (firmware without line-number support). SD uploads are still
    /// numbered because Marlin requires it while writing to the card.
    Never,
}

/// How the engine talks to the printer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectOptions {
    /// Line framing. Default [`ChecksumMode::Always`].
    pub checksum: ChecksumMode,
    /// No `ok` for this long (and no `busy:`) means a lost `ok`; the
    /// engine then recovers. Seconds, default 10. Commands known to run
    /// long (homing, probing, heat waits, dwells) get six times as long.
    pub ok_timeout_secs: f32,
    /// How long the handshake may take (boards reset on open). Seconds,
    /// default 15.
    pub handshake_timeout_secs: f32,
    /// M105 polling interval when the firmware cannot auto-report
    /// temperatures. Seconds, default 2; 0 disables polling.
    pub temp_poll_secs: f32,
    /// Send M115, M503 and M114 after the handshake. Default true.
    pub query_on_connect: bool,
    /// Pulse DTR after opening a serial port (resets most boards). Default
    /// false.
    pub reset_on_connect: bool,
    /// Commands that may await their `ok` at the same time; 1 is strict
    /// ping-pong (default). Clamped to 1..=32.
    pub max_in_flight: usize,
    /// `M155 S<n>` / `M27 S<n>` auto-report interval when supported.
    /// Seconds, default 2.
    pub autoreport_interval_secs: u32,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        ConnectOptions {
            checksum: ChecksumMode::Always,
            ok_timeout_secs: 10.0,
            handshake_timeout_secs: 15.0,
            temp_poll_secs: 2.0,
            query_on_connect: true,
            reset_on_connect: false,
            max_in_flight: 1,
            autoreport_interval_secs: 2,
        }
    }
}

fn clamp_secs(value: f32, min: f32, max: f32, default: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        default
    }
}

impl ConnectOptions {
    /// A copy with every value forced into its valid range (garbage from a
    /// hand-edited file must not stall or crash the engine).
    pub fn sanitized(&self) -> ConnectOptions {
        let d = ConnectOptions::default();
        ConnectOptions {
            checksum: self.checksum,
            ok_timeout_secs: clamp_secs(self.ok_timeout_secs, 0.05, 3600.0, d.ok_timeout_secs),
            handshake_timeout_secs: clamp_secs(
                self.handshake_timeout_secs,
                0.1,
                3600.0,
                d.handshake_timeout_secs,
            ),
            temp_poll_secs: clamp_secs(self.temp_poll_secs, 0.0, 3600.0, d.temp_poll_secs),
            query_on_connect: self.query_on_connect,
            reset_on_connect: self.reset_on_connect,
            max_in_flight: self.max_in_flight.clamp(1, MAX_IN_FLIGHT_LIMIT),
            autoreport_interval_secs: self.autoreport_interval_secs.min(60),
        }
    }
}

/// A named hotend/bed temperature pair.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempPreset {
    /// Display name ("PLA").
    pub name: String,
    /// Hotend temperature (°C).
    pub hotend: f32,
    /// Bed temperature (°C).
    pub bed: f32,
}

impl TempPreset {
    fn new(name: &str, hotend: f32, bed: f32) -> TempPreset {
        TempPreset {
            name: name.to_string(),
            hotend,
            bed,
        }
    }
}

/// Where X0 Y0 is on the bed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BedOrigin {
    /// Front left corner (most Cartesian printers).
    #[default]
    FrontLeft,
    /// Bed centre (deltas, some CoreXY machines).
    Center,
}

/// A user-defined button that sends G-code.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Macro {
    /// Button label.
    pub name: String,
    /// G-code to send (templated like the pause/resume scripts).
    pub gcode: String,
    /// Optional button colour (RGB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
    /// Ask before sending.
    #[serde(default)]
    pub confirm: bool,
}

impl Macro {
    fn new(name: &str, gcode: &str, confirm: bool) -> Macro {
        Macro {
            name: name.to_string(),
            gcode: gcode.to_string(),
            color: None,
            confirm,
        }
    }
}

/// Everything Sprint knows about one printer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrinterProfile {
    /// Display name.
    pub name: String,
    /// Where to connect. Default: the virtual printer.
    pub connection: ConnectTarget,
    /// Protocol options.
    pub options: ConnectOptions,
    /// Connect when the application starts. Default false.
    pub auto_connect: bool,
    /// Bed size X, Y in mm. Default [220, 220].
    pub bed_size: [f32; 2],
    /// Maximum Z in mm. Default 250.
    pub max_z: f32,
    /// Where the origin is. Default front left.
    pub origin: BedOrigin,
    /// Number of extruders. Default 1.
    pub extruders: u8,
    /// Has a heated bed. Default true.
    pub heated_bed: bool,
    /// Has a heated chamber. Default false.
    pub heated_chamber: bool,
    /// Filament diameter in mm. Default 1.75.
    pub filament_diameter: f32,
    /// Nozzle diameter in mm. Default 0.4.
    pub nozzle_diameter: f32,
    /// Temperature presets.
    pub temp_presets: Vec<TempPreset>,
    /// XY jog feedrate (mm/min). Default 3000.
    pub jog_feedrate_xy: f32,
    /// Z jog feedrate (mm/min). Default 600.
    pub jog_feedrate_z: f32,
    /// Extrude/retract feedrate (mm/min). Default 300.
    pub extrude_feedrate: f32,
    /// Run when a host print is paused (see [`crate::script`]).
    pub pause_script: String,
    /// Run when a paused host print resumes.
    pub resume_script: String,
    /// Run when a host print is cancelled.
    pub cancel_script: String,
    /// Motion limits for print-time estimation; `None` = defaults or the
    /// values reported by M503.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "kinematics_toml"
    )]
    pub kinematics: Option<KinematicLimits>,
}

impl Default for PrinterProfile {
    fn default() -> Self {
        PrinterProfile {
            name: "Virtual printer".to_string(),
            connection: ConnectTarget::default(),
            options: ConnectOptions::default(),
            auto_connect: false,
            bed_size: [220.0, 220.0],
            max_z: 250.0,
            origin: BedOrigin::FrontLeft,
            extruders: 1,
            heated_bed: true,
            heated_chamber: false,
            filament_diameter: 1.75,
            nozzle_diameter: 0.4,
            temp_presets: vec![
                TempPreset::new("PLA", 205.0, 60.0),
                TempPreset::new("PETG", 240.0, 80.0),
                TempPreset::new("ABS", 250.0, 100.0),
                TempPreset::new("TPU", 225.0, 50.0),
            ],
            jog_feedrate_xy: 3000.0,
            jog_feedrate_z: 600.0,
            extrude_feedrate: 300.0,
            pause_script: DEFAULT_PAUSE_SCRIPT.to_string(),
            resume_script: DEFAULT_RESUME_SCRIPT.to_string(),
            cancel_script: DEFAULT_CANCEL_SCRIPT.to_string(),
            kinematics: None,
        }
    }
}

/// `PrinterProfile::kinematics` in TOML. `KinematicLimits` serializes
/// `junction_deviation: None` (classic jerk) by leaving the key out, and a
/// missing key reads back as the default junction deviation, so classic
/// jerk would not survive a save. Here `junction_deviation = false` stands
/// for `None`; files without the key keep their old meaning.
mod kinematics_toml {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use sprint_gcode::KinematicLimits;

    #[derive(Serialize, Deserialize)]
    #[serde(untagged)]
    enum Junction {
        /// `false`: classic jerk; `true`: the default junction deviation.
        Enabled(bool),
        /// Junction deviation in mm.
        Mm(f32),
    }

    #[derive(Serialize, Deserialize)]
    #[serde(default)]
    struct Repr {
        max_feedrate: [f32; 4],
        max_accel: [f32; 4],
        print_accel: f32,
        retract_accel: f32,
        travel_accel: f32,
        junction_deviation: Junction,
        jerk: [f32; 4],
        min_feedrate: f32,
    }

    impl Default for Repr {
        fn default() -> Self {
            Repr::from(KinematicLimits::default())
        }
    }

    impl From<KinematicLimits> for Repr {
        fn from(k: KinematicLimits) -> Self {
            Repr {
                max_feedrate: k.max_feedrate,
                max_accel: k.max_accel,
                print_accel: k.print_accel,
                retract_accel: k.retract_accel,
                travel_accel: k.travel_accel,
                junction_deviation: k
                    .junction_deviation
                    .map_or(Junction::Enabled(false), Junction::Mm),
                jerk: k.jerk,
                min_feedrate: k.min_feedrate,
            }
        }
    }

    impl From<Repr> for KinematicLimits {
        fn from(r: Repr) -> Self {
            let junction_deviation = match r.junction_deviation {
                Junction::Enabled(false) => None,
                Junction::Enabled(true) => KinematicLimits::default().junction_deviation,
                Junction::Mm(mm) => Some(mm),
            };
            KinematicLimits {
                max_feedrate: r.max_feedrate,
                max_accel: r.max_accel,
                print_accel: r.print_accel,
                retract_accel: r.retract_accel,
                travel_accel: r.travel_accel,
                junction_deviation,
                jerk: r.jerk,
                min_feedrate: r.min_feedrate,
            }
        }
    }

    pub(super) fn serialize<S: Serializer>(
        value: &Option<KinematicLimits>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.map(Repr::from).serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<KinematicLimits>, D::Error> {
        Ok(Option::<Repr>::deserialize(deserializer)?.map(KinematicLimits::from))
    }
}

/// Distance of the parking position from the bed edges (mm).
const PARK_MARGIN: f32 = 10.0;

fn finite_or(value: f32, default: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        default
    }
}

impl PrinterProfile {
    /// Smallest and largest X, Y, Z of the build volume, honouring
    /// [`PrinterProfile::origin`].
    pub fn build_volume(&self) -> ([f32; 3], [f32; 3]) {
        let x = finite_or(self.bed_size[0], 220.0);
        let y = finite_or(self.bed_size[1], 220.0);
        let z = finite_or(self.max_z, 250.0);
        match self.origin {
            BedOrigin::FrontLeft => ([0.0, 0.0, 0.0], [x, y, z]),
            BedOrigin::Center => ([-x / 2.0, -y / 2.0, 0.0], [x / 2.0, y / 2.0, z]),
        }
    }

    /// The parking position used by `{park_x}` / `{park_y}`: the back left
    /// corner, 10 mm in from the edges (Marlin's NOZZLE_PARK_POINT default).
    pub fn park_position(&self) -> (f32, f32) {
        let (min, max) = self.build_volume();
        let margin_x = PARK_MARGIN.min((max[0] - min[0]) / 2.0);
        let margin_y = PARK_MARGIN.min((max[1] - min[1]) / 2.0);
        (min[0] + margin_x, max[1] - margin_y)
    }

    /// A script context for `state` with this profile's geometry.
    pub fn script_context<'a>(&self, state: &'a MachineState) -> ScriptContext<'a> {
        let (_, max) = self.build_volume();
        let (park_x, park_y) = self.park_position();
        ScriptContext {
            state,
            max_x: max[0],
            max_y: max[1],
            max_z: max[2],
            park_x,
            park_y,
        }
    }
}

/// The whole configuration file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Printer profiles (at least one after loading).
    pub profiles: Vec<PrinterProfile>,
    /// Index into `profiles` (clamped by [`AppConfig::active`]).
    pub active_profile: usize,
    /// Recently opened G-code files, most recent first.
    pub recent_files: Vec<PathBuf>,
    /// User macros.
    pub macros: Vec<Macro>,
    /// Console lines kept in memory. Default 5000.
    pub console_max_lines: usize,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            profiles: vec![PrinterProfile::default()],
            active_profile: 0,
            recent_files: Vec::new(),
            macros: vec![
                Macro::new("Home", "G28", false),
                Macro::new("Bed mesh", "G28\nG29\nM420 V", true),
                Macro::new("Cool down", "M104 S0\nM140 S0\nM107", false),
                Macro::new("Motors off", "M84", false),
            ],
            console_max_lines: DEFAULT_CONSOLE_LINES,
        }
    }
}

static DEFAULT_PROFILE: LazyLock<PrinterProfile> = LazyLock::new(PrinterProfile::default);

impl AppConfig {
    /// Load from [`AppConfig::path`]. Never fails: see
    /// [`AppConfig::load_or_default`].
    pub fn load() -> AppConfig {
        AppConfig::load_or_default(&AppConfig::path())
    }

    /// Load from `path`. A missing or unreadable file is an
    /// [`HostError::Io`], invalid TOML (or text that is not UTF-8) an
    /// [`HostError::Config`].
    pub fn load_from(path: &Path) -> Result<AppConfig, HostError> {
        let bytes = fs::read(path)?;
        // Not UTF-8 is a corrupt file (to be backed up), not an I/O error.
        let text = String::from_utf8(bytes)
            .map_err(|e| HostError::Config(format!("{}: not UTF-8 text: {e}", path.display())))?;
        let config: AppConfig = toml::from_str(&text)
            .map_err(|e| HostError::Config(format!("{}: {e}", path.display())))?;
        Ok(config.normalized())
    }

    /// Load from `path`, falling back to the defaults when the file is
    /// missing or unreadable. A file that exists but does not parse is
    /// renamed to `<name>.bak` (replacing an older backup) so the user's
    /// data is not lost when the defaults are saved later.
    pub fn load_or_default(path: &Path) -> AppConfig {
        match AppConfig::load_from(path) {
            Ok(config) => config,
            Err(HostError::Io(e)) if e.kind() == io::ErrorKind::NotFound => AppConfig::default(),
            Err(HostError::Config(msg)) => {
                let backup = backup_path(path);
                match fs::rename(path, &backup) {
                    Ok(()) => log::warn!(
                        "configuration is invalid ({msg}); moved it to {} and using defaults",
                        backup.display()
                    ),
                    Err(e) => log::warn!(
                        "configuration is invalid ({msg}); could not back it up ({e}); using defaults"
                    ),
                }
                AppConfig::default()
            }
            Err(e) => {
                log::warn!("could not read {}: {e}; using defaults", path.display());
                AppConfig::default()
            }
        }
    }

    /// Save to [`AppConfig::path`].
    pub fn save(&self) -> Result<(), HostError> {
        self.save_to(&AppConfig::path())
    }

    /// Save to `path` atomically: the TOML is written to a temporary file
    /// next to it, flushed to disk and renamed over `path`. Missing parent
    /// directories are created.
    pub fn save_to(&self, path: &Path) -> Result<(), HostError> {
        let text = toml::to_string_pretty(self).map_err(|e| {
            HostError::Config(format!("could not serialize the configuration: {e}"))
        })?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let tmp = sibling_with_suffix(path, ".tmp");
        let result = (|| -> io::Result<()> {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        Ok(result?)
    }

    /// `<config dir>/Sprint/config.toml` (on Windows
    /// `%APPDATA%\Sprint\config.toml`), or `sprint-config.toml` in the
    /// working directory when the platform has no configuration directory.
    pub fn path() -> PathBuf {
        directories::BaseDirs::new().map_or_else(
            || PathBuf::from("sprint-config.toml"),
            |dirs| dirs.config_dir().join("Sprint").join("config.toml"),
        )
    }

    /// The active profile (the last one if the index is out of range, a
    /// default profile if there are none).
    pub fn active(&self) -> &PrinterProfile {
        let n = self.profiles.len();
        if n == 0 {
            return &DEFAULT_PROFILE;
        }
        &self.profiles[self.active_profile.min(n - 1)]
    }

    /// Mutable access to the active profile (one is created if there are
    /// none).
    pub fn active_mut(&mut self) -> &mut PrinterProfile {
        if self.profiles.is_empty() {
            self.profiles.push(PrinterProfile::default());
        }
        let index = self.active_profile.min(self.profiles.len() - 1);
        self.active_profile = index;
        &mut self.profiles[index]
    }

    /// Put `path` at the front of the recent files (removing an older
    /// entry for it), keeping at most [`MAX_RECENT_FILES`].
    pub fn push_recent(&mut self, path: PathBuf) {
        self.recent_files.retain(|p| p != &path);
        self.recent_files.insert(0, path);
        self.recent_files.truncate(MAX_RECENT_FILES);
    }

    /// Repair values a hand-edited file may have broken.
    fn normalized(mut self) -> AppConfig {
        if self.profiles.is_empty() {
            self.profiles.push(PrinterProfile::default());
        }
        self.active_profile = self.active_profile.min(self.profiles.len() - 1);
        self.recent_files.truncate(MAX_RECENT_FILES);
        if self.console_max_lines == 0 {
            self.console_max_lines = DEFAULT_CONSOLE_LINES;
        }
        self.console_max_lines = self.console_max_lines.clamp(100, 1_000_000);
        self
    }
}

/// `config.toml` -> `config.toml.bak`.
fn backup_path(path: &Path) -> PathBuf {
    sibling_with_suffix(path, ".bak")
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "config.toml".into());
    name.push(suffix);
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_contract() {
        let o = ConnectOptions::default();
        assert_eq!(o.checksum, ChecksumMode::Always);
        assert_eq!(o.ok_timeout_secs, 10.0);
        assert_eq!(o.handshake_timeout_secs, 15.0);
        assert_eq!(o.temp_poll_secs, 2.0);
        assert!(o.query_on_connect && !o.reset_on_connect);
        assert_eq!(o.max_in_flight, 1);
        assert_eq!(o.autoreport_interval_secs, 2);

        let p = PrinterProfile::default();
        assert_eq!(p.connection, ConnectTarget::Simulator(Default::default()));
        assert!(!p.auto_connect);
        assert_eq!(p.bed_size, [220.0, 220.0]);
        assert_eq!(p.max_z, 250.0);
        assert_eq!(p.origin, BedOrigin::FrontLeft);
        assert_eq!(p.extruders, 1);
        assert!(p.heated_bed && !p.heated_chamber);
        assert_eq!(p.filament_diameter, 1.75);
        assert_eq!(p.nozzle_diameter, 0.4);
        let presets: Vec<(&str, f32, f32)> = p
            .temp_presets
            .iter()
            .map(|t| (t.name.as_str(), t.hotend, t.bed))
            .collect();
        assert_eq!(
            presets,
            [
                ("PLA", 205.0, 60.0),
                ("PETG", 240.0, 80.0),
                ("ABS", 250.0, 100.0),
                ("TPU", 225.0, 50.0)
            ]
        );
        assert_eq!(
            (p.jog_feedrate_xy, p.jog_feedrate_z, p.extrude_feedrate),
            (3000.0, 600.0, 300.0)
        );
        assert_eq!(AppConfig::default().console_max_lines, 5000);
    }

    #[test]
    fn sanitizing_clamps_garbage() {
        let o = ConnectOptions {
            ok_timeout_secs: f32::NAN,
            handshake_timeout_secs: -1.0,
            temp_poll_secs: f32::INFINITY,
            max_in_flight: 0,
            autoreport_interval_secs: 1000,
            ..ConnectOptions::default()
        }
        .sanitized();
        assert_eq!(o.ok_timeout_secs, 10.0);
        assert_eq!(o.handshake_timeout_secs, 0.1);
        assert_eq!(o.temp_poll_secs, 2.0);
        assert_eq!(o.max_in_flight, 1);
        assert_eq!(o.autoreport_interval_secs, 60);
        let o = ConnectOptions {
            max_in_flight: 1000,
            ..ConnectOptions::default()
        };
        assert_eq!(o.sanitized().max_in_flight, MAX_IN_FLIGHT_LIMIT);
    }

    #[test]
    fn geometry_and_parking() {
        let mut p = PrinterProfile::default();
        assert_eq!(p.build_volume(), ([0.0; 3], [220.0, 220.0, 250.0]));
        assert_eq!(p.park_position(), (10.0, 210.0));
        p.origin = BedOrigin::Center;
        p.bed_size = [200.0, 300.0];
        assert_eq!(
            p.build_volume(),
            ([-100.0, -150.0, 0.0], [100.0, 150.0, 250.0])
        );
        assert_eq!(p.park_position(), (-90.0, 140.0));
        p.bed_size = [f32::NAN, 4.0];
        p.origin = BedOrigin::FrontLeft;
        assert_eq!(p.park_position(), (10.0, 2.0));
        let state = MachineState::new();
        let ctx = p.script_context(&state);
        assert_eq!((ctx.max_x, ctx.max_y, ctx.max_z), (220.0, 4.0, 250.0));
    }

    #[test]
    fn active_profile_access_is_total() {
        let mut c = AppConfig {
            profiles: Vec::new(),
            active_profile: 7,
            ..AppConfig::default()
        };
        assert_eq!(c.active().name, "Virtual printer");
        c.active_mut().name = "Mine".into();
        assert_eq!(c.profiles.len(), 1);
        assert_eq!(c.active_profile, 0);
        c.active_profile = 99;
        assert_eq!(c.active().name, "Mine");
    }

    #[test]
    fn recent_files_are_deduplicated_and_capped() {
        let mut c = AppConfig::default();
        for i in 0..20 {
            c.push_recent(PathBuf::from(format!("f{i}.gcode")));
        }
        c.push_recent(PathBuf::from("f15.gcode"));
        assert_eq!(c.recent_files.len(), MAX_RECENT_FILES);
        assert_eq!(c.recent_files[0], PathBuf::from("f15.gcode"));
        assert_eq!(c.recent_files[1], PathBuf::from("f19.gcode"));
        assert_eq!(
            c.recent_files
                .iter()
                .filter(|p| p.as_os_str() == "f15.gcode")
                .count(),
            1
        );
    }

    #[test]
    fn toml_round_trip_of_every_target_kind() {
        let mut c = AppConfig::default();
        c.profiles.push(PrinterProfile {
            name: "Serial".into(),
            connection: ConnectTarget::Serial {
                port: "COM3".into(),
                baud: 250_000,
            },
            kinematics: Some(KinematicLimits::default()),
            ..PrinterProfile::default()
        });
        c.profiles.push(PrinterProfile {
            name: "Net".into(),
            connection: ConnectTarget::Tcp {
                host: "::1".into(),
                port: 2323,
            },
            options: ConnectOptions {
                checksum: ChecksumMode::PrintOnly,
                max_in_flight: 4,
                ..ConnectOptions::default()
            },
            origin: BedOrigin::Center,
            ..PrinterProfile::default()
        });
        c.macros[0].color = Some([255, 0, 10]);
        c.active_profile = 2;
        c.push_recent(PathBuf::from("C:/prints/cube.gcode"));
        let text = toml::to_string_pretty(&c).unwrap();
        let back: AppConfig = toml::from_str(&text).unwrap();
        assert_eq!(back, c, "{text}");
    }

    #[test]
    fn classic_jerk_kinematics_survive_a_round_trip() {
        let classic = KinematicLimits {
            junction_deviation: None,
            jerk: [10.0, 10.0, 0.3, 4.5],
            ..KinematicLimits::default()
        };
        let mut c = AppConfig::default();
        c.active_mut().kinematics = Some(classic);
        let text = toml::to_string_pretty(&c).unwrap();
        assert!(text.contains("junction_deviation = false"), "{text}");
        let back: AppConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.active().kinematics, Some(classic));

        let custom = KinematicLimits {
            junction_deviation: Some(0.02),
            ..KinematicLimits::default()
        };
        c.active_mut().kinematics = Some(custom);
        let back: AppConfig = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(back.active().kinematics, Some(custom));

        // Files written before (no key, a number) keep their meaning.
        let parse = |table: &str| -> Option<KinematicLimits> {
            let text = format!("[[profiles]]\nname = \"p\"\n[profiles.kinematics]\n{table}\n");
            toml::from_str::<AppConfig>(&text).unwrap().profiles[0].kinematics
        };
        assert_eq!(
            parse("print_accel = 900.0"),
            Some(KinematicLimits {
                print_accel: 900.0,
                ..KinematicLimits::default()
            })
        );
        assert_eq!(
            parse("junction_deviation = 0.05")
                .unwrap()
                .junction_deviation,
            Some(0.05)
        );
        assert_eq!(
            parse("junction_deviation = 0").unwrap().junction_deviation,
            Some(0.0)
        );
        assert_eq!(
            parse("junction_deviation = true")
                .unwrap()
                .junction_deviation,
            KinematicLimits::default().junction_deviation
        );
        assert!(
            toml::from_str::<AppConfig>(
                "[[profiles]]\n[profiles.kinematics]\njunction_deviation = \"x\"\n"
            )
            .is_err()
        );
    }

    #[test]
    fn missing_fields_take_defaults() {
        let c: AppConfig =
            toml::from_str("console_max_lines = 900\n[[profiles]]\nname = \"X\"\n").unwrap();
        assert_eq!(c.console_max_lines, 900);
        assert_eq!(c.profiles.len(), 1);
        assert_eq!(c.profiles[0].name, "X");
        assert_eq!(c.profiles[0].bed_size, [220.0, 220.0]);
        assert_eq!(c.profiles[0].pause_script, DEFAULT_PAUSE_SCRIPT);
        let empty: AppConfig = toml::from_str("").unwrap();
        assert_eq!(empty, AppConfig::default());
    }

    #[test]
    fn backup_names() {
        assert_eq!(
            backup_path(Path::new("dir/config.toml")),
            PathBuf::from("dir/config.toml.bak")
        );
        assert_eq!(
            sibling_with_suffix(Path::new("config.toml"), ".tmp"),
            PathBuf::from("config.toml.tmp")
        );
    }
}
