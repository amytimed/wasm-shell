use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum VfsError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("not a directory: {0}")]
    NotADir(String),
    #[error("not a file: {0}")]
    NotAFile(String),
    #[error("already exists: {0}")]
    AlreadyExists(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("is a directory: {0}")]
    IsADir(String),
    #[error("directory not empty: {0}")]
    NotEmpty(String),
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("mount error: {0}")]
    Mount(String),
}

#[derive(Debug, Error)]
pub enum ShellError {
    #[error("VFS error: {0}")]
    Vfs(#[from] VfsError),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("command not found: {0}")]
    CommandNotFound(String),
    #[error("io error: {0}")]
    Io(String),
    /// Produced by the `exit` built-in to terminate the current shell session.
    #[error("exit {0}")]
    Exit(i32),
}
