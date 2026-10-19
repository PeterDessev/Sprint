//! The macros card: defaults for an empty list, the editor (new, edit,
//! validation, test send), the context menu (duplicate, reorder, delete)
//! and confirmation while a job runs. Every change must mark the config
//! dirty. Only the simulator is ever connected.

mod controls_common;

use controls_common::{
    Rig, by_label, by_label_contains, printing, received, settle, sidebar_fixed, wait_for_commands,
    wait_idle,
};
use egui::{Event, Key, Modifiers};
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};
use sprint_gui::theme::Theme;
use sprint_host::Macro;

fn harness(rig: Rig) -> Harness<'static, Rig> {
    let mut h = sidebar_fixed(Theme::Dark, (420.0, 2600.0), rig);
    settle(&mut h);
    h
}

fn names(h: &Harness<'_, Rig>) -> Vec<String> {
    h.state()
        .config
        .macros
        .iter()
        .map(|m| m.name.clone())
        .collect()
}

fn click(h: &mut Harness<'_, Rig>, label: &str) {
    h.get(by_label(label)).click();
    settle(h);
}

/// Open the context menu of the macro tile labelled `tile` and pick the
/// entry whose text contains `entry`. (Other controls may share a tile's
/// name; the macros card is the lowest one.)
fn menu(h: &mut Harness<'_, Rig>, tile: &str, entry: &str) {
    h.query_all(by_label(tile))
        .max_by(|a, b| a.rect().min.y.total_cmp(&b.rect().min.y))
        .unwrap_or_else(|| panic!("no macro {tile:?}"))
        .click_secondary();
    settle(h);
    h.get(by_label_contains(entry)).click();
    settle(h);
}

/// Open a new macro in the editor and fill in its name and G-code.
fn new_macro(h: &mut Harness<'_, Rig>, name: &str, gcode: &str) {
    click(h, "New macro");
    // The "+" button and the editor's title.
    assert_eq!(h.query_all(by_label("New macro")).count(), 2);
    // The name field has the focus; Tab moves on to the G-code.
    h.event(Event::Text(name.into()));
    h.key_press(Key::Tab);
    h.event(Event::Paste(gcode.into()));
    settle(h);
}

fn is_enabled(h: &Harness<'_, Rig>, label: &str) -> bool {
    !h.get(by_label(label)).accesskit_node().is_disabled()
}

#[test]
fn empty_list_offers_the_defaults() {
    let mut rig = Rig::synthetic(controls_common::heating_state());
    rig.config.macros.clear();
    let mut h = harness(rig);
    click(&mut h, "Add defaults");
    assert_eq!(
        names(&h),
        [
            "Preheat PLA",
            "Level corners",
            "Filament change",
            "Report settings"
        ]
    );
    assert!(h.state().dirty);
    // The grid replaces the offer.
    assert!(h.query(by_label("Add defaults")).is_none());
    assert!(h.query(by_label("Level corners…")).is_some());
}

#[test]
fn a_new_macro_is_saved_and_marks_the_config_dirty() {
    let mut h = harness(Rig::synthetic(controls_common::heating_state()));
    let before = names(&h).len();
    new_macro(&mut h, "Purge", "G1 Z0.3 F600\nG1 X5 Y20 E10 F1200 ; prime");
    assert!(is_enabled(&h, "Save"));
    click(&mut h, "Save");
    assert!(h.query(by_label("Edit macro")).is_none(), "editor closed");
    let config = &h.state().config;
    assert_eq!(config.macros.len(), before + 1);
    let saved: &Macro = config.macros.last().expect("saved");
    assert_eq!(saved.name, "Purge");
    assert_eq!(saved.gcode, "G1 Z0.3 F600\nG1 X5 Y20 E10 F1200 ; prime");
    assert!(!saved.confirm);
    assert!(h.state().dirty);
}

#[test]
fn the_editor_needs_a_name_and_commands() {
    let mut h = harness(Rig::synthetic(controls_common::heating_state()));
    click(&mut h, "New macro");
    assert!(!is_enabled(&h, "Save"));
    // A name alone is not enough: comments are no commands.
    h.event(Event::Text("Only comments".into()));
    h.key_press(Key::Tab);
    h.event(Event::Paste("; nothing to send".into()));
    settle(&mut h);
    assert!(!is_enabled(&h, "Save"));
    assert!(h.query(by_label_contains("No commands yet.")).is_some());
    h.event(Event::Paste("\nM84".into()));
    settle(&mut h);
    assert!(is_enabled(&h, "Save"));
    // Escape cancels without saving anything.
    let before = names(&h);
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(h.query(by_label("Save")).is_none(), "editor closed");
    assert_eq!(names(&h), before);
    assert!(!h.state().dirty);
}

