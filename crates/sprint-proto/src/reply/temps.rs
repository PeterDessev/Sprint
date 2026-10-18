//! Temperature reports: Marlin, Prusa, Klipper, RepRapFirmware,
//! Smoothieware and Repetier variants of `T:210.0 /210.0 B:60.0 /60.0 @:64`.

use std::collections::BTreeMap;

use super::{HeaterReading, TempReport};

/// Highest tool index accepted (guards against absurd `T999999:` keys).
const MAX_TOOLS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Slot {
    /// `T:` — the active (or only) hotend.
    Plain,
    Tool(usize),
    Bed,
    Chamber,
    Cooler,
    Probe,
    Board,
    Redundant,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Scale {
    /// Marlin `@:` style: 0..=127, or 0..=255 when any value exceeds 127.
    Auto,
    /// Smoothieware `@<pwm>` style: 0..=255.
    Pwm255,
}

#[derive(Clone, Copy, Debug, Default)]
struct RawHeater {
    current: Option<f32>,
    target: Option<f32>,
    power: Option<(f32, Scale)>,
}

enum Key {
    Heater(Slot),
    Power(Slot),
    ExtruderIndex,
    Ignored,
}

fn tool_index(digits: &str) -> Option<usize> {
    if digits.is_empty() || digits.len() > 3 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<usize>().ok().filter(|&n| n < MAX_TOOLS)
}

fn classify(key: &str) -> Key {
    match key {
        "T" => Key::Heater(Slot::Plain),
        "B" => Key::Heater(Slot::Bed),
        "C" => Key::Heater(Slot::Chamber),
        "L" => Key::Heater(Slot::Cooler),
        "P" => Key::Heater(Slot::Probe),
        // Prusa reports its ambient (board) thermistor as A:, Marlin 2.1 its
        // board sensor as M:.
        "A" | "M" => Key::Heater(Slot::Board),
        "R" => Key::Heater(Slot::Redundant),
        "@" => Key::Power(Slot::Plain),
        "B@" => Key::Power(Slot::Bed),
        "C@" => Key::Power(Slot::Chamber),
        "L@" => Key::Power(Slot::Cooler),
        "E" => Key::ExtruderIndex,
        _ => {
            if let Some(n) = key.strip_prefix('T').and_then(tool_index) {
                Key::Heater(Slot::Tool(n))
            } else if let Some(n) = key.strip_prefix('@').and_then(tool_index) {
                Key::Power(Slot::Tool(n))
            } else {
                Key::Ignored
            }
        }
    }
}

fn number(s: &str) -> Option<f32> {
    s.trim().parse::<f32>().ok().filter(|v| v.is_finite())
}

fn is_number(s: &str) -> bool {
    number(s).is_some()
}

/// True when `tok` can start a temperature report: `T:`, `Tn:`, `B:` or
/// `C:` with a numeric value (possibly in the next token).
fn is_report_start(tok: &str, next: Option<&str>) -> bool {
    let Some((key, val)) = tok.split_once(':') else {
        return false;
    };
    let heater = matches!(
        classify(key),
        Key::Heater(Slot::Plain | Slot::Tool(_) | Slot::Bed | Slot::Chamber)
    );
    let value = if val.is_empty() {
        next.unwrap_or("")
    } else {
        val
    };
    let current = value.split('/').next().unwrap_or("");
    heater && is_number(current)
}

/// Parse a temperature report. Returns `None` unless the line starts with
/// a hotend, bed or chamber reading (`T:`, `T0:`, `B:`, `C:`), so position
/// reports and other lines containing colons are not mistaken for one.
///
/// Power values (`@:`, `B@:`, `@0:`, Smoothieware `@0`) are normalised to
/// 0..=1. Hotends are indexed by tool; when only `T:` is present it is
/// tool 0 (or the tool given by a heating-wait line's `E:`). Gaps in the
/// hotend list are filled with readings whose `current` is NaN.
pub fn parse_temperatures(line: &str) -> Option<TempReport> {
    let toks: Vec<&str> = line.split_whitespace().collect();
    if !is_report_start(toks.first()?, toks.get(1).copied()) {
        return None;
    }

    let mut heaters: BTreeMap<Slot, RawHeater> = BTreeMap::new();
    let mut extruder_index: Option<usize> = None;
    let mut last: Option<Slot> = None;
    let mut i = 0;
    while i < toks.len() {
        let tok = toks[i];
        i += 1;

        if let Some(rest) = tok.strip_prefix('/') {
            // "/210.0" or "/ 210.0": target of the previous heater.
            let text = if rest.is_empty() {
                match toks.get(i) {
                    Some(next) if is_number(next) => {
                        i += 1;
                        *next
                    }
                    _ => "",
                }
            } else {
                rest
            };
            if let (Some(slot), Some(v)) = (last, number(text)) {
                heaters.entry(slot).or_default().target = Some(v);
            }
            continue;
        }
        if tok.starts_with('(') {
            // Raw ADC values (SHOW_TEMP_ADC_VALUES): "(3456.00)".
            continue;
        }
        if let Some((key, val)) = tok.split_once(':') {
            let val = if val.is_empty() {
                match toks.get(i) {
                    Some(next)
                        if !next.contains(':')
                            && is_number(next.split('/').next().unwrap_or("")) =>
                    {
                        i += 1;
                        *next
                    }
                    _ => "",
                }
            } else {
                val
            };
            match classify(key) {
                Key::Heater(slot) => {
                    let mut parts = val.splitn(2, '/');
                    let current = parts.next().and_then(number);
                    let target = parts.next().and_then(number);
                    if let Some(current) = current {
                        let h = heaters.entry(slot).or_default();
                        h.current = Some(current);
                        if target.is_some() {
                            h.target = target;
                        }
                        last = Some(slot);
                    } else {
                        last = None;
                    }
                }
                Key::Power(slot) => {
                    if let Some(p) = number(val) {
                        heaters.entry(slot).or_default().power = Some((p, Scale::Auto));
                    }
                    last = None;
                }
                Key::ExtruderIndex => {
                    extruder_index = val.trim().parse::<usize>().ok().filter(|&n| n < MAX_TOOLS);
                    last = None;
                }
                Key::Ignored => last = None,
            }
            continue;
        }
        if let Some(rest) = tok.strip_prefix('@') {
            // Smoothieware: "T:21.0 /0.0 @0" — PWM of the previous heater.
            if let (Some(slot), Some(p)) = (last, number(rest)) {
                heaters.entry(slot).or_default().power = Some((p, Scale::Pwm255));
            }
            continue;
        }
    }

    // Marlin reports power 0..127; a value above 127 means 0..255.
    let wide = heaters
        .values()
        .filter_map(|h| h.power)
        .any(|(p, scale)| scale == Scale::Auto && p > 127.0);
    let normalise = |power: Option<(f32, Scale)>| {
        power.map(|(p, scale)| {
            let full = if scale == Scale::Pwm255 || wide {
                255.0
            } else {
                127.0
            };
            (p / full).clamp(0.0, 1.0)
        })
    };
    let reading = |raw: &RawHeater| {
        raw.current.map(|current| HeaterReading {
            current,
            target: raw.target,
            power: normalise(raw.power),
        })
    };

    // Hotends: indexed readings win over the plain "T:" one.
    let mut tools: BTreeMap<usize, RawHeater> = heaters
        .iter()
        .filter_map(|(slot, raw)| match slot {
            Slot::Tool(n) if raw.current.is_some() => Some((*n, *raw)),
            _ => None,
        })
        .collect();
    // Indexed powers ("@1:") whose temperature was not reported are ignored.
    if let Some(plain) = heaters.get(&Slot::Plain) {
        let idx = extruder_index.unwrap_or(0);
        if plain.current.is_some() {
            tools.entry(idx).or_insert(*plain);
        }
        if let (Some(power), Some(tool)) = (plain.power, tools.get_mut(&idx)) {
            tool.power.get_or_insert(power);
        }
    }
    let mut hotends = Vec::new();
    if let Some((&max, _)) = tools.last_key_value() {
        hotends = (0..=max)
            .map(|n| {
                tools.get(&n).and_then(reading).unwrap_or(HeaterReading {
                    current: f32::NAN,
                    target: None,
                    power: None,
                })
            })
            .collect();
    }

    let get = |slot: Slot| heaters.get(&slot).and_then(reading);
    let chamber = get(Slot::Chamber);
    let mut cooler = get(Slot::Cooler);
    // Marlin prints the laser cooler's power as "C@:" too.
    if chamber.is_none()
        && let (Some(c), Some(raw)) = (cooler.as_mut(), heaters.get(&Slot::Chamber))
    {
        c.power = c.power.or(normalise(raw.power));
    }
    Some(TempReport {
        hotends,
        bed: get(Slot::Bed),
        chamber,
        probe: get(Slot::Probe).map(|h| h.current),
        board: get(Slot::Board).map(|h| h.current),
        redundant: get(Slot::Redundant).map(|h| h.current),
        cooler,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Reply;

    fn h(current: f32, target: Option<f32>, power: Option<f32>) -> HeaterReading {
        HeaterReading {
            current,
            target,
            power,
        }
    }

    fn approx_power(r: &HeaterReading, expected: f32) {
        let p = r.power.expect("power");
        assert!((p - expected).abs() < 1e-4, "power {p} != {expected}");
    }

    #[test]
    fn marlin_single_extruder() {
        let r = parse_temperatures("T:210.00 /210.00 B:60.00 /60.00 @:64 B@:0").unwrap();
        assert_eq!(r.hotends.len(), 1);
        assert_eq!(r.hotends[0].current, 210.0);
        assert_eq!(r.hotends[0].target, Some(210.0));
        approx_power(&r.hotends[0], 64.0 / 127.0);
        assert_eq!(r.bed, Some(h(60.0, Some(60.0), Some(0.0))));
        assert_eq!(r.chamber, None);
    }

    #[test]
    fn marlin_multi_extruder() {
        let r = parse_temperatures(
            "T:200.00 /200.00 B:60.00 /60.00 T0:200.00 /200.00 T1:150.00 /180.00 @:127 B@:30 @0:127 @1:64",
        )
        .unwrap();
        assert_eq!(r.hotends.len(), 2);
        assert_eq!(r.hotends[0].current, 200.0);
        assert_eq!(r.hotends[1].current, 150.0);
        assert_eq!(r.hotends[1].target, Some(180.0));
        approx_power(&r.hotends[0], 1.0);
        approx_power(&r.hotends[1], 64.0 / 127.0);
        approx_power(r.bed.as_ref().unwrap(), 30.0 / 127.0);
    }

    #[test]
    fn spaces_around_slash_and_glued_targets() {
        let r = parse_temperatures("T:210.0 / 210.0 B:60.0 / 0.0").unwrap();
        assert_eq!(r.hotends[0], h(210.0, Some(210.0), None));
        assert_eq!(r.bed, Some(h(60.0, Some(0.0), None)));
        let r = parse_temperatures("T:210.0/215.0 B:60/65").unwrap();
        assert_eq!(r.hotends[0].target, Some(215.0));
        assert_eq!(r.bed.unwrap().target, Some(65.0));
    }

    #[test]
    fn heating_wait_lines() {
        let r = parse_temperatures("T:205.12 E:0 W:3").unwrap();
        assert_eq!(r.hotends, vec![h(205.12, None, None)]);
        let r = parse_temperatures("T:205.1 E:0 B:59.8").unwrap();
        assert_eq!(r.hotends, vec![h(205.1, None, None)]);
        assert_eq!(r.bed, Some(h(59.8, None, None)));
        let r = parse_temperatures("T:205.1 E:1 W:?").unwrap();
        assert_eq!(r.hotends.len(), 2);
        assert!(r.hotends[0].current.is_nan());
        assert_eq!(r.hotends[1].current, 205.1);
        let r = parse_temperatures("T:199.95 /200.00 B:59.98 /60.00 @:55 B@:12 W:?").unwrap();
        assert_eq!(r.hotends[0].target, Some(200.0));
    }

    #[test]
    fn prusa_report() {
        let r = parse_temperatures("T:21.6 /0.0 B:21.9 /0.0 T0:21.6 /0.0 @:0 B@:0 P:0.0 A:30.9")
            .unwrap();
        assert_eq!(r.hotends, vec![h(21.6, Some(0.0), Some(0.0))]);
        assert_eq!(r.bed, Some(h(21.9, Some(0.0), Some(0.0))));
        assert_eq!(r.probe, Some(0.0));
        assert_eq!(r.board, Some(30.9));
    }

    #[test]
    fn klipper_report() {
        let r = parse_temperatures("B:60.0 /60.0 T0:210.0 /210.0").unwrap();
        assert_eq!(r.hotends, vec![h(210.0, Some(210.0), None)]);
        assert_eq!(r.bed, Some(h(60.0, Some(60.0), None)));
        let r = parse_temperatures("B:24.4 /0.0 T0:24.6 /0.0 T1:25.0 /0.0").unwrap();
        assert_eq!(r.hotends.len(), 2);
    }

    #[test]
    fn smoothieware_power_without_colon() {
        let r = parse_temperatures("T:21.0 /0.0 @0 B:21.2 /0.0 @255").unwrap();
        assert_eq!(r.hotends, vec![h(21.0, Some(0.0), Some(0.0))]);
        assert_eq!(r.bed, Some(h(21.2, Some(0.0), Some(1.0))));
        let r = parse_temperatures("T:200.0 /200.0 @128").unwrap();
        approx_power(&r.hotends[0], 128.0 / 255.0);
    }

    #[test]
    fn power_scale_255_when_above_127() {
        let r = parse_temperatures("T:200.0 /200.0 B:60.0 /60.0 @:200 B@:100").unwrap();
        approx_power(&r.hotends[0], 200.0 / 255.0);
        approx_power(r.bed.as_ref().unwrap(), 100.0 / 255.0);
    }

    #[test]
    fn repetier_and_rrf() {
        let r =
            parse_temperatures("T:20.61 /0 B:20.61 /0 B@:0 @:0 T0:20.61 /0 @0:0 T1:20.0 /0 @1:0")
                .unwrap();
        assert_eq!(r.hotends.len(), 2);
        assert_eq!(r.hotends[1].current, 20.0);
        let r = parse_temperatures("T:25.3 /0.0 B:25.1 /0.0").unwrap();
        assert_eq!(r.hotends[0].current, 25.3);
    }

    #[test]
    fn chamber_cooler_redundant_board_and_adc() {
        let r = parse_temperatures(
            "T:210.00 /210.00 (3456.00) B:60.00 /60.00 (1234.00) C:35.00 /40.00 L:10.00 /5.00 R:209.50 /210.00 M:40.10 /0.00 S:45.0 /0.0 @:64 B@:0 C@:20",
        )
        .unwrap();
        assert_eq!(r.chamber.as_ref().unwrap().target, Some(40.0));
        approx_power(r.chamber.as_ref().unwrap(), 20.0 / 127.0);
        assert_eq!(r.cooler.as_ref().unwrap().current, 10.0);
        assert_eq!(r.redundant, Some(209.5));
        assert_eq!(r.board, Some(40.1));
        // Cooler power reported as C@ when there is no chamber.
        let r = parse_temperatures("T:21.00 /0.00 L:12.00 /10.00 @:0 C@:64").unwrap();
        approx_power(r.cooler.as_ref().unwrap(), 64.0 / 127.0);
    }

    #[test]
    fn rejects_non_temperature_lines() {
        for line in [
            "",
            "ok",
            "X:0.00 Y:0.00 Z:0.00 E:0.00 Count X:0 Y:0 Z:0",
            "C: X:0.00 Y:0.00 Z:0.00 E:0.00",
            "T:abc",
            "echo:busy: processing",
            "Bed X: 110.00 Y: 110.00 Z: 0.42",
            "TMC: X:0 Y:0",
            "x_min: open",
            "@:64",
            "T0",
        ] {
            assert!(parse_temperatures(line).is_none(), "{line:?}");
        }
    }

    #[test]
    fn reports_with_placeholders_compare_equal() {
        // Regression: the NaN placeholder for an unreported tool made the
        // report unequal to itself under derived equality.
        let r = parse_temperatures("T1:200.0 /200.0 B:60.0 /60.0").unwrap();
        assert!(r.hotends[0].current.is_nan());
        assert_eq!(r, r.clone());
        assert_eq!(
            Reply::Temperatures(r.clone()),
            Reply::Temperatures(r.clone())
        );
        let other = parse_temperatures("T0:20.0 T1:200.0 /200.0 B:60.0 /60.0").unwrap();
        assert_ne!(r, other);
    }

    #[test]
    fn absurd_tool_indexes_are_ignored() {
        let r = parse_temperatures("T:20.0 T999999:30.0 @99999:5").unwrap();
        assert_eq!(r.hotends.len(), 1);
    }
}
