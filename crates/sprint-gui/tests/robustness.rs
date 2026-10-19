//! Malformed and extreme inputs (NaN, infinities, huge values, empty and
//! ragged data, absurd sizes) must render without panicking — including
//! egui's debug assertions — in both themes, with hover and clicks.

mod common;

use common::{click_at, harness};
use sprint_gui::{
    icons,
    theme::{HeaterKind, Theme, Tone},
    widgets::*,
};

const NASTY: [f32; 8] = [
    f32::NAN,
    f32::INFINITY,
    f32::NEG_INFINITY,
    f32::MAX,
    -f32::MAX,
    f32::MIN_POSITIVE,
    -0.0,
    1e-30,
];

fn nasty_series() -> Vec<f32> {
    let mut v: Vec<f32> = NASTY.to_vec();
    v.extend([1.0, 2.0, f32::NAN, 3.0, f32::MAX, -f32::MAX, 0.0]);
    v
}

fn everything(ui: &mut egui::Ui) {
    let series = nasty_series();
    let points: Vec<[f64; 2]> = series
        .iter()
        .enumerate()
        .map(|(i, v)| [i as f64, f64::from(*v)])
        .chain([[f64::NAN, 1.0], [1e300, -1e300], [-1e300, f64::INFINITY]])
        .collect();
    let grid = vec![
        NASTY.to_vec(),
        vec![],
        vec![f32::MAX, -f32::MAX, 0.0],
        vec![f32::NAN; 12],
    ];
    ui.horizontal_wrapped(|ui| {
        for v in NASTY {
            ui.add(ProgressRing::new(v).size(v).thickness(v).text(""));
            ui.add(
                LinearProgress::new(v)
                    .width(v)
                    .height(v)
                    .show_percentage(true),
            );
            ui.add(EStopButton::new().style(EStopStyle::Octagon(v)));
            status_dot(ui, Tone::Warning, v);
            ui.add(
                Sparkline::new(&series)
                    .width(v)
                    .height(v)
                    .range(v, -v)
                    .min_span(v),
            );
        }
    });
    ui.add(Sparkline::new(&series).reference(Some(f32::NAN)));
    ui.add(Sparkline::new(&series).smooth(false).fill(false));
    MiniChart::new()
        .series(ChartSeries::new("nasty", &points, egui::Color32::RED).fill(true))
        .series(ChartSeries::new("dash", &points, egui::Color32::BLUE).dashed(true))
        .y_range(f64::NAN, f64::INFINITY)
        .height(f32::NAN)
        .show(ui);
    MiniChart::new()
        .series(ChartSeries::new(
            "huge",
            &[[0.0, 1e300], [1.0, -1e300]],
            egui::Color32::RED,
        ))
        .show(ui);
    let _ = Heatmap::new(&grid)
        .scale(f32::NAN)
        .max_cell(f32::INFINITY)
        .show(ui);
    let _ = Heatmap::new(&grid)
        .positions(Some(&NASTY), Some(&[f32::NAN; 4]))
        .decimals(usize::MAX)
        .id_salt("positions")
        .show(ui);
    for (i, v) in NASTY.iter().enumerate() {
        let _ = TemperatureCard::new("Hotend", HeaterKind::Hotend)
            .id_salt(i)
            .current(*v)
            .target(*v)
            .power(Some(*v))
            .history(&series)
            .presets(&[("x", *v), ("y", f32::NAN)])
            .max_target(*v)
            .tolerance(*v)
            .show(ui);
    }
    for v in NASTY {
        let mut x = f64::from(v);
        ui.add(
            NumericField::new(&mut x)
                .label("nasty")
                .range(f64::from(v)..=f64::from(-v))
                .original(f64::from(-v))
                .decimals(usize::MAX)
                .speed(f64::NAN)
                .label_width(v)
                .field_width(v),
        );
    }
    let _ = JogPad::new()
        .size(f32::NAN)
        .steps([f32::NAN, f32::INFINITY, -1.0, 0.0])
        .show(ui);
    let _ = ZJog::new()
        .height(f32::INFINITY)
        .width(f32::NAN)
        .steps([f32::MAX; 4])
        .show(ui);
    let (mut length, mut speed) = (f32::INFINITY, f32::NAN);
    let _ = ExtruderControl::new(&mut length, &mut speed)
        .presets(&NASTY)
        .limits(f32::NAN, -1.0)
        .show(ui);
    KeyValueGrid::new("kv")
        .key_width(f32::NAN)
        .row_spacing(f32::INFINITY)
        .show(ui, |kv| {
            kv.row("", "");
            kv.row_mono("\u{0}\u{FFFF}", "\n\n\t");
            kv.row("long", &"x".repeat(5000));
        });
    let none: [(u8, &str); 0] = [];
    ui.add(SegmentedControl::new(&mut 0_u8, &none).segment_width(f32::NAN));
    ui.add(Toggle::new(&mut true).label(""));
    let _ = Card::new("").padding(i8::MIN).show(ui, |_| {});
    EmptyState::new("", "").message("").action("").show(ui);
    ui.add(ActionButton::new("").min_width(f32::NAN).fill_width(true));
    ui.add(IconButton::new("").tint(egui::Color32::TRANSPARENT));
    ui.add(Badge::new(""));
    ui.add(Chip::new("").icon(icons::FAN));
    ui.add(StatusPill::new("", Tone::Danger).pulse(true));
}

#[test]
fn nasty_inputs_render_hover_and_click_without_panicking() {
    for theme in Theme::ALL {
        let mut h = harness(theme, (900.0, 3000.0), everything);
        // Sweep the pointer and click across the whole page.
        for y in (0..3000).step_by(97) {
            for x in (0..900).step_by(131) {
                click_at(&mut h, egui::pos2(x as f32, y as f32));
            }
        }
        h.run_ok();
    }
}

#[test]
fn toasts_and_dialogs_with_odd_text() {
    let mut toasts = Toasts::new().width(f32::NAN).max_visible(usize::MAX);
    toasts
        .error("")
        .message("x".repeat(10_000))
        .duration(f32::NAN);
    toasts.info("\u{0}");
    let mut h = common::harness_state(Theme::Dark, (400.0, 300.0), toasts, |ui, toasts| {
        toasts.show(ui.ctx());
        let _ = ConfirmDialog::new("odd", "")
            .message(&"y".repeat(5000))
            .details("")
            .width(f32::NAN)
            .show(ui.ctx());
    });
    click_at(&mut h, egui::pos2(200.0, 150.0));
    h.run_ok();
}
