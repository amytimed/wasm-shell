use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

// ── VecWriter ─────────────────────────────────────────────────────────────────

/// An `AsyncWrite` backed by a shared `Vec<u8>`.
/// Cloning produces a second handle to the same buffer — writes from any
/// handle are appended to the same backing store.
#[derive(Clone)]
pub struct VecWriter(pub(crate) Arc<Mutex<Vec<u8>>>);

impl VecWriter {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    /// Snapshot the current contents.
    pub fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
    }
}

impl Default for VecWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncWrite for VecWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

// ── BytesReader ───────────────────────────────────────────────────────────────

/// An `AsyncRead` over an in-memory byte buffer.
pub struct BytesReader {
    data: Arc<Vec<u8>>,
    pos: usize,
}

impl BytesReader {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data: Arc::new(data), pos: 0 }
    }

    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    /// Create a new reader sharing the same data, starting from the current position.
    pub fn fork(&self) -> Self {
        Self { data: Arc::clone(&self.data), pos: self.pos }
    }
}

impl AsyncRead for BytesReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let pos = self.pos;
        let available = self.data.len().saturating_sub(pos);
        let to_read = available.min(buf.remaining());
        if to_read > 0 {
            buf.put_slice(&self.data[pos..pos + to_read]);
            self.pos += to_read;
        }
        Poll::Ready(Ok(()))
    }
}

// ── Io ────────────────────────────────────────────────────────────────────────

/// Internal I/O bundle passed through command execution.
pub(crate) struct Io {
    pub stdin: BytesReader,
    pub stdout: VecWriter,
    pub stderr: VecWriter,
}

impl Io {
    pub fn new() -> Self {
        Self {
            stdin: BytesReader::empty(),
            stdout: VecWriter::new(),
            stderr: VecWriter::new(),
        }
    }

    /// Clone handles — stdout/stderr clones share the same backing buffer.
    /// Stdin gets an independent cursor at the same position.
    pub fn share(&self) -> Self {
        Self {
            stdin: self.stdin.fork(),
            stdout: self.stdout.clone(),
            stderr: self.stderr.clone(),
        }
    }
}

impl Default for Io {
    fn default() -> Self {
        Self::new()
    }
}
