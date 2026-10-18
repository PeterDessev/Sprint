//! Heater identities and states, and the one-line heater readout.

/// Which heater a reading belongs to.
///
/// Mirrors the host's heater identities so applications can map them one
/// to one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HeaterKind {
    /// Hotend of the given tool (0-based).
    Hotend(u8),
    /// Heated bed.
    Bed,
    /// Heated chamber.
    Chamber,
}

impl HeaterKind {
    /// Short label: `"T0"`, `"Bed"`, `"Chamber"`.
    pub fn label(self) -> String {
        match self {
            HeaterKind::Hotend(tool) => format!("T{tool}"),
            HeaterKind::Bed => "Bed".to_string(),
            HeaterKind::Chamber => "Chamber".to_string(),
        }
    }
}

/// What a heater is doing relative to its target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HeaterState {
    /// No usable reading (missing or not a number).
    #[default]
    Unknown,
    /// No target set (heater off).
    Off,
    /// Below the target by more than [`HeaterState::TOLERANCE`].
    Heating,
    /// Above the target by more than [`HeaterState::TOLERANCE`].
    Cooling,
    /// Within [`HeaterState::TOLERANCE`] of the target.
    AtTarget,
}

impl HeaterState {
    /// Distance from the target (°C) still counted as "at target".
    pub const TOLERANCE: f32 = 2.0;

    /// Classify a reading. A missing, zero, negative or non-finite target
    /// means the heater is off.
    pub fn classify(current: f32, target: Option<f32>) -> HeaterState {
        if !current.is_finite() {
            return HeaterState::Unknown;
        }
        match target {
            Some(t) if t.is_finite() && t > 0.0 => {
                if (current - t).abs() <= Self::TOLERANCE {
                    HeaterState::AtTarget
                } else if current < t {
                    HeaterState::Heating
                } else {
                    HeaterState::Cooling
                }
            }
            _ => HeaterState::Off,
        }
    }

    /// Lowercase name: `"heating"`, `"at target"`, ...
    pub fn name(self) -> &'static str {
        match self {
            HeaterState::Unknown => "unknown",
            HeaterState::Off => "off",
            HeaterState::Heating => "heating",
            HeaterState::Cooling => "cooling",
            HeaterState::AtTarget => "at target",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_states() {
        assert_eq!(HeaterState::classify(20.0, None), HeaterState::Off);
        assert_eq!(HeaterState::classify(20.0, Some(0.0)), HeaterState::Off);
        assert_eq!(HeaterState::classify(20.0, Some(-5.0)), HeaterState::Off);
        assert_eq!(
            HeaterState::classify(20.0, Some(f32::NAN)),
            HeaterState::Off
        );
        assert_eq!(
            HeaterState::classify(f32::NAN, Some(200.0)),
            HeaterState::Unknown
        );
        assert_eq!(
            HeaterState::classify(150.0, Some(200.0)),
            HeaterState::Heating
        );
        assert_eq!(
            HeaterState::classify(198.5, Some(200.0)),
            HeaterState::AtTarget
        );
        assert_eq!(
            HeaterState::classify(202.0, Some(200.0)),
            HeaterState::AtTarget
        );
        assert_eq!(
            HeaterState::classify(230.0, Some(200.0)),
            HeaterState::Cooling
        );
    }

    #[test]
    fn labels() {
        assert_eq!(HeaterKind::Hotend(1).label(), "T1");
        assert_eq!(HeaterKind::Bed.label(), "Bed");
        assert_eq!(HeaterKind::Chamber.label(), "Chamber");
        assert_eq!(HeaterState::AtTarget.name(), "at target");
    }
}
