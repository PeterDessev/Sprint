//! Raw TCP links (serial-to-network bridges, `sprint sim --tcp`).

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use super::link::{Link, LinkControl, READ_TIMEOUT};
use crate::error::HostError;

/// How long establishing the connection may take per address.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// A peer that does not read for this long is treated as gone.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Connect to `host:port` (every resolved address is tried in turn).
pub(crate) fn connect(host: &str, port: u16) -> Result<Link, HostError> {
    let addrs: Vec<_> = (host, port).to_socket_addrs()?.collect();
    if addrs.is_empty() {
        return Err(HostError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{host} did not resolve to any address"),
        )));
    }
    let mut last = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => return link(stream),
            Err(e) => last = Some(e),
        }
    }
    Err(HostError::Io(last.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::NotConnected, "connection failed")
    })))
}

fn link(stream: TcpStream) -> Result<Link, HostError> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let reader = stream.try_clone()?;
    let control = stream.try_clone()?;
    Ok(Link {
        reader: Box::new(reader),
        writer: Box::new(stream),
        control: LinkControl::Tcp(control),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn talks_to_a_local_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 5];
            s.read_exact(&mut buf).unwrap();
            s.write_all(b"ok\n").unwrap();
            buf
        });
        let mut link = connect("127.0.0.1", port).unwrap();
        link.writer.write_all(b"M105\n").unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 16];
        while !got.ends_with(b"\n") {
            match link.reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) => assert!(super::super::link::is_no_data(&e), "{e}"),
            }
        }
        assert_eq!(got, b"ok\n");
        assert_eq!(&server.join().unwrap(), b"M105\n");
        let (_, _, handle) = link.split();
        handle.close();
    }

    #[test]
    fn refused_connections_are_errors() {
        // Bind and drop to find a port nobody listens on.
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert!(matches!(connect("127.0.0.1", port), Err(HostError::Io(_))));
    }
}
