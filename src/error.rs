use std::{error::Error, fmt::Display, io};

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    Corrupt { offset: u64, reason: CorruptReason },
    OffsetOutOfRange { offset: u64, log_len: u64 },
    PayloadTooLarge { len: usize },
}

impl Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(err) => write!(f, "{err}"),
            StoreError::Corrupt { offset, reason } => write!(f, "{reason} at offset {offset}"),
            StoreError::OffsetOutOfRange { offset, log_len } => {
                write!(f, "offset {offset} out of range for log length {log_len}")
            }
            StoreError::PayloadTooLarge { len } => write!(f, "payload too large ({len})"),
        }
    }
}

impl From<io::Error> for StoreError {
    fn from(value: io::Error) -> Self {
        StoreError::Io(value)
    }
}

impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            StoreError::Io(err) => Some(err),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorruptReason {
    ChecksumMismatch,
    LengthOutOfRange,
}

impl Display for CorruptReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CorruptReason::ChecksumMismatch => {
                write!(f, "checksum mismatch")
            }
            CorruptReason::LengthOutOfRange => {
                write!(f, "length out of range")
            }
        }
    }
}
