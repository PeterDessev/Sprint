//! The connection bar below the menu: profile, connection target (serial
//! port, virtual printer or TCP), baud rate, connect / disconnect, status,
//! firmware, link quality and the always-visible E-STOP.

use egui::{Align, Color32, Layout, Rect, RichText, Sense, Stroke, Ui, vec2};
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{BAUD_RATES, ConnectTarget, ConnectionStatus, DEFAULT_BAUD, PortInfo};

use crate::connection::{LinkQuality, PortScanner};
use crate::context::{AppAction, AppCtx};
use crate::theme::{self, Palette, Tone};
use crate::widgets::{ActionButton, EStopButton, StatusPill};
use crate::{format, icons};

/// Width of the link quality bars.
const QUALITY_W: f32 = 22.0;
/// Text size of the firmware label.
const FIRMWARE_FONT: f32 = 13.0;

/// The kind of connection target chosen in the bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    /// A serial port.
    Serial,
    /// A TCP bridge.
    Network,
    /// The built-in virtual printer.
    Simulator,
}

impl TargetKind {
    /// The kind of `target`.
    pub fn of(target: &ConnectTarget) -> TargetKind {
        match target {
            ConnectTarget::Serial { .. } => TargetKind::Serial,
            ConnectTarget::Tcp { .. } => TargetKind::Network,
            ConnectTarget::Simulator(_) => TargetKind::Simulator,
        }
    }
}

/// Label and tone of the status pill.
pub fn status_pill(status: &ConnectionStatus, halted: bool) -> (&'static str, Tone, bool) {
    if halted && status.is_open() {
        return ("Halted", Tone::Danger, false);
    }
    match status {
        ConnectionStatus::Disconnected => ("Disconnected", Tone::Neutral, false),
        ConnectionStatus::Connecting => ("Connecting", Tone::Warning, true),
        ConnectionStatus::Handshaking => ("Handshaking", Tone::Warning, true),
        ConnectionStatus::Connected => ("Connected", Tone::Success, false),
        ConnectionStatus::Failed(_) => ("Failed", Tone::Danger, false),
    }
}

/// Parse a `host:port` field into a TCP target.
pub fn parse_host_port(text: &str) -> Option<ConnectTarget> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let text = text.strip_prefix("tcp://").unwrap_or(text);
    match ConnectTarget::parse(&format!("tcp://{text}")) {
        Ok(target @ ConnectTarget::Tcp { .. }) => Some(target),
        _ => None,
    }
}

/// The text of the `host:port` field for a TCP target.
pub fn host_port_text(target: &ConnectTarget) -> String {
    match target {
        ConnectTarget::Tcp { host, port } if host.contains(':') => format!("[{host}]:{port}"),
        ConnectTarget::Tcp { host, port } => format!("{host}:{port}"),
        _ => String::new(),
    }
}

/// The connection bar.
pub struct ConnectionBar {
    /// Serial port enumeration.
    pub scanner: PortScanner,
    /// Link quality grading.
    pub quality: LinkQuality,
    tcp_text: String,
    tcp_valid: bool,
    /// The profile and address the TCP field shows (an address set
    /// elsewhere, or another profile, replaces what was typed).
    tcp_for: Option<(usize, ConnectTarget)>,
}

impl ConnectionBar {
    /// A bar using `scanner` for the port list.
    pub fn new(scanner: PortScanner) -> ConnectionBar {
        ConnectionBar {
            scanner,
            quality: LinkQuality::default(),
            tcp_text: String::new(),
            tcp_valid: true,
            tcp_for: None,
        }
    }

    /// Per-frame bookkeeping (port refresh while disconnected, link
    /// quality); `now` in seconds.
    pub fn tick(&mut self, ctx: &AppCtx<'_>, now: f64) {
        let open = ctx.state.status.is_open();
        self.scanner.poll(now, !open);
        self.quality
            .update(&ctx.state.link, ctx.state.status.is_connected());
    }

