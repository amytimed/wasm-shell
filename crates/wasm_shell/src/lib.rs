pub mod builtins;
pub mod env;
pub mod error;
pub mod exec;
pub mod expand;
pub mod io;
pub mod registry;
pub mod shell;
pub mod vfs;

#[cfg(feature = "wasm")]
pub mod wasm;

#[cfg(test)]
mod tests;

// Convenient top-level re-exports.
pub use env::EnvMap;
pub use error::{ShellError, VfsError};
pub use io::{BytesReader, VecWriter};
pub use registry::ProgramRegistry;
pub use shell::{ExecOutput, ExitCode, ProgramContext, Shell};
pub use vfs::{MountPoint, Stat, Vfs, normalize_path, resolve_path};
