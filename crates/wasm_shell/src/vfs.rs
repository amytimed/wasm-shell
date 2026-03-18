use std::collections::BTreeMap;
use std::sync::Arc;
use async_trait::async_trait;
use crate::error::VfsError;

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Stat {
    pub is_file: bool,
    pub is_dir: bool,
    pub is_device: bool,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceKind {
    Null,
    Stdin,
    Stdout,
    Stderr,
}

// ── MountPoint trait ──────────────────────────────────────────────────────────

/// Host-provided callbacks for a mounted path.
/// All paths passed to these methods are relative to the mount's virtual root
/// (e.g., if mounted at `/home/agent`, reading `/home/agent/foo.txt` passes
/// `"/foo.txt"` to `read`).
#[async_trait]
pub trait MountPoint: Send + Sync {
    async fn read(&self, path: &str) -> Result<Vec<u8>, VfsError>;
    async fn write(&self, path: &str, data: &[u8]) -> Result<(), VfsError>;
    async fn list(&self, path: &str) -> Result<Vec<String>, VfsError>;
    async fn stat(&self, path: &str) -> Result<Stat, VfsError>;
    async fn remove(&self, path: &str) -> Result<(), VfsError>;
}

// ── Internal tree node ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum VfsNode {
    File(Vec<u8>),
    Dir(BTreeMap<String, VfsNode>),
    Device(DeviceKind),
}

struct MountEntry {
    /// Normalized virtual path, no trailing slash. e.g. "/home/agent"
    vpath: String,
    point: Arc<dyn MountPoint>,
}

// ── Path utilities (pub for use in other modules) ─────────────────────────────

/// Normalize an absolute path: collapse `.`, `..`, duplicate slashes.
/// Returns an absolute path without trailing slash, except for root `"/"`.
pub fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => { parts.pop(); }
            s => parts.push(s),
        }
    }
    if parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", parts.join("/"))
    }
}

/// Resolve `path` relative to `cwd`. Absolute paths are normalized as-is.
pub fn resolve_path(cwd: &str, path: &str) -> String {
    if path.starts_with('/') {
        normalize_path(path)
    } else {
        normalize_path(&format!("{}/{}", cwd, path))
    }
}

fn path_components(path: &str) -> Vec<&str> {
    path.trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect()
}

/// Split a normalized absolute path into (parent, filename).
/// `"/usr/bin/foo"` → `("/usr/bin", "foo")`.
/// `"/foo"` → `("/", "foo")`.
fn split_parent(path: &str) -> Result<(String, String), VfsError> {
    if path == "/" {
        return Err(VfsError::InvalidPath("no parent of root".into()));
    }
    let (parent, name) = path.rsplit_once('/').unwrap();
    let parent = if parent.is_empty() { "/" } else { parent };
    Ok((parent.to_string(), name.to_string()))
}

// ── Tree traversal helpers ────────────────────────────────────────────────────

fn node_at<'a>(root: &'a VfsNode, comps: &[&str], orig: &str) -> Result<&'a VfsNode, VfsError> {
    if comps.is_empty() {
        return Ok(root);
    }
    match root {
        VfsNode::Dir(map) => {
            let child = map
                .get(comps[0])
                .ok_or_else(|| VfsError::NotFound(orig.to_string()))?;
            node_at(child, &comps[1..], orig)
        }
        _ => Err(VfsError::NotADir(orig.to_string())),
    }
}

fn node_at_mut<'a>(
    root: &'a mut VfsNode,
    comps: &[&str],
    orig: &str,
) -> Result<&'a mut VfsNode, VfsError> {
    if comps.is_empty() {
        return Ok(root);
    }
    match root {
        VfsNode::Dir(map) => {
            let child = map
                .get_mut(comps[0])
                .ok_or_else(|| VfsError::NotFound(orig.to_string()))?;
            node_at_mut(child, &comps[1..], orig)
        }
        _ => Err(VfsError::NotADir(orig.to_string())),
    }
}

// ── Vfs ───────────────────────────────────────────────────────────────────────

pub struct Vfs {
    root: VfsNode,
    mounts: Vec<MountEntry>,
}

impl Vfs {
    pub fn new() -> Self {
        let mut vfs = Self {
            root: VfsNode::Dir(BTreeMap::new()),
            mounts: Vec::new(),
        };
        vfs.init();
        vfs
    }

    fn init(&mut self) {
        for dir in &[
            "/usr",
            "/usr/bin",
            "/usr/local",
            "/usr/local/bin",
            "/bin",
            "/etc",
            "/home",
            "/tmp",
            "/dev",
        ] {
            self.mem_mkdir(dir).expect("VFS init");
        }
        for (path, dev) in &[
            ("/dev/null", DeviceKind::Null),
            ("/dev/stdin", DeviceKind::Stdin),
            ("/dev/stdout", DeviceKind::Stdout),
            ("/dev/stderr", DeviceKind::Stderr),
        ] {
            self.mem_set_node(path, VfsNode::Device(dev.clone()))
                .expect("VFS init");
        }
    }

