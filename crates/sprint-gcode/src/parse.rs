//! G-code line lexer.
//!
//! [`parse_line`] / [`parse_into`] split one line into its parts (line
//! number, command code, parameter words, free-text argument, comment and
//! checksum). [`clean_line`] produces the text that is sent to a printer.
//!
//! The lexer works on bytes and never panics: unknown characters are skipped,
//! numbers that do not fit are treated as missing, and all slicing happens at
//! ASCII delimiters so it is always on UTF-8 character boundaries.

use std::borrow::Cow;
use std::fmt;

/// A G/M/T (or other letter) command code such as `G1`, `M104`, `T0` or
/// `G29.1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Code {
    /// Command letter, uppercase ASCII (`b'G'`, `b'M'` or `b'T'`).
    pub letter: u8,
    /// Command number (`1` for `G1`, `104` for `M104`).
    pub number: u16,
    /// Sub-code after a dot (`Some(1)` for `G29.1`).
    pub sub: Option<u8>,
}

impl Code {
    /// A code without sub-code. `letter` is uppercased.
    pub const fn new(letter: u8, number: u16) -> Code {
        Code {
            letter: letter.to_ascii_uppercase(),
            number,
            sub: None,
        }
    }

    /// `G<n>`.
    pub const fn g(n: u16) -> Code {
        Code::new(b'G', n)
    }

    /// `M<n>`.
    pub const fn m(n: u16) -> Code {
        Code::new(b'M', n)
    }

    /// `T<n>`.
    pub const fn t(n: u16) -> Code {
        Code::new(b'T', n)
    }

    /// True for exactly `<letter><number>` without a sub-code
    /// (`Code::g(29).is('G', 29)` but not `G29.1`). `letter` is
    /// case-insensitive.
    pub fn is(&self, letter: char, number: u16) -> bool {
        self.sub.is_none()
            && self.number == number
            && letter.is_ascii()
            && self.letter == (letter as u8).to_ascii_uppercase()
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", char::from(self.letter), self.number)?;
        if let Some(sub) = self.sub {
            write!(f, ".{sub}")?;
        }
        Ok(())
    }
}

/// One parameter word such as `X10.5`.
///
/// A word written without a number (`X` in `G28 X`) has `value: f64::NAN`;
/// [`ParsedLine::get`] reports it as absent while [`ParsedLine::has`] still
/// sees it. Quoted string parameters (`P"file.g"`, RepRapFirmware style) are
/// also stored with a NaN value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Word {
    /// Parameter letter, uppercase ASCII.
    pub letter: u8,
    /// Numeric value, or NaN when the word has none.
    pub value: f64,
}

/// One parsed line. Borrowed from the source text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedLine<'a> {
    /// Leading `N` word.
    pub line_number: Option<u32>,
    /// First command word (`G1`, `M104`, `T0`, `G29.1`).
    pub code: Option<Code>,
    /// Parameters after the code, in source order.
    pub words: Vec<Word>,
    /// Free-text argument of commands that take one (M23/M28/M30/M32/M33
    /// file names, M117/M118 messages, M928), trimmed. For extended commands
    /// (Klipper `SET_FAN_SPEED SPEED=1`, RepRapFirmware meta commands, ...)
    /// that have no `code`, this holds the whole command without comment.
    pub text: Option<&'a str>,
    /// Text after `;` (preferred) or inside the first `(...)`, trimmed.
    pub comment: Option<&'a str>,
    /// `*nn` checksum.
    pub checksum: Option<u8>,
}

impl<'a> ParsedLine<'a> {
    /// Numeric value of the parameter `letter` (case-insensitive). `None`
    /// when absent or written without a number.
    pub fn get(&self, letter: char) -> Option<f64> {
        let letter = ascii_upper(letter)?;
        self.words
            .iter()
            .find(|w| w.letter == letter)
            .map(|w| w.value)
            .filter(|v| !v.is_nan())
    }

    /// True when the parameter `letter` appears, with or without a value.
    pub fn has(&self, letter: char) -> bool {
        match ascii_upper(letter) {
            Some(l) => self.words.iter().any(|w| w.letter == l),
            None => false,
        }
    }

    /// No code and no words (blank lines, comments, extended commands).
    pub fn is_empty(&self) -> bool {
        self.code.is_none() && self.words.is_empty()
    }

    /// True when the line's code is exactly `<letter><number>`.
    pub fn is(&self, letter: char, number: u16) -> bool {
        self.code.is_some_and(|c| c.is(letter, number))
    }

    /// Reset to the empty state, keeping the word buffer's allocation.
    pub fn clear(&mut self) {
        self.line_number = None;
        self.code = None;
        self.words.clear();
        self.text = None;
        self.comment = None;
        self.checksum = None;
    }
}

fn ascii_upper(c: char) -> Option<u8> {
    if c.is_ascii() {
        Some((c as u8).to_ascii_uppercase())
    } else {
        None
    }
}

/// Commands whose argument is free text (file name or message).
fn is_text_command(code: Code) -> bool {
    code.letter == b'M'
        && code.sub.is_none()
        && matches!(code.number, 23 | 28 | 30 | 32 | 33 | 117 | 118 | 928)
}

/// Parse one line. Accepts `G1X10Y20`, lowercase, extra spaces and tabs,
/// `N12 ... *34`, `;` and `(...)` comments, `%` program markers. Never
/// panics. Unknown/extended commands (Klipper `SET_FAN_SPEED SPEED=1`) yield
/// `code: None` and no words; their text is in [`ParsedLine::text`].
pub fn parse_line(line: &str) -> ParsedLine<'_> {
    let mut out = ParsedLine::default();
    parse_into(line, &mut out);
    out
}

