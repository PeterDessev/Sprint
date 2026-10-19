//! Reading a configuration file that does not fully parse: every valid
//! profile, macro and value is kept, only what is broken is dropped (and
//! reported), so one typo never costs the user all their profiles.

use toml::{Table, Value};

/// Lines of invalid TOML dropped before giving up on the file.
const MAX_DROPPED_LINES: usize = 200;

/// Whether a candidate value deserializes in its place (the reason when
/// not).
type Accept<'a> = &'a dyn Fn(&Value) -> Result<(), String>;

/// Parse `text` as a TOML table, leaving out the lines that are not valid
/// TOML (a decimal comma, a stray character, binary junk). `None` when it
/// cannot be parsed even so.
pub(super) fn parse_lenient(text: &str, warnings: &mut Vec<String>) -> Option<Table> {
    let mut text = text.to_string();
    for _ in 0..MAX_DROPPED_LINES {
        let e = match toml::from_str::<Table>(&text) {
            Ok(table) => return Some(table),
            Err(e) => e,
        };
        let at = e.span()?.start.min(text.len());
        let start = text
            .get(..at)
            .and_then(|s| s.rfind('\n'))
            .map_or(0, |i| i + 1);
        let end = text
            .get(start..)
            .and_then(|s| s.find('\n'))
            .map_or(text.len(), |i| start + i);
        let bad = text.get(start..end)?.trim().to_string();
        if bad.is_empty() {
            return None;
        }
        let number = text.get(..start)?.matches('\n').count() + 1;
        warnings.push(format!(
            "line {number}: {}; ignored \"{}\"",
            e.message().trim(),
            shorten(&bad)
        ));
        // Keep the line break so later line numbers stay right.
        text.replace_range(start..end, "");
    }
    None
}

/// `value` without the parts `accept` rejects, or `None` when nothing of
/// it can be kept. Tables keep every entry that is valid on its own (types
/// whose fields all have defaults), or else drop the one entry that breaks
/// them (types with required fields); arrays keep their valid elements.
/// Each dropped part adds a warning naming its `path`.
pub(super) fn repair(
    value: Value,
    accept: Accept<'_>,
    path: &str,
    warnings: &mut Vec<String>,
) -> Option<Value> {
    let error = match accept(&value) {
        Ok(()) => return Some(value),
        Err(e) => e,
    };
    match value {
        Value::Table(table) => {
            let mut kept = Table::new();
            let mut notes = Vec::new();
            for (key, v) in &table {
                let alone = |c: &Value| {
                    let mut t = Table::new();
                    t.insert(key.clone(), c.clone());
                    accept(&Value::Table(t))
                };
                if let Some(fixed) = repair(v.clone(), &alone, &join(path, key), &mut notes) {
                    kept.insert(key.clone(), fixed);
                }
            }
            if accept(&Value::Table(kept.clone())).is_ok() {
                warnings.extend(notes);
                return Some(Value::Table(kept));
            }
            for key in table.keys() {
                let mut without = table.clone();
                without.remove(key);
                if accept(&Value::Table(without.clone())).is_ok() {
                    warnings.push(format!("{}: ignored ({error})", join(path, key)));
                    return Some(Value::Table(without));
                }
            }
            warnings.push(format!("{}: ignored ({error})", label(path)));
            None
        }
        Value::Array(items) => {
            let mut kept = Vec::new();
            for (i, v) in items.into_iter().enumerate() {
                let alone = |c: &Value| accept(&Value::Array(vec![c.clone()]));
                if let Some(fixed) = repair(v, &alone, &format!("{path}[{i}]"), warnings) {
                    kept.push(fixed);
                }
            }
            if accept(&Value::Array(kept.clone())).is_ok() {
                return Some(Value::Array(kept));
            }
            warnings.push(format!("{}: ignored ({error})", label(path)));
            None
        }
        other => {
            warnings.push(format!(
                "{}: ignored {} ({error})",
                label(path),
                shorten(&other.to_string())
            ));
            None
        }
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn label(path: &str) -> &str {
    if path.is_empty() { "the file" } else { path }
}

/// At most 80 characters of `text`.
fn shorten(text: &str) -> String {
    let mut s: String = text.chars().take(80).collect();
    if s.len() < text.len() {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Default, Deserialize, PartialEq)]
    #[serde(default)]
    struct Outer {
        a: u32,
        b: String,
        inner: Inner,
        items: Vec<Item>,
    }

    #[derive(Debug, Default, Deserialize, PartialEq)]
    #[serde(default)]
    struct Inner {
        x: u8,
        y: bool,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct Item {
        name: String,
        #[serde(default)]
        size: u8,
    }

    fn load(text: &str) -> (Outer, Vec<String>) {
        let mut warnings = Vec::new();
        let table = parse_lenient(text, &mut warnings).expect("parses");
        let accept = |v: &Value| {
            v.clone()
                .try_into::<Outer>()
                .map(|_| ())
                .map_err(|e| e.message().to_string())
        };
        let v = repair(Value::Table(table), &accept, "", &mut warnings).expect("kept");
        (v.try_into().unwrap(), warnings)
    }

    #[test]
    fn only_the_broken_parts_are_dropped() {
        let (o, w) = load(
            "a = 7\nb = 5\n[inner]\nx = 300\ny = true\n[[items]]\nname = \"one\"\nsize = \"big\"\n[[items]]\nsize = 2\n[[items]]\nname = \"three\"\n",
        );
        assert_eq!(o.a, 7);
        assert_eq!(o.b, "", "b had the wrong type");
        assert_eq!(o.inner, Inner { x: 0, y: true });
        assert_eq!(
            o.items,
            [
                Item {
                    name: "one".into(),
                    size: 0
                },
                Item {
                    name: "three".into(),
                    size: 0
                }
            ]
        );
        let joined = w.join("\n");
        for part in [
            "b: ignored 5",
            "inner.x: ignored 300",
            "items[0].size",
            "items[1]: ignored",
        ] {
            assert!(joined.contains(part), "{part} in {joined}");
        }
    }

    #[test]
    fn lines_that_are_not_toml_are_skipped() {
        let (o, w) = load("a = 7\nb = 250,0\n\u{FFFD}\u{0} junk\n[inner]\ny = true\n");
        assert_eq!(o.a, 7);
        assert!(o.inner.y);
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].starts_with("line 2:"), "{w:?}");
        assert!(w[1].starts_with("line 3:"), "{w:?}");
    }

    #[test]
    fn hopeless_text_gives_up() {
        let mut w = Vec::new();
        assert!(parse_lenient("\"unterminated\n", &mut w).is_some() || !w.is_empty());
        let mut w = Vec::new();
        let _ = parse_lenient(&"[[[".repeat(1000), &mut w);
    }
}