    // ── Mount management ──────────────────────────────────────────────────────

    /// Register a host mount point at `virtual_path`.
    /// Any existing mount at the same path is replaced.
    pub fn add_mount(&mut self, virtual_path: &str, point: Arc<dyn MountPoint>) {
        let vpath = normalize_path(virtual_path);
        self.mounts.retain(|m| m.vpath != vpath);
        self.mounts.push(MountEntry { vpath, point });
        // Longest prefix first so more specific mounts take priority.
        self.mounts.sort_by(|a, b| b.vpath.len().cmp(&a.vpath.len()));
    }

    /// Find the mount index and relative path for `path`, if any.
    fn find_mount(&self, path: &str) -> Option<(usize, String)> {
        for (i, entry) in self.mounts.iter().enumerate() {
            if path == entry.vpath {
                return Some((i, String::new()));
            }
            if path.starts_with(&entry.vpath)
                && path.as_bytes().get(entry.vpath.len()) == Some(&b'/')
            {
                return Some((i, path[entry.vpath.len()..].to_string()));
            }
        }
        None
    }

    // ── Public async API ──────────────────────────────────────────────────────

    pub async fn read_file(&self, path: &str) -> Result<Vec<u8>, VfsError> {
        let path = normalize_path(path);
        if let Some((idx, rel)) = self.find_mount(&path) {
            let point = Arc::clone(&self.mounts[idx].point);
            return point.read(&rel).await;
        }
        self.mem_read(&path)
    }

