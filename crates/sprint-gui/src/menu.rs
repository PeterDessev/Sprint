//! The main menu bar (File, Printer, View, Tools, Help).
//!
//! [`menu_bar`] only draws; what the user picked comes back as
//! [`MenuCommand`]s for the shell to run.

use std::path::PathBuf;

use egui::{Button, RichText, Ui};
use sprint_host::{JobKind, JobStatus, PrinterState};
use sprint_viewer::ViewPreset;

use crate::context::{AppAction, Tab};
use crate::panels::job_bar::print_blocker;
use crate::theme::{self, Theme};
use crate::{format, icons};

/// What the user picked in the menu.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuCommand {
    /// A regular app action.
    Action(AppAction),
    /// Show or hide the controls sidebar.
    ToggleSidebar,
    /// Show or hide the console.
    ToggleConsole,
    /// Forget the recent files.
    ClearRecent,
    /// Close the window.
    Quit,
    /// M112 now.
    EmergencyStop,
    /// M999.
    RestartFirmware,
    /// Home all axes (G28).
    HomeAll,
    /// Disable the steppers (M84).
    MotorsOff,
    /// Read the firmware info and settings again (M115, M503).
    RefreshFirmware,
    /// Orient the 3D view.
    ViewPreset(ViewPreset),
    /// Fit the 3D view to the toolpath.
    FitView,
    /// Clear the console.
    ClearConsole,
}

/// What the menu needs to know to enable its items.
pub struct MenuInfo<'a> {
    /// The printer state of this frame.
    pub state: &'a PrinterState,
    /// A file is loaded.
    pub has_file: bool,
    /// The loaded file came from disk (can be reloaded).
    pub file_on_disk: bool,
    /// The loaded file is being printed by the host.
    pub printing_file: bool,
    /// A load or analysis runs.
    pub loading: bool,
    /// A connect worker runs.
    pub connecting: bool,
    /// Recent files, newest first.
    pub recent: &'a [PathBuf],
    /// The center tab shown.
    pub tab: Tab,
    /// The controls sidebar is open.
    pub show_sidebar: bool,
    /// The console is open.
    pub show_console: bool,
    /// The active theme.
    pub theme: Theme,
    /// The app icon, drawn left of the menus.
    pub icon: Option<&'a egui::TextureHandle>,
}

/// A menu item with an icon and an optional shortcut; true when clicked.
fn item(ui: &mut Ui, enabled: bool, icon: &str, text: &str, shortcut: Option<&str>) -> bool {
    let mut button = Button::new(icons::with_text(icon, text));
    if let Some(keys) = shortcut {
        button = button.shortcut_text(keys);
    }
    let clicked = ui.add_enabled(enabled, button).clicked();
    if clicked {
        ui.close();
    }
    clicked
}

/// Draw the menu bar; returns what was picked.
pub fn menu_bar(ui: &mut Ui, info: &MenuInfo<'_>) -> Vec<MenuCommand> {
    let mut out = Vec::new();
    egui::MenuBar::new().ui(ui, |ui| {
        if let Some(icon) = info.icon {
            ui.add(egui::Image::new(icon).fit_to_exact_size(egui::vec2(18.0, 18.0)))
                .on_hover_text(format!("Sprint {}", env!("CARGO_PKG_VERSION")));
            ui.add_space(2.0);
        }
        ui.menu_button("File", |ui| file_menu(ui, info, &mut out));
        ui.menu_button("Printer", |ui| printer_menu(ui, info, &mut out));
        ui.menu_button("View", |ui| view_menu(ui, info, &mut out));
        ui.menu_button("Tools", |ui| tools_menu(ui, info, &mut out));
        ui.menu_button("Help", |ui| help_menu(ui, &mut out));
    });
    out
}

