//! Firmware identification: the M115 `FIRMWARE_NAME:` line, firmware flavor
//! detection and descriptions of the `Cap:` capability flags.

use serde::{Deserialize, Serialize};

/// Firmware family, detected from the M115 reply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FirmwareFlavor {
    /// Not (yet) identified.
    #[default]
    Unknown,
    /// Marlin 1.x / 2.x (and most vendor forks of it).
    Marlin,
    /// Prusa-Firmware (Marlin 1.0 derivative used by Original Prusa printers).
    Prusa,
    /// Klipper (G-code served by the klippy host).
    Klipper,
    /// RepRapFirmware (Duet boards).
    RepRapFirmware,
    /// Smoothieware.
    Smoothieware,
    /// Repetier-Firmware.
    Repetier,
    /// Sprinter.
    Sprinter,
    /// Teacup.
    Teacup,
    /// Grbl (CNC firmware, occasionally used on printers/plotters).
    Grbl,
}

impl FirmwareFlavor {
    /// Display name of the flavor.
    pub fn name(&self) -> &'static str {
        match self {
            FirmwareFlavor::Unknown => "Unknown",
            FirmwareFlavor::Marlin => "Marlin",
            FirmwareFlavor::Prusa => "Prusa",
            FirmwareFlavor::Klipper => "Klipper",
            FirmwareFlavor::RepRapFirmware => "RepRapFirmware",
            FirmwareFlavor::Smoothieware => "Smoothieware",
            FirmwareFlavor::Repetier => "Repetier",
            FirmwareFlavor::Sprinter => "Sprinter",
            FirmwareFlavor::Teacup => "Teacup",
            FirmwareFlavor::Grbl => "Grbl",
        }
    }

    /// Whether the firmware answers `M503` with its settings. False for
    /// Klipper (configuration lives in `printer.cfg`), Teacup and Grbl.
    pub fn supports_m503(&self) -> bool {
        !matches!(
            self,
            FirmwareFlavor::Klipper | FirmwareFlavor::Teacup | FirmwareFlavor::Grbl
        )
    }

    /// Detect the flavor from a firmware name or banner, case-insensitively
    /// ("Prusa-Firmware 3.13.2 based on Marlin" is Prusa, not Marlin).
    pub fn detect(name: &str) -> FirmwareFlavor {
        let n = name.to_ascii_lowercase();
        // Order matters: forks mention their parent ("based on Marlin").
        const RULES: [(&str, FirmwareFlavor); 10] = [
            ("prusa", FirmwareFlavor::Prusa),
            ("klipper", FirmwareFlavor::Klipper),
            ("reprapfirmware", FirmwareFlavor::RepRapFirmware),
            ("rrf", FirmwareFlavor::RepRapFirmware),
            ("smoothie", FirmwareFlavor::Smoothieware),
            ("repetier", FirmwareFlavor::Repetier),
            ("teacup", FirmwareFlavor::Teacup),
            ("sprinter", FirmwareFlavor::Sprinter),
            ("grbl", FirmwareFlavor::Grbl),
            ("marlin", FirmwareFlavor::Marlin),
        ];
        RULES
            .iter()
            .find(|(needle, _)| n.contains(needle))
            .map_or(FirmwareFlavor::Unknown, |(_, flavor)| *flavor)
    }
}

impl std::fmt::Display for FirmwareFlavor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// What the firmware reported about itself in its M115 reply.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FirmwareInfo {
    /// The `FIRMWARE_NAME` value, e.g. "Marlin 2.1.2.1 (Jul 29 2023 12:00:00)".
    pub name: String,
    /// Version: `FIRMWARE_VERSION` when present, otherwise extracted from
    /// the name ("2.1.2.1", "3.13.2", "1.0.3").
    pub version: Option<String>,
    /// Detected firmware family.
    pub flavor: FirmwareFlavor,
    /// `SOURCE_CODE_URL` / `FIRMWARE_URL` (percent-escapes decoded).
    pub source_url: Option<String>,
    /// `PROTOCOL_VERSION`.
    pub protocol_version: Option<String>,
    /// `MACHINE_TYPE`.
    pub machine_type: Option<String>,
    /// `EXTRUDER_COUNT`.
    pub extruder_count: Option<u8>,
    /// `UUID`.
    pub uuid: Option<String>,
    /// Every `KEY:value` pair on the line, in order.
    pub fields: Vec<(String, String)>,
    /// The line as received.
    pub raw: String,
}

