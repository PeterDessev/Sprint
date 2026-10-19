//! The application shell, [`SprintApp`]: it owns the printer, the
//! configuration, the loaded file and every panel, lays out the main
//! window, and routes [`AppAction`]s, keyboard shortcuts, dropped files,
//! dialogs, printer events and persistence.
//!
//! Each frame:
//!
//! 1. take a [`PrinterState`] snapshot and collect what the workers
//!    reported (connects, file loads, printer events, console lines);
//! 2. handle the close request, shortcuts and dropped files (before any
//!    widget sees the keys);
//! 3. draw the panels (menu, connection bar, halted banner, job and status
//!    bars, controls, console, tabs), then the dialogs and toasts;
//! 4. run the [`AppAction`]s the panels pushed;
//! 5. update the window title and save the configuration (debounced).
//!
//! Nothing here blocks the UI thread: connecting, loading, analysis, file
//! dialogs and configuration saves run on worker threads.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use egui::{
    Align, Align2, Color32, CornerRadius, Frame, Key, KeyboardShortcut, Layout, Margin, Modifiers,
    Rect, RichText, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, Ui, ViewportCommand,
    vec2,
};
use sprint_gcode::{AnalyzeOptions, GcodeFile, KinematicLimits};
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{
    AppConfig, ConnectTarget, Event, HostPrompt, JobKind, JobStatus, Printer, PrinterState,
};
use sprint_viewer::ViewPreset;

use crate::config_store::ConfigSaver;
use crate::connection::{ConnectOutcome, Connector, PortScanner};
use crate::context::{AppAction, AppCtx, LoadedFile, Tab};
use crate::dialogs::{self, StartPrintDialog};
use crate::loader::{self, FileLoader, FileWatch, LoadEvent};
use crate::menu::{self, MenuCommand, MenuInfo};
use crate::notify::Notifier;
use crate::panels::job_bar::{self, print_blocker};
use crate::panels::viewer_tab::ViewerExtras;
use crate::panels::{
    BedMeshTab, CalibrationTab, ConnectionBar, ConsolePanel, ControlsSidebar, GcodeTab, JobBar,
    PrinterInfoTab, SdCardTab, TemperatureGraphTab, TuningTab, ViewerTab,
};
use crate::prefs::UiPrefs;
use crate::settings::{SettingsDialog, SettingsOutcome};
use crate::theme::{self, Palette, TextRole, Theme, Tone};
use crate::widgets::{ConfirmDialog, DialogResult, IconButton, Toasts};
use crate::{GuiOptions, app_icon, format, icons, native_dialog};

/// Printer events handled per frame at most (the rest wait a frame).
const EVENTS_PER_FRAME: usize = 20_000;
/// Height of the center tab strip.
const TAB_STRIP_H: f32 = 38.0;
/// Horizontal padding inside a tab.
const TAB_PAD: f32 = 12.0;
/// Horizontal padding inside an icon-only tab.
const ICON_TAB_PAD: f32 = 9.0;
/// Width of the panel toggles at the right end of the tab strip.
const TOGGLES_W: f32 = 64.0;

/// How the app starts (see [`SprintApp::new`]).
#[derive(Clone, Debug)]
pub struct AppOptions {
    /// G-code file to load at startup.
    pub file: Option<PathBuf>,
    /// Connect to this virtual printer at startup (instead of the active
    /// profile's auto-connect).
    pub simulator: Option<SimConfig>,
    /// The configuration to start with.
    pub config: AppConfig,
    /// Where the configuration is saved; `None` keeps it in memory.
    pub config_path: Option<PathBuf>,
    /// Enumerate serial ports in the background (names only, ports are
    /// never opened).
    pub scan_ports: bool,
    /// Use native file dialogs. Without them "Open" only counts the
    /// request (headless runs and tests).
    pub native_dialogs: bool,
    /// Restore the UI preferences from eframe's storage.
    pub restore_prefs: bool,
}

impl AppOptions {
    /// The desktop app: the configuration from [`AppConfig::path`] (saved
    /// back there), port scanning and native dialogs.
    pub fn desktop(gui: GuiOptions) -> AppOptions {
        AppOptions {
            file: gui.file,
            simulator: gui.simulator.then(SimConfig::default),
            config: AppConfig::load(),
            config_path: Some(AppConfig::path()),
            scan_ports: true,
            native_dialogs: true,
            restore_prefs: true,
        }
    }

    /// A self-contained app for tests, demos and screenshots: the default
    /// configuration kept in memory, no port scans, no native dialogs, no
    /// restored preferences.
    pub fn headless() -> AppOptions {
        AppOptions {
            file: None,
            simulator: None,
            config: AppConfig::default(),
            config_path: None,
            scan_ports: false,
            native_dialogs: false,
            restore_prefs: false,
        }
    }
}

/// The dialogs the shell can show (see [`SprintApp::dialog`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogKind {
    /// Start print.
    StartPrint,
    /// Settings and printer profiles.
    Settings,
    /// About Sprint.
    About,
    /// Keyboard shortcuts.
    Shortcuts,
    /// "Cancel the print?"
    ConfirmCancel,
    /// "Disconnect and stop the job?"
    ConfirmDisconnect,
    /// "Quit and stop the print?"
    ConfirmQuit,
}

enum Modal {
    StartPrint(Box<StartPrintDialog>),
    Settings(Box<SettingsDialog>),
    About,
    Shortcuts,
    ConfirmCancel,
    ConfirmDisconnect { reconnect: bool },
    ConfirmQuit,
}

impl Modal {
    fn kind(&self) -> DialogKind {
        match self {
            Modal::StartPrint(_) => DialogKind::StartPrint,
            Modal::Settings(_) => DialogKind::Settings,
            Modal::About => DialogKind::About,
            Modal::Shortcuts => DialogKind::Shortcuts,
            Modal::ConfirmCancel => DialogKind::ConfirmCancel,
            Modal::ConfirmDisconnect { .. } => DialogKind::ConfirmDisconnect,
            Modal::ConfirmQuit => DialogKind::ConfirmQuit,
        }
    }
}

/// What the last analysis of the loaded file was asked for.
#[derive(Clone, Debug, PartialEq)]
struct AnalysisRequest {
    options: AnalyzeOptions,
    /// The profile settings it was made for (filament diameter and
    /// kinematics), to notice a switch to another profile.
    profile: (f32, Option<KinematicLimits>),
}

impl AnalysisRequest {
    fn new(profile: &sprint_host::PrinterProfile, state: &PrinterState) -> AnalysisRequest {
        AnalysisRequest {
            options: loader::analyze_options(profile, state),
            profile: (profile.filament_diameter, profile.kinematics),
        }
    }
}

/// What became of the firmware's current host prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptChoice {
    Answered,
    Hidden,
}

/// Everything lent to the panels through [`AppCtx`].
struct Core {
    printer: Printer,
    state: PrinterState,
    config: AppConfig,
    config_dirty: bool,
    file: Option<LoadedFile>,
    toasts: Toasts,
    actions: Vec<AppAction>,
    selected_line: Option<u32>,
}