/// Like [`parse_line`] but reuses `out` (and its word buffer), so parsing a
/// whole file performs no allocation after the first few lines.
pub fn parse_into<'a>(line: &'a str, out: &mut ParsedLine<'a>) {
    out.clear();
    let b = line.as_bytes();
    let Some(prefix) = scan_prefix(b) else {
        // Blank, comment-only or `%` line: still record the comment.
        record_leading_comments(line, out);
        return;
    };
    out.line_number = prefix.line_number;
    // Paren comments before the command.
    if let Some(c) = prefix.paren_comment {
        out.comment = sub(line, c.0, c.1).map(str::trim);
    }
    let i = prefix.start;
    match first_token(b, i) {
        FirstToken::Extended => parse_extended(line, i, out),
        FirstToken::Words => parse_words(line, i, out),
        FirstToken::Code { code, end } => {
            out.code = Some(code);
            if is_text_command(code) {
                parse_text_argument(line, end, out);
            } else {
                parse_words(line, end, out);
            }
        }
    }
}

/// Where the command starts after the optional line number and leading
/// paren comments.
struct Prefix {
    start: usize,
    line_number: Option<u32>,
    numbered: bool,
    paren_comment: Option<(usize, usize)>,
}

/// Skip leading whitespace, the `N` word and leading `(...)` comments.
/// Returns `None` when the line holds no command (blank, comment-only,
/// `%` marker, or only a checksum).
fn scan_prefix(b: &[u8]) -> Option<Prefix> {
    let n = b.len();
    let mut i = skip_ws(b, 0);
    if i >= n || b[i] == b'%' {
        return None;
    }
    let mut line_number = None;
    let mut numbered = false;
    if let Some((num, end)) = line_number_at(b, i) {
        line_number = num;
        numbered = true;
        i = skip_ws(b, end);
    }
    let mut paren_comment = None;
    loop {
        if i >= n {
            return None;
        }
        match b[i] {
            b';' | b'*' => return None,
            b'(' => {
                let close = find_byte(b, i + 1, n, b')');
                if paren_comment.is_none() {
                    paren_comment = Some((i + 1, close));
                }
                i = skip_ws(b, (close + 1).min(n));
            }
            _ => break,
        }
    }
    Some(Prefix {
        start: i,
        line_number,
        numbered,
        paren_comment,
    })
}

/// For lines without a command: pick up the `;` or `(...)` comment (and the
/// line number / checksum of e.g. `N3 *12`).
fn record_leading_comments<'a>(line: &'a str, out: &mut ParsedLine<'a>) {
    let b = line.as_bytes();
    let n = b.len();
    let mut i = skip_ws(b, 0);
    if i >= n {
        return;
    }
    if b[i] == b'%' {
        let rest = sub(line, i + 1, n).map(str::trim).unwrap_or("");
        if !rest.is_empty() {
            out.comment = Some(rest);
        }
        return;
    }
    if let Some((num, end)) = line_number_at(b, i) {
        out.line_number = num;
        i = end;
    }
    while i < n {
        match b[i] {
            b';' => {
                out.comment = sub(line, i + 1, n).map(str::trim);
                return;
            }
            b'(' => {
                let close = find_byte(b, i + 1, n, b')');
                if out.comment.is_none() {
                    out.comment = sub(line, i + 1, close).map(str::trim);
                }
                i = close + 1;
            }
            b'*' => {
                let (cs, end) = checksum_at(b, i);
                out.checksum = cs;
                i = end;
            }
            _ => i += 1,
        }
    }
}

enum FirstToken {
    /// Not a classic `<letter><number>` command: kept verbatim.
    Extended,
    /// A parameter-like first word (`X10 Y20`, `S100`): no code.
    Words,
    /// A G/M/T code ending at `end`.
    Code { code: Code, end: usize },
}

/// Classify the first token of a command starting at `i` (non-whitespace).
fn first_token(b: &[u8], i: usize) -> FirstToken {
    let Some(&c0) = b.get(i) else {
        return FirstToken::Extended;
    };
    if !c0.is_ascii_alphabetic() || !b.get(i + 1).is_some_and(u8::is_ascii_digit) {
        return FirstToken::Extended;
    }
    let letter = c0.to_ascii_uppercase();
    if !matches!(letter, b'G' | b'M' | b'T') {
        return FirstToken::Words;
    }
    match parse_code_number(b, i + 1) {
        Some((number, sub, end)) => FirstToken::Code {
            code: Code {
                letter,
                number,
                sub,
            },
            end,
        },
        None => FirstToken::Extended,
    }
}

/// Parse `29` or `29.1` starting at `i`. `None` on overflow.
fn parse_code_number(b: &[u8], mut i: usize) -> Option<(u16, Option<u8>, usize)> {
    let mut number: u32 = 0;
    let mut any = false;
    while let Some(&d) = b.get(i) {
        if !d.is_ascii_digit() {
            break;
        }
        number = number * 10 + u32::from(d - b'0');
        if number > u32::from(u16::MAX) {
            return None;
        }
        any = true;
        i += 1;
    }
    if !any {
        return None;
    }
    let number = u16::try_from(number).ok()?;
    let mut sub = None;
    if b.get(i) == Some(&b'.') && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
        i += 1;
        let mut s: u32 = 0;
        while let Some(&d) = b.get(i) {
            if !d.is_ascii_digit() {
                break;
            }
            s = s * 10 + u32::from(d - b'0');
            if s > u32::from(u8::MAX) {
                return None;
            }
            i += 1;
        }
        sub = Some(u8::try_from(s).ok()?);
    }
    Some((number, sub, i))
}

