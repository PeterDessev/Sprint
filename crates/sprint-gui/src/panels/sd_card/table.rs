//! The SD card's file table: sorting and filtering (pure) and the rows.

use std::cmp::Ordering;

use egui::{
    Align, Align2, CornerRadius, FontId, Layout, Rect, Sense, Stroke, Ui, UiBuilder, WidgetInfo,
    WidgetType, pos2, vec2,
};
use sprint_proto::SdFile;

use crate::icons;
use crate::insight::{format, ui as kit};
use crate::theme::{self, TextRole, with_alpha};
use crate::widgets::{ActionButton, Badge, IconButton};

/// Column a table is sorted by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SortKey {
    /// File name (long name when known), case-insensitively.
    #[default]
    Name,
    /// File size (unknown sizes last).
    Size,
}

/// A sort order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sort {
    /// The column.
    pub key: SortKey,
    /// Largest / last first.
    pub descending: bool,
}

impl Sort {
    /// Click on a column header: the same column flips the direction, a
    /// new one starts ascending (sizes start with the largest).
    pub fn toggle(&mut self, key: SortKey) {
        if self.key == key {
            self.descending = !self.descending;
        } else {
            self.key = key;
            self.descending = key == SortKey::Size;
        }
    }
}

/// The name shown for a file: its long name when the firmware reported
/// one, else the 8.3 name.
pub fn display_name(file: &SdFile) -> &str {
    file.long_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(&file.name)
}

/// Compare names the way people expect: case-insensitively, with runs of
/// digits compared by value ("part2" before "part10").
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut a = a.chars().peekable();
    let mut b = b.chars().peekable();
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars<'_>>| {
                    let mut digits = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        digits.push(c);
                        it.next();
                    }
                    digits
                };
                let (da, db) = (take(&mut a), take(&mut b));
                let (ta, tb) = (da.trim_start_matches('0'), db.trim_start_matches('0'));
                let ord = ta
                    .len()
                    .cmp(&tb.len())
                    .then_with(|| ta.cmp(tb))
                    .then_with(|| da.len().cmp(&db.len()));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x
                    .to_lowercase()
                    .cmp(y.to_lowercase())
                    .then_with(|| x.cmp(&y));
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// Indices of the files matching `filter` (case-insensitive, in the long
/// or the 8.3 name), in `sort` order.
pub fn sorted_indices(files: &[SdFile], sort: Sort, filter: &str) -> Vec<usize> {
    let needle = filter.trim().to_lowercase();
    let mut idx: Vec<usize> = (0..files.len())
        .filter(|&i| {
            needle.is_empty()
                || display_name(&files[i]).to_lowercase().contains(&needle)
                || files[i].name.to_lowercase().contains(&needle)
        })
        .collect();
    idx.sort_by(|&a, &b| {
        let (fa, fb) = (&files[a], &files[b]);
        let by_name = || natural_cmp(display_name(fa), display_name(fb));
        match sort.key {
            SortKey::Name => {
                let ord = by_name();
                if sort.descending { ord.reverse() } else { ord }
            }
            // Unknown sizes stay last either way; ties by name.
            SortKey::Size => match (fa.size, fb.size) {
                (Some(x), Some(y)) => {
                    let ord = x.cmp(&y);
                    let ord = if sort.descending { ord.reverse() } else { ord };
                    ord.then_with(by_name)
                }
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => by_name(),
            },
        }
    });
    idx
}

/// What a row asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowAction {
    /// Print the file.
    Print,
    /// Delete the file.
    Delete,
}

/// Width of the size column.
const SIZE_W: f32 = 84.0;
/// Width of the actions column.
const ACTIONS_W: f32 = 64.0;
/// Row height.
const ROW_H: f32 = 46.0;