impl Core {
    fn ctx(&mut self) -> AppCtx<'_> {
        AppCtx {
            printer: &self.printer,
            state: &self.state,
            config: &mut self.config,
            config_dirty: &mut self.config_dirty,
            file: self.file.as_ref(),
            toasts: &mut self.toasts,
            actions: &mut self.actions,
            selected_line: self.selected_line,
        }
    }

    /// The loaded file is being printed (or uploaded) by the host.
    fn printing_loaded_file(&self) -> bool {
        match (&self.state.job, &self.file) {
            (Some(job), Some(file)) => {
                job.status.is_active() && format::job_matches_file(job, file.file.name())
            }
            _ => false,
        }
    }

    /// A job the host streams is running (closing the app would stop it).
    fn host_job_running(&self) -> bool {
        self.state
            .job
            .as_ref()
            .is_some_and(|j| j.status.is_active() && j.kind != JobKind::SdPrint)
    }
}

/// Every panel of the window.
struct Panels {
    connection: ConnectionBar,
    controls: ControlsSidebar,
    viewer: ViewerTab,
    gcode: GcodeTab,
    temperature: TemperatureGraphTab,
    printer_info: PrinterInfoTab,
    tuning: TuningTab,
    calibration: CalibrationTab,
    bed_mesh: BedMeshTab,
    sd_card: SdCardTab,
    console: ConsolePanel,
    job_bar: JobBar,
}

/// The Sprint desktop application (an [`eframe::App`]).
pub struct SprintApp {
    core: Core,
    panels: Panels,
    prefs: UiPrefs,
    events: Receiver<Event>,
    notifier: Notifier,
    connector: Connector,
    loader: FileLoader,
    watch: FileWatch,
    saver: ConfigSaver,
    modal: Option<Modal>,
    prompt: Option<(HostPrompt, PromptChoice)>,
    thumbnail: Option<TextureHandle>,
    icon: TextureHandle,
    title: String,
    clock: Instant,
    requested_analysis: Option<AnalysisRequest>,
    reconnect_pending: bool,
    /// The target and options of the last connect, for Reconnect.
    last_connect: Option<(ConnectTarget, sprint_host::ConnectOptions)>,
    quit_confirmed: bool,
    file_dialog: Option<Receiver<Option<PathBuf>>>,
    file_dialog_requests: usize,
    native_dialogs: bool,
    state_override: Option<PrinterState>,
    toast_top: f32,
    egui_ctx: egui::Context,
}

impl SprintApp {
    /// Create the app from eframe's creation context: install the theme
    /// and fonts, start the printer handle (disconnected), and queue the
    /// startup file and connection from `options`. A real port is opened
    /// at startup only when the active profile enables `auto_connect`.
    pub fn new(cc: &eframe::CreationContext<'_>, options: AppOptions) -> SprintApp {
        let ctx = cc.egui_ctx.clone();
        let prefs = if options.restore_prefs {
            UiPrefs::load(cc.storage)
        } else {
            UiPrefs::default()
        };
        theme::setup(&ctx, prefs.theme);
        let mut config = options.config;
        // At least one profile, and a valid index.
        let _ = config.active_mut();
        let printer = Printer::with_console_capacity(config.console_max_lines);
        printer.set_profile(config.active().clone());
        let events = printer.subscribe();
        let waker_ctx = ctx.clone();
        printer.set_waker(move || waker_ctx.request_repaint());

        let viewer = ViewerTab::new(cc.wgpu_render_state.as_ref(), prefs.viewer.clone());
        let console = ConsolePanel::new(
            config.console_max_lines,
            prefs.console.clone(),
            prefs.history.clone(),
        );
        let scanner = PortScanner::new(options.scan_ports, Some(ctx.clone()));
        let icon = ctx.load_texture(
            "sprint-app-icon",
            app_icon::color_image(128),
            TextureOptions::LINEAR,
        );
        let mut app = SprintApp {
            core: Core {
                printer,
                state: PrinterState::default(),
                config,
                config_dirty: false,
                file: None,
                toasts: Toasts::new(),
                actions: Vec::new(),
                selected_line: None,
            },
            panels: Panels {
                connection: ConnectionBar::new(scanner),
                controls: ControlsSidebar::default(),
                viewer,
                gcode: GcodeTab::default(),
                temperature: TemperatureGraphTab::default(),
                printer_info: PrinterInfoTab::default(),
                tuning: TuningTab::default(),
                calibration: CalibrationTab::default(),
                bed_mesh: BedMeshTab::default(),
                sd_card: SdCardTab::default(),
                console,
                job_bar: JobBar::default(),
            },
            prefs,
            events,
            notifier: Notifier::default(),
            connector: Connector::new(Some(ctx.clone())),
            loader: FileLoader::new(Some(ctx.clone())),
            watch: FileWatch::new(Some(ctx.clone())),
            saver: ConfigSaver::new(options.config_path),
            modal: None,
            prompt: None,
            thumbnail: None,
            icon,
            title: "Sprint".to_owned(),
            clock: Instant::now(),
            requested_analysis: None,
            reconnect_pending: false,
            last_connect: None,
            quit_confirmed: false,
            file_dialog: None,
            file_dialog_requests: 0,
            native_dialogs: options.native_dialogs,
            state_override: None,
            toast_top: crate::widgets::TOAST_OFFSET.y,
            egui_ctx: ctx,
        };
        if let Some(path) = options.file {
            app.core.actions.push(AppAction::LoadFile(path));
        }
        if let Some(sim) = options.simulator {
            let connect_options = app.core.config.active().options.clone();
            app.open_link(ConnectTarget::Simulator(sim), connect_options);
        } else if app.core.config.active().auto_connect {
            app.core.actions.push(AppAction::Connect);
        }
        app
    }

    // ----- accessors (tests, demos and embedding) -----

    /// The printer handle.
    pub fn printer(&self) -> &Printer {
        &self.core.printer
    }

    /// The printer state used for the last frame.
    pub fn state(&self) -> &PrinterState {
        &self.core.state
    }

    /// The configuration.
    pub fn config(&self) -> &AppConfig {
        &self.core.config
    }

    /// The loaded file.
    pub fn file(&self) -> Option<&LoadedFile> {
        self.core.file.as_ref()
    }

    /// True while a file loads or is analyzed.
    pub fn is_loading(&self) -> bool {
        self.loader.is_busy()
    }

    /// The center tab shown.
    pub fn tab(&self) -> Tab {
        self.prefs.tab
    }

    /// The selected source line.
    pub fn selected_line(&self) -> Option<u32> {
        self.core.selected_line
    }

    /// The open dialog.
    pub fn dialog(&self) -> Option<DialogKind> {
        self.modal.as_ref().map(Modal::kind)
    }

    /// The open start-print dialog.
    pub fn start_print_dialog_mut(&mut self) -> Option<&mut StartPrintDialog> {
        match &mut self.modal {
            Some(Modal::StartPrint(d)) => Some(d),
            _ => None,
        }
    }

    /// The open settings dialog.
    pub fn settings_dialog_mut(&mut self) -> Option<&mut SettingsDialog> {
        match &mut self.modal {
            Some(Modal::Settings(d)) => Some(d),
            _ => None,
        }
    }

    /// The toasts on screen.
    pub fn toasts(&self) -> &Toasts {
        &self.core.toasts
    }

    /// The toasts, mutable (e.g. to clear them before a screenshot).
    pub fn toasts_mut(&mut self) -> &mut Toasts {
        &mut self.core.toasts
    }

    /// The console panel.
    pub fn console(&self) -> &ConsolePanel {
        &self.panels.console
    }