impl FirmwareInfo {
    /// Value of a field by key (case-insensitive), e.g. `get("MACHINE_TYPE")`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }
}

/// True when `bytes[start..]` begins an M115 key: an uppercase letter
/// followed by uppercase letters, digits, `_` or `-`, then `:`. Returns the
/// key length (excluding the colon).
fn key_at(bytes: &[u8], start: usize) -> Option<usize> {
    let first = *bytes.get(start)?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    let mut end = start + 1;
    while let Some(&b) = bytes.get(end) {
        if b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_' || b == b'-' {
            end += 1;
        } else {
            break;
        }
    }
    // Single-letter keys would split values like "Prusa i3 X:..."; real M115
    // keys are always longer.
    (end - start >= 2 && bytes.get(end) == Some(&b':')).then_some(end - start)
}

/// Split an M115 line into `KEY:value` pairs. Keys are uppercase tokens
/// followed by `:` at the start of the line or after whitespace / a comma;
/// values run until the next key (so they may contain spaces and colons).
fn split_fields(line: &str) -> Vec<(String, String)> {
    let bytes = line.as_bytes();
    // (key start, key len)
    let mut keys: Vec<(usize, usize)> = Vec::new();
    for i in 0..bytes.len() {
        let at_boundary = i == 0 || matches!(bytes[i - 1], b' ' | b'\t' | b',');
        if at_boundary && let Some(len) = key_at(bytes, i) {
            keys.push((i, len));
        }
    }
    let mut fields = Vec::with_capacity(keys.len());
    for (idx, &(start, len)) in keys.iter().enumerate() {
        let value_start = start + len + 1;
        let value_end = keys.get(idx + 1).map_or(line.len(), |&(next, _)| next);
        let key = &line[start..start + len];
        let value = line
            .get(value_start..value_end)
            .unwrap_or("")
            .trim()
            .trim_end_matches(',')
            .trim();
        fields.push((key.to_string(), value.to_string()));
    }
    fields
}

