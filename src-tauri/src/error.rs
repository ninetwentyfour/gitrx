use std::fmt;

/// Application-level error used across the git layer and command boundary.
///
/// Implements `From` for the common failure sources (`git2::Error`, `io::Error`)
/// and converts cleanly into a human-readable `String` for Tauri commands,
/// which return `Result<T, String>`.
#[derive(Debug)]
pub enum AppError {
    /// A libgit2 operation failed.
    Git(String),
    /// A filesystem / IO operation failed.
    Io(String),
    /// A domain/validation error with a bespoke message.
    Message(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Git(msg) => write!(f, "Git error: {msg}"),
            AppError::Io(msg) => write!(f, "IO error: {msg}"),
            AppError::Message(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for AppError {}

impl From<git2::Error> for AppError {
    fn from(err: git2::Error) -> Self {
        AppError::Git(err.message().to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::Io(err.to_string())
    }
}

impl From<AppError> for String {
    fn from(err: AppError) -> Self {
        err.to_string()
    }
}

impl AppError {
    /// Construct an `AppError` from a plain message.
    pub fn msg(msg: impl Into<String>) -> Self {
        AppError::Message(msg.into())
    }
}

/// Convenience result alias for the git layer.
pub type AppResult<T> = Result<T, AppError>;