    /// The console panel, mutable.
    pub fn console_mut(&mut self) -> &mut ConsolePanel {
        &mut self.panels.console
    }

    /// The G-code tab.
    pub fn gcode_tab(&self) -> &GcodeTab {
        &self.panels.gcode
    }

    /// The 3D view tab.
    pub fn viewer_tab(&self) -> &ViewerTab {
        &self.panels.viewer
    }

    /// The 3D view tab, mutable.
    pub fn viewer_tab_mut(&mut self) -> &mut ViewerTab {
        &mut self.panels.viewer
    }

    /// The UI preferences as they would be persisted now.
    pub fn ui_prefs(&self) -> UiPrefs {
        UiPrefs {
            viewer: self.panels.viewer.prefs.clone(),
            console: self.panels.console.prefs.clone(),
            history: self.panels.console.history().to_vec(),
            ..self.prefs.clone()
        }
    }

    /// The loaded file changed on disk since it was loaded (a reload is
    /// offered).
    pub fn file_changed_on_disk(&self) -> bool {
        self.watch.is_changed()
    }

    /// True while a connect or disconnect runs on its worker thread, or a
    /// reconnect waits for them.
    pub fn connection_busy(&self) -> bool {
        !self.connector.is_idle() || self.reconnect_pending
    }

    /// The window title of the last frame.
    pub fn window_title(&self) -> &str {
        &self.title
    }

    /// How often "Open" was requested while native dialogs are off.
    pub fn file_dialog_requests(&self) -> usize {
        self.file_dialog_requests
    }

    /// Where the configuration is saved (`None`: kept in memory).
    pub fn config_path(&self) -> Option<&std::path::Path> {
        self.saver.path()
    }

    /// Queue an action; it runs at the end of the next frame.
    pub fn push_action(&mut self, action: AppAction) {
        self.core.actions.push(action);
    }

    /// Show `state` instead of the printer's real state (demos, screenshots
    /// and tests of states that are hard to reach); `None` goes back.
    pub fn set_state_override(&mut self, state: Option<PrinterState>) {
        self.state_override = state;
    }

    /// Open the controls sidebar and the console, or close them.
    pub fn set_side_panels(&mut self, sidebar: bool, console: bool) {
        self.prefs.show_sidebar = sidebar;
        self.prefs.show_console = console;
    }

    // ----- the frame -----

