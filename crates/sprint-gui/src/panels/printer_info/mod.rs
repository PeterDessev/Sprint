//! Printer tab: firmware, capabilities, statistics, endstops, position,
//! link quality, thermal health, a settings summary and recent firmware
//! messages, as a responsive grid of cards (one to three columns).

mod cards;
mod thermal;

use std::collections::VecDeque;

use crate::context::AppCtx;
use crate::icons;
use crate::insight::{format, ui as kit};
use crate::theme::{self, Tone};
use crate::widgets::{EmptyState, IconButton, StatusPill};

/// Round-trip samples kept for the link sparkline (one per second).
pub const RTT_HISTORY_LEN: usize = 120;
/// Narrowest card column (points).
const MIN_COLUMN: f32 = 340.0;
/// How often live endstop polling sends M119 (seconds).
const ENDSTOP_POLL_SECS: f64 = 1.0;

/// Printer tab: firmware, capabilities, statistics, endstops, link quality and thermal health.
#[derive(Default)]
pub struct PrinterInfoTab {
    /// Round trips (ms), one per second of link time, oldest first.
    rtt_history: VecDeque<f32>,
    /// `connected_secs` of the last RTT sample.
    last_rtt_sample: Option<f64>,
    /// Poll M119 once a second while the tab is visible.
    endstops_live: bool,
    /// UI time of the last live M119.
    last_endstop_poll: Option<f64>,
    /// Show capabilities the firmware lacks too.
    show_disabled_caps: bool,
}

/// The cards of the grid, in placement order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CardKind {
    Firmware,
    Thermal,
    Link,
    Statistics,
    Capabilities,
    Position,
    Endstops,
    Settings,
    Messages,
}

const CARDS: [CardKind; 9] = [
    CardKind::Firmware,
    CardKind::Thermal,
    CardKind::Link,
    CardKind::Capabilities,
    CardKind::Statistics,
    CardKind::Position,
    CardKind::Endstops,
    CardKind::Settings,
    CardKind::Messages,
];

