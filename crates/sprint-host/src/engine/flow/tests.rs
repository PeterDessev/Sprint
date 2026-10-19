//! Flow control tests: hand-written protocol scenarios, then randomized
//! runs against a model firmware that behaves like Marlin (line-number
//! checks, `Error` + `Resend` + `ok`, receive buffer flushed on errors,
//! corrupted lines, lost `ok`s), checking that every command is executed
//! exactly once and in order, that a line is only retransmitted after a
//! genuine resend request covering it, and that nothing deadlocks.

use super::*;
use sprint_proto::parse_framed;
use std::collections::HashMap;

type F = Flow<String>;

fn t0() -> Instant {
    Instant::now()
}

fn ms(base: Instant, n: u64) -> Instant {
    base + Duration::from_millis(n)
}

/// Feed one received line the way the engine does.
fn feed(flow: &mut F, line: &str, now: Instant) -> Option<FeedResult> {
    flow.begin_line();
    if let Some(rest) = line.strip_prefix("Error:") {
        if let Some(kind) = classify_line_error(rest) {
            flow.on_error(kind);
        }
        return None;
    }
    if let Some(n) = line.strip_prefix("Resend: ") {
        return Some(FeedResult::Resend(flow.on_resend(n.parse().unwrap(), now)));
    }
    if line == "ok" || line.starts_with("ok ") {
        let n = line
            .split_whitespace()
            .find_map(|w| w.strip_prefix('N').and_then(|v| v.parse().ok()));
        return Some(FeedResult::Ok(flow.on_ok(n, now)));
    }
    None
}

#[derive(Debug)]
enum FeedResult {
    Ok(OkOutcome<String>),
    Resend(ResendAction),
}

fn acked(r: Option<FeedResult>) -> Vec<String> {
    match r {
        Some(FeedResult::Ok(OkOutcome::Acked(v))) => v.into_iter().map(|a| a.meta).collect(),
        other => panic!("expected acks, got {other:?}"),
    }
}

fn resend(r: Option<FeedResult>) -> ResendAction {
    match r {
        Some(FeedResult::Resend(a)) => a,
        other => panic!("expected a resend action, got {other:?}"),
    }
}

/// A flow that has sent "N0 M110 N0" and received its ok.
fn synced(window: usize, now: Instant) -> F {
    let mut f = F::new(window, now);
    let w = f.send_line_reset(0, true, "M110".into(), now);
    assert_eq!(w.text, frame_numbered(0, "M110 N0"));
    assert!(f.barrier_active() && !f.can_send_new());
    assert_eq!(acked(feed(&mut f, "ok", now)), ["M110"]);
    assert!(f.is_idle() && f.can_send_new());
    f
}

fn send(f: &mut F, cmd: &str, now: Instant) -> Wire<String> {
    assert!(f.can_send_new(), "window closed before {cmd}");
    f.send(cmd, true, cmd.to_string(), now)
}

#[test]
fn line_errors_are_classified() {
    use LineError::*;
    let cases = [
        ("checksum mismatch, Last Line: 4", Some(Checksum)),
        ("No Checksum with line number, Last Line: 4", Some(Checksum)),
        (
            "Line Number is not Last Line Number+1, Last Line: 4",
            Some(LineNumber),
        ),
        ("No Line Number with checksum, Last Line: 4", Some(Other)),
        ("Printer halted. kill() called!", None),
        ("MINTEMP triggered", None),
    ];
    for (m, want) in cases {
        assert_eq!(classify_line_error(m), want, "{m}");
    }
}

