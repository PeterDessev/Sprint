//! Parsing and display of connection targets.

use std::fmt;

use sprint_sim::SimConfig;

use super::{ConnectTarget, DEFAULT_BAUD};
use crate::error::HostError;

impl ConnectTarget {
    /// Parse a connection target.
    ///
    /// Accepted forms:
    ///
    /// * serial ports: `COM3`, `com3@115200`, `/dev/ttyUSB0@250000`,
    ///   `\\.\COM10`; the baud rate defaults to 115200, and `@0` or
    ///   `@auto` asks for auto-detection;
    /// * TCP: `tcp://host:port`, `tcp://[::1]:23` (IPv6 in brackets);
    /// * the virtual printer: `sim`, `sim://` or
    ///   `sim://?time_scale=10&extruders=2&checksum_error_rate=0.05`, where
    ///   the query parameters set [`SimConfig`] fields (`machine_name`,
    ///   `extruders`, `heated_bed`, `heated_chamber`, `build_volume`
    ///   (`x,y,z`), `time_scale`, `instant_moves`, `boot_delay_ms`,
    ///   `checksum_error_rate`, `drop_ok_rate`, `planner_buffer`,
    ///   `rx_buffer`, `auto_report_temps`, `auto_report_position`,
    ///   `advanced_ok`, `mesh_size` (`5x5`), `ambient`, `seed`). Unknown
    ///   parameters are an error.
    pub fn parse(s: &str) -> Result<ConnectTarget, HostError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(HostError::InvalidTarget("empty connection target".into()));
        }
        if let Some(rest) = strip_prefix_ci(s, "sim") {
            return parse_sim(s, rest);
        }
        if let Some(rest) = strip_prefix_ci(s, "tcp://") {
            return parse_tcp(s, rest);
        }
        if let Some((scheme, _)) = s.split_once("://") {
            return Err(HostError::InvalidTarget(format!(
                "unknown scheme \"{scheme}://\" in \"{s}\" (use tcp://host:port, sim or a serial port)"
            )));
        }
        parse_serial(s)
    }

    /// A short human-readable description: `COM3 @ 115200`,
    /// `/dev/ttyUSB0 @ auto`, `tcp://host:port`, `Virtual printer`.
    pub fn label(&self) -> String {
        match self {
            ConnectTarget::Serial { port, baud } => {
                if *baud == 0 {
                    format!("{port} @ auto")
                } else {
                    format!("{port} @ {baud}")
                }
            }
            ConnectTarget::Tcp { host, port } => format!("tcp://{}:{port}", bracket_host(host)),
            ConnectTarget::Simulator(_) => "Virtual printer".to_string(),
        }
    }

    /// True for the virtual printer.
    pub fn is_simulator(&self) -> bool {
        matches!(self, ConnectTarget::Simulator(_))
    }
}

/// The parseable form: `COM3@115200`, `COM3@auto`, `tcp://host:port`,
/// `sim` or `sim://?key=value&...` with the simulator fields that differ
/// from their defaults (`sd_files` cannot be expressed and is left out).
/// `ConnectTarget::parse(&t.to_string())` gives `t` back for serial and TCP
/// targets and for simulators without custom SD files.
impl fmt::Display for ConnectTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectTarget::Serial { port, baud } => {
                if *baud == 0 {
                    write!(f, "{port}@auto")
                } else {
                    write!(f, "{port}@{baud}")
                }
            }
            ConnectTarget::Tcp { host, port } => write!(f, "tcp://{}:{port}", bracket_host(host)),
            ConnectTarget::Simulator(config) => {
                let params = sim_params(config);
                if params.is_empty() {
                    write!(f, "sim")
                } else {
                    write!(f, "sim://?{}", params.join("&"))
                }
            }
        }
    }
}