/// Decode `%XX` escapes (Smoothieware reports `http%3A//smoothieware.org`).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && hex.bytes().all(|b| b.is_ascii_hexdigit())
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// Extract a version number from a firmware name: the first token (split on
/// whitespace and `_`) before any `(` that contains a digit and a `.`, with a
/// leading `v`/`V` removed.
fn version_from_name(name: &str) -> Option<String> {
    let head = name.split('(').next().unwrap_or("");
    head.split(|c: char| c.is_whitespace() || c == '_')
        .map(|t| t.trim_matches(|c: char| c == ',' || c == ';'))
        .find(|t| t.contains('.') && t.bytes().any(|b| b.is_ascii_digit()))
        .map(|t| {
            let stripped = t
                .strip_prefix(['v', 'V'])
                .filter(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
            stripped.unwrap_or(t).to_string()
        })
}

/// Parse an M115 `FIRMWARE_NAME:...` line. Values may contain spaces and
/// colons (URLs, dates); a value ends only where the next `KEY:` begins.
/// A leading `ok ` or `echo:` is tolerated. Returns `None` for lines without
/// a `FIRMWARE_NAME` field.
pub fn parse_firmware_line(line: &str) -> Option<FirmwareInfo> {
    let raw = line.trim();
    let mut text = raw;
    if let Some(rest) = text.strip_prefix("ok ") {
        text = rest.trim_start();
    }
    if let Some(rest) = text.strip_prefix("echo:") {
        text = rest.trim_start();
    }
    if !text.starts_with("FIRMWARE_NAME:") {
        return None;
    }
    let fields = split_fields(text);
    let get = |key: &str| {
        fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .filter(|v| !v.is_empty())
    };
    let name = get("FIRMWARE_NAME").unwrap_or_default();
    let version = get("FIRMWARE_VERSION").or_else(|| version_from_name(&name));
    let flavor = FirmwareFlavor::detect(&name);
    let source_url = get("SOURCE_CODE_URL")
        .or_else(|| get("X-SOURCE_CODE_URL"))
        .or_else(|| get("FIRMWARE_URL"))
        .map(|u| percent_decode(&u));
    let extruder_count = get("EXTRUDER_COUNT").and_then(|v| v.parse().ok());
    Some(FirmwareInfo {
        version,
        flavor,
        source_url,
        protocol_version: get("PROTOCOL_VERSION"),
        machine_type: get("MACHINE_TYPE"),
        extruder_count,
        uuid: get("UUID"),
        name,
        fields,
        raw: raw.to_string(),
    })
}

/// Known `Cap:` capability names (Marlin 2.1, Prusa) with descriptions.
const CAPABILITIES: &[(&str, &str)] = &[
    (
        "SERIAL_XON_XOFF",
        "Software flow control (XON/XOFF) on the serial link",
    ),
    (
        "BINARY_FILE_TRANSFER",
        "Binary protocol for fast file uploads to the SD card",
    ),
    (
        "EEPROM",
        "Settings can be saved to persistent storage with M500",
    ),
    (
        "VOLUMETRIC",
        "Volumetric extrusion (M200 filament diameter)",
    ),
    (
        "AUTOREPORT_POS",
        "Periodic position reports on request (M154)",
    ),
    (
        "AUTOREPORT_POSITION",
        "Periodic position reports on request (M155 / M154)",
    ),
    (
        "AUTOREPORT_TEMP",
        "Periodic temperature reports on request (M155), no polling needed",
    ),
    (
        "AUTOREPORT_FANS",
        "Periodic fan speed reports on request (M123)",
    ),
    ("PROGRESS", "Accepts print progress from the host (M73)"),
    ("PRINT_JOB", "Print job timer control (M75, M76, M77)"),
    ("AUTOLEVEL", "Automatic bed leveling (G29)"),
    ("RUNOUT", "Filament runout sensor (M412)"),
    ("Z_PROBE", "Z probe for single-point probing (G30)"),
    ("LEVELING_DATA", "Can report the bed leveling mesh (M420 V)"),
    (
        "BUILD_PERCENT",
        "Shows print progress percentage on the display (M73)",
    ),
    ("SOFTWARE_POWER", "Power supply switching (M80 / M81)"),
    ("TOGGLE_LIGHTS", "Case light on/off control (M355)"),
    (
        "CASE_LIGHT_BRIGHTNESS",
        "Case light brightness control (M355 P)",
    ),
    (
        "EMERGENCY_PARSER",
        "Emergency commands (M112, M108, M410, M876) act immediately, bypassing the queue",
    ),
    (
        "HOST_ACTION_COMMANDS",
        "Sends //action: notifications to the host (pause, resume, cancel...)",
    ),
    (
        "PROMPT_SUPPORT",
        "Host prompts with buttons (//action:prompt_*) answered by M876",
    ),
    ("SDCARD", "SD card / media support"),
    (
        "MULTI_VOLUME",
        "Several storage volumes (SD card and USB drive)",
    ),
    ("REPEAT", "Repeat markers in G-code (M808)"),
    (
        "SD_WRITE",
        "Files can be written to the SD card (M28 / M29)",
    ),
    (
        "AUTOREPORT_SD_STATUS",
        "Periodic SD print progress reports on request (M27 S)",
    ),
    (
        "LONG_FILENAME",
        "Reports long file names in listings (M20 L) and M33",
    ),
    (
        "LFN_WRITE",
        "Files with long names can be created on the SD card",
    ),
    (
        "CUSTOM_FIRMWARE_UPLOAD",
        "Firmware updates can be uploaded and applied from the host",
    ),
    (
        "EXTENDED_M20",
        "Extended SD listings with file sizes and timestamps (M20 L T)",
    ),
    (
        "THERMAL_PROTECTION",
        "Thermal runaway protection is enabled for the heaters",
    ),
    (
        "MOTION_MODES",
        "Modal motion such as G80 cancel / G5 Bezier and canned cycles",
    ),
    ("ARCS", "Arc moves (G2 / G3)"),
    ("BABYSTEPPING", "Live Z (and XY) babystepping (M290)"),
    ("CHAMBER_TEMPERATURE", "Reports a chamber temperature"),
    ("COOLER_TEMPERATURE", "Reports a laser cooler temperature"),
    ("MEATPACK", "MeatPack compression of the G-code stream"),
    ("CONFIG_EXPORT", "Can export its configuration (M360)"),
    ("PAREN_COMMENTS", "Accepts (parenthesis) comments in G-code"),
    ("GCODE_QUOTED_STRINGS", "Accepts quoted string parameters"),
    ("SINGLENOZZLE", "Several extruders share one nozzle"),
    ("SPINDLE", "Spindle / laser control (M3, M4, M5)"),
    ("LASER", "Laser control (M3, M4, M5)"),
    ("FAN_COUNT", "Number of controllable fans"),
    ("PRUSA_MMU2", "Prusa Multi Material Unit 2 attached"),
    ("MMU", "Multi material unit support"),
    ("HOST_RESCUE", "Host-assisted power-loss recovery"),
    (
        "POWER_LOSS_RECOVERY",
        "Can resume a print after a power loss (M413)",
    ),
    ("INPUT_SHAPING", "Input shaping to reduce ringing (M593)"),
    ("LINEAR_ADVANCE", "Linear pressure advance (M900)"),
];

/// Human description of a known `Cap:` capability name (case-insensitive).
pub fn capability_description(name: &str) -> Option<&'static str> {
    CAPABILITIES
        .iter()
        .find(|(cap, _)| cap.eq_ignore_ascii_case(name.trim()))
        .map(|(_, desc)| *desc)
}

