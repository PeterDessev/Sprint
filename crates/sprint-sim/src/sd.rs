//! In-memory SD card: FAT-style 8.3 short names, long names, synthetic
//! G-code content for seeded files and storage for uploaded files.

use crate::rng::fnv1a;
use std::fmt::Write as _;

/// Total bytes the card accepts for uploads.
pub(crate) const CARD_CAPACITY: usize = 256 * 1024 * 1024;

/// One file on the card.
#[derive(Clone, Debug)]
pub(crate) struct SdFile {
    /// Name as created (LFN).
    pub long_name: String,
    /// FAT 8.3 name ("CUBE~1.GCO").
    pub short_name: String,
    /// Size in bytes.
    pub size: u64,
    /// Content; generated lazily for seeded files.
    data: Option<Vec<u8>>,
}

/// The card.
#[derive(Clone, Debug)]
pub(crate) struct SdCard {
    files: Vec<SdFile>,
    seed: u64,
}

const VALID_SHORT: &str = "!#$%&'()-@^_`{}~";

fn short_char(c: char) -> Option<char> {
    let u = c.to_ascii_uppercase();
    if u.is_ascii_alphanumeric() || VALID_SHORT.contains(u) {
        Some(u)
    } else {
        None
    }
}

/// Whether `name` is already a valid 8.3 name (case-insensitively).
fn fits_8_3(name: &str) -> bool {
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) => (b, e),
        None => (name, ""),
    };
    !base.is_empty()
        && base.len() <= 8
        && ext.len() <= 3
        && !(name.contains('.') && ext.is_empty())
        && base
            .chars()
            .chain(ext.chars())
            .all(|c| short_char(c).is_some())
}

impl SdCard {
    /// A card with the seeded files.
    pub(crate) fn new(files: &[(String, u64)], seed: u64) -> SdCard {
        let mut card = SdCard {
            files: Vec::new(),
            seed,
        };
        for (name, size) in files {
            let name = clean_name(name);
            if name.is_empty() || card.find(&name).is_some() {
                continue;
            }
            let short_name = card.short_name_for(&name);
            card.files.push(SdFile {
                long_name: name,
                short_name,
                size: *size,
                data: None,
            });
        }
        card
    }

    /// Files on the card.
    #[cfg(test)]
    pub(crate) fn files(&self) -> &[SdFile] {
        &self.files
    }

    /// Generate a unique 8.3 name for a long name.
    pub(crate) fn short_name_for(&self, long: &str) -> String {
        let trimmed = long.trim_start_matches('.');
        if fits_8_3(trimmed) {
            let upper = trimmed.to_ascii_uppercase();
            if !self.files.iter().any(|f| f.short_name == upper) {
                return upper;
            }
        }
        let (base, ext) = match trimmed.rsplit_once('.') {
            Some((b, e)) if !b.is_empty() => (b, e),
            _ => (trimmed, ""),
        };
        let clean = |s: &str| -> String {
            s.chars()
                .filter(|c| *c != ' ' && *c != '.')
                .map(|c| short_char(c).unwrap_or('_'))
                .collect()
        };
        let mut base: String = clean(base);
        if base.is_empty() {
            base.push('_');
        }
        let ext: String = clean(ext).chars().take(3).collect();
        for n in 1..1_000_000u32 {
            let suffix = format!("~{n}");
            let keep = 8usize.saturating_sub(suffix.len());
            let stem: String = base.chars().take(keep.min(6)).collect();
            let candidate = if ext.is_empty() {
                format!("{stem}{suffix}")
            } else {
                format!("{stem}{suffix}.{ext}")
            };
            if !self.files.iter().any(|f| f.short_name == candidate) {
                return candidate;
            }
        }
        "NONAME~1.GCO".to_string()
    }

    /// Find a file by short or long name (case-insensitive, leading '/'
    /// ignored).
    pub(crate) fn find(&self, name: &str) -> Option<usize> {
        let n = clean_name(name);
        if n.is_empty() {
            return None;
        }
        self.files
            .iter()
            .position(|f| f.short_name.eq_ignore_ascii_case(&n))
            .or_else(|| self.files.iter().position(|f| f.long_name == n))
            .or_else(|| {
                self.files
                    .iter()
                    .position(|f| f.long_name.eq_ignore_ascii_case(&n))
            })
    }

    /// Find with Marlin's M23 fallback: the name up to the first space.
    pub(crate) fn find_lenient(&self, name: &str) -> Option<usize> {
        self.find(name).or_else(|| {
            let first = name.trim().split(' ').next().unwrap_or("");
            self.find(first)
        })
    }

    /// The file at `index`.
    pub(crate) fn file(&self, index: usize) -> Option<&SdFile> {
        self.files.get(index)
    }

    /// The content of a file, generating synthetic content on first use.
    pub(crate) fn data(&mut self, index: usize) -> &[u8] {
        let seed = self.seed;
        match self.files.get_mut(index) {
            Some(f) => {
                if f.data.is_none() {
                    f.data = Some(synthesize(&f.long_name, f.size, seed));
                }
                f.data.as_deref().unwrap_or(&[])
            }
            None => &[],
        }
    }

