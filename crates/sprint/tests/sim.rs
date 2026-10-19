//! `sprint sim` run as a child process and reached over loopback TCP.
//! Only the virtual printer is involved; no serial port is touched.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Generous limit for each step (CI machines can be slow).
const STEP_TIMEOUT: Duration = Duration::from_secs(20);

/// A running `sprint sim`, killed when dropped.
struct SimProcess {
    child: Child,
    first_line: String,
}

impl SimProcess {
    fn spawn(args: &[&str]) -> SimProcess {
        let mut child = Command::new(env!("CARGO_BIN_EXE_sprint"))
            .arg("sim")
            .args(args)
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sprint sim");
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let _ = tx.send(line);
            // Keep draining so the child never blocks on a full pipe.
            let mut sink = Vec::new();
            let _ = reader.read_to_end(&mut sink);
        });
        let first_line = match rx.recv_timeout(STEP_TIMEOUT) {
            Ok(line) => line,
            Err(e) => {
                let _ = child.kill();
                panic!("no output from sprint sim: {e}");
            }
        };
        SimProcess { child, first_line }
    }

    /// The bound address: the last word of the first line.
    fn addr(&self) -> SocketAddr {
        let word = self.first_line.split_whitespace().last().unwrap_or("");
        word.parse()
            .unwrap_or_else(|e| panic!("bad first line {:?}: {e}", self.first_line))
    }
}

impl Drop for SimProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read from `stream` until the received text contains `want`.
fn read_until(stream: &mut TcpStream, text: &mut String, want: &str) {
    let deadline = Instant::now() + STEP_TIMEOUT;
    let mut buf = [0u8; 4096];
    while !text.contains(want) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {want:?}; got {text:?}"
        );
        match stream.read(&mut buf) {
            Ok(0) => panic!("connection closed waiting for {want:?}; got {text:?}"),
            Ok(n) => text.push_str(&String::from_utf8_lossy(&buf[..n])),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => panic!("read failed: {e}"),
        }
    }
}

#[test]
fn sim_serves_a_virtual_printer_over_tcp() {
    let sim = SimProcess::spawn(&[
        "--tcp",
        "127.0.0.1:0",
        "--time-scale",
        "50",
        "--boot-delay-ms",
        "10",
        "--name",
        "CLI Test Printer",
    ]);
    assert!(
        sim.first_line
            .starts_with("Virtual printer listening on 127.0.0.1:"),
        "{:?}",
        sim.first_line
    );
    let addr = sim.addr();
    assert_ne!(addr.port(), 0);
    let mut stream = TcpStream::connect_timeout(&addr, STEP_TIMEOUT).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("timeout");
    let mut text = String::new();
    // Commands sent before the boot banner are dropped, as on a real board.
    read_until(&mut stream, &mut text, "echo:SD card ok");
    assert!(text.starts_with("start"), "{text:?}");
    stream.write_all(b"M115\n").expect("send M115");
    let mut reply = String::new();
    read_until(&mut stream, &mut reply, "FIRMWARE_NAME:");
    read_until(&mut stream, &mut reply, "\nok");
    assert!(reply.contains("FIRMWARE_NAME:Marlin"), "{reply:?}");
    assert!(reply.contains("MACHINE_TYPE:CLI Test Printer"), "{reply:?}");
    // A second client gets a fresh printer once the first leaves.
    drop(stream);
    let mut again = TcpStream::connect_timeout(&addr, STEP_TIMEOUT).expect("reconnect");
    again
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("timeout");
    let mut text = String::new();
    read_until(&mut again, &mut text, "start");
}

#[test]
fn sim_reports_an_address_it_cannot_use() {
    let taken = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = taken.local_addr().expect("addr").port();
    let output = Command::new(env!("CARGO_BIN_EXE_sprint"))
        .args(["sim", "--tcp", &format!("127.0.0.1:{port}")])
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .output()
        .expect("run sprint sim");
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(
        text.contains(&format!("cannot listen on 127.0.0.1:{port}")),
        "{text}"
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sprint"))
        .args(["sim", "--tcp", "not an address"])
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .output()
        .expect("run sprint sim");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot listen on not an address"));
}