    /// Draw the bar. `connecting` is true while a connect worker runs,
    /// `closing` while a disconnect waits for a cancelled job to stop;
    /// `open` is the target of the open (or opening) connection, shown
    /// instead of the profile's while the bar is locked (a session started
    /// on the virtual printer keeps saying so whatever the profile names).
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        ctx: &mut AppCtx<'_>,
        connecting: bool,
        closing: bool,
        open: Option<&ConnectTarget>,
    ) {
        let p = theme::ui_palette(ui);
        let locked = ctx.state.status.is_open() || connecting;
        let now = ui.input(|i| i.time);
        let shown = open
            .filter(|_| locked)
            .cloned()
            .unwrap_or_else(|| ctx.config.active().connection.clone());
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            // The E-STOP first, at the right end; the rest gets the room
            // left of it, and the extras that do not fit stay out.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let estop = EStopButton::new().show_above_modals(ui);
                if estop.changed() {
                    if ctx.state.status.is_open() {
                        ctx.printer.emergency_stop();
                        ctx.toasts
                            .warning("Emergency stop sent")
                            .message(crate::job_text::ESTOP_MESSAGE);
                    } else {
                        ctx.toasts
                            .warning("Emergency stop: not connected")
                            .message("No printer is connected, nothing was sent.");
                    }
                }
                ui.add_space(8.0);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    self.connection_controls(ui, ctx, (connecting, closing), now, &shown, &p);
                });
            });
        });
    }

    /// Everything left of the E-STOP.
    fn connection_controls(
        &mut self,
        ui: &mut Ui,
        ctx: &mut AppCtx<'_>,
        (connecting, closing): (bool, bool),
        now: f64,
        shown: &ConnectTarget,
        p: &Palette,
    ) {
        let status = ctx.state.status.clone();
        let locked = status.is_open() || connecting;
        self.profile_combo(ui, ctx, locked);
        ui.add_space(2.0);
        self.target_combo(ui, ctx, locked, now, shown);
        self.target_details(ui, ctx, locked, shown);
        ui.add_space(4.0);
        self.connect_button(ui, ctx, &status, connecting, closing);
        ui.add_space(4.0);
        let (label, tone, pulse) = status_pill(&status, ctx.state.halted);
        let pill = ui.add(StatusPill::new(label, tone).pulse(pulse));
        let firmware = ctx
            .state
            .firmware
            .as_ref()
            .filter(|_| status.is_connected())
            .map(|fw| firmware_text(&fw.name, fw.version.as_deref()));
        // The firmware label and the link bars where they fit (the bars
        // first: they are small); a firmware left out is on the pill.
        let spacing = ui.spacing().item_spacing.x;
        let bars = status.is_connected() && ui.available_width() >= QUALITY_W + spacing;
        let firmware_fits = firmware.as_ref().is_some_and(|text| {
            let width = ui
                .painter()
                .layout_no_wrap(
                    icons::with_text(icons::FIRMWARE, text),
                    egui::FontId::proportional(FIRMWARE_FONT),
                    p.text_dim,
                )
                .size()
                .x;
            ui.available_width() >= width + spacing + if bars { QUALITY_W + spacing } else { 0.0 }
        });
        let hover = match (&status, &ctx.state.target) {
            (ConnectionStatus::Failed(reason), _) => Some(format::sanitize(reason, 300)),
            (_, Some(target)) => Some(match firmware.as_ref().filter(|_| !firmware_fits) {
                Some(fw) => format!("{} · {fw}", format::sanitize(target, 120)),
                None => format::sanitize(target, 120),
            }),
            _ => None,
        };
        if let Some(text) = hover {
            pill.on_hover_text(text);
        }
        if firmware_fits {
            self.firmware_label(ui, ctx, p);
        }
        if bars {
            self.quality_bars(ui, ctx, p);
        }
    }

    fn profile_combo(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>, locked: bool) {
        let active = ctx
            .config
            .active_profile
            .min(ctx.config.profiles.len().saturating_sub(1));
        let name = format::sanitize(&ctx.config.active().name, 28);
        let mut chosen = None;
        let mut manage = false;
        ui.add_enabled_ui(!locked, |ui| {
            egui::ComboBox::from_id_salt("sprint-profile")
                .selected_text(icons::with_text(icons::PRINTER, &name))
                .width(170.0)
                .show_ui(ui, |ui| {
                    for (i, profile) in ctx.config.profiles.iter().enumerate() {
                        let label = format::sanitize(&profile.name, 40);
                        if ui.selectable_label(i == active, label).clicked() {
                            chosen = Some(i);
                        }
                    }
                    ui.separator();
                    if ui
                        .button(icons::with_text(icons::SETTINGS, "Manage profiles…"))
                        .clicked()
                    {
                        manage = true;
                    }
                })
                .response
                .on_hover_text("Printer profile")
                .on_disabled_hover_text("Disconnect to switch profiles");
        });
        if let Some(i) = chosen
            && i != active
        {
            ctx.config.active_profile = i;
            *ctx.config_dirty = true;
        }
        if manage {
            ctx.actions.push(AppAction::OpenSettings);
        }
    }

    fn target_combo(
        &mut self,
        ui: &mut Ui,
        ctx: &mut AppCtx<'_>,
        locked: bool,
        now: f64,
        target: &ConnectTarget,
    ) {
        let target = target.clone();
        let kind = TargetKind::of(&target);
        let selected = match &target {
            ConnectTarget::Serial { port, .. } => {
                icons::with_text(icons::USB, &format::sanitize(port, 24))
            }
            ConnectTarget::Tcp { .. } => icons::with_text(icons::NETWORK, "Network (TCP)"),
            ConnectTarget::Simulator(_) => icons::with_text(icons::SIMULATOR, "Virtual printer"),
        };
        let mut new_target = None;
        ui.add_enabled_ui(!locked, |ui| {
            let combo = egui::ComboBox::from_id_salt("sprint-target")
                .selected_text(selected)
                .width(190.0)
                .height(360.0)
                .show_ui(ui, |ui| {
                    ui.set_min_width(300.0);
                    if ui
                        .selectable_label(
                            kind == TargetKind::Simulator,
                            icons::with_text(icons::SIMULATOR, "Virtual printer"),
                        )
                        .on_hover_text("Sprint's built-in simulated Marlin printer")
                        .clicked()
                        && kind != TargetKind::Simulator
                    {
                        new_target = Some(ConnectTarget::Simulator(SimConfig::default()));
                    }
                    if ui
                        .selectable_label(
                            kind == TargetKind::Network,
                            icons::with_text(icons::NETWORK, "Network (TCP)…"),
                        )
                        .on_hover_text("A serial-to-network bridge or `sprint sim --tcp`")
                        .clicked()
                        && kind != TargetKind::Network
                    {
                        new_target = Some(ConnectTarget::Tcp {
                            host: "localhost".to_owned(),
                            port: 23,
                        });
                    }
                    ui.separator();
                    let baud = match &target {
                        ConnectTarget::Serial { baud, .. } => *baud,
                        _ => DEFAULT_BAUD,
                    };
                    let current = match &target {
                        ConnectTarget::Serial { port, .. } => Some(port.as_str()),
                        _ => None,
                    };
                    let ports = self.scanner.ports();
                    if ports.is_empty() {
                        ui.label(
                            RichText::new(if self.scanner.is_scanning() {
                                "Looking for serial ports…"
                            } else {
                                "No serial ports found"
                            })
                            .weak(),
                        );
                    }
                    for port in ports {
                        if port_row(ui, port, current == Some(port.name.as_str())).clicked() {
                            new_target = Some(ConnectTarget::Serial {
                                port: port.name.clone(),
                                baud,
                            });
                        }
                    }
                    if let Some(name) = current
                        && !ports.iter().any(|p| p.name == name)
                    {
                        ui.label(
                            RichText::new(format!(
                                "{} {} (not found)",
                                icons::WARNING,
                                format::sanitize(name, 24)
                            ))
                            .color(theme::ui_palette(ui).warning),
                        );
                    }
                });
            if combo.response.clicked() {
                self.scanner.request(now);
            }
            combo
                .response
                .on_hover_text("Where the printer is connected")
                .on_disabled_hover_text("Disconnect to change the connection");
        });
        if let Some(target) = new_target {
            if let ConnectTarget::Tcp { .. } = &target {
                self.tcp_text = host_port_text(&target);
                self.tcp_valid = true;
            }
            ctx.config.active_mut().connection = target;
            *ctx.config_dirty = true;
        }
    }

    /// The baud combo (serial) or the host:port field (TCP).
    fn target_details(
        &mut self,
        ui: &mut Ui,
        ctx: &mut AppCtx<'_>,
        locked: bool,
        shown: &ConnectTarget,
    ) {
        if locked {
            // Nothing to edit: what is connected, read-only.
            match shown {
                ConnectTarget::Serial { baud, .. } => {
                    ui.add_enabled_ui(false, |ui| {
                        egui::ComboBox::from_id_salt("sprint-baud")
                            .selected_text(baud_label(*baud))
                            .width(84.0)
                            .show_ui(ui, |_| {});
                    });
                }
                ConnectTarget::Tcp { .. } => {
                    let mut text = host_port_text(shown);
                    ui.add_enabled(
                        false,
                        egui::TextEdit::singleline(&mut text).desired_width(150.0),
                    );
                }
                ConnectTarget::Simulator(_) => {}
            }
            return;
        }
        match ctx.config.active().connection.clone() {
            ConnectTarget::Serial { port, baud } => {
                let mut chosen = None;
                ui.add_enabled_ui(!locked, |ui| {
                    egui::ComboBox::from_id_salt("sprint-baud")
                        .selected_text(baud_label(baud))
                        .width(84.0)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(baud == 0, "Auto").clicked() {
                                chosen = Some(0);
                            }
                            for &rate in BAUD_RATES {
                                if ui
                                    .selectable_label(baud == rate, rate.to_string())
                                    .clicked()
                                {
                                    chosen = Some(rate);
                                }
                            }
                        })
                        .response
                        .on_hover_text("Baud rate (Auto tries the common rates in turn)");
                });
                if let Some(baud) = chosen {
                    ctx.config.active_mut().connection = ConnectTarget::Serial { port, baud };
                    *ctx.config_dirty = true;
                }
            }
            ConnectTarget::Tcp { .. } => {
                let target = ctx.config.active().connection.clone();
                let shown = (ctx.config.active_profile, target);
                if self.tcp_for.as_ref() != Some(&shown) {
                    // Another profile, or an address set in the settings: an
                    // unfinished entry for the previous one does not apply.
                    self.tcp_text = host_port_text(&shown.1);
                    self.tcp_valid = true;
                }
                let p = theme::ui_palette(ui);
                let edit = ui.add_enabled(
                    !locked,
                    egui::TextEdit::singleline(&mut self.tcp_text)
                        .hint_text("host:port")
                        .desired_width(150.0)
                        .text_color_opt((!self.tcp_valid).then_some(p.danger)),
                );
                if edit.changed() {
                    match parse_host_port(&self.tcp_text) {
                        Some(target) => {
                            self.tcp_valid = true;
                            ctx.config.active_mut().connection = target;
                            *ctx.config_dirty = true;
                        }
                        None => self.tcp_valid = false,
                    }
                }
                edit.on_hover_text(if self.tcp_valid {
                    "Host and port of the TCP bridge"
                } else {
                    "Enter host:port, for example 192.168.1.20:23"
                });
                self.tcp_for = Some((
                    ctx.config.active_profile,
                    ctx.config.active().connection.clone(),
                ));
            }
            ConnectTarget::Simulator(_) => {}
        }
    }

    fn connect_button(
        &mut self,
        ui: &mut Ui,
        ctx: &mut AppCtx<'_>,
        status: &ConnectionStatus,
        connecting: bool,
        closing: bool,
    ) {
        let opening = connecting
            || matches!(
                status,
                ConnectionStatus::Connecting | ConnectionStatus::Handshaking
            );
        if closing && status.is_open() && !opening {
            // The job is being cancelled before the link closes.
            let tip = if ctx.job_active() {
                "Cancelling the job first: the connection closes once the printer has stopped"
            } else {
                "Closing the connection"
            };
            ui.add_enabled(
                false,
                ActionButton::new("Disconnecting…").icon(icons::DISCONNECT),
            )
            .on_disabled_hover_text(tip);
            ui.spinner();
        } else if opening {
            let r = ui.add(ActionButton::new("Cancel").icon(icons::CLOSE));
            ui.spinner();
            if r.on_hover_text("Stop connecting").clicked() {
                ctx.actions.push(AppAction::Disconnect);
            }
        } else if status.is_open() {
            if ui
                .add(ActionButton::new("Disconnect").icon(icons::DISCONNECT))
                .on_hover_text("Close the connection")
                .clicked()
            {
                ctx.actions.push(AppAction::Disconnect);
            }
        } else {
            let enabled = TargetKind::of(&ctx.config.active().connection) != TargetKind::Network
                || self.tcp_valid;
            if ui
                .add_enabled(
                    enabled,
                    ActionButton::new("Connect").icon(icons::CONNECT).primary(),
                )
                .on_hover_text(format!(
                    "Connect to {}",
                    ctx.config.active().connection.label()
                ))
                .clicked()
            {
                ctx.actions.push(AppAction::Connect);
            }
        }
    }

    fn firmware_label(&self, ui: &mut Ui, ctx: &AppCtx<'_>, p: &Palette) {
        let Some(fw) = &ctx.state.firmware else {
            return;
        };
        let text = firmware_text(&fw.name, fw.version.as_deref());
        let r = ui.label(
            RichText::new(icons::with_text(icons::FIRMWARE, &text))
                .color(p.text_dim)
                .size(FIRMWARE_FONT),
        );
        r.on_hover_ui(|ui| {
            ui.label(RichText::new(format::sanitize(&fw.name, 120)).strong());
            if let Some(machine) = &fw.machine_type {
                ui.label(format!("Machine: {}", format::sanitize(machine, 80)));
            }
            if let Some(n) = fw.extruder_count {
                ui.label(format!("Extruders: {n}"));
            }
            if let Some(proto) = &fw.protocol_version {
                ui.label(format!("Protocol: {}", format::sanitize(proto, 40)));
            }
            ui.label(format!("Flavor: {}", fw.flavor.name()));
        });
    }

    fn quality_bars(&self, ui: &mut Ui, ctx: &AppCtx<'_>, p: &Palette) {
        let link = &ctx.state.link;
        let level = self.quality.level(link.rtt_avg_ms);
        let (rect, response) = ui.allocate_exact_size(vec2(QUALITY_W, 18.0), Sense::hover());
        let color = match level {
            4 | 3 => p.success,
            2 => p.warning,
            _ => p.danger,
        };
        paint_bars(ui, rect, level, color, p.border_strong);
        response.on_hover_ui(|ui| {
            ui.label(RichText::new(quality_title(level)).strong());
            egui::Grid::new("sprint-link-tooltip")
                .num_columns(2)
                .spacing([16.0, 2.0])
                .show(ui, |ui| {
                    let mut row = |k: &str, v: String| {
                        ui.label(RichText::new(k).weak());
                        ui.label(v);
                        ui.end_row();
                    };
                    row(
                        "Round trip",
                        format!("{:.1} ms avg · {:.1} max", link.rtt_avg_ms, link.rtt_max_ms),
                    );
                    row("Throughput", format!("{:.0} lines/s", link.lines_per_sec));
                    row("Lines sent", format::count(link.lines_sent));
                    row("Resends", format::count(link.resends));
                    row("Errors", format::count(link.errors));
                    row("Timeouts", format::count(link.timeouts));
                });
        });
    }
}