    /// Create (or truncate) a file for writing; returns its index.
    pub(crate) fn create(&mut self, name: &str) -> Option<usize> {
        let name = clean_name(name);
        if name.is_empty() || name.len() > 255 {
            return None;
        }
        if let Some(i) = self.find(&name) {
            let f = &mut self.files[i];
            f.data = Some(Vec::new());
            f.size = 0;
            return Some(i);
        }
        let short_name = self.short_name_for(&name);
        self.files.push(SdFile {
            long_name: name,
            short_name,
            size: 0,
            data: Some(Vec::new()),
        });
        Some(self.files.len() - 1)
    }

    fn used_bytes(&self) -> usize {
        self.files
            .iter()
            .map(|f| f.data.as_ref().map_or(f.size as usize, Vec::len))
            .sum()
    }

    /// Append one line (plus "\n") to a file being written. Returns false
    /// when the card is full.
    pub(crate) fn append_line(&mut self, index: usize, line: &str) -> bool {
        if self.used_bytes() + line.len() + 1 > CARD_CAPACITY {
            return false;
        }
        let seed = self.seed;
        let Some(f) = self.files.get_mut(index) else {
            return false;
        };
        let data = f
            .data
            .get_or_insert_with(|| synthesize(&f.long_name, f.size, seed));
        data.extend_from_slice(line.as_bytes());
        data.push(b'\n');
        f.size = data.len() as u64;
        true
    }

    /// Delete a file; returns whether it existed.
    pub(crate) fn delete(&mut self, name: &str) -> Option<usize> {
        let i = self.find(name)?;
        self.files.remove(i);
        Some(i)
    }

    /// Marlin M20 listing lines (between "Begin file list" and "End file
    /// list").
    pub(crate) fn listing(&self, long: bool) -> Vec<String> {
        self.files
            .iter()
            .map(|f| {
                if long {
                    format!("{} {} {}", f.short_name, f.size, f.long_name)
                } else {
                    format!("{} {}", f.short_name, f.size)
                }
            })
            .collect()
    }
}

/// Normalise a requested file name: trim, drop a leading '/', and remove
/// control characters.
fn clean_name(name: &str) -> String {
    name.trim()
        .trim_start_matches('/')
        .chars()
        .filter(|c| !c.is_control() && *c != '*')
        .collect::<String>()
        .trim()
        .to_string()
}

/// Deterministic, realistic-looking G-code of exactly `size` bytes.
pub(crate) fn synthesize(name: &str, size: u64, seed: u64) -> Vec<u8> {
    let size = usize::try_from(size.min(crate::config::MAX_SYNTHETIC_FILE_SIZE)).unwrap_or(0);
    let mut out = String::with_capacity(size + 64);
    let footer = "M104 S0\nM140 S0\nM107\nG91\nG1 Z10 F600\nG90\nG1 X0 Y200 F3000\nM84\n; end of synthetic print\n";
    if size > 600 {
        let budget = size - footer.len() - 1;
        let h = fnv1a(name.as_bytes()) ^ seed;
        let side = 20.0 + (h % 15) as f64;
        let cx = 110.0;
        let cy = 110.0;
        let mut body = String::new();
        let _ = write!(
            body,
            "; Synthetic print generated by the Sprint virtual printer\n; file: {}\n; layer_height = 0.2\n; object size = {side:.0} mm\nM140 S60\nM104 S200\nM190 S60\nM109 S200\nG21\nG90\nM82\nM107\nG28\nG92 E0\nG1 Z0.2 F600\n",
            name.replace(['\n', '\r'], " ")
        );
        let mut e = 0.0f64;
        let mut z = 0.2f64;
        let mut layer = 0u32;
        let per_mm = 0.0333;
        'layers: loop {
            if z > 240.0 {
                break;
            }
            let mut lines = format!(";LAYER:{layer}\nG1 Z{z:.2} F600\n");
            if layer == 1 {
                lines.push_str("M106 S255\n");
            }
            for p in 0..3 {
                let half = side / 2.0 - f64::from(p) * 0.45;
                let corners = [
                    (cx - half, cy - half),
                    (cx + half, cy - half),
                    (cx + half, cy + half),
                    (cx - half, cy + half),
                    (cx - half, cy - half),
                ];
                let _ = writeln!(lines, "G1 X{:.3} Y{:.3} F9000", corners[0].0, corners[0].1);
                for w in corners.windows(2) {
                    let len = ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt();
                    e += len * per_mm;
                    let _ = writeln!(lines, "G1 X{:.3} Y{:.3} E{e:.5} F1800", w[1].0, w[1].1);
                }
            }
            let inner = side / 2.0 - 1.35;
            let mut x = cx - inner;
            let mut up = true;
            let _ = writeln!(lines, "G1 X{:.3} Y{:.3} F9000", x, cy - inner);
            while x <= cx + inner {
                let ty = if up { cy + inner } else { cy - inner };
                e += 2.0 * inner * per_mm;
                let _ = writeln!(lines, "G1 X{x:.3} Y{ty:.3} E{e:.5} F3000");
                x += 0.45;
                up = !up;
                if x <= cx + inner {
                    let _ = writeln!(lines, "G1 X{x:.3} Y{ty:.3} F3000");
                }
            }
            if body.len() + lines.len() > budget {
                // Fill the rest of the budget with whole lines of this layer.
                for l in lines.lines() {
                    if body.len() + l.len() + 1 > budget {
                        break 'layers;
                    }
                    body.push_str(l);
                    body.push('\n');
                }
                break;
            }
            body.push_str(&lines);
            layer += 1;
            z += 0.2;
        }
        out.push_str(&body);
        // Pad before the footer so the print still ends with it.
        pad_to(&mut out, size - footer.len());
        out.push_str(footer);
    }
    pad_to(&mut out, size);
    let mut bytes = out.into_bytes();
    bytes.truncate(size);
    bytes
}