#[test]
fn ping_pong() {
    let t = t0();
    let mut f = synced(1, t);
    let w = send(&mut f, "G28", t);
    assert_eq!(w.text, "N1 G28*18");
    assert_eq!(w.number, Some(1));
    assert!(!f.can_send_new());
    match feed(&mut f, "ok", ms(t, 7)) {
        Some(FeedResult::Ok(OkOutcome::Acked(v))) => {
            assert_eq!(v.len(), 1);
            assert_eq!(v[0].meta, "G28");
            assert_eq!(v[0].rtt, Duration::from_millis(7));
        }
        other => panic!("{other:?}"),
    }
    // Smoothed with the earlier M110 round trip (0 ms).
    assert_eq!(f.rtt_avg(), Some(Duration::from_micros(700)));
    let w = send(&mut f, "M105", ms(t, 7));
    assert_eq!(w.number, Some(2));
    assert!(matches!(
        feed(&mut f, "ok", ms(t, 8)),
        Some(FeedResult::Ok(OkOutcome::Acked(_)))
    ));
    assert!(matches!(
        feed(&mut f, "ok", ms(t, 9)),
        Some(FeedResult::Ok(OkOutcome::Spurious))
    ));
    assert_eq!(f.next_line(), 3);
}

#[test]
fn unnumbered_and_immediate_commands() {
    let t = t0();
    let mut f = F::new(1, t);
    let w = f.send("  G28  ", false, "a".into(), t);
    assert_eq!((w.text.as_str(), w.number), ("G28", None));
    // Immediate commands bypass the window.
    let w = f.send_immediate("M108", true, "b".into(), t);
    assert_eq!(w.text, "M108");
    assert_eq!(f.in_flight_len(), 2);
    f.send_immediate("M112", false, "c".into(), t);
    assert_eq!(f.in_flight_len(), 2);
    assert_eq!(acked(feed(&mut f, "ok", t)), ["a"]);
    assert_eq!(acked(feed(&mut f, "ok", t)), ["b"]);
    assert_eq!(f.next_line(), 1, "unnumbered lines use no numbers");
}

#[test]
fn resend_in_ping_pong_retransmits_immediately() {
    let t = t0();
    let mut f = synced(1, t);
    send(&mut f, "G1 X1", t);
    assert!(feed(&mut f, "Error:checksum mismatch, Last Line: 0", t).is_none());
    assert_eq!(
        resend(feed(&mut f, "Resend: 1", t)),
        ResendAction::Rewind { from: 1, stale: 0 }
    );
    assert!(matches!(
        feed(&mut f, "ok", t),
        Some(FeedResult::Ok(OkOutcome::Swallowed))
    ));
    assert_eq!(f.backlog_len(), 1);
    let w = f.poll_retransmit(t).expect("retransmission");
    assert_eq!(w.text, frame_numbered(1, "G1 X1"));
    assert!(f.poll_retransmit(t).is_none());
    // The retransmission is corrupted too: a checksum error is genuine.
    feed(&mut f, "Error:checksum mismatch, Last Line: 0", t);
    assert_eq!(
        resend(feed(&mut f, "Resend: 1", t)),
        ResendAction::Rewind { from: 1, stale: 0 }
    );
    feed(&mut f, "ok", t);
    assert!(f.poll_retransmit(t).is_some());
    assert_eq!(acked(feed(&mut f, "ok", t)), ["G1 X1"]);
    assert!(f.is_idle());
}

