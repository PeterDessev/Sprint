//! Slicer metadata and embedded thumbnails.
//!
//! Slicers write their information into comments: a header near the start
//! (generator, estimates, thumbnails, Cura's `;TIME:` block, Simplify3D's
//! settings summary, Orca/Bambu's `HEADER_BLOCK`) and often a summary plus
//! a `; key = value` configuration dump at the very end (PrusaSlicer,
//! SuperSlicer, OrcaSlicer). [`read_metadata`] reads the head of the file
//! (through the first layer, so per-object labels are seen) and the trailing
//! comment block instead of the whole file.

use base64::Engine as _;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

use crate::file::GcodeFile;
use crate::toolpath::{SlicerMetadata, Thumbnail, ThumbnailFormat};

/// Stop scanning the head after this many command lines when the file has
/// no layer markers.
const HEAD_COMMANDS_WITHOUT_MARKERS: usize = 5000;
/// Hard cap on the number of head lines read.
const HEAD_LINE_CAP: usize = 2_000_000;
/// Walking backwards, stop after this many command lines.
const TAIL_COMMANDS: usize = 200;
/// Hard cap on the number of tail lines read.
const TAIL_LINE_CAP: usize = 100_000;
/// Largest base64 payload accepted for one thumbnail.
const THUMBNAIL_MAX_BASE64: usize = 32 * 1024 * 1024;

const BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// Scan comments for slicer metadata and thumbnails (PrusaSlicer,
/// SuperSlicer, OrcaSlicer, Bambu Studio, Cura, Simplify3D, ideaMaker,
/// KISSlicer basics).
pub fn read_metadata(file: &GcodeFile) -> SlicerMetadata {
    let mut reader = Reader::default();
    let n = file.len();

    // Head: until the second layer marker (or enough commands when the file
    // has none), never stopping inside a thumbnail or config block.
    let mut i = 0;
    let mut markers = 0usize;
    let mut commands = 0usize;
    let mut commands_at_marker = 0usize;
    while i < n && i < HEAD_LINE_CAP {
        let line = file.line(i);
        let kind = reader.line(line, Region::Head);
        i += 1;
        match kind {
            LineKind::Command => commands += 1,
            LineKind::LayerMarker => {
                // OrcaSlicer writes two markers per layer (";LAYER_CHANGE"
                // and "; CHANGE_LAYER"): markers without a command between
                // them are one layer change.
                if markers == 0 || commands > commands_at_marker {
                    markers += 1;
                }
                commands_at_marker = commands;
                if markers >= 2 && !reader.in_block() {
                    break;
                }
            }
            LineKind::Other => {}
        }
        if markers == 0 && commands >= HEAD_COMMANDS_WITHOUT_MARKERS && !reader.in_block() {
            break;
        }
    }
    reader.end_block();
    let head_end = i;
    if head_end >= n {
        // The head covered the whole (short) file, so its trailing comment
        // block is the tail.
        reader.tail_settings = std::mem::take(&mut reader.head_trailing);
    }

    // Tail: the trailing run of comments plus a few commands before it.
    let mut start = n;
    let mut tail_commands = 0usize;
    while start > head_end && n - start < TAIL_LINE_CAP {
        let line = file.line(start - 1);
        if is_command_line(line) {
            tail_commands += 1;
            if tail_commands > TAIL_COMMANDS {
                break;
            }
        }
        start -= 1;
    }
    for j in start..n {
        reader.line(file.line(j), Region::Tail);
    }
    reader.end_block();
    reader.finish()
}

