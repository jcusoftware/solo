//! Local HTTP preview server for compiled site output (`solo preview`).
//!
//! Serves one compiled site directory over HTTP bound to `127.0.0.1` only.
//! Request paths are jailed to the canonicalized serve root (the same
//! discipline as the MCP path jail in `mcp.rs`): `..` and absolute components
//! are rejected up front, sensitive path components (`.dev.vars`, `.env*`,
//! `*.pem`, `id_rsa`, `private/`, ...) are rejected, and the canonicalized
//! target must remain under the root. There are no directory listings.
//!
//! The Cloudflare `_headers` file is not applied as HTTP headers: it is a
//! Cloudflare Static Assets directive, not a portable HTTP config. It is
//! served as a plain static file if requested directly.
//!
//! This is host-side CLI code only (no wasm constraint) and uses only the
//! Rust standard library (`TcpListener` plus threads) — no new dependencies.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::mcp::is_sensitive_component;

/// Set by the SIGINT/SIGTERM handler installed by [`serve_forever`] (Unix).
#[cfg(unix)]
static PROCESS_SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Default TCP port bound by `solo preview` on 127.0.0.1.
pub const DEFAULT_PREVIEW_PORT: u16 = 8787;

/// Maximum request head bytes accepted before the connection is dropped.
const MAX_REQUEST_HEAD_BYTES: usize = 16 * 1024;

/// Read/idle timeout for a single preview connection.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// A running preview server bound to one port on `127.0.0.1`.
pub struct PreviewHandle {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl PreviewHandle {
    /// The `127.0.0.1` address (including the actual bound port) being served.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Block until the accept loop exits. Used by `solo preview`, which stays
    /// in the foreground until a SIGINT/SIGTERM handler signals shutdown.
    pub fn wait(mut self) -> Result<(), String> {
        #[cfg(unix)]
        {
            // A process-directed signal is handled by exactly one thread. If
            // the kernel picks this (main) thread, the accept thread stays
            // blocked in `accept()` — so do not join blindly. Poll until the
            // accept loop exits on its own or the signal handler flips
            // [`PROCESS_SHUTDOWN`]; in the latter case wake the accept loop
            // with a self-connect before joining.
            while !self
                .join
                .as_ref()
                .is_some_and(|join| join.is_finished())
            {
                if PROCESS_SHUTDOWN.load(Ordering::SeqCst) {
                    self.shutdown.store(true, Ordering::Relaxed);
                    let _ = TcpStream::connect_timeout(&self.addr, CONNECTION_TIMEOUT);
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        match self.join.take() {
            Some(join) => join
                .join()
                .map_err(|_| "preview server thread panicked".to_string()),
            None => Ok(()),
        }
    }

    /// Signal shutdown, wake the accept loop with a self-connect, and wait for
    /// it to exit cleanly. Only exercised by tests; the CLI flow blocks in
    /// [`Self::wait`] until the process is terminated by Ctrl-C.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn stop(self) -> Result<(), String> {
        self.shutdown.store(true, Ordering::Relaxed);
        // The accept loop is blocked in `accept()`; connect to ourselves so it
        // wakes, observes the shutdown flag, and exits.
        let _ = TcpStream::connect_timeout(&self.addr, CONNECTION_TIMEOUT);
        self.wait()
    }
}

impl Drop for PreviewHandle {
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            self.shutdown.store(true, Ordering::Relaxed);
            let _ = TcpStream::connect_timeout(&self.addr, CONNECTION_TIMEOUT);
            // Detach if join cannot complete promptly; the flag makes the loop exit.
            drop(join);
        }
    }
}

/// Bind `127.0.0.1:<port>` (port 0 selects a free port) and serve `root` on a
/// background thread, returning a handle to the running server.
pub fn spawn(root: &Path, port: u16) -> Result<PreviewHandle, String> {
    let canon_root = canonicalize_root(root)?;
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("failed to bind 127.0.0.1:{port}: {e}"))?;
    let addr = listener
        .local_addr()
        .map_err(|e| format!("failed to determine bound address: {e}"))?;

