//! Object labels tracked while analysing (see [`ObjectInfo`]).

use std::collections::HashMap;
use std::ops::Range;

use crate::toolpath::{ObjectInfo, Segment};

/// Most objects tracked; `Segment::NO_OBJECT` is not a valid index, so later
/// labels get no object.
const MAX_OBJECTS: usize = Segment::NO_OBJECT as usize;

#[derive(Debug)]
struct Tracked {
    name: String,
    id: Option<u32>,
    lines: Vec<Range<u32>>,
}

/// Which object the walk is in, and the blocks each object has had so far.
///
/// An object block starts at a start marker and runs to the next start or
/// end marker; `moves` (segments plus E-only moves recorded so far) tells
/// blocks with moves from empty ones (M486 naming blocks in the header,
/// definitions).
#[derive(Debug, Default)]
pub(crate) struct ObjectTracker {
    objects: Vec<Tracked>,
    by_name: HashMap<String, u16>,
    by_id: HashMap<u32, u16>,
    current: Option<u16>,
    /// First line of the current block (its start marker).
    block_start: u32,
    /// `moves` when the current block started.
    block_moves: usize,
}

impl ObjectTracker {
    /// `Segment::object` for a move made now.
    pub(crate) fn current(&self) -> u16 {
        self.current.unwrap_or(Segment::NO_OBJECT)
    }

    /// A marker on line `ln` that starts the object called `name`
    /// (`; printing object`, `;MESH:`, `EXCLUDE_OBJECT_START`).
    pub(crate) fn start_named(&mut self, name: &str, ln: u32, moves: usize) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let index = match self.by_name.get(name) {
            Some(&i) => Some(i),
            None => self.create(name.to_string(), None),
        };
        self.switch(index, ln, moves);
    }

    /// `EXCLUDE_OBJECT_DEFINE NAME=`: the object exists but has not started.
    pub(crate) fn define(&mut self, name: &str) {
        let name = name.trim();
        if !name.is_empty() && !self.by_name.contains_key(name) {
            self.create(name.to_string(), None);
        }
    }

    /// `M486 S<id> [A<name>]` on line `ln`. Without a name the object keeps
    /// the one it was given before, else it is `"Object <id>"`.
    pub(crate) fn start_m486(&mut self, id: u32, name: Option<&str>, ln: u32, moves: usize) {
        let name = name.map(str::trim).filter(|n| !n.is_empty());
        let index = match self.by_id.get(&id) {
            Some(&i) => {
                if let Some(name) = name {
                    self.rename(i, name);
                }
                Some(i)
            }
            None => {
                // The same label written as a comment marker earlier.
                let unnumbered = name
                    .and_then(|n| self.by_name.get(n).copied())
                    .filter(|&i| {
                        self.objects
                            .get(usize::from(i))
                            .is_some_and(|o| o.id.is_none())
                    });
                match unnumbered {
                    Some(i) => {
                        if let Some(obj) = self.objects.get_mut(usize::from(i)) {
                            obj.id = Some(id);
                        }
                        self.by_id.insert(id, i);
                        Some(i)
                    }
                    None => {
                        let name = name.map_or_else(|| format!("Object {id}"), str::to_string);
                        self.create(name, Some(id))
                    }
                }
            }
        };
        self.switch(index, ln, moves);
    }

    /// `M486 A<name>` without `S`: names the current M486 object.
    pub(crate) fn name_current(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if let Some(i) = self.current
            && self
                .objects
                .get(usize::from(i))
                .is_some_and(|o| o.id.is_some())
        {
            self.rename(i, name);
        }
    }

    /// An end marker on line `ln` (`M486 S-1`, `EXCLUDE_OBJECT_END`,
    /// `; stop printing object`, `;MESH:NONMESH`).
    pub(crate) fn end(&mut self, ln: u32, moves: usize) {
        self.switch(None, ln, moves);
    }

    /// Close the last block at the end of the file and total the segments.
    pub(crate) fn finish(
        mut self,
        line_count: u32,
        moves: usize,
        segments: &[Segment],
    ) -> Vec<ObjectInfo> {
        self.switch(None, line_count, moves);
        let mut out: Vec<ObjectInfo> = self
            .objects
            .into_iter()
            .map(|t| ObjectInfo {
                name: t.name,
                id: t.id,
                lines: t.lines,
                ..ObjectInfo::default()
            })
            .collect();
        let mut totals = vec![(0.0f64, 0.0f64); out.len()];
        for s in segments {
            let Some(i) = s.object_index() else {
                continue;
            };
            let (Some(obj), Some(total)) = (out.get_mut(i), totals.get_mut(i)) else {
                continue;
            };
            obj.segments += 1;
            total.0 += f64::from(s.duration);
            if s.is_extrusion() {
                total.1 += f64::from(s.extruded);
                obj.bounds.include_bead(s);
            }
        }
        for (obj, (time, extruded)) in out.iter_mut().zip(totals) {
            obj.time = time as f32;
            obj.extruded = extruded as f32;
        }
        out
    }

    fn create(&mut self, name: String, id: Option<u32>) -> Option<u16> {
        if self.objects.len() >= MAX_OBJECTS {
            return None;
        }
        let index = u16::try_from(self.objects.len()).ok()?;
        self.by_name.entry(name.clone()).or_insert(index);
        if let Some(id) = id {
            self.by_id.insert(id, index);
        }
        self.objects.push(Tracked {
            name,
            id,
            lines: Vec::new(),
        });
        Some(index)
    }

    fn rename(&mut self, index: u16, name: &str) {
        let Some(obj) = self.objects.get_mut(usize::from(index)) else {
            return;
        };
        if obj.name == name {
            return;
        }
        if self.by_name.get(&obj.name) == Some(&index) {
            self.by_name.remove(&obj.name);
        }
        obj.name = name.to_string();
        self.by_name.entry(obj.name.clone()).or_insert(index);
    }

    /// Close the current block on line `ln` and continue in `next`.
    fn switch(&mut self, next: Option<u16>, ln: u32, moves: usize) {
        if let Some(i) = self.current
            && moves > self.block_moves
            && let Some(obj) = self.objects.get_mut(usize::from(i))
        {
            let range = self.block_start..ln.max(self.block_start);
            match obj.lines.last_mut() {
                // Restarted right after its own block: one range.
                Some(last) if last.end.saturating_add(1) >= range.start => last.end = range.end,
                _ => obj.lines.push(range),
            }
        }
        self.current = next;
        self.block_start = ln;
        self.block_moves = moves;
    }
}