/// True when a comment line marks a layer change in any supported slicer
/// (`;LAYER_CHANGE`, `;LAYER:n`, `; CHANGE_LAYER`, `; layer 3, Z = 0.6`,
/// `; BEGIN_LAYER_OBJECT`). `comment` is the trimmed text after `;`.
pub fn is_layer_marker(comment: &str) -> bool {
    if comment == "LAYER_CHANGE" || comment == "CHANGE_LAYER" {
        return true;
    }
    if let Some(rest) = comment.strip_prefix("LAYER:") {
        let rest = rest.trim_start();
        return rest.starts_with(|c: char| c.is_ascii_digit() || c == '-');
    }
    if let Some(rest) = comment.strip_prefix("layer ") {
        return rest.starts_with(|c: char| c.is_ascii_digit());
    }
    comment.starts_with("BEGIN_LAYER_OBJECT")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Region {
    Head,
    Tail,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Command,
    LayerMarker,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Block {
    #[default]
    None,
    /// `; prusaslicer_config = begin` / `; CONFIG_BLOCK_START`.
    Config,
    /// Simplify3D `; Settings Summary`.
    S3dSettings,
}

struct ThumbState {
    width: u32,
    height: u32,
    format: ThumbnailFormat,
    base64: String,
}

#[derive(Default)]
struct Reader {
    meta: SlicerMetadata,
    block: Block,
    thumb: Option<ThumbState>,
    /// First hotend / bed temperature commanded in the head.
    cmd_hotend: Option<f32>,
    cmd_bed: Option<f32>,
    /// Sum of PrusaSlicer per-extruder `filament used [g]`.
    grams_per_extruder: Option<f32>,
    total_grams: Option<f32>,
    /// Per-material ideaMaker `Material#n Used`.
    idea_material: Option<f32>,
    griffin_name: Option<String>,
    griffin_version: Option<String>,
    s3d: bool,
    /// Entries inside explicit config blocks (preferred).
    block_settings: Vec<(String, String)>,
    /// `key = value` entries in the trailing comment block (older
    /// PrusaSlicer/Slic3r without block markers).
    tail_settings: Vec<(String, String)>,
    /// `key = value` entries in the head after its last command: the
    /// trailing block when the head scan reaches the end of a short file.
    head_trailing: Vec<(String, String)>,
}

impl Reader {
    fn in_block(&self) -> bool {
        self.thumb.is_some() || self.block != Block::None
    }

    fn end_block(&mut self) {
        self.thumb = None;
        self.block = Block::None;
    }

    fn line(&mut self, line: &str, region: Region) -> LineKind {
        let t = line.trim();
        if t.is_empty() {
            return LineKind::Other;
        }
        let Some(comment) = t.strip_prefix(';') else {
            // Thumbnail and config blocks consist of comments only.
            self.thumb = None;
            if self.block == Block::Config {
                self.block = Block::None;
            }
            if self.block == Block::S3dSettings {
                self.block = Block::None;
            }
            if t.starts_with('(') {
                return LineKind::Other;
            }
            self.head_trailing.clear();
            self.command(t, region);
            return LineKind::Command;
        };
        let comment = comment.trim();
        if self.thumb.is_some() {
            self.thumbnail_line(comment);
            return LineKind::Other;
        }
        if is_layer_marker(comment) {
            return LineKind::LayerMarker;
        }
        self.comment(comment, region);
        LineKind::Other
    }

    fn thumbnail_line(&mut self, comment: &str) {
        let lower_end = comment.starts_with("thumbnail") && comment.ends_with(" end");
        if lower_end {
            if let Some(state) = self.thumb.take() {
                self.finish_thumbnail(state);
            }
            return;
        }
        let Some(state) = self.thumb.as_mut() else {
            return;
        };
        if state.base64.len() + comment.len() > THUMBNAIL_MAX_BASE64 {
            self.thumb = None;
            return;
        }
        state.base64.extend(
            comment
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')),
        );
    }

    fn finish_thumbnail(&mut self, state: ThumbState) {
        let Ok(data) = BASE64.decode(state.base64.as_bytes()) else {
            return;
        };
        if data.is_empty() {
            return;
        }
        let format = sniff_format(&data).unwrap_or(state.format);
        let (mut width, mut height) = (state.width, state.height);
        if (width == 0 || height == 0)
            && format == ThumbnailFormat::Png
            && let Some((w, h)) = png_size(&data)
        {
            width = w;
            height = h;
        }
        self.meta.thumbnails.push(Thumbnail {
            width,
            height,
            format,
            data,
        });
    }

    fn command(&mut self, t: &str, region: Region) {
        if region == Region::Head {
            if is_code(t, "M104") || is_code(t, "M109") {
                if self.cmd_hotend.is_none() {
                    self.cmd_hotend = param_value(t, 'S').filter(|v| *v > 0.0);
                }
            } else if (is_code(t, "M140") || is_code(t, "M190")) && self.cmd_bed.is_none() {
                self.cmd_bed = param_value(t, 'S').filter(|v| *v > 0.0);
            }
        }
        if is_code(t, "M486") {
            if let Some(name) = m486_name(t) {
                self.add_object(name, region);
            }
        } else if is_code(t, "EXCLUDE_OBJECT_DEFINE")
            && let Some(name) = klipper_name(t)
        {
            self.add_object(name, region);
        }
    }

    /// Object labels are only collected from the head: the tail repeats the
    /// labels of whatever objects reach the top layers.
    fn add_object(&mut self, name: &str, region: Region) {
        if region != Region::Head {
            return;
        }
        let name = name.trim();
        if name.is_empty() || self.meta.object_names.iter().any(|n| n == name) {
            return;
        }
        self.meta.object_names.push(name.to_string());
    }

    fn comment(&mut self, c: &str, region: Region) {
        // Cura's serialized profile is split at arbitrary points; its pieces
        // can look like `key = value` lines.
        if c.starts_with("SETTING_") {
            return;
        }
        // Thumbnail start.
        if let Some(state) = thumbnail_begin(c) {
            self.thumb = Some(state);
            return;
        }
        // Block markers.
        match c {
            "CONFIG_BLOCK_START" => {
                self.block = Block::Config;
                return;
            }
            "CONFIG_BLOCK_END" => {
                self.block = Block::None;
                return;
            }
            "Settings Summary" if self.s3d => {
                self.block = Block::S3dSettings;
                return;
            }
            _ => {}
        }
        if let Some((key, value)) = split_key_value(c, " = ") {
            if key.ends_with("_config") && (value == "begin" || value == "end") {
                self.block = if value == "begin" {
                    Block::Config
                } else {
                    Block::None
                };
                return;
            }
            self.key_value(key, value);
            let entry = (key.to_string(), value.to_string());
            if self.block == Block::Config {
                self.block_settings.push(entry);
            } else if region == Region::Tail {
                self.tail_settings.push(entry);
            } else {
                self.head_trailing.push(entry);
            }
            return;
        }
        if self.block == Block::S3dSettings
            && let Some((key, value)) = c.split_once(',')
            && !key.is_empty()
            && key.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            self.s3d_setting(key, value, region);
            self.block_settings
                .push((key.to_string(), value.to_string()));
            return;
        }
        self.header_line(c, region);
    }

    /// `; key = value` (PrusaSlicer family, KISSlicer).
    fn key_value(&mut self, key: &str, value: &str) {
        let m = &mut self.meta;
        match key {
            "estimated printing time (normal mode)" => {
                set(&mut m.estimated_time, parse_duration(value))
            }
            "filament used [mm]" => set(&mut m.filament_used_mm, sum_list(value)),
            "filament used [g]" => set(&mut self.grams_per_extruder, sum_list(value)),
            "total filament used [g]" => set(&mut self.total_grams, first_number(value)),
            "layer_height" | "layer_thickness_mm" => set(&mut m.layer_height, first_number(value)),
            "first_layer_height" | "initial_layer_print_height" => {
                if let Some(pct) = value.trim().strip_suffix('%') {
                    // Old PrusaSlicer: percentage of the layer height,
                    // resolved in `finish`.
                    if let Ok(p) = pct.trim().parse::<f32>() {
                        m.first_layer_height = Some(-p);
                    }
                } else {
                    set(&mut m.first_layer_height, first_number(value));
                }
            }
            "nozzle_diameter" => set(&mut m.nozzle_diameter, first_number(value)),
            "filament_diameter" | "fiber_dia_mm" => {
                set(&mut m.filament_diameter, first_number(value))
            }
            "filament_density" => set(&mut m.filament_density, first_number(value)),
            "filament_type" => {
                if m.filament_type.is_none() {
                    let first = value
                        .split([';', ','])
                        .next()
                        .unwrap_or("")
                        .trim()
                        .trim_matches('"');
                    if !first.is_empty() {
                        m.filament_type = Some(first.to_string());
                    }
                }
            }
            "temperature" | "nozzle_temperature" => set(&mut m.hotend_temp, first_number(value)),
            "bed_temperature" | "hot_plate_temp" => set(&mut m.bed_temp, first_number(value)),
            "gcode_flavor" => {
                if m.flavor.is_none() && !value.is_empty() {
                    m.flavor = Some(value.trim().to_string());
                }
            }
            "total layers count" => set_u32(&mut m.layer_count, first_number(value)),
            _ => {}
        }
    }

    fn s3d_setting(&mut self, key: &str, value: &str, region: Region) {
        let m = &mut self.meta;
        match key {
            "layerHeight" => set(&mut m.layer_height, first_number(value)),
            "firstLayerHeightPercentage" => {
                if m.first_layer_height.is_none()
                    && let Some(p) = first_number(value)
                {
                    m.first_layer_height = Some(-p);
                }
            }
            "extruderDiameter" => set(&mut m.nozzle_diameter, first_number(value)),
            "filamentDiameters" => set(&mut m.filament_diameter, first_number(value)),
            "filamentDensities" => set(&mut m.filament_density, first_number(value)),
            "printMaterial" => {
                if m.filament_type.is_none() && !value.trim().is_empty() {
                    m.filament_type = Some(value.trim().to_string());
                }
            }
            "applyToModels" => {
                for name in value.split(',') {
                    self.add_object(name, region);
                }
            }
            _ => {}
        }
    }

    /// Header lines that are not `key = value`.
    fn header_line(&mut self, c: &str, region: Region) {
        // Generators.
        if let Some(rest) = c.strip_prefix("generated by ") {
            let name = rest.split(" on ").next().unwrap_or(rest);
            set_string(&mut self.meta.slicer, clean_generator(name));
            return;
        }
        if let Some(rest) = c.strip_prefix("Generated with ") {
            set_string(&mut self.meta.slicer, rest.trim().to_string());
            return;
        }
        if let Some(rest) = c.strip_prefix("G-Code generated by Simplify3D(R) Version ") {
            self.s3d = true;
            set_string(&mut self.meta.slicer, format!("Simplify3D {}", rest.trim()));
            return;
        }
        if let Some(rest) = c.strip_prefix("Sliced by ideaMaker") {
            let version = rest.split(',').next().unwrap_or("").trim();
            let name = if version.is_empty() {
                "ideaMaker".to_string()
            } else {
                format!("ideaMaker {version}")
            };
            set_string(&mut self.meta.slicer, name);
            return;
        }
        if c.starts_with("KISSlicer") {
            set_string(&mut self.meta.slicer, "KISSlicer".to_string());
            return;
        }
        if let Some(rest) = c.strip_prefix("BambuStudio ") {
            set_string(
                &mut self.meta.slicer,
                format!("BambuStudio {}", rest.trim()),
            );
            return;
        }
        // Per-object labels.
        if let Some(name) = c.strip_prefix("printing object ") {
            self.add_object(name, region);
            return;
        }
        if let Some(name) = c.strip_prefix("MESH:") {
            if name.trim() != "NONMESH" {
                self.add_object(name, region);
            }
            return;
        }
        // Orca/Bambu header: "model printing time: 1h; total estimated time: 1h 5m".
        if c.contains("total estimated time:") {
            for part in c.split(';') {
                if let Some(v) = part.trim().strip_prefix("total estimated time:") {
                    set(&mut self.meta.estimated_time, parse_duration(v));
                }
            }
            return;
        }
        let Some((key, value)) = c.split_once(':') else {
            self.simplify3d_summary(c);
            return;
        };
        let (key, value) = (key.trim(), value.trim());
        let m = &mut self.meta;
        match key {
            // Cura.
            "FLAVOR" => {
                if m.flavor.is_none() && !value.is_empty() {
                    m.flavor = Some(value.to_string());
                }
            }
            "TIME" | "PRINT.TIME" => set(&mut m.estimated_time, parse_duration(value)),
            "Filament used" => {
                // "1.234m" or "1.2m, 0.5m" (metres).
                let total: f32 = value
                    .split(',')
                    .filter_map(|v| first_number(v.trim().trim_end_matches('m')))
                    .sum();
                set(&mut m.filament_used_mm, Some(total * 1000.0));
            }
            "Layer height" | "Layer Height" => set(&mut m.layer_height, first_number(value)),
            "LAYER_COUNT" | "total layer number" => {
                set_u32(&mut m.layer_count, first_number(value))
            }
            "GENERATOR.NAME" => self.griffin_name = Some(value.to_string()),
            "GENERATOR.VERSION" => self.griffin_version = Some(value.to_string()),
            "EXTRUDER_TRAIN.0.NOZZLE.DIAMETER" => set(&mut m.nozzle_diameter, first_number(value)),
            // Orca/Bambu header block.
            "total filament length [mm]" => set(&mut m.filament_used_mm, first_number(value)),
            "total filament weight [g]" => set(&mut self.total_grams, first_number(value)),
            "filament_density" => set(&mut m.filament_density, first_number(value)),
            "filament_diameter" => set(&mut m.filament_diameter, first_number(value)),
            // ideaMaker.
            "Print Time" => set(&mut m.estimated_time, parse_duration(value)),
            "Estimated Build Time" => set(&mut m.estimated_time, parse_duration(value)),
            _ => {
                if let Some(rest) = key.strip_prefix("Material#")
                    && rest.trim().ends_with("Used")
                {
                    if let Some(v) = first_number(value) {
                        *self.idea_material.get_or_insert(0.0) += v;
                    }
                } else if key.starts_with("Filament Diameter #") {
                    set(&mut m.filament_diameter, first_number(value));
                } else if key.starts_with("Filament Density #") {
                    set(&mut m.filament_density, first_number(value));
                } else {
                    self.simplify3d_summary(c);
                }
            }
        }
    }

    /// Simplify3D build summary: "Build time: 1 hours 2 minutes",
    /// "Filament length: 1234.5 mm (1.23 m)", "Plastic weight: 3.68 g".
    fn simplify3d_summary(&mut self, c: &str) {
        let Some((key, value)) = c.split_once(':') else {
            return;
        };
        let m = &mut self.meta;
        match key.trim() {
            "Build time" => set(&mut m.estimated_time, parse_duration(value)),
            "Filament length" => set(&mut m.filament_used_mm, first_number(value)),
            "Plastic weight" => set(&mut self.total_grams, first_number(value)),
            _ => {}
        }
    }

    fn finish(mut self) -> SlicerMetadata {
        self.meta.settings = if self.block_settings.is_empty() {
            std::mem::take(&mut self.tail_settings)
        } else {
            std::mem::take(&mut self.block_settings)
        };
        if self.meta.slicer.is_none()
            && let Some(name) = self.griffin_name.take()
        {
            self.meta.slicer = Some(match self.griffin_version.take() {
                Some(v) => format!("{name} {v}"),
                None => name,
            });
        }
        if self.meta.filament_used_mm.is_none() {
            self.meta.filament_used_mm = self.idea_material;
        }
        self.meta.filament_used_g = self.total_grams.or(self.grams_per_extruder);
        if let Some(neg) = self.meta.first_layer_height.filter(|v| *v < 0.0) {
            // Percentage of the layer height.
            self.meta.first_layer_height = self.meta.layer_height.map(|h| h * -neg / 100.0);
        }
        if self.cmd_hotend.is_some() {
            self.meta.hotend_temp = self.cmd_hotend;
        }
        if self.cmd_bed.is_some() {
            self.meta.bed_temp = self.cmd_bed;
        }
        self.meta
    }
}

fn set(slot: &mut Option<f32>, value: Option<f32>) {
    if slot.is_none() {
        *slot = value.filter(|v| v.is_finite());
    }
}

fn set_u32(slot: &mut Option<u32>, value: Option<f32>) {
    if slot.is_none()
        && let Some(v) = value.filter(|v| v.is_finite() && *v >= 0.0 && *v < 1e9)
    {
        *slot = Some(v as u32);
    }
}

fn set_string(slot: &mut Option<String>, value: String) {
    if slot.is_none() && !value.trim().is_empty() {
        *slot = Some(value);
    }
}

/// "PrusaSlicer 2.7.1+win64" -> "PrusaSlicer 2.7.1".
fn clean_generator(name: &str) -> String {
    let name = name.trim();
    match name.split_once(' ') {
        Some((product, version)) => {
            let version = version.trim();
            let version = version.split('+').next().unwrap_or(version);
            format!("{product} {version}")
        }
        None => name.to_string(),
    }
}

/// `key<sep>value` with a plain-text key (letters, digits, spaces, `_`,
/// `[`, `]`, `(`, `)`, `.`, `-`).
fn split_key_value<'a>(c: &'a str, sep: &str) -> Option<(&'a str, &'a str)> {
    let (key, value) = c.split_once(sep)?;
    let key = key.trim();
    if key.is_empty()
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b" _[]().-^/".contains(&b))
    {
        return None;
    }
    Some((key, value.trim()))
}