fn bracket_host(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

fn invalid(msg: impl Into<String>) -> HostError {
    HostError::InvalidTarget(msg.into())
}

fn parse_serial(s: &str) -> Result<ConnectTarget, HostError> {
    let (port, baud) = match s.rsplit_once('@') {
        Some((port, baud)) => {
            let baud = baud.trim();
            let baud = if baud.eq_ignore_ascii_case("auto") {
                0
            } else {
                baud.parse::<u32>()
                    .map_err(|_| invalid(format!("invalid baud rate \"{baud}\" in \"{s}\"")))?
            };
            (port.trim(), baud)
        }
        None => (s, DEFAULT_BAUD),
    };
    if port.is_empty() {
        return Err(invalid(format!("missing serial port name in \"{s}\"")));
    }
    if port.chars().any(char::is_control) {
        return Err(invalid("serial port name contains control characters"));
    }
    Ok(ConnectTarget::Serial {
        port: normalize_port(port),
        baud,
    })
}

/// Uppercase Windows `comN` names (`com3` -> `COM3`, `\\.\com10` ->
/// `\\.\COM10`); other names are kept as written (Unix paths are
/// case-sensitive).
fn normalize_port(port: &str) -> String {
    let (prefix, name) = match port.strip_prefix(r"\\.\") {
        Some(rest) => (r"\\.\", rest),
        None => ("", port),
    };
    let is_com = name.len() > 3
        && name.is_char_boundary(3)
        && name[..3].eq_ignore_ascii_case("com")
        && name[3..].bytes().all(|b| b.is_ascii_digit());
    if is_com {
        format!("{prefix}{}", name.to_ascii_uppercase())
    } else {
        port.to_string()
    }
}

fn parse_tcp(original: &str, rest: &str) -> Result<ConnectTarget, HostError> {
    let rest = rest.trim_end_matches('/');
    let (host, port) = if let Some(after) = rest.strip_prefix('[') {
        let (host, tail) = after
            .split_once(']')
            .ok_or_else(|| invalid(format!("missing ']' in \"{original}\"")))?;
        let port = tail
            .strip_prefix(':')
            .ok_or_else(|| invalid(format!("missing port in \"{original}\"")))?;
        (host, port)
    } else {
        let (host, port) = rest
            .rsplit_once(':')
            .ok_or_else(|| invalid(format!("missing port in \"{original}\" (tcp://host:port)")))?;
        if host.contains(':') {
            return Err(invalid(format!(
                "IPv6 addresses need brackets: tcp://[{host}]:{port}"
            )));
        }
        (host, port)
    };
    let host = host.trim();
    if host.is_empty() {
        return Err(invalid(format!("missing host in \"{original}\"")));
    }
    if host
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '/')
    {
        return Err(invalid(format!("invalid host \"{host}\"")));
    }
    let port: u16 = port
        .trim()
        .parse()
        .map_err(|_| invalid(format!("invalid port \"{port}\" in \"{original}\"")))?;
    if port == 0 {
        return Err(invalid("TCP port 0 is not allowed"));
    }
    Ok(ConnectTarget::Tcp {
        host: host.to_string(),
        port,
    })
}

fn parse_sim(original: &str, rest: &str) -> Result<ConnectTarget, HostError> {
    let rest = if let Some(r) = rest.strip_prefix("://") {
        r
    } else if let Some(r) = rest.strip_prefix(':') {
        r
    } else {
        rest
    };
    let query = if rest.is_empty() {
        ""
    } else if let Some(q) = rest.strip_prefix('?') {
        q
    } else if let Some(q) = rest.strip_prefix("/?") {
        q
    } else if rest == "/" {
        ""
    } else {
        return Err(invalid(format!(
            "unexpected \"{rest}\" in \"{original}\" (use sim://?key=value&...)"
        )));
    };
    let mut config = SimConfig::default();
    for pair in query.split('&') {
        if pair.trim().is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some((k, v)) => (k.trim(), Some(percent_decode(v.trim()))),
            None => (pair.trim(), None),
        };
        apply_sim_param(&mut config, key, value.as_deref())?;
    }
    Ok(ConnectTarget::Simulator(config))
}