/// "Marlin 2.1.2.1" from an M115 name and version (the full name when
/// there is no version).
pub fn firmware_text(name: &str, version: Option<&str>) -> String {
    let first = name.split_whitespace().next().unwrap_or("");
    match version.map(str::trim).filter(|v| !v.is_empty()) {
        Some(version) if !first.is_empty() => format!(
            "{} {}",
            format::sanitize(first, 24),
            format::sanitize(version, 24)
        ),
        Some(version) => format::sanitize(version, 24),
        None if first.is_empty() => "Firmware".to_owned(),
        None => format::sanitize(name.trim(), 40),
    }
}

fn quality_title(level: u8) -> &'static str {
    match level {
        4 => "Link quality: excellent",
        3 => "Link quality: good",
        2 => "Link quality: fair",
        _ => "Link quality: poor",
    }
}

fn baud_label(baud: u32) -> String {
    if baud == 0 {
        "Auto".to_owned()
    } else {
        baud.to_string()
    }
}

/// One serial port in the target combo: name, description and a marker
/// for likely printers.
fn port_row(ui: &mut Ui, port: &PortInfo, selected: bool) -> egui::Response {
    let p = theme::ui_palette(ui);
    let mut job = egui::text::LayoutJob::default();
    let body = egui::FontId::proportional(14.0);
    let small = egui::FontId::proportional(12.0);
    let fmt = |font: &egui::FontId, color: Color32| egui::TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    job.append(
        &format!("{} ", icons::USB),
        0.0,
        fmt(
            &body,
            if port.likely_printer {
                p.accent
            } else {
                p.text_dim
            },
        ),
    );
    job.append(&format::sanitize(&port.name, 24), 0.0, fmt(&body, p.text));
    let description = format::sanitize(&port.description, 48);
    if !description.is_empty() && description != port.name {
        job.append(&format!("   {description}"), 0.0, fmt(&small, p.text_dim));
    }
    if port.likely_printer {
        job.append("   ● printer", 0.0, fmt(&small, p.success));
    }
    let r = ui.selectable_label(selected, job);
    r.on_hover_ui(|ui| {
        ui.label(RichText::new(format::sanitize(&port.name, 60)).strong());
        if !port.description.is_empty() {
            ui.label(format::sanitize(&port.description, 120));
        }
        if let Some(m) = &port.manufacturer {
            ui.label(format!("Manufacturer: {}", format::sanitize(m, 60)));
        }
        if let (Some(vid), Some(pid)) = (port.vid, port.pid) {
            ui.label(format!("USB {vid:04X}:{pid:04X}"));
        }
        if port.likely_printer {
            ui.label("Looks like a 3D printer board");
        }
    })
}

