//! The error type of the host engine.

/// Errors reported by the host engine.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// The operation needs a connected printer (handshake complete).
    #[error("not connected to a printer")]
    NotConnected,
    /// `connect` was called while a connection is open or being opened.
    #[error("already connected; disconnect first")]
    AlreadyConnected,
    /// The printer is busy with something that prevents the operation
    /// (for example a print job is already running).
    #[error("busy: {0}")]
    Busy(String),
    /// The firmware is halted (M112, thermal runaway, ...). Reset the board
    /// ([`Printer::reset_board`](crate::Printer::reset_board)) or reconnect
    /// first; M999 only restarts a firmware that stopped without being
    /// killed ([`PrinterState::restartable`](crate::PrinterState::restartable)).
    #[error("the printer is halted; reset the board or reconnect")]
    Halted,
    /// The connection cannot do this (for example reset a board behind a
    /// network bridge); the message says what to do instead.
    #[error("not supported: {0}")]
    Unsupported(String),
    /// An I/O error from a link or a file.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A serial port could not be opened or configured.
    #[error("serial port error: {0}")]
    Serial(String),
    /// A connection target string could not be parsed.
    #[error("invalid connection target: {0}")]
    InvalidTarget(String),
    /// A configuration file could not be read, parsed or written.
    #[error("configuration error: {0}")]
    Config(String),
    /// An argument was out of range or malformed.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// Something did not answer in time.
    #[error("timed out: {0}")]
    Timeout(String),
    /// The connection attempt was cancelled ([`Printer::disconnect`](crate::Printer::disconnect)
    /// while connecting).
    #[error("the connection attempt was cancelled")]
    Cancelled,
}

impl From<serialport::Error> for HostError {
    fn from(e: serialport::Error) -> Self {
        match e.kind() {
            serialport::ErrorKind::Io(kind) => {
                HostError::Io(std::io::Error::new(kind, e.to_string()))
            }
            _ => HostError::Serial(e.to_string()),
        }
    }
}