/// Parameter words from `i` to the end of the line.
fn parse_words<'a>(line: &'a str, mut i: usize, out: &mut ParsedLine<'a>) {
    let b = line.as_bytes();
    let n = b.len();
    while i < n {
        let c = b[i];
        if c.is_ascii_alphabetic() {
            let letter = c.to_ascii_uppercase();
            i += 1;
            if b.get(i) == Some(&b'"') {
                i = skip_quoted(b, i);
                out.words.push(Word {
                    letter,
                    value: f64::NAN,
                });
                continue;
            }
            let (value, end) = parse_number(b, i);
            i = end;
            out.words.push(Word {
                letter,
                value: value.unwrap_or(f64::NAN),
            });
        } else {
            match c {
                b';' => {
                    out.comment = sub(line, i + 1, n).map(str::trim);
                    return;
                }
                b'(' => {
                    let close = find_byte(b, i + 1, n, b')');
                    if out.comment.is_none() {
                        out.comment = sub(line, i + 1, close).map(str::trim);
                    }
                    i = close + 1;
                }
                b'*' => {
                    let (cs, end) = checksum_at(b, i);
                    out.checksum = cs;
                    // Anything after the checksum is ignored, except a comment.
                    if let Some(semi) = find_opt(b, end, n, b';') {
                        out.comment = sub(line, semi + 1, n).map(str::trim);
                    }
                    return;
                }
                b'"' => i = skip_quoted(b, i),
                _ => i += 1,
            }
        }
    }
}

/// Free-text argument of M117 & co., from `start` (just after the code).
fn parse_text_argument<'a>(line: &'a str, start: usize, out: &mut ParsedLine<'a>) {
    let b = line.as_bytes();
    let n = b.len();
    let mut end = n;
    if let Some(semi) = find_opt(b, start, n, b';') {
        out.comment = sub(line, semi + 1, n).map(str::trim);
        end = semi;
    }
    if out.line_number.is_some()
        && let Some((cs, star)) = trailing_checksum(b, start, end)
    {
        out.checksum = cs;
        end = star;
    }
    let text = sub(line, start, end).map(str::trim).unwrap_or("");
    if !text.is_empty() {
        out.text = Some(text);
    }
}

/// Extended command (Klipper/RRF): whole command as text, no words.
fn parse_extended<'a>(line: &'a str, start: usize, out: &mut ParsedLine<'a>) {
    let b = line.as_bytes();
    let n = b.len();
    let mut end = n;
    if let Some(semi) = find_opt(b, start, n, b';') {
        out.comment = sub(line, semi + 1, n).map(str::trim);
        end = semi;
    }
    if out.line_number.is_some()
        && let Some((cs, star)) = trailing_checksum(b, start, end)
    {
        out.checksum = cs;
        end = star;
    }
    let text = sub(line, start, end).map(str::trim).unwrap_or("");
    if !text.is_empty() {
        out.text = Some(text);
    }
}

/// `*nn` at the end of `b[start..end]` (allowing trailing whitespace).
/// Returns the checksum and the index of `*`.
fn trailing_checksum(b: &[u8], start: usize, end: usize) -> Option<(Option<u8>, usize)> {
    let mut j = end;
    while j > start && is_ws(b[j - 1]) {
        j -= 1;
    }
    let digits_end = j;
    while j > start && b[j - 1].is_ascii_digit() {
        j -= 1;
    }
    if j == digits_end || j == start || b[j - 1] != b'*' {
        return None;
    }
    let (cs, _) = checksum_at(b, j - 1);
    Some((cs, j - 1))
}

/// Parse `*nn` at `i` (which holds `*`). Returns the value (None if missing
/// or > 255) and the index after the digits.
fn checksum_at(b: &[u8], i: usize) -> (Option<u8>, usize) {
    let mut j = i + 1;
    let mut value: u32 = 0;
    let mut any = false;
    while let Some(&d) = b.get(j) {
        if !d.is_ascii_digit() {
            break;
        }
        value = value.saturating_mul(10).saturating_add(u32::from(d - b'0'));
        any = true;
        j += 1;
    }
    let cs = if any { u8::try_from(value).ok() } else { None };
    (cs, j)
}

/// `N<digits>` at `i`: returns the number (None on overflow) and the end.
fn line_number_at(b: &[u8], i: usize) -> Option<(Option<u32>, usize)> {
    if !matches!(b.get(i), Some(b'N' | b'n')) || !b.get(i + 1).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let mut j = i + 1;
    let mut value: u64 = 0;
    while let Some(&d) = b.get(j) {
        if !d.is_ascii_digit() {
            break;
        }
        value = value.saturating_mul(10).saturating_add(u64::from(d - b'0'));
        j += 1;
    }
    Some((u32::try_from(value).ok(), j))
}

const POW10: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];

