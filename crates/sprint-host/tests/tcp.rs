//! The TCP transport against the simulator's TCP server on an ephemeral
//! loopback port.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use sprint_host::sprint_gcode::GcodeFile;
use sprint_host::sprint_sim::SimServer;
use sprint_host::{
    ConnectOptions, ConnectTarget, ConnectionStatus, JobStatus, PrintOptions, Printer,
};

fn server() -> SimServer {
    let mut sim = fast_sim(100.0);
    sim.checksum_error_rate = 0.05;
    SimServer::bind("127.0.0.1:0", sim).expect("bind")
}

fn target(server: &SimServer) -> ConnectTarget {
    let text = format!("tcp://127.0.0.1:{}", server.local_addr().port());
    ConnectTarget::parse(&text).unwrap()
}

#[test]
fn prints_over_tcp() {
    let server = server();
    let printer = Printer::new();
    printer
        .connect(
            target(&server),
            ConnectOptions {
                max_in_flight: 2,
                ..options()
            },
        )
        .unwrap();
    wait_for(&printer, "connected over TCP", |s| {
        s.is_connected() && s.settings_loaded
    });
    assert!(printer.sim_control().is_none());
    let label = printer.with_state(|s| s.target.clone()).unwrap();
    assert_eq!(
        label,
        format!("tcp://127.0.0.1:{}", server.local_addr().port())
    );

    let mut text = String::from("G28\nG90\n");
    for i in 0..500 {
        text.push_str(&format!("G1 X{} Y{} F6000\n", i % 200, i % 150));
    }
    printer
        .start_print(
            Arc::new(GcodeFile::from_text("tcp.gcode", text)),
            PrintOptions::default(),
        )
        .unwrap();
    wait_for(&printer, "the print", |s| !s.job_active());
    let s = printer.snapshot();
    assert_eq!(s.job.map(|j| j.status), Some(JobStatus::Completed));
    assert!(s.link.resends > 0 && s.link.bytes_received > 0 && s.link.bytes_sent > 0);

    let start = Instant::now();
    printer.disconnect();
    assert!(start.elapsed() < Duration::from_secs(2));
    // The server takes the next client with a fresh printer.
    printer.connect(target(&server), options()).unwrap();
    wait_for(&printer, "reconnected", |s| s.is_connected());
    drop(printer);
    server.shutdown().unwrap();
}

#[test]
fn a_closed_server_fails_the_connection() {
    let server = server();
    let printer = Printer::new();
    printer.connect(target(&server), options()).unwrap();
    wait_for(&printer, "connected", |s| s.is_connected());
    server.shutdown().unwrap();
    wait_for(&printer, "the link to be lost", |s| {
        matches!(s.status, ConnectionStatus::Failed(_))
    });
    let status = printer.with_state(|s| s.status.clone());
    assert!(
        matches!(&status, ConnectionStatus::Failed(m) if m.contains("lost")),
        "{status:?}"
    );
    // Nothing listens there any more.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let err = printer.connect(
        ConnectTarget::Tcp {
            host: "127.0.0.1".into(),
            port,
        },
        options(),
    );
    assert!(err.is_err());
    assert!(matches!(
        printer.with_state(|s| s.status.clone()),
        ConnectionStatus::Failed(_)
    ));
}