/// Four signal bars, `level` of them lit.
fn paint_bars(ui: &Ui, rect: Rect, level: u8, on: Color32, off: Color32) {
    let painter = ui.painter();
    let w = 3.5;
    let gap = 2.0;
    for i in 0..4u8 {
        let h = 5.0 + f32::from(i) * 3.5;
        let x = rect.left() + 2.0 + f32::from(i) * (w + gap);
        let bar = Rect::from_min_max(
            egui::pos2(x, rect.bottom() - 2.0 - h),
            egui::pos2(x + w, rect.bottom() - 2.0),
        );
        if i < level {
            painter.rect_filled(bar, 1.0, on);
        } else {
            painter.rect_stroke(bar, 1.0, Stroke::new(1.0, off), egui::StrokeKind::Inside);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port_parsing() {
        assert_eq!(
            parse_host_port("192.168.1.20:23"),
            Some(ConnectTarget::Tcp {
                host: "192.168.1.20".into(),
                port: 23
            })
        );
        assert_eq!(
            parse_host_port(" tcp://printer.local:8080 "),
            Some(ConnectTarget::Tcp {
                host: "printer.local".into(),
                port: 8080
            })
        );
        assert!(parse_host_port("").is_none());
        assert!(parse_host_port("nohost").is_none());
        assert!(parse_host_port("host:99999").is_none());
        let v6 = parse_host_port("[::1]:23").expect("ipv6");
        assert_eq!(host_port_text(&v6), "[::1]:23");
        assert_eq!(
            host_port_text(&ConnectTarget::Tcp {
                host: "a".into(),
                port: 1
            }),
            "a:1"
        );
        assert_eq!(host_port_text(&ConnectTarget::default()), "");
    }

    #[test]
    fn pills() {
        assert_eq!(
            status_pill(&ConnectionStatus::Connected, false).1,
            Tone::Success
        );
        assert_eq!(status_pill(&ConnectionStatus::Connected, true).0, "Halted");
        assert!(status_pill(&ConnectionStatus::Handshaking, false).2);
        assert_eq!(
            status_pill(&ConnectionStatus::Failed("x".into()), false).1,
            Tone::Danger
        );
        // A halted flag left over from a closed connection is not shown.
        assert_eq!(
            status_pill(&ConnectionStatus::Disconnected, true).0,
            "Disconnected"
        );
        assert_eq!(baud_label(0), "Auto");
        assert_eq!(
            firmware_text("Marlin 2.1.2.1 (Github)", Some("2.1.2.1")),
            "Marlin 2.1.2.1"
        );
        assert_eq!(firmware_text("Klipper", None), "Klipper");
        assert_eq!(firmware_text("", None), "Firmware");
        assert_eq!(firmware_text("  ", Some("1.0")), "1.0");
        assert_eq!(baud_label(250_000), "250000");
    }
}