    let shutdown = Arc::new(AtomicBool::new(false));
    let thread_shutdown = Arc::clone(&shutdown);
    let join = std::thread::Builder::new()
        .name("solo-preview".to_string())
        .spawn(move || accept_loop(listener, canon_root, &thread_shutdown))
        .map_err(|e| format!("failed to spawn preview thread: {e}"))?;

    Ok(PreviewHandle {
        addr,
        shutdown,
        join: Some(join),
    })
}

/// Serve `root` on `127.0.0.1:<port>` on the calling thread until Ctrl-C
/// (SIGINT) or SIGTERM. On Unix, a signal handler flips a flag and interrupts
/// the blocking `accept()` (no `SA_RESTART`), so the loop exits and the
/// process shuts down cleanly with exit code 0 — even when the CLI was
/// backgrounded with SIGINT ignored. On non-Unix targets the OS default
/// Ctrl-C behavior terminates the process instead.
pub fn serve_forever(root: &Path, port: u16) -> Result<(), String> {
    #[cfg(unix)]
    install_shutdown_signal_handlers()?;
    let handle = spawn(root, port)?;
    println!(
        "solo preview: serving '{}' at http://{}/ (press Ctrl-C to stop)",
        root.display(),
        handle.addr()
    );
    handle.wait()?;
    println!("solo preview: stopped.");
    Ok(())
}

#[cfg(unix)]
extern "C" fn on_shutdown_signal(_sig: libc::c_int) {
    PROCESS_SHUTDOWN.store(true, Ordering::SeqCst);
}

/// Install SIGINT/SIGTERM handlers that set [`PROCESS_SHUTDOWN`] (Unix only).
/// Without `SA_RESTART` the blocking `accept()` in the accept loop returns
/// `EINTR` on signal delivery, unblocking shutdown.
#[cfg(unix)]
fn install_shutdown_signal_handlers() -> Result<(), String> {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_shutdown_signal as *const () as usize;
        if libc::sigemptyset(&mut sa.sa_mask) != 0 {
            return Err("failed to initialize signal mask".to_string());
        }
        sa.sa_flags = 0;
        for sig in [libc::SIGINT, libc::SIGTERM] {
            if libc::sigaction(sig, &sa, std::ptr::null_mut()) != 0 {
                return Err(format!("failed to install handler for signal {sig}"));
            }
        }
    }
    Ok(())
}

fn canonicalize_root(root: &Path) -> Result<PathBuf, String> {
    if root.as_os_str().is_empty() {
        return Err("preview directory must not be empty".to_string());
    }
    let canon_root = fs::canonicalize(root).map_err(|e| {
        format!(
            "failed to resolve preview directory '{}': {e}",
            root.display()
        )
    })?;
    if !canon_root.is_dir() {
        return Err(format!(
            "preview directory '{}' is not a directory",
            root.display()
        ));
    }
    Ok(canon_root)
}

/// Whether the accept loop should stop: either an explicit `stop()` (tests)
/// or, on Unix, a SIGINT/SIGTERM delivered to the process.
fn shutdown_requested(shutdown: &AtomicBool) -> bool {
    if shutdown.load(Ordering::Relaxed) {
        return true;
    }
    #[cfg(unix)]
    if PROCESS_SHUTDOWN.load(Ordering::SeqCst) {
        return true;
    }
    false
}