/// Parse `[+-]digits[.digits]` at `i` (no exponent: `E` is an axis letter).
/// Returns `None` when there is no digit or the value is not finite, and the
/// index after the number.
pub(crate) fn parse_number(b: &[u8], mut i: usize) -> (Option<f64>, usize) {
    let mut neg = false;
    if let Some(&s) = b.get(i)
        && (s == b'-' || s == b'+')
    {
        neg = s == b'-';
        i += 1;
    }
    let mut mantissa: u64 = 0;
    let mut sig_digits = 0u32;
    let mut frac_digits = 0u32;
    let mut int_overflow = 0i32;
    let mut any = false;
    let mut dot = false;
    while let Some(&c) = b.get(i) {
        if c.is_ascii_digit() {
            any = true;
            if mantissa == 0 && c == b'0' {
                // Leading zeros carry no significance.
                if dot {
                    frac_digits += 1;
                }
            } else if sig_digits < 19 {
                mantissa = mantissa * 10 + u64::from(c - b'0');
                sig_digits += 1;
                if dot {
                    frac_digits += 1;
                }
            } else if !dot {
                int_overflow += 1;
            }
            i += 1;
        } else if c == b'.' && !dot {
            dot = true;
            i += 1;
        } else {
            break;
        }
    }
    if !any {
        return (None, i);
    }
    let mut value = mantissa as f64;
    if int_overflow > 0 {
        value *= 10f64.powi(int_overflow);
    }
    if frac_digits > 0 {
        value = match POW10.get(frac_digits as usize) {
            Some(p) => value / p,
            None => value / 10f64.powi(frac_digits as i32),
        };
    }
    if neg {
        value = -value;
    }
    if value.is_finite() {
        (Some(value), i)
    } else {
        (None, i)
    }
}

#[inline]
fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c)
}

#[inline]
fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && is_ws(b[i]) {
        i += 1;
    }
    i
}

/// Index of `needle` in `b[from..to]`, or `to` if absent.
#[inline]
fn find_byte(b: &[u8], from: usize, to: usize, needle: u8) -> usize {
    find_opt(b, from, to, needle).unwrap_or(to)
}

#[inline]
fn find_opt(b: &[u8], from: usize, to: usize, needle: u8) -> Option<usize> {
    b.get(from..to)?
        .iter()
        .position(|&c| c == needle)
        .map(|p| p + from)
}

/// Skip a `"..."` string starting at `i` (which holds the opening quote).
/// A doubled quote (`""`) inside is an escaped quote (RepRapFirmware).
fn skip_quoted(b: &[u8], i: usize) -> usize {
    let n = b.len();
    let mut j = i + 1;
    while j < n {
        if b[j] == b'"' {
            if b.get(j + 1) == Some(&b'"') {
                j += 2;
                continue;
            }
            return j + 1;
        }
        j += 1;
    }
    n
}

#[inline]
fn sub(s: &str, from: usize, to: usize) -> Option<&str> {
    s.get(from..to.min(s.len()))
}

/// True when `clean_line(line)` would return `Some`, without allocating.
pub fn has_command(line: &str) -> bool {
    scan_prefix(line.as_bytes()).is_some()
}

/// The text to send to a printer: comments removed (`;` and `(...)`), any
/// existing `N` word and `*checksum` removed, whitespace trimmed and runs of
/// spaces collapsed. Returns `None` when nothing is left. Text arguments of
/// M117/M118/M23/... are preserved verbatim (a `;` inside an M117 message
/// still starts a comment, as in Marlin).
///
/// Classic commands are uppercased (`g1 x10` → `G1 X10`) for firmwares that
/// are case-sensitive; quoted strings, text arguments and extended commands
/// (Klipper macros, `@` host commands, ...) are kept exactly as written.
/// When nothing needs to change the result borrows from `line`.
pub fn clean_line(line: &str) -> Option<Cow<'_, str>> {
    let b = line.as_bytes();
    let n = b.len();
    let prefix = scan_prefix(b)?;
    let i = prefix.start;
    match first_token(b, i) {
        FirstToken::Extended => {
            let mut end = find_byte(b, i, n, b';');
            if prefix.numbered
                && let Some((_, star)) = trailing_checksum(b, i, end)
            {
                end = star;
            }
            let s = sub(line, i, end)?.trim_end();
            (!s.is_empty()).then_some(Cow::Borrowed(s))
        }
        FirstToken::Code {
            code,
            end: code_end,
        } if is_text_command(code) => {
            let mut end = find_byte(b, code_end, n, b';');
            if prefix.numbered
                && let Some((_, star)) = trailing_checksum(b, code_end, end)
            {
                end = star;
            }
            let code_text = sub(line, i, code_end)?;
            let rest = sub(line, code_end, end)?;
            let text = rest.trim();
            let code_upper = !code_text.bytes().any(|c| c.is_ascii_lowercase());
            if text.is_empty() {
                return Some(if code_upper {
                    Cow::Borrowed(code_text)
                } else {
                    Cow::Owned(code_text.to_ascii_uppercase())
                });
            }
            // Borrow when the source already reads "CODE text".
            let lead = rest.len() - rest.trim_start().len();
            if code_upper && lead == 1 && rest.starts_with(' ') {
                let text_end = code_end + 1 + text.len();
                if let Some(s) = sub(line, i, text_end) {
                    return Some(Cow::Borrowed(s));
                }
            }
            let mut s = String::with_capacity(code_text.len() + 1 + text.len());
            s.push_str(&code_text.to_ascii_uppercase());
            s.push(' ');
            s.push_str(text);
            Some(Cow::Owned(s))
        }
        FirstToken::Code { .. } | FirstToken::Words => clean_classic(line, i),
    }
}