/// "; thumbnail begin 220x124 12345" (and `thumbnail_JPG`/`_QOI`/`_PNG`).
fn thumbnail_begin(c: &str) -> Option<ThumbState> {
    let rest = c.strip_prefix("thumbnail")?;
    let (tag, rest) = rest.split_once(' ')?;
    let format = match tag {
        "" | "_PNG" => ThumbnailFormat::Png,
        "_JPG" | "_JPEG" => ThumbnailFormat::Jpeg,
        "_QOI" => ThumbnailFormat::Qoi,
        _ => return None,
    };
    let rest = rest.trim_start().strip_prefix("begin")?;
    let mut parts = rest.split_whitespace();
    let (width, height) = parts
        .next()
        .and_then(|d| d.split_once(['x', 'X']))
        .map(|(w, h)| (w.parse().unwrap_or(0), h.parse().unwrap_or(0)))
        .unwrap_or((0, 0));
    let expected = parts
        .next()
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(0);
    Some(ThumbState {
        width,
        height,
        format,
        base64: String::with_capacity(expected.min(THUMBNAIL_MAX_BASE64)),
    })
}

fn sniff_format(data: &[u8]) -> Option<ThumbnailFormat> {
    if data.starts_with(b"\x89PNG") {
        Some(ThumbnailFormat::Png)
    } else if data.starts_with(&[0xFF, 0xD8]) {
        Some(ThumbnailFormat::Jpeg)
    } else if data.starts_with(b"qoif") {
        Some(ThumbnailFormat::Qoi)
    } else {
        None
    }
}