fn apply_sim_param(c: &mut SimConfig, key: &str, value: Option<&str>) -> Result<(), HostError> {
    fn need<'a>(key: &str, value: Option<&'a str>) -> Result<&'a str, HostError> {
        value.ok_or_else(|| invalid(format!("simulator parameter \"{key}\" needs a value")))
    }
    fn num<T: std::str::FromStr>(key: &str, value: Option<&str>) -> Result<T, HostError> {
        let v = need(key, value)?;
        v.parse().map_err(|_| {
            invalid(format!(
                "invalid value \"{v}\" for simulator parameter \"{key}\""
            ))
        })
    }
    fn float(key: &str, value: Option<&str>) -> Result<f32, HostError> {
        let v: f32 = num(key, value)?;
        if v.is_finite() {
            Ok(v)
        } else {
            Err(invalid(format!(
                "simulator parameter \"{key}\" must be finite"
            )))
        }
    }
    fn boolean(key: &str, value: Option<&str>) -> Result<bool, HostError> {
        match value.map(str::to_ascii_lowercase).as_deref() {
            None | Some("1" | "true" | "yes" | "on") => Ok(true),
            Some("0" | "false" | "no" | "off") => Ok(false),
            Some(v) => Err(invalid(format!(
                "invalid value \"{v}\" for simulator parameter \"{key}\" (true/false)"
            ))),
        }
    }
    match key {
        "machine_name" => c.machine_name = need(key, value)?.to_string(),
        "extruders" => c.extruders = num(key, value)?,
        "heated_bed" => c.heated_bed = boolean(key, value)?,
        "heated_chamber" => c.heated_chamber = boolean(key, value)?,
        "build_volume" => {
            let v = need(key, value)?;
            let parts: Vec<&str> = v.split([',', 'x', 'X']).collect();
            if parts.len() != 3 {
                return Err(invalid(format!(
                    "build_volume needs three values (x,y,z), got \"{v}\""
                )));
            }
            for (slot, part) in c.build_volume.iter_mut().zip(parts) {
                *slot = float(key, Some(part.trim()))?;
            }
        }
        "time_scale" => c.time_scale = float(key, value)?,
        "instant_moves" => c.instant_moves = boolean(key, value)?,
        "boot_delay_ms" => c.boot_delay_ms = num(key, value)?,
        "checksum_error_rate" => c.checksum_error_rate = float(key, value)?,
        "drop_ok_rate" => c.drop_ok_rate = float(key, value)?,
        "planner_buffer" => c.planner_buffer = num(key, value)?,
        "rx_buffer" => c.rx_buffer = num(key, value)?,
        "auto_report_temps" => c.auto_report_temps = boolean(key, value)?,
        "auto_report_position" => c.auto_report_position = boolean(key, value)?,
        "advanced_ok" => c.advanced_ok = boolean(key, value)?,
        "mesh_size" => {
            let v = need(key, value)?;
            let (a, b) = v
                .split_once(['x', 'X', ','])
                .ok_or_else(|| invalid(format!("mesh_size needs columns x rows, got \"{v}\"")))?;
            c.mesh_size = (num(key, Some(a.trim()))?, num(key, Some(b.trim()))?);
        }
        "ambient" => c.ambient = float(key, value)?,
        "seed" => c.seed = num(key, value)?,
        _ => {
            return Err(invalid(format!("unknown simulator parameter \"{key}\"")));
        }
    }
    Ok(())
}

/// The query parameters describing the fields of `c` that differ from the
/// defaults.
fn sim_params(c: &SimConfig) -> Vec<String> {
    let d = SimConfig::default();
    let mut out = Vec::new();
    if c.machine_name != d.machine_name {
        out.push(format!("machine_name={}", percent_encode(&c.machine_name)));
    }
    if c.extruders != d.extruders {
        out.push(format!("extruders={}", c.extruders));
    }
    if c.heated_bed != d.heated_bed {
        out.push(format!("heated_bed={}", c.heated_bed));
    }
    if c.heated_chamber != d.heated_chamber {
        out.push(format!("heated_chamber={}", c.heated_chamber));
    }
    if c.build_volume != d.build_volume {
        let [x, y, z] = c.build_volume;
        out.push(format!("build_volume={x},{y},{z}"));
    }
    if c.time_scale != d.time_scale {
        out.push(format!("time_scale={}", c.time_scale));
    }
    if c.instant_moves != d.instant_moves {
        out.push(format!("instant_moves={}", c.instant_moves));
    }
    if c.boot_delay_ms != d.boot_delay_ms {
        out.push(format!("boot_delay_ms={}", c.boot_delay_ms));
    }
    if c.checksum_error_rate != d.checksum_error_rate {
        out.push(format!("checksum_error_rate={}", c.checksum_error_rate));
    }
    if c.drop_ok_rate != d.drop_ok_rate {
        out.push(format!("drop_ok_rate={}", c.drop_ok_rate));
    }
    if c.planner_buffer != d.planner_buffer {
        out.push(format!("planner_buffer={}", c.planner_buffer));
    }
    if c.rx_buffer != d.rx_buffer {
        out.push(format!("rx_buffer={}", c.rx_buffer));
    }
    if c.auto_report_temps != d.auto_report_temps {
        out.push(format!("auto_report_temps={}", c.auto_report_temps));
    }
    if c.auto_report_position != d.auto_report_position {
        out.push(format!("auto_report_position={}", c.auto_report_position));
    }
    if c.advanced_ok != d.advanced_ok {
        out.push(format!("advanced_ok={}", c.advanced_ok));
    }
    if c.mesh_size != d.mesh_size {
        out.push(format!("mesh_size={}x{}", c.mesh_size.0, c.mesh_size.1));
    }
    if c.ambient != d.ambient {
        out.push(format!("ambient={}", c.ambient));
    }
    if c.seed != d.seed {
        out.push(format!("seed={}", c.seed));
    }
    out
}

