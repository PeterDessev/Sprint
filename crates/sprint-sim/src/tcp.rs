//! Serving the virtual printer over TCP so other hosts (Printrun,
//! OctoPrint, Sprint itself) can connect to it.

use crate::config::SimConfig;
use crate::link::{SimLink, spawn};
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// How often blocking loops re-check for shutdown.
const POLL: Duration = Duration::from_millis(50);
/// A client that does not read its data for this long is disconnected.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Serve the virtual printer over TCP (one client at a time; every client
/// gets a freshly booted printer). Blocks, and returns only when the
/// listener fails.
pub fn serve_tcp(addr: &str, config: SimConfig) -> io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    log::info!(
        "virtual printer listening on {}",
        listener
            .local_addr()
            .map_or_else(|_| addr.to_string(), |a| a.to_string())
    );
    serve_listener(&listener, &config, &AtomicBool::new(false))
}

/// A TCP server running on a background thread (for tests and embedding).
/// Dropping it stops the server.
///
/// The accept loop ends only when it is stopped or the listener fails;
/// [`SimServer::is_finished`] and [`SimServer::wait`] let a program that
/// serves until it is killed notice the second case.
pub struct SimServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    /// Set by the server thread when its accept loop has ended (also while
    /// a [`SimServer::wait`] holds the join handle).
    finished: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<io::Result<()>>>>,
}

/// Sets its flag when dropped: the server thread ended, however it ended.
struct SetOnDrop(Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl SimServer {
    /// Bind to `addr` (for example "127.0.0.1:0" for an ephemeral port)
    /// and serve in the background.
    pub fn bind(addr: impl ToSocketAddrs, config: SimConfig) -> io::Result<SimServer> {
        let listener = TcpListener::bind(addr)?;
        let local = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let finished = Arc::new(AtomicBool::new(false));
        let thread_finished = SetOnDrop(Arc::clone(&finished));
        let thread = thread::Builder::new()
            .name("sprint-sim-tcp".to_string())
            .spawn(move || {
                let _finished = thread_finished;
                serve_listener(&listener, &config, &thread_stop)
            })?;
        Ok(SimServer {
            addr: local,
            stop,
            finished,
            thread: Mutex::new(Some(thread)),
        })
    }

    /// The address the server listens on.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// The accept loop has ended: stopped, or the listener failed
    /// ([`SimServer::wait`] tells which).
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    /// Block until the accept loop ends and return how: `Ok` after
    /// [`SimServer::stop`], the listener's error otherwise. Only the first
    /// call gets the error: other calls (also ones made meanwhile from
    /// other threads) wait for the end too and return `Ok(())`.
    pub fn wait(&self) -> io::Result<()> {
        let thread = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match thread {
            Some(t) => t
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("server thread panicked"))),
            None => {
                // Another call is joining the thread.
                while !self.is_finished() {
                    thread::sleep(POLL);
                }
                Ok(())
            }
        }
    }

    /// Ask the server to stop (disconnecting any client) without waiting.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Stop serving (disconnecting any client) and wait for the thread.
    pub fn shutdown(self) -> io::Result<()> {
        self.stop();
        self.wait()
    }
}

impl Drop for SimServer {
    fn drop(&mut self) {
        self.stop();
        let _ = self.wait();
    }
}

fn is_transient(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::Interrupted
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::TimedOut
    )
}

/// Accept loop shared by [`serve_tcp`] and [`SimServer`].
fn serve_listener(listener: &TcpListener, config: &SimConfig, stop: &AtomicBool) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, peer)) => {
                log::info!("virtual printer: client {peer} connected");
                if let Err(e) = bridge(stream, config, stop) {
                    log::warn!("virtual printer: client {peer}: {e}");
                }
                log::info!("virtual printer: client {peer} disconnected");
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL / 2),
            Err(e) if is_transient(&e) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Shuttle bytes between one client and a fresh simulator until either
/// side goes away.
fn bridge(stream: TcpStream, config: &SimConfig, stop: &AtomicBool) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(POLL))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let SimLink {
        mut reader,
        mut writer,
        control,
    } = spawn(config.clone());
    let done = Arc::new(AtomicBool::new(false));
    let mut incoming = stream.try_clone()?;
    let thread_done = Arc::clone(&done);
    let upstream = thread::Builder::new()
        .name("sprint-sim-tcp-rx".to_string())
        .spawn(move || {
            let mut buf = [0u8; 4096];
            while !thread_done.load(Ordering::Acquire) {
                match incoming.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if writer.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock
                                | io::ErrorKind::TimedOut
                                | io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => break,
                }
            }
            thread_done.store(true, Ordering::Release);
        })?;
    let mut outgoing = stream;
    reader.set_timeout(POLL);
    let mut buf = [0u8; 4096];
    let result = loop {
        if done.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
            break Ok(());
        }
        match reader.read(&mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => {
                if let Err(e) = outgoing.write_all(&buf[..n]) {
                    break if is_transient(&e) || e.kind() == io::ErrorKind::BrokenPipe {
                        Ok(())
                    } else {
                        Err(e)
                    };
                }
            }
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
            Err(e) => break Err(e),
        }
    };
    done.store(true, Ordering::Release);
    control.shutdown();
    let _ = outgoing.shutdown(Shutdown::Both);
    let _ = upstream.join();
    result
}