#[test]
fn ctrl_enter_saves_with_color_and_confirmation() {
    let mut h = harness(Rig::synthetic(controls_common::heating_state()));
    new_macro(&mut h, "  Spaced name  ", "M117 Hi");
    click(&mut h, "Color #FF6FAE");
    click(&mut h, "Ask before running");
    h.key_press_modifiers(Modifiers::COMMAND, Key::Enter);
    settle(&mut h);
    let saved = h.state().config.macros.last().expect("saved").clone();
    assert_eq!(saved.name, "Spaced name", "the name is trimmed");
    assert_eq!(saved.color, Some([0xFF, 0x6F, 0xAE]));
    assert!(saved.confirm);
}

#[test]
fn a_click_outside_keeps_unsaved_edits() {
    let mut h = harness(Rig::synthetic(controls_common::heating_state()));
    let outside = egui::pos2(6.0, 6.0);
    // Nothing typed yet: a click outside closes the editor.
    click(&mut h, "New macro");
    controls_common::click_at(&mut h, outside);
    settle(&mut h);
    assert!(
        h.query(by_label("Save")).is_none(),
        "an untouched editor closes"
    );
    // With edits the click is ignored ...
    new_macro(&mut h, "Purge", "G1 X5\nG1 X10");
    controls_common::click_at(&mut h, outside);
    settle(&mut h);
    assert!(
        h.query(by_label("Save")).is_some(),
        "the edits were thrown away"
    );
    click(&mut h, "Save");
    assert_eq!(names(&h).last().map(String::as_str), Some("Purge"));
    // ... and so is editing a macro back to what it was.
    menu(&mut h, "Purge", "Edit");
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.event(Event::Text("Prime".into()));
    settle(&mut h);
    controls_common::click_at(&mut h, outside);
    settle(&mut h);
    assert!(
        h.query(by_label("Save")).is_some(),
        "the edit was thrown away"
    );
    // Escape still discards on purpose.
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(h.query(by_label("Save")).is_none());
    assert_eq!(names(&h).last().map(String::as_str), Some("Purge"));
}

/// A long macro scrolls inside the G-code field: the dialog, its title and
/// its buttons stay on screen.
#[test]
fn a_long_macro_keeps_the_editor_on_screen() {
    let mut rig = Rig::synthetic(controls_common::heating_state());
    let gcode: String = (0..80).map(|i| format!("G1 X{i} Y{i} F3000\n")).collect();
    rig.config.macros.insert(
        0,
        Macro {
            name: "Long".into(),
            gcode: gcode.clone(),
            color: None,
            confirm: false,
        },
    );
    let mut h = harness(rig);
    menu(&mut h, "Long", "Edit");
    for height in [700.0, 520.0] {
        h.set_size(egui::vec2(420.0, height));
        settle(&mut h);
        let screen = h.ctx.content_rect();
        for label in ["Edit macro", "Save", "Cancel", "Test send"] {
            let rect = h.get(by_label(label)).rect();
            assert!(
                screen.contains_rect(rect),
                "{label} is off screen at {height} pt: {rect:?}"
            );
        }
    }
    // The pointer reaches Save, and the text is saved unchanged.
    let save = h.get(by_label("Save")).rect().center();
    controls_common::click_at(&mut h, save);
    settle(&mut h);
    assert!(h.query(by_label("Edit macro")).is_none(), "editor closed");
    assert_eq!(h.state().config.macros[0].gcode, gcode);
}

#[test]
fn context_menu_edits_duplicates_reorders_and_deletes() {
    let mut h = harness(Rig::synthetic(controls_common::heating_state()));
    assert_eq!(names(&h), ["Home", "Bed mesh", "Cool down", "Motors off"]);
    // Edit: the editor opens with the macro; the change replaces it.
    menu(&mut h, "Home", "Edit");
    assert!(h.query(by_label("Edit macro")).is_some());
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.event(Event::Text("Home all".into()));
    settle(&mut h);
    click(&mut h, "Save");
    assert_eq!(names(&h)[0], "Home all");
    assert_eq!(h.state().config.macros[0].gcode, "G28");
    // Duplicate inserts a copy right after.
    menu(&mut h, "Motors off", "Duplicate");
    assert_eq!(names(&h)[3..], ["Motors off", "Motors off (copy)"]);
    // Reorder.
    menu(&mut h, "Motors off (copy)", "Move earlier");
    assert_eq!(names(&h)[2..4], ["Cool down", "Motors off (copy)"]);
    menu(&mut h, "Home all", "Move later");
    assert_eq!(names(&h)[..2], ["Bed mesh", "Home all"]);
    // Delete asks first.
    menu(&mut h, "Cool down", "Delete");
    assert!(h.query(by_label("Delete macro “Cool down”?")).is_some());
    click(&mut h, "Cancel");
    assert!(names(&h).contains(&"Cool down".to_owned()));
    menu(&mut h, "Cool down", "Delete");
    click(&mut h, "Delete");
    assert!(!names(&h).contains(&"Cool down".to_owned()));
    assert_eq!(names(&h).len(), 4);
    assert!(h.state().dirty);
}