    /// Run one frame of the app in `ui` (the window's root).
    pub fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        let secs = now.saturating_duration_since(self.clock).as_secs_f64();
        self.begin_frame(&ctx, secs);
        self.handle_close_request(&ctx);
        self.shortcuts(&ctx);
        self.handle_dropped_files(&ctx);
        self.draw(ui);
        self.show_dialogs(&ctx);
        // Below the top bars (and the halted banner) so toasts never cover
        // their buttons.
        let toasts = std::mem::take(&mut self.core.toasts);
        self.core.toasts = toasts.anchor(Align2::RIGHT_TOP, vec2(-16.0, self.toast_top));
        self.core.toasts.show(&ctx);
        self.drop_overlay(&ctx);
        self.run_actions(&ctx);
        self.end_frame(&ctx, now);
    }

    fn begin_frame(&mut self, ctx: &egui::Context, secs: f64) {
        self.core.state = match &self.state_override {
            Some(state) => state.clone(),
            None => self.core.printer.snapshot(),
        };
        // A cancelled connect may still fail; its failure event comes before
        // its outcome, so it arrives while the cancel is pending or in the
        // frame the outcome is collected (whatever the thread timing).
        let mut cancelled = self.connector.is_cancelling();
        for outcome in self.connector.poll() {
            cancelled |= matches!(outcome, ConnectOutcome::Cancelled { .. });
            self.connect_outcome(outcome);
        }
        self.poll_events(secs, cancelled);
        if self.reconnect_pending && self.connector.is_idle() {
            self.reconnect_pending = false;
            self.connect_again();
        }
        self.poll_file_dialog();
        self.poll_loader(ctx);
        self.panels.console.sync(&self.core.printer);
        {
            let c = self.core.ctx();
            self.panels.connection.tick(&c, secs);
        }
        self.watch.poll(secs);
        match (&self.core.state.prompt, &self.prompt) {
            (None, _) => self.prompt = None,
            (Some(current), Some((seen, _))) if current != seen => self.prompt = None,
            _ => {}
        }
        self.maybe_reanalyze();
    }

    fn end_frame(&mut self, ctx: &egui::Context, now: Instant) {
        let name = self.core.file.as_ref().map(|f| f.file.name().to_owned());
        let title = format::window_title(name.as_deref(), self.core.state.job.as_ref());
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        if self.core.config_dirty {
            self.core.config_dirty = false;
            self.core
                .printer
                .set_profile(self.core.config.active().clone());
            self.saver.touch(now);
        }
        if let Some(error) = self.saver.tick(&self.core.config, now) {
            self.core
                .toasts
                .error("Could not save the settings")
                .message(format::sanitize(&error, 200));
        }
        if self.saver.is_pending() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        if self.core.file.as_ref().is_some_and(|f| f.path.is_some()) {
            ctx.request_repaint_after(Duration::from_secs_f64(FileWatch::INTERVAL));
        }
        if !self.core.state.status.is_open() {
            ctx.request_repaint_after(Duration::from_secs_f64(
                crate::connection::PORT_REFRESH_SECS,
            ));
        }
    }

    // ----- workers -----

    fn connect_outcome(&mut self, outcome: ConnectOutcome) {
        match outcome {
            ConnectOutcome::Opened { .. } => {}
            ConnectOutcome::Failed { label, error } => {
                // A failed open also sets the status to Failed, which the
                // notifier reports; only report what it cannot see.
                let reported = self
                    .core
                    .printer
                    .with_state(|s| matches!(s.status, sprint_host::ConnectionStatus::Failed(_)));
                if !reported {
                    self.core
                        .toasts
                        .error(format!(
                            "Could not connect to {}",
                            format::sanitize(&label, 60)
                        ))
                        .message(format::sanitize(&error, 200));
                }
                self.reconnect_pending = false;
            }
            // The user cancelled: not an error. A link that opened all the
            // same is closed again (a pending reconnect follows once it is).
            ConnectOutcome::Cancelled { opened, .. } => {
                if opened {
                    self.connector.disconnect(&self.core.printer);
                }
            }
            // A pending reconnect starts once nothing is opening or closing
            // any more (see begin_frame).
            ConnectOutcome::Closed => {}
        }
    }

    fn poll_file_dialog(&mut self) {
        let Some(rx) = &self.file_dialog else {
            return;
        };
        match rx.try_recv() {
            Ok(picked) => {
                self.file_dialog = None;
                if let Some(path) = picked {
                    self.load_file(path);
                }
            }
            Err(crossbeam_channel::TryRecvError::Disconnected) => self.file_dialog = None,
            Err(crossbeam_channel::TryRecvError::Empty) => {}
        }
    }

    fn poll_loader(&mut self, ctx: &egui::Context) {
        let mut abandoned = false;
        for event in self.loader.poll() {
            // The rest of an abandoned job's batch (progress, its toolpath)
            // belongs to the version that was not taken.
            if abandoned {
                continue;
            }
            match event {
                LoadEvent::Opened { file, path, reload } => {
                    if self.reload_lands_on_a_print(&file, reload) {
                        self.loader.abandon();
                        abandoned = true;
                        self.core
                            .toasts
                            .warning("The file is printing")
                            .message("Reload it from disk after the print.");
                    } else {
                        self.file_opened(file, path, reload);
                    }
                }
                LoadEvent::Progress(p) => {
                    if let Some(f) = &mut self.core.file {
                        f.analysis_progress = Some(p);
                    }
                }
                LoadEvent::Analyzed {
                    toolpath,
                    thumbnail,
                    seconds,
                } => {
                    log::info!(
                        "analyzed {} layers in {seconds:.2} s",
                        toolpath.layers.len()
                    );
                    if let Some(f) = &mut self.core.file {
                        f.toolpath = Some(toolpath);
                        f.analysis_progress = None;
                    }
                    self.thumbnail = thumbnail.map(|t| {
                        ctx.load_texture("sprint-thumbnail", t.image, TextureOptions::LINEAR)
                    });
                }
                LoadEvent::Failed { name, error } => {
                    if let Some(f) = &mut self.core.file {
                        f.analysis_progress = None;
                    }
                    self.core
                        .toasts
                        .error(format!("Could not open {}", format::sanitize(&name, 60)))
                        .message(format::sanitize(&error, 300));
                }
                LoadEvent::Cancelled { name } => {
                    if let Some(f) = &mut self.core.file {
                        f.analysis_progress = None;
                    }
                    self.core
                        .toasts
                        .info("Loading cancelled")
                        .message(format::sanitize(&name, 60));
                }
            }
        }
    }

    /// A reload (started before the print) brought a new version of the
    /// file being printed: the print streams the version on screen, which
    /// stays until the print is over.
    fn reload_lands_on_a_print(&self, file: &Arc<GcodeFile>, reload: bool) -> bool {
        reload
            && self
                .core
                .file
                .as_ref()
                .is_some_and(|current| !Arc::ptr_eq(&current.file, file))
            && self.core.printing_loaded_file()
    }

    fn file_opened(&mut self, file: Arc<GcodeFile>, path: Option<PathBuf>, reload: bool) {
        // A re-analysis of the file shown keeps its toolpath until the new
        // one is ready.
        if let Some(current) = &mut self.core.file
            && Arc::ptr_eq(&current.file, &file)
        {
            current.analysis_progress = Some(0.0);
            return;
        }
        if let Some(path) = &path {
            self.core.config.push_recent(path.clone());
            self.core.config_dirty = true;
        }
        self.watch.watch(path.clone());
        let lines = file.len();
        if !(reload
            && self
                .core
                .selected_line
                .is_some_and(|l| (l as usize) < lines))
        {
            self.core.selected_line = None;
        }
        self.thumbnail = None;
        self.core.file = Some(LoadedFile {
            path,
            file,
            toolpath: None,
            analysis_progress: Some(0.0),
        });
        if !reload && !matches!(self.prefs.tab, Tab::View3D | Tab::Gcode) {
            self.prefs.tab = Tab::View3D;
        }
    }

    /// Toast what the printer reported. `cancelled`: a connect the user
    /// cancelled is ending, its failure is not news.
    fn poll_events(&mut self, secs: f64, cancelled: bool) {
        let job_name = self.core.state.job.as_ref().map(|j| j.name.clone());
        for event in self.events.try_iter().take(EVENTS_PER_FRAME) {
            if cancelled
                && matches!(
                    event,
                    Event::Status(sprint_host::ConnectionStatus::Failed(_))
                )
            {
                continue;
            }
            if let Some(notice) = self.notifier.notice(&event, secs, job_name.as_deref()) {
                notice.show(&mut self.core.toasts);
            }
        }
    }

    /// Analyze the file again (only between jobs) when the active profile's
    /// filament or kinematics changed since the last analysis, or with the
    /// firmware's motion limits once they are known and the profile has no
    /// limits of its own. A disconnect keeps an analysis made with the
    /// firmware's limits.
    fn maybe_reanalyze(&mut self) {
        let state = &self.core.state;
        let Some(requested) = &self.requested_analysis else {
            return;
        };
        if self.loader.is_busy()
            || state.job_active()
            || !self
                .core
                .file
                .as_ref()
                .is_some_and(|f| f.toolpath.is_some())
        {
            return;
        }
        let profile = self.core.config.active();
        let wanted = AnalysisRequest::new(profile, state);
        let firmware_limits =
            state.is_connected() && state.settings_loaded && profile.kinematics.is_none();
        if wanted.options != requested.options
            && (wanted.profile != requested.profile || firmware_limits)
        {
            self.reanalyze();
        }
    }

    // ----- actions -----

    fn run_actions(&mut self, ctx: &egui::Context) {
        // Actions may queue more actions; bound the rounds.
        for _ in 0..4 {
            let actions = std::mem::take(&mut self.core.actions);
            if actions.is_empty() {
                break;
            }
            for action in actions {
                self.handle_action(ctx, action);
            }
        }
    }

    fn handle_action(&mut self, ctx: &egui::Context, action: AppAction) {
        match action {
            AppAction::OpenFileDialog => self.open_file_dialog(),
            AppAction::LoadFile(path) => self.load_file(path),
            AppAction::ShowTab(tab) => self.prefs.tab = tab,
            AppAction::SelectLine(line) => self.select_line(line),
            AppAction::StartPrintDialog { start_line } => self.open_start_print(start_line),
            AppAction::OpenSettings => self.open_settings(),
            AppAction::ReloadFile => self.reload_file(),
            AppAction::CloseFile => self.close_file(),
            AppAction::Connect => self.connect(),
            AppAction::Disconnect => {
                if self.core.host_job_running() {
                    self.open_modal(Modal::ConfirmDisconnect { reconnect: false });
                } else {
                    self.reconnect_pending = false;
                    self.connector.disconnect(&self.core.printer);
                }
            }
            AppAction::Reconnect => {
                if self.core.host_job_running() {
                    self.open_modal(Modal::ConfirmDisconnect { reconnect: true });
                } else {
                    self.reconnect();
                }
            }
            AppAction::PauseJob => self.core.printer.pause(),
            AppAction::ResumeJob => self.core.printer.resume(),
            AppAction::CancelJob => {
                if self.core.state.job_active() {
                    self.open_modal(Modal::ConfirmCancel);
                }
            }
            AppAction::FindInGcode => {
                if self.core.file.is_some() {
                    self.prefs.tab = Tab::Gcode;
                    self.panels.gcode.open_find();
                }
            }
            AppAction::GotoLine => {
                if self.core.file.is_some() {
                    self.prefs.tab = Tab::Gcode;
                    self.panels.gcode.open_goto();
                }
            }
            AppAction::OpenShortcuts => self.open_modal(Modal::Shortcuts),
            AppAction::OpenAbout => self.open_modal(Modal::About),
            AppAction::SetTheme(theme) => self.set_theme(ctx, theme),
            AppAction::CancelLoading => self.loader.cancel(),
            AppAction::Reanalyze => self.reanalyze(),
        }
    }

    fn open_modal(&mut self, modal: Modal) {
        if self.modal.is_none() {
            self.modal = Some(modal);
        }
    }

    /// Connect to the active profile's target.
    fn connect(&mut self) {
        if self.connector.is_connecting() || self.core.printer.with_state(|s| s.status.is_open()) {
            return;
        }
        let profile = self.core.config.active().clone();
        self.core.printer.set_profile(profile.clone());
        self.open_link(profile.connection, profile.options);
    }

    /// Open `target` on a worker thread and remember it for Reconnect.
    fn open_link(&mut self, target: ConnectTarget, options: sprint_host::ConnectOptions) {
        self.last_connect = Some((target.clone(), options.clone()));
        self.connector.connect(&self.core.printer, target, options);
    }

    /// Open the last connection again (the virtual printer of a `--sim`
    /// start stays the virtual printer, whatever the profile names), or
    /// the profile's target when nothing was connected yet.
    fn connect_again(&mut self) {
        if self.connector.is_connecting() || self.core.printer.with_state(|s| s.status.is_open()) {
            return;
        }
        match self.last_connect.clone() {
            Some((target, options)) => self.open_link(target, options),
            None => self.connect(),
        }
    }

    fn reconnect(&mut self) {
        let open = self.core.printer.with_state(|s| s.status.is_open());
        if open || self.connector.is_connecting() {
            self.connector.disconnect(&self.core.printer);
        }
        if self.connector.is_idle() {
            self.connect_again();
        } else {
            // Opened again once nothing is opening or closing any more
            // (see begin_frame): a connect started meanwhile would be
            // cancelled by the disconnect still running.
            self.reconnect_pending = true;
        }
    }

    fn open_file_dialog(&mut self) {
        if !self.native_dialogs {
            self.file_dialog_requests += 1;
            return;
        }
        if self.file_dialog.is_some() {
            return;
        }
        let directory = self
            .core
            .file
            .as_ref()
            .and_then(|f| f.path.as_deref())
            .or_else(|| self.core.config.recent_files.first().map(PathBuf::as_path))
            .and_then(|p| p.parent())
            .map(std::path::Path::to_path_buf);
        self.file_dialog = Some(native_dialog::spawn(
            Some(self.egui_ctx.clone()),
            move || native_dialog::pick_gcode_file(directory.as_deref()),
        ));
    }

    /// Load (and analyze) `path`; refused for the file being printed.
    fn load_file(&mut self, path: PathBuf) {
        let same = self
            .core
            .file
            .as_ref()
            .and_then(|f| f.path.as_ref())
            .is_some_and(|p| *p == path);
        if same && self.core.printing_loaded_file() {
            self.core
                .toasts
                .warning("The file is printing")
                .message("Reload it from disk after the print.");
            return;
        }
        let request = AnalysisRequest::new(self.core.config.active(), &self.core.state);
        let options = request.options.clone();
        self.requested_analysis = Some(request);
        self.loader.load(path, options, same);
    }

    fn reload_file(&mut self) {
        match self.core.file.as_ref().and_then(|f| f.path.clone()) {
            Some(path) => self.load_file(path),
            None => {
                if self.core.file.is_some() {
                    self.core
                        .toasts
                        .info("Nothing to reload")
                        .message("This file was not opened from disk.");
                }
            }
        }
    }

    fn reanalyze(&mut self) {
        let Some(file) = &self.core.file else {
            return;
        };
        let request = AnalysisRequest::new(self.core.config.active(), &self.core.state);
        let options = request.options.clone();
        self.requested_analysis = Some(request);
        self.loader
            .analyze(Arc::clone(&file.file), file.path.clone(), options, true);
    }

    fn close_file(&mut self) {
        if self.loader.is_busy() {
            self.loader.cancel();
        }
        self.core.file = None;
        self.core.selected_line = None;
        self.thumbnail = None;
        self.watch.watch(None);
    }

    fn select_line(&mut self, line: u32) {
        let Some(file) = &self.core.file else {
            return;
        };
        let line = line.min(file.file.len().saturating_sub(1) as u32);
        self.core.selected_line = Some(line);
        // The tab the click came from shows the line already.
        if self.prefs.tab != Tab::Gcode {
            self.panels.gcode.scroll_to(line);
        }
        if self.prefs.tab != Tab::View3D
            && let Some(toolpath) = &file.toolpath
        {
            self.panels.viewer.reveal_line(toolpath, line);
        }
    }

    fn open_start_print(&mut self, start_line: usize) {
        let Some(file) = &self.core.file else {
            self.core
                .toasts
                .warning("Nothing to print")
                .message("Open a G-code file first.");
            return;
        };
        if let Some(reason) = print_blocker(&self.core.state, true) {
            self.core
                .toasts
                .warning("Cannot print right now")
                .message(reason);
            return;
        }
        let dialog = StartPrintDialog::new(start_line, file);
        self.open_modal(Modal::StartPrint(Box::new(dialog)));
    }

    fn open_settings(&mut self) {
        let ports = self
            .panels
            .connection
            .scanner
            .ports()
            .iter()
            .map(|p| p.name.clone())
            .collect();
        let dialog = SettingsDialog::new(&self.core.config, self.prefs.theme, ports)
            .with_config_path(self.saver.path().map(std::path::Path::to_path_buf));
        self.open_modal(Modal::Settings(Box::new(dialog)));
    }

    fn apply_settings(&mut self, ctx: &egui::Context, dialog: SettingsDialog) {
        let theme = dialog.theme;
        // Only what the dialog edits: recent files and macros may have
        // changed while it was open (a file dropped meanwhile).
        let draft = dialog.draft;
        self.core.config.profiles = draft.profiles;
        self.core.config.active_profile = draft.active_profile;
        self.core.config.console_max_lines = draft.console_max_lines;
        self.core.config_dirty = true;
        let lines = self.core.config.console_max_lines;
        self.core.printer.set_console_capacity(lines);
        self.panels.console.set_capacity(lines);
        self.core
            .printer
            .set_profile(self.core.config.active().clone());
        if theme != self.prefs.theme {
            self.set_theme(ctx, theme);
        }
        // A changed filament or kinematics re-analyzes the file between
        // jobs (see maybe_reanalyze).
        self.core.toasts.success("Settings saved");
    }

    fn set_theme(&mut self, ctx: &egui::Context, theme: Theme) {
        self.prefs.theme = theme;
        theme::apply(ctx, theme);
    }

    fn menu_command(&mut self, ctx: &egui::Context, command: MenuCommand) {
        let printer = &self.core.printer;
        match command {
            MenuCommand::Action(action) => self.core.actions.push(action),
            MenuCommand::ToggleSidebar => self.prefs.show_sidebar = !self.prefs.show_sidebar,
            MenuCommand::ToggleConsole => self.prefs.show_console = !self.prefs.show_console,
            MenuCommand::ClearRecent => {
                self.core.config.recent_files.clear();
                self.core.config_dirty = true;
            }
            MenuCommand::Quit => ctx.send_viewport_cmd(ViewportCommand::Close),
            MenuCommand::EmergencyStop => self.emergency_stop(),
            MenuCommand::RestartFirmware => {
                printer.firmware_restart();
                self.core.toasts.info("Restarting the firmware (M999)");
            }
            MenuCommand::HomeAll => printer.home(sprint_host::Axes::ALL),
            MenuCommand::MotorsOff => printer.motors_off(),
            MenuCommand::RefreshFirmware => {
                printer.request_firmware_info();
                printer.request_settings();
            }
            MenuCommand::ViewPreset(preset) => {
                self.prefs.tab = Tab::View3D;
                self.panels.viewer.set_view(preset);
            }
            MenuCommand::FitView => {
                self.prefs.tab = Tab::View3D;
                self.panels.viewer.fit();
            }
            MenuCommand::ClearConsole => self.panels.console.clear(printer),
        }
    }

    fn emergency_stop(&mut self) {
        if self.core.printer.with_state(|s| s.status.is_open()) {
            self.core.printer.emergency_stop();
            self.core
                .toasts
                .warning("Emergency stop sent")
                .message("M112: the firmware halted. Restart it (M999) or reconnect.");
        } else {
            self.core
                .toasts
                .warning("Emergency stop: not connected")
                .message("No printer is connected, nothing was sent.");
        }
    }

    // ----- input -----

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested())
            && !self.quit_confirmed
            && self.core.host_job_running()
        {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.modal = Some(Modal::ConfirmQuit);
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        // Also while a panel asks something (the console's "send a
        // dangerous command?"): its keys belong to that question.
        let panel_question = ctx.memory(|m| m.top_modal_layer().is_some());
        if self.modal.is_some() || self.prompt_is_open() || panel_question {
            return;
        }
        let typing = ctx.egui_wants_keyboard_input();
        let job = self
            .core
            .state
            .job
            .as_ref()
            .filter(|j| j.status.is_active() && j.kind != JobKind::SdUpload);
        let pause_action = job.and_then(|j| match j.status {
            JobStatus::Paused => Some(AppAction::ResumeJob),
            JobStatus::Printing | JobStatus::Starting => Some(AppAction::PauseJob),
            _ => None,
        });
        let in_viewer = self.prefs.tab == Tab::View3D && self.core.file.is_some();
        let mut actions = Vec::new();
        let mut preset = None;
        let mut fit = false;
        ctx.input_mut(|i| {
            let command = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
            for (key, action) in [
                (Key::O, AppAction::OpenFileDialog),
                (Key::P, AppAction::StartPrintDialog { start_line: 0 }),
                (Key::F, AppAction::FindInGcode),
                (Key::G, AppAction::GotoLine),
                (Key::Comma, AppAction::OpenSettings),
            ] {
                if i.consume_shortcut(&command(key)) {
                    actions.push(action);
                }
            }
            if i.consume_key(Modifiers::NONE, Key::F5) {
                actions.push(AppAction::ReloadFile);
            }
            if typing {
                return;
            }
            if let Some(action) = &pause_action
                && i.consume_key(Modifiers::NONE, Key::Space)
            {
                actions.push(action.clone());
            }
            if in_viewer {
                let digits = [
                    Key::Num1,
                    Key::Num2,
                    Key::Num3,
                    Key::Num4,
                    Key::Num5,
                    Key::Num6,
                    Key::Num7,
                ];
                for (key, p) in digits.into_iter().zip(ViewPreset::ALL) {
                    if i.consume_key(Modifiers::NONE, key) {
                        preset = Some(p);
                    }
                }
                if i.consume_key(Modifiers::NONE, Key::Z) {
                    fit = true;
                }
            }
        });
        self.core.actions.extend(actions);
        if let Some(p) = preset {
            self.panels.viewer.set_view(p);
        }
        if fit {
            self.panels.viewer.fit();
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        let Some(first) = dropped.first() else {
            return;
        };
        if dropped.len() > 1 {
            self.core
                .toasts
                .info("One file at a time")
                .message("Only the first dropped file is opened.");
        }
        let path = first.path().to_path_buf();
        if path.as_os_str().is_empty() {
            return;
        }
        self.core.actions.push(AppAction::LoadFile(path));
    }

    fn prompt_is_open(&self) -> bool {
        self.core.state.prompt.is_some() && self.prompt.is_none()
    }

    // ----- drawing -----

    fn draw(&mut self, ui: &mut Ui) {
        let p = theme::ui_palette(ui);
        let ctx = ui.ctx().clone();
        egui::Panel::top("sprint-menu-bar")
            .frame(
                Frame::new()
                    .fill(p.bg)
                    .inner_margin(Margin::symmetric(8, 3)),
            )
            .show(ui, |ui| {
                let info = MenuInfo {
                    state: &self.core.state,
                    has_file: self.core.file.is_some(),
                    file_on_disk: self.core.file.as_ref().is_some_and(|f| f.path.is_some()),
                    printing_file: self.core.printing_loaded_file(),
                    loading: self.loader.is_busy(),
                    connecting: self.connector.is_connecting(),
                    recent: &self.core.config.recent_files,
                    tab: self.prefs.tab,
                    show_sidebar: self.prefs.show_sidebar,
                    show_console: self.prefs.show_console,
                    theme: self.prefs.theme,
                    icon: Some(&self.icon),
                };
                let commands = menu::menu_bar(ui, &info);
                for command in commands {
                    self.menu_command(&ctx, command);
                }
            });
        egui::Panel::top("sprint-connection-bar")
            .frame(
                Frame::new()
                    .fill(p.panel)
                    .inner_margin(Margin::symmetric(10, 7)),
            )
            .show(ui, |ui| {
                let connecting = self.connector.is_connecting();
                let mut c = self.core.ctx();
                let open = self.last_connect.as_ref().map(|(target, _)| target);
                self.panels.connection.ui(ui, &mut c, connecting, open);
            });
        if self.core.state.halted && self.core.state.status.is_open() {
            egui::Panel::top("sprint-halted-banner")
                .frame(
                    Frame::new()
                        .fill(theme::mix(
                            p.panel,
                            p.danger,
                            if p.dark { 0.16 } else { 0.10 },
                        ))
                        .inner_margin(Margin::symmetric(14, 6)),
                )
                .show(ui, |ui| {
                    let mut c = self.core.ctx();
                    dialogs::halted_banner(ui, &mut c);
                });
        }
        egui::Panel::bottom("sprint-status-bar")
            .frame(
                Frame::new()
                    .fill(p.bg)
                    .inner_margin(Margin::symmetric(12, 0)),
            )
            .exact_size(26.0)
            .show(ui, |ui| {
                let c = self.core.ctx();
                job_bar::status_bar(ui, &c);
            });
        egui::Panel::bottom("sprint-job-bar")
            .frame(
                Frame::new()
                    .fill(p.panel)
                    .inner_margin(Margin::symmetric(12, 8)),
            )
            .exact_size(64.0)
            .show(ui, |ui| {
                let load = self.loader.status();
                let mut c = self.core.ctx();
                self.panels.job_bar.ui(ui, &mut c, load.as_ref());
            });
        egui::Panel::left("sprint-controls")
            .frame(
                Frame::new()
                    .fill(p.panel)
                    .inner_margin(Margin::symmetric(10, 10)),
            )
            .size_range(280.0..=480.0)
            .default_size(300.0)
            .show_collapsible(ui, &mut self.prefs.show_sidebar, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("sprint-controls-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut c = self.core.ctx();
                        self.panels.controls.ui(ui, &mut c);
                    });
            });
        egui::Panel::right("sprint-console")
            .frame(Frame::new().fill(p.panel))
            .size_range(300.0..=760.0)
            .default_size(360.0)
            .show_collapsible(ui, &mut self.prefs.show_console, |ui| {
                let mut c = self.core.ctx();
                self.panels.console.ui(ui, &mut c);
            });
        egui::CentralPanel::no_frame().show(ui, |ui| {
            let rect = ui.max_rect();
            self.toast_top = rect.top() + TAB_STRIP_H + 8.0;
            ui.painter().rect_filled(rect, 0.0, p.bg);
            self.tab_strip(ui, &p);
            self.notices(ui, &p);
            self.tab_content(ui);
        });
    }

    fn tab_strip(&mut self, ui: &mut Ui, p: &Palette) {
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), TAB_STRIP_H), Sense::hover());
        ui.painter().rect_filled(rect, 0.0, p.panel);
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0, p.border),
        );
        let mut strip = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect.shrink2(vec2(6.0, 0.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        strip.spacing_mut().item_spacing.x = 2.0;
        // Labels where they fit: all tabs, else the selected one, else none
        // (the icons keep their titles as tooltips).
        let current = self.prefs.tab;
        let room = rect.width() - 12.0 - TOGGLES_W;
        let width_with = |labels: &dyn Fn(Tab) -> bool| -> f32 {
            Tab::ALL
                .iter()
                .map(|&t| tab_width(&strip, t, t == current, labels(t)) + 2.0)
                .sum()
        };
        let labels: fn(Tab, Tab) -> bool = if width_with(&|_| true) <= room {
            |_, _| true
        } else if width_with(&|t| t == current) <= room {
            |t, current| t == current
        } else {
            |_, _| false
        };
        for tab in Tab::ALL {
            let selected = tab == current;
            if tab_button(&mut strip, tab, selected, labels(tab, current), p).clicked() {
                self.prefs.tab = tab;
            }
        }
        strip.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if ui
                .add(
                    IconButton::new(icons::CONSOLE)
                        .ghost()
                        .small()
                        .selected(self.prefs.show_console)
                        .tooltip("Show or hide the console"),
                )
                .clicked()
            {
                self.prefs.show_console = !self.prefs.show_console;
            }
            if ui
                .add(
                    IconButton::new(icons::phosphor::SIDEBAR_SIMPLE)
                        .ghost()
                        .small()
                        .selected(self.prefs.show_sidebar)
                        .tooltip("Show or hide the controls"),
                )
                .clicked()
            {
                self.prefs.show_sidebar = !self.prefs.show_sidebar;
            }
        });
    }

    /// Strips above the tab content: the file changed on disk, a hidden
    /// host prompt.
    fn notices(&mut self, ui: &mut Ui, p: &Palette) {
        if self.watch.is_changed()
            && let Some(file) = &self.core.file
        {
            let printing = self.core.printing_loaded_file();
            let name = format::sanitize(file.file.name(), 60);
            let text = if printing {
                format!("{name} changed on disk. Reload it after the print.")
            } else {
                format!("{name} changed on disk.")
            };
            let mut buttons = vec![("Keep this version", false)];
            if !printing {
                buttons.insert(0, ("Reload", true));
            }
            match notice_strip(ui, p, Tone::Info, icons::REFRESH, &text, &buttons) {
                Some(0) if !printing => {
                    self.watch.dismiss();
                    self.core.actions.push(AppAction::ReloadFile);
                }
                Some(_) => self.watch.dismiss(),
                None => {}
            }
        }
        if let (Some(prompt), Some((_, PromptChoice::Hidden))) =
            (&self.core.state.prompt, &self.prompt)
        {
            let message = if prompt.message.trim().is_empty() {
                "The printer is waiting for you.".to_owned()
            } else {
                format!(
                    "The printer is waiting: {}",
                    format::sanitize(&prompt.message, 120)
                )
            };
            if notice_strip(
                ui,
                p,
                Tone::Warning,
                icons::BELL,
                &message,
                &[("Answer", true)],
            )
            .is_some()
            {
                self.prompt = None;
            }
        }
    }

    fn tab_content(&mut self, ui: &mut Ui) {
        let load = self.loader.status();
        let mut c = self.core.ctx();
        match self.prefs.tab {
            Tab::View3D => {
                let extras = ViewerExtras {
                    thumbnail: self.thumbnail.as_ref(),
                    load: load.as_ref(),
                };
                self.panels.viewer.ui(ui, &mut c, extras);
            }
            Tab::Gcode => self.panels.gcode.ui(ui, &mut c),
            Tab::Temperature => self.panels.temperature.ui(ui, &mut c),
            Tab::Printer => self.panels.printer_info.ui(ui, &mut c),
            Tab::Tuning => self.panels.tuning.ui(ui, &mut c),
            Tab::Calibration => self.panels.calibration.ui(ui, &mut c),
            Tab::BedMesh => self.panels.bed_mesh.ui(ui, &mut c),
            Tab::SdCard => self.panels.sd_card.ui(ui, &mut c),
        }
    }

    fn drop_overlay(&self, ctx: &egui::Context) {
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if !hovering {
            return;
        }
        let p = theme::palette(ctx);
        let screen = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("sprint-drop-overlay"),
        ));
        painter.rect_filled(screen, 0.0, theme::with_alpha(p.bg, 0.82));
        let card = Rect::from_center_size(screen.center(), vec2(360.0, 150.0));
        painter.rect_filled(card, CornerRadius::same(14), p.surface);
        painter.rect_stroke(
            card,
            CornerRadius::same(14),
            Stroke::new(2.0, p.accent),
            StrokeKind::Inside,
        );
        painter.text(
            card.center() - vec2(0.0, 22.0),
            Align2::CENTER_CENTER,
            icons::OPEN,
            egui::FontId::new(36.0, crate::fonts::icon_family()),
            p.accent,
        );
        painter.text(
            card.center() + vec2(0.0, 26.0),
            Align2::CENTER_CENTER,
            "Drop to open the G-code file",
            egui::FontId::proportional(16.0),
            p.text,
        );
    }

    // ----- dialogs -----

    fn show_dialogs(&mut self, ctx: &egui::Context) {
        if let Some(modal) = self.modal.take() {
            self.modal = self.show_modal(ctx, modal);
        }
        self.show_prompt(ctx);
    }

    fn show_modal(&mut self, ctx: &egui::Context, modal: Modal) -> Option<Modal> {
        match modal {
            Modal::StartPrint(mut dialog) => {
                let file = self.core.file.clone()?;
                // Another file (or a reload) replaced the one it was opened
                // for: its start line and layer mean nothing there.
                if !dialog.is_for(&file) {
                    return None;
                }
                let mut c = self.core.ctx();
                match dialog.show(ctx, &mut c, &file) {
                    Some(_) => None,
                    None => Some(Modal::StartPrint(dialog)),
                }
            }
            Modal::Settings(mut dialog) => {
                let open = self.core.state.status.is_open() || self.connector.is_connecting();
                match dialog.show(ctx, open) {
                    Some(SettingsOutcome::Saved) => {
                        self.apply_settings(ctx, *dialog);
                        None
                    }
                    Some(SettingsOutcome::Cancelled) => None,
                    None => Some(Modal::Settings(dialog)),
                }
            }
            Modal::About => (!dialogs::about_dialog(ctx, Some(&self.icon))).then_some(Modal::About),
            Modal::Shortcuts => (!dialogs::shortcuts_dialog(ctx)).then_some(Modal::Shortcuts),
            Modal::ConfirmCancel => {
                let job = self
                    .core
                    .state
                    .job
                    .as_ref()
                    .filter(|j| j.status.is_active())?;
                let message = format!(
                    "{} stops at {}%. The cancel script parks the head and turns the heaters off.",
                    format::sanitize(&job.name, 60),
                    format::progress_percent(job.progress)
                );
                let answer = ConfirmDialog::new("sprint-cancel-job", "Cancel the print?")
                    .message(&message)
                    .confirm_label("Cancel print")
                    .cancel_label("Keep printing")
                    .danger(true)
                    .show(ctx);
                match answer {
                    Some(DialogResult::Confirmed) => {
                        self.core.printer.cancel();
                        self.core.toasts.info("Cancelling the print");
                        None
                    }
                    Some(DialogResult::Cancelled) => None,
                    None => Some(Modal::ConfirmCancel),
                }
            }
            Modal::ConfirmDisconnect { reconnect } => {
                if !self.core.host_job_running() {
                    if reconnect {
                        self.reconnect();
                    } else {
                        self.connector.disconnect(&self.core.printer);
                    }
                    return None;
                }
                let title = if reconnect {
                    "Reconnect and stop the job?"
                } else {
                    "Disconnect and stop the job?"
                };
                let answer = ConfirmDialog::new("sprint-disconnect", title)
                    .message(
                        "The host is streaming a job; closing the connection stops it where it is.",
                    )
                    .confirm_label(if reconnect { "Reconnect" } else { "Disconnect" })
                    .cancel_label("Keep printing")
                    .danger(true)
                    .show(ctx);
                match answer {
                    Some(DialogResult::Confirmed) => {
                        if reconnect {
                            self.reconnect();
                        } else {
                            self.reconnect_pending = false;
                            self.connector.disconnect(&self.core.printer);
                        }
                        None
                    }
                    Some(DialogResult::Cancelled) => None,
                    None => Some(Modal::ConfirmDisconnect { reconnect }),
                }
            }
            Modal::ConfirmQuit => {
                let answer = ConfirmDialog::new("sprint-quit", "Quit and stop the print?")
                    .message("Sprint is streaming a print to the printer. Quitting stops it where it is.")
                    .confirm_label("Quit")
                    .cancel_label("Keep printing")
                    .danger(true)
                    .show(ctx);
                match answer {
                    Some(DialogResult::Confirmed) => {
                        self.quit_confirmed = true;
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                        None
                    }
                    Some(DialogResult::Cancelled) => None,
                    None => Some(Modal::ConfirmQuit),
                }
            }
        }
    }

    fn show_prompt(&mut self, ctx: &egui::Context) {
        let Some(prompt) = self.core.state.prompt.clone() else {
            return;
        };
        if self.prompt.is_some() {
            return;
        }
        if let Some(answer) = dialogs::prompt_dialog(ctx, &prompt) {
            let choice = if answer == usize::MAX {
                PromptChoice::Hidden
            } else {
                self.core.printer.answer_prompt(answer);
                PromptChoice::Answered
            };
            self.prompt = Some((prompt, choice));
        }
    }
}