#[test]
fn windowed_resend_waits_for_stale_lines_and_ignores_duplicates() {
    let t = t0();
    let mut f = synced(4, t);
    for c in ["A", "B", "C", "D"] {
        send(&mut f, c, t);
    }
    assert!(!f.can_send_new());
    // B (line 2) is corrupt; A was accepted.
    feed(&mut f, "Error:checksum mismatch, Last Line: 1", t);
    assert_eq!(
        resend(feed(&mut f, "Resend: 2", t)),
        ResendAction::Rewind { from: 2, stale: 2 }
    );
    assert!(matches!(
        feed(&mut f, "ok", t),
        Some(FeedResult::Ok(OkOutcome::Swallowed))
    ));
    assert_eq!(f.in_flight_len(), 1, "only A is still in flight");
    assert_eq!(f.backlog_len(), 3);
    // Nothing is retransmitted while A is outstanding.
    assert!(f.poll_retransmit(ms(t, 100)).is_none());
    assert_eq!(acked(feed(&mut f, "ok", ms(t, 1))), ["A"]);
    // C arrives late and is rejected: a duplicate.
    feed(
        &mut f,
        "Error:Line Number is not Last Line Number+1, Last Line: 1",
        ms(t, 2),
    );
    assert_eq!(
        resend(feed(&mut f, "Resend: 2", ms(t, 2))),
        ResendAction::Duplicate
    );
    assert!(matches!(
        feed(&mut f, "ok", ms(t, 2)),
        Some(FeedResult::Ok(OkOutcome::Swallowed))
    ));
    // D was flushed silently: retransmission waits for the settle time.
    assert!(f.poll_retransmit(ms(t, 3)).is_none());
    let w = f
        .poll_retransmit(ms(t, 2) + SETTLE_MIN)
        .expect("retransmit B");
    assert_eq!(w.meta, "B");
    let now = ms(t, 20);
    assert_eq!(f.poll_retransmit(now).map(|w| w.meta).as_deref(), Some("C"));
    assert_eq!(f.poll_retransmit(now).map(|w| w.meta).as_deref(), Some("D"));
    assert!(f.poll_retransmit(now).is_none());
    assert!(f.can_send_new());
    // D's late stale rejection (budget left) is still a duplicate ...
    feed(
        &mut f,
        "Error:Line Number is not Last Line Number+1, Last Line: 1",
        now,
    );
    assert_eq!(
        resend(feed(&mut f, "Resend: 2", now)),
        ResendAction::Duplicate
    );
    feed(&mut f, "ok", now);
    // ... but now the budget is spent: the next one is genuine.
    feed(
        &mut f,
        "Error:Line Number is not Last Line Number+1, Last Line: 1",
        now,
    );
    assert_eq!(
        resend(feed(&mut f, "Resend: 2", now)),
        ResendAction::Rewind { from: 2, stale: 2 }
    );
}

#[test]
fn duplicates_before_retransmission_are_always_ignored() {
    let t = t0();
    let mut f = synced(4, t);
    for c in ["A", "B", "C", "D"] {
        send(&mut f, c, t);
    }
    feed(&mut f, "Error:checksum mismatch, Last Line: 0", t);
    assert_eq!(
        resend(feed(&mut f, "Resend: 1", t)),
        ResendAction::Rewind { from: 1, stale: 3 }
    );
    feed(&mut f, "ok", t);
    for _ in 0..3 {
        feed(
            &mut f,
            "Error:Line Number is not Last Line Number+1, Last Line: 0",
            t,
        );
        assert_eq!(
            resend(feed(&mut f, "Resend: 1", t)),
            ResendAction::Duplicate
        );
        feed(&mut f, "ok", t);
    }
    // Every stale line answered: no settle wait needed.
    let w = f.poll_retransmit(t).expect("retransmission");
    assert_eq!(w.meta, "A");
}

#[test]
fn ok_with_line_numbers_is_verified() {
    let t = t0();
    let mut f = synced(4, t);
    for c in ["A", "B", "C"] {
        send(&mut f, c, t);
    }
    // The oks of A and B were lost.
    assert_eq!(acked(feed(&mut f, "ok N3 P15 B3", t)), ["A", "B", "C"]);
    assert!(matches!(
        feed(&mut f, "ok N2", t),
        Some(FeedResult::Ok(OkOutcome::Spurious))
    ));
    send(&mut f, "D", t);
    assert!(matches!(
        feed(&mut f, "ok N99", t),
        Some(FeedResult::Ok(OkOutcome::Spurious))
    ));
    assert_eq!(acked(feed(&mut f, "ok N4", t)), ["D"]);
}