/// Decode `%XX` escapes and `+` (space). Invalid escapes are kept as
/// written; invalid UTF-8 is replaced.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                let hex = |at: usize| b.get(at).and_then(|&c| char::from(c).to_digit(16));
                match (hex(i + 1), hex(i + 2)) {
                    (Some(h), Some(l)) => {
                        // Two hex digits always fit in a byte.
                        out.push(u8::try_from(h * 16 + l).unwrap_or(b'?'));
                        i += 3;
                    }
                    _ => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serial(port: &str, baud: u32) -> ConnectTarget {
        ConnectTarget::Serial {
            port: port.to_string(),
            baud,
        }
    }

    #[test]
    fn parses_serial_ports() {
        let cases = [
            ("COM3", serial("COM3", 115_200)),
            ("com3@115200", serial("COM3", 115_200)),
            ("  COM7@250000 ", serial("COM7", 250_000)),
            ("/dev/ttyUSB0@250000", serial("/dev/ttyUSB0", 250_000)),
            ("/dev/ttyACM0", serial("/dev/ttyACM0", 115_200)),
            (r"\\.\COM10", serial(r"\\.\COM10", 115_200)),
            (r"\\.\com10@57600", serial(r"\\.\COM10", 57_600)),
            ("COM4@auto", serial("COM4", 0)),
            ("COM4@0", serial("COM4", 0)),
            (
                "/dev/serial/by-id/usb-Prusa_MK3@115200",
                serial("/dev/serial/by-id/usb-Prusa_MK3", 115_200),
            ),
            ("comport", serial("comport", 115_200)),
        ];
        for (text, expected) in cases {
            assert_eq!(ConnectTarget::parse(text).unwrap(), expected, "{text}");
        }
    }

    #[test]
    fn rejects_bad_serial_targets() {
        for text in [
            "",
            "   ",
            "@115200",
            "COM3@fast",
            "COM3@-1",
            "COM3@99999999999",
            "COM\u{7}3",
        ] {
            assert!(
                matches!(ConnectTarget::parse(text), Err(HostError::InvalidTarget(_))),
                "{text:?}"
            );
        }
    }

    #[test]
    fn parses_tcp_targets() {
        let cases = [
            ("tcp://localhost:23", "localhost", 23),
            ("TCP://192.168.1.50:8080", "192.168.1.50", 8080),
            ("tcp://[::1]:23", "::1", 23),
            ("tcp://[fe80::1%eth0]:2323", "fe80::1%eth0", 2323),
            ("tcp://printer.local:23/", "printer.local", 23),
        ];
        for (text, host, port) in cases {
            assert_eq!(
                ConnectTarget::parse(text).unwrap(),
                ConnectTarget::Tcp {
                    host: host.to_string(),
                    port
                },
                "{text}"
            );
        }
        for text in [
            "tcp://",
            "tcp://host",
            "tcp://host:",
            "tcp://:23",
            "tcp://host:0",
            "tcp://host:70000",
            "tcp://::1:23",
            "tcp://[::1]",
            "tcp://[::1:23",
            "tcp://ho st:23",
            "ftp://host:21",
        ] {
            assert!(ConnectTarget::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn parses_simulator_targets() {
        for text in ["sim", "SIM", "sim://", "sim:", "sim:///", "sim://?"] {
            assert_eq!(
                ConnectTarget::parse(text).unwrap(),
                ConnectTarget::Simulator(SimConfig::default()),
                "{text}"
            );
        }
        let t = ConnectTarget::parse("sim://?time_scale=10&extruders=2&checksum_error_rate=0.05")
            .unwrap();
        let ConnectTarget::Simulator(c) = t else {
            panic!("not a simulator");
        };
        assert_eq!(c.time_scale, 10.0);
        assert_eq!(c.extruders, 2);
        assert_eq!(c.checksum_error_rate, 0.05);
        assert_eq!(c.drop_ok_rate, 0.0);

        let t = ConnectTarget::parse(
            "sim://?instant_moves&heated_bed=false&build_volume=300,310,400&mesh_size=7x3&seed=42&machine_name=My+Box%21&boot_delay_ms=5&advanced_ok=1",
        )
        .unwrap();
        let ConnectTarget::Simulator(c) = t else {
            panic!("not a simulator");
        };
        assert!(c.instant_moves);
        assert!(!c.heated_bed);
        assert_eq!(c.build_volume, [300.0, 310.0, 400.0]);
        assert_eq!(c.mesh_size, (7, 3));
        assert_eq!(c.seed, 42);
        assert_eq!(c.machine_name, "My Box!");
        assert_eq!(c.boot_delay_ms, 5);
        assert!(c.advanced_ok);
    }

    #[test]
    fn rejects_bad_simulator_targets() {
        for text in [
            "sim://?bogus=1",
            "sim://?extruders=many",
            "sim://?extruders",
            "sim://?time_scale=NaN",
            "sim://?heated_bed=maybe",
            "sim://?build_volume=1,2",
            "sim://?mesh_size=5",
            "sim://host",
            "simulator",
        ] {
            assert!(ConnectTarget::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn labels() {
        assert_eq!(serial("COM3", 115_200).label(), "COM3 @ 115200");
        assert_eq!(serial("/dev/ttyUSB0", 0).label(), "/dev/ttyUSB0 @ auto");
        assert_eq!(
            ConnectTarget::Tcp {
                host: "host".into(),
                port: 23
            }
            .label(),
            "tcp://host:23"
        );
        assert_eq!(
            ConnectTarget::Tcp {
                host: "::1".into(),
                port: 23
            }
            .label(),
            "tcp://[::1]:23"
        );
        assert_eq!(
            ConnectTarget::Simulator(SimConfig::default()).label(),
            "Virtual printer"
        );
    }

    #[test]
    fn display_round_trips() {
        let targets = [
            serial("COM3", 115_200),
            serial("/dev/ttyUSB0", 0),
            ConnectTarget::Tcp {
                host: "::1".into(),
                port: 23,
            },
            ConnectTarget::Tcp {
                host: "printer.local".into(),
                port: 8080,
            },
            ConnectTarget::Simulator(SimConfig::default()),
            ConnectTarget::Simulator(SimConfig {
                machine_name: "Box & Co = 1".into(),
                extruders: 3,
                heated_bed: false,
                heated_chamber: true,
                build_volume: [180.5, 180.0, 200.0],
                time_scale: 12.5,
                instant_moves: true,
                boot_delay_ms: 1,
                checksum_error_rate: 0.1,
                drop_ok_rate: 0.01,
                planner_buffer: 32,
                rx_buffer: 8,
                auto_report_temps: false,
                auto_report_position: false,
                advanced_ok: true,
                mesh_size: (3, 4),
                ambient: 18.25,
                seed: 7,
                ..SimConfig::default()
            }),
        ];
        for t in targets {
            let text = t.to_string();
            assert_eq!(ConnectTarget::parse(&text).unwrap(), t, "{text}");
        }
        assert_eq!(
            ConnectTarget::Simulator(SimConfig::default()).to_string(),
            "sim"
        );
    }

    #[test]
    fn percent_decoding_is_lenient() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%4"), "%4");
        assert_eq!(percent_decode("%C3%A9"), "\u{e9}");
        assert_eq!(percent_decode("%FF"), "\u{FFFD}");
    }
}