/// Append short comment lines (at most 80 bytes each, like a slicer's
/// thumbnail block) until `out` is `len` bytes long.
fn pad_to(out: &mut String, len: usize) {
    const LINE: usize = 80;
    while out.len() < len {
        let left = len - out.len();
        // Never leave a single byte over for the last line.
        let n = if left > LINE && left - LINE == 1 {
            LINE - 1
        } else {
            left.min(LINE)
        };
        if n == 1 {
            out.push('\n');
        } else {
            out.push(';');
            out.extend(std::iter::repeat_n(' ', n - 2));
            out.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_names_follow_fat_rules() {
        let card = SdCard::new(
            &[
                ("calibration_cube.gcode".into(), 10),
                ("calibration_cube_v2.gcode".into(), 10),
                ("CUBE.GCO".into(), 10),
                ("first layer test.gcode".into(), 10),
                ("x.y.z.gcode".into(), 10),
                ("noext".into(), 10),
            ],
            1,
        );
        let names: Vec<&str> = card.files().iter().map(|f| f.short_name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "CALIBR~1.GCO",
                "CALIBR~2.GCO",
                "CUBE.GCO",
                "FIRSTL~1.GCO",
                "XYZ~1.GCO",
                "NOEXT"
            ]
        );
    }

    #[test]
    fn find_by_short_long_and_case() {
        let card = SdCard::new(&[("Benchy 0.2mm.gcode".into(), 5)], 1);
        assert_eq!(card.find("BENCHY~1.GCO"), Some(0));
        assert_eq!(card.find("/benchy~1.gco"), Some(0));
        assert_eq!(card.find("Benchy 0.2mm.gcode"), Some(0));
        assert_eq!(card.find("benchy 0.2MM.gcode"), Some(0));
        assert_eq!(card.find("nope.gco"), None);
        assert_eq!(card.find(""), None);
        assert_eq!(card.find_lenient("benchy~1.gco extra"), Some(0));
    }

    #[test]
    fn synthetic_content_has_exact_size() {
        for size in [0u64, 1, 2, 5, 599, 601, 700, 12_630, 48_213, 300_001] {
            let d = synthesize("cube.gcode", size, 3);
            assert_eq!(d.len() as u64, size, "size {size}");
        }
        let d = synthesize("cube.gcode", 48_213, 3);
        let text = String::from_utf8(d).unwrap();
        assert!(text.contains("M109 S200"));
        assert!(text.contains("G1 X"));
        assert!(text.contains("M84"));
        assert_eq!(
            synthesize("cube.gcode", 5000, 3),
            synthesize("cube.gcode", 5000, 3)
        );
    }

    #[test]
    fn large_synthetic_files_have_short_lines_and_end_with_the_footer() {
        // Far more than the layers fill (they stop at the top of the
        // volume): the rest must still be ordinary short lines, before
        // the end-of-print footer.
        for size in [12_000_000u64, 5_000_000, 700, 1000] {
            let d = synthesize("big.gcode", size, 3);
            assert_eq!(d.len() as u64, size);
            let longest = d.split(|&b| b == b'\n').map(<[u8]>::len).max();
            assert!(
                longest.unwrap_or(0) <= 96,
                "size {size}: line of {longest:?}"
            );
            let text = String::from_utf8(d).unwrap();
            assert!(
                text.ends_with("M84\n; end of synthetic print\n"),
                "size {size}: {}",
                &text[text.len().saturating_sub(80)..]
            );
        }
    }

    #[test]
    fn create_append_delete() {
        let mut card = SdCard::new(&[], 1);
        let i = card.create("test.gco").unwrap();
        assert!(card.append_line(i, "G28"));
        assert!(card.append_line(i, "G1 X10"));
        assert_eq!(card.file(i).unwrap().size, 11);
        assert_eq!(card.data(i), b"G28\nG1 X10\n");
        assert_eq!(card.listing(false), vec!["TEST.GCO 11".to_string()]);
        assert_eq!(card.listing(true), vec!["TEST.GCO 11 test.gco".to_string()]);
        // Re-creating truncates.
        let j = card.create("TEST.GCO").unwrap();
        assert_eq!(i, j);
        assert_eq!(card.file(j).unwrap().size, 0);
        assert!(card.delete("test.gco").is_some());
        assert!(card.delete("test.gco").is_none());
        assert!(card.create("   ").is_none());
    }
}