    pub async fn write_file(&mut self, path: &str, data: Vec<u8>) -> Result<(), VfsError> {
        let path = normalize_path(path);
        if let Some((idx, rel)) = self.find_mount(&path) {
            let point = Arc::clone(&self.mounts[idx].point);
            return point.write(&rel, &data).await;
        }
        // Handle devices before mutating.
        {
            let comps = path_components(&path);
            match node_at(&self.root, &comps, &path) {
                Ok(VfsNode::Device(DeviceKind::Null)) => return Ok(()),
                // stdin/stdout/stderr writes are handled by the execution engine (Stage 3).
                // For now silently discard.
                Ok(VfsNode::Device(_)) => return Ok(()),
                Ok(VfsNode::Dir(_)) => return Err(VfsError::IsADir(path)),
                Ok(VfsNode::File(_)) | Err(VfsError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        self.mem_set_node(&path, VfsNode::File(data))
    }

    pub async fn list_dir(&self, path: &str) -> Result<Vec<String>, VfsError> {
        let path = normalize_path(path);
        if let Some((idx, rel)) = self.find_mount(&path) {
            let point = Arc::clone(&self.mounts[idx].point);
            return point.list(&rel).await;
        }
        self.mem_list(&path)
    }

    pub async fn stat(&self, path: &str) -> Result<Stat, VfsError> {
        let path = normalize_path(path);
        if let Some((idx, rel)) = self.find_mount(&path) {
            let point = Arc::clone(&self.mounts[idx].point);
            return point.stat(&rel).await;
        }
        self.mem_stat(&path)
    }

    pub async fn mkdir(&mut self, path: &str, parents: bool) -> Result<(), VfsError> {
        let path = normalize_path(path);
        if parents {
            self.mem_mkdir_parents(&path)
        } else {
            self.mem_mkdir(&path)
        }
    }

    pub async fn remove(&mut self, path: &str, recursive: bool) -> Result<(), VfsError> {
        let path = normalize_path(path);
        if let Some((idx, rel)) = self.find_mount(&path) {
            let point = Arc::clone(&self.mounts[idx].point);
            return point.remove(&rel).await;
        }
        self.mem_remove(&path, recursive)
    }

    pub async fn rename(&mut self, from: &str, to: &str) -> Result<(), VfsError> {
        let from = normalize_path(from);
        let to = normalize_path(to);
        // Cross-mount rename not supported in Stage 2.
        if self.find_mount(&from).is_some() || self.find_mount(&to).is_some() {
            return Err(VfsError::Mount("rename across mount points not supported".into()));
        }
        let node = self.mem_remove_node(&from)?;
        self.mem_set_node(&to, node)
    }

    pub async fn copy(&mut self, from: &str, to: &str) -> Result<(), VfsError> {
        let from = normalize_path(from);
        let to = normalize_path(to);
        // Read from source (may be mounted), write to dest (may be mounted).
        let data = self.read_file(&from).await?;
        self.write_file(&to, data).await
    }

    // ── Private sync helpers for the in-memory tree ───────────────────────────

    /// Insert or overwrite a node at `path` (must already have a parent dir).
    fn mem_set_node(&mut self, path: &str, node: VfsNode) -> Result<(), VfsError> {
        let path = normalize_path(path);
        if path == "/" {
            return Err(VfsError::InvalidPath("cannot overwrite root".into()));
        }
        let (parent_str, name) = split_parent(&path)?;
        let comps = path_components(&parent_str);
        // Collect comps as owned to avoid borrowing parent_str across the mut borrow of self.root.
        let comps: Vec<String> = comps.iter().map(|s| s.to_string()).collect();
        let comps_ref: Vec<&str> = comps.iter().map(|s| s.as_str()).collect();
        let parent = node_at_mut(&mut self.root, &comps_ref, &parent_str)?;
        match parent {
            VfsNode::Dir(map) => {
                map.insert(name, node);
                Ok(())
            }
            _ => Err(VfsError::NotADir(parent_str)),
        }
    }

    /// Create a directory. Parent must exist.
    fn mem_mkdir(&mut self, path: &str) -> Result<(), VfsError> {
        let path = normalize_path(path);
        if path == "/" {
            return Ok(());
        }
        let (parent_str, name) = split_parent(&path)?;
        let comps: Vec<String> = path_components(&parent_str)
            .iter()
            .map(|s| s.to_string())
            .collect();
        let comps_ref: Vec<&str> = comps.iter().map(|s| s.as_str()).collect();
        let parent = node_at_mut(&mut self.root, &comps_ref, &parent_str)?;
        match parent {
            VfsNode::Dir(map) => {
                if map.contains_key(&name) {
                    return Err(VfsError::AlreadyExists(path));
                }
                map.insert(name, VfsNode::Dir(BTreeMap::new()));
                Ok(())
            }
            _ => Err(VfsError::NotADir(parent_str)),
        }
    }

    fn mem_mkdir_parents(&mut self, path: &str) -> Result<(), VfsError> {
        let comps: Vec<String> = path_components(path)
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut cur = String::from("/");
        for comp in comps {
            let next = if cur == "/" {
                format!("/{}", comp)
            } else {
                format!("{}/{}", cur, comp)
            };
            match self.mem_stat(&next) {
                Ok(s) if s.is_dir => {}
                Ok(_) => return Err(VfsError::NotADir(next)),
                Err(VfsError::NotFound(_)) => self.mem_mkdir(&next)?,
                Err(e) => return Err(e),
            }
            cur = next;
        }
        Ok(())
    }

    fn mem_read(&self, path: &str) -> Result<Vec<u8>, VfsError> {
        let comps = path_components(path);
        match node_at(&self.root, &comps, path)? {
            VfsNode::File(data) => Ok(data.clone()),
            VfsNode::Device(DeviceKind::Null) => Ok(Vec::new()),
            VfsNode::Dir(_) => Err(VfsError::IsADir(path.to_string())),
            VfsNode::Device(_) => Err(VfsError::PermissionDenied(path.to_string())),
        }
    }

    fn mem_list(&self, path: &str) -> Result<Vec<String>, VfsError> {
        let comps = path_components(path);
        match node_at(&self.root, &comps, path)? {
            VfsNode::Dir(map) => Ok(map.keys().cloned().collect()),
            _ => Err(VfsError::NotADir(path.to_string())),
        }
    }

    fn mem_stat(&self, path: &str) -> Result<Stat, VfsError> {
        let comps = path_components(path);
        Ok(match node_at(&self.root, &comps, path)? {
            VfsNode::File(data) => Stat {
                is_file: true,
                is_dir: false,
                is_device: false,
                size: data.len() as u64,
            },
            VfsNode::Dir(_) => Stat {
                is_file: false,
                is_dir: true,
                is_device: false,
                size: 0,
            },
            VfsNode::Device(_) => Stat {
                is_file: false,
                is_dir: false,
                is_device: true,
                size: 0,
            },
        })
    }

    fn mem_remove(&mut self, path: &str, recursive: bool) -> Result<(), VfsError> {
        if !recursive {
            let entries = self.mem_list(path).unwrap_or_default();
            if !entries.is_empty() {
                return Err(VfsError::NotEmpty(path.to_string()));
            }
        }
        self.mem_remove_node(path)?;
        Ok(())
    }

    fn mem_remove_node(&mut self, path: &str) -> Result<VfsNode, VfsError> {
        let (parent_str, name) = split_parent(path)?;
        let comps: Vec<String> = path_components(&parent_str)
            .iter()
            .map(|s| s.to_string())
            .collect();
        let comps_ref: Vec<&str> = comps.iter().map(|s| s.as_str()).collect();
        let parent = node_at_mut(&mut self.root, &comps_ref, &parent_str)?;
        match parent {
            VfsNode::Dir(map) => {
                map.remove(&name).ok_or_else(|| VfsError::NotFound(path.to_string()))
            }
            _ => Err(VfsError::NotADir(parent_str)),
        }
    }
}

impl Default for Vfs {
    fn default() -> Self {
        Self::new()
    }
}