impl eframe::App for SprintApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.ui_prefs().save(storage);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::palette_for(&self.egui_ctx, self.prefs.theme)
            .bg
            .to_normalized_gamma_f32()
    }
}

impl Drop for SprintApp {
    fn drop(&mut self) {
        if self.core.config_dirty {
            self.saver.touch(Instant::now());
        }
        if let Err(e) = self.saver.flush(&self.core.config) {
            log::warn!("could not save the configuration: {e}");
        }
        self.loader.cancel();
        self.core.printer.disconnect();
    }
}

/// Galleys of a tab's icon and title.
fn tab_galleys(ui: &Ui, tab: Tab, selected: bool) -> (Arc<egui::Galley>, Arc<egui::Galley>) {
    let font = if selected {
        egui::FontId::new(13.5, theme::font(ui, TextRole::Title).family)
    } else {
        egui::FontId::proportional(13.5)
    };
    let icon_font = egui::FontId::new(15.0, crate::fonts::icon_family());
    // Placeholder color: the color passed when painting applies.
    let painter = ui.painter();
    (
        painter.layout_no_wrap(tab.icon().to_owned(), icon_font, Color32::PLACEHOLDER),
        painter.layout_no_wrap(tab.title().to_owned(), font, Color32::PLACEHOLDER),
    )
}