/// All known capability names with descriptions, in a stable order.
pub fn known_capabilities() -> &'static [(&'static str, &'static str)] {
    CAPABILITIES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marlin_212() {
        let line = "FIRMWARE_NAME:Marlin 2.1.2.1 (Jul 29 2023 12:00:00) SOURCE_CODE_URL:github.com/MarlinFirmware/Marlin PROTOCOL_VERSION:1.0 MACHINE_TYPE:Ender-3 V2 EXTRUDER_COUNT:1 UUID:cede2a2f-41a2-4748-9b12-c55c62f367ff";
        let info = parse_firmware_line(line).unwrap();
        assert_eq!(info.name, "Marlin 2.1.2.1 (Jul 29 2023 12:00:00)");
        assert_eq!(info.version.as_deref(), Some("2.1.2.1"));
        assert_eq!(info.flavor, FirmwareFlavor::Marlin);
        assert_eq!(
            info.source_url.as_deref(),
            Some("github.com/MarlinFirmware/Marlin")
        );
        assert_eq!(info.protocol_version.as_deref(), Some("1.0"));
        assert_eq!(info.machine_type.as_deref(), Some("Ender-3 V2"));
        assert_eq!(info.extruder_count, Some(1));
        assert_eq!(
            info.uuid.as_deref(),
            Some("cede2a2f-41a2-4748-9b12-c55c62f367ff")
        );
        assert_eq!(info.fields.len(), 6);
        assert_eq!(info.raw, line);
        assert_eq!(info.get("machine_type"), Some("Ender-3 V2"));
    }

    #[test]
    fn prusa() {
        let info = parse_firmware_line("FIRMWARE_NAME:Prusa-Firmware 3.13.2 based on Marlin FIRMWARE_URL:https://github.com/prusa3d/Prusa-Firmware PROTOCOL_VERSION:1.0 MACHINE_TYPE:Prusa i3 MK3S EXTRUDER_COUNT:1 UUID:00000000-0000-0000-0000-000000000000").unwrap();
        assert_eq!(info.flavor, FirmwareFlavor::Prusa);
        assert_eq!(info.version.as_deref(), Some("3.13.2"));
        assert_eq!(info.name, "Prusa-Firmware 3.13.2 based on Marlin");
        assert_eq!(
            info.source_url.as_deref(),
            Some("https://github.com/prusa3d/Prusa-Firmware")
        );
        assert_eq!(info.machine_type.as_deref(), Some("Prusa i3 MK3S"));
    }

    #[test]
    fn klipper_rrf_smoothie_repetier() {
        let k = parse_firmware_line("FIRMWARE_NAME:Klipper FIRMWARE_VERSION:v0.12.0-123-gabcdef")
            .unwrap();
        assert_eq!(k.flavor, FirmwareFlavor::Klipper);
        assert_eq!(k.version.as_deref(), Some("v0.12.0-123-gabcdef"));
        assert!(!k.flavor.supports_m503());

        let r = parse_firmware_line("FIRMWARE_NAME: RepRapFirmware for Duet 3 MB6HC FIRMWARE_VERSION: 3.4.5 ELECTRONICS: Duet 3 MB6HC v1.01 FIRMWARE_DATE: 2022-11-30 19:36:12").unwrap();
        assert_eq!(r.flavor, FirmwareFlavor::RepRapFirmware);
        assert_eq!(r.name, "RepRapFirmware for Duet 3 MB6HC");
        assert_eq!(r.version.as_deref(), Some("3.4.5"));
        assert_eq!(r.get("ELECTRONICS"), Some("Duet 3 MB6HC v1.01"));
        assert_eq!(r.get("FIRMWARE_DATE"), Some("2022-11-30 19:36:12"));

        let s = parse_firmware_line("FIRMWARE_NAME:Smoothieware, FIRMWARE_URL:http%3A//smoothieware.org, X-SOURCE_CODE_URL:https://github.com/Smoothieware/Smoothieware, FIRMWARE_VERSION:edge-1234abc, X-FIRMWARE_BUILD_DATE:Jan  1 2020 00:00:00, X-SYSTEM_CLOCK:100MHz, X-AXES:5").unwrap();
        assert_eq!(s.flavor, FirmwareFlavor::Smoothieware);
        assert_eq!(s.name, "Smoothieware");
        assert_eq!(s.version.as_deref(), Some("edge-1234abc"));
        assert_eq!(
            s.source_url.as_deref(),
            Some("https://github.com/Smoothieware/Smoothieware")
        );
        assert_eq!(s.get("X-FIRMWARE_BUILD_DATE"), Some("Jan  1 2020 00:00:00"));
        assert_eq!(s.get("X-AXES"), Some("5"));
        assert_eq!(s.get("FIRMWARE_URL"), Some("http%3A//smoothieware.org"));

        let rep = parse_firmware_line("FIRMWARE_NAME:Repetier_1.0.3 COMPILED:Jan 1 2020 FIRMWARE_URL:https://github.com/repetier/Repetier-Firmware/ PROTOCOL_VERSION:1.0 MACHINE_TYPE:Mendel EXTRUDER_COUNT:1 REPETIER_PROTOCOL:3").unwrap();
        assert_eq!(rep.flavor, FirmwareFlavor::Repetier);
        assert_eq!(rep.version.as_deref(), Some("1.0.3"));
        assert_eq!(rep.get("COMPILED"), Some("Jan 1 2020"));
        assert_eq!(rep.get("REPETIER_PROTOCOL"), Some("3"));
    }

    #[test]
    fn older_marlin_and_forks() {
        let m11 = parse_firmware_line("FIRMWARE_NAME:Marlin 1.1.9 (Github) SOURCE_CODE_URL:https://github.com/MarlinFirmware/Marlin PROTOCOL_VERSION:1.0 MACHINE_TYPE:3D Printer EXTRUDER_COUNT:1 UUID:cede2a2f-41a2-4748-9b12-c55c62f367ff").unwrap();
        assert_eq!(m11.version.as_deref(), Some("1.1.9"));
        assert_eq!(m11.name, "Marlin 1.1.9 (Github)");
        let creality =
            parse_firmware_line("FIRMWARE_NAME:Marlin V1.0.1 SOURCE_CODE_URL:github.com").unwrap();
        assert_eq!(creality.version.as_deref(), Some("1.0.1"));
        let bugfix =
            parse_firmware_line("FIRMWARE_NAME:Marlin bugfix-2.1.x (Jan  1 2024 00:00:00)")
                .unwrap();
        assert_eq!(bugfix.version.as_deref(), Some("bugfix-2.1.x"));
        let ok = parse_firmware_line("ok FIRMWARE_NAME:Klipper FIRMWARE_VERSION:v0.11.0").unwrap();
        assert_eq!(ok.flavor, FirmwareFlavor::Klipper);
    }

    #[test]
    fn rejects_other_lines() {
        assert!(parse_firmware_line("").is_none());
        assert!(parse_firmware_line("ok").is_none());
        assert!(parse_firmware_line("Cap:EEPROM:1").is_none());
        assert!(parse_firmware_line("echo:Marlin 2.1.2").is_none());
        let empty = parse_firmware_line("FIRMWARE_NAME:").unwrap();
        assert_eq!(empty.name, "");
        assert_eq!(empty.version, None);
        assert_eq!(empty.flavor, FirmwareFlavor::Unknown);
    }

    #[test]
    fn flavor_names_and_detection() {
        assert_eq!(
            FirmwareFlavor::detect("Grbl 1.1h ['$' for help]"),
            FirmwareFlavor::Grbl
        );
        assert_eq!(FirmwareFlavor::detect("Teacup"), FirmwareFlavor::Teacup);
        assert_eq!(FirmwareFlavor::detect("Sprinter"), FirmwareFlavor::Sprinter);
        assert_eq!(FirmwareFlavor::detect("whatever"), FirmwareFlavor::Unknown);
        assert_eq!(FirmwareFlavor::RepRapFirmware.to_string(), "RepRapFirmware");
        assert!(FirmwareFlavor::Marlin.supports_m503());
        assert!(FirmwareFlavor::Unknown.supports_m503());
    }

    #[test]
    fn capabilities_cover_marlin_21() {
        for cap in [
            "SERIAL_XON_XOFF",
            "BINARY_FILE_TRANSFER",
            "EEPROM",
            "VOLUMETRIC",
            "AUTOREPORT_POS",
            "AUTOREPORT_TEMP",
            "PROGRESS",
            "PRINT_JOB",
            "AUTOLEVEL",
            "RUNOUT",
            "Z_PROBE",
            "LEVELING_DATA",
            "BUILD_PERCENT",
            "SOFTWARE_POWER",
            "TOGGLE_LIGHTS",
            "CASE_LIGHT_BRIGHTNESS",
            "EMERGENCY_PARSER",
            "HOST_ACTION_COMMANDS",
            "PROMPT_SUPPORT",
            "SDCARD",
            "MULTI_VOLUME",
            "REPEAT",
            "SD_WRITE",
            "AUTOREPORT_SD_STATUS",
            "LONG_FILENAME",
            "LFN_WRITE",
            "CUSTOM_FIRMWARE_UPLOAD",
            "EXTENDED_M20",
            "THERMAL_PROTECTION",
            "MOTION_MODES",
            "ARCS",
            "BABYSTEPPING",
            "CHAMBER_TEMPERATURE",
            "COOLER_TEMPERATURE",
            "MEATPACK",
            "CONFIG_EXPORT",
            "PAREN_COMMENTS",
            "GCODE_QUOTED_STRINGS",
        ] {
            assert!(capability_description(cap).is_some(), "{cap}");
        }
        assert!(capability_description("autoreport_temp").is_some());
        assert!(capability_description("NOT_A_CAP").is_none());
        let names: std::collections::HashSet<_> =
            known_capabilities().iter().map(|c| c.0).collect();
        assert_eq!(
            names.len(),
            known_capabilities().len(),
            "duplicate capability"
        );
    }
}
