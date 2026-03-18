use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::error::ShellError;
use crate::shell::{ExitCode, ProgramContext};
use crate::vfs::normalize_path;

/// The return type of a program callback.
pub type ProgramFuture = Pin<Box<dyn Future<Output = Result<ExitCode, ShellError>> + Send>>;

/// A type-erased, cloneable program callback.
pub type ProgramFn = dyn Fn(ProgramContext) -> ProgramFuture + Send + Sync;

pub struct ProgramRegistry {
    /// Keyed by normalized full path, e.g. `"/usr/bin/foo"`.
    programs: HashMap<String, Arc<ProgramFn>>,
}

impl ProgramRegistry {
    pub fn new() -> Self {
        Self {
            programs: HashMap::new(),
        }
    }

    /// Register a program.
    ///
    /// - Bare names (`"foo"`) are stored at `/usr/bin/foo`.
    /// - Names containing `/` are stored at the normalized path.
    pub fn add<F, Fut>(&mut self, name: &str, f: F)
    where
        F: Fn(ProgramContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ExitCode, ShellError>> + Send + 'static,
    {
        let path = canonical_program_path(name);
        let boxed: Arc<ProgramFn> = Arc::new(move |ctx| Box::pin(f(ctx)));
        self.programs.insert(path, boxed);
    }

    /// Resolve a command name to a callback using `path_var` (the `$PATH` value).
    ///
    /// - If `name` contains `/`, it is treated as a direct path.
    /// - Otherwise, each directory in `path_var` is tried in order.
    pub fn resolve(&self, name: &str, path_var: &str) -> Option<Arc<ProgramFn>> {
        if name.contains('/') {
            let path = normalize_path(name);
            self.programs.get(&path).cloned()
        } else {
            for dir in path_var.split(':') {
                let dir = dir.trim_end_matches('/');
                let path = format!("{}/{}", dir, name);
                if let Some(cb) = self.programs.get(&path) {
                    return Some(cb.clone());
                }
            }
            None
        }
    }

    /// Returns the full path a bare name would be stored at.
    pub fn canonical_path(name: &str) -> String {
        canonical_program_path(name)
    }

    /// Like `resolve`, but returns the resolved path string instead of the callback.
    pub fn find_path(&self, name: &str, path_var: &str) -> Option<String> {
        if name.contains('/') {
            let path = normalize_path(name);
            if self.programs.contains_key(&path) { Some(path) } else { None }
        } else {
            for dir in path_var.split(':') {
                let dir = dir.trim_end_matches('/');
                let path = format!("{}/{}", dir, name);
                if self.programs.contains_key(&path) {
                    return Some(path);
                }
            }
            None
        }
    }
}

impl Default for ProgramRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn canonical_program_path(name: &str) -> String {
    if name.contains('/') {
        normalize_path(name)
    } else {
        format!("/usr/bin/{}", name)
    }
}
