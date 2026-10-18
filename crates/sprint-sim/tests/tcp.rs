//! Serving the simulator on an ephemeral loopback port.

mod common;

use common::{fast, frame};
use sprint_sim::SimServer;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

fn read_until(reader: &mut BufReader<TcpStream>, want: &str) -> Vec<String> {
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).expect("read from server");
        assert!(n > 0, "server closed early; got {lines:?}");
        let line = line.trim_end().to_string();
        let done = line.starts_with(want);
        lines.push(line);
        if done {
            return lines;
        }
    }
}

fn connect(addr: std::net::SocketAddr) -> (TcpStream, BufReader<TcpStream>) {
    let stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let reader = BufReader::new(stream.try_clone().unwrap());
    (stream, reader)
}

#[test]
fn serves_a_fresh_printer_per_client() {
    let server = SimServer::bind("127.0.0.1:0", fast(50.0)).expect("bind");
    let addr = server.local_addr();
    assert!(addr.ip().is_loopback());
    assert_ne!(addr.port(), 0);

    let (mut stream, mut reader) = connect(addr);
    let banner = read_until(&mut reader, "echo:SD card ok");
    assert_eq!(banner[0], "start");
    stream.write_all(b"M115\n").unwrap();
    let info = read_until(&mut reader, "ok");
    assert!(info[0].starts_with("FIRMWARE_NAME:Marlin 2.1.2.1 (Sprint Virtual)"));
    let burst = format!("{}\nG92 X5\nM114\n", frame(1, "M110"));
    stream.write_all(burst.as_bytes()).unwrap();
    read_until(&mut reader, "ok");
    read_until(&mut reader, "ok");
    let pos = read_until(&mut reader, "ok");
    assert!(pos[0].starts_with("X:5.00"), "{pos:?}");
    drop(reader);
    drop(stream);

    // The next client gets a newly booted printer.
    let (mut stream, mut reader) = connect(addr);
    let banner = read_until(&mut reader, "echo:SD card ok");
    assert_eq!(banner[0], "start");
    stream.write_all(b"M114\n").unwrap();
    let pos = read_until(&mut reader, "ok");
    assert!(pos[0].starts_with("X:0.00"), "{pos:?}");
    drop(stream);
    server.shutdown().expect("clean shutdown");
}

#[test]
fn shutdown_disconnects_the_client() {
    let server = SimServer::bind("127.0.0.1:0", fast(50.0)).expect("bind");
    let (_stream, mut reader) = connect(server.local_addr());
    read_until(&mut reader, "echo:SD card ok");
    server.shutdown().expect("clean shutdown");
    let mut rest = String::new();
    // EOF (or a reset) once the server is gone.
    let n = reader.read_line(&mut rest).unwrap_or(0);
    assert_eq!(n, 0, "{rest:?}");
}

#[test]
fn bind_errors_are_reported() {
    let server = SimServer::bind("127.0.0.1:0", fast(50.0)).expect("bind");
    let taken = server.local_addr();
    assert!(SimServer::bind(taken, fast(50.0)).is_err());
    assert!(sprint_sim::serve_tcp("definitely-not-an-address", fast(50.0)).is_err());
}
