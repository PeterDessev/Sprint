//! Homing: which axes the printer is known to have homed
//! ([`PrinterState::homed`]), and asking once before an action that needs
//! homed axes (an absolute move, babystepping) when they are not known to
//! be.
//!
//! An axis whose homing is unknown (nothing since connecting tells, the
//! printer may have been homed from its own display) counts as not homed,
//! as it does for the host's pause, resume and cancel scripts.

use sprint_host::{Axes, PrinterState};

/// The axes of `needed` that are not known to be homed.
pub fn unhomed(state: &PrinterState, needed: Axes) -> Axes {
    needed.without(state.homed.unwrap_or(Axes::NONE))
}

/// The axes as the subject of a sentence: "Z is", "X and Y are", "X, Y and
/// Z are" ("No axis is" for none).
pub fn axes_are(axes: Axes) -> String {
    let letters: Vec<&str> = [(axes.x, "X"), (axes.y, "Y"), (axes.z, "Z")]
        .into_iter()
        .filter_map(|(on, letter)| on.then_some(letter))
        .collect();
    match letters.as_slice() {
        [] => "No axis is".to_owned(),
        [one] => format!("{one} is"),
        [rest @ .., last] => format!("{} and {last} are", rest.join(", ")),
    }
}

/// Why `axes` count as not homed, one sentence.
pub fn why_unhomed(state: &PrinterState, axes: Axes) -> String {
    let subject = axes_are(axes);
    let reason = unhomed_reason(state, axes);
    match state.homed {
        // "Sprint …" keeps its capital.
        None => format!("{subject} not known to be homed: {reason}"),
        Some(_) => format!("{subject} not homed: {}", lowercase_first(&reason)),
    }
}

/// Why `axes` count as not homed, without naming them first (for a text
/// under a title that does).
pub fn unhomed_reason(state: &PrinterState, axes: Axes) -> String {
    match state.homed {
        None => "Sprint has not seen the printer home since connecting (homing from its own \
                 display does not show)."
            .to_owned(),
        Some(_) => format!(
            "{} not homed since connecting, or the motors were turned off or the board was \
             reset since.",
            if axes_are(axes).ends_with(" is") {
                "It was"
            } else {
                "They were"
            }
        ),
    }
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A "do it anyway" for an action on axes not known to be homed: given
/// once, it holds until the homing state changes (a homing, the motors
/// turned off, a reset or a new connection ask again).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Consent {
    /// The homing state the consent was given in.
    given: Option<Option<Axes>>,
}

impl Consent {
    /// Forget a consent the printer's state no longer matches. Call it
    /// every frame.
    pub fn observe(&mut self, state: &PrinterState) {
        if !state.is_connected() || self.given.is_some_and(|homed| homed != state.homed) {
            self.given = None;
        }
    }

    /// The axes to ask about before an action needing `needed` (none: go
    /// ahead).
    pub fn ask(&self, state: &PrinterState, needed: Axes) -> Axes {
        if self.given == Some(state.homed) {
            Axes::NONE
        } else {
            unhomed(state, needed)
        }
    }

    /// The user said to go ahead without homing.
    pub fn give(&mut self, state: &PrinterState) {
        self.given = Some(state.homed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_host::ConnectionStatus;

    #[test]
    fn unknown_homing_counts_as_not_homed() {
        let mut state = PrinterState::default();
        assert_eq!(unhomed(&state, Axes::Z), Axes::Z);
        state.homed = Some(Axes::XY);
        assert_eq!(unhomed(&state, Axes::ALL), Axes::Z);
        assert_eq!(unhomed(&state, Axes::XY), Axes::NONE);
        state.homed = Some(Axes::ALL);
        assert!(unhomed(&state, Axes::ALL).is_empty());
    }

    #[test]
    fn sentences_name_the_axes() {
        assert_eq!(axes_are(Axes::Z), "Z is");
        assert_eq!(axes_are(Axes::XY), "X and Y are");
        assert_eq!(axes_are(Axes::ALL), "X, Y and Z are");
        assert_eq!(axes_are(Axes::NONE), "No axis is");
        let mut state = PrinterState::default();
        assert_eq!(
            why_unhomed(&state, Axes::Z),
            "Z is not known to be homed: Sprint has not seen the printer home since connecting \
             (homing from its own display does not show)."
        );
        assert!(unhomed_reason(&state, Axes::Z).starts_with("Sprint has not seen"));
        state.homed = Some(Axes::NONE);
        assert_eq!(
            why_unhomed(&state, Axes::XY),
            "X and Y are not homed: they were not homed since connecting, or the motors were \
             turned off or the board was reset since."
        );
        assert!(unhomed_reason(&state, Axes::Z).starts_with("It was not homed"));
    }

    #[test]
    fn consent_holds_until_the_homing_state_changes() {
        let mut state = PrinterState {
            status: ConnectionStatus::Connected,
            ..PrinterState::default()
        };
        let mut consent = Consent::default();
        assert_eq!(consent.ask(&state, Axes::ALL), Axes::ALL);
        consent.give(&state);
        consent.observe(&state);
        assert!(consent.ask(&state, Axes::ALL).is_empty());
        // Homing X and Y: Z alone is asked about again.
        state.homed = Some(Axes::XY);
        consent.observe(&state);
        assert_eq!(consent.ask(&state, Axes::ALL), Axes::Z);
        consent.give(&state);
        assert!(consent.ask(&state, Axes::ALL).is_empty());
        // A new connection asks again, though its homing is unknown too.
        state.homed = None;
        consent.give(&state);
        state.status = ConnectionStatus::Disconnected;
        consent.observe(&state);
        state.status = ConnectionStatus::Connected;
        assert_eq!(consent.ask(&state, Axes::Z), Axes::Z);
        // Nothing to ask once homed.
        state.homed = Some(Axes::ALL);
        assert!(Consent::default().ask(&state, Axes::ALL).is_empty());
    }
}