/// The column headers; clicking one sorts by it.
pub fn header(ui: &mut Ui, sort: &mut Sort) {
    let p = theme::ui_palette(ui);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
    let arrow = |key: SortKey| {
        if sort.key != key {
            None
        } else if sort.descending {
            Some(icons::CARET_DOWN)
        } else {
            Some(icons::CARET_UP)
        }
    };
    let mut clicked = None;
    let name_rect = Rect::from_min_max(
        rect.min + vec2(32.0, 0.0),
        pos2(rect.max.x - SIZE_W - ACTIONS_W, rect.max.y),
    );
    // The button's own padding lines its text up with the sizes, which end
    // 8 points before the actions column.
    let size_rect = Rect::from_min_max(
        pos2(rect.max.x - SIZE_W - ACTIONS_W - 8.0, rect.min.y),
        pos2(rect.max.x - ACTIONS_W, rect.max.y),
    );
    for (key, label, r, layout) in [
        (
            SortKey::Name,
            "Name",
            name_rect,
            Layout::left_to_right(Align::Center),
        ),
        // Right-aligned like the sizes below it.
        (
            SortKey::Size,
            "Size",
            size_rect,
            Layout::right_to_left(Align::Center),
        ),
    ] {
        let mut child = ui.new_child(UiBuilder::new().max_rect(r).layout(layout));
        let mut button = ActionButton::new(label)
            .ghost()
            .small()
            .tooltip(format!("Sort by {}", label.to_lowercase()));
        if let Some(icon) = arrow(key) {
            button = button.icon(icon).selected(true);
        }
        if child.add(button).clicked() {
            clicked = Some(key);
        }
    }
    ui.painter().line_segment(
        [pos2(rect.min.x, rect.max.y), pos2(rect.max.x, rect.max.y)],
        Stroke::new(1.0, p.border),
    );
    if let Some(key) = clicked {
        sort.toggle(key);
    }
}

