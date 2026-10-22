//! Static catalog of known configuration commands (Marlin 2.1 set), used by
//! the tuning UI to label, range-check and explain each setting.

use serde::{Deserialize, Serialize};

/// Grouping of settings in the tuning UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SettingCategory {
    /// Steps, speeds, accelerations, jerk, input shaping, kinematics.
    Motion,
    /// Heater control (PID, MPC), presets, temperature units.
    Thermal,
    /// Filament, linear advance, cold extrusion, runout.
    Extrusion,
    /// Probe offsets and servo probes.
    Probing,
    /// Bed leveling and Z alignment.
    Leveling,
    /// Home offsets and endstop adjustments.
    Homing,
    /// Stepper driver (TMC) configuration.
    Drivers,
    /// Firmware retraction.
    Retraction,
    /// Everything else (display, fans, tool changing, power loss...).
    Other,
}

impl SettingCategory {
    /// All categories in display order.
    pub const ALL: [SettingCategory; 9] = [
        SettingCategory::Motion,
        SettingCategory::Thermal,
        SettingCategory::Extrusion,
        SettingCategory::Probing,
        SettingCategory::Leveling,
        SettingCategory::Homing,
        SettingCategory::Drivers,
        SettingCategory::Retraction,
        SettingCategory::Other,
    ];

    /// Display name.
    pub fn name(&self) -> &'static str {
        match self {
            SettingCategory::Motion => "Motion",
            SettingCategory::Thermal => "Thermal",
            SettingCategory::Extrusion => "Extrusion",
            SettingCategory::Probing => "Probing",
            SettingCategory::Leveling => "Leveling",
            SettingCategory::Homing => "Homing",
            SettingCategory::Drivers => "Drivers",
            SettingCategory::Retraction => "Retraction",
            SettingCategory::Other => "Other",
        }
    }
}

/// Static description of a known setting command (used by the tuning UI).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SettingDef {
    /// Command code, e.g. `"M92"`.
    pub code: &'static str,
    /// Short name, e.g. "Steps per unit".
    pub name: &'static str,
    /// UI grouping.
    pub category: SettingCategory,
    /// What the setting does, one or two sentences.
    pub description: &'static str,
    /// Parameters that select an instance rather than carry a value
    /// (`E` of M301, `T`/`I` of M906, `S` of M145, `X`/`Y` of M593). They are
    /// part of [`SettingCommand::key`](crate::SettingCommand::key). Every
    /// selector also appears in `params`.
    pub selectors: &'static [char],
    /// Parameters in the order the firmware reports them.
    pub params: &'static [ParamDef],
    /// The value persists across reboots only after `M500`.
    pub requires_eeprom_save: bool,
}

impl SettingDef {
    /// Definition of one parameter.
    pub fn param(&self, letter: char) -> Option<&'static ParamDef> {
        let letter = letter.to_ascii_uppercase();
        self.params.iter().find(|p| p.letter == letter)
    }

    /// True when `letter` selects an instance (see [`SettingDef::selectors`]).
    pub fn is_selector(&self, letter: char) -> bool {
        self.selectors.contains(&letter.to_ascii_uppercase())
    }

    /// The firmware reports this setting, but sending it back would do more
    /// than change it (see [`is_report_only`]): show it, never apply it.
    pub fn is_report_only(&self) -> bool {
        is_report_only(self.code)
    }
}

/// Commands that heat or move. A firmware may report one among its settings
/// (Marlin reports Autotemp as `M104 S<min> B<max> F<factor>`), but sending
/// it back does more than change a setting: `M104 S` sets the hotend
/// temperature.
static REPORT_ONLY: &[&str] = &[
    "M104", "M109", "M140", "M190", "M141", "M191", "G0", "G1", "G2", "G3", "G28",
];

/// True when settings with this code are only ever reported: sending one
/// would heat or move the printer (see [`SettingDef::is_report_only`]).
pub fn is_report_only(code: &str) -> bool {
    normalize_code(code).is_some_and(|c| REPORT_ONLY.contains(&c.as_str()))
}

