//! Excluding objects while printing: nothing of an excluded object reaches
//! the printer, the rest of the print is unchanged, and the printer is
//! brought back to the file's position after every skipped part.

mod common;

use std::sync::Arc;

use common::*;
use sprint_host::sprint_gcode::{GcodeFile, state_at_line};
use sprint_host::{JobStatus, PrintOptions, Printer};

/// Moves of one object on one layer, with travel, extrusion, a retract and
/// a prime (absolute or relative extrusion).
fn object_moves(x0: f64, layer: usize, e: &mut f64, relative: bool) -> Vec<String> {
    let mut extrude = |amount: f64| {
        *e += amount;
        if relative {
            format!("{amount:.3}")
        } else {
            format!("{:.3}", *e)
        }
    };
    let y = 10.0 + layer as f64;
    vec![
        format!("G1 X{x0} Y{y} F6000"),
        format!("G1 E{} F2400", extrude(0.8)),
        format!("G1 X{} Y{y} E{} F1500", x0 + 10.0, extrude(0.5)),
        format!("G1 X{} Y{} E{}", x0 + 10.0, y + 10.0, extrude(0.5)),
        format!("G1 X{x0} Y{} E{}", y + 10.0, extrude(0.5)),
        format!("G1 E{} F2400", extrude(-0.8)),
    ]
}

/// Three objects over three layers, labelled with `start`/`end` markers.
fn plate(
    relative: bool,
    start: impl Fn(usize, &str) -> String,
    end: impl Fn(usize, &str) -> String,
    header: &str,
) -> Arc<GcodeFile> {
    let names = ["cube", "cylinder", "cone"];
    let mut text = format!(
        "G28\nM302 P1\nG90\n{}\n{header}",
        if relative { "M83" } else { "M82" }
    );
    let mut e = 0.0;
    for layer in 0..3 {
        text.push_str(&format!("G1 Z{:.1} F600\n", 0.3 + 0.2 * layer as f64));
        for (id, name) in names.iter().enumerate() {
            text.push_str(&start(id, name));
            text.push('\n');
            for line in object_moves(20.0 + 40.0 * id as f64, layer, &mut e, relative) {
                text.push_str(&line);
                text.push('\n');
            }
            text.push_str(&end(id, name));
            text.push('\n');
        }
    }
    text.push_str("G1 X5 Y5 F6000\n");
    Arc::new(GcodeFile::from_text("plate.gcode", text))
}

fn m486_plate(relative: bool) -> Arc<GcodeFile> {
    plate(
        relative,
        |id, name| format!("M486 S{id} A\"{name}\""),
        |_, _| "M486 S-1".to_string(),
        "M486 T3\n",
    )
}

/// The file's commands, without the moves inside `skip`'s blocks.
fn expected(file: &GcodeFile, skip: &str, start: &str) -> Vec<String> {
    let mut inside = false;
    let mut out = Vec::new();
    for i in 0..file.len() {
        let raw = file.line(i);
        if raw.starts_with(start) {
            inside = raw.contains(skip);
        } else if raw.starts_with("M486 S-1") || raw.starts_with(";END") {
            inside = false;
        }
        let Some(cmd) = file.command(i) else {
            continue;
        };
        if inside && cmd.starts_with("G1") {
            continue;
        }
        out.push(cmd.into_owned());
    }
    out
}

/// Start `file` behind a 2 s dwell, so objects can be excluded before
/// any of them is streamed.
fn start(printer: &Printer, file: &Arc<GcodeFile>) {
    let control = printer.sim_control().unwrap();
    control.clear_received();
    printer.send("G4 S2");
    printer
        .start_print(Arc::clone(file), PrintOptions::default())
        .unwrap();
}

fn printed(printer: &Printer) -> Vec<String> {
    wait_for(printer, "completed", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Completed)
    });
    wait_idle(printer);
    let got = printer.sim_control().unwrap().received_commands();
    got.into_iter().filter(|c| c != "G4 S2").collect()
}

