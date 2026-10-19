//! Widgets rendered through the public API with real sample toolpaths.

use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::{Block, StatefulWidget, Widget};
use sprint_gcode::{AnalyzeOptions, GcodeFile, Toolpath, analyze_simple};
use sprint_tui::{
    CommandInput, CommandLine, CompletionPopup, ConsoleLine, ConsoleState, ConsoleView,
    FeatureLegend, HeaterGauge, HeaterKind, HelpOverlay, HelpSection, InputEvent, JobProgress,
    JobTone, KeyHints, LayerCanvas, LayerFit, StatsPanel, TempChart, TempPoint, Theme, part_bounds,
};

fn sample(name: &str) -> Toolpath {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name);
    let file = GcodeFile::load(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    analyze_simple(&file, &AnalyzeOptions::default())
}

fn render(widget: impl Widget, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| frame.render_widget(widget, frame.area()))
        .expect("draw");
    terminal.backend().buffer().clone()
}

fn render_stateful<W: StatefulWidget>(
    widget: W,
    width: u16,
    height: u16,
    state: &mut W::State,
) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| frame.render_stateful_widget(widget, frame.area(), state))
        .expect("draw");
    terminal.backend().buffer().clone()
}

fn text(buf: &Buffer) -> String {
    let area = buf.area;
    (area.top()..area.bottom())
        .map(|y| {
            (area.left()..area.right())
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn braille_cells(buf: &Buffer) -> usize {
    buf.content()
        .iter()
        .filter(|c| {
            c.symbol()
                .chars()
                .any(|ch| ('\u{2801}'..='\u{28FF}').contains(&ch))
        })
        .count()
}

fn temps() -> Vec<TempPoint> {
    (0..120)
        .map(|i| {
            let t = f64::from(i);
            let f = (i as f32 / 60.0).min(1.0);
            TempPoint {
                t,
                hotends: vec![(20.0 + 190.0 * f, 210.0), (20.0, 0.0)],
                bed: Some((20.0 + 40.0 * f, 60.0)),
                chamber: Some((25.0, 0.0)),
            }
        })
        .collect()
}

fn console() -> Vec<ConsoleLine> {
    vec![
        ConsoleLine::info("Connected"),
        ConsoleLine::sent("M115"),
        ConsoleLine::received("FIRMWARE_NAME:Marlin 2.1.2"),
        ConsoleLine::received("ok T:210.0 /210.0").with_auto(true),
        ConsoleLine::warning("Resend: 12"),
        ConsoleLine::error("Error:Printer halted"),
    ]
}

#[test]
fn stats_panel_describes_the_calibration_cube() {
    let tp = sample("calibration-cube.gcode");
    let part = part_bounds(&tp);
    let size = part.size();
    assert!(
        (size[0] - 20.0).abs() < 1.0 && (size[1] - 20.0).abs() < 1.0,
        "{size:?}"
    );
    let buf = render(StatsPanel::from_toolpath(&tp), 110, 40);
    let screen = text(&buf);
    assert!(
        screen.contains("Slicer       PrusaSlicer 2.7.1"),
        "{screen}"
    );
    assert!(screen.contains("Layers       100, 0.20 mm"), "{screen}");
    // Path centrelines of a 20 mm cube: a little under 20 mm wide, as tall
    // as its top layer.
    let expected = format!("Size         {:.1} x {:.1} x 20.0 mm", size[0], size[1]);
    assert!(screen.contains(&expected), "{expected}\n{screen}");
    assert!(
        screen.contains("Thumbnails   2: 16x16 PNG, 220x124 PNG"),
        "{screen}"
    );
    assert!(screen.contains("Flags        relative E"), "{screen}");
    assert!(screen.contains("■ External perimeter"), "{screen}");
    assert!(screen.contains("■ Travel"), "{screen}");
}

#[test]
fn layer_canvas_draws_sample_layers_with_progress() {
    let tp = sample("calibration-cube.gcode");
    assert_eq!(tp.layers.len(), 100);
    for layer in [0, 30, 99] {
        // The source line of the middle move of the layer.
        let segments = tp.layers[layer].segments.clone();
        let mid = tp.segments[segments.start + (segments.end - segments.start) / 2].line;
        let canvas = LayerCanvas::new(&tp, layer, Some(mid), [250.0, 210.0])
            .fit(LayerFit::Object)
            .show_label(true)
            .block(Block::bordered());
        let buf = render(canvas, 60, 24);
        let screen = text(&buf);
        assert!(braille_cells(&buf) > 20, "layer {layer}:\n{screen}");
        assert_eq!(screen.matches('●').count(), 1, "layer {layer}:\n{screen}");
        assert!(
            screen.contains(&format!("Layer {}/100", layer + 1)),
            "{screen}"
        );
    }
    // Bed fit shows the whole 250 x 210 bed outline.
    let buf = render(LayerCanvas::new(&tp, 10, None, [250.0, 210.0]), 60, 20);
    assert!(braille_cells(&buf) > 40);
    assert!(!text(&buf).contains('●'), "no progress, no tool marker");
}

#[test]
fn other_samples_render() {
    for name in ["twisted-vase.gcode", "arcs-test.gcode"] {
        let tp = sample(name);
        assert!(!tp.layers.is_empty(), "{name}");
        for layer in [0, tp.layers.len() / 2, tp.layers.len() - 1] {
            let buf = render(
                LayerCanvas::new(&tp, layer, None, [220.0, 220.0])
                    .fit(LayerFit::Object)
                    .show_travel(true),
                50,
                20,
            );
            assert!(braille_cells(&buf) > 0, "{name} layer {layer}");
        }
        let buf = render(FeatureLegend::new(&tp, None), 80, 3);
        assert!(text(&buf).contains('■'), "{name}");
        let buf = render(StatsPanel::from_toolpath(&tp), 100, 40);
        assert!(text(&buf).contains("Print time"), "{name}");
    }
}

/// Every widget, drawn into an area of the given size.
fn draw_all(tp: &Toolpath, theme: &Theme, width: u16, height: u16) -> Vec<Buffer> {
    let temps = temps();
    let log = console();
    let sections = [HelpSection::new("Keys", [("q", "Quit"), ("?", "Help")])];
    let mut input = CommandInput::new();
    input.set_text("M10");
    input.complete();
    let input_area = Rect::new(0, height.saturating_sub(1), width, 1.min(height));
    let popup = CompletionPopup::new(&input).theme(theme);
    let popup_area = popup.area(input_area, Rect::new(0, 0, width, height));
    let mut out = vec![
        render(
            LayerCanvas::new(tp, 5, Some(tp.layers[5].lines.start + 3), [250.0, 210.0])
                .theme(theme),
            width,
            height,
        ),
        render(FeatureLegend::new(tp, Some(5)).theme(theme), width, height),
        render(
            TempChart::new(&temps)
                .window(Duration::from_secs(60))
                .theme(theme),
            width,
            height,
        ),
        render(
            HeaterGauge::new(HeaterKind::Hotend(0), 150.0, Some(210.0))
                .power(Some(0.8))
                .theme(theme),
            width,
            height,
        ),
        render(
            JobProgress::new("calibration-cube.gcode", 0.4)
                .elapsed(Some(Duration::from_secs(600)))
                .eta(Some(Duration::from_secs(900)))
                .layer(Some((40, 100)))
                .status("Printing", JobTone::Active)
                .theme(theme),
            width,
            height,
        ),
        render(
            ConsoleView::new(&log)
                .search(Some("ok"))
                .show_auto(true)
                .theme(theme),
            width,
            height,
        ),
        render(
            CommandLine::new(&input).placeholder("type").theme(theme),
            width,
            height,
        ),
        render(
            KeyHints::new([("q", "Quit"), ("?", "Help")]).theme(theme),
            width,
            height,
        ),
        render(HelpOverlay::new(&sections).theme(theme), width, height),
        render(StatsPanel::from_toolpath(tp).theme(theme), width, height),
    ];
    if let Some(area) = popup_area {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| frame.render_widget(popup, area))
            .expect("draw");
        out.push(terminal.backend().buffer().clone());
    }
    out
}

#[test]
fn every_widget_survives_odd_sizes() {
    let tp = sample("calibration-cube.gcode");
    let theme = Theme::dark();
    for (w, h) in [
        (0, 0),
        (1, 1),
        (2, 2),
        (5, 3),
        (17, 4),
        (80, 1),
        (1, 40),
        (300, 100),
    ] {
        let _ = draw_all(&tp, &theme, w, h);
    }
}

#[test]
fn monochrome_theme_uses_no_colors() {
    let tp = sample("calibration-cube.gcode");
    let theme = Theme::monochrome();
    let buffers = draw_all(&tp, &theme, 80, 24);
    assert_eq!(buffers.len(), 11, "the completion popup was drawn too");
    for (i, buf) in buffers.iter().enumerate() {
        for cell in buf.content() {
            assert_eq!(cell.fg, Color::Reset, "widget {i}: {cell:?}");
            assert_eq!(cell.bg, Color::Reset, "widget {i}: {cell:?}");
        }
    }
}

#[test]
fn ansi16_theme_uses_only_basic_colors() {
    let tp = sample("calibration-cube.gcode");
    let theme = Theme::ansi16();
    for buf in draw_all(&tp, &theme, 80, 24) {
        for cell in buf.content() {
            for color in [cell.fg, cell.bg] {
                assert!(
                    !matches!(color, Color::Rgb(..) | Color::Indexed(_)),
                    "{cell:?}"
                );
            }
        }
    }
}

#[test]
fn random_keys_never_break_the_command_line() {
    use crossterm::event::{KeyEventKind, KeyEventState};
    // Deterministic xorshift, so failures reproduce.
    let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
    let mut next = move |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        usize::try_from(seed % u64::try_from(n.max(1)).unwrap_or(1)).unwrap_or(0)
    };
    // Multibyte, wide, combining and zero-width characters.
    let chars = ['G', '1', ' ', 'm', 'é', '日', '\u{301}', '\u{200b}', '😀'];
    let codes = [
        KeyCode::Backspace,
        KeyCode::Delete,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Enter,
        KeyCode::Esc,
    ];
    let emacs = ['a', 'e', 'b', 'f', 'u', 'k', 'w', 'y', 'd', 'h', 'p', 'n'];
    let mods = [
        KeyModifiers::NONE,
        KeyModifiers::NONE,
        KeyModifiers::SHIFT,
        KeyModifiers::CONTROL,
        KeyModifiers::ALT,
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ];
    let mut input = CommandInput::new();
    let mut terminal = Terminal::new(TestBackend::new(40, 12)).expect("terminal");
    for step in 0..30_000 {
        let code = match next(4) {
            0 => KeyCode::Char(chars[next(chars.len())]),
            1 => KeyCode::Char(emacs[next(emacs.len())]),
            _ => codes[next(codes.len())],
        };
        let kind = if next(10) == 0 {
            KeyEventKind::Release
        } else {
            KeyEventKind::Press
        };
        let key = KeyEvent {
            code,
            modifiers: mods[next(mods.len())],
            kind,
            state: KeyEventState::NONE,
        };
        if next(300) == 0 {
            input.insert_str("pasted\r\nline\t日");
        } else {
            let _ = input.handle_key(key);
        }
        let text = input.text();
        assert!(text.is_char_boundary(input.cursor()), "step {step}");
        assert!(!text.chars().any(char::is_control), "step {step}: {text:?}");
        if step % 101 == 0 {
            let width = u16::try_from(next(30)).unwrap_or(0);
            let area = Rect::new(0, 11, width, 1);
            terminal
                .draw(|frame| {
                    let line = CommandLine::new(&input).prompt("sprint> ");
                    if let Some(pos) = line.cursor_position(area) {
                        assert!(pos.x < area.right(), "step {step}: {pos:?}");
                    }
                    frame.render_widget(line, area);
                    let popup = CompletionPopup::new(&input);
                    if let Some(popup_area) = popup.area(area, frame.area()) {
                        frame.render_widget(popup, popup_area);
                    }
                })
                .expect("draw");
        }
    }
}

#[test]
fn typed_commands_flow_into_the_console() {
    let mut input = CommandInput::new();
    let mut log: Vec<ConsoleLine> = Vec::new();
    let mut state = ConsoleState::new();
    for command in ["G28", "m104 s210", "M105"] {
        for c in command.chars() {
            input.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        match input.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            InputEvent::Submitted(line) => {
                log.push(ConsoleLine::sent(line));
                log.push(ConsoleLine::received("ok"));
            }
            other => panic!("{command}: {other:?}"),
        }
    }
    assert_eq!(input.history(), ["G28", "m104 s210", "M105"]);
    let buf = render_stateful(ConsoleView::new(&log), 30, 4, &mut state);
    let screen = text(&buf);
    assert!(screen.starts_with("m104 s210"), "{screen}");
    assert!(
        screen.contains("m104 s210") && screen.contains("M105"),
        "{screen}"
    );
    // Recall the first command from the history and complete nothing more.
    for _ in 0..3 {
        input.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    }
    assert_eq!(input.text(), "G28");
}