#[test]
fn barrier_blocks_and_repeats() {
    let t = t0();
    let mut f = F::new(4, t);
    let first = f.send_line_reset(0, true, "M110".into(), t);
    assert!(!f.can_send_new());
    assert!(f.poll_retransmit(t).is_none());
    // A resend request while the barrier is out repeats it.
    assert_eq!(resend(feed(&mut f, "Resend: 57", t)), ResendAction::Barrier);
    let again = f.poll_retransmit(ms(t, 1)).expect("barrier again");
    assert_eq!(again.text, first.text);
    assert_eq!(f.in_flight_len(), 1);
    feed(&mut f, "ok", t); // the ok after the Resend
    assert!(f.barrier_active());
    // Handshake retries.
    assert_eq!(f.repeat_barrier(ms(t, 2)).map(|w| w.text), Some(first.text));
    assert_eq!(acked(feed(&mut f, "ok", ms(t, 3))), ["M110"]);
    assert!(f.is_idle() && f.can_send_new());
    assert_eq!(send(&mut f, "G28", t).number, Some(1));
    // Unnumbered barrier.
    let mut g = F::new(1, t);
    assert_eq!(g.send_line_reset(41, false, "x".into(), t).text, "M110 N41");
    acked(feed(&mut g, "ok", t));
    assert_eq!(g.next_line(), 42);
}

#[test]
fn odd_resend_requests() {
    let t = t0();
    let mut f = synced(2, t);
    send(&mut f, "A", t);
    // Firmware ahead of us.
    assert_eq!(
        resend(feed(&mut f, "Resend: 9", t)),
        ResendAction::Desync { expected: 9 }
    );
    feed(&mut f, "ok", t);
    // Everything we sent was accepted.
    assert_eq!(
        resend(feed(&mut f, "Resend: 2", t)),
        ResendAction::NothingToResend
    );
    feed(&mut f, "ok", t);
    assert_eq!(f.in_flight_len(), 1);
    assert_eq!(acked(feed(&mut f, "ok", t)), ["A"]);
    // A line older than the ring.
    for i in 0..(RING_CAPACITY + 50) {
        send(&mut f, &format!("G1 X{i}"), t);
        acked(feed(&mut f, "ok", t));
    }
    assert!(f.ring.len() <= RING_CAPACITY);
    assert_eq!(
        resend(feed(&mut f, "Resend: 1", t)),
        ResendAction::Unavailable { line: 1 }
    );
    feed(&mut f, "ok", t);
    // A recent one is still there.
    let recent = f.next_line() - 3;
    assert_eq!(
        resend(feed(&mut f, &format!("Resend: {recent}"), t)),
        ResendAction::Rewind {
            from: recent,
            stale: 0
        }
    );
}

#[test]
fn a_rewind_past_lines_the_firmware_accepted_moves_forward() {
    let t = t0();
    let mut f = synced(4, t);
    for c in ["A", "B", "C", "D"] {
        send(&mut f, c, t);
    }
    feed(&mut f, "Error:checksum mismatch, Last Line: 0", t);
    resend(feed(&mut f, "Resend: 1", t));
    feed(&mut f, "ok", t);
    // Firmware later says it has accepted 1..=2 (the originals got through
    // after all): lines 1 and 2 will be acknowledged, 3 is next.
    feed(
        &mut f,
        "Error:Line Number is not Last Line Number+1, Last Line: 2",
        t,
    );
    assert_eq!(
        resend(feed(&mut f, "Resend: 3", t)),
        ResendAction::Rewind { from: 3, stale: 0 }
    );
    feed(&mut f, "ok", t);
    assert_eq!(f.in_flight_len(), 2);
    assert_eq!(
        f.poll_retransmit(ms(t, 50)).map(|w| w.meta).as_deref(),
        Some("C")
    );
    assert_eq!(acked(feed(&mut f, "ok", t)), ["A"]);
}

