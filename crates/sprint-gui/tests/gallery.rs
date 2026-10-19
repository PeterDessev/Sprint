//! Renders the widget gallery example (both themes side by side) and
//! checks that interacting with it never panics.

mod common;

// The example's UI, reused here; its `main` is not called.
#[allow(dead_code)]
#[path = "../examples/gallery.rs"]
mod gallery;

use common::{fit_state, snapshot};
use egui_kittest::kittest::Queryable as _;
use gallery::{Gallery, GalleryView};
use sprint_gui::theme::Theme;

#[test]
fn gallery_snapshot() {
    let mut h = fit_state(Theme::Dark, 1120.0, Gallery::new(), |ui, g| g.ui(ui));
    snapshot(&mut h, "gallery");
}

#[test]
fn gallery_single_theme_views_render() {
    for view in [GalleryView::Dark, GalleryView::Light] {
        let mut g = Gallery::new();
        g.view = view;
        let mut h = fit_state(Theme::Light, 560.0, g, |ui, g| g.ui(ui));
        h.run_ok();
        assert!(h.query_by_label("Start print").is_some());
    }
}

#[test]
fn gallery_survives_clicks_and_time() {
    let mut g = Gallery::new();
    g.view = GalleryView::Dark;
    let mut h = fit_state(Theme::Dark, 560.0, g, |ui, g| {
        g.tick(0.25);
        g.ui(ui);
    });
    for label in ["Info", "Success", "Warning", "Error", "E-STOP", "Light"] {
        h.get_by_label(label).click();
        h.run_ok();
    }
    // Open and answer both dialogs.
    h.get_by_label("Reset EEPROM…").click();
    h.run_ok();
    h.key_press(egui::Key::Escape);
    h.run_ok();
    assert!(h.query_by_label("Reset all settings?").is_none());
    h.get_by_label("Start print").click();
    h.run_ok();
    h.key_press(egui::Key::Enter);
    h.run_ok();
    assert!(h.query_by_label("Start printing benchy.gcode?").is_none());
}