/// One parameter of a [`SettingDef`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamDef {
    /// Parameter letter (uppercase).
    pub letter: char,
    /// Label for the UI, e.g. "X steps".
    pub label: &'static str,
    /// Unit, e.g. "steps/mm", "mm/s", "°C"; empty when unitless.
    pub unit: &'static str,
    /// Smallest sensible value.
    pub min: f64,
    /// Largest sensible value.
    pub max: f64,
    /// Decimals to show / round to.
    pub decimals: u8,
    /// The parameter is a bare letter without a value (`M569 S1 X Y`,
    /// `M593 X`, `M149 C`); stored as NaN in a [`SettingCommand`](crate::SettingCommand).
    pub flag: bool,
}

const fn num(
    letter: char,
    label: &'static str,
    unit: &'static str,
    min: f64,
    max: f64,
    decimals: u8,
) -> ParamDef {
    ParamDef {
        letter,
        label,
        unit,
        min,
        max,
        decimals,
        flag: false,
    }
}

/// An index parameter (tool, stepper, preset...).
const fn index(letter: char, label: &'static str, max: f64) -> ParamDef {
    num(letter, label, "", 0.0, max, 0)
}

/// A 0/1 switch.
const fn toggle(letter: char, label: &'static str) -> ParamDef {
    num(letter, label, "", 0.0, 1.0, 0)
}

/// A bare letter without a value.
const fn flag(letter: char, label: &'static str) -> ParamDef {
    ParamDef {
        letter,
        label,
        unit: "",
        min: 0.0,
        max: 1.0,
        decimals: 0,
        flag: true,
    }
}

const STEPS: &str = "steps/mm";
const MM: &str = "mm";
const MM_S: &str = "mm/s";
const MM_S2: &str = "mm/s²";
const MM_MIN: &str = "mm/min";
const DEG_C: &str = "°C";
const MA: &str = "mA";

use SettingCategory as C;