fn accept_loop(listener: TcpListener, root: PathBuf, shutdown: &AtomicBool) {
    loop {
        if shutdown_requested(shutdown) {
            break;
        }
        match listener.accept() {
            Ok((stream, _peer)) => {
                if shutdown_requested(shutdown) {
                    break;
                }
                let _ = handle_connection(stream, &root);
            }
            // EINTR: a shutdown signal interrupted accept(); re-check the flag.
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

fn handle_connection(stream: TcpStream, root: &Path) -> std::io::Result<()> {
    stream.set_read_timeout(Some(CONNECTION_TIMEOUT))?;
    stream.set_write_timeout(Some(CONNECTION_TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(()); // client closed before sending anything
    }

    // Drain the rest of the request head (bounded); preview responses are
    // `Connection: close` so the body is never read.
    let mut head_bytes = request_line.len();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        head_bytes += n;
        if head_bytes > MAX_REQUEST_HEAD_BYTES {
            return write_response(
                &mut writer,
                431,
                "Request Header Fields Too Large",
                None,
                b"",
                false,
            );
        }
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    if method != "GET" && method != "HEAD" {
        return write_response(&mut writer, 405, "Method Not Allowed", None, b"", false);
    }

    serve_request(&mut writer, target, method == "HEAD", root);
    Ok(())
}

fn serve_request(writer: &mut TcpStream, target: &str, head_only: bool, root: &Path) {
    let mut bad =
        |code: u16, reason: &str| write_response(writer, code, reason, None, b"", head_only);

    if target.is_empty() || !target.starts_with('/') || target.contains('\0') {
        let _ = bad(400, "Bad Request");
        return;
    }
    // Strip any query string; only the path is served.
    let raw_path = target.split('?').next().unwrap_or(target);

    let Some(decoded) = percent_decode(raw_path) else {
        let _ = bad(400, "Bad Request");
        return;
    };

    let rel = decoded.trim_start_matches('/');
    let rel_path = Path::new(rel);
    for comp in rel_path.components() {
        match comp {
            // CurDir ("." segments) is skipped when joining; everything that
            // could escape the root is refused before any filesystem access.
            Component::Normal(part) => {
                if is_sensitive_component(&part.to_string_lossy()) {
                    let _ = bad(403, "Forbidden");
                    return;
                }
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                let _ = bad(403, "Forbidden");
                return;
            }
        }
    }

    // Canonicalize (fails with 404 for missing paths) and require the resolved
    // target to stay under the serve root — mirroring the MCP path jail.
    let Ok(canon_target) = fs::canonicalize(root.join(rel_path)) else {
        let _ = bad(404, "Not Found");
        return;
    };
    let mut file_target = canon_target.clone();
    if file_target.is_dir() {
        // No directory listings: only an index.html inside is served.
        file_target = file_target.join("index.html");
        match fs::canonicalize(&file_target) {
            Ok(canon_index) => file_target = canon_index,
            Err(_) => {
                let _ = bad(404, "Not Found");
                return;
            }
        }
    }
    if !file_target.starts_with(root) || !file_target.is_file() {
        let _ = bad(404, "Not Found");
        return;
    }

    match fs::read(&file_target) {
        Ok(body) => {
            let content_type = mime_for_path(&file_target);
            let _ = write_response(writer, 200, "OK", Some(content_type), &body, head_only);
        }
        Err(_) => {
            let _ = bad(404, "Not Found");
        }
    }
}

fn write_response(
    writer: &mut impl Write,
    status: u16,
    reason: &str,
    content_type: Option<&str>,
    body: &[u8],
    head_only: bool,
) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status} {reason}\r\n");
    if let Some(ct) = content_type {
        head.push_str("Content-Type: ");
        head.push_str(ct);
        head.push_str("\r\n");
    }
    head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    head.push_str("Connection: close\r\n\r\n");
    writer.write_all(head.as_bytes())?;
    if !head_only {
        writer.write_all(body)?;
    }
    writer.flush()
}

/// Percent-decode a raw request path, rejecting invalid escapes and NUL bytes.
fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                if i + 2 >= bytes.len() {
                    return None;
                }
                let hi = (bytes[i + 1] as char).to_digit(16)?;
                let lo = (bytes[i + 2] as char).to_digit(16)?;
                let byte = (hi * 16 + lo) as u8;
                if byte == 0 {
                    return None;
                }
                out.push(byte);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// MIME type for the file set emitted by `compile_site` (plus common static
/// assets). `_headers`/`_redirects` are Cloudflare directives served as plain
/// text if requested directly — they are never applied as HTTP headers here.
#[must_use]
pub fn mime_for_path(path: &Path) -> &'static str {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        let lower = name.to_ascii_lowercase();
        if lower == "_headers" || lower == "_redirects" {
            return "text/plain; charset=utf-8";
        }
    }
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("xml") => "application/xml",
        Some("txt") => "text/plain; charset=utf-8",
        Some("ico") => "image/x-icon",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("md") => "text/markdown; charset=utf-8",
        Some("webmanifest") => "application/manifest+json",
        _ => "application/octet-stream",
    }
}