/// Width and height from a PNG IHDR chunk.
fn png_size(data: &[u8]) -> Option<(u32, u32)> {
    let w = data.get(16..20)?;
    let h = data.get(20..24)?;
    Some((
        u32::from_be_bytes([w[0], w[1], w[2], w[3]]),
        u32::from_be_bytes([h[0], h[1], h[2], h[3]]),
    ))
}

/// First number in `s` (`"0.45mm"` -> 0.45, `"1.75,1.75"` -> 1.75).
pub(crate) fn first_number(s: &str) -> Option<f32> {
    let b = s.as_bytes();
    let start = b
        .iter()
        .position(|c| c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.'))?;
    let mut end = start + 1;
    let mut dot = b.get(start) == Some(&b'.');
    while let Some(&c) = b.get(end) {
        if c.is_ascii_digit() {
            end += 1;
        } else if c == b'.' && !dot {
            dot = true;
            end += 1;
        } else {
            break;
        }
    }
    s.get(start..end)?
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite())
}

/// Sum of a comma/semicolon separated list of numbers.
fn sum_list(s: &str) -> Option<f32> {
    let mut any = false;
    let mut total = 0.0f32;
    for part in s.split([',', ';']) {
        if let Some(v) = first_number(part) {
            any = true;
            total += v;
        }
    }
    any.then_some(total)
}