/// Width of a tab, with or without its title (icon-only tabs are
/// narrower).
fn tab_width(ui: &Ui, tab: Tab, selected: bool, label: bool) -> f32 {
    let (icon, text) = tab_galleys(ui, tab, selected);
    if label {
        TAB_PAD + icon.size().x + 7.0 + text.size().x + TAB_PAD
    } else {
        2.0 * ICON_TAB_PAD + icon.size().x
    }
}

/// One tab of the center tab strip (icon, and the title when `label`).
fn tab_button(ui: &mut Ui, tab: Tab, selected: bool, label: bool, p: &Palette) -> egui::Response {
    let (icon, text) = tab_galleys(ui, tab, selected);
    let width = tab_width(ui, tab, selected, label);
    let height = ui.available_height();
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            true,
            selected,
            tab.title(),
        )
    });
    if ui.is_rect_visible(rect) {
        let hovered = response.hovered();
        let color = if selected || hovered {
            p.text
        } else {
            p.text_dim
        };
        if hovered && !selected {
            ui.painter().rect_filled(
                rect.shrink2(vec2(0.0, 5.0)),
                CornerRadius::same(theme::RADIUS),
                theme::with_alpha(p.text, 0.05),
            );
        }
        let icon_color = if selected { p.accent } else { color };
        let y = rect.center().y;
        let pad = if label { TAB_PAD } else { ICON_TAB_PAD };
        ui.painter().galley(
            egui::pos2(rect.left() + pad, y - icon.size().y / 2.0),
            Arc::clone(&icon),
            icon_color,
        );
        if label {
            ui.painter().galley(
                egui::pos2(
                    rect.left() + TAB_PAD + icon.size().x + 7.0,
                    y - text.size().y / 2.0,
                ),
                text,
                color,
            );
        }
        if selected {
            let bar = Rect::from_min_max(
                egui::pos2(rect.left() + 8.0, rect.bottom() - 2.5),
                egui::pos2(rect.right() - 8.0, rect.bottom()),
            );
            ui.painter()
                .rect_filled(bar, CornerRadius::same(1), p.accent);
        }
    }
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    if label {
        response
    } else {
        response.on_hover_text(tab.title())
    }
}

/// A slim notice strip with buttons; returns the index of the clicked
/// button.
fn notice_strip(
    ui: &mut Ui,
    p: &Palette,
    tone: Tone,
    icon: &str,
    text: &str,
    buttons: &[(&str, bool)],
) -> Option<usize> {
    let mut clicked = None;
    Frame::new()
        .fill(theme::mix(
            p.bg,
            p.tone(tone),
            if p.dark { 0.14 } else { 0.10 },
        ))
        .inner_margin(Margin::symmetric(12, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                // Buttons first, from the right; the text is cut to the room
                // left of them (in full on hover).
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    for (i, (label, primary)) in buttons.iter().enumerate() {
                        let mut button = crate::widgets::ActionButton::new(label).small();
                        if *primary {
                            button = button.primary();
                        }
                        if ui.add(button).clicked() {
                            clicked = Some(i);
                        }
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new(icon).color(p.tone(tone)));
                        ui.add(egui::Label::new(RichText::new(text).color(p.text)).truncate());
                    });
                });
            });
        });
    clicked
}