#[test]
fn timeouts_release_everything() {
    let t = t0();
    let mut f = synced(4, t);
    let limit = |_: &String| Duration::from_millis(100);
    assert!(!f.timed_out(ms(t, 1000), limit), "nothing in flight");
    send(&mut f, "A", t);
    send(&mut f, "B", t);
    assert!(!f.timed_out(ms(t, 100), limit));
    f.note_activity(ms(t, 90));
    assert!(!f.timed_out(ms(t, 150), limit), "busy extends the timeout");
    assert!(f.timed_out(ms(t, 191), limit));
    // A long-running command extends it for everyone.
    let long = |m: &String| Duration::from_millis(if m == "B" { 1000 } else { 100 });
    assert!(!f.timed_out(ms(t, 191), long));
    assert_eq!(f.release_all(ms(t, 200)), ["A", "B"]);
    assert!(f.is_idle());
    assert!(!f.timed_out(ms(t, 10_000), limit));
}

#[test]
fn reset_forgets_everything() {
    let t = t0();
    let mut f = synced(4, t);
    send(&mut f, "A", t);
    resend(feed(&mut f, "Resend: 1", t));
    f.reset(t);
    assert!(f.is_idle());
    assert!(matches!(
        feed(&mut f, "ok", t),
        Some(FeedResult::Ok(OkOutcome::Spurious))
    ));
}

// ---------------------------------------------------------------------
// Randomized runs against a model firmware.
// ---------------------------------------------------------------------

/// xorshift64*: deterministic and dependency-free.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
    fn chance(&mut self, p: f64) -> bool {
        let unit = (self.next() >> 11) as f64 / ((1u64 << 53) as f64);
        unit < p
    }
}

#[derive(Clone, Copy)]
struct Scenario {
    window: usize,
    corrupt: f64,
    drop_ok: f64,
    /// Marlin flushes its receive buffer on errors (AVR boards); 32-bit
    /// boards answer every out-of-order line instead.
    flush: bool,
    advanced_ok: bool,
    bufsize: usize,
    seed: u64,
    commands: usize,
}

struct Firmware {
    rng: Rng,
    s: Scenario,
    last: i64,
    rx: VecDeque<String>,
    queue: VecDeque<(String, Option<u32>)>,
    executed: Vec<String>,
    out: VecDeque<String>,
}

impl Firmware {
    fn reject(&mut self, why: &str) {
        self.out
            .push_back(format!("Error:{why}, Last Line: {}", self.last));
        self.out.push_back(format!("Resend: {}", self.last + 1));
        self.out.push_back("ok".to_string());
        if self.s.flush {
            self.rx.clear();
        }
    }

    fn accept(&mut self, raw: &str) {
        if !raw.starts_with('N') {
            self.queue.push_back((raw.to_string(), None));
            return;
        }
        let f = parse_framed(raw).expect("host sends valid frames");
        let n = i64::from(f.line_number);
        if let Some(v) = f.command.strip_prefix("M110 N") {
            self.last = v.parse().unwrap();
            self.queue
                .push_back((f.command.to_string(), Some(f.line_number)));
        } else if n != self.last + 1 {
            self.reject("Line Number is not Last Line Number+1");
        } else if self.rng.chance(self.s.corrupt) {
            self.reject("checksum mismatch");
        } else {
            self.last = n;
            self.queue
                .push_back((f.command.to_string(), Some(f.line_number)));
        }
    }

    /// Read up to `max` received lines while the command queue has room.
    fn read(&mut self, max: usize) {
        for _ in 0..max {
            if self.queue.len() >= self.s.bufsize {
                return;
            }
            let Some(line) = self.rx.pop_front() else {
                return;
            };
            self.accept(&line);
        }
    }

    fn execute(&mut self, max: usize) {
        for _ in 0..max {
            let Some((cmd, n)) = self.queue.pop_front() else {
                return;
            };
            self.executed.push(cmd);
            if !self.rng.chance(self.s.drop_ok) {
                self.out.push_back(match (self.s.advanced_ok, n) {
                    (true, Some(n)) => format!("ok N{n} P15 B3"),
                    _ => "ok".to_string(),
                });
            }
        }
    }
}

struct Report {
    timeouts: usize,
    rewinds: usize,
    duplicates: usize,
    transmissions: usize,
    steps: usize,
}