/// One file; returns the action asked for.
pub fn row(
    ui: &mut Ui,
    file: &SdFile,
    selected: bool,
    stripe: bool,
    actions_enabled: (bool, bool),
) -> Option<RowAction> {
    let p = theme::ui_palette(ui);
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::hover());
    let name = display_name(file);
    let size = file
        .size
        .map_or_else(|| format::MISSING.to_owned(), format::bytes);
    response.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::Label,
            true,
            format!("{name} ({}), {size}", file.name),
        )
    });
    let mut action = None;
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let hovered = response.hovered() || ui.rect_contains_pointer(rect);
        if hovered {
            painter.rect_filled(rect, CornerRadius::same(theme::RADIUS), p.raised);
        } else if stripe {
            painter.rect_filled(
                rect,
                CornerRadius::same(theme::RADIUS),
                with_alpha(p.raised, 0.45),
            );
        }
        // File icon tile.
        let tile =
            Rect::from_center_size(pos2(rect.min.x + 18.0, rect.center().y), vec2(28.0, 28.0));
        painter.rect_filled(
            tile,
            CornerRadius::same(theme::RADIUS),
            if selected { p.accent_soft } else { p.raised },
        );
        painter.text(
            tile.center(),
            Align2::CENTER_CENTER,
            icons::FILE,
            kit::icon_font(ui, 15.0),
            if selected { p.accent } else { p.text_dim },
        );
        let x = rect.min.x + 40.0;
        let size_x = rect.max.x - ACTIONS_W - 8.0;
        let name_w = (size_x - SIZE_W - x).max(30.0);
        let mut badge_w = 0.0;
        if selected {
            let badge = Badge::new("SELECTED").tone(theme::Tone::Accent);
            badge_w = badge.width(ui) + 6.0;
        }
        let title = kit::truncated(
            ui,
            name,
            FontId::new(13.5, theme::font(ui, TextRole::Title).family),
            p.text,
            (name_w - badge_w).max(20.0),
        );
        let short = kit::truncated(
            ui,
            &file.name,
            theme::builtin_font(ui, egui::TextStyle::Monospace).clone(),
            p.text_dim,
            name_w,
        );
        let title_w = title.size().x;
        painter.galley(pos2(x, rect.min.y + 6.0), title, p.text);
        painter.galley(pos2(x, rect.min.y + 26.0), short, p.text_dim);
        if selected {
            let badge_rect = Rect::from_min_size(
                pos2(x + title_w + 6.0, rect.min.y + 7.0),
                vec2(badge_w - 6.0, 18.0),
            );
            ui.new_child(
                UiBuilder::new()
                    .max_rect(badge_rect)
                    .layout(Layout::left_to_right(Align::Center)),
            )
            .add(Badge::new("SELECTED").tone(theme::Tone::Accent));
        }
        let size_g = kit::truncated(ui, &size, FontId::proportional(13.0), p.text_dim, SIZE_W);
        ui.painter().galley(
            pos2(
                size_x - size_g.size().x,
                rect.center().y - size_g.size().y / 2.0,
            ),
            size_g,
            p.text_dim,
        );
    }
    // The buttons go into a child Ui over the row's right end.
    let buttons = Rect::from_min_max(pos2(rect.max.x - ACTIONS_W, rect.min.y), rect.max);
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(buttons)
            .layout(Layout::right_to_left(Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 4.0;
    let (can_print, can_delete) = actions_enabled;
    if child
        .add_enabled(
            can_delete,
            IconButton::new(icons::DELETE)
                .ghost()
                .small()
                .tooltip(format!("Delete {name}")),
        )
        .clicked()
    {
        action = Some(RowAction::Delete);
    }
    if child
        .add_enabled(
            can_print,
            IconButton::new(icons::PLAY)
                .small()
                .tooltip(format!("Print {name}")),
        )
        .clicked()
    {
        action = Some(RowAction::Print);
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, long: Option<&str>, size: Option<u64>) -> SdFile {
        SdFile {
            name: name.to_owned(),
            long_name: long.map(str::to_owned),
            size,
        }
    }

    fn files() -> Vec<SdFile> {
        vec![
            file("PART10.GCO", Some("part10.gcode"), Some(500)),
            file("BENCHY~1.GCO", Some("Benchy.gcode"), Some(2_000)),
            file("PART2.GCO", Some("part2.gcode"), None),
            file("CUBE.GCO", None, Some(100)),
            file("ZED.GCO", Some("  "), Some(2_000)),
        ]
    }

    #[test]
    fn names_compare_naturally() {
        assert_eq!(natural_cmp("part2", "part10"), Ordering::Less);
        assert_eq!(natural_cmp("Part2", "part2"), Ordering::Less);
        assert_eq!(natural_cmp("a", "B"), Ordering::Less);
        assert_eq!(natural_cmp("x007", "x7"), Ordering::Greater);
        assert_eq!(natural_cmp("", "a"), Ordering::Less);
        assert_eq!(natural_cmp("same", "same"), Ordering::Equal);
        // Huge digit runs do not overflow.
        assert_eq!(
            natural_cmp("v99999999999999999999999", "v100000000000000000000000"),
            Ordering::Less
        );
    }

    #[test]
    fn display_names_prefer_long_names() {
        let f = files();
        assert_eq!(display_name(&f[1]), "Benchy.gcode");
        assert_eq!(display_name(&f[3]), "CUBE.GCO");
        assert_eq!(
            display_name(&f[4]),
            "ZED.GCO",
            "blank long names are ignored"
        );
    }

    #[test]
    fn sorting_and_filtering() {
        let f = files();
        let names =
            |idx: Vec<usize>| -> Vec<&str> { idx.iter().map(|&i| display_name(&f[i])).collect() };
        assert_eq!(
            names(sorted_indices(&f, Sort::default(), "")),
            [
                "Benchy.gcode",
                "CUBE.GCO",
                "part2.gcode",
                "part10.gcode",
                "ZED.GCO"
            ]
        );
        let mut sort = Sort::default();
        sort.toggle(SortKey::Name);
        assert!(sort.descending);
        assert_eq!(names(sorted_indices(&f, sort, ""))[0], "ZED.GCO");
        sort.toggle(SortKey::Size);
        assert_eq!(
            sort,
            Sort {
                key: SortKey::Size,
                descending: true
            }
        );
        assert_eq!(
            names(sorted_indices(&f, sort, "")),
            [
                "Benchy.gcode",
                "ZED.GCO",
                "part10.gcode",
                "CUBE.GCO",
                "part2.gcode"
            ]
        );
        sort.toggle(SortKey::Size);
        assert_eq!(
            names(sorted_indices(&f, sort, "")),
            [
                "CUBE.GCO",
                "part10.gcode",
                "Benchy.gcode",
                "ZED.GCO",
                "part2.gcode"
            ]
        );
        // The filter matches long and short names.
        assert_eq!(
            names(sorted_indices(&f, Sort::default(), "PART")),
            ["part2.gcode", "part10.gcode"]
        );
        assert_eq!(
            names(sorted_indices(&f, Sort::default(), "benchy~")),
            ["Benchy.gcode"]
        );
        assert!(sorted_indices(&f, Sort::default(), "nothing").is_empty());
        assert!(sorted_indices(&[], Sort::default(), "").is_empty());
    }
}
