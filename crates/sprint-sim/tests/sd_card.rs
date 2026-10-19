//! SD card listing, upload with line numbers, printing and statistics.

mod common;

use common::{Host, PATIENCE, fast, frame};
use sprint_sim::SimConfig;

fn sd_config() -> SimConfig {
    SimConfig {
        sd_files: vec![
            ("calibration_cube.gcode".into(), 48_213),
            ("Benchy 0.2mm.gcode".into(), 10_000),
        ],
        instant_moves: true,
        ..fast(50.0)
    }
}

#[test]
fn listing_short_and_long_names() {
    let mut h = Host::booted(sd_config());
    assert_eq!(
        h.command("M20"),
        vec![
            "Begin file list",
            "CALIBR~1.GCO 48213",
            "BENCHY~1.GCO 10000",
            "End file list",
            "ok"
        ]
    );
    assert_eq!(
        h.command("M20 L"),
        vec![
            "Begin file list",
            "CALIBR~1.GCO 48213 calibration_cube.gcode",
            "BENCHY~1.GCO 10000 Benchy 0.2mm.gcode",
            "End file list",
            "ok"
        ]
    );
    assert_eq!(h.command("M21"), vec!["echo:SD card ok", "ok"]);
    assert_eq!(
        h.command("M33 benchy~1.gco"),
        vec!["Benchy 0.2mm.gcode", "ok"]
    );
    assert_eq!(h.command("M22"), vec!["echo:SD card released", "ok"]);
    assert_eq!(h.command("M20"), vec!["echo:No media", "ok"]);
    h.command("M21");
}

#[test]
fn upload_list_print_delete() {
    let mut h = Host::booted(sd_config());
    h.command("M110 N0");
    h.control().clear_received();
    assert_eq!(
        h.numbered("M28 upload.gco"),
        vec![
            "echo:Now fresh file: upload.gco",
            "Writing to file: upload.gco",
            "ok"
        ]
    );
    let body: Vec<String> = ["M302 P1", "G92 X0 Y0 Z0 E0"]
        .iter()
        .map(|s| s.to_string())
        .chain((1..=50).map(|i| format!("G1 X{} Y{} E{} F3000", i, i * 2, i)))
        .collect();
    h.stream(&body, std::time::Duration::from_secs(2));
    assert_eq!(h.numbered("M29"), vec!["Done saving file.", "ok"]);
    // The upload is recorded exactly once, in order.
    let received = h.control().received_commands();
    assert_eq!(received.first().map(String::as_str), Some("M28 upload.gco"));
    assert_eq!(&received[1..received.len() - 1], &body[..]);
    // Marlin stores each line with CR LF.
    let size: usize = body.iter().map(|l| l.len() + 2).sum();
    let listing = h.command("M20 L");
    assert!(
        listing.contains(&format!("UPLOAD.GCO {size} upload.gco")),
        "{listing:?}"
    );

    h.control().clear_received();
    assert_eq!(
        h.command("M23 /UPLOAD.GCO"),
        vec![
            "echo:Now fresh file: /UPLOAD.GCO".to_string(),
            format!("File opened: UPLOAD.GCO Size: {size}"),
            "File selected".to_string(),
            "ok".to_string()
        ]
    );
    assert_eq!(
        h.command("M27"),
        vec![format!("SD printing byte 0/{size}"), "ok".to_string()]
    );
    h.command("M24");
    h.until(PATIENCE, |l| l == "Done printing file");
    assert_eq!(h.command("M27"), vec!["Not SD printing", "ok"]);
    assert!(h.command("M114")[0].starts_with("X:50.00 Y:100.00 Z:0.00 E:50.00"));
    // Commands read from the card are not host commands.
    assert!(
        !h.control()
            .received_commands()
            .iter()
            .any(|c| c.starts_with("G1 X"))
    );
    let stats = h.command("M78");
    assert!(
        stats[0].starts_with("Stats: Prints: 1, Finished: 1"),
        "{stats:?}"
    );
    assert_eq!(
        h.command("M30 upload.gco"),
        vec!["File deleted:upload.gco", "ok"]
    );
    assert_eq!(
        h.command("M30 upload.gco"),
        vec!["Deletion failed, File: upload.gco.", "ok"]
    );
}

#[test]
fn upload_survives_injected_checksum_errors() {
    let mut h = Host::booted(SimConfig {
        checksum_error_rate: 0.2,
        seed: 5,
        ..sd_config()
    });
    h.command("M110 N0");
    let mut lines = vec!["M28 noisy.gco".to_string()];
    lines.extend((0..80).map(|i| format!("G1 X{i} Y{i} F1200")));
    lines.push("M29".to_string());
    let (resends, _) = h.stream(&lines, std::time::Duration::from_secs(2));
    assert!(resends > 5);
    let expected: usize = lines[1..81].iter().map(|l| l.len() + 2).sum();
    let listing = h.command("M20 L");
    assert!(
        listing.contains(&format!("NOISY.GCO {expected} noisy.gco")),
        "{listing:?}"
    );
}

#[test]
fn unnumbered_lines_are_rejected_while_writing() {
    let mut h = Host::booted(sd_config());
    h.command("M110 N0");
    h.numbered("M28 x.gco");
    h.write("G28");
    let out = h.until(PATIENCE, |l| l == "ok");
    assert_eq!(
        out,
        vec![
            "Error:No Checksum with line number, Last Line: 1",
            "Resend: 2",
            "ok"
        ]
    );
    // M29 is accepted without a line number (Pronterface does that).
    assert_eq!(h.command("M29"), vec!["Done saving file.", "ok"]);
    h.write(&frame(2, "M105"));
    assert!(h.until(PATIENCE, |l| l.starts_with("ok"))[0].starts_with("ok T:"));
}

#[test]
fn print_seeded_file_with_heating() {
    let mut h = Host::booted(SimConfig {
        sd_files: vec![("small.gcode".into(), 3_000)],
        instant_moves: true,
        ..fast(400.0)
    });
    h.command("M23 small.gcode");
    h.command("M24");
    assert!(h.control().snapshot().sd_printing);
    let out = h.until(std::time::Duration::from_secs(20), |l| {
        l == "Done printing file"
    });
    // The synthetic file heats the bed and the hotend first.
    assert!(
        out.iter()
            .any(|l| l.starts_with(" T:") && l.contains(" W:")),
        "{out:?}"
    );
    assert!(out.iter().any(|l| l.starts_with("echo:Print time: ")));
    assert!(!h.control().snapshot().sd_printing);
}

#[test]
fn a_print_started_on_the_printer_answers_without_oks() {
    let mut h = Host::booted(sd_config());
    h.control().clear_received();
    // Chosen on the printer's menu: messages, but no ok for the host.
    h.control()
        .run_command("M23 benchy~1.gco ; from the menu\n\nM24");
    let out = h.until(PATIENCE, |l| l == "//action:resume");
    assert!(
        out.contains(&"File opened: benchy~1.gco Size: 10000".to_string()),
        "{out:?}"
    );
    assert!(!out.iter().any(|l| l.starts_with("ok")), "{out:?}");
    // (Answered once the file's heat-up lets the queue move.)
    let status = h.command("M27");
    assert!(
        status
            .iter()
            .any(|l| l.starts_with("SD printing byte ") && l.ends_with("/10000")),
        "{status:?}"
    );
    h.until(PATIENCE, |l| l == "Done printing file");
    assert_eq!(h.command("M27"), ["Not SD printing", "ok"]);
    // Only the host's own commands are recorded.
    assert_eq!(h.control().received_commands(), ["M27", "M27"]);
}