#[test]
fn test_send_reaches_the_printer_without_saving() {
    let mut h = harness(Rig::simulator());
    let printer = h.state().printer.clone();
    let before = names(&h);
    new_macro(&mut h, "Probe", "M117 Testing\n; a comment\nG28 X");
    h.get(by_label("Test send")).click();
    // Only a few frames: the harness clock runs fast and the toast is brief.
    h.step();
    h.step();
    assert!(h.state().toasts.iter().any(|t| t.title() == "Sent Probe"));
    wait_for_commands(&printer, &["M117 Testing", "G28 X"]);
    settle(&mut h);
    // Still open, nothing saved.
    assert!(h.query(by_label("Save")).is_some());
    click(&mut h, "Cancel");
    assert_eq!(names(&h), before);
    assert!(!h.state().dirty);
}

/// G-code the reference marks as dangerous (M502 factory reset, M112, ...)
/// is never sent without a question, whether the macro asks or not, and
/// also not from the editor's test send.
#[test]
fn dangerous_macros_always_ask_first() {
    let mut rig = Rig::simulator();
    rig.config.macros.push(Macro {
        name: "Factory".into(),
        gcode: "M502\nM503".into(),
        color: None,
        confirm: false,
    });
    let mut h = harness(rig);
    let printer = h.state().printer.clone();
    click(&mut h, "Factory");
    assert!(h.query(by_label("Run “Factory”?")).is_some(), "no question");
    assert!(h.query(by_label_contains("M502 (")).is_some());
    click(&mut h, "Cancel");
    wait_idle(&printer);
    assert!(received(&printer).is_empty(), "{:?}", received(&printer));
    // The editor's test send asks too, above the editor.
    new_macro(&mut h, "Reset", "M502");
    assert!(
        h.query(by_label_contains("Sprint asks before sending it"))
            .is_some()
    );
    h.get(by_label("Test send")).click();
    settle(&mut h);
    assert!(h.query(by_label("Run “Reset”?")).is_some(), "no question");
    click(&mut h, "Run");
    wait_for_commands(&printer, &["M502"]);
    // The editor is still open with the draft.
    assert!(h.query(by_label("Save")).is_some());
    click(&mut h, "Cancel");
    assert!(h.query(by_label("Save")).is_none());
    assert!(!h.state().dirty);
}

/// Macros written in the shell with arguments (`{0}`, `{1=60}`, `{args}`)
/// ask for them in a dialog; the placeholders never reach the printer.
#[test]
fn macros_with_arguments_ask_for_them() {
    let mut rig = Rig::simulator();
    rig.config.macros.push(Macro {
        name: "Heat".into(),
        gcode: "M104 S{0}\nM140 S{1=60}\nM117 {args} at X{x}".into(),
        color: None,
        confirm: false,
    });
    let mut h = harness(rig);
    let printer = h.state().printer.clone();
    // The tile says it asks before running.
    click(&mut h, "Heat…");
    assert!(h.query(by_label("Run “Heat”")).is_some(), "no dialog");
    assert!(h.query(by_label("Argument {0}")).is_some());
    assert!(h.query(by_label("Argument {1}")).is_some());
    // {0} has no default: Run waits for it.
    assert!(!is_enabled(&h, "Run"));
    // The first field has the focus.
    h.event(Event::Text("205".into()));
    settle(&mut h);
    assert!(is_enabled(&h, "Run"));
    // The preview shows what goes out, the host's placeholders rendered.
    assert!(
        h.query(by_label_contains("M104 S205\nM140 S60\nM117 205 60 at X"))
            .is_some()
    );
    click(&mut h, "Run");
    let got = wait_for_commands(&printer, &["M104 S205", "M140 S60"]);
    assert!(!got.iter().any(|c| c.contains('{')), "{got:?}");
    assert!(h.query(by_label("Run “Heat”")).is_none());
    // Cancel sends nothing.
    wait_idle(&printer);
    controls_common::sim(&printer).clear_received();
    click(&mut h, "Heat…");
    click(&mut h, "Cancel");
    wait_idle(&printer);
    assert!(received(&printer).is_empty(), "{:?}", received(&printer));
}

#[test]
fn macros_ask_first_while_a_job_runs() {
    let mut rig = Rig::simulator();
    rig.tweak = Some(printing);
    let mut h = harness(rig);
    let printer = h.state().printer.clone();
    // "Home" does not ask normally, but a job is running.
    click(&mut h, "Home");
    assert!(h.query(by_label("Run “Home”?")).is_some());
    click(&mut h, "Cancel");
    wait_idle(&printer);
    assert!(received(&printer).is_empty(), "{:?}", received(&printer));
    click(&mut h, "Home");
    click(&mut h, "Run");
    wait_for_commands(&printer, &["G28"]);
    // The editor's test send is not offered during a job.
    click(&mut h, "New macro");
    h.event(Event::Text("Probe".into()));
    h.key_press(Key::Tab);
    h.event(Event::Paste("G28".into()));
    settle(&mut h);
    assert!(!is_enabled(&h, "Test send"));
}
