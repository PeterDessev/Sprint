//! UI state that survives restarts (stored through eframe's persistence):
//! the current tab, panel visibility, theme, viewer options, console
//! filters and history, and layer slider preferences. Panel sizes are kept
//! by egui's own memory, which eframe persists too.

use serde::{Deserialize, Serialize};
use sprint_viewer::{ColorMode, RenderStyle, ViewMode, ViewerOptions};

use crate::context::Tab;
use crate::theme::Theme;

/// Key of the preferences in eframe's storage.
pub const STORAGE_KEY: &str = "sprint-gui-prefs";

/// Console history entries kept.
pub const HISTORY_LEN: usize = 200;

/// Everything persisted about the UI.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiPrefs {
    /// The center tab shown.
    pub tab: Tab,
    /// The left controls sidebar is open.
    pub show_sidebar: bool,
    /// The right console panel is open.
    pub show_console: bool,
    /// Dark or light.
    pub theme: Theme,
    /// Viewer options.
    pub viewer: ViewerPrefs,
    /// Console filters.
    pub console: ConsolePrefs,
    /// Commands typed into the console, oldest first.
    pub history: Vec<String>,
}

impl Default for UiPrefs {
    fn default() -> Self {
        UiPrefs {
            tab: Tab::View3D,
            show_sidebar: true,
            show_console: true,
            theme: Theme::Dark,
            viewer: ViewerPrefs::default(),
            console: ConsolePrefs::default(),
            history: Vec::new(),
        }
    }
}

impl UiPrefs {
    /// Read from eframe's storage (defaults when missing or unreadable).
    pub fn load(storage: Option<&dyn eframe::Storage>) -> UiPrefs {
        let mut prefs = storage
            .and_then(|s| eframe::get_value::<UiPrefs>(s, STORAGE_KEY))
            .unwrap_or_default();
        prefs.sanitize();
        prefs
    }

    /// Write to eframe's storage.
    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, STORAGE_KEY, self);
    }

    /// Repair values a damaged store may hold.
    pub fn sanitize(&mut self) {
        self.history.retain(|h| !h.trim().is_empty());
        if self.history.len() > HISTORY_LEN {
            let excess = self.history.len() - HISTORY_LEN;
            self.history.drain(..excess);
        }
        for entry in &mut self.history {
            if entry.len() > 4096 {
                let mut cut = 4096;
                while !entry.is_char_boundary(cut) {
                    cut -= 1;
                }
                entry.truncate(cut);
            }
        }
    }
}

/// Persisted viewer options (the subset of [`ViewerOptions`] that is a
/// user preference rather than per-file state).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewerPrefs {
    /// Color mode name (see [`ColorMode::name`]).
    pub color_mode: String,
    /// Draw lines instead of tubes.
    pub lines: bool,
    /// Top-down layer view instead of the 3D orbit view.
    pub layer_view: bool,
    /// Show travel moves.
    pub show_travel: bool,
    /// Show retraction markers.
    pub show_retractions: bool,
    /// Show the bed.
    pub show_bed: bool,
    /// Show the build volume.
    pub show_build_volume: bool,
    /// Show the axes.
    pub show_axes: bool,
    /// Show the tool head marker.
    pub show_tool_head: bool,
    /// The floating legend is open.
    pub show_legend: bool,
    /// The floating file information card is open.
    pub show_file_info: bool,
    /// The top layer follows the print.
    pub follow_print: bool,
    /// The file information card was shown open once: from then on it
    /// opens collapsed for each file (a header line), so it does not cover
    /// the part.
    pub file_info_seen: bool,
}

impl Default for ViewerPrefs {
    fn default() -> Self {
        let o = ViewerOptions::default();
        ViewerPrefs {
            color_mode: o.color_mode.name().to_owned(),
            lines: o.style == RenderStyle::Lines,
            layer_view: o.view_mode == ViewMode::LayerTop,
            show_travel: o.show_travel,
            show_retractions: o.show_retractions,
            show_bed: o.show_bed,
            show_build_volume: o.show_build_volume,
            show_axes: o.show_axes,
            show_tool_head: o.show_tool_head,
            show_legend: true,
            show_file_info: true,
            follow_print: true,
            file_info_seen: false,
        }
    }
}

impl ViewerPrefs {
    /// The color mode (Feature for unknown names).
    pub fn color_mode(&self) -> ColorMode {
        ColorMode::ALL
            .into_iter()
            .find(|m| m.name() == self.color_mode)
            .unwrap_or_default()
    }

