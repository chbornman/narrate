//! Command-layer error type: every core error funnels into one serializable
//! message for IPC. No business logic; no error is swallowed.

use photoproof_core::collections::CollectionsError;
use photoproof_core::library::LibraryError;
use photoproof_core::sidecar::SidecarError;
use photoproof_core::{AppendError, IdError, StoreError};

#[derive(Debug, thiserror::Error)]
pub enum CmdError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Append(#[from] AppendError),
    #[error(transparent)]
    Library(#[from] LibraryError),
    #[error(transparent)]
    Sidecar(#[from] SidecarError),
    #[error(transparent)]
    Collections(#[from] CollectionsError),
    #[error("invalid id: {0}")]
    Id(#[from] IdError),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("temporarily unavailable: {0}")]
    Unavailable(String),
    #[error("{0}")]
    Invalid(String),
}

impl serde::Serialize for CmdError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

pub type CmdResult<T> = Result<T, CmdError>;