/// Builds a `Cow<str>` that stays borrowed while the output is a contiguous
/// slice of the source.
struct CowBuilder<'a> {
    src: &'a str,
    start: usize,
    end: usize,
    owned: Option<String>,
}

impl<'a> CowBuilder<'a> {
    fn new(src: &'a str) -> Self {
        CowBuilder {
            src,
            start: 0,
            end: 0,
            owned: None,
        }
    }

    fn is_empty(&self) -> bool {
        match &self.owned {
            Some(s) => s.is_empty(),
            None => self.start == self.end,
        }
    }

    fn materialize(&mut self) -> &mut String {
        let (src, start, end) = (self.src, self.start, self.end);
        self.owned
            .get_or_insert_with(|| src.get(start..end).unwrap_or("").to_string())
    }

    /// Append `src[from..to]`.
    fn push_src(&mut self, from: usize, to: usize) {
        if self.owned.is_none() {
            if self.start == self.end {
                self.start = from;
                self.end = to;
                return;
            }
            if from == self.end {
                self.end = to;
                return;
            }
        }
        let piece = self.src.get(from..to).unwrap_or("");
        self.materialize().push_str(piece);
    }

    fn push_str(&mut self, s: &str) {
        self.materialize().push_str(s);
    }

    fn finish(self) -> Option<Cow<'a, str>> {
        let cow = match self.owned {
            Some(s) => Cow::Owned(s),
            None => Cow::Borrowed(self.src.get(self.start..self.end).unwrap_or("")),
        };
        (!cow.is_empty()).then_some(cow)
    }
}

fn clean_classic(line: &str, start: usize) -> Option<Cow<'_, str>> {
    let b = line.as_bytes();
    let n = b.len();
    let mut out = CowBuilder::new(line);
    let mut pending_space = false;
    let mut j = start;
    while j < n {
        match b[j] {
            c if is_ws(c) => {
                pending_space = true;
                j += 1;
            }
            b';' | b'*' => break,
            b'(' => {
                let close = find_byte(b, j + 1, n, b')');
                pending_space = true;
                j = (close + 1).min(n);
            }
            b'"' => {
                let end = skip_quoted(b, j);
                emit_space(&mut out, b, &mut pending_space, j);
                out.push_src(j, end);
                j = end;
            }
            _ => {
                let run_start = j;
                let mut lower = false;
                while j < n {
                    let c = b[j];
                    if is_ws(c) || matches!(c, b';' | b'*' | b'(' | b'"') {
                        break;
                    }
                    lower |= c.is_ascii_lowercase();
                    j += 1;
                }
                emit_space(&mut out, b, &mut pending_space, run_start);
                if lower {
                    let piece = line.get(run_start..j).unwrap_or("");
                    out.push_str(&piece.to_ascii_uppercase());
                } else {
                    out.push_src(run_start, j);
                }
            }
        }
    }
    out.finish()
}

/// Emit one separating space before the token at `next` if whitespace (or a
/// removed comment) preceded it.
fn emit_space(out: &mut CowBuilder<'_>, b: &[u8], pending: &mut bool, next: usize) {
    if !*pending {
        return;
    }
    *pending = false;
    if out.is_empty() {
        return;
    }
    if out.owned.is_none() && out.end + 1 == next && b.get(out.end) == Some(&b' ') {
        out.push_src(out.end, next);
    } else {
        out.push_str(" ");
    }
}

/// An extended (non `G`/`M`/`T`) command such as Klipper's
/// `SET_FAN_SPEED FAN=part SPEED=0.5`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtendedCommand<'a> {
    /// Command name as written (`SET_FAN_SPEED`).
    pub name: &'a str,
    /// Everything after the name, trimmed.
    pub args: &'a str,
}

impl<'a> ExtendedCommand<'a> {
    /// Split an extended command line (comment already removed, e.g.
    /// [`ParsedLine::text`]) into name and arguments.
    pub fn parse(text: &'a str) -> Option<ExtendedCommand<'a>> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let (name, args) = match text.find(|c: char| c.is_whitespace()) {
            Some(p) => (&text[..p], text[p..].trim()),
            None => (text, ""),
        };
        Some(ExtendedCommand { name, args })
    }