/// Durations written by slicers: `"1d 2h 3m 4s"`, `"2h3m"`, `"6666"`
/// (seconds), `"1 hour 2 minutes"`, `"45.5 minutes"`.
pub fn parse_duration(s: &str) -> Option<f32> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut total = 0.0f64;
    let mut any = false;
    while i < b.len() {
        if !(b[i].is_ascii_digit() || b[i] == b'.') {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
            i += 1;
        }
        let value: f64 = s.get(start..i)?.parse().ok()?;
        while i < b.len() && b[i] == b' ' {
            i += 1;
        }
        let unit_start = i;
        while i < b.len() && b[i].is_ascii_alphabetic() {
            i += 1;
        }
        let unit = s.get(unit_start..i).unwrap_or("").to_ascii_lowercase();
        let factor = match unit.as_str() {
            "d" | "day" | "days" => 86400.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            "" | "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            _ => return None,
        };
        total += value * factor;
        any = true;
    }
    (any && total.is_finite()).then_some(total as f32)
}

/// True when the command `t` starts with `code` (ASCII case-insensitive)
/// followed by whitespace, a comment or the end of the line.
fn is_code(t: &str, code: &str) -> bool {
    let Some(prefix) = t.get(..code.len()) else {
        return false;
    };
    prefix.eq_ignore_ascii_case(code)
        && t.get(code.len()..)
            .and_then(|rest| rest.chars().next())
            .is_none_or(|c| c.is_whitespace() || c == ';')
}

fn is_command_line(line: &str) -> bool {
    let t = line.trim_start();
    !t.is_empty() && !t.starts_with(';') && !t.starts_with('(')
}

/// Value of a numeric parameter in a raw command (`M104 S215` -> 215).
fn param_value(t: &str, letter: char) -> Option<f32> {
    let code_end = t.find(|c: char| c.is_whitespace()).unwrap_or(t.len());
    let rest = t.get(code_end..)?;
    let rest = rest.split(';').next().unwrap_or(rest);
    for word in rest.split_whitespace() {
        let mut chars = word.chars();
        if chars
            .next()
            .is_some_and(|c| c.eq_ignore_ascii_case(&letter))
        {
            return first_number(chars.as_str());
        }
    }
    None
}

/// `M486 A"name"` / `M486 S0 Aname`.
fn m486_name(t: &str) -> Option<&str> {
    let t = t.split(';').next().unwrap_or(t);
    let rest = t.get(4..)?;
    let b = rest.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let at_word_start = i == 0 || b[i - 1].is_ascii_whitespace();
        if at_word_start && (b[i] == b'A' || b[i] == b'a') {
            let after = rest.get(i + 1..)?;
            if let Some(quoted) = after.strip_prefix('"') {
                return Some(quoted.split('"').next().unwrap_or(quoted));
            }
            let name = after.trim();
            return (!name.is_empty()).then_some(name);
        }
        i += 1;
    }
    None
}

