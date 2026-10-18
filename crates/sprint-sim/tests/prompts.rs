//! Host action commands and prompts: M600, filament runout, M876, M118.

mod common;

use common::{Host, PATIENCE, fast};
use std::time::Duration;

#[test]
fn m600_prompts_and_waits_for_the_user() {
    let mut h = Host::booted(fast(50.0));
    h.command("G28");
    h.write("M600");
    let out = h.until(PATIENCE, |l| l == "//action:prompt_show");
    assert_eq!(out[0], "//action:paused");
    let n = out.len();
    assert_eq!(
        &out[n - 4..],
        &[
            "//action:prompt_end",
            "//action:prompt_begin Nozzle Parked",
            "//action:prompt_button Continue",
            "//action:prompt_show"
        ]
    );
    let waiting = h.until(PATIENCE, |l| l == "echo:busy: paused for user");
    assert!(!waiting.iter().any(|l| l == "ok"));
    h.write("M876 S0");
    let out = h.until(PATIENCE, |l| l == "//action:resumed");
    assert!(out.contains(&"//action:prompt_end".to_string()));
    // ok for M600, then for M876.
    let rest = h.until(PATIENCE, |l| l == "ok");
    assert_eq!(rest.last().map(String::as_str), Some("ok"));
    h.until(PATIENCE, |l| l == "ok");
    assert_eq!(
        h.command("M114")[0],
        "X:0.00 Y:0.00 Z:0.00 E:0.00 Count X:0 Y:0 Z:0"
    );
}

#[test]
fn filament_runout_from_control() {
    let mut h = Host::booted(fast(50.0));
    h.control().trigger_filament_runout();
    let out = h.until(PATIENCE, |l| l == "//action:prompt_show");
    assert_eq!(out[0], "//action:out_of_filament T0");
    assert_eq!(out[1], "//action:paused");
    h.until(PATIENCE, |l| l == "echo:busy: paused for user");
    h.write("M108");
    h.until(PATIENCE, |l| l == "//action:resumed");
    // The injected M600 does not produce an ok; M108 does.
    let rest = h.until(PATIENCE, |l| l == "ok");
    assert!(!rest.iter().any(|l| l.starts_with("//action:")));
    let endstops = h.command("M119");
    assert!(endstops.contains(&"filament: open".to_string()));
    assert!(h.drain(Duration::from_millis(100)).is_empty());
}

#[test]
fn serial_print_flags() {
    let mut h = Host::booted(fast(50.0));
    assert_eq!(h.command("M118 Hello"), vec!["Hello", "ok"]);
    assert_eq!(h.command("M118 E1 Hello"), vec!["echo:Hello", "ok"]);
    assert_eq!(
        h.command("M118 A1 action:notification Hi"),
        vec!["//action:notification Hi", "ok"]
    );
    assert_eq!(h.command("M118 E1 A1 x"), vec!["echo://x", "ok"]);
    assert_eq!(h.command("M117 Hello LCD"), vec!["ok"]);
}