fn check_excluded_print(relative: bool) {
    let printer = connect(fast_sim(1.0), options());
    let file = m486_plate(relative);
    start(&printer, &file);
    let objects = printer.with_state(|s| s.job.as_ref().unwrap().objects.clone());
    assert_eq!(objects, ["cube", "cylinder", "cone"]);
    printer.exclude_object("cylinder");
    wait_for(&printer, "the exclusion", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.excluded_objects == ["cylinder"])
    });
    let got = printed(&printer);
    // Nothing of the cylinder (X60..X70) reached the printer.
    assert!(
        !got.iter()
            .any(|c| c.starts_with("G1 X60") || c.starts_with("G1 X70")),
        "{got:?}"
    );
    // Everything else arrived unchanged and in order, with the travels
    // back to the file's position after each skipped part.
    let resync: Vec<&String> = got.iter().filter(|c| c.starts_with("G0 ")).collect();
    assert_eq!(resync.len(), 3, "{got:?}");
    let without_resync: Vec<String> = got
        .iter()
        .filter(|c| {
            !c.starts_with("G0 ")
                && !(c.starts_with("G1 F") && c.split_whitespace().count() == 2)
                && !c.starts_with("G92 E")
        })
        .cloned()
        .collect();
    assert_eq!(without_resync, expected(&file, "cylinder", "M486 S"));
    // The extruder ends where the file says (G92 E kept absolute E in step).
    let end = state_at_line(&file, file.len());
    let snap = printer.sim_control().unwrap().snapshot();
    for axis in 0..2 {
        assert!(
            (f64::from(snap.position[axis]) - end.position[axis]).abs() < 0.01,
            "{:?} vs {:?}",
            snap.position,
            end.position
        );
    }
    if !relative {
        let g92: Vec<&String> = got.iter().filter(|c| c.starts_with("G92 E")).collect();
        assert_eq!(g92.len(), 3, "{got:?}");
        assert!(
            (f64::from(snap.position[3]) - end.position[3]).abs() < 0.01,
            "E {} vs {}",
            snap.position[3],
            end.position[3]
        );
    }
}

#[test]
fn an_excluded_object_is_not_printed_absolute_extrusion() {
    check_excluded_print(false);
}

#[test]
fn an_excluded_object_is_not_printed_relative_extrusion() {
    check_excluded_print(true);
}

#[test]
fn every_label_style_names_and_excludes_objects() {
    let styles: [(&str, Arc<GcodeFile>, &str); 3] = [
        (
            "klipper",
            plate(
                true,
                |_, name| format!("EXCLUDE_OBJECT_START NAME={name}"),
                |_, name| format!("EXCLUDE_OBJECT_END NAME={name}"),
                "EXCLUDE_OBJECT_DEFINE NAME=cube\nEXCLUDE_OBJECT_DEFINE NAME=cylinder\nEXCLUDE_OBJECT_DEFINE NAME=cone\n",
            ),
            "EXCLUDE_OBJECT_START",
        ),
        (
            "comments",
            plate(
                true,
                |_, name| format!("; printing object {name}"),
                |_, name| format!("; stop printing object {name}"),
                "",
            ),
            "; printing object",
        ),
        (
            "cura",
            plate(
                true,
                |_, name| format!(";MESH:{name}"),
                |_, _| ";MESH:NONMESH".to_string(),
                "",
            ),
            ";MESH:",
        ),
    ];
    for (style, file, _) in styles {
        let printer = connect(fast_sim(1.0), options());
        start(&printer, &file);
        let objects = printer.with_state(|s| s.job.as_ref().unwrap().objects.clone());
        assert_eq!(objects, ["cube", "cylinder", "cone"], "{style}");
        printer.exclude_object("cone");
        let got = printed(&printer);
        assert!(
            !got.iter()
                .any(|c| c.starts_with("G1 X100") || c.starts_with("G1 X110")),
            "{style}: {got:?}"
        );
        assert!(got.iter().any(|c| c.starts_with("G1 X60")), "{style}");
    }
}

#[test]
fn an_object_can_be_printed_again() {
    let printer = connect(fast_sim(1.0), options());
    let file = m486_plate(true);
    start(&printer, &file);
    printer.exclude_object("cube");
    printer.include_object("cube");
    printer.exclude_object("no such object");
    let got = printed(&printer);
    assert_eq!(got, expected(&file, "nothing", "M486 S"));
    let job = printer.with_state(|s| s.job.clone()).unwrap();
    assert!(job.excluded_objects.is_empty());
    assert!(
        console(&printer)
            .iter()
            .any(|e| e.text.contains("no object named \"no such object\""))
    );
}

#[test]
fn the_current_object_follows_the_stream() {
    // Moves take time, so the job record shows each object for a while.
    let sim = sprint_host::sprint_sim::SimConfig {
        instant_moves: false,
        ..fast_sim(10.0)
    };
    let printer = connect(sim, options());
    let file = m486_plate(true);
    start(&printer, &file);
    let mut seen = Vec::new();
    while printer.with_state(|s| s.job_active()) {
        if let Some(c) =
            printer.with_state(|s| s.job.as_ref().and_then(|j| j.current_object.clone()))
            && seen.last() != Some(&c)
        {
            seen.push(c);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(seen.contains(&"cube".to_string()), "{seen:?}");
    let job = printer.with_state(|s| s.job.clone()).unwrap();
    assert_eq!(job.current_object, None, "the file ends outside any object");
}
