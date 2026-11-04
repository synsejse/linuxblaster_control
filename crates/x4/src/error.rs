//! Errors returned by this crate.
use std::io;

/// Protocol, validation, transport and serial failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A malformed reply, a rejected write or an invalid request.
    #[error("{0}")]
    Invalid(String),
    /// An operation this firmware lacks.
    #[error("{0}")]
    Unsupported(String),
    /// The X4 did not answer before the deadline.
    #[error(
        "X4 response timed out; reopen and read state before another write"
    )]
    Timeout,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Serial(#[from] serialport::Error),
}
impl Error {
    /// Whether the device simply did not answer in time.
    pub fn is_timeout(&self) -> bool {
        match self {
            Self::Timeout => true,
            Self::Io(error) => error.kind() == io::ErrorKind::TimedOut,
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// An [`Error::Invalid`] for a malformed reply or invalid request.
pub(crate) fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
/// An [`Error::Unsupported`] for an operation this firmware lacks.
pub(crate) fn unsupported(message: impl Into<String>) -> Error {
    Error::Unsupported(message.into())
}