fn run(s: Scenario) -> Report {
    let base = Instant::now();
    let mut now = base;
    let mut rng = Rng(s.seed ^ 0x9E37_79B9_7F4A_7C15);
    let mut fw = Firmware {
        rng: Rng(s.seed.wrapping_mul(31).wrapping_add(7)),
        s,
        last: 0,
        rx: VecDeque::new(),
        queue: VecDeque::new(),
        executed: Vec::new(),
        out: VecDeque::new(),
    };
    let commands: Vec<String> = (0..s.commands).map(|i| format!("G1 X{i}")).collect();
    let mut flow = F::new(s.window, now);
    let mut to_fw: VecDeque<String> = VecDeque::new();
    let mut to_host: VecDeque<String> = VecDeque::new();
    let mut tx_count: HashMap<u32, usize> = HashMap::new();
    let mut covered: HashMap<u32, usize> = HashMap::new();
    let mut report = Report {
        timeouts: 0,
        rewinds: 0,
        duplicates: 0,
        transmissions: 0,
        steps: 0,
    };
    let mut next = 0;
    let mut probe = false;
    let timeout = Duration::from_millis(500);

    let w = flow.send_line_reset(0, true, "M110".into(), now);
    to_fw.push_back(w.text);
    loop {
        report.steps += 1;
        assert!(
            report.steps < 2_000_000,
            "no progress: executed {}",
            fw.executed.len()
        );
        now += Duration::from_micros(500);

        // Wire to firmware in random batches; the firmware reads and
        // executes at random paces.
        for _ in 0..rng.below(to_fw.len() + 1) {
            if let Some(l) = to_fw.pop_front() {
                fw.rx.push_back(l);
            }
        }
        let max_read = rng.below(6);
        fw.read(max_read);
        let max_exec = rng.below(3);
        fw.execute(max_exec);
        for _ in 0..rng.below(fw.out.len() + 1) {
            if let Some(l) = fw.out.pop_front() {
                to_host.push_back(l);
            }
        }

        // Host: replies.
        while let Some(line) = to_host.pop_front() {
            match feed(&mut flow, &line, now) {
                Some(FeedResult::Resend(ResendAction::Rewind { from, .. })) => {
                    report.rewinds += 1;
                    for k in from..flow.next_line() {
                        *covered.entry(k).or_default() += 1;
                    }
                }
                Some(FeedResult::Resend(ResendAction::Duplicate)) => report.duplicates += 1,
                Some(FeedResult::Resend(
                    a @ (ResendAction::Desync { .. } | ResendAction::Unavailable { .. }),
                )) => panic!("unexpected {a:?}"),
                _ => {}
            }
        }
        if flow.timed_out(now, |_| timeout) {
            report.timeouts += 1;
            flow.release_all(now);
            probe = true;
        }

        // Host: send.
        loop {
            if let Some(w) = flow.poll_retransmit(now) {
                if let Some(n) = w.number {
                    *tx_count.entry(n).or_default() += 1;
                }
                report.transmissions += 1;
                to_fw.push_back(w.text);
                continue;
            }
            if !flow.can_send_new() {
                break;
            }
            let cmd = if probe {
                probe = false;
                "M105".to_string()
            } else if next < commands.len() {
                next += 1;
                commands[next - 1].clone()
            } else {
                break;
            };
            let w = flow.send(&cmd, true, cmd.clone(), now);
            *tx_count.entry(w.number.unwrap()).or_default() += 1;
            report.transmissions += 1;
            to_fw.push_back(w.text);
        }

        let quiet = to_fw.is_empty()
            && to_host.is_empty()
            && fw.rx.is_empty()
            && fw.queue.is_empty()
            && fw.out.is_empty();
        if next == commands.len() && flow.is_idle() && quiet && !probe {
            break;
        }
    }

    let executed: Vec<&String> = fw
        .executed
        .iter()
        .filter(|c| !c.starts_with("M110") && *c != "M105")
        .collect();
    assert_eq!(
        executed.len(),
        commands.len(),
        "commands lost or duplicated"
    );
    for (got, want) in executed.iter().zip(&commands) {
        assert_eq!(*got, want);
    }
    // A line goes out again only when a genuine request covered it.
    for (n, count) in &tx_count {
        let allowed = 1 + covered.get(n).copied().unwrap_or(0);
        assert!(
            *count <= allowed,
            "line {n} sent {count} times, {allowed} allowed"
        );
    }
    if std::env::var_os("SPRINT_FLOW_STATS").is_some() {
        eprintln!(
            "window {} flush {} corrupt {} drop {} seed {}: {} tx, {} rewinds, {} dups, {} timeouts",
            s.window,
            s.flush,
            s.corrupt,
            s.drop_ok,
            s.seed,
            report.transmissions,
            report.rewinds,
            report.duplicates,
            report.timeouts
        );
    }
    report
}