/// `EXCLUDE_OBJECT_DEFINE NAME=part_1 CENTER=...`.
fn klipper_name(t: &str) -> Option<&str> {
    let cmd = crate::parse::ExtendedCommand::parse(t.split(';').next().unwrap_or(t))?;
    cmd.param("NAME")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(text: &str) -> SlicerMetadata {
        read_metadata(&GcodeFile::from_text("t", text))
    }

    /// A minimal valid 1x1 PNG.
    fn tiny_png() -> Vec<u8> {
        vec![
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0,
            0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0, 0x90, 0x77, 0x53, 0xDE,
        ]
    }

    fn thumbnail_block(tag: &str, w: u32, h: u32, data: &[u8]) -> String {
        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
        let mut s = format!("; {tag} begin {w}x{h} {}\n", b64.len());
        for chunk in b64.as_bytes().chunks(78) {
            s.push_str("; ");
            s.push_str(std::str::from_utf8(chunk).unwrap_or(""));
            s.push('\n');
        }
        s.push_str(&format!("; {tag} end\n;\n"));
        s
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("1d 2h 3m 4s"), Some(93784.0));
        assert_eq!(parse_duration("12m 34s"), Some(754.0));
        assert_eq!(parse_duration("2h3m"), Some(7380.0));
        assert_eq!(parse_duration("6666"), Some(6666.0));
        assert_eq!(parse_duration(" 1 hour 2 minutes"), Some(3720.0));
        assert_eq!(parse_duration("1 hours 0 minutes"), Some(3600.0));
        assert_eq!(parse_duration("45.5 minutes"), Some(2730.0));
        assert_eq!(parse_duration("soon"), None);
        assert_eq!(parse_duration("3 fortnights"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn numbers() {
        assert_eq!(first_number("0.45mm"), Some(0.45));
        assert_eq!(first_number(" 1.75,1.75"), Some(1.75));
        assert_eq!(first_number("abc"), None);
        assert_eq!(first_number("-"), None);
        assert_eq!(sum_list("1.5, 2.5"), Some(4.0));
        assert_eq!(sum_list("x"), None);
    }

    #[test]
    fn prusaslicer() {
        let png = tiny_png();
        let mut text = String::from(
            "; generated by PrusaSlicer 2.7.1+win64 on 2024-01-15 at 10:00:00 UTC\n\n;\n\n",
        );
        text.push_str(&thumbnail_block("thumbnail", 16, 16, &png));
        text.push_str(&thumbnail_block("thumbnail_QOI", 220, 124, b"qoif-data"));
        text.push_str("; external perimeters extrusion width = 0.45mm\n\nM73 P0 R12\nM201 X1000\n;TYPE:Custom\nM104 S215\nM140 S60\nG28\n");
        text.push_str(";LAYER_CHANGE\n;Z:0.2\n; printing object cube id:0 copy 0\nG1 X1 E1\n; stop printing object cube id:0 copy 0\n");
        text.push_str(";LAYER_CHANGE\n;Z:0.4\n; printing object never-seen\nG1 X2 E2\n");
        for i in 0..300 {
            text.push_str(&format!("G1 X{i} E0.1\n"));
        }
        text.push_str("M107\nM84\n; filament used [mm] = 1234.56, 10\n; filament used [cm3] = 2.97\n; filament used [g] = 3.68, 0.02\n; total filament used [g] = 3.70\n; estimated printing time (normal mode) = 1h 2m 3s\n\n");
        text.push_str("; prusaslicer_config = begin\n; bed_temperature = 60\n; filament_density = 1.24\n; filament_diameter = 1.75,1.75\n; filament_type = PLA;PETG\n; first_layer_height = 0.3\n; gcode_flavor = marlin2\n; layer_height = 0.2\n; nozzle_diameter = 0.4,0.4\n; temperature = 210\n; prusaslicer_config = end\n");
        let m = meta(&text);
        assert_eq!(m.slicer.as_deref(), Some("PrusaSlicer 2.7.1"));
        assert_eq!(m.estimated_time, Some(3723.0));
        assert_eq!(m.filament_used_mm, Some(1244.56));
        assert_eq!(m.filament_used_g, Some(3.70));
        assert_eq!(m.layer_height, Some(0.2));
        assert_eq!(m.first_layer_height, Some(0.3));
        assert_eq!(m.nozzle_diameter, Some(0.4));
        assert_eq!(m.filament_diameter, Some(1.75));
        assert_eq!(m.filament_density, Some(1.24));
        assert_eq!(m.filament_type.as_deref(), Some("PLA"));
        // Commanded temperatures win over the config values.
        assert_eq!(m.hotend_temp, Some(215.0));
        assert_eq!(m.bed_temp, Some(60.0));
        assert_eq!(m.flavor.as_deref(), Some("marlin2"));
        assert_eq!(m.object_names, vec!["cube id:0 copy 0".to_string()]);
        assert_eq!(m.thumbnails.len(), 2);
        assert_eq!((m.thumbnails[0].width, m.thumbnails[0].height), (16, 16));
        assert_eq!(m.thumbnails[0].format, ThumbnailFormat::Png);
        assert_eq!(m.thumbnails[0].data, png);
        assert_eq!(m.thumbnails[1].format, ThumbnailFormat::Qoi);
        assert_eq!(m.settings.len(), 9);
        assert_eq!(
            m.settings[0],
            ("bed_temperature".to_string(), "60".to_string())
        );
    }

    #[test]
    fn old_prusa_percent_first_layer() {
        let m = meta("G28\n; layer_height = 0.2\n; first_layer_height = 150%\n");
        assert_eq!(m.first_layer_height, Some(0.3));
    }

    #[test]
    fn trailing_settings_of_a_short_file() {
        // Old Slic3r/PrusaSlicer dump the config without block markers. In a
        // file short enough for the head scan to reach its end, the trailing
        // block must still be reported, but not header lines.
        let m = meta(
            "; generated by Slic3r 1.3.0 on 2018-01-01\n; external perimeters extrusion width = 0.45mm\nG28\n;LAYER_CHANGE\nG1 X1 E1\nM84\n; filament used = 12.3mm\n\n; layer_height = 0.2\n; perimeters = 2\n",
        );
        assert_eq!(
            m.settings,
            vec![
                ("filament used".to_string(), "12.3mm".to_string()),
                ("layer_height".to_string(), "0.2".to_string()),
                ("perimeters".to_string(), "2".to_string()),
            ]
        );
    }

    #[test]
    fn cura() {
        let text = ";FLAVOR:Marlin\n;TIME:6666\n;Filament used: 1.23456m, 0.5m\n;Layer height: 0.2\n;MINX:1\n;Generated with Cura_SteamEngine 5.6.0\nM140 S60\nM105\nM190 S60\nM104 S200\nM109 S200\nG28\n;LAYER_COUNT:300\n;LAYER:0\n;MESH:vase.stl\nG1 X1 E1\n;MESH:NONMESH\n;LAYER:1\n;MESH:other.stl\n;End of Gcode\n;SETTING_3 {\"global_quality\": \"[general]\\\\nversion = 4\\\\nna\n;SETTING_3 me = Vase #1\\\\ndefinition = custom\"}\n";
        let m = meta(text);
        assert_eq!(m.slicer.as_deref(), Some("Cura_SteamEngine 5.6.0"));
        assert_eq!(m.estimated_time, Some(6666.0));
        assert!((m.filament_used_mm.unwrap_or_default() - 1734.56).abs() < 0.01);
        assert_eq!(m.layer_height, Some(0.2));
        assert_eq!(m.flavor.as_deref(), Some("Marlin"));
        assert_eq!(m.layer_count, Some(300));
        assert_eq!(m.object_names, vec!["vase.stl".to_string()]);
        assert_eq!(m.hotend_temp, Some(200.0));
        assert_eq!(m.bed_temp, Some(60.0));
        assert!(m.settings.is_empty());
    }

    #[test]
    fn cura_thumbnail_after_header() {
        let mut text =
            String::from(";FLAVOR:Marlin\n;TIME:100\n;Generated with Cura_SteamEngine 5.6.0\n");
        text.push_str(&thumbnail_block("thumbnail", 0, 0, &tiny_png()));
        text.push_str("M104 S200\n;LAYER:0\nG1 X1 E1\n");
        let m = meta(&text);
        assert_eq!(m.thumbnails.len(), 1);
        // Size recovered from the PNG header.
        assert_eq!((m.thumbnails[0].width, m.thumbnails[0].height), (1, 1));
    }

    #[test]
    fn simplify3d() {
        let text = "; G-Code generated by Simplify3D(R) Version 4.1.2\n; Oct 12, 2020 at 9:12:40 PM\n; Settings Summary\n;   processName,Process1\n;   applyToModels,cube,cylinder\n;   extruderDiameter,0.4\n;   layerHeight,0.2\n;   firstLayerHeightPercentage,150\n;   filamentDiameters,1.75|1.75\n;   filamentDensities,1.25|1.25\n;   printMaterial,PLA\nG90\nM82\nM140 S60\nM104 S205\n; layer 1, Z = 0.300\n; feature outer perimeter\nG1 X1 E1\n; layer 2, Z = 0.5\nG1 X2 E2\n; Build Summary\n;   Build time: 1 hours 2 minutes\n;   Filament length: 1234.5 mm (1.23 m)\n;   Plastic volume: 2965.12 mm^3 (2.97 cc)\n;   Plastic weight: 3.68 g (0.01 lb)\n;   Material cost: 0.07\n";
        let m = meta(text);
        assert_eq!(m.slicer.as_deref(), Some("Simplify3D 4.1.2"));
        assert_eq!(m.estimated_time, Some(3720.0));
        assert_eq!(m.filament_used_mm, Some(1234.5));
        assert_eq!(m.filament_used_g, Some(3.68));
        assert_eq!(m.layer_height, Some(0.2));
        assert!((m.first_layer_height.unwrap_or_default() - 0.3).abs() < 1e-6);
        assert_eq!(m.nozzle_diameter, Some(0.4));
        assert_eq!(m.filament_diameter, Some(1.75));
        assert_eq!(m.filament_density, Some(1.25));
        assert_eq!(m.filament_type.as_deref(), Some("PLA"));
        assert_eq!(
            m.object_names,
            vec!["cube".to_string(), "cylinder".to_string()]
        );
        assert_eq!(m.settings.len(), 8);
    }

    #[test]
    fn orca_and_bambu() {
        let mut text = String::from(
            "; HEADER_BLOCK_START\n; generated by OrcaSlicer 2.1.1 on 2024-05-01 at 12:00:00\n; model printing time: 1h 2m 3s; total estimated time: 1h 5m 0s\n; total layer number: 100\n; total filament length [mm] : 1500.5\n; total filament weight [g] : 4.5\n; HEADER_BLOCK_END\n\n; THUMBNAIL_BLOCK_START\n",
        );
        text.push_str(&thumbnail_block("thumbnail", 300, 300, &tiny_png()));
        text.push_str("; THUMBNAIL_BLOCK_END\n\n; CONFIG_BLOCK_START\n; filament_type = PETG\n; initial_layer_print_height = 0.25\n; layer_height = 0.16\n; nozzle_temperature = 240\n; hot_plate_temp = 70\n; CONFIG_BLOCK_END\n");
        text.push_str("EXCLUDE_OBJECT_DEFINE NAME=cube_id_0_copy_0 CENTER=10,10 POLYGON=[[0,0],[1,1]]\nEXCLUDE_OBJECT_DEFINE NAME='second part'\n;LAYER_CHANGE\n;Z:0.25\nG1 X1 E1\n;LAYER_CHANGE\n");
        text.push_str(
            "; filament used [mm] = 1500.50\n; estimated printing time (normal mode) = 1h 6m\n",
        );
        let m = meta(&text);
        assert_eq!(m.slicer.as_deref(), Some("OrcaSlicer 2.1.1"));
        assert_eq!(m.estimated_time, Some(3900.0));
        assert_eq!(m.layer_count, Some(100));
        assert_eq!(m.filament_used_mm, Some(1500.5));
        assert_eq!(m.filament_used_g, Some(4.5));
        assert_eq!(m.first_layer_height, Some(0.25));
        assert_eq!(m.layer_height, Some(0.16));
        assert_eq!(m.hotend_temp, Some(240.0));
        assert_eq!(m.bed_temp, Some(70.0));
        assert_eq!(m.filament_type.as_deref(), Some("PETG"));
        assert_eq!(m.thumbnails.len(), 1);
        assert_eq!(
            m.object_names,
            vec!["cube_id_0_copy_0".to_string(), "second part".to_string()]
        );
        assert!(
            m.settings
                .iter()
                .any(|(k, v)| k == "layer_height" && v == "0.16")
        );

        let m = meta(
            "; HEADER_BLOCK_START\n; BambuStudio 01.08.04.51\n; model printing time: 30m; total estimated time: 35m 10s\n; HEADER_BLOCK_END\n",
        );
        assert_eq!(m.slicer.as_deref(), Some("BambuStudio 01.08.04.51"));
        assert_eq!(m.estimated_time, Some(2110.0));
    }

    #[test]
    fn double_layer_markers_count_once() {
        // OrcaSlicer tags each layer twice; object labels inside the first
        // layer must still be read.
        let mut text = String::from(
            "; generated by OrcaSlicer 2.1.1 on 2024-05-01 at 12:00:00\nG28\nM486 T2\n;LAYER_CHANGE\n;Z:0.2\n; CHANGE_LAYER\n; Z_HEIGHT: 0.2\nM486 S0 A\"cube\"\nG1 X1 E1\nM486 S1 A\"cylinder\"\nG1 X2 E1\n;LAYER_CHANGE\n;Z:0.4\n; CHANGE_LAYER\nM486 S2 A\"late\"\n",
        );
        for i in 0..300 {
            text.push_str(&format!("G1 X{i} E0.1\n"));
        }
        let m = meta(&text);
        assert_eq!(m.object_names, ["cube", "cylinder"]);
    }

    #[test]
    fn ideamaker_and_kisslicer() {
        let m = meta(
            ";Sliced by ideaMaker 4.4.1.6145, Tue Jan 2 2024\n;Dimension:220.000000 220.000000 250.000000 0.400000\n;Print Time: 3600\n;Material#1 Used: 1000.5\n;Material#2 Used: 0.50\n;Filament Diameter #1: 2.85\n;Filament Density #1: 1.27\nG28\n;LAYER:0\n",
        );
        assert_eq!(m.slicer.as_deref(), Some("ideaMaker 4.4.1.6145"));
        assert_eq!(m.estimated_time, Some(3600.0));
        assert_eq!(m.filament_used_mm, Some(1001.0));
        assert_eq!(m.filament_diameter, Some(2.85));
        assert_eq!(m.filament_density, Some(1.27));

        let m = meta(
            "; KISSlicer - PRO\n; version 2.0.7\n;    Estimated Build Time:   45.50 minutes\n; layer_thickness_mm = 0.25\n; fiber_dia_mm = 1.75\nG28\n",
        );
        assert_eq!(m.slicer.as_deref(), Some("KISSlicer"));
        assert_eq!(m.estimated_time, Some(2730.0));
        assert_eq!(m.layer_height, Some(0.25));
        assert_eq!(m.filament_diameter, Some(1.75));
    }

    #[test]
    fn marlin_object_labels() {
        let m = meta(
            "M486 T2\nM486 S0 A\"Shape-Box id:0 copy 0\"\nM486 S1\nM486 ACylinder\nM486 S-1\n",
        );
        assert_eq!(
            m.object_names,
            vec!["Shape-Box id:0 copy 0".to_string(), "Cylinder".to_string()]
        );
    }

    #[test]
    fn layer_markers() {
        for c in [
            "LAYER_CHANGE",
            "LAYER:0",
            "LAYER:-2",
            "CHANGE_LAYER",
            "layer 3, Z = 0.6",
            "BEGIN_LAYER_OBJECT z=0.2",
        ] {
            assert!(is_layer_marker(c), "{c}");
        }
        for c in [
            "LAYER_COUNT:30",
            "LAYER:",
            "layerHeight,0.2",
            "layer_height = 0.2",
            "TYPE:Skirt",
        ] {
            assert!(!is_layer_marker(c), "{c}");
        }
    }

    #[test]
    fn broken_blocks_are_bounded() {
        // A thumbnail block that never ends must not swallow the file, and
        // garbage base64 is dropped.
        let m = meta(
            "; thumbnail begin 10x10 999\n; !!!!\nG28\n; thumbnail begin 2x2 4\n; ####\n; thumbnail end\n; thumbnail begin axb\n; thumbnail end\n",
        );
        assert!(m.thumbnails.is_empty());
        let m = meta("");
        assert_eq!(m, SlicerMetadata::default());
        let m = meta("; generated by\n; estimated printing time (normal mode) = garbage\n;TIME:\n");
        assert_eq!(m.estimated_time, None);
    }

    #[test]
    fn tail_is_scanned_without_reading_everything() {
        let mut text = String::from("; generated by PrusaSlicer 2.7.1 on x\n;LAYER_CHANGE\n");
        for i in 0..20000 {
            text.push_str(&format!(";LAYER_CHANGE\nG1 X{i} E1\n"));
        }
        text.push_str("M84\n; filament used [mm] = 99.5\n; layer_height = 0.15\n");
        let m = meta(&text);
        assert_eq!(m.filament_used_mm, Some(99.5));
        assert_eq!(m.layer_height, Some(0.15));
    }
}