impl PrinterInfoTab {
    /// Draw the panel.
    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &mut AppCtx<'_>) {
        self.sample_link(ctx);
        if !ctx.connected() {
            ui.add_space(48.0);
            let message = match &ctx.state.status {
                sprint_host::ConnectionStatus::Failed(reason) => {
                    format!("The last connection failed: {reason}")
                }
                sprint_host::ConnectionStatus::Connecting
                | sprint_host::ConnectionStatus::Handshaking => {
                    "Waiting for the firmware to answer…".to_owned()
                }
                _ => "Connect to a printer to see its firmware, capabilities, statistics, link quality and thermal health.".to_owned(),
            };
            let _ = EmptyState::new(icons::PRINTER, "Not connected")
                .message(&message)
                .show(ui);
            return;
        }
        self.poll_endstops(ui, ctx);
        egui::ScrollArea::vertical()
            .id_salt("printer-info-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(2.0);
                self.header(ui, ctx);
                ui.add_space(theme::GAP_LG);
                kit::masonry(ui, MIN_COLUMN, 3, CARDS.len(), |ui, i| {
                    self.card(ui, ctx, CARDS[i]);
                });
                ui.add_space(theme::GAP_LG);
            });
    }

    /// The round-trip history kept for the sparkline (ms, oldest first).
    pub fn rtt_history(&self) -> impl ExactSizeIterator<Item = f32> + '_ {
        self.rtt_history.iter().copied()
    }

    fn card(&mut self, ui: &mut egui::Ui, ctx: &mut AppCtx<'_>, kind: CardKind) {
        match kind {
            CardKind::Firmware => cards::firmware(ui, ctx),
            CardKind::Thermal => thermal::thermal_health(ui, ctx),
            CardKind::Link => cards::link(ui, ctx, &self.rtt_history),
            CardKind::Statistics => cards::statistics(ui, ctx),
            CardKind::Capabilities => cards::capabilities(ui, ctx, &mut self.show_disabled_caps),
            CardKind::Position => cards::position(ui, ctx),
            CardKind::Endstops => cards::endstops(ui, ctx, &mut self.endstops_live),
            CardKind::Settings => cards::settings_summary(ui, ctx),
            CardKind::Messages => cards::messages(ui, ctx),
        }
    }

    /// Keep one round-trip sample per second of link time.
    fn sample_link(&mut self, ctx: &AppCtx<'_>) {
        let link = &ctx.state.link;
        let now = link.connected_secs;
        if !ctx.state.status.is_open() || !now.is_finite() {
            return;
        }
        match self.last_rtt_sample {
            // A new connection: its clock restarted.
            Some(last) if now + 0.5 < last => {
                self.rtt_history.clear();
                self.last_rtt_sample = None;
            }
            Some(last) if now - last < 1.0 => return,
            _ => {}
        }
        if link.oks == 0 || !link.rtt_last_ms.is_finite() || link.rtt_last_ms < 0.0 {
            return;
        }
        if self.rtt_history.len() >= RTT_HISTORY_LEN {
            self.rtt_history.pop_front();
        }
        self.rtt_history.push_back(link.rtt_last_ms);
        self.last_rtt_sample = Some(now);
    }

    /// Live endstop polling: M119 once a second while idle.
    fn poll_endstops(&mut self, ui: &egui::Ui, ctx: &AppCtx<'_>) {
        if !self.endstops_live {
            return;
        }
        if ctx.job_active() {
            return;
        }
        let now = ui.input(|i| i.time);
        if self
            .last_endstop_poll
            .is_none_or(|last| now - last >= ENDSTOP_POLL_SECS || now < last)
        {
            ctx.printer.request_endstops();
            self.last_endstop_poll = Some(now);
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f64(ENDSTOP_POLL_SECS));
    }

    fn header(&mut self, ui: &mut egui::Ui, ctx: &mut AppCtx<'_>) {
        let state = ctx.state;
        let fw = state.firmware.as_ref();
        let title = fw
            .and_then(|f| f.machine_type.as_deref())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Printer");
        let mut sub = Vec::new();
        if let Some(f) = fw {
            sub.push(f.name.clone());
        }
        if let Some(n) = fw.and_then(|f| f.extruder_count) {
            sub.push(format!("{n} extruder{}", if n == 1 { "" } else { "s" }));
        }
        if let Some(target) = &state.target {
            sub.push(target.clone());
        }
        let subtitle = sub.join("  ·  ");
        let printer = ctx.printer;
        kit::page_header(
            ui,
            icons::PRINTER,
            title,
            (!subtitle.is_empty()).then_some(subtitle.as_str()),
            |ui| {
                if ui
                    .add(
                        IconButton::new(icons::REFRESH)
                            .tooltip("Refresh everything (M115, M78, M119, M114, M503)"),
                    )
                    .clicked()
                {
                    printer.request_firmware_info();
                    printer.request_stats();
                    printer.request_endstops();
                    printer.request_position();
                    if state
                        .firmware
                        .as_ref()
                        .is_none_or(|f| f.flavor.supports_m503())
                    {
                        printer.request_settings();
                    }
                }
                let uptime = format!("Up {}", format::duration(state.link.connected_secs));
                ui.add(StatusPill::new(&uptime, Tone::Neutral).icon(icons::CLOCK));
                let (label, tone) = if state.halted {
                    ("Halted", Tone::Danger)
                } else if ctx.job_active() {
                    ("Printing", Tone::Accent)
                } else if state.busy.is_some() {
                    ("Busy", Tone::Warning)
                } else {
                    ("Connected", Tone::Success)
                };
                ui.add(StatusPill::new(label, tone));
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_card_is_placed_once() {
        for kind in [
            CardKind::Firmware,
            CardKind::Thermal,
            CardKind::Link,
            CardKind::Statistics,
            CardKind::Capabilities,
            CardKind::Position,
            CardKind::Endstops,
            CardKind::Settings,
            CardKind::Messages,
        ] {
            assert_eq!(CARDS.iter().filter(|k| **k == kind).count(), 1, "{kind:?}");
        }
    }
}