fn file_menu(ui: &mut Ui, info: &MenuInfo<'_>, out: &mut Vec<MenuCommand>) {
    use AppAction as A;
    if item(ui, true, icons::OPEN, "Open G-code…", Some("Ctrl+O")) {
        out.push(MenuCommand::Action(A::OpenFileDialog));
    }
    ui.add_enabled_ui(!info.recent.is_empty(), |ui| {
        ui.menu_button(
            icons::with_text(icons::phosphor::CLOCK_COUNTER_CLOCKWISE, "Open recent"),
            |ui| {
                for path in info.recent {
                    let name = format::sanitize(&crate::loader::display_name(path), 60);
                    let r = ui
                        .button(icons::with_text(icons::FILE, &name))
                        .on_hover_text(path.display().to_string());
                    if r.clicked() {
                        out.push(MenuCommand::Action(A::LoadFile(path.clone())));
                        ui.close();
                    }
                }
                ui.separator();
                if item(ui, true, icons::CLEAR, "Clear recent files", None) {
                    out.push(MenuCommand::ClearRecent);
                }
            },
        );
    });
    if item(
        ui,
        info.file_on_disk && !info.printing_file && !info.loading,
        icons::REFRESH,
        "Reload from disk",
        Some("F5"),
    ) {
        out.push(MenuCommand::Action(A::ReloadFile));
    }
    if item(ui, info.has_file, icons::CLOSE, "Close file", None) {
        out.push(MenuCommand::Action(A::CloseFile));
    }
    ui.separator();
    if item(ui, true, icons::phosphor::SIGN_OUT, "Quit", Some("Alt+F4")) {
        out.push(MenuCommand::Quit);
    }
}

fn printer_menu(ui: &mut Ui, info: &MenuInfo<'_>, out: &mut Vec<MenuCommand>) {
    use AppAction as A;
    let state = info.state;
    let open = state.status.is_open();
    let connected = state.is_connected();
    let job = state.job.as_ref().filter(|j| j.status.is_active());
    if open || info.connecting {
        if item(ui, true, icons::DISCONNECT, "Disconnect", None) {
            out.push(MenuCommand::Action(A::Disconnect));
        }
    } else if item(ui, true, icons::CONNECT, "Connect", None) {
        out.push(MenuCommand::Action(A::Connect));
    }
    if item(ui, open, icons::REFRESH, "Reconnect", None) {
        out.push(MenuCommand::Action(A::Reconnect));
    }
    ui.separator();
    let can_print = print_blocker(state, info.has_file).is_none();
    if item(ui, can_print, icons::PLAY, "Start print…", Some("Ctrl+P")) {
        out.push(MenuCommand::Action(A::StartPrintDialog { start_line: 0 }));
    }
    let pausable = job.is_some_and(|j| {
        j.kind != JobKind::SdUpload
            && matches!(
                j.status,
                JobStatus::Printing | JobStatus::Starting | JobStatus::Paused
            )
    });
    let paused = job.is_some_and(|j| j.status.is_paused());
    let (icon, text) = if paused {
        (icons::PLAY, "Resume")
    } else {
        (icons::PAUSE, "Pause")
    };
    if item(ui, pausable, icon, text, Some("Space")) {
        out.push(MenuCommand::Action(if paused {
            A::ResumeJob
        } else {
            A::PauseJob
        }));
    }
    if item(
        ui,
        job.is_some_and(|j| j.status != JobStatus::Cancelling),
        icons::STOP,
        "Cancel print…",
        None,
    ) {
        out.push(MenuCommand::Action(A::CancelJob));
    }
    ui.separator();
    let idle = connected && job.is_none() && !state.halted;
    if item(ui, idle, icons::HOME, "Home all axes (G28)", None) {
        out.push(MenuCommand::HomeAll);
    }
    if item(ui, idle, icons::MOTORS_OFF, "Motors off (M84)", None) {
        out.push(MenuCommand::MotorsOff);
    }
    if item(
        ui,
        connected,
        icons::FIRMWARE,
        "Read firmware info and settings",
        None,
    ) {
        out.push(MenuCommand::RefreshFirmware);
    }
    ui.separator();
    // Only for a halted firmware, like the halted banner: Marlin answers
    // M999 with a resend request even when it runs, which reads as a bad
    // link.
    if item(
        ui,
        open && state.halted,
        icons::RESTART,
        "Restart firmware (M999)",
        None,
    ) {
        out.push(MenuCommand::RestartFirmware);
    }
    let estop = Button::new(
        RichText::new(icons::with_text(icons::ESTOP, "Emergency stop (M112)"))
            .color(theme::ui_palette(ui).danger),
    );
    if ui.add(estop).clicked() {
        out.push(MenuCommand::EmergencyStop);
        ui.close();
    }
}