static DEFS: &[SettingDef] = &[
    SettingDef {
        code: "M92",
        name: "Steps per unit",
        category: C::Motion,
        description: "Motor steps needed to move each axis by one millimetre. The extruder value is what E-steps calibration adjusts.",
        selectors: &['T'],
        params: &[
            num('X', "X steps", STEPS, 0.001, 100_000.0, 2),
            num('Y', "Y steps", STEPS, 0.001, 100_000.0, 2),
            num('Z', "Z steps", STEPS, 0.001, 100_000.0, 2),
            num('E', "Extruder steps", STEPS, 0.001, 100_000.0, 2),
            index('T', "Extruder", 7.0),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M201",
        name: "Maximum acceleration",
        category: C::Motion,
        description: "Ceiling on the acceleration of each axis during print moves. Lower values reduce ringing and skipped steps at the cost of speed.",
        selectors: &['T'],
        params: &[
            num('X', "X acceleration", MM_S2, 0.0, 100_000.0, 0),
            num('Y', "Y acceleration", MM_S2, 0.0, 100_000.0, 0),
            num('Z', "Z acceleration", MM_S2, 0.0, 100_000.0, 0),
            num('E', "Extruder acceleration", MM_S2, 0.0, 100_000.0, 0),
            num('F', "XY frequency limit", "Hz", 0.0, 100.0, 0),
            num('S', "Minimum speed factor", "%", 1.0, 100.0, 0),
            index('T', "Extruder", 7.0),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M203",
        name: "Maximum feedrate",
        category: C::Motion,
        description: "Top speed each axis may reach, whatever the G-code asks for.",
        selectors: &['T'],
        params: &[
            num('X', "X speed", MM_S, 0.0, 10_000.0, 2),
            num('Y', "Y speed", MM_S, 0.0, 10_000.0, 2),
            num('Z', "Z speed", MM_S, 0.0, 10_000.0, 2),
            num('E', "Extruder speed", MM_S, 0.0, 10_000.0, 2),
            index('T', "Extruder", 7.0),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M204",
        name: "Default acceleration",
        category: C::Motion,
        description: "Acceleration used for extruding moves, retractions and travel moves when the G-code does not override it.",
        selectors: &[],
        params: &[
            num('P', "Printing", MM_S2, 0.0, 100_000.0, 0),
            num('R', "Retraction", MM_S2, 0.0, 100_000.0, 0),
            num('T', "Travel", MM_S2, 0.0, 100_000.0, 0),
            num(
                'S',
                "Printing and travel (legacy)",
                MM_S2,
                0.0,
                100_000.0,
                0,
            ),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M205",
        name: "Advanced motion",
        category: C::Motion,
        description: "Cornering and minimum-speed behaviour of the planner: junction deviation or classic jerk, minimum feedrates and the shortest segment duration.",
        selectors: &[],
        params: &[
            num('B', "Minimum segment time", "µs", 0.0, 1_000_000.0, 0),
            num('S', "Minimum print feedrate", MM_S, 0.0, 1000.0, 2),
            num('T', "Minimum travel feedrate", MM_S, 0.0, 1000.0, 2),
            num('J', "Junction deviation", MM, 0.0, 0.5, 3),
            num('X', "X jerk", MM_S, 0.0, 100.0, 2),
            num('Y', "Y jerk", MM_S, 0.0, 100.0, 2),
            num('Z', "Z jerk", MM_S, 0.0, 100.0, 2),
            num('E', "Extruder jerk", MM_S, 0.0, 100.0, 2),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M206",
        name: "Home offset",
        category: C::Homing,
        description: "Shifts the coordinate system relative to the home position, e.g. to correct where X0 Y0 lands on the bed.",
        selectors: &[],
        params: &[
            num('X', "X offset", MM, -1000.0, 1000.0, 2),
            num('Y', "Y offset", MM, -1000.0, 1000.0, 2),
            num('Z', "Z offset", MM, -1000.0, 1000.0, 2),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M301",
        name: "Hotend PID",
        category: C::Thermal,
        description: "Gains of the hotend temperature controller. Run a PID autotune to find good values for your heater block.",
        selectors: &['E'],
        params: &[
            index('E', "Hotend", 7.0),
            num('P', "Proportional (Kp)", "", 0.0, 1000.0, 2),
            num('I', "Integral (Ki)", "", 0.0, 1000.0, 2),
            num('D', "Derivative (Kd)", "", 0.0, 10_000.0, 2),
            num('C', "Extrusion scaling (Kc)", "", 0.0, 1000.0, 2),
            num('L', "Extrusion lag", "samples", 0.0, 80.0, 0),
            num('F', "Fan scaling (Kf)", "", 0.0, 100.0, 2),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M304",
        name: "Bed PID",
        category: C::Thermal,
        description: "Gains of the heated bed temperature controller (when the bed uses PID instead of on/off control).",
        selectors: &[],
        params: &[
            num('P', "Proportional (Kp)", "", 0.0, 1000.0, 2),
            num('I', "Integral (Ki)", "", 0.0, 1000.0, 2),
            num('D', "Derivative (Kd)", "", 0.0, 10_000.0, 2),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M309",
        name: "Chamber PID",
        category: C::Thermal,
        description: "Gains of the heated chamber temperature controller.",
        selectors: &[],
        params: &[
            num('P', "Proportional (Kp)", "", 0.0, 1000.0, 2),
            num('I', "Integral (Ki)", "", 0.0, 1000.0, 2),
            num('D', "Derivative (Kd)", "", 0.0, 10_000.0, 2),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M306",
        name: "Model predictive temperature control",
        category: C::Thermal,
        description: "Physical model of the hotend used by MPC instead of PID: heater power, heat capacities and heat-loss coefficients. Measure them with M306 T.",
        selectors: &['E'],
        params: &[
            index('E', "Hotend", 7.0),
            num('P', "Heater power", "W", 0.0, 500.0, 2),
            num('C', "Block heat capacity", "J/K", 0.0, 100.0, 2),
            num('R', "Sensor responsiveness", "", 0.0, 1.0, 4),
            num('A', "Ambient heat transfer", "W/K", 0.0, 1.0, 4),
            num('F', "Fan heat transfer", "W/K", 0.0, 1.0, 4),
            num('H', "Filament heat capacity", "J/K/mm", 0.0, 1.0, 4),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M851",
        name: "Probe offset",
        category: C::Probing,
        description: "Position of the probe's trigger point relative to the nozzle. Z is the live-adjustable first-layer height offset.",
        selectors: &[],
        params: &[
            num('X', "X offset", MM, -500.0, 500.0, 2),
            num('Y', "Y offset", MM, -500.0, 500.0, 2),
            num('Z', "Z offset", MM, -20.0, 20.0, 2),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M900",
        name: "Linear advance",
        category: C::Extrusion,
        description: "Pressure advance factor: extra filament pushed while accelerating (and pulled while braking) so line width stays even at corners.",
        selectors: &['T'],
        params: &[
            num('K', "Advance factor (K)", "", 0.0, 10.0, 3),
            num('L', "Second advance factor", "", 0.0, 10.0, 3),
            index('T', "Extruder", 7.0),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M200",
        name: "Filament diameter",
        category: C::Extrusion,
        description: "Nominal filament diameter and volumetric mode, in which E values are cubic millimetres instead of filament length.",
        selectors: &['T'],
        params: &[
            num('D', "Filament diameter", MM, 0.0, 5.0, 2),
            toggle('S', "Volumetric extrusion"),
            num('L', "Volumetric speed limit", "mm³/s", 0.0, 100.0, 2),
            index('T', "Extruder", 7.0),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M104",
        name: "Autotemp",
        category: C::Thermal,
        description: "While printing, the hotend target rises from the minimum towards the maximum with the extrusion rate, scaled by the factor. Shown for reference only: M104 S also sets the hotend temperature, so Sprint never sends this setting back.",
        selectors: &[],
        params: &[
            num('S', "Minimum temperature", DEG_C, 0.0, 450.0, 0),
            num('B', "Maximum temperature", DEG_C, 0.0, 450.0, 0),
            num('F', "Factor", "", 0.0, 10.0, 2),
        ],
        requires_eeprom_save: false,
    },
    SettingDef {
        code: "M145",
        name: "Preheat presets",
        category: C::Thermal,
        description: "Temperatures and fan speed of the display's material preheat presets (preset 0 is usually PLA, 1 ABS).",
        selectors: &['S'],
        params: &[
            index('S', "Preset", 9.0),
            num('H', "Hotend temperature", DEG_C, 0.0, 450.0, 0),
            num('B', "Bed temperature", DEG_C, 0.0, 150.0, 0),
            num('F', "Fan speed", "", 0.0, 255.0, 0),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M420",
        name: "Bed leveling state",
        category: C::Leveling,
        description: "Whether the stored leveling mesh is applied, and the height over which its correction fades out.",
        selectors: &[],
        params: &[
            toggle('S', "Leveling enabled"),
            num('Z', "Fade height", MM, 0.0, 100.0, 2),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M413",
        name: "Power-loss recovery",
        category: C::Other,
        description: "Keeps track of the print so it can be resumed after a power cut.",
        selectors: &[],
        params: &[toggle('S', "Enabled")],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M906",
        name: "Stepper driver current",
        category: C::Drivers,
        description: "Motor current of TMC stepper drivers. More current gives more torque but heats motors and drivers.",
        selectors: &['T', 'I'],
        params: &[
            num('X', "X current", MA, 0.0, 5000.0, 0),
            num('Y', "Y current", MA, 0.0, 5000.0, 0),
            num('Z', "Z current", MA, 0.0, 5000.0, 0),
            num('E', "Extruder current", MA, 0.0, 5000.0, 0),
            index('I', "Stepper index", 3.0),
            index('T', "Extruder", 7.0),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M913",
        name: "Hybrid threshold",
        category: C::Drivers,
        description: "Speed above which TMC drivers leave quiet StealthChop for SpreadCycle, which keeps torque at high speed.",
        selectors: &['T', 'I'],
        params: &[
            num('X', "X threshold", MM_S, 0.0, 10_000.0, 0),
            num('Y', "Y threshold", MM_S, 0.0, 10_000.0, 0),
            num('Z', "Z threshold", MM_S, 0.0, 10_000.0, 0),
            num('E', "Extruder threshold", MM_S, 0.0, 10_000.0, 0),
            index('I', "Stepper index", 3.0),
            index('T', "Extruder", 7.0),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M914",
        name: "StallGuard threshold",
        category: C::Drivers,
        description: "Sensitivity of sensorless homing. The meaning of higher values depends on the driver model.",
        selectors: &['I'],
        params: &[
            num('X', "X sensitivity", "", -64.0, 255.0, 0),
            num('Y', "Y sensitivity", "", -64.0, 255.0, 0),
            num('Z', "Z sensitivity", "", -64.0, 255.0, 0),
            index('I', "Stepper index", 3.0),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M569",
        name: "Driver stepping mode",
        category: C::Drivers,
        description: "Which TMC drivers run in quiet StealthChop mode (S1) rather than SpreadCycle (S0). The axis letters list the affected drivers.",
        selectors: &['T', 'I'],
        params: &[
            toggle('S', "StealthChop enabled"),
            flag('X', "X driver"),
            flag('Y', "Y driver"),
            flag('Z', "Z driver"),
            flag('E', "Extruder driver"),
            index('I', "Stepper index", 3.0),
            index('T', "Extruder", 7.0),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M593",
        name: "Input shaping",
        category: C::Motion,
        description: "Cancels the frame's resonance on one axis to reduce ringing. Frequency 0 turns shaping off for that axis.",
        selectors: &['X', 'Y'],
        params: &[
            flag('X', "X axis"),
            flag('Y', "Y axis"),
            num('F', "Frequency", "Hz", 0.0, 200.0, 2),
            num('D', "Damping ratio", "", 0.0, 1.0, 2),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M207",
        name: "Firmware retraction",
        category: C::Retraction,
        description: "How G10 retracts: length, speed and optional Z hop, plus the longer length used before tool swaps.",
        selectors: &[],
        params: &[
            num('S', "Retract length", MM, 0.0, 100.0, 2),
            num('W', "Tool-swap retract length", MM, 0.0, 200.0, 2),
            num('F', "Retract feedrate", MM_MIN, 0.0, 100_000.0, 0),
            num('Z', "Z hop", MM, 0.0, 10.0, 2),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M208",
        name: "Firmware recover",
        category: C::Retraction,
        description: "How G11 undoes a retraction: extra length added back and the recover speed.",
        selectors: &[],
        params: &[
            num('S', "Extra recover length", MM, -100.0, 100.0, 2),
            num('W', "Extra tool-swap recover length", MM, -100.0, 100.0, 2),
            num('F', "Recover feedrate", MM_MIN, 0.0, 100_000.0, 0),
            num('R', "Tool-swap recover feedrate", MM_MIN, 0.0, 100_000.0, 0),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M209",
        name: "Auto-retract",
        category: C::Retraction,
        description: "Treat extrude-only moves in the G-code as firmware retractions and recoveries.",
        selectors: &[],
        params: &[toggle('S', "Enabled")],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M217",
        name: "Tool change",
        category: C::Other,
        description: "Filament handling and parking during tool changes on multi-extruder machines.",
        selectors: &[],
        params: &[
            num('S', "Swap length", MM, 0.0, 500.0, 2),
            num('B', "Extra resume length", MM, 0.0, 100.0, 2),
            num('E', "Prime length", MM, 0.0, 100.0, 2),
            num('P', "Prime feedrate", MM_MIN, 0.0, 100_000.0, 0),
            num('R', "Retract feedrate", MM_MIN, 0.0, 100_000.0, 0),
            num('U', "Unretract feedrate", MM_MIN, 0.0, 100_000.0, 0),
            num('F', "Fan speed", "", 0.0, 255.0, 0),
            num('G', "Fan time", "s", 0.0, 60.0, 0),
            num('X', "Park X", MM, -1000.0, 1000.0, 2),
            num('Y', "Park Y", MM, -1000.0, 1000.0, 2),
            num('Z', "Z raise", MM, 0.0, 100.0, 2),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M218",
        name: "Hotend offset",
        category: C::Other,
        description: "Nozzle position of an additional hotend relative to the first one.",
        selectors: &['T'],
        params: &[
            index('T', "Hotend", 7.0),
            num('X', "X offset", MM, -500.0, 500.0, 2),
            num('Y', "Y offset", MM, -500.0, 500.0, 2),
            num('Z', "Z offset", MM, -10.0, 10.0, 3),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M149",
        name: "Temperature units",
        category: C::Thermal,
        description: "Unit used for temperatures in commands and reports.",
        selectors: &[],
        params: &[
            flag('C', "Celsius"),
            flag('F', "Fahrenheit"),
            flag('K', "Kelvin"),
        ],
        requires_eeprom_save: false,
    },
    SettingDef {
        code: "M665",
        name: "Delta geometry",
        category: C::Motion,
        description: "Arm length, radius, height and the tower angle and diagonal rod corrections of a delta printer. Usually set by delta auto-calibration (G33). Marlin before 2.1 reports its calibration radius as B instead of a rod trim.",
        selectors: &[],
        params: &[
            num('L', "Diagonal rod", MM, 0.0, 1000.0, 2),
            num('R', "Delta radius", MM, 0.0, 1000.0, 2),
            num('H', "Delta height", MM, 0.0, 1000.0, 2),
            num('S', "Segments per second", "", 0.0, 1000.0, 0),
            num('X', "Tower A angle trim", "°", -10.0, 10.0, 2),
            num('Y', "Tower B angle trim", "°", -10.0, 10.0, 2),
            num('Z', "Tower C angle trim", "°", -10.0, 10.0, 2),
            num('A', "Tower A rod trim", MM, -10.0, 10.0, 2),
            num('B', "Tower B rod trim", MM, -500.0, 500.0, 2),
            num('C', "Tower C rod trim", MM, -10.0, 10.0, 2),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M666",
        name: "Endstop adjustment",
        category: C::Homing,
        description: "Per-axis endstop corrections: tower endstop trims on a delta, or the offset between paired endstops on dual-motor axes.",
        selectors: &[],
        params: &[
            num('X', "X adjustment", MM, -50.0, 50.0, 2),
            num('Y', "Y adjustment", MM, -50.0, 50.0, 2),
            num('Z', "Z adjustment", MM, -50.0, 50.0, 2),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M250",
        name: "LCD contrast",
        category: C::Other,
        description: "Contrast of monochrome graphical displays.",
        selectors: &[],
        params: &[num('C', "Contrast", "", 0.0, 255.0, 0)],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M255",
        name: "Display sleep",
        category: C::Other,
        description: "Minutes of inactivity before the display backlight turns off (0 keeps it on).",
        selectors: &[],
        params: &[num('S', "Timeout", "min", 0.0, 99.0, 0)],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M256",
        name: "LCD brightness",
        category: C::Other,
        description: "Backlight brightness of displays that support dimming.",
        selectors: &[],
        params: &[num('B', "Brightness", "", 0.0, 255.0, 0)],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M302",
        name: "Cold extrusion",
        category: C::Extrusion,
        description: "Minimum hotend temperature for extruder moves, or permission to extrude cold (useful for testing without filament).",
        selectors: &[],
        params: &[
            num('S', "Minimum temperature", DEG_C, 0.0, 300.0, 0),
            toggle('P', "Allow cold extrusion"),
        ],
        requires_eeprom_save: false,
    },
    SettingDef {
        code: "M592",
        name: "Nonlinear extrusion",
        category: C::Extrusion,
        description: "Speed-dependent extrusion correction: the flow is scaled by A·v² + B·v + C to compensate for slip at high flow rates.",
        selectors: &[],
        params: &[
            num('A', "Quadratic term", "", -10.0, 10.0, 4),
            num('B', "Linear term", "", -10.0, 10.0, 4),
            num('C', "Constant term", "", 0.0, 10.0, 4),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M86",
        name: "Hotend idle timeout",
        category: C::Thermal,
        description: "Lowers heater targets when the machine sits idle and hot for too long.",
        selectors: &[],
        params: &[
            num('S', "Timeout", "s", 0.0, 86_400.0, 0),
            num('T', "Trigger temperature", DEG_C, 0.0, 450.0, 0),
            num('E', "Idle hotend temperature", DEG_C, 0.0, 450.0, 0),
            num('B', "Idle bed temperature", DEG_C, 0.0, 150.0, 0),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M412",
        name: "Filament runout",
        category: C::Extrusion,
        description: "Runout sensor switch, the filament length still printed after runout is detected, and whether the host handles the pause.",
        selectors: &[],
        params: &[
            toggle('S', "Sensor enabled"),
            num('D', "Runout distance", MM, 0.0, 500.0, 2),
            toggle('H', "Host handling"),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M425",
        name: "Backlash compensation",
        category: C::Motion,
        description: "Extra motion added when an axis reverses direction to take up mechanical slack.",
        selectors: &[],
        params: &[
            num('F', "Correction", "", 0.0, 1.0, 2),
            num('S', "Smoothing distance", MM, 0.0, 10.0, 2),
            num('X', "X backlash", MM, 0.0, 5.0, 3),
            num('Y', "Y backlash", MM, 0.0, 5.0, 3),
            num('Z', "Z backlash", MM, 0.0, 5.0, 3),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M603",
        name: "Filament change lengths",
        category: C::Extrusion,
        description: "How much filament M600 and the load/unload menus pull out and push in.",
        selectors: &['T'],
        params: &[
            index('T', "Extruder", 7.0),
            num('U', "Unload length", MM, 0.0, 1000.0, 2),
            num('L', "Load length", MM, 0.0, 1000.0, 2),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M710",
        name: "Controller fan",
        category: C::Other,
        description: "Electronics cooling fan speeds while steppers are active and idle, automatic mode and switch-off delay.",
        selectors: &[],
        params: &[
            num('S', "Active speed", "", 0.0, 255.0, 0),
            num('I', "Idle speed", "", 0.0, 255.0, 0),
            toggle('A', "Automatic"),
            num('D', "Off delay", "s", 0.0, 3600.0, 0),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M281",
        name: "Servo angles",
        category: C::Probing,
        description: "Deploy and stow angles of a servo, such as the arm of a servo-driven probe.",
        selectors: &['P'],
        params: &[
            index('P', "Servo", 3.0),
            num('L', "Deploy angle", "°", 0.0, 180.0, 0),
            num('U', "Stow angle", "°", 0.0, 180.0, 0),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "M422",
        name: "Z stepper alignment points",
        category: C::Leveling,
        description: "Probe positions (S) and stepper locations (W) used by G34 to square a gantry driven by several Z motors.",
        selectors: &['S', 'W'],
        params: &[
            index('S', "Probe point", 8.0),
            index('W', "Stepper", 8.0),
            num('X', "X position", MM, -1000.0, 1000.0, 2),
            num('Y', "Y position", MM, -1000.0, 1000.0, 2),
        ],
        requires_eeprom_save: true,
    },
    SettingDef {
        code: "G29",
        name: "Mesh point",
        category: C::Leveling,
        description: "One stored point of the leveling mesh as M503 reports it: G29 S3 sets a mesh bed leveling point (I/J from 0, or X/Y from 1 on Marlin 1.1), G29 S4 the mesh Z offset, and G29 W a bilinear point.",
        selectors: &['S', 'W', 'I', 'J', 'X', 'Y'],
        params: &[
            index('S', "Mode", 5.0),
            flag('W', "Write bilinear point"),
            index('I', "Column", 255.0),
            index('J', "Row", 255.0),
            index('X', "Column (from 1)", 255.0),
            index('Y', "Row (from 1)", 255.0),
            num('Z', "Z", MM, -10.0, 10.0, 5),
        ],
        requires_eeprom_save: true,
    },
];

/// All known setting definitions, in display order.
pub fn setting_defs() -> &'static [SettingDef] {
    DEFS
}

/// Definition for a command code (case-insensitive, `"m092"` == `"M92"`).
pub fn setting_def(code: &str) -> Option<&'static SettingDef> {
    let code = normalize_code(code)?;
    DEFS.iter().find(|d| d.code == code)
}

/// Normalise a command code: uppercase letter, number without leading
/// zeros, optional `.sub`. `None` when `code` is not a letter + number.
pub(crate) fn normalize_code(code: &str) -> Option<String> {
    let code = code.trim();
    let mut chars = code.chars();
    let letter = chars.next()?.to_ascii_uppercase();
    if !letter.is_ascii_alphabetic() {
        return None;
    }
    let rest = chars.as_str();
    let (num, sub) = match rest.split_once('.') {
        Some((n, s)) => (n, Some(s)),
        None => (rest, None),
    };
    if num.is_empty() || num.len() > 9 || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u32 = num.parse().ok()?;
    match sub {
        None => Some(format!("{letter}{n}")),
        Some(s) if !s.is_empty() && s.len() <= 3 && s.bytes().all(|b| b.is_ascii_digit()) => {
            Some(format!("{letter}{n}.{s}"))
        }
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn no_duplicate_codes() {
        let mut seen = HashSet::new();
        for def in setting_defs() {
            assert!(seen.insert(def.code), "duplicate {}", def.code);
        }
    }

    #[test]
    fn covers_required_codes() {
        for code in [
            "M92", "M201", "M203", "M204", "M205", "M206", "M301", "M304", "M309", "M851", "M900",
            "M200", "M145", "M420", "M413", "M906", "M913", "M914", "M569", "M593", "M207", "M208",
            "M209", "M217", "M218", "M149", "M665", "M666", "M250", "M255", "M302", "M592",
        ] {
            assert!(setting_def(code).is_some(), "missing {code}");
        }
    }

    #[test]
    fn definitions_are_consistent() {
        for def in setting_defs() {
            assert_eq!(normalize_code(def.code).as_deref(), Some(def.code));
            assert!(
                !def.name.is_empty() && !def.description.is_empty(),
                "{}",
                def.code
            );
            assert!(!def.params.is_empty(), "{} has no params", def.code);
            let mut letters = HashSet::new();
            for p in def.params {
                assert!(p.letter.is_ascii_uppercase(), "{} {}", def.code, p.letter);
                assert!(
                    letters.insert(p.letter),
                    "{} repeats {}",
                    def.code,
                    p.letter
                );
                assert!(p.min <= p.max, "{} {} range", def.code, p.letter);
                assert!(p.min.is_finite() && p.max.is_finite());
                assert!(!p.label.is_empty());
                assert!(p.decimals <= 6);
            }
            for &s in def.selectors {
                assert!(
                    def.param(s).is_some(),
                    "{} selector {} is not a param",
                    def.code,
                    s
                );
                assert!(def.is_selector(s));
            }
        }
    }

    #[test]
    fn heating_and_moving_codes_are_report_only() {
        // Marlin reports Autotemp in M503 as `M104 S210 B250 F0.10`, but
        // `M104 S` sets the hotend target: it is shown, never applied.
        let autotemp = setting_def("M104").unwrap();
        assert_eq!(autotemp.name, "Autotemp");
        assert_eq!(autotemp.category, SettingCategory::Thermal);
        assert!(autotemp.is_report_only());
        for code in ["M104", "m140", "M0190", "G1", "G28"] {
            assert!(is_report_only(code), "{code}");
        }
        for code in ["M92", "M301", "M145", "G21", "G29", "", "x"] {
            assert!(!is_report_only(code), "{code}");
        }
        let reported: Vec<_> = setting_defs()
            .iter()
            .filter(|d| d.is_report_only())
            .map(|d| d.code)
            .collect();
        assert_eq!(reported, ["M104"]);
    }

    #[test]
    fn lookup_is_case_and_zero_insensitive() {
        assert_eq!(setting_def("m92").map(|d| d.code), Some("M92"));
        assert_eq!(setting_def("M092").map(|d| d.code), Some("M92"));
        assert_eq!(setting_def(" M301 ").map(|d| d.name), Some("Hotend PID"));
        assert!(setting_def("M9999").is_none());
        assert!(setting_def("").is_none());
        assert!(setting_def("92").is_none());
        assert_eq!(setting_def("M906").unwrap().selectors, &['T', 'I']);
        assert!(setting_def("M593").unwrap().param('x').unwrap().flag);
    }

    #[test]
    fn normalize() {
        assert_eq!(normalize_code("g01").as_deref(), Some("G1"));
        assert_eq!(normalize_code("G0").as_deref(), Some("G0"));
        assert_eq!(normalize_code("G00").as_deref(), Some("G0"));
        assert_eq!(normalize_code("g29.1").as_deref(), Some("G29.1"));
        assert_eq!(normalize_code("G038.2").as_deref(), Some("G38.2"));
        assert_eq!(normalize_code("t3").as_deref(), Some("T3"));
        assert_eq!(normalize_code("G29."), None);
        assert_eq!(normalize_code("G"), None);
        assert_eq!(normalize_code("GX"), None);
        assert_eq!(normalize_code("1G"), None);
        assert_eq!(normalize_code("Ö1"), None);
        assert_eq!(normalize_code("M99999999999999"), None);
    }
}