    /// Set the color mode.
    pub fn set_color_mode(&mut self, mode: ColorMode) {
        self.color_mode = mode.name().to_owned();
    }

    /// Copy the preferences into viewer options.
    pub fn apply(&self, o: &mut ViewerOptions) {
        o.color_mode = self.color_mode();
        o.style = if self.lines {
            RenderStyle::Lines
        } else {
            RenderStyle::Tubes
        };
        o.view_mode = if self.layer_view {
            ViewMode::LayerTop
        } else {
            ViewMode::Perspective
        };
        o.show_travel = self.show_travel;
        o.show_retractions = self.show_retractions;
        o.show_bed = self.show_bed;
        o.show_build_volume = self.show_build_volume;
        o.show_axes = self.show_axes;
        o.show_tool_head = self.show_tool_head;
    }

    /// Take over what the viewer changed by itself (presets switch to the
    /// 3D view, the legend toggles nothing persisted).
    pub fn update_from(&mut self, o: &ViewerOptions) {
        self.set_color_mode(o.color_mode);
        self.lines = o.style == RenderStyle::Lines;
        self.layer_view = o.view_mode == ViewMode::LayerTop;
    }
}

/// Persisted console filters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConsolePrefs {
    /// Hide automatic traffic (temperature reports and their oks).
    pub hide_auto: bool,
    /// Hide bare "ok" replies.
    pub hide_ok: bool,
    /// Show timestamps.
    pub timestamps: bool,
}

impl Default for ConsolePrefs {
    fn default() -> Self {
        ConsolePrefs {
            hide_auto: true,
            hide_ok: false,
            timestamps: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory eframe store.
    #[derive(Default)]
    struct MemoryStore(std::collections::HashMap<String, String>);

    impl eframe::Storage for MemoryStore {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_owned(), value);
        }
        fn remove_string(&mut self, key: &str) {
            self.0.remove(key);
        }
        fn flush(&mut self) {}
    }

    #[test]
    fn round_trips_through_storage_and_tolerates_damage() {
        let mut prefs = UiPrefs {
            tab: Tab::Gcode,
            show_console: false,
            theme: Theme::Light,
            history: vec!["G28".into(), "M105".into()],
            ..UiPrefs::default()
        };
        prefs.viewer.set_color_mode(ColorMode::Speed);
        let mut store = MemoryStore::default();
        prefs.save(&mut store);
        assert_eq!(UiPrefs::load(Some(&store)), prefs);

        // Missing fields take their defaults.
        eframe::Storage::set_string(&mut store, STORAGE_KEY, "(tab: Tuning)".into());
        let partial = UiPrefs::load(Some(&store));
        assert_eq!(partial.tab, Tab::Tuning);
        assert!(partial.console.hide_auto);
        // Garbage gives the defaults.
        eframe::Storage::set_string(&mut store, STORAGE_KEY, "{{{ nope".into());
        assert_eq!(UiPrefs::load(Some(&store)), UiPrefs::default());
        assert_eq!(UiPrefs::load(None), UiPrefs::default());
    }

    #[test]
    fn viewer_prefs_map_onto_options() {
        let mut prefs = ViewerPrefs {
            lines: true,
            layer_view: true,
            show_travel: true,
            ..ViewerPrefs::default()
        };
        prefs.set_color_mode(ColorMode::Fan);
        let mut o = ViewerOptions::default();
        prefs.apply(&mut o);
        assert_eq!(o.color_mode, ColorMode::Fan);
        assert_eq!(o.style, RenderStyle::Lines);
        assert_eq!(o.view_mode, ViewMode::LayerTop);
        assert!(o.show_travel);
        o.view_mode = ViewMode::Perspective;
        prefs.update_from(&o);
        assert!(!prefs.layer_view);
        prefs.color_mode = "nonsense".into();
        assert_eq!(prefs.color_mode(), ColorMode::Feature);
    }

    #[test]
    fn sanitize_bounds_history() {
        let mut prefs = UiPrefs {
            history: (0..300).map(|i| format!("M{i}")).collect(),
            ..UiPrefs::default()
        };
        prefs.history.push("   ".into());
        prefs.history.push("é".repeat(3000));
        prefs.sanitize();
        assert_eq!(prefs.history.len(), HISTORY_LEN);
        assert!(prefs.history.iter().all(|h| h.len() <= 4096));
        assert!(prefs.history.iter().all(|h| !h.trim().is_empty()));
    }
}