fn view_menu(ui: &mut Ui, info: &MenuInfo<'_>, out: &mut Vec<MenuCommand>) {
    for tab in Tab::ALL {
        let text = icons::with_text(tab.icon(), tab.title());
        if ui.add(Button::selectable(info.tab == tab, text)).clicked() {
            out.push(MenuCommand::Action(AppAction::ShowTab(tab)));
            ui.close();
        }
    }
    ui.separator();
    let mut sidebar = info.show_sidebar;
    if ui.checkbox(&mut sidebar, "Controls sidebar").clicked() {
        out.push(MenuCommand::ToggleSidebar);
    }
    let mut console = info.show_console;
    if ui.checkbox(&mut console, "Console").clicked() {
        out.push(MenuCommand::ToggleConsole);
    }
    ui.separator();
    ui.menu_button(icons::with_text(icons::VIEW_3D, "3D view"), |ui| {
        for (i, preset) in ViewPreset::ALL.into_iter().enumerate() {
            let keys = (i + 1).to_string();
            if item(
                ui,
                true,
                icons::phosphor::CUBE_FOCUS,
                preset.name(),
                Some(&keys),
            ) {
                out.push(MenuCommand::ViewPreset(preset));
            }
        }
        ui.separator();
        if item(
            ui,
            true,
            icons::phosphor::CORNERS_OUT,
            "Fit to view",
            Some("Z"),
        ) {
            out.push(MenuCommand::FitView);
        }
    });
    ui.separator();
    for theme in Theme::ALL {
        let icon = if theme.is_dark() {
            icons::MOON
        } else {
            icons::SUN
        };
        let text = icons::with_text(icon, &format!("{} theme", theme.name()));
        if ui
            .add(Button::selectable(info.theme == theme, text))
            .clicked()
        {
            out.push(MenuCommand::Action(AppAction::SetTheme(theme)));
            ui.close();
        }
    }
}

fn tools_menu(ui: &mut Ui, info: &MenuInfo<'_>, out: &mut Vec<MenuCommand>) {
    use AppAction as A;
    if item(ui, true, icons::SETTINGS, "Settings…", Some("Ctrl+,")) {
        out.push(MenuCommand::Action(A::OpenSettings));
    }
    if item(ui, true, icons::PRINTER, "Printer profiles…", None) {
        out.push(MenuCommand::Action(A::OpenSettings));
    }
    ui.separator();
    if item(
        ui,
        info.has_file,
        icons::SEARCH,
        "Find in G-code…",
        Some("Ctrl+F"),
    ) {
        out.push(MenuCommand::Action(A::FindInGcode));
    }
    if item(
        ui,
        info.has_file,
        icons::ARROW_RIGHT,
        "Go to line…",
        Some("Ctrl+G"),
    ) {
        out.push(MenuCommand::Action(A::GotoLine));
    }
    if item(
        ui,
        info.has_file && !info.loading,
        icons::LAYERS,
        "Analyze the file again",
        None,
    ) {
        out.push(MenuCommand::Action(A::Reanalyze));
    }
    ui.separator();
    if item(ui, true, icons::CLEAR, "Clear the console", None) {
        out.push(MenuCommand::ClearConsole);
    }
}

fn help_menu(ui: &mut Ui, out: &mut Vec<MenuCommand>) {
    if item(
        ui,
        true,
        icons::phosphor::KEYBOARD,
        "Keyboard shortcuts",
        None,
    ) {
        out.push(MenuCommand::Action(AppAction::OpenShortcuts));
    }
    ui.separator();
    if item(ui, true, icons::INFO, "About Sprint", None) {
        out.push(MenuCommand::Action(AppAction::OpenAbout));
    }
}