fn scenarios(window: usize, flush: bool) -> impl Iterator<Item = Scenario> {
    (0..12u64).map(move |seed| Scenario {
        window,
        corrupt: 0.1,
        drop_ok: 0.0,
        flush,
        advanced_ok: false,
        bufsize: 4,
        seed: seed + 1,
        commands: 1500,
    })
}

#[test]
fn ping_pong_with_corruption_never_times_out() {
    for s in scenarios(1, true) {
        let r = run(s);
        assert_eq!(r.timeouts, 0, "seed {}", s.seed);
        assert_eq!(r.duplicates, 0, "seed {}", s.seed);
        assert!(r.rewinds > 50, "seed {}: {} rewinds", s.seed, r.rewinds);
    }
}

#[test]
fn windowed_streaming_with_corruption_and_buffer_flushes() {
    let mut timeouts = 0;
    let mut duplicates = 0;
    for window in [2, 4, 8] {
        for s in scenarios(window, true) {
            let r = run(s);
            timeouts += r.timeouts;
            duplicates += r.duplicates;
            assert!(
                r.transmissions < s.commands * 3,
                "window {window} seed {}: {} transmissions",
                s.seed,
                r.transmissions
            );
        }
    }
    assert!(duplicates > 0, "the scenario exercises duplicate requests");
    // Timeouts are a last resort, not the normal recovery path: settling
    // after stale lines keeps retransmissions from being flushed.
    assert_eq!(timeouts, 0);
}

#[test]
fn windowed_streaming_without_buffer_flushes() {
    for window in [2, 4] {
        for s in scenarios(window, false) {
            let r = run(s);
            assert!(
                r.timeouts <= 1,
                "window {window} seed {}: {}",
                s.seed,
                r.timeouts
            );
        }
    }
}

#[test]
fn lost_oks_are_recovered_by_timeouts() {
    for window in [1, 4] {
        for seed in 1..6 {
            let r = run(Scenario {
                window,
                corrupt: 0.05,
                drop_ok: 0.02,
                flush: true,
                advanced_ok: false,
                bufsize: 4,
                seed,
                commands: 600,
            });
            assert!(r.timeouts > 0, "window {window} seed {seed}");
        }
    }
}

#[test]
fn advanced_ok_heals_lost_oks_without_timeouts() {
    for seed in 1..6 {
        let r = run(Scenario {
            window: 4,
            corrupt: 0.0,
            drop_ok: 0.05,
            flush: true,
            advanced_ok: true,
            bufsize: 4,
            seed,
            commands: 800,
        });
        // Only the M110 ok and the very last oks can be lost without a
        // later ok N to heal them.
        assert!(r.timeouts <= 2, "seed {seed}: {}", r.timeouts);
    }
}

#[test]
fn clean_links_need_no_recovery() {
    for window in [1, 3, 16] {
        let r = run(Scenario {
            window,
            corrupt: 0.0,
            drop_ok: 0.0,
            flush: true,
            advanced_ok: false,
            bufsize: 4,
            seed: 99,
            commands: 2000,
        });
        assert_eq!((r.timeouts, r.rewinds, r.duplicates), (0, 0, 0));
        assert_eq!(r.transmissions, 2000);
    }
}