    /// True when the name equals `name`, ignoring ASCII case.
    pub fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    /// Value of a `KEY=value` argument (key case-insensitive). Values may be
    /// quoted with `"` or `'`; the quotes are removed.
    pub fn param(&self, key: &str) -> Option<&'a str> {
        let args = self.args;
        let bytes = args.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            while i < bytes.len() && is_ws(bytes[i]) {
                i += 1;
            }
            let key_start = i;
            while i < bytes.len() && bytes[i] != b'=' && !is_ws(bytes[i]) {
                i += 1;
            }
            let this_key = args.get(key_start..i).unwrap_or("");
            if bytes.get(i) != Some(&b'=') {
                continue;
            }
            i += 1;
            let (value_start, value_end);
            match bytes.get(i) {
                Some(&q) if q == b'"' || q == b'\'' => {
                    value_start = i + 1;
                    let close = find_byte(bytes, value_start, bytes.len(), q);
                    value_end = close;
                    i = (close + 1).min(bytes.len());
                }
                _ => {
                    value_start = i;
                    while i < bytes.len() && !is_ws(bytes[i]) {
                        i += 1;
                    }
                    value_end = i;
                }
            }
            if this_key.eq_ignore_ascii_case(key) {
                return args.get(value_start..value_end);
            }
        }
        None
    }

    /// Numeric value of a `KEY=value` argument.
    pub fn param_f64(&self, key: &str) -> Option<f64> {
        self.param(key)?
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(p: &ParsedLine<'_>) -> Vec<(char, f64)> {
        p.words
            .iter()
            .map(|w| (char::from(w.letter), w.value))
            .collect()
    }

    #[test]
    fn code_display_and_is() {
        assert_eq!(Code::g(1).to_string(), "G1");
        assert_eq!(Code::m(104).to_string(), "M104");
        let c = Code {
            letter: b'G',
            number: 29,
            sub: Some(1),
        };
        assert_eq!(c.to_string(), "G29.1");
        assert!(!c.is('G', 29));
        assert!(Code::g(28).is('g', 28));
        assert!(!Code::g(28).is('M', 28));
        assert!(!Code::g(28).is('é', 28));
        assert_eq!(Code::t(3).to_string(), "T3");
    }

    #[test]
    fn simple_move() {
        let p = parse_line("G1 X10 Y20.5 E0.123 F1800");
        assert_eq!(p.code, Some(Code::g(1)));
        assert_eq!(
            words(&p),
            vec![('X', 10.0), ('Y', 20.5), ('E', 0.123), ('F', 1800.0)]
        );
        assert_eq!(p.get('x'), Some(10.0));
        assert!(p.has('F'));
        assert!(!p.has('Z'));
        assert_eq!(p.comment, None);
    }

    #[test]
    fn packed_and_lowercase() {
        let p = parse_line("G1X10Y20");
        assert_eq!(p.code, Some(Code::g(1)));
        assert_eq!(words(&p), vec![('X', 10.0), ('Y', 20.0)]);
        let p = parse_line("g1 x10 y-2");
        assert_eq!(p.code, Some(Code::g(1)));
        assert_eq!(words(&p), vec![('X', 10.0), ('Y', -2.0)]);
        let p = parse_line("G01 X1");
        assert_eq!(p.code, Some(Code::g(1)));
    }

    #[test]
    fn signs_and_decimals() {
        let p = parse_line("G1 X-.5 Y+2 Z1. E-0.00001");
        assert_eq!(
            words(&p),
            vec![('X', -0.5), ('Y', 2.0), ('Z', 1.0), ('E', -0.00001)]
        );
        // "E" is an axis letter, never an exponent.
        let p = parse_line("G1 X1E5");
        assert_eq!(words(&p), vec![('X', 1.0), ('E', 5.0)]);
    }

    #[test]
    fn number_parsing_matches_std() {
        for s in [
            "0.1",
            "123.456",
            "-7.25",
            "0.00001",
            "99999.99999",
            "1",
            "-0",
            "3.14159265358979",
            "0.3",
            "1234567.891",
        ] {
            let (v, _) = parse_number(s.as_bytes(), 0);
            assert_eq!(v, Some(s.parse::<f64>().unwrap_or_default()), "{s}");
        }
        let (v, end) = parse_number(b"-", 0);
        assert_eq!((v, end), (None, 1));
        let huge = "9".repeat(400);
        assert_eq!(parse_number(huge.as_bytes(), 0).0, None);
        let long_frac = format!("1.{}", "5".repeat(40));
        let (v, _) = parse_number(long_frac.as_bytes(), 0);
        assert!((v.unwrap_or_default() - 1.5555555555555556).abs() < 1e-12);
    }

    #[test]
    fn tabs_and_spaces() {
        let p = parse_line("\t G1\t X1 \t Y2   ");
        assert_eq!(p.code, Some(Code::g(1)));
        assert_eq!(words(&p), vec![('X', 1.0), ('Y', 2.0)]);
    }

    #[test]
    fn line_number_and_checksum() {
        let p = parse_line("N12 G1 X1*34");
        assert_eq!(p.line_number, Some(12));
        assert_eq!(p.code, Some(Code::g(1)));
        assert_eq!(words(&p), vec![('X', 1.0)]);
        assert_eq!(p.checksum, Some(34));
        let p = parse_line("N0 M110 N0*125");
        assert_eq!(p.line_number, Some(0));
        assert_eq!(p.code, Some(Code::m(110)));
        assert_eq!(p.get('N'), Some(0.0));
        assert_eq!(p.checksum, Some(125));
        let p = parse_line("n5g28");
        assert_eq!(p.line_number, Some(5));
        assert_eq!(p.code, Some(Code::g(28)));
        let p = parse_line("G1 X1 *999");
        assert_eq!(p.checksum, None);
    }

    #[test]
    fn comments() {
        let p = parse_line("G1 X1 ; move it");
        assert_eq!(p.comment, Some("move it"));
        let p = parse_line(";TYPE:External perimeter");
        assert!(p.is_empty());
        assert_eq!(p.comment, Some("TYPE:External perimeter"));
        let p = parse_line("G1 X10 (mid comment) Y20");
        assert_eq!(words(&p), vec![('X', 10.0), ('Y', 20.0)]);
        assert_eq!(p.comment, Some("mid comment"));
        let p = parse_line("(leading) G28 ; trailing");
        assert_eq!(p.code, Some(Code::g(28)));
        assert_eq!(p.comment, Some("trailing"));
        let p = parse_line("G1 X1 (unclosed");
        assert_eq!(p.comment, Some("unclosed"));
        let p = parse_line("(only a comment)");
        assert!(p.is_empty());
        assert_eq!(p.comment, Some("only a comment"));
    }

    #[test]
    fn percent_marker() {
        let p = parse_line("%");
        assert!(p.is_empty());
        assert_eq!(p.comment, None);
        let p = parse_line("  % program start");
        assert!(p.is_empty());
        assert_eq!(p.comment, Some("program start"));
        assert_eq!(clean_line("%"), None);
    }

    #[test]
    fn text_commands() {
        let p = parse_line("M117 Hello World");
        assert_eq!(p.code, Some(Code::m(117)));
        assert_eq!(p.text, Some("Hello World"));
        assert!(p.words.is_empty());
        let p = parse_line("M117 Layer 3; of 10");
        assert_eq!(p.text, Some("Layer 3"));
        assert_eq!(p.comment, Some("of 10"));
        let p = parse_line("M23 /folder/FILE X.GCO");
        assert_eq!(p.text, Some("/folder/FILE X.GCO"));
        let p = parse_line("N4 M117 Hi there*45");
        assert_eq!(p.text, Some("Hi there"));
        assert_eq!(p.checksum, Some(45));
        let p = parse_line("M117 5*3");
        assert_eq!(p.text, Some("5*3"));
        let p = parse_line("M117");
        assert_eq!(p.code, Some(Code::m(117)));
        assert_eq!(p.text, None);
        let p = parse_line("M118 E1 //action:notification Hi");
        assert_eq!(p.text, Some("E1 //action:notification Hi"));
        let p = parse_line("M117 (not a comment)");
        assert_eq!(p.text, Some("(not a comment)"));
        for cmd in ["M28", "M30", "M32", "M33", "M928"] {
            let line = format!("{cmd} name.gco");
            assert_eq!(parse_line(&line).text, Some("name.gco"), "{cmd}");
        }
    }

    #[test]
    fn tool_and_subcodes() {
        let p = parse_line("M104 T1 S200");
        assert_eq!(p.code, Some(Code::m(104)));
        assert_eq!(p.get('T'), Some(1.0));
        assert_eq!(p.get('S'), Some(200.0));
        let p = parse_line("G29.1");
        assert_eq!(
            p.code,
            Some(Code {
                letter: b'G',
                number: 29,
                sub: Some(1)
            })
        );
        let p = parse_line("T1");
        assert_eq!(p.code, Some(Code::t(1)));
        assert!(p.words.is_empty());
    }

    #[test]
    fn valueless_and_quoted_words() {
        let p = parse_line("G28 X Y");
        assert!(p.has('X') && p.has('Y'));
        assert_eq!(p.get('X'), None);
        let p = parse_line("M98 P\"homeall.g\" S1");
        assert!(p.has('P'));
        assert_eq!(p.get('P'), None);
        assert_eq!(p.get('S'), Some(1.0));
        let p = parse_line("M550 P\"semi;colon\" ; real comment");
        assert_eq!(p.comment, Some("real comment"));
    }

    #[test]
    fn extended_commands() {
        let p = parse_line("SET_FAN_SPEED FAN=part SPEED=0.5 ; cool");
        assert_eq!(p.code, None);
        assert!(p.words.is_empty());
        assert!(p.is_empty());
        assert_eq!(p.text, Some("SET_FAN_SPEED FAN=part SPEED=0.5"));
        assert_eq!(p.comment, Some("cool"));
        let p = parse_line("EXCLUDE_OBJECT_DEFINE NAME=cube_1 CENTER=10,10");
        assert_eq!(p.code, None);
        let p = parse_line("TURN_OFF_HEATERS");
        assert_eq!(p.code, None);
        assert_eq!(p.text, Some("TURN_OFF_HEATERS"));
        let p = parse_line("NOZZLE_CLEAN");
        assert_eq!(p.line_number, None);
        assert_eq!(p.text, Some("NOZZLE_CLEAN"));
        let p = parse_line("T-1");
        assert_eq!(p.code, None);
        let p = parse_line("G99999999");
        assert_eq!(p.code, None);
        let p = parse_line("@pause");
        assert_eq!(p.code, None);
        assert_eq!(p.text, Some("@pause"));
    }

    #[test]
    fn words_without_code() {
        let p = parse_line("X10 Y20");
        assert_eq!(p.code, None);
        assert_eq!(words(&p), vec![('X', 10.0), ('Y', 20.0)]);
        assert!(!p.is_empty());
    }

    #[test]
    fn garbage_never_panics() {
        for s in [
            "",
            " ",
            "*",
            "N",
            "N1",
            "N1 *",
            "((((",
            "))))",
            ";;;;",
            "G",
            "G.",
            "G1.",
            "M117 ;",
            "\u{FFFD}\u{FFFD}G1 X1",
            "G1 X\u{FFFD}1",
            "G1 X1e999",
            "G1 X--1",
            "G1 X1.2.3",
            "\"",
            "G1 P\"unterminated",
            "N99999999999999999999 G1",
            "G1 X1 * ; c",
            "é",
            "G1 Xé",
        ] {
            let _ = parse_line(s);
            let _ = clean_line(s);
        }
    }

    #[test]
    fn clean_basic() {
        assert_eq!(clean_line("G1 X10 Y20").as_deref(), Some("G1 X10 Y20"));
        assert!(matches!(clean_line("G1 X10 Y20"), Some(Cow::Borrowed(_))));
        assert_eq!(
            clean_line("  G1 X10 Y20 ; comment  ").as_deref(),
            Some("G1 X10 Y20")
        );
        assert!(matches!(
            clean_line("  G1 X10 Y20 ; comment  "),
            Some(Cow::Borrowed(_))
        ));
        assert_eq!(clean_line("N12 G1 X1*34").as_deref(), Some("G1 X1"));
        assert_eq!(clean_line("G1  X1\t\tY2").as_deref(), Some("G1 X1 Y2"));
        assert_eq!(
            clean_line("G1 X10 (mid) Y20").as_deref(),
            Some("G1 X10 Y20")
        );
        assert_eq!(clean_line("g1 x10").as_deref(), Some("G1 X10"));
        assert_eq!(clean_line("; only comment"), None);
        assert_eq!(clean_line("   "), None);
        assert_eq!(clean_line("(c)"), None);
        assert_eq!(clean_line("N5 *12"), None);
        assert_eq!(clean_line("M110 N0").as_deref(), Some("M110 N0"));
        assert_eq!(clean_line("N3 M110 N0*99").as_deref(), Some("M110 N0"));
        assert_eq!(clean_line("G1X10Y20").as_deref(), Some("G1X10Y20"));
        assert_eq!(clean_line("G28 (home)").as_deref(), Some("G28"));
        assert!(matches!(clean_line("G28 (home)"), Some(Cow::Borrowed(_))));
    }

    #[test]
    fn clean_text_and_extended() {
        assert_eq!(
            clean_line("M117 Hello  World ; c").as_deref(),
            Some("M117 Hello  World")
        );
        assert!(matches!(
            clean_line("M117 Hello  World ; c"),
            Some(Cow::Borrowed(_))
        ));
        assert_eq!(clean_line("m117 hi").as_deref(), Some("M117 hi"));
        assert_eq!(clean_line("M117   spaced").as_deref(), Some("M117 spaced"));
        assert_eq!(clean_line("M117").as_deref(), Some("M117"));
        assert_eq!(clean_line("N5 M117 Hi*30").as_deref(), Some("M117 Hi"));
        assert_eq!(
            clean_line("M23 /Some Dir/file.gco").as_deref(),
            Some("M23 /Some Dir/file.gco")
        );
        assert_eq!(
            clean_line("SET_FAN_SPEED FAN=x  SPEED=1 ; c").as_deref(),
            Some("SET_FAN_SPEED FAN=x  SPEED=1")
        );
        assert_eq!(
            clean_line("set_fan_speed fan=x").as_deref(),
            Some("set_fan_speed fan=x")
        );
        assert_eq!(
            clean_line("RESPOND MSG=\"(hello)\"").as_deref(),
            Some("RESPOND MSG=\"(hello)\"")
        );
        assert_eq!(
            clean_line("M550 P\"My  Printer;1\" ; c").as_deref(),
            Some("M550 P\"My  Printer;1\"")
        );
    }

    #[test]
    fn clean_then_parse_round_trip() {
        for s in [
            "N12 g1 x10 (c) y-2 e.5 ; hi",
            "M104 T1 S200 *12",
            "G28 X Y",
            "M117 Printing; layer",
            "G29.1 S1",
        ] {
            let a = parse_line(s);
            let cleaned = clean_line(s).unwrap_or_default();
            let b = parse_line(&cleaned);
            assert_eq!(a.code, b.code, "{s}");
            assert_eq!(a.text, b.text, "{s}");
            assert_eq!(a.words.len(), b.words.len(), "{s}");
            for (x, y) in a.words.iter().zip(&b.words) {
                assert_eq!(x.letter, y.letter);
                assert!(x.value == y.value || (x.value.is_nan() && y.value.is_nan()));
            }
        }
    }

    #[test]
    fn has_command_matches_clean_line() {
        for s in [
            "", ";c", "G1", "N1", "N1 G1", "(a) (b)", "(a) G1", "%", "*1", "M117", "  T0  ",
        ] {
            assert_eq!(has_command(s), clean_line(s).is_some(), "{s:?}");
        }
    }

    #[test]
    fn extended_params() {
        let cmd = ExtendedCommand::parse("EXCLUDE_OBJECT_DEFINE NAME='my part' CENTER=1,2")
            .unwrap_or(ExtendedCommand { name: "", args: "" });
        assert!(cmd.is("exclude_object_define"));
        assert_eq!(cmd.param("name"), Some("my part"));
        assert_eq!(cmd.param("CENTER"), Some("1,2"));
        assert_eq!(cmd.param("POLYGON"), None);
        let cmd = ExtendedCommand::parse("SET_VELOCITY_LIMIT ACCEL=3000 VELOCITY=250")
            .unwrap_or(ExtendedCommand { name: "", args: "" });
        assert_eq!(cmd.param_f64("ACCEL"), Some(3000.0));
        assert_eq!(cmd.param_f64("velocity"), Some(250.0));
        assert_eq!(ExtendedCommand::parse("   "), None);
        let cmd = ExtendedCommand::parse("X BROKEN= =1 KEY")
            .unwrap_or(ExtendedCommand { name: "", args: "" });
        assert_eq!(cmd.param("BROKEN"), Some(""));
        assert_eq!(cmd.param("KEY"), None);
    }

    #[test]
    fn parse_into_reuses_buffer() {
        let text = String::from("G1 X1 Y2\nG1 X3 Y4 Z5");
        let mut p = ParsedLine::default();
        for line in text.lines() {
            parse_into(line, &mut p);
            assert!(p.words.len() >= 2);
        }
        assert_eq!(p.get('Z'), Some(5.0));
        parse_into("; nothing", &mut p);
        assert!(p.is_empty());
        assert_eq!(p.get('Z'), None);
    }
}
